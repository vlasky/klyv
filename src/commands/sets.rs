use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

const KIND: &str = "set";

fn empty() -> Reply {
    Reply::Array(vec![], Empty::Set)
}

fn members(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Reply> {
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<Vec<u8>> = stmt
        .query_map(params, |row| bytes_col(row, 0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}

pub(crate) fn cmd_sadd(conn: &Connection, key: Arg<'_>, members: &[Arg<'_>]) -> CmdResult {
    if members.is_empty() {
        return Err(CmdError::new(
            "ERR wrong number of arguments for 'sadd' command",
        ));
    }
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let mut count = 0i64;
    for member in members {
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO set_members (key, member) VALUES (?1, ?2)",
            params![key, member],
        )?;
        count += inserted as i64;
    }
    grow(conn, key, KIND, count)?;
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_srem(conn: &Connection, key: Arg<'_>, members: &[Arg<'_>]) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let mut count = 0i64;
    for member in members {
        let deleted = conn.execute(
            "DELETE FROM set_members WHERE key = ?1 AND member = ?2",
            params![key, member],
        )?;
        count += deleted as i64;
    }
    shrink(conn, key, count)?;
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_smembers(conn: &Connection, key: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(empty());
    }
    Ok(members(
        conn,
        "SELECT member FROM set_members WHERE key = ?1",
        &[&key],
    )?)
}

pub(crate) fn cmd_sismember(conn: &Connection, key: Arg<'_>, member: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Int(0));
    }
    let exists = conn
        .query_row(
            "SELECT 1 FROM set_members WHERE key = ?1 AND member = ?2",
            params![key, member],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(Reply::Int(exists as i64))
}

pub(crate) fn cmd_scard(conn: &Connection, key: Arg<'_>) -> CmdResult {
    Ok(Reply::Int(
        live_len(conn, key, KIND, now_ms())?.unwrap_or(0),
    ))
}

pub(crate) fn cmd_spop(conn: &Connection, key: Arg<'_>) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let picked: Option<(i64, Vec<u8>)> = conn
        .query_row(
            "SELECT rowid, member FROM set_members WHERE key = ?1 ORDER BY RANDOM() LIMIT 1",
            params![key],
            |row| Ok((row.get(0)?, bytes_col(row, 1)?)),
        )
        .optional()?;
    match picked {
        Some((rowid, member)) => {
            conn.execute("DELETE FROM set_members WHERE rowid = ?1", params![rowid])?;
            shrink(conn, key, 1)?;
            Ok(Reply::Bulk(member))
        }
        None => Ok(Reply::Nil),
    }
}

/// Type-checks every input key (WRONGTYPE on any mismatch) and returns the
/// ones that are live; missing/expired input sets are treated as empty.
fn live_sets<'a>(conn: &Connection, keys: &[Arg<'a>], now: i64) -> Result<Vec<Arg<'a>>, CmdError> {
    let mut live = Vec::with_capacity(keys.len());
    for &k in keys {
        if check_type(conn, k, KIND, now)? {
            live.push(k);
        }
    }
    Ok(live)
}

