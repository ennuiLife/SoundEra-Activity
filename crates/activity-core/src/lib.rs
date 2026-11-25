//! Protocol types and the rules that need no network: session tokens and the presence board.

pub mod presence;
pub mod session;
pub mod social;

pub use presence::{PlayState, PresenceBoard, PresenceUpdate, PRESENCE_TTL_MS};
pub use session::SessionKey;
pub use social::{ChatMessage, ProfileSummary, ReactionKind, Ranked, ReplyRef};
