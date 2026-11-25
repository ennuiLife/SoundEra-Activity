use rusqlite::{params, Connection};
use std::path::Path;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS users (
            name TEXT PRIMARY KEY,
            share_presence INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS follows (
            follower TEXT NOT NULL,
            followee TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (follower, followee)
        );
        CREATE TABLE IF NOT EXISTS reactions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_user TEXT NOT NULL,
            to_user TEXT NOT NULL,
            song_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            UNIQUE (from_user, to_user, song_id, kind)
        );
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_user TEXT NOT NULL,
            body TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS blocks (
            blocker TEXT NOT NULL,
            blocked TEXT NOT NULL,
            PRIMARY KEY (blocker, blocked)
        );",
    )
}

/// Make sure the user exists (sharing is on by default, as everyone on the server sees everyone today).
pub fn touch_user(conn: &Connection, name: &str, now_secs: u64) -> rusqlite::Result<()> {
    conn.execute("INSERT OR IGNORE INTO users (name, share_presence, created_at) VALUES (?1, 1, ?2)", params![name, now_secs as i64])?;
    Ok(())
}

pub fn share_presence(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT share_presence FROM users WHERE name = ?1", params![name], |r| r.get::<_, i64>(0)).map(|v| v != 0).or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(true),
        e => Err(e),
    })
}

pub fn set_share_presence(conn: &Connection, name: &str, on: bool) -> rusqlite::Result<()> {
    conn.execute("UPDATE users SET share_presence = ?2 WHERE name = ?1", params![name, on as i64])?;
    Ok(())
}

/// Follow someone. Returns false when it was already so.
pub fn follow(conn: &Connection, follower: &str, followee: &str, now_secs: u64) -> rusqlite::Result<bool> {
    Ok(conn.execute("INSERT OR IGNORE INTO follows (follower, followee, created_at) VALUES (?1, ?2, ?3)", params![follower, followee, now_secs as i64])? > 0)
}

pub fn unfollow(conn: &Connection, follower: &str, followee: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM follows WHERE follower = ?1 AND followee = ?2", params![follower, followee])? > 0)
}

/// Who `user` follows, by name.
pub fn following(conn: &Connection, user: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT followee FROM follows WHERE follower = ?1 ORDER BY followee")?;
    let rows = stmt.query_map(params![user], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Who follows `user`, by name.
pub fn followers(conn: &Connection, user: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT follower FROM follows WHERE followee = ?1 ORDER BY follower")?;
    let rows = stmt.query_map(params![user], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Do we know this user (they have signed in at least once)?
pub fn user_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT 1 FROM users WHERE name = ?1", params![name], |_| Ok(())).map(|_| true).or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        e => Err(e),
    })
}

// ---- blocks ----

pub fn block(conn: &Connection, blocker: &str, blocked: &str) -> rusqlite::Result<()> {
    conn.execute("INSERT OR IGNORE INTO blocks (blocker, blocked) VALUES (?1, ?2)", params![blocker, blocked])?;
    Ok(())
}

pub fn unblock(conn: &Connection, blocker: &str, blocked: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM blocks WHERE blocker = ?1 AND blocked = ?2", params![blocker, blocked])?;
    Ok(())
}

pub fn blocked_by(conn: &Connection, blocker: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT blocked FROM blocks WHERE blocker = ?1 ORDER BY blocked")?;
    let rows = stmt.query_map(params![blocker], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Has `blocker` blocked `blocked`?
pub fn is_blocked(conn: &Connection, blocker: &str, blocked: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT 1 FROM blocks WHERE blocker = ?1 AND blocked = ?2", params![blocker, blocked], |_| Ok(())).map(|_| true).or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        e => Err(e),
    })
}

// ---- reactions ----

/// Record a reaction; the same person reacting the same way to the same song again changes nothing.
/// Returns whether it was new.
pub fn add_reaction(conn: &Connection, from: &str, to: &str, song_id: &str, kind: &str, now_ms: u64) -> rusqlite::Result<bool> {
    Ok(conn.execute("INSERT OR IGNORE INTO reactions (from_user, to_user, song_id, kind, created_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![from, to, song_id, kind, now_ms as i64])? > 0)
}

pub struct ReactionRow {
    pub from: String,
    pub song_id: String,
    pub kind: String,
    pub created_at_ms: u64,
}

/// Reactions `user` received since `since_ms`, newest first, from people they have not blocked.
pub fn reactions_for(conn: &Connection, user: &str, since_ms: u64, limit: u32) -> rusqlite::Result<Vec<ReactionRow>> {
    let mut stmt = conn.prepare(
        "SELECT from_user, song_id, kind, created_at FROM reactions
         WHERE to_user = ?1 AND created_at >= ?2 AND from_user NOT IN (SELECT blocked FROM blocks WHERE blocker = ?1)
         ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![user, since_ms as i64, limit], |r| Ok(ReactionRow { from: r.get(0)?, song_id: r.get(1)?, kind: r.get(2)?, created_at_ms: r.get::<_, i64>(3)? as u64 }))?;
    rows.collect()
}

// ---- chat ----

pub fn add_message(conn: &Connection, from: &str, body: &str, now_ms: u64, keep_ms: u64) -> rusqlite::Result<i64> {
    conn.execute("DELETE FROM messages WHERE created_at < ?1", params![now_ms.saturating_sub(keep_ms) as i64])?;
    conn.execute("INSERT INTO messages (from_user, body, created_at) VALUES (?1, ?2, ?3)", params![from, body, now_ms as i64])?;
    Ok(conn.last_insert_rowid())
}

/// The newest `limit` messages after id `after`, oldest first, leaving out people `viewer` has blocked.
pub fn messages_for(conn: &Connection, viewer: &str, after: i64, limit: u32) -> rusqlite::Result<Vec<activity_core::ChatMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_user, body, created_at FROM (
            SELECT id, from_user, body, created_at FROM messages
            WHERE id > ?2 AND from_user NOT IN (SELECT blocked FROM blocks WHERE blocker = ?1)
            ORDER BY id DESC LIMIT ?3
         ) ORDER BY id",
    )?;
    let rows = stmt.query_map(params![viewer, after, limit], |r| Ok(activity_core::ChatMessage { id: r.get(0)?, from: r.get(1)?, body: r.get(2)?, created_at_ms: r.get::<_, i64>(3)? as u64 }))?;
    rows.collect()
}

/// Delete a message, but only if `user` wrote it. Returns whether one was deleted.
pub fn delete_own_message(conn: &Connection, user: &str, id: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM messages WHERE id = ?1 AND from_user = ?2", params![id, user])? > 0)
}
