//! What each user is playing right now. Entries expire when a client stops sending its heart-beat.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A presence entry is dropped this long after its last update (clients send one every 15 s).
pub const PRESENCE_TTL_MS: u64 = 90_000;
const MAX_TEXT: usize = 300;
const MAX_POSITION_MS: u64 = 10 * 24 * 3600 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayState {
    Playing,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceUpdate {
    pub song_id: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub state: PlayState,
    #[serde(default)]
    pub device: String,
}

impl PresenceUpdate {
    /// Refuse what is too long, has control characters or makes no sense. Text is only ever shown, never run.
    pub fn validate(&self) -> Result<(), &'static str> {
        for text in [&self.song_id, &self.title, &self.artist, &self.album, &self.device] {
            if text.chars().count() > MAX_TEXT {
                return Err("a text is too long");
            }
            if text.chars().any(|c| c.is_control()) {
                return Err("a text has control characters");
            }
        }
        if self.song_id.is_empty() || self.title.is_empty() {
            return Err("song_id and title are needed");
        }
        if self.position_ms > MAX_POSITION_MS || self.duration_ms > MAX_POSITION_MS {
            return Err("a time is out of range");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceEntry {
    pub user: String,
    #[serde(flatten)]
    pub presence: PresenceUpdate,
    pub updated_at_ms: u64,
}

#[derive(Default)]
pub struct PresenceBoard {
    map: HashMap<String, (PresenceUpdate, u64)>,
}

impl PresenceBoard {
    pub fn set(&mut self, user: &str, update: PresenceUpdate, now_ms: u64) -> PresenceEntry {
        self.map.insert(user.to_string(), (update.clone(), now_ms));
        PresenceEntry { user: user.to_string(), presence: update, updated_at_ms: now_ms }
    }

    /// Returns whether the user had an entry.
    pub fn remove(&mut self, user: &str) -> bool {
        self.map.remove(user).is_some()
    }

    /// Everyone else's live entries, by user name.
    pub fn visible_to(&mut self, viewer: &str, now_ms: u64) -> Vec<PresenceEntry> {
        self.map.retain(|_, (_, at)| now_ms.saturating_sub(*at) < PRESENCE_TTL_MS);
        let mut list: Vec<PresenceEntry> = self
            .map
            .iter()
            .filter(|(user, _)| user.as_str() != viewer)
            .map(|(user, (p, at))| PresenceEntry { user: user.clone(), presence: p.clone(), updated_at_ms: *at })
            .collect();
        list.sort_by(|a, b| a.user.cmp(&b.user));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(title: &str) -> PresenceUpdate {
        PresenceUpdate { song_id: "s1".into(), title: title.into(), artist: "Fictional Band".into(), album: String::new(), position_ms: 1000, duration_ms: 200_000, state: PlayState::Playing, device: "Desktop".into() }
    }

    #[test]
    fn a_viewer_sees_others_but_not_themselves() {
        let mut b = PresenceBoard::default();
        b.set("alice", song("One"), 0);
        b.set("bob", song("Two"), 0);
        let seen = b.visible_to("alice", 10);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].user, "bob");
    }

    #[test]
    fn entries_expire_without_a_heart_beat() {
        let mut b = PresenceBoard::default();
        b.set("bob", song("Two"), 0);
        assert_eq!(b.visible_to("alice", PRESENCE_TTL_MS - 1).len(), 1);
        assert!(b.visible_to("alice", PRESENCE_TTL_MS).is_empty());
    }

    #[test]
    fn removing_ends_presence_at_once() {
        let mut b = PresenceBoard::default();
        b.set("bob", song("Two"), 0);
        assert!(b.remove("bob"));
        assert!(!b.remove("bob"));
        assert!(b.visible_to("alice", 1).is_empty());
    }

    #[test]
    fn bad_input_is_refused() {
        assert!(song("ok").validate().is_ok());
        assert!(song(&"x".repeat(301)).validate().is_err());
        assert!(song("line\nbreak").validate().is_err());
        assert!(song("").validate().is_err());
        let mut p = song("ok");
        p.position_ms = u64::MAX;
        assert!(p.validate().is_err());
    }
}