fn placeholders(n: usize, start: usize) -> String {
    (0..n)
        .map(|i| format!("?{}", i + start))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn cmd_sunion(conn: &Connection, keys: &[Arg<'_>]) -> CmdResult {
    let live = live_sets(conn, keys, now_ms())?;
    if live.is_empty() {
        return Ok(empty());
    }
    let sql = format!(
        "SELECT DISTINCT member FROM set_members WHERE key IN ({})",
        placeholders(live.len(), 1)
    );
    let params: Vec<&dyn rusqlite::ToSql> =
        live.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
    Ok(members(conn, &sql, &params)?)
}

pub(crate) fn cmd_sinter(conn: &Connection, keys: &[Arg<'_>]) -> CmdResult {
    if keys.is_empty() {
        return Ok(empty());
    }
    // Any missing/expired input set makes the intersection empty.
    let live = live_sets(conn, keys, now_ms())?;
    if live.len() != keys.len() {
        return Ok(empty());
    }
    // Dedup keys so repeated args don't break the COUNT(DISTINCT key) test.
    let mut keys = live;
    keys.sort();
    keys.dedup();
    let n = keys.len();
    let sql = format!(
        "SELECT member FROM set_members WHERE key IN ({}) GROUP BY member HAVING COUNT(DISTINCT key) = ?{}",
        placeholders(n, 1),
        n + 1
    );
    let count = n as i64;
    let mut params: Vec<&dyn rusqlite::ToSql> =
        keys.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
    params.push(&count);
    Ok(members(conn, &sql, &params)?)
}

pub(crate) fn cmd_sdiff(conn: &Connection, keys: &[Arg<'_>]) -> CmdResult {
    let Some((first, rest)) = keys.split_first() else {
        return Ok(empty());
    };
    let now = now_ms();
    let first = *first;
    if !check_type(conn, first, KIND, now)? {
        // Still type-check the others so a wrong-type argument is an error.
        live_sets(conn, rest, now)?;
        return Ok(empty());
    }
    // Missing/expired "other" sets subtract nothing, so drop them.
    let rest = live_sets(conn, rest, now)?;
    if rest.is_empty() {
        return Ok(members(
            conn,
            "SELECT member FROM set_members WHERE key = ?1",
            &[&first],
        )?);
    }
    let sql = format!(
        "SELECT member FROM set_members WHERE key = ?1 AND member NOT IN (
            SELECT member FROM set_members WHERE key IN ({}))",
        placeholders(rest.len(), 2)
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&first as &dyn rusqlite::ToSql];
    params.extend(rest.iter().map(|k| k as &dyn rusqlite::ToSql));
    Ok(members(conn, &sql, &params)?)
}

fn out_of_range() -> CmdError {
    CmdError::new("ERR value is out of range, must be positive")
}

/// SPOP with a count: remove and return up to `count` random members.
pub(crate) fn cmd_spop_count(conn: &Connection, key: Arg<'_>, count: i64) -> CmdResult {
    if count < 0 {
        return Err(out_of_range());
    }
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let picked: Vec<(i64, Vec<u8>)> = {
        let mut stmt = conn.prepare(
            "SELECT rowid, member FROM set_members WHERE key = ?1 ORDER BY RANDOM() LIMIT ?2",
        )?;
        stmt.query_map(params![key, count], |row| {
            Ok((row.get(0)?, bytes_col(row, 1)?))
        })?
        .collect::<Result<_, _>>()?
    };
    for (rowid, _) in &picked {
        conn.execute("DELETE FROM set_members WHERE rowid = ?1", params![rowid])?;
    }
    shrink(conn, key, picked.len() as i64)?;
    Ok(Reply::Array(
        picked.into_iter().map(|(_, m)| m).collect(),
        Empty::Set,
    ))
}

/// SRANDMEMBER. Without a count: one random member, or nil. With a
/// non-negative count: up to that many *distinct* members. With a negative
/// count: exactly that many picks, repeats allowed (none if the set is
/// missing). The counted forms reply with an array, not a set, because the
/// result may contain repeats.
pub(crate) fn cmd_srandmember(conn: &Connection, key: Arg<'_>, count: Option<i64>) -> CmdResult {
    let live = check_type(conn, key, KIND, now_ms())?;
    let distinct = |limit: i64| -> rusqlite::Result<Vec<Vec<u8>>> {
        let mut stmt = conn
            .prepare("SELECT member FROM set_members WHERE key = ?1 ORDER BY RANDOM() LIMIT ?2")?;
        stmt.query_map(params![key, limit], |row| bytes_col(row, 0))?
            .collect()
    };
    let Some(count) = count else {
        let one = if live { distinct(1)?.pop() } else { None };
        return Ok(one.map_or(Reply::Nil, Reply::Bulk));
    };
    if !live || count == 0 {
        return Ok(Reply::Array(vec![], Empty::List));
    }
    if count > 0 {
        return Ok(Reply::Array(distinct(count)?, Empty::List));
    }
    // Redis's own bound, which also keeps the negation below in range.
    if count < -(i64::MAX / 2) {
        return Err(CmdError::new("ERR value is out of range"));
    }
    let all = distinct(i64::MAX)?;
    let picks: Vec<i64> = {
        // SQLite is the only source of randomness in the binary; one query
        // draws every index.
        let mut stmt = conn.prepare(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             SELECT abs(random() % ?2) FROM n",
        )?;
        stmt.query_map(params![-count, all.len() as i64], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    Ok(Reply::Array(
        picks.into_iter().map(|i| all[i as usize].clone()).collect(),
        Empty::List,
    ))
}

/// SMOVE: move a member from one set to another. 1 if it was moved, 0 if
/// the source does not hold it.
pub(crate) fn cmd_smove(
    conn: &Connection,
    src: Arg<'_>,
    dst: Arg<'_>,
    member: Arg<'_>,
) -> CmdResult {
    let now = now_ms();
    // A missing source is 0 before the destination is even looked at.
    if !check_type(conn, src, KIND, now)? {
        return Ok(Reply::Int(0));
    }
    ensure_type(conn, dst, KIND, now)?;
    if src == dst {
        return cmd_sismember(conn, src, member);
    }
    let removed = conn.execute(
        "DELETE FROM set_members WHERE key = ?1 AND member = ?2",
        params![src, member],
    )?;
    if removed == 0 {
        return Ok(Reply::Int(0));
    }
    shrink(conn, src, 1)?;
    drop_if_expired(conn, dst, now)?;
    let added = conn.execute(
        "INSERT OR IGNORE INTO set_members (key, member) VALUES (?1, ?2)",
        params![dst, member],
    )?;
    // Zero only if the destination already held the member, in which case
    // the destination exists and adding 0 to its length is harmless.
    grow(conn, dst, KIND, added as i64)?;
    Ok(Reply::Int(1))
}
