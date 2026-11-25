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
        let known = matches!(q.get("u").map(String::as_str), Some("alice") | Some("bob"));
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
