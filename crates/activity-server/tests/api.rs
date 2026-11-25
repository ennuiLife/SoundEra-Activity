//! The whole service against a fake Navidrome. Users and songs here are made up.

use activity_core::SessionKey;
use activity_server::{config::Config, db, router, AppState};
use axum::{extract::Query, routing::get, Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{collections::HashMap, net::SocketAddr};

/// A stand-in for Navidrome's `ping`: accepts alice/bob with token "good-token" or password "secret".
async fn fake_navidrome() -> String {
    async fn ping(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        let known = matches!(q.get("u").map(String::as_str), Some("alice") | Some("bob") | Some("cara"));
        let good = known && (q.get("t").map(String::as_str) == Some("good-token") && q.contains_key("s") || q.get("p").map(String::as_str) == Some("secret"));
        Json(if good { json!({"subsonic-response": {"status": "ok", "version": "1.16.1"}}) } else { json!({"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "Wrong username or password"}}}) })
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, Router::new().route("/rest/ping", get(ping))).await.unwrap() });
    format!("http://{addr}")
}

struct Service {
    base: String,
    http: reqwest::Client,
}

async fn start_with(navidrome_url: String) -> Service {
    let config = Config { navidrome_url, bind: String::new(), data_dir: "unused".into(), session_ttl_secs: 3600 };
    let state = AppState::new(config, SessionKey::generate(), db::open_in_memory().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
    Service { base: format!("http://{addr}/activity/v1"), http: reqwest::Client::new() }
}

async fn start() -> Service {
    start_with(fake_navidrome().await).await
}

impl Service {
    async fn login(&self, user: &str) -> String {
        let r = self.http.post(format!("{}/login", self.base)).json(&json!({"user": user, "token": "good-token", "salt": "abc"})).send().await.unwrap();
        assert_eq!(r.status(), 200, "login for {user}");
        r.json::<Value>().await.unwrap()["session"].as_str().unwrap().to_string()
    }

    fn song(title: &str) -> Value {
        json!({"song_id": "s1", "title": title, "artist": "Fictional Band", "album": "Made Up", "position_ms": 5000, "duration_ms": 200000, "state": "playing", "device": "Desktop"})
    }

    async fn put_presence(&self, session: &str, body: Value) -> reqwest::Response {
        self.http.put(format!("{}/presence", self.base)).bearer_auth(session).json(&body).send().await.unwrap()
    }

    async fn presence(&self, session: &str) -> Vec<Value> {
        self.http.get(format!("{}/presence", self.base)).bearer_auth(session).send().await.unwrap().json().await.unwrap()
    }
}

#[tokio::test]
async fn a_good_login_works_with_token_or_password_and_a_bad_one_does_not() {
    let s = start().await;
    s.login("alice").await;
    let r = s.http.post(format!("{}/login", s.base)).json(&json!({"user": "bob", "password": "secret"})).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = s.http.post(format!("{}/login", s.base)).json(&json!({"user": "alice", "token": "bad", "salt": "x"})).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = s.http.post(format!("{}/login", s.base)).json(&json!({"user": "mallory", "password": "secret"})).send().await.unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn malformed_logins_are_refused_before_asking_navidrome() {
    let s = start().await;
    for body in [json!({"user": "", "password": "x"}), json!({"user": "alice"}), json!({"user": "alice", "token": "t"}), json!({"user": "alice", "token": "t", "salt": "s", "password": "p"}), json!({"user": "a\nb", "password": "x"}), json!({"user": "alice", "password": "x".repeat(600)})] {
        let r = s.http.post(format!("{}/login", s.base)).json(&body).send().await.unwrap();
        assert_eq!(r.status(), 400, "{body}");
    }
}

#[tokio::test]
async fn too_many_wrong_logins_are_slowed_down() {
    let s = start().await;
    for _ in 0..5 {
        let r = s.http.post(format!("{}/login", s.base)).json(&json!({"user": "alice", "token": "bad", "salt": "x"})).send().await.unwrap();
        assert_eq!(r.status(), 401);
    }
    // Even the right login waits for the window to pass.
    let r = s.http.post(format!("{}/login", s.base)).json(&json!({"user": "alice", "token": "good-token", "salt": "x"})).send().await.unwrap();
    assert_eq!(r.status(), 429);
}

#[tokio::test]
async fn an_unreachable_navidrome_is_a_bad_gateway_not_a_wrong_password() {
    let s = start_with("http://127.0.0.1:1".into()).await;
    let r = s.http.post(format!("{}/login", s.base)).json(&json!({"user": "alice", "password": "secret"})).send().await.unwrap();
    assert_eq!(r.status(), 502);
}

#[tokio::test]
async fn nothing_works_without_a_valid_session() {
    let s = start().await;
    assert_eq!(s.http.get(format!("{}/presence", s.base)).send().await.unwrap().status(), 401);
    assert_eq!(s.http.get(format!("{}/me", s.base)).bearer_auth("nonsense").send().await.unwrap().status(), 401);
    assert_eq!(s.put_presence("nonsense", Service::song("x")).await.status(), 401);
}

#[tokio::test]
async fn people_see_each_others_presence_but_not_their_own() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    assert_eq!(s.put_presence(&a, Service::song("One")).await.status(), 204);
    assert_eq!(s.put_presence(&b, Service::song("Two")).await.status(), 204);
    let seen_by_alice = s.presence(&a).await;
    assert_eq!(seen_by_alice.len(), 1);
    assert_eq!(seen_by_alice[0]["user"], "bob");
    assert_eq!(seen_by_alice[0]["title"], "Two");
    assert_eq!(s.presence(&b).await[0]["user"], "alice");
    // Clearing ends it at once.
    assert_eq!(s.http.delete(format!("{}/presence", s.base)).bearer_auth(&b).send().await.unwrap().status(), 204);
    assert!(s.presence(&a).await.is_empty());
}

#[tokio::test]
async fn switching_sharing_off_hides_you_and_refuses_new_updates() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    s.put_presence(&b, Service::song("Two")).await;
    assert_eq!(s.presence(&a).await.len(), 1);
    let r = s.http.put(format!("{}/me/settings", s.base)).bearer_auth(&b).json(&json!({"share_presence": false})).send().await.unwrap();
    assert_eq!(r.status(), 204);
    assert!(s.presence(&a).await.is_empty());
    assert_eq!(s.put_presence(&b, Service::song("Two")).await.status(), 403);
    let me: Value = s.http.get(format!("{}/me", s.base)).bearer_auth(&b).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["share_presence"], false);
    // And back on.
    s.http.put(format!("{}/me/settings", s.base)).bearer_auth(&b).json(&json!({"share_presence": true})).send().await.unwrap();
    assert_eq!(s.put_presence(&b, Service::song("Two")).await.status(), 204);
}

#[tokio::test]
async fn bad_presence_is_refused() {
    let s = start().await;
    let a = s.login("alice").await;
    assert_eq!(s.put_presence(&a, Service::song(&"x".repeat(400))).await.status(), 400);
    assert_eq!(s.put_presence(&a, Service::song("line\nbreak")).await.status(), 400);
    assert_eq!(s.put_presence(&a, json!({"title": "no song id"})).await.status(), 422);
    let big = "x".repeat(20_000);
    assert_eq!(s.http.put(format!("{}/presence", s.base)).bearer_auth(&a).header("content-type", "application/json").body(format!("{{\"title\":\"{big}\"}}")).send().await.unwrap().status(), 413);
}

#[tokio::test]
async fn the_websocket_pushes_others_presence_and_its_end() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let url = s.base.replace("http://", "ws://") + &format!("/ws?session={a}");
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    // Alice's own updates are not echoed to her; Bob's are pushed.
    s.put_presence(&a, Service::song("Mine")).await;
    s.put_presence(&b, Service::song("Two")).await;
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next()).await.unwrap().unwrap().unwrap();
    let v: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(v["type"], "presence");
    assert_eq!(v["entry"]["user"], "bob");
    assert_eq!(v["entry"]["title"], "Two");
    s.http.delete(format!("{}/presence", s.base)).bearer_auth(&b).send().await.unwrap();
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next()).await.unwrap().unwrap().unwrap();
    let v: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(v, json!({"type": "presence_ended", "user": "bob"}));
}

