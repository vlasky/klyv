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

pub(crate) fn cmd_sadd(conn: &Connection, key: &str, members: &[String]) -> CmdResult {
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
    claim(conn, key, KIND)?;
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_srem(conn: &Connection, key: &str, members: &[String]) -> CmdResult {
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
    if count > 0 {
        release_if_empty(conn, key, KIND)?;
    }
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_smembers(conn: &Connection, key: &str) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(empty());
    }
    Ok(members(
        conn,
        "SELECT member FROM set_members WHERE key = ?1",
        &[&key],
    )?)
}

pub(crate) fn cmd_sismember(conn: &Connection, key: &str, member: &str) -> CmdResult {
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

pub(crate) fn cmd_scard(conn: &Connection, key: &str) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Int(0));
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM set_members WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )?;
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_spop(conn: &Connection, key: &str) -> CmdResult {
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
            release_if_empty(conn, key, KIND)?;
            Ok(Reply::Bulk(member))
        }
        None => Ok(Reply::Nil),
    }
}

/// Type-checks every input key (WRONGTYPE on any mismatch) and returns the
/// ones that are live; missing/expired input sets are treated as empty.
fn live_sets<'a>(
    conn: &Connection,
    keys: &'a [String],
    now: i64,
) -> Result<Vec<&'a String>, CmdError> {
    let mut live = Vec::with_capacity(keys.len());
    for k in keys {
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

pub(crate) fn cmd_sunion(conn: &Connection, keys: &[String]) -> CmdResult {
    let live = live_sets(conn, keys, now_ms())?;
    if live.is_empty() {
        return Ok(empty());
    }
    let sql = format!(
        "SELECT DISTINCT member FROM set_members WHERE key IN ({})",
        placeholders(live.len(), 1)
    );
    let params: Vec<&dyn rusqlite::ToSql> =
        live.iter().map(|k| *k as &dyn rusqlite::ToSql).collect();
    Ok(members(conn, &sql, &params)?)
}

pub(crate) fn cmd_sinter(conn: &Connection, keys: &[String]) -> CmdResult {
    if keys.is_empty() {
        return Ok(empty());
    }
    // Any missing/expired input set makes the intersection empty.
    let live = live_sets(conn, keys, now_ms())?;
    if live.len() != keys.len() {
        return Ok(empty());
    }
    // Dedup keys so repeated args don't break the COUNT(DISTINCT key) test.
    let mut keys: Vec<&String> = live;
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
        keys.iter().map(|k| *k as &dyn rusqlite::ToSql).collect();
    params.push(&count);
    Ok(members(conn, &sql, &params)?)
}

pub(crate) fn cmd_sdiff(conn: &Connection, keys: &[String]) -> CmdResult {
    let Some((first, rest)) = keys.split_first() else {
        return Ok(empty());
    };
    let now = now_ms();
    if !check_type(conn, first, KIND, now)? {
        // Still type-check the others so a wrong-type argument is an error.
        live_sets(conn, rest, now)?;
        return Ok(empty());
    }
    // Missing/expired "other" sets subtract nothing, so drop them.
    let rest = live_sets(conn, rest, now)?;
    if rest.is_empty() {
        return cmd_smembers(conn, first);
    }
    let sql = format!(
        "SELECT member FROM set_members WHERE key = ?1 AND member NOT IN (
            SELECT member FROM set_members WHERE key IN ({}))",
        placeholders(rest.len(), 2)
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![first as &dyn rusqlite::ToSql];
    params.extend(rest.iter().map(|k| *k as &dyn rusqlite::ToSql));
    Ok(members(conn, &sql, &params)?)
}
