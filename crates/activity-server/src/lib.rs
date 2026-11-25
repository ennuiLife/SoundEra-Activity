//! SoundEra Activity: login through Navidrome, then presence (what each person plays now) over HTTP and WebSocket.

pub mod config;
pub mod db;
pub mod navidrome;
pub mod update;

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
/// The biggest picture a person may upload (the service keeps a small square PNG made from it).
const MAX_AVATAR_UPLOAD: usize = 4 * 1024 * 1024;
/// The side of the stored square picture.
const AVATAR_SIDE: u32 = 256;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Presence { entry: activity_core::presence::PresenceEntry },
    PresenceEnded { user: String },
    Reaction { from: String, to: String, song_id: String, kind: String },
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
            Event::Chat { message } => message.from != viewer && !blocked(&message.from) && message.to.as_deref().is_none_or(|t| t == viewer),
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
    /// Parties and their invitations. In memory, like presence.
    parties: Arc<Mutex<activity_core::party::Parties>>,
}

impl AppState {
    pub fn new(config: Config, key: SessionKey, db: rusqlite::Connection) -> AppState {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build().expect("http client");
        AppState { config: Arc::new(config), key, db: Arc::new(Mutex::new(db)), board: Arc::default(), events: broadcast::channel(256).0, http, fails: Arc::default(), chat_times: Arc::default(), listens: Arc::default(), parties: Arc::default() }
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
        .route("/activity/v1/profile", put(put_profile))
        .route("/activity/v1/profile/{user}", get(get_profile))
        .route("/activity/v1/avatar", put(put_avatar).delete(delete_avatar).layer(DefaultBodyLimit::max(MAX_AVATAR_UPLOAD)))
        .route("/activity/v1/avatar/{user}", get(get_avatar))
        .route("/activity/v1/blocks", get(list_blocks))
        .route("/activity/v1/blocks/{user}", put(block_user).delete(unblock_user))
        .route("/activity/v1/listen", get(listen_state).delete(listen_leave))
        .route("/activity/v1/listen/{host}", put(listen_join))
        .route("/activity/v1/party", get(party_state).delete(party_leave))
        .route("/activity/v1/party/invite/{user}", put(party_invite))
        .route("/activity/v1/party/accept/{id}", put(party_accept))
        .route("/activity/v1/party/invites/{id}", axum::routing::delete(party_decline))
        .route("/activity/v1/party/leader/{user}", put(party_set_leader))
        .route("/activity/v1/party/members/{user}", axum::routing::delete(party_kick))
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
        let _ = db::delete_profile(&state.db.lock().unwrap(), &user);
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
    if !activity_core::social::valid_reaction(&req.kind) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "unknown reaction"));
    }
    let kind = req.kind.clone();
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
        db::add_reaction(&conn, &user, &req.to, &req.song_id, &kind, now).map_err(internal)?
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
    /// The id of the message this one replies to.
    #[serde(default)]
    reply_to: Option<i64>,
    /// A whisper to this person: only they and the writer see it.
    #[serde(default)]
    to: Option<String>,
    /// A song suggested to `to` (a whisper that carries a song).
    #[serde(default)]
    song_id: Option<String>,
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
    let (id, reply) = {
        let conn = state.db.lock().unwrap();
        db::touch_user(&conn, &user, now / 1000).map_err(internal)?;
        if let Some(target) = req.reply_to {
            if !db::message_exists(&conn, target).map_err(internal)? {
                return Err(ApiError(StatusCode::BAD_REQUEST, "that message is gone"));
            }
        }
        if req.song_id.as_ref().is_some_and(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control)) || (req.song_id.is_some() && req.to.is_none()) {
            return Err(ApiError(StatusCode::BAD_REQUEST, "a suggestion needs a song and a person"));
        }
        if let Some(to) = &req.to {
            if *to == user {
                return Err(ApiError(StatusCode::BAD_REQUEST, "you cannot whisper to yourself"));
            }
            if req.reply_to.is_some() {
                return Err(ApiError(StatusCode::BAD_REQUEST, "a whisper cannot be a reply"));
            }
            if !db::user_exists(&conn, to).map_err(internal)? {
                return Err(ApiError(StatusCode::NOT_FOUND, "that person has not used the activity service yet"));
            }
            if db::is_blocked(&conn, &user, to).map_err(internal)? || db::is_blocked(&conn, to, &user).map_err(internal)? {
                return Err(ApiError(StatusCode::FORBIDDEN, "you cannot whisper to that person"));
            }
        }
        let id = db::add_message(&conn, &user, &body, req.reply_to, req.to.as_deref(), req.song_id.as_deref(), now, activity_core::social::CHAT_KEEP_MS).map_err(internal)?;
        let reply = match req.reply_to {
            Some(target) => Some(db::reply_ref(&conn, target).map_err(internal)?),
            None => None,
        };
        (id, reply)
    };
    let message = activity_core::ChatMessage { id, from: user, body, created_at_ms: now, reply, to: req.to.clone(), song_id: req.song_id.clone() };
    let _ = state.events.send(Event::Chat { message: message.clone() });
    Ok(Json(message))
}

