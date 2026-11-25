//! Short-lived signed session tokens (HMAC-SHA256). The service issues one after Navidrome has confirmed a login;
//! no password or Navidrome token is ever kept.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct SessionKey(Vec<u8>);

impl SessionKey {
    pub fn generate() -> Self {
        let mut k = vec![0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut k);
        SessionKey(k)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        (bytes.len() >= 32).then_some(SessionKey(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn mac(&self) -> HmacSha256 {
        HmacSha256::new_from_slice(&self.0).expect("HMAC takes any key length")
    }

    /// A token for `user`, valid for `ttl_secs` from `now_secs`.
    pub fn issue(&self, user: &str, now_secs: u64, ttl_secs: u64) -> String {
        let payload = B64.encode(format!("{}:{}", now_secs + ttl_secs, user));
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        format!("{payload}.{}", B64.encode(mac.finalize().into_bytes()))
    }

    /// The user a token belongs to, if its signature is good and it has not expired.
    pub fn verify(&self, token: &str, now_secs: u64) -> Option<String> {
        let (payload, sig) = token.split_once('.')?;
        let sig = B64.decode(sig).ok()?;
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.verify_slice(&sig).ok()?; // constant time
        let text = String::from_utf8(B64.decode(payload).ok()?).ok()?;
        let (exp, user) = text.split_once(':')?;
        (exp.parse::<u64>().ok()? > now_secs && !user.is_empty()).then(|| user.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_round_trips_until_it_expires() {
        let key = SessionKey::generate();
        let t = key.issue("alice", 1000, 60);
        assert_eq!(key.verify(&t, 1000).as_deref(), Some("alice"));
        assert_eq!(key.verify(&t, 1059).as_deref(), Some("alice"));
        assert_eq!(key.verify(&t, 1060), None);
    }

    #[test]
    fn user_names_with_colons_and_unicode_survive() {
        let key = SessionKey::generate();
        let t = key.issue("müller:א", 0, 10);
        assert_eq!(key.verify(&t, 1).as_deref(), Some("müller:א"));
    }

    #[test]
    fn a_changed_or_foreign_token_is_refused() {
        let key = SessionKey::generate();
        let other = SessionKey::generate();
        let t = key.issue("alice", 0, 100);
        assert_eq!(other.verify(&t, 1), None);
        let (p, s) = t.split_once('.').unwrap();
        let forged = format!("{}.{s}", B64.encode(format!("{}:bob", 100)));
        assert_eq!(key.verify(&forged, 1), None);
        assert_eq!(key.verify(&format!("{p}.AAAA"), 1), None);
        assert_eq!(key.verify("", 1), None);
        assert_eq!(key.verify("no-dot", 1), None);
    }

    #[test]
    fn short_keys_are_refused() {
        assert!(SessionKey::from_bytes(vec![1; 8]).is_none());
        assert!(SessionKey::from_bytes(vec![1; 32]).is_some());
    }
}
