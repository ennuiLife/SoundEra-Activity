//! SoundEra Activity: login through Navidrome, then presence (what each person plays now) over HTTP and WebSocket.

pub mod config;
pub mod db;
pub mod navidrome;

use activity_core::{PresenceBoard, PresenceUpdate, SessionKey};
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, ConnectInfo, DefaultBodyLimit, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use config::Config;
use navidrome::{Credentials, Verdict};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;

const LOGIN_FAILS_MAX: u32 = 5;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_NAME: usize = 128;
const MAX_SECRET: usize = 512;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Presence { entry: activity_core::presence::PresenceEntry },
    PresenceEnded { user: String },
    Reaction { from: String, to: String, song_id: String, kind: activity_core::ReactionKind },
    Chat { message: activity_core::ChatMessage },
    ChatDeleted { id: i64 },
    /// Someone started listening along with `host` (told to the host).
    ListenerJoined { host: String, listener: String },
    /// A listener stopped (told to the host).
    ListenerLeft { host: String, listener: String },
    /// The host stopped playing or sharing, so everyone listening along is let go (told to those listeners).
    ListenEnded { host: String, listeners: Vec<String> },
}

impl Event {
    /// Should `viewer` get this event? Nobody gets an echo of their own action, a reaction goes only to the person
    /// it is for, and nothing from someone the viewer has blocked is delivered.
    fn is_for(&self, viewer: &str, db: &rusqlite::Connection) -> bool {
        let blocked = |who: &str| db::is_blocked(db, viewer, who).unwrap_or(false);
        // Presence is hidden both ways between two people when either has blocked the other.
        let either = |who: &str| blocked(who) || db::is_blocked(db, who, viewer).unwrap_or(false);
        match self {
            Event::Presence { entry } => entry.user != viewer && !either(&entry.user),
            Event::PresenceEnded { user } => user != viewer && !either(user),
            Event::Reaction { from, to, .. } => to == viewer && !blocked(from),
            Event::Chat { message } => message.from != viewer && !blocked(&message.from),
            Event::ChatDeleted { .. } => true,
            Event::ListenerJoined { host, .. } | Event::ListenerLeft { host, .. } => host == viewer,
            Event::ListenEnded { listeners, .. } => listeners.iter().any(|l| l == viewer),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    key: SessionKey,
    db: Arc<Mutex<rusqlite::Connection>>,
    board: Arc<Mutex<PresenceBoard>>,
    events: broadcast::Sender<Event>,
    http: reqwest::Client,
    fails: Arc<Mutex<HashMap<String, (u32, Instant)>>>,
    chat_times: Arc<Mutex<HashMap<String, Vec<u64>>>>,
    /// Who listens along with whom: listener -> host. In memory, like presence.
    listens: Arc<Mutex<HashMap<String, String>>>,
}

impl AppState {
    pub fn new(config: Config, key: SessionKey, db: rusqlite::Connection) -> AppState {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build().expect("http client");
        AppState { config: Arc::new(config), key, db: Arc::new(Mutex::new(db)), board: Arc::default(), events: broadcast::channel(256).0, http, fails: Arc::default(), chat_times: Arc::default(), listens: Arc::default() }
    }

    fn too_many_fails(&self, who: &str) -> bool {
        let mut fails = self.fails.lock().unwrap();
        fails.retain(|_, (_, at)| at.elapsed() < LOGIN_WINDOW);
        fails.get(who).is_some_and(|(n, _)| *n >= LOGIN_FAILS_MAX)
    }

    fn note_fail(&self, who: &str) {
        let mut fails = self.fails.lock().unwrap();
        let e = fails.entry(who.to_string()).or_insert((0, Instant::now()));
        e.0 += 1;
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/activity/v1/login", post(login))
        .route("/activity/v1/me", get(me))
        .route("/activity/v1/me/settings", put(set_settings))
        .route("/activity/v1/presence", get(list_presence).put(put_presence).delete(delete_presence))
        .route("/activity/v1/follows", get(list_follows))
        .route("/activity/v1/follows/{user}", put(follow_user).delete(unfollow_user))
        .route("/activity/v1/feed", get(feed))
        .route("/activity/v1/reactions", get(list_reactions).post(post_reaction))
        .route("/activity/v1/chat", get(list_chat).post(post_chat))
        .route("/activity/v1/chat/{id}", axum::routing::delete(delete_chat))
        .route("/activity/v1/blocks", get(list_blocks))
        .route("/activity/v1/blocks/{user}", put(block_user).delete(unblock_user))
        .route("/activity/v1/listen", get(listen_state).delete(listen_leave))
        .route("/activity/v1/listen/{host}", put(listen_join))
        .route("/activity/v1/health", get(health))
        .route("/activity/v1/ws", get(ws))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(state)
}

struct ApiError(StatusCode, &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}
type ApiResult<T> = Result<T, ApiError>;

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    tracing::error!("internal error: {e}");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

fn user_from(state: &AppState, token: &str) -> ApiResult<String> {
    let now = now_ms() / 1000;
    state.key.verify(token, now).ok_or(ApiError(StatusCode::UNAUTHORIZED, "sign in again"))
}

fn bearer(headers: &HeaderMap) -> ApiResult<String> {
    let value = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).ok_or(ApiError(StatusCode::UNAUTHORIZED, "sign in first"))?;
    Ok(value.to_string())
}

fn authed(state: &AppState, headers: &HeaderMap) -> ApiResult<String> {
    user_from(state, &bearer(headers)?)
}

#[derive(Deserialize)]
struct LoginRequest {
    user: String,
    token: Option<String>,
    salt: Option<String>,
    password: Option<String>,
}

#[derive(Serialize)]
struct LoginResponse {
    session: String,
    expires_at: u64,
}

async fn login(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, Json(req): Json<LoginRequest>) -> ApiResult<Json<LoginResponse>> {
    let user = req.user.trim().to_string();
    if user.is_empty() || user.chars().count() > MAX_NAME || user.chars().any(|c| c.is_control()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "bad user name"));
    }
    let secret_len = req.token.as_deref().map_or(0, str::len) + req.salt.as_deref().map_or(0, str::len) + req.password.as_deref().map_or(0, str::len);
    if secret_len > MAX_SECRET {
        return Err(ApiError(StatusCode::BAD_REQUEST, "credentials too long"));
    }
    let credentials = match (&req.token, &req.salt, &req.password) {
        (Some(t), Some(s), None) if !t.is_empty() && !s.is_empty() => Credentials::TokenSalt { token: t, salt: s },
        (None, None, Some(p)) if !p.is_empty() => Credentials::Password(p),
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "send token and salt, or a password")),
    };
    let (ip_key, user_key) = (format!("ip:{}", peer.ip()), format!("user:{}", user.to_lowercase()));
    if state.too_many_fails(&ip_key) || state.too_many_fails(&user_key) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "too many tries, wait a minute"));
    }
    match navidrome::verify(&state.http, &state.config.navidrome_url, &user, &credentials).await {
        Verdict::Good => {}
        Verdict::Refused => {
            state.note_fail(&ip_key);
            state.note_fail(&user_key);
            return Err(ApiError(StatusCode::UNAUTHORIZED, "the server did not accept that login"));
        }
        Verdict::Unavailable => return Err(ApiError(StatusCode::BAD_GATEWAY, "the music server could not be reached")),
    }
    let now = now_ms() / 1000;
    db::touch_user(&state.db.lock().unwrap(), &user, now).map_err(internal)?;
    let session = state.key.issue(&user, now, state.config.session_ttl_secs);
    Ok(Json(LoginResponse { session, expires_at: now + state.config.session_ttl_secs }))
}