/// Send your own profile summary (plays, liked songs, playlists, top artists and genres) so other people can see it.
/// Only while sharing is on; switching sharing off removes it.
async fn put_profile(State(state): State<AppState>, headers: HeaderMap, Json(summary): Json<activity_core::ProfileSummary>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    summary.validate().map_err(|m| ApiError(StatusCode::BAD_REQUEST, m))?;
    let conn = state.db.lock().unwrap();
    if !db::share_presence(&conn, &user).map_err(internal)? {
        return Err(ApiError(StatusCode::FORBIDDEN, "sharing is switched off"));
    }
    let json = serde_json::to_string(&summary).map_err(internal)?;
    db::put_profile(&conn, &user, &json, now_ms()).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct ProfileOut {
    user: String,
    summary: activity_core::ProfileSummary,
    updated_at_ms: u64,
    followers: usize,
    following: usize,
    you_follow: bool,
    playing_now: bool,
}

/// Someone else's profile. Hidden (404) when they share nothing, or when either of you blocked the other.
async fn get_profile(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<Json<ProfileOut>> {
    let user = authed(&state, &headers)?;
    let not_found = ApiError(StatusCode::NOT_FOUND, "no profile to show");
    let (summary, updated_at_ms, followers, following, you_follow) = {
        let conn = state.db.lock().unwrap();
        if target != user
            && (db::is_blocked(&conn, &user, &target).map_err(internal)? || db::is_blocked(&conn, &target, &user).map_err(internal)? || !db::share_presence(&conn, &target).map_err(internal)?)
        {
            return Err(not_found);
        }
        let Some((json, at)) = db::get_profile(&conn, &target).map_err(internal)? else { return Err(not_found) };
        let summary: activity_core::ProfileSummary = serde_json::from_str(&json).map_err(internal)?;
        let followers = db::followers(&conn, &target).map_err(internal)?;
        let following = db::following(&conn, &target).map_err(internal)?;
        (summary, at, followers.len(), following.len(), followers.contains(&user))
    };
    let playing_now = state.board.lock().unwrap().visible_to(&user, now_ms()).iter().any(|e| e.user == target);
    Ok(Json(ProfileOut { user: target, summary, updated_at_ms, followers, following, you_follow, playing_now }))
}

/// Turn an uploaded picture (PNG or JPEG, any size up to the limits) into a small square PNG, or say why not.
fn make_avatar(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    use image::{ImageFormat, ImageReader};
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|_| "that is not a picture")?;
    if !matches!(reader.format(), Some(ImageFormat::Png) | Some(ImageFormat::Jpeg)) {
        return Err("only PNG and JPEG pictures are accepted");
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let picture = reader.decode().map_err(|_| "that picture could not be read")?;
    let (w, h) = (picture.width(), picture.height());
    if w == 0 || h == 0 {
        return Err("that picture is empty");
    }
    // The middle square, made small.
    let side = w.min(h);
    let square = picture.crop_imm((w - side) / 2, (h - side) / 2, side, side).resize_exact(AVATAR_SIDE, AVATAR_SIDE, image::imageops::FilterType::Lanczos3);
    let mut out = std::io::Cursor::new(Vec::new());
    square.write_to(&mut out, ImageFormat::Png).map_err(|_| "that picture could not be saved")?;
    Ok(out.into_inner())
}

/// Set your own picture: send a PNG or JPEG as the request body (up to 4 MB).
async fn put_avatar(State(state): State<AppState>, headers: HeaderMap, body: axum::body::Bytes) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    let png = make_avatar(&body).map_err(|m| ApiError(StatusCode::BAD_REQUEST, m))?;
    let conn = state.db.lock().unwrap();
    db::touch_user(&conn, &user, now_ms() / 1000).map_err(internal)?;
    db::put_avatar(&conn, &user, &png, now_ms()).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_avatar(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    db::delete_avatar(&state.db.lock().unwrap(), &user).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Someone's picture as a PNG (404 when they have none, or when either of you blocked the other).
async fn get_avatar(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<Response> {
    let user = authed(&state, &headers)?;
    let conn = state.db.lock().unwrap();
    if target != user && (db::is_blocked(&conn, &user, &target).map_err(internal)? || db::is_blocked(&conn, &target, &user).map_err(internal)?) {
        return Err(ApiError(StatusCode::NOT_FOUND, "no picture"));
    }
    let Some((png, at)) = db::get_avatar(&conn, &target).map_err(internal)? else { return Err(ApiError(StatusCode::NOT_FOUND, "no picture")) };
    Ok(([(header::CONTENT_TYPE, "image/png".to_string()), (header::ETAG, format!("\"{at}\"")), (header::CACHE_CONTROL, "private, max-age=60".to_string())], png).into_response())
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
    // ...and sharing a party: the blocked person is sent away (or you leave, when they are the DJ).
    state.parties.lock().unwrap().separate(&user, &target);
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

// ---- Parties: a few people, one DJ who decides what plays for everyone. Followers poll `GET /party`. ----

#[derive(Serialize)]
struct PartyOut {
    party: Option<activity_core::party::PartyView>,
    invites: Vec<activity_core::party::InviteView>,
}

fn party_error(e: activity_core::party::PartyError) -> ApiError {
    use activity_core::party::PartyError as E;
    let code = match e {
        E::NotLeader => StatusCode::FORBIDDEN,
        E::NotInParty | E::NoSuchInvite | E::NotMember => StatusCode::NOT_FOUND,
        _ => StatusCode::CONFLICT,
    };
    ApiError(code, e.message())
}

async fn party_state(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<PartyOut>> {
    let user = authed(&state, &headers)?;
    let mut parties = state.parties.lock().unwrap();
    let invites = parties.invites_for(&user, now_ms());
    Ok(Json(PartyOut { party: parties.party_of(&user), invites }))
}

/// Only people you follow can be invited, and never someone who blocked you or whom you blocked.
async fn party_invite(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    {
        let conn = state.db.lock().unwrap();
        if !db::following(&conn, &user).map_err(internal)?.contains(&target) {
            return Err(ApiError(StatusCode::FORBIDDEN, "you can only invite people you follow"));
        }
        if db::is_blocked(&conn, &user, &target).map_err(internal)? || db::is_blocked(&conn, &target, &user).map_err(internal)? {
            return Err(ApiError(StatusCode::FORBIDDEN, "you cannot invite that person"));
        }
    }
    state.parties.lock().unwrap().invite(&user, &target, now_ms()).map_err(party_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn party_accept(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(id): axum::extract::Path<u64>) -> ApiResult<Json<activity_core::party::PartyView>> {
    let user = authed(&state, &headers)?;
    let view = state.parties.lock().unwrap().accept(&user, id, now_ms()).map_err(party_error)?;
    Ok(Json(view))
}

async fn party_decline(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(id): axum::extract::Path<u64>) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    state.parties.lock().unwrap().decline(&user, id);
    Ok(StatusCode::NO_CONTENT)
}

async fn party_leave(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let user = authed(&state, &headers)?;
    state.parties.lock().unwrap().leave(&user);
    Ok(StatusCode::NO_CONTENT)
}

async fn party_set_leader(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<Json<activity_core::party::PartyView>> {
    let user = authed(&state, &headers)?;
    let view = state.parties.lock().unwrap().set_leader(&user, &target).map_err(party_error)?;
    Ok(Json(view))
}

async fn party_kick(State(state): State<AppState>, headers: HeaderMap, axum::extract::Path(target): axum::extract::Path<String>) -> ApiResult<Json<activity_core::party::PartyView>> {
    let user = authed(&state, &headers)?;
    let view = state.parties.lock().unwrap().kick(&user, &target).map_err(party_error)?;
    Ok(Json(view))
}
