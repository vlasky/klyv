use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

// --- Set commands ---

pub(crate) fn cmd_sadd(conn: &Connection, key: &str, members: &[String]) -> CmdResult {
    ensure_type(conn, key, "set")?;
    drop_if_expired(conn, key)?;
    let mut count = 0i64;
    for member in members {
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO set_members (key, member) VALUES (?1, ?2)",
            params![key, member],
        )?;
        count += inserted as i64;
    }
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_srem(conn: &Connection, key: &str, members: &[String]) -> CmdResult {
    ensure_type(conn, key, "set")?;
    drop_if_expired(conn, key)?;
    let mut count = 0i64;
    for member in members {
        let deleted = conn.execute(
            "DELETE FROM set_members WHERE key = ?1 AND member = ?2",
            params![key, member],
        )?;
        count += deleted as i64;
    }
    if count > 0 {
        drop_expiry_if_empty(conn, key)?;
    }
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_smembers(conn: &Connection, key: &str) -> CmdResult {
    ensure_type(conn, key, "set")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    let mut stmt = conn.prepare("SELECT member FROM set_members WHERE key = ?1")?;
    let rows: Vec<String> = stmt
        .query_map(params![key], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}

pub(crate) fn cmd_sismember(conn: &Connection, key: &str, member: &str) -> CmdResult {
    ensure_type(conn, key, "set")?;
    if is_expired(conn, key)? {
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
    ensure_type(conn, key, "set")?;
    if is_expired(conn, key)? {
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
    ensure_type(conn, key, "set")?;
    drop_if_expired(conn, key)?;
    let picked: Option<(i64, String)> = conn
        .query_row(
            "SELECT rowid, member FROM set_members WHERE key = ?1 ORDER BY RANDOM() LIMIT 1",
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match picked {
        Some((rowid, member)) => {
            conn.execute("DELETE FROM set_members WHERE rowid = ?1", params![rowid])?;
            drop_expiry_if_empty(conn, key)?;
            Ok(Reply::Bulk(member))
        }
        None => Ok(Reply::Nil),
    }
}

pub(crate) fn cmd_sunion(conn: &Connection, keys: &[String]) -> CmdResult {
    for k in keys {
        ensure_type(conn, k, "set")?;
    }
    // Expired input sets are treated as empty and contribute nothing.
    let mut live: Vec<&String> = Vec::with_capacity(keys.len());
    for k in keys {
        if !is_expired(conn, k)? {
            live.push(k);
        }
    }
    if live.is_empty() {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    let placeholders: Vec<String> = live
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect();
    let sql = format!(
        "SELECT DISTINCT member FROM set_members WHERE key IN ({})",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> =
        live.iter().map(|k| *k as &dyn rusqlite::ToSql).collect();
    let rows: Vec<String> = stmt
        .query_map(params.as_slice(), |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}

pub(crate) fn cmd_sinter(conn: &Connection, keys: &[String]) -> CmdResult {
    for k in keys {
        ensure_type(conn, k, "set")?;
    }
    if keys.is_empty() {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    // Any expired/missing input set makes the intersection empty.
    for k in keys {
        if is_expired(conn, k)? {
            return Ok(Reply::Array(vec![], Empty::Set));
        }
    }
    // Dedup keys so repeated args don't break the COUNT(DISTINCT key) test.
    let mut keys: Vec<&String> = keys.iter().collect();
    keys.sort();
    keys.dedup();
    let num_keys = keys.len();
    let placeholders: Vec<String> = keys
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect();
    let sql = format!(
        "SELECT member FROM set_members WHERE key IN ({}) GROUP BY member HAVING COUNT(DISTINCT key) = ?{}",
        placeholders.join(", "),
        num_keys + 1
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = keys
        .iter()
        .map(|k| Box::new((*k).clone()) as Box<dyn rusqlite::ToSql>)
        .collect();
    params.push(Box::new(num_keys as i64));
    let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows: Vec<String> = stmt
        .query_map(params_ref.as_slice(), |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}

pub(crate) fn cmd_sdiff(conn: &Connection, keys: &[String]) -> CmdResult {
    for k in keys {
        ensure_type(conn, k, "set")?;
    }
    if keys.is_empty() {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    let first = &keys[0];
    if is_expired(conn, first)? {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    if keys.len() == 1 {
        return cmd_smembers(conn, first);
    }
    // Expired "other" sets subtract nothing, so drop them.
    let mut rest: Vec<&String> = Vec::with_capacity(keys.len() - 1);
    for k in &keys[1..] {
        if !is_expired(conn, k)? {
            rest.push(k);
        }
    }
    if rest.is_empty() {
        return cmd_smembers(conn, first);
    }
    let placeholders: Vec<String> = rest
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect();
    let sql = format!(
        "SELECT member FROM set_members WHERE key = ?1 AND member NOT IN (SELECT member FROM set_members WHERE key IN ({}))",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![first as &dyn rusqlite::ToSql];
    for k in &rest {
        params.push(*k as &dyn rusqlite::ToSql);
    }
    let rows: Vec<String> = stmt
        .query_map(params.as_slice(), |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}
