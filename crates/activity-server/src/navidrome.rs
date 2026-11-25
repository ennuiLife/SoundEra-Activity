//! Asking Navidrome whether a login is good. Credentials are replayed once and never stored or logged.

use serde::Deserialize;

#[derive(Debug)]
pub enum Credentials<'a> {
    TokenSalt { token: &'a str, salt: &'a str },
    Password(&'a str),
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: Inner,
}
#[derive(Deserialize)]
struct Inner {
    status: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Good,
    Refused,
    /// Navidrome could not be reached or answered nonsense; not the user's fault.
    Unavailable,
}

pub async fn verify(http: &reqwest::Client, base: &str, user: &str, credentials: &Credentials<'_>) -> Verdict {
    let mut query: Vec<(&str, String)> = vec![("u", user.to_string()), ("v", "1.16.1".into()), ("c", "soundera-activity".into()), ("f", "json".into())];
    match credentials {
        Credentials::TokenSalt { token, salt } => {
            query.push(("t", token.to_string()));
            query.push(("s", salt.to_string()));
        }
        Credentials::Password(p) => query.push(("p", p.to_string())),
    }
    let response = match http.get(format!("{base}/rest/ping")).query(&query).send().await {
        Ok(r) => r,
        Err(e) => {
            // `without_url` keeps the query (with the token) out of the log.
            tracing::warn!("Navidrome could not be reached: {}", e.without_url());
            return Verdict::Unavailable;
        }
    };
    if !response.status().is_success() {
        return Verdict::Unavailable;
    }
    match response.json::<Envelope>().await {
        Ok(e) if e.response.status == "ok" => Verdict::Good,
        Ok(_) => Verdict::Refused,
        Err(_) => Verdict::Unavailable,
    }
}
