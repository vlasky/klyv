use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

const KIND: &str = "string";

pub(crate) fn get_string(conn: &Connection, key: Arg<'_>) -> rusqlite::Result<Option<Vec<u8>>> {
    conn.query_row(
        "SELECT value FROM strings WHERE key = ?1",
        params![key],
        |row| bytes_col(row, 0),
    )
    .optional()
}

fn put_string(conn: &Connection, key: Arg<'_>, value: Vec<u8>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, bind_value(value)],
    )?;
    Ok(())
}

/// SET's existence condition (Redis `NX` / `XX`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetCond {
    Always,
    /// Only if the key does not exist (`NX`).
    IfAbsent,
    /// Only if the key already exists (`XX`).
    IfExists,
}

pub(crate) fn cmd_set(
    conn: &Connection,
    key: Arg<'_>,
    value: Arg<'_>,
    cond: SetCond,
    ttl_ms: Option<i64>,
    keep_ttl: bool,
) -> CmdResult {
    if let Some(ms) = ttl_ms
        && ms <= 0
    {
        return Err(CmdError::new("ERR invalid expire time in 'set' command"));
    }
    let now = now_ms();
    let existing = lookup(conn, key)?;
    let live = existing.as_ref().is_some_and(|r| r.is_live(now));
    match cond {
        SetCond::IfAbsent if live => return Ok(Reply::Nil),
        SetCond::IfExists if !live => return Ok(Reply::Nil),
        _ => {}
    }
    // SET overwrites any existing key regardless of its type; the stale rows
    // of an expired key go too.
    if let Some(r) = &existing
        && (r.kind != KIND || !live)
    {
        clear_data(conn, key, r.kind)?;
    }
    // Like Redis, SET discards any existing TTL unless --keep-ttl is given.
    let expires_at = match ttl_ms {
        Some(ms) => Some(now.saturating_add(ms)),
        None if keep_ttl && live => existing.and_then(|r| r.expires_at),
        None => None,
    };
    conn.execute(
        "INSERT INTO keyspace (key, type, expires_at, len) VALUES (?1, 'string', ?2, NULL)
         ON CONFLICT(key) DO UPDATE
             SET type = 'string', expires_at = excluded.expires_at, len = NULL",
        params![key, expires_at],
    )?;
    put_string(conn, key, value.as_bytes().to_vec())?;
    Ok(Reply::Simple("OK"))
}

pub(crate) fn cmd_getdel(conn: &Connection, key: Arg<'_>) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    match get_string(conn, key)? {
        Some(v) => {
            remove_key(conn, key)?;
            Ok(Reply::Bulk(v))
        }
        None => Ok(Reply::Nil),
    }
}

pub(crate) fn cmd_get(conn: &Connection, key: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Nil);
    }
    Ok(match get_string(conn, key)? {
        Some(v) => Reply::Bulk(v),
        None => Reply::Nil,
    })
}

pub(crate) fn cmd_del(conn: &Connection, keys: &[Arg<'_>]) -> CmdResult {
    let now = now_ms();
    let mut count = 0i64;
    for &key in keys {
        // Count keys like Redis; an expired key is already logically gone so
        // it doesn't count, but its physical rows are still reclaimed.
        if remove_key(conn, key)?.is_some_and(|r| r.is_live(now)) {
            count += 1;
        }
    }
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_incrby(conn: &Connection, key: Arg<'_>, amount: i64) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let val = match get_string(conn, key)? {
        Some(v) => parse_int(&v)
            .ok_or_else(|| CmdError::new("ERR value is not an integer or out of range"))?,
        None => 0,
    };
    let new_val = val
        .checked_add(amount)
        .ok_or_else(|| CmdError::new("ERR increment or decrement would overflow"))?;
    put_string(conn, key, new_val.to_string().into_bytes())?;
    claim_string(conn, key)?;
    Ok(Reply::Int(new_val))
}

pub(crate) fn cmd_append(conn: &Connection, key: Arg<'_>, value: Arg<'_>) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let mut new_val = get_string(conn, key)?.unwrap_or_default();
    new_val.extend_from_slice(value.as_bytes());
    let len = new_val.len();
    put_string(conn, key, new_val)?;
    claim_string(conn, key)?;
    Ok(Reply::Int(len as i64))
}

pub(crate) fn cmd_strlen(conn: &Connection, key: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Int(0));
    }
    let len = get_string(conn, key)?.map_or(0, |v| v.len());
    Ok(Reply::Int(len as i64))
}

