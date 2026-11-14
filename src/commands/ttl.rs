use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, params};

/// The optional condition on EXPIRE and its relatives. Every flag that is set
/// must hold for the expiry to be applied. Redis rejects NX combined with
/// any other, and GT with LT; that is checked where the flags are parsed.
#[derive(Clone, Copy, Default)]
pub(crate) struct ExpireCond {
    /// Only if the key has no expiry.
    pub(crate) nx: bool,
    /// Only if the key has an expiry.
    pub(crate) xx: bool,
    /// Only if the new expiry is later than the current one. A key with no
    /// expiry counts as never expiring, so GT never applies to it.
    pub(crate) gt: bool,
    /// Only if the new expiry is earlier than the current one, which for a
    /// key with no expiry is always.
    pub(crate) lt: bool,
}

impl ExpireCond {
    /// Redis's rules for combining the flags.
    pub(crate) fn validate(self) -> Result<Self, CmdError> {
        if self.nx && (self.xx || self.gt || self.lt) {
            return Err(CmdError::new(
                "ERR NX and XX, GT or LT options at the same time are not compatible",
            ));
        }
        if self.gt && self.lt {
            return Err(CmdError::new(
                "ERR GT and LT options at the same time are not compatible",
            ));
        }
        Ok(self)
    }

    fn allows(self, current: Option<i64>, new: i64) -> bool {
        (!self.nx || current.is_none())
            && (!self.xx || current.is_some())
            && (!self.gt || current.is_some_and(|c| new > c))
            && (!self.lt || current.is_none_or(|c| new < c))
    }
}

/// Sets an absolute expiry (Unix ms) on a live key. Returns 0 for a missing
/// or already-expired key, like Redis, or if `cond` does not allow it.
fn set_expiry(
    conn: &Connection,
    key: Arg<'_>,
    now: i64,
    at_ms: i64,
    cond: ExpireCond,
) -> CmdResult {
    let Some(row) = lookup(conn, key)?.filter(|r| r.is_live(now)) else {
        return Ok(Reply::Int(0));
    };
    if !cond.allows(row.expires_at, at_ms) {
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

pub(crate) fn cmd_expire(
    conn: &Connection,
    key: Arg<'_>,
    seconds: i64,
    cond: ExpireCond,
) -> CmdResult {
    let now = now_ms();
    let at = after(now, seconds_to_ms(seconds, "expire")?, "expire")?;
    set_expiry(conn, key, now, at, cond)
}

pub(crate) fn cmd_pexpire(
    conn: &Connection,
    key: Arg<'_>,
    milliseconds: i64,
    cond: ExpireCond,
) -> CmdResult {
    let now = now_ms();
    let at = after(now, milliseconds, "pexpire")?;
    set_expiry(conn, key, now, at, cond)
}

pub(crate) fn cmd_expireat(
    conn: &Connection,
    key: Arg<'_>,
    timestamp: i64,
    cond: ExpireCond,
) -> CmdResult {
    let at = seconds_to_ms(timestamp, "expireat")?;
    set_expiry(conn, key, now_ms(), at, cond)
}

pub(crate) fn cmd_pexpireat(
    conn: &Connection,
    key: Arg<'_>,
    timestamp_ms: i64,
    cond: ExpireCond,
) -> CmdResult {
    set_expiry(conn, key, now_ms(), timestamp_ms, cond)
}

/// Remaining lifetime in ms: Err(-2) missing/expired, Err(-1) no expiry.
fn remaining_ms(conn: &Connection, key: Arg<'_>) -> Result<Result<i64, i64>, CmdError> {
    let now = now_ms();
    Ok(match lookup(conn, key)? {
        Some(r) if r.is_live(now) => match r.expires_at {
            Some(t) => Ok(t - now),
            None => Err(-1),
        },
        _ => Err(-2),
    })
}

/// Redis rounds the remaining milliseconds to the nearest second, so a key
/// given 100 s reports 100 an instant later, not 99.
fn ms_to_ttl_seconds(ms: i64) -> i64 {
    (ms + 500) / 1000
}

pub(crate) fn cmd_ttl(conn: &Connection, key: Arg<'_>) -> CmdResult {
    Ok(Reply::Int(match remaining_ms(conn, key)? {
        Ok(ms) => ms_to_ttl_seconds(ms),
        Err(code) => code,
    }))
}

pub(crate) fn cmd_pttl(conn: &Connection, key: Arg<'_>) -> CmdResult {
    Ok(Reply::Int(match remaining_ms(conn, key)? {
        Ok(ms) => ms,
        Err(code) => code,
    }))
}

pub(crate) fn cmd_persist(conn: &Connection, key: Arg<'_>) -> CmdResult {
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
    let expired: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT key FROM keyspace WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        )?;
        stmt.query_map(params![now_ms()], |row| bytes_col(row, 0))?
            .collect::<Result<_, _>>()?
    };
    for key in &expired {
        remove_key(conn, Arg(key))?;
    }
    Ok(Reply::Int(expired.len() as i64))
}

#[cfg(test)]
mod tests {
    use super::ms_to_ttl_seconds;

    // Tested here as a pure function: asserting an exact TTL across two
    // commands depends on how much time passes between them.
    #[test]
    fn ttl_rounds_to_the_nearest_second() {
        assert_eq!(ms_to_ttl_seconds(99_950), 100);
        assert_eq!(ms_to_ttl_seconds(99_500), 100);
        assert_eq!(ms_to_ttl_seconds(99_499), 99);
        assert_eq!(ms_to_ttl_seconds(1_500), 2);
        assert_eq!(ms_to_ttl_seconds(1_499), 1);
        assert_eq!(ms_to_ttl_seconds(499), 0);
        assert_eq!(ms_to_ttl_seconds(1), 0);
    }
}