#[tokio::test]
async fn the_websocket_needs_a_valid_session() {
    let s = start().await;
    let url = s.base.replace("http://", "ws://") + "/ws?session=nonsense";
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
}

#[tokio::test]
async fn follows_make_a_feed_of_the_people_you_follow() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    s.put_presence(&b, Service::song("Two")).await;
    // Unknown people and yourself cannot be followed.
    assert_eq!(s.http.put(format!("{}/follows/nobody", s.base)).bearer_auth(&a).send().await.unwrap().status(), 404);
    assert_eq!(s.http.put(format!("{}/follows/alice", s.base)).bearer_auth(&a).send().await.unwrap().status(), 400);
    // The feed is empty until you follow someone, then shows them.
    let feed = |session: String| {
        let (http, base) = (s.http.clone(), s.base.clone());
        async move { http.get(format!("{base}/feed")).bearer_auth(session).send().await.unwrap().json::<Vec<Value>>().await.unwrap() }
    };
    assert!(feed(a.clone()).await.is_empty());
    assert_eq!(s.http.put(format!("{}/follows/bob", s.base)).bearer_auth(&a).send().await.unwrap().status(), 204);
    assert_eq!(s.http.put(format!("{}/follows/bob", s.base)).bearer_auth(&a).send().await.unwrap().status(), 204); // again is fine
    assert_eq!(feed(a.clone()).await[0]["user"], "bob");
    let f: Value = s.http.get(format!("{}/follows", s.base)).bearer_auth(&a).send().await.unwrap().json().await.unwrap();
    assert_eq!(f, json!({"following": ["bob"], "followers": []}));
    let f: Value = s.http.get(format!("{}/follows", s.base)).bearer_auth(&b).send().await.unwrap().json().await.unwrap();
    assert_eq!(f, json!({"following": [], "followers": ["alice"]}));
    // Unfollowing empties the feed again; bob's presence itself is unchanged.
    assert_eq!(s.http.delete(format!("{}/follows/bob", s.base)).bearer_auth(&a).send().await.unwrap().status(), 204);
    assert!(feed(a.clone()).await.is_empty());
    assert_eq!(s.presence(&a).await.len(), 1);
}