pub(crate) fn cmd_mset(conn: &Connection, pairs: &[Arg<'_>]) -> CmdResult {
    if pairs.is_empty() || !pairs.len().is_multiple_of(2) {
        return Err(CmdError::new(
            "ERR wrong number of arguments for 'mset' command",
        ));
    }
    let now = now_ms();
    for chunk in pairs.chunks(2) {
        let key = chunk[0];
        // MSET overwrites each key regardless of its prior type and, like
        // Redis, discards any existing TTL.
        if let Some(r) = lookup(conn, key)?
            && (r.kind != KIND || !r.is_live(now))
        {
            clear_data(conn, key, r.kind)?;
        }
        conn.execute(
            "INSERT INTO keyspace (key, type, expires_at, len) VALUES (?1, 'string', NULL, NULL)
             ON CONFLICT(key) DO UPDATE SET type = 'string', expires_at = NULL, len = NULL",
            params![key],
        )?;
        put_string(conn, key, chunk[1].as_bytes().to_vec())?;
    }
    Ok(Reply::Simple("OK"))
}

pub(crate) fn cmd_mget(conn: &Connection, keys: &[Arg<'_>]) -> CmdResult {
    let now = now_ms();
    let mut replies = Vec::with_capacity(keys.len());
    for &key in keys {
        // Redis MGET yields nil for missing and wrong-type keys alike.
        let value = if key_type(conn, key, now)? == Some(KIND) {
            get_string(conn, key)?
        } else {
            None
        };
        replies.push(value.map_or(Reply::Nil, Reply::Bulk));
    }
    Ok(Reply::Lines(replies))
}

/// GETSET: store a new value and return the old one. Like SET it discards
/// any TTL, but unlike SET it refuses a key of another type, since the old
/// value has to be returned as a string.
pub(crate) fn cmd_getset(conn: &Connection, key: Arg<'_>, value: Arg<'_>) -> CmdResult {
    let old = if check_type(conn, key, KIND, now_ms())? {
        get_string(conn, key)?
    } else {
        None
    };
    cmd_set(conn, key, value, SetCond::Always, None, false)?;
    Ok(old.map_or(Reply::Nil, Reply::Bulk))
}

/// What GETEX does to the key's expiry.
#[derive(Clone, Copy)]
pub(crate) enum GetEx {
    /// Leave it alone (plain GETEX is GET).
    Keep,
    /// Remove it (`PERSIST`).
    Persist,
    /// Expire at this Unix time in milliseconds (`EX`/`PX`/`EXAT`/`PXAT`,
    /// already converted). A time in the past expires the key at once.
    At(i64),
}

/// GETEX: return the value and change the expiry in one step.
pub(crate) fn cmd_getex(conn: &Connection, key: Arg<'_>, expiry: GetEx) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Nil);
    }
    let value = get_string(conn, key)?;
    let expires_at = match expiry {
        GetEx::Keep => return Ok(value.map_or(Reply::Nil, Reply::Bulk)),
        GetEx::Persist => None,
        GetEx::At(ms) => Some(ms),
    };
    conn.execute(
        "UPDATE keyspace SET expires_at = ?2 WHERE key = ?1",
        params![key, expires_at],
    )?;
    Ok(value.map_or(Reply::Nil, Reply::Bulk))
}

/// The unit of a GETEX expiry argument.
#[derive(Clone, Copy)]
pub(crate) enum GetExUnit {
    Seconds,
    Millis,
    UnixSeconds,
    UnixMillis,
}

/// Converts a GETEX `EX`/`PX`/`EXAT`/`PXAT` argument to an absolute expiry.
/// Like Redis, zero, negative and unrepresentable values are an error.
pub(crate) fn getex_expiry(unit: GetExUnit, n: i64) -> Result<GetEx, CmdError> {
    let invalid = || CmdError::new("ERR invalid expire time in 'getex' command");
    if n <= 0 {
        return Err(invalid());
    }
    let ms = match unit {
        GetExUnit::Seconds | GetExUnit::UnixSeconds => n.checked_mul(1000).ok_or_else(invalid)?,
        GetExUnit::Millis | GetExUnit::UnixMillis => n,
    };
    Ok(GetEx::At(match unit {
        GetExUnit::Seconds | GetExUnit::Millis => now_ms().saturating_add(ms),
        GetExUnit::UnixSeconds | GetExUnit::UnixMillis => ms,
    }))
}
