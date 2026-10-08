use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

// --- String commands ---

pub(crate) fn get_string(conn: &Connection, key: &str) -> Result<Option<String>, rusqlite::Error> {
    conn.query_row(
        "SELECT value FROM strings WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
}

pub(crate) fn cmd_set(
    conn: &Connection,
    key: &str,
    value: &str,
    nx: bool,
    ttl_seconds: Option<i64>,
    keep_ttl: bool,
) -> CmdResult {
    if let Some(secs) = ttl_seconds
        && secs <= 0
    {
        return Err(CmdError::new("ERR invalid expire time in 'set' command"));
    }
    if nx && key_exists_in_data(conn, key)? && !is_expired(conn, key)? {
        return Ok(Reply::Nil);
    }
    // SET overwrites any existing key, regardless of its prior type.
    conn.execute("DELETE FROM list_items WHERE key = ?1", params![key])?;
    conn.execute("DELETE FROM set_members WHERE key = ?1", params![key])?;
    conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key])?;
    // Like Redis, SET discards any existing TTL unless --keep-ttl is given;
    // even then a stale (already-expired) expiry row goes, so the new value
    // isn't hidden.
    if !keep_ttl || is_expired(conn, key)? {
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    if let Some(secs) = ttl_seconds {
        conn.execute(
            "INSERT OR REPLACE INTO expiry (key, expires_at) VALUES (?1, unixepoch() + ?2)",
            params![key, secs],
        )?;
    }
    Ok(Reply::Simple("OK"))
}

pub(crate) fn cmd_getdel(conn: &Connection, key: &str) -> CmdResult {
    ensure_type(conn, key, "string")?;
    drop_if_expired(conn, key)?;
    match get_string(conn, key)? {
        Some(v) => {
            conn.execute("DELETE FROM strings WHERE key = ?1", params![key])?;
            conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
            Ok(Reply::Bulk(v))
        }
        None => Ok(Reply::Nil),
    }
}

pub(crate) fn cmd_get(conn: &Connection, key: &str) -> CmdResult {
    ensure_type(conn, key, "string")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Nil);
    }
    Ok(match get_string(conn, key)? {
        Some(v) => Reply::Bulk(v),
        None => Reply::Nil,
    })
}

pub(crate) fn cmd_del(conn: &Connection, keys: &[String]) -> CmdResult {
    let mut count = 0i64;
    for key in keys {
        // Count keys like Redis, not rows; an expired key is already logically
        // gone so it doesn't count, but its physical rows are still reclaimed.
        if key_exists_in_data(conn, key)? && !is_expired(conn, key)? {
            count += 1;
        }
        conn.execute("DELETE FROM strings WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_incrby(conn: &Connection, key: &str, amount: i64) -> CmdResult {
    ensure_type(conn, key, "string")?;
    drop_if_expired(conn, key)?;
    let val: i64 = match get_string(conn, key)? {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| CmdError::new("ERR value is not an integer"))?,
        None => 0,
    };
    let new_val = val
        .checked_add(amount)
        .ok_or_else(|| CmdError::new("ERR increment or decrement would overflow"))?;
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, new_val.to_string()],
    )?;
    Ok(Reply::Int(new_val))
}

pub(crate) fn cmd_append(conn: &Connection, key: &str, value: &str) -> CmdResult {
    ensure_type(conn, key, "string")?;
    drop_if_expired(conn, key)?;
    let new_val = match get_string(conn, key)? {
        Some(existing) => format!("{existing}{value}"),
        None => value.to_string(),
    };
    let len = new_val.len();
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, new_val],
    )?;
    Ok(Reply::Int(len as i64))
}

pub(crate) fn cmd_strlen(conn: &Connection, key: &str) -> CmdResult {
    ensure_type(conn, key, "string")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let len = get_string(conn, key)?.map(|s| s.len()).unwrap_or(0);
    Ok(Reply::Int(len as i64))
}

pub(crate) fn cmd_mset(conn: &Connection, pairs: &[String]) -> CmdResult {
    if !pairs.len().is_multiple_of(2) {
        return Err(CmdError::new(
            "ERR wrong number of arguments for 'mset' command",
        ));
    }
    for chunk in pairs.chunks(2) {
        // MSET overwrites each key regardless of its prior type.
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![chunk[0]])?;
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![chunk[0]])?;
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![chunk[0]])?;
        // Like Redis, MSET discards any existing TTL.
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![chunk[0]])?;
        conn.execute(
            "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
            params![chunk[0], chunk[1]],
        )?;
    }
    Ok(Reply::Simple("OK"))
}

pub(crate) fn cmd_mget(conn: &Connection, keys: &[String]) -> CmdResult {
    let mut replies = Vec::with_capacity(keys.len());
    for key in keys {
        if is_expired(conn, key)? {
            replies.push(Reply::Nil);
            continue;
        }
        replies.push(match get_string(conn, key)? {
            Some(v) => Reply::Bulk(v),
            None => Reply::Nil,
        });
    }
    Ok(Reply::Lines(replies))
}