async fn next_event(socket: &mut (impl futures_util::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>> + Unpin)) -> Value {
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next()).await.unwrap().unwrap().unwrap();
    serde_json::from_str(msg.to_text().unwrap()).unwrap()
}

async fn nothing_more(socket: &mut (impl futures_util::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>> + Unpin)) {
    assert!(tokio::time::timeout(std::time::Duration::from_millis(300), socket.next()).await.is_err(), "an event arrived that should not have");
}

impl Service {
    async fn react(&self, session: &str, to: &str, song_id: &str, kind: &str) -> reqwest::Response {
        self.http.post(format!("{}/reactions", self.base)).bearer_auth(session).json(&json!({"to": to, "song_id": song_id, "kind": kind})).send().await.unwrap()
    }
    async fn chat(&self, session: &str, body: &str) -> reqwest::Response {
        self.http.post(format!("{}/chat", self.base)).bearer_auth(session).json(&json!({"body": body})).send().await.unwrap()
    }
    async fn messages(&self, session: &str) -> Vec<Value> {
        self.http.get(format!("{}/chat", self.base)).bearer_auth(session).send().await.unwrap().json().await.unwrap()
    }
}

#[tokio::test]
async fn a_reaction_reaches_only_the_person_playing_the_song() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let url = |session: &str| s.base.replace("http://", "ws://") + &format!("/ws?session={session}");
    let (mut ws_b, _) = tokio_tungstenite::connect_async(url(&b)).await.unwrap();
    let (mut ws_a, _) = tokio_tungstenite::connect_async(url(&a)).await.unwrap();
    s.put_presence(&b, Service::song("Two")).await;
    assert_eq!(next_event(&mut ws_a).await["type"], "presence");
    // Not playing it, yourself, unknown kinds: refused.
    assert_eq!(s.react(&a, "bob", "other-song", "like").await.status(), 409);
    assert_eq!(s.react(&a, "alice", "s1", "like").await.status(), 400);
    assert_eq!(s.react(&a, "bob", "s1", "angry").await.status(), 400);
    assert_eq!(s.react(&a, "nobody", "s1", "like").await.status(), 409);
    // The real thing: bob gets it live, alice gets no echo, and it is in his inbox.
    assert_eq!(s.react(&a, "bob", "s1", "fire").await.status(), 204);
    let v = next_event(&mut ws_b).await;
    assert_eq!((v["type"].as_str(), v["from"].as_str(), v["kind"].as_str(), v["song_id"].as_str()), (Some("reaction"), Some("alice"), Some("fire"), Some("s1")));
    nothing_more(&mut ws_a).await;
    // The same reaction again is accepted but changes nothing (no second event, one inbox row).
    assert_eq!(s.react(&a, "bob", "s1", "fire").await.status(), 204);
    nothing_more(&mut ws_b).await;
    let inbox: Vec<Value> = s.http.get(format!("{}/reactions", s.base)).bearer_auth(&b).send().await.unwrap().json().await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["from"], "alice");
}

