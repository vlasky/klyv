use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

// --- TTL commands ---

pub(crate) fn cmd_expire(conn: &Connection, key: &str, seconds: i64) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    conn.execute(
        "INSERT OR REPLACE INTO expiry (key, expires_at) VALUES (?1, unixepoch() + ?2)",
        params![key, seconds],
    )?;
    Ok(Reply::Int(1))
}

pub(crate) fn cmd_expireat(conn: &Connection, key: &str, timestamp: i64) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    conn.execute(
        "INSERT OR REPLACE INTO expiry (key, expires_at) VALUES (?1, ?2)",
        params![key, timestamp],
    )?;
    Ok(Reply::Int(1))
}

pub(crate) fn cmd_ttl(conn: &Connection, key: &str) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Ok(Reply::Int(-2));
    }
    let remaining: Option<i64> = conn
        .query_row(
            "SELECT expires_at - unixepoch() FROM expiry WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Reply::Int(remaining.unwrap_or(-1)))
}

pub(crate) fn cmd_persist(conn: &Connection, key: &str) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let removed = conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    Ok(Reply::Int((removed > 0) as i64))
}

pub(crate) fn cmd_purge(conn: &Connection) -> CmdResult {
    let expired_keys: Vec<String> = {
        let mut stmt = conn.prepare("SELECT key FROM expiry WHERE expires_at <= unixepoch()")?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    let count = expired_keys.len() as i64;
    for key in &expired_keys {
        conn.execute("DELETE FROM strings WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    Ok(Reply::Int(count))
}
