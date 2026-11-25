//! Reactions and chat: what is allowed, with no network involved.

use serde::{Deserialize, Serialize};

pub const MAX_CHAT_CHARS: usize = 500;
/// At most this many chat messages per user in `CHAT_BURST_WINDOW_MS`.
pub const CHAT_BURST: usize = 5;
pub const CHAT_BURST_WINDOW_MS: u64 = 10_000;
/// Chat history is kept this long.
pub const CHAT_KEEP_MS: u64 = 30 * 24 * 3600 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReactionKind {
    Like,
    Fire,
    Sad,
    Laugh,
}

impl ReactionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReactionKind::Like => "like",
            ReactionKind::Fire => "fire",
            ReactionKind::Sad => "sad",
            ReactionKind::Laugh => "laugh",
        }
    }

    pub fn parse(s: &str) -> Option<ReactionKind> {
        Some(match s {
            "like" => ReactionKind::Like,
            "fire" => ReactionKind::Fire,
            "sad" => ReactionKind::Sad,
            "laugh" => ReactionKind::Laugh,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: i64,
    pub from: String,
    pub body: String,
    pub created_at_ms: u64,
}

/// A chat text is trimmed and must be 1 to `MAX_CHAT_CHARS` characters with no control characters other than
/// line breaks (which are kept as `\n`). Text is only ever shown by clients, never run or fetched.
pub fn clean_chat_body(raw: &str) -> Result<String, &'static str> {
    let body = raw.replace("\r\n", "\n");
    let body = body.trim();
    if body.is_empty() {
        return Err("write something first");
    }
    if body.chars().count() > MAX_CHAT_CHARS {
        return Err("that message is too long");
    }
    if body.chars().any(|c| c.is_control() && c != '\n') {
        return Err("a message cannot have control characters");
    }
    if body.matches('\n').count() > 8 {
        return Err("too many line breaks");
    }
    Ok(body.to_string())
}

/// Sliding-window limit for one user's chat messages: `times` holds the milliseconds of their recent messages.
pub fn chat_allowed(times: &mut Vec<u64>, now_ms: u64) -> bool {
    times.retain(|t| now_ms.saturating_sub(*t) < CHAT_BURST_WINDOW_MS);
    if times.len() >= CHAT_BURST {
        return false;
    }
    times.push(now_ms);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_unknown_ones_are_refused() {
        for k in [ReactionKind::Like, ReactionKind::Fire, ReactionKind::Sad, ReactionKind::Laugh] {
            assert_eq!(ReactionKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ReactionKind::parse("angry"), None);
        assert_eq!(ReactionKind::parse(""), None);
    }

    #[test]
    fn chat_text_is_cleaned_and_limited() {
        assert_eq!(clean_chat_body("  hello  ").unwrap(), "hello");
        assert_eq!(clean_chat_body("a\r\nb").unwrap(), "a\nb");
        assert!(clean_chat_body("   ").is_err());
        assert!(clean_chat_body(&"x".repeat(501)).is_err());
        assert!(clean_chat_body(&"x".repeat(500)).is_ok());
        assert!(clean_chat_body("bell\u{7}").is_err());
        assert!(clean_chat_body("tab\there").is_err());
        assert!(clean_chat_body(&"a\n".repeat(20)).is_err());
        assert_eq!(clean_chat_body("héllo א 你好").unwrap(), "héllo א 你好");
    }

    #[test]
    fn the_chat_rate_limit_is_a_sliding_window() {
        let mut t = Vec::new();
        for i in 0..CHAT_BURST {
            assert!(chat_allowed(&mut t, i as u64));
        }
        assert!(!chat_allowed(&mut t, 100));
        assert!(chat_allowed(&mut t, CHAT_BURST_WINDOW_MS + 10)); // the first ones have slid out
    }
}
