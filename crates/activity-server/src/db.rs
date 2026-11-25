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