#[derive(Serialize)]
struct Me {
    user: String,
    share_presence: bool,
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Me>> {
    let user = authed(&state, &headers)?;
    let share_presence = db::share_presence(&state.db.lock().unwrap(), &user).map_err(internal)?;
    Ok(Json(Me { user, share_presence }))
}

#[derive(Deserialize)]
struct Settings {
    share_presence: bool,
}

async fn set_settings(State(state): State<AppState>, headers: HeaderMap, Json(s): Json<Settings>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    {
        let conn = state.db.lock().unwrap();
        db::touch_user(&conn, &user, now_ms() / 1000).map_err(internal)?;
        db::set_share_presence(&conn, &user, s.share_presence).map_err(internal)?;
    }
    if !s.share_presence {
        end_presence(&state, &user);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn end_presence(state: &AppState, user: &str) {
    if state.board.lock().unwrap().remove(user) {
        let _ = state.events.send(Event::PresenceEnded { user: user.to_string() });
    }
    release_listeners(state, user);
}

/// Everyone who listened along with `host` is let go (the host stopped, or blocked or was blocked).
fn release_listeners(state: &AppState, host: &str) {
    let gone: Vec<String> = {
        let mut listens = state.listens.lock().unwrap();
        let gone: Vec<String> = listens.iter().filter(|(_, h)| h.as_str() == host).map(|(l, _)| l.clone()).collect();
        for l in &gone {
            listens.remove(l);
        }
        gone
    };
    if !gone.is_empty() {
        let _ = state.events.send(Event::ListenEnded { host: host.to_string(), listeners: gone });
    }
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

/// For load balancers and Docker health checks: no login, and nothing but the version.
async fn health() -> Json<Health> {
    Json(Health { status: "ok", version: env!("CARGO_PKG_VERSION") })
}

#[derive(Serialize)]
struct ListenState {
    listening_to: Option<String>,
    listeners: Vec<String>,
}

/// Listen along with someone who is playing now: you get their current presence back (to start in step) and from
/// then on their presence updates (song, position, play or pause) are what to follow. They are told you joined.
async fn listen_join(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(host): axum::extract::Path<String>) -> ApiResult<Json<activity_core::presence::PresenceEntry>> {
    let user = authed(&state, &headers)?;
    if host == user {
        return Err(ApiError(StatusCode::BAD_REQUEST, "you cannot listen along with yourself"));
    }
    let mut live = state.board.lock().unwrap().visible_to(&user, now_ms());
    hide_blocked(&state, &user, &mut live);
    let entry = live.into_iter().find(|e| e.user == host).ok_or(ApiError(StatusCode::CONFLICT, "that person is not playing anything you can join"))?;
    let previous = state.listens.lock().unwrap().insert(user.clone(), host.clone());
    if let Some(old) = previous.filter(|o| *o != host) {
        let _ = state.events.send(Event::ListenerLeft { host: old, listener: user.clone() });
    }
    let _ = state.events.send(Event::ListenerJoined { host, listener: user });
    Ok(Json(entry))
}

async fn listen_leave(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    if let Some(host) = state.listens.lock().unwrap().remove(&user) {
        let _ = state.events.send(Event::ListenerLeft { host, listener: user });
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Who you listen along with, and who listens along with you (only while those people still play).
async fn listen_state(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<ListenState>> {
    let user = authed(&state, &headers)?;
    let playing: std::collections::HashSet<String> = state.board.lock().unwrap().visible_to("", now_ms()).into_iter().map(|e| e.user).collect();
    let mut listens = state.listens.lock().unwrap();
    listens.retain(|_, host| playing.contains(host));
    let listening_to = listens.get(&user).cloned();
    let mut listeners: Vec<String> = listens.iter().filter(|(_, h)| **h == user).map(|(l, _)| l.clone()).collect();
    listeners.sort();
    Ok(Json(ListenState { listening_to, listeners }))
}

async fn put_presence(State(state): State<AppState>, headers: HeaderMap, Json(update): Json<PresenceUpdate>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    update.validate().map_err(|m| ApiError(StatusCode::BAD_REQUEST, m))?;
    if !db::share_presence(&state.db.lock().unwrap(), &user).map_err(internal)? {
        return Err(ApiError(StatusCode::FORBIDDEN, "sharing is switched off"));
    }
    let entry = state.board.lock().unwrap().set(&user, update, now_ms());
    let _ = state.events.send(Event::Presence { entry });
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_presence(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    end_presence(&state, &user);
    Ok(StatusCode::NO_CONTENT)
}

/// Leave out people who have blocked `viewer`, or whom `viewer` has blocked.
fn hide_blocked(state: &AppState, viewer: &str, entries: &mut Vec<activity_core::presence::PresenceEntry>) {
    let conn = state.db.lock().unwrap();
    entries.retain(|e| !db::is_blocked(&conn, viewer, &e.user).unwrap_or(false) && !db::is_blocked(&conn, &e.user, viewer).unwrap_or(false));
}

async fn list_presence(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Vec<activity_core::presence::PresenceEntry>>> {
    let user = authed(&state, &headers)?;
    let mut all = state.board.lock().unwrap().visible_to(&user, now_ms());
    hide_blocked(&state, &user, &mut all);
    Ok(Json(all))
}

#[derive(Serialize)]
struct Follows {
    following: Vec<String>,
    followers: Vec<String>,
}

async fn list_follows(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Follows>> {
    let user = authed(&state, &headers)?;
    let conn = state.db.lock().unwrap();
    Ok(Json(Follows { following: db::following(&conn, &user).map_err(internal)?, followers: db::followers(&conn, &user).map_err(internal)? }))
}

async fn follow_user(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    if target == user {
        return Err(ApiError(StatusCode::BAD_REQUEST, "you cannot follow yourself"));
    }
    let conn = state.db.lock().unwrap();
    if !db::user_exists(&conn, &target).map_err(internal)? {
        return Err(ApiError(StatusCode::NOT_FOUND, "that person has not used the activity service yet"));
    }
    db::follow(&conn, &user, &target, now_ms() / 1000).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unfollow_user(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    db::unfollow(&state.db.lock().unwrap(), &user, &target).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

/// What the people you follow are playing now.
async fn feed(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Vec<activity_core::presence::PresenceEntry>>> {
    let user = authed(&state, &headers)?;
    let following = db::following(&state.db.lock().unwrap(), &user).map_err(internal)?;
    let mut all = state.board.lock().unwrap().visible_to(&user, now_ms());
    all.retain(|e| following.contains(&e.user));
    hide_blocked(&state, &user, &mut all);
    Ok(Json(all))
}

#[derive(Deserialize)]
struct ReactionRequest {
    to: String,
    song_id: String,
    kind: String,
}

/// React to what someone is playing right now. The person must be playing that very song (so reactions cannot be
/// aimed at nothing), cannot be yourself, and must not have blocked you.
async fn post_reaction(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<ReactionRequest>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    let kind = activity_core::ReactionKind::parse(&req.kind).ok_or(ApiError(StatusCode::BAD_REQUEST, "unknown reaction"))?;
    if req.to == user {
        return Err(ApiError(StatusCode::BAD_REQUEST, "you cannot react to yourself"));
    }
    let playing_it = state.board.lock().unwrap().visible_to(&user, now_ms()).iter().any(|e| e.user == req.to && e.presence.song_id == req.song_id);
    if !playing_it {
        return Err(ApiError(StatusCode::CONFLICT, "that person is not playing that song now"));
    }
    let now = now_ms();
    let is_new = {
        let conn = state.db.lock().unwrap();
        if db::is_blocked(&conn, &req.to, &user).map_err(internal)? {
            // Do not tell the sender they are blocked: the reaction simply goes nowhere.
            return Ok(StatusCode::NO_CONTENT);
        }
        db::add_reaction(&conn, &user, &req.to, &req.song_id, kind.as_str(), now).map_err(internal)?
    };
    if is_new {
        let _ = state.events.send(Event::Reaction { from: user, to: req.to, song_id: req.song_id, kind });
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct ReactionOut {
    from: String,
    song_id: String,
    kind: String,
    created_at_ms: u64,
}

/// Reactions you received in the last 7 days, newest first (for a client that was offline).
async fn list_reactions(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Vec<ReactionOut>>> {
    let user = authed(&state, &headers)?;
    let since = now_ms().saturating_sub(7 * 24 * 3600 * 1000);
    let rows = db::reactions_for(&state.db.lock().unwrap(), &user, since, 50).map_err(internal)?;
    Ok(Json(rows.into_iter().map(|r| ReactionOut { from: r.from, song_id: r.song_id, kind: r.kind, created_at_ms: r.created_at_ms }).collect()))
}

#[derive(Deserialize)]
struct ChatRequest {
    body: String,
}

async fn post_chat(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<ChatRequest>) -> ApiResult<Json<activity_core::ChatMessage>> {
    let user = authed(&state, &headers)?;
    let body = activity_core::social::clean_chat_body(&req.body).map_err(|m| ApiError(StatusCode::BAD_REQUEST, m))?;
    let now = now_ms();
    {
        let mut times = state.chat_times.lock().unwrap();
        if !activity_core::social::chat_allowed(times.entry(user.clone()).or_default(), now) {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "slow down a little"));
        }
    }
    let id = {
        let conn = state.db.lock().unwrap();
        db::touch_user(&conn, &user, now / 1000).map_err(internal)?;
        db::add_message(&conn, &user, &body, now, activity_core::social::CHAT_KEEP_MS).map_err(internal)?
    };
    let message = activity_core::ChatMessage { id, from: user, body, created_at_ms: now };
    let _ = state.events.send(Event::Chat { message: message.clone() });
    Ok(Json(message))
}

#[derive(Deserialize)]
struct ChatQuery {
    #[serde(default)]
    after: i64,
    limit: Option<u32>,
}

async fn list_chat(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<ChatQuery>) -> ApiResult<Json<Vec<activity_core::ChatMessage>>> {
    let user = authed(&state, &headers)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    Ok(Json(db::messages_for(&state.db.lock().unwrap(), &user, q.after, limit).map_err(internal)?))
}

async fn delete_chat(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(id): axum::extract::Path<i64>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    if !db::delete_own_message(&state.db.lock().unwrap(), &user, id).map_err(internal)? {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such message of yours"));
    }
    let _ = state.events.send(Event::ChatDeleted { id });
    Ok(StatusCode::NO_CONTENT)
}

async fn list_blocks(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Vec<String>>> {
    let user = authed(&state, &headers)?;
    Ok(Json(db::blocked_by(&state.db.lock().unwrap(), &user).map_err(internal)?))
}

async fn block_user(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    if target == user {
        return Err(ApiError(StatusCode::BAD_REQUEST, "you cannot block yourself"));
    }
    let conn = state.db.lock().unwrap();
    db::block(&conn, &user, &target).map_err(internal)?;
    // Blocking also ends following in both directions.
    let _ = db::unfollow(&conn, &user, &target);
    let _ = db::unfollow(&conn, &target, &user);
    drop(conn);
    // ...and listening along, both ways.
    let was_mine = {
        let mut listens = state.listens.lock().unwrap();
        let mine = (listens.get(&user).map(String::as_str) == Some(target.as_str())).then(|| listens.remove(&user)).flatten();
        let theirs = (listens.get(&target).map(String::as_str) == Some(user.as_str())).then(|| listens.remove(&target)).flatten();
        (mine, theirs)
    };
    if was_mine.0.is_some() {
        let _ = state.events.send(Event::ListenerLeft { host: target.clone(), listener: user.clone() });
    }
    if was_mine.1.is_some() {
        let _ = state.events.send(Event::ListenerLeft { host: user.clone(), listener: target.clone() });
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn unblock_user(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    db::unblock(&state.db.lock().unwrap(), &user, &target).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct WsQuery {
    session: String,
}

async fn ws(State(state): State<AppState>, Query(q): Query<WsQuery>, upgrade: WebSocketUpgrade) -> ApiResult<Response> {
    let user = user_from(&state, &q.session)?;
    Ok(upgrade.max_message_size(4096).on_upgrade(move |socket| ws_loop(state, user, socket)))
}

async fn ws_loop(state: AppState, user: String, mut socket: WebSocket) {
    let mut events = state.events.subscribe();
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => {
                    let wanted = event.is_for(&user, &state.db.lock().unwrap());
                    if !wanted { continue }
                    let Ok(text) = serde_json::to_string(&event) else { continue };
                    if socket.send(Message::Text(text.into())).await.is_err() { break }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {} // clients send nothing over the socket; ignore anything else
            },
        }
    }
}