#[tokio::test]
async fn chat_is_posted_listed_limited_and_deleted_only_by_its_writer() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let r = s.chat(&a, "  hello everyone  ").await;
    assert_eq!(r.status(), 200);
    let first: Value = r.json().await.unwrap();
    assert_eq!((first["from"].as_str(), first["body"].as_str()), (Some("alice"), Some("hello everyone")));
    s.chat(&b, "hi alice").await;
    let list = s.messages(&b).await;
    assert_eq!(list.iter().map(|m| m["body"].as_str().unwrap()).collect::<Vec<_>>(), ["hello everyone", "hi alice"]);
    // After an id, only newer ones.
    let id = first["id"].as_i64().unwrap();
    let newer: Vec<Value> = s.http.get(format!("{}/chat?after={id}", s.base)).bearer_auth(&a).send().await.unwrap().json().await.unwrap();
    assert_eq!(newer.len(), 1);
    // Bad text.
    assert_eq!(s.chat(&a, "   ").await.status(), 400);
    assert_eq!(s.chat(&a, &"x".repeat(501)).await.status(), 400);
    assert_eq!(s.chat(&a, "bell\u{7}").await.status(), 400);
    // Only the writer can delete.
    assert_eq!(s.http.delete(format!("{}/chat/{id}", s.base)).bearer_auth(&b).send().await.unwrap().status(), 404);
    assert_eq!(s.http.delete(format!("{}/chat/{id}", s.base)).bearer_auth(&a).send().await.unwrap().status(), 204);
    assert_eq!(s.messages(&b).await.len(), 1);
}

#[tokio::test]
async fn chat_is_rate_limited_per_person() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    for i in 0..5 {
        assert_eq!(s.chat(&a, &format!("m{i}")).await.status(), 200);
    }
    assert_eq!(s.chat(&a, "one too many").await.status(), 429);
    assert_eq!(s.chat(&b, "bob is not limited by alice").await.status(), 200);
}

#[tokio::test]
async fn chat_goes_live_to_others_but_not_to_the_writer() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let url = |session: &str| s.base.replace("http://", "ws://") + &format!("/ws?session={session}");
    let (mut ws_a, _) = tokio_tungstenite::connect_async(url(&a)).await.unwrap();
    let (mut ws_b, _) = tokio_tungstenite::connect_async(url(&b)).await.unwrap();
    let id = s.chat(&a, "hello").await.json::<Value>().await.unwrap()["id"].as_i64().unwrap();
    let v = next_event(&mut ws_b).await;
    assert_eq!((v["type"].as_str(), v["message"]["body"].as_str()), (Some("chat"), Some("hello")));
    nothing_more(&mut ws_a).await;
    s.http.delete(format!("{}/chat/{id}", s.base)).bearer_auth(&a).send().await.unwrap();
    assert_eq!(next_event(&mut ws_b).await, json!({"type": "chat_deleted", "id": id}));
}

