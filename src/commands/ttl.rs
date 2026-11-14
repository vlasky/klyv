use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, params};

/// Sets an absolute expiry (Unix ms) on a live key. Returns 0 for a missing
/// or already-expired key, like Redis.
fn set_expiry(conn: &Connection, key: &str, now: i64, at_ms: i64) -> CmdResult {
    if key_type(conn, key, now)?.is_none() {
        return Ok(Reply::Int(0));
    }
    conn.execute(
        "UPDATE keyspace SET expires_at = ?2 WHERE key = ?1",
        params![key, at_ms],
    )?;
    Ok(Reply::Int(1))
}

fn invalid_expire(name: &str) -> CmdError {
    CmdError::new(format!("ERR invalid expire time in '{name}' command"))
}

/// Seconds to milliseconds. Like Redis, negative values are allowed (they
/// expire the key at once) but a value whose conversion would overflow is an
/// error, reported before the key is even looked up.
fn seconds_to_ms(seconds: i64, name: &str) -> Result<i64, CmdError> {
    seconds
        .checked_mul(1000)
        .ok_or_else(|| invalid_expire(name))
}

fn after(now: i64, ms: i64, name: &str) -> Result<i64, CmdError> {
    now.checked_add(ms).ok_or_else(|| invalid_expire(name))
}

pub(crate) fn cmd_expire(conn: &Connection, key: &str, seconds: i64) -> CmdResult {
    let now = now_ms();
    let at = after(now, seconds_to_ms(seconds, "expire")?, "expire")?;
    set_expiry(conn, key, now, at)
}

pub(crate) fn cmd_pexpire(conn: &Connection, key: &str, milliseconds: i64) -> CmdResult {
    let now = now_ms();
    let at = after(now, milliseconds, "pexpire")?;
    set_expiry(conn, key, now, at)
}

pub(crate) fn cmd_expireat(conn: &Connection, key: &str, timestamp: i64) -> CmdResult {
    let at = seconds_to_ms(timestamp, "expireat")?;
    set_expiry(conn, key, now_ms(), at)
}

pub(crate) fn cmd_pexpireat(conn: &Connection, key: &str, timestamp_ms: i64) -> CmdResult {
    set_expiry(conn, key, now_ms(), timestamp_ms)
}

/// Remaining lifetime in ms: Err(-2) missing/expired, Err(-1) no expiry.
fn remaining_ms(conn: &Connection, key: &str) -> Result<Result<i64, i64>, CmdError> {
    let now = now_ms();
    Ok(match lookup(conn, key)? {
        Some(r) if r.is_live(now) => match r.expires_at {
            Some(t) => Ok(t - now),
            None => Err(-1),
        },
        _ => Err(-2),
    })
}

pub(crate) fn cmd_ttl(conn: &Connection, key: &str) -> CmdResult {
    // Redis rounds the millisecond remainder to the nearest second.
    Ok(Reply::Int(match remaining_ms(conn, key)? {
        Ok(ms) => (ms + 500) / 1000,
        Err(code) => code,
    }))
}

pub(crate) fn cmd_pttl(conn: &Connection, key: &str) -> CmdResult {
    Ok(Reply::Int(match remaining_ms(conn, key)? {
        Ok(ms) => ms,
        Err(code) => code,
    }))
}

pub(crate) fn cmd_persist(conn: &Connection, key: &str) -> CmdResult {
    let now = now_ms();
    match lookup(conn, key)? {
        Some(r) if r.is_live(now) && r.expires_at.is_some() => {
            conn.execute(
                "UPDATE keyspace SET expires_at = NULL WHERE key = ?1",
                params![key],
            )?;
            Ok(Reply::Int(1))
        }
        _ => Ok(Reply::Int(0)),
    }
}

pub(crate) fn cmd_purge(conn: &Connection) -> CmdResult {
    let expired: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT key FROM keyspace WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        )?;
        stmt.query_map(params![now_ms()], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    for key in &expired {
        remove_key(conn, key)?;
    }
    Ok(Reply::Int(expired.len() as i64))
}
