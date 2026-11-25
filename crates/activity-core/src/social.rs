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

/// The message a chat message replies to: a short look at it. `from` and `body` are `None` when it was deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRef {
    pub id: i64,
    pub from: Option<String>,
    pub body: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: i64,
    pub from: String,
    pub body: String,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyRef>,
    /// A whisper: only the writer and this person see it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// A song suggested in a whisper (its id; the body says what it is).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song_id: Option<String>,
    /// The emoji people attached to this message, each with how many and whether the viewer is one of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<MessageReaction>,
}

/// One emoji on a chat message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageReaction {
    pub emoji: String,
    pub count: u32,
    pub mine: bool,
}

/// Most different emoji one message can carry.
pub const MAX_MESSAGE_EMOJI: usize = 20;

/// Is `emoji` one single emoji (not text)?
pub fn valid_emoji(emoji: &str) -> bool {
    valid_reaction(&format!("emoji:{emoji}"))
}

/// How much of the replied-to message is shown (in characters).
pub const REPLY_PREVIEW_CHARS: usize = 120;

/// One name with a count, for the "most played" lists of a profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ranked {
    pub name: String,
    pub plays: u32,
}

/// What a person chooses to show on their profile (sent by their SoundEra while they share their activity). The
/// service never sees the library itself, only these few numbers and names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub plays: u32,
    pub liked: u32,
    pub playlists: u32,
    #[serde(default)]
    pub top_artists: Vec<Ranked>,
    #[serde(default)]
    pub top_genres: Vec<Ranked>,
}

impl ProfileSummary {
    /// Refuse what is too long or makes no sense (this comes from other people's computers).
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.top_artists.len() > 5 || self.top_genres.len() > 5 {
            return Err("too many entries");
        }
        for r in self.top_artists.iter().chain(self.top_genres.iter()) {
            if r.name.is_empty() || r.name.chars().count() > 120 || r.name.chars().any(|c| c.is_control()) {
                return Err("a name is not allowed");
            }
        }
        Ok(())
    }
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
    fn a_profile_summary_is_limited() {
        let ok = ProfileSummary { plays: 10, liked: 2, playlists: 1, top_artists: vec![Ranked { name: "Fictional Band".into(), plays: 5 }], top_genres: vec![] };
        assert!(ok.validate().is_ok());
        let mut many = ok.clone();
        many.top_artists = (0..6).map(|i| Ranked { name: format!("a{i}"), plays: 1 }).collect();
        assert!(many.validate().is_err());
        let mut bad = ok.clone();
        bad.top_genres = vec![Ranked { name: "x\ny".into(), plays: 1 }];
        assert!(bad.validate().is_err());
        bad.top_genres = vec![Ranked { name: "".into(), plays: 1 }];
        assert!(bad.validate().is_err());
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

/// A reaction can be one of the four named kinds, or any one emoji sent as `emoji:<the emoji>`.
pub fn valid_reaction(kind: &str) -> bool {
    if ReactionKind::parse(kind).is_some() {
        return true;
    }
    match kind.strip_prefix("emoji:") {
        Some(e) => !e.is_empty() && e.len() <= 32 && e.chars().count() <= 8 && !e.is_ascii() && !e.chars().any(|c| c.is_control() || c.is_whitespace()),
        None => false,
    }
}

#[cfg(test)]
mod reaction_tests {
    use super::*;

    #[test]
    fn named_and_emoji_reactions_are_accepted_and_text_is_not() {
        assert!(valid_reaction("fire"));
        assert!(valid_reaction("emoji:🎉"));
        assert!(valid_reaction("emoji:👍🏽"));
        assert!(!valid_reaction("emoji:"));
        assert!(!valid_reaction("emoji:hello"));
        assert!(!valid_reaction("emoji:🎉 x"));
        assert!(!valid_reaction("nope"));
    }
}
