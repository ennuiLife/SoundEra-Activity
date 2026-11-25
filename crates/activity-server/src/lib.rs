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
}

impl Event {
    fn user(&self) -> &str {
        match self {
            Event::Presence { entry } => &entry.user,
            Event::PresenceEnded { user } => user,
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
}

impl AppState {
    pub fn new(config: Config, key: SessionKey, db: rusqlite::Connection) -> AppState {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build().expect("http client");
        AppState { config: Arc::new(config), key, db: Arc::new(Mutex::new(db)), board: Arc::default(), events: broadcast::channel(256).0, http, fails: Arc::default() }
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

async fn list_presence(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Vec<activity_core::presence::PresenceEntry>>> {
    let user = authed(&state, &headers)?;
    Ok(Json(state.board.lock().unwrap().visible_to(&user, now_ms())))
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
    Ok(Json(all))
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
                Ok(event) if event.user() != user => {
                    let Ok(text) = serde_json::to_string(&event) else { continue };
                    if socket.send(Message::Text(text.into())).await.is_err() { break }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {} // clients send nothing over the socket; ignore anything else
            },
        }
    }
}