#[tokio::test]
async fn blocking_hides_chat_reactions_presence_and_follows_both_ways() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    s.put_presence(&b, Service::song("Two")).await;
    s.http.put(format!("{}/follows/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    s.chat(&b, "annoying message").await;
    assert_eq!(s.http.put(format!("{}/blocks/alice", s.base)).bearer_auth(&a).send().await.unwrap().status(), 400);
    assert_eq!(s.http.put(format!("{}/blocks/bob", s.base)).bearer_auth(&a).send().await.unwrap().status(), 204);
    let blocks: Vec<String> = s.http.get(format!("{}/blocks", s.base)).bearer_auth(&a).send().await.unwrap().json().await.unwrap();
    assert_eq!(blocks, ["bob"]);
    // Alice no longer sees Bob's chat, presence or feed entry, and Bob no longer sees Alice's presence either way.
    assert!(s.messages(&a).await.is_empty());
    assert_eq!(s.messages(&b).await.len(), 1, "bob still sees the room");
    assert!(s.presence(&a).await.is_empty());
    s.put_presence(&a, Service::song("One")).await;
    assert!(s.presence(&b).await.is_empty());
    let f: Value = s.http.get(format!("{}/follows", s.base)).bearer_auth(&a).send().await.unwrap().json().await.unwrap();
    assert_eq!(f["following"], json!([]));
    // Bob reacting to Alice goes nowhere (and he is not told).
    assert_eq!(s.react(&b, "alice", "s1", "like").await.status(), 204);
    let inbox: Vec<Value> = s.http.get(format!("{}/reactions", s.base)).bearer_auth(&a).send().await.unwrap().json().await.unwrap();
    assert!(inbox.is_empty(), "the blocked person's reaction is dropped without telling them");
    // Unblocking brings it all back.
    s.http.delete(format!("{}/blocks/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    assert_eq!(s.messages(&a).await.len(), 1);
    assert_eq!(s.presence(&a).await.len(), 1);
}

impl Service {
    async fn listen_state(&self, session: &str) -> Value {
        self.http.get(format!("{}/listen", self.base)).bearer_auth(session).send().await.unwrap().json().await.unwrap()
    }
}

#[tokio::test]
async fn health_needs_no_login_and_says_only_the_version() {
    let s = start().await;
    let v: Value = s.http.get(format!("{}/health", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["status"], "ok");
    assert!(v["version"].as_str().is_some_and(|x| !x.is_empty()));
    assert_eq!(v.as_object().unwrap().len(), 2, "nothing else is revealed");
}

#[tokio::test]
async fn listening_along_needs_someone_playing_and_tells_the_host() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let url = |session: &str| s.base.replace("http://", "ws://") + &format!("/ws?session={session}");
    let (mut ws_b, _) = tokio_tungstenite::connect_async(url(&b)).await.unwrap();
    // Nobody playing, or yourself: refused.
    assert_eq!(s.http.put(format!("{}/listen/bob", s.base)).bearer_auth(&a).send().await.unwrap().status(), 409);
    assert_eq!(s.http.put(format!("{}/listen/alice", s.base)).bearer_auth(&a).send().await.unwrap().status(), 400);
    s.put_presence(&b, Service::song("Two")).await;
    let r = s.http.put(format!("{}/listen/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let entry: Value = r.json().await.unwrap();
    assert_eq!((entry["user"].as_str(), entry["title"].as_str(), entry["position_ms"].as_u64()), (Some("bob"), Some("Two"), Some(5000)));
    assert_eq!(next_event(&mut ws_b).await, json!({"type": "listener_joined", "host": "bob", "listener": "alice"}));
    assert_eq!(s.listen_state(&a).await, json!({"listening_to": "bob", "listeners": []}));
    assert_eq!(s.listen_state(&b).await, json!({"listening_to": null, "listeners": ["alice"]}));
    // Leaving tells the host and clears it.
    assert_eq!(s.http.delete(format!("{}/listen", s.base)).bearer_auth(&a).send().await.unwrap().status(), 204);
    assert_eq!(next_event(&mut ws_b).await, json!({"type": "listener_left", "host": "bob", "listener": "alice"}));
    assert_eq!(s.listen_state(&b).await["listeners"], json!([]));
}

#[tokio::test]
async fn listeners_are_let_go_when_the_host_stops_or_blocks() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let (mut ws_a, _) = tokio_tungstenite::connect_async(s.base.replace("http://", "ws://") + &format!("/ws?session={a}")).await.unwrap();
    s.put_presence(&b, Service::song("Two")).await;
    assert_eq!(next_event(&mut ws_a).await["type"], "presence");
    s.http.put(format!("{}/listen/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    // The host stops: the listener is told and the listening is gone.
    s.http.delete(format!("{}/presence", s.base)).bearer_auth(&b).send().await.unwrap();
    let mut seen_end = false;
    for _ in 0..3 {
        let v = next_event(&mut ws_a).await;
        if v["type"] == "listen_ended" {
            assert_eq!(v, json!({"type": "listen_ended", "host": "bob", "listeners": ["alice"]}));
            seen_end = true;
            break;
        }
    }
    assert!(seen_end, "alice was told the listening ended");
    assert_eq!(s.listen_state(&a).await["listening_to"], json!(null));
    // Blocking ends it too, in either direction, and joining a blocker is refused.
    s.put_presence(&b, Service::song("Two")).await;
    s.http.put(format!("{}/listen/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    s.http.put(format!("{}/blocks/alice", s.base)).bearer_auth(&b).send().await.unwrap();
    assert_eq!(s.listen_state(&a).await["listening_to"], json!(null));
    assert_eq!(s.http.put(format!("{}/listen/bob", s.base)).bearer_auth(&a).send().await.unwrap().status(), 409);
}

#[tokio::test]
async fn a_chat_reply_carries_a_short_look_at_the_message_it_answers() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let first: Value = s.chat(&a, "Who wants to hear the new album tonight?").await.json().await.unwrap();
    let id = first["id"].as_i64().unwrap();
    assert!(first.get("reply").is_none(), "a plain message has no reply field");
    let r = s.http.post(format!("{}/chat", s.base)).bearer_auth(&b).json(&json!({"body": "Me!", "reply_to": id})).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let reply: Value = r.json().await.unwrap();
    assert_eq!(reply["reply"], json!({"id": id, "from": "alice", "body": "Who wants to hear the new album tonight?"}));
    // The list carries it too, and a gone message is said to be gone instead of being invented.
    let list = s.messages(&a).await;
    assert_eq!(list[1]["reply"]["from"], "alice");
    assert_eq!(s.http.delete(format!("{}/chat/{id}", s.base)).bearer_auth(&a).send().await.unwrap().status(), 204);
    let list = s.messages(&b).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["reply"], json!({"id": id, "from": null, "body": null}));
    // Replying to a message that does not exist is refused.
    let r = s.http.post(format!("{}/chat", s.base)).bearer_auth(&b).json(&json!({"body": "hello?", "reply_to": 9999})).send().await.unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn a_chat_reply_is_pushed_live_with_its_reference() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let id = s.chat(&a, "first").await.json::<Value>().await.unwrap()["id"].as_i64().unwrap();
    let (mut ws_a, _) = tokio_tungstenite::connect_async(s.base.replace("http://", "ws://") + &format!("/ws?session={a}")).await.unwrap();
    s.http.post(format!("{}/chat", s.base)).bearer_auth(&b).json(&json!({"body": "second", "reply_to": id})).send().await.unwrap();
    let v = next_event(&mut ws_a).await;
    assert_eq!((v["type"].as_str(), v["message"]["reply"]["body"].as_str()), (Some("chat"), Some("first")));
}

fn summary() -> Value {
    json!({"plays": 654, "liked": 16, "playlists": 5, "top_artists": [{"name": "Fictional Band", "plays": 89}], "top_genres": [{"name": "Ambient", "plays": 40}]})
}

#[tokio::test]
async fn a_profile_is_shared_seen_by_others_and_gone_when_sharing_stops() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let put = |session: String, body: Value| {
        let (http, base) = (s.http.clone(), s.base.clone());
        async move { http.put(format!("{base}/profile")).bearer_auth(session).json(&body).send().await.unwrap().status() }
    };
    let get = |session: String, who: &'static str| {
        let (http, base) = (s.http.clone(), s.base.clone());
        async move { http.get(format!("{base}/profile/{who}")).bearer_auth(session).send().await.unwrap() }
    };
    assert_eq!(get(a.clone(), "bob").await.status(), 404, "nothing shared yet");
    assert_eq!(put(b.clone(), summary()).await, 204);
    s.http.put(format!("{}/follows/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    s.put_presence(&b, Service::song("Two")).await;
    let v: Value = get(a.clone(), "bob").await.json().await.unwrap();
    assert_eq!((v["user"].as_str(), v["summary"]["plays"].as_u64(), v["summary"]["top_artists"][0]["name"].as_str()), (Some("bob"), Some(654), Some("Fictional Band")));
    assert_eq!((v["followers"].as_u64(), v["you_follow"].as_bool(), v["playing_now"].as_bool()), (Some(1), Some(true), Some(true)), "alice follows bob: bob has one follower, and you_follow is true for alice");
    // Bad input is refused.
    let mut bad = summary();
    bad["top_artists"] = json!((0..6).map(|i| json!({"name": format!("a{i}"), "plays": 1})).collect::<Vec<_>>());
    assert_eq!(put(b.clone(), bad).await, 400);
    // Blocking hides it both ways; switching sharing off removes it.
    s.http.put(format!("{}/blocks/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    assert_eq!(get(a.clone(), "bob").await.status(), 404);
    s.http.delete(format!("{}/blocks/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    assert_eq!(get(a.clone(), "bob").await.status(), 200);
    s.http.put(format!("{}/me/settings", s.base)).bearer_auth(&b).json(&json!({"share_presence": false})).send().await.unwrap();
    assert_eq!(get(a.clone(), "bob").await.status(), 404);
    assert_eq!(put(b.clone(), summary()).await, 403);
}

/// A real (tiny) picture, made here, so the tests need no files.
fn picture(format: image::ImageFormat, w: u32, h: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, format).unwrap();
    out.into_inner()
}

#[tokio::test]
async fn a_picture_is_made_square_small_and_served_to_others() {
    let s = start().await;
    let (a, b) = (s.login("alice").await, s.login("bob").await);
    let put = |session: String, bytes: Vec<u8>| {
        let (http, base) = (s.http.clone(), s.base.clone());
        async move { http.put(format!("{base}/avatar")).bearer_auth(session).body(bytes).send().await.unwrap() }
    };
    let get = |session: String, who: &'static str| {
        let (http, base) = (s.http.clone(), s.base.clone());
        async move { http.get(format!("{base}/avatar/{who}")).bearer_auth(session).send().await.unwrap() }
    };
    assert_eq!(get(a.clone(), "bob").await.status(), 404, "no picture yet");
    // A wide JPEG becomes a 256 x 256 PNG.
    assert_eq!(put(b.clone(), picture(image::ImageFormat::Jpeg, 640, 300)).await.status(), 204);
    let r = get(a.clone(), "bob").await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    let png = r.bytes().await.unwrap();
    let decoded = image::load_from_memory(&png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (256, 256));
    // Not a picture, a wrong format, and a nothing are refused; it needs a login.
    assert_eq!(put(b.clone(), b"definitely not a picture".to_vec()).await.status(), 400);
    assert_eq!(put(b.clone(), picture(image::ImageFormat::Gif, 8, 8)).await.status(), 400);
    assert_eq!(put(b.clone(), Vec::new()).await.status(), 400);
    assert_eq!(s.http.get(format!("{}/avatar/bob", s.base)).send().await.unwrap().status(), 401);
    // A huge upload is stopped before it is read.
    assert_eq!(put(b.clone(), vec![0u8; 5 * 1024 * 1024]).await.status(), 413);
    // Blocking hides it, and removing it removes it.
    s.http.put(format!("{}/blocks/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    assert_eq!(get(a.clone(), "bob").await.status(), 404);
    s.http.delete(format!("{}/blocks/bob", s.base)).bearer_auth(&a).send().await.unwrap();
    assert_eq!(get(a.clone(), "bob").await.status(), 200);
    assert_eq!(s.http.delete(format!("{}/avatar", s.base)).bearer_auth(&b).send().await.unwrap().status(), 204);
    assert_eq!(get(a.clone(), "bob").await.status(), 404);
}

#[tokio::test]
async fn a_party_has_a_dj_who_alone_passes_the_lead_and_only_followed_people_can_be_invited() {
    let s = start().await;
    let (a, b, c) = (s.login("alice").await, s.login("bob").await, s.login("cara").await);
    let call = |method: reqwest::Method, path: String, session: String| {
        let (http, base) = (s.http.clone(), s.base.clone());
        async move { http.request(method, format!("{base}{path}")).bearer_auth(session).send().await.unwrap() }
    };
    let state = |session: String| {
        let call = &call;
        async move { call(reqwest::Method::GET, "/party".into(), session).await.json::<Value>().await.unwrap() }
    };
    // Nobody is invited without being followed first.
    assert_eq!(call(reqwest::Method::PUT, "/party/invite/bob".into(), a.clone()).await.status(), 403);
    call(reqwest::Method::PUT, "/follows/bob".into(), a.clone()).await;
    call(reqwest::Method::PUT, "/follows/cara".into(), a.clone()).await;
    assert_eq!(call(reqwest::Method::PUT, "/party/invite/bob".into(), a.clone()).await.status(), 204);
    let st = state(b.clone()).await;
    assert_eq!(st["party"], Value::Null);
    let id = st["invites"][0]["party_id"].as_u64().unwrap();
    assert_eq!(st["invites"][0]["from"], "alice");
    assert_eq!(call(reqwest::Method::PUT, format!("/party/accept/{id}"), b.clone()).await.status(), 200);
    call(reqwest::Method::PUT, "/party/invite/cara".into(), a.clone()).await;
    call(reqwest::Method::PUT, format!("/party/accept/{id}"), c.clone()).await;
    let st = state(c.clone()).await;
    assert_eq!(st["party"]["leader"], "alice");
    assert_eq!(st["party"]["members"], json!(["alice", "bob", "cara"]));
    // Only the DJ passes the lead or removes someone.
    assert_eq!(call(reqwest::Method::PUT, "/party/leader/cara".into(), b.clone()).await.status(), 403);
    assert_eq!(call(reqwest::Method::DELETE, "/party/members/cara".into(), b.clone()).await.status(), 403);
    assert_eq!(call(reqwest::Method::PUT, "/party/leader/bob".into(), a.clone()).await.status(), 200);
    assert_eq!(state(a.clone()).await["party"]["leader"], "bob");
    assert_eq!(call(reqwest::Method::PUT, "/party/leader/alice".into(), a.clone()).await.status(), 403, "alice is no longer the DJ");
    // When the DJ leaves, the longest member takes over; the last one leaving ends the party.
    call(reqwest::Method::DELETE, "/party".into(), b.clone()).await;
    assert_eq!(state(a.clone()).await["party"]["leader"], "alice");
    assert_eq!(state(b.clone()).await["party"], Value::Null);
    assert_eq!(call(reqwest::Method::DELETE, "/party/members/cara".into(), a.clone()).await.status(), 200);
    assert_eq!(state(c.clone()).await["party"], Value::Null);
    // Blocking someone in your party sends them away.
    call(reqwest::Method::PUT, "/party/invite/bob".into(), a.clone()).await;
    let id = state(b.clone()).await["invites"][0]["party_id"].as_u64().unwrap();
    call(reqwest::Method::PUT, format!("/party/accept/{id}"), b.clone()).await;
    call(reqwest::Method::PUT, "/blocks/bob".into(), a.clone()).await;
    assert_eq!(state(b.clone()).await["party"], Value::Null);
}
