use crate::cli::InsertWhere;
use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

const KIND: &str = "list";

fn empty() -> Reply {
    Reply::Array(vec![], Empty::List)
}

// Returns (element count, min idx, max idx) for a list in one query.
fn list_bounds(conn: &Connection, key: &str) -> rusqlite::Result<(i64, Option<f64>, Option<f64>)> {
    conn.query_row(
        "SELECT COUNT(*), MIN(idx), MAX(idx) FROM list_items WHERE key = ?1",
        params![key],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
}

fn push(conn: &Connection, key: &str, values: &[String], head: bool) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let (count, min_idx, max_idx) = list_bounds(conn, key)?;
    let (mut idx, step) = if head {
        (min_idx.map_or(0.0, |m| m - 1.0), -1.0)
    } else {
        (max_idx.map_or(0.0, |m| m + 1.0), 1.0)
    };
    for value in values {
        conn.execute(
            "INSERT INTO list_items (key, idx, value) VALUES (?1, ?2, ?3)",
            params![key, idx, value],
        )?;
        idx += step;
    }
    claim(conn, key, KIND)?;
    Ok(Reply::Int(count + values.len() as i64))
}

pub(crate) fn cmd_lpush(conn: &Connection, key: &str, values: &[String]) -> CmdResult {
    push(conn, key, values, true)
}

pub(crate) fn cmd_rpush(conn: &Connection, key: &str, values: &[String]) -> CmdResult {
    push(conn, key, values, false)
}

pub(crate) fn cmd_pop(conn: &Connection, key: &str, order: &str) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let result: Option<(i64, Vec<u8>)> = conn
        .query_row(
            &format!(
                "SELECT rowid, value FROM list_items WHERE key = ?1 ORDER BY idx {order} LIMIT 1"
            ),
            params![key],
            |row| Ok((row.get(0)?, bytes_col(row, 1)?)),
        )
        .optional()?;
    match result {
        Some((rowid, value)) => {
            conn.execute("DELETE FROM list_items WHERE rowid = ?1", params![rowid])?;
            release_if_empty(conn, key, KIND)?;
            Ok(Reply::Bulk(value))
        }
        None => Ok(Reply::Nil),
    }
}

// Redis range normalization: a negative start clamps to the head, but a stop
// that is still negative after adding len means the range ends before the
// head — the caller must treat s > e as empty, not clamp e back to 0.
fn normalize_range(start: i64, stop: i64, len: i64) -> (i64, i64) {
    let s = if start < 0 {
        (len + start).max(0)
    } else {
        start.min(len)
    };
    let e = if stop < 0 {
        len + stop
    } else {
        stop.min(len - 1)
    };
    (s, e)
}

pub(crate) fn cmd_lrange(conn: &Connection, key: &str, start: i64, stop: i64) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(empty());
    }
    let (len, _, _) = list_bounds(conn, key)?;
    if len == 0 {
        return Ok(empty());
    }
    let (s, e) = normalize_range(start, stop, len);
    if s > e {
        return Ok(empty());
    }
    let limit = e - s + 1;
    let mut stmt = conn.prepare(
        "SELECT value FROM list_items WHERE key = ?1 ORDER BY idx ASC LIMIT ?2 OFFSET ?3",
    )?;
    let rows: Vec<Vec<u8>> = stmt
        .query_map(params![key, limit, s], |row| bytes_col(row, 0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::List))
}

pub(crate) fn cmd_llen(conn: &Connection, key: &str) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Int(0));
    }
    let (len, _, _) = list_bounds(conn, key)?;
    Ok(Reply::Int(len))
}

pub(crate) fn cmd_lrem(conn: &Connection, key: &str, count: i64, value: &str) -> CmdResult {
    let (order, limit) = match count.cmp(&0) {
        std::cmp::Ordering::Greater => ("ASC", count.unsigned_abs() as usize),
        std::cmp::Ordering::Less => ("DESC", count.unsigned_abs() as usize),
        std::cmp::Ordering::Equal => ("ASC", usize::MAX),
    };
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let sql =
        format!("SELECT rowid FROM list_items WHERE key = ?1 AND value = ?2 ORDER BY idx {order}");
    let rowids: Vec<i64> = {
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_map(params![key, value], |row| row.get(0))?
            .take(limit)
            .collect::<Result<_, _>>()?
    };
    let removed = rowids.len() as i64;
    for rowid in &rowids {
        conn.execute("DELETE FROM list_items WHERE rowid = ?1", params![rowid])?;
    }
    if removed > 0 {
        release_if_empty(conn, key, KIND)?;
    }
    Ok(Reply::Int(removed))
}

pub(crate) fn cmd_lpos(conn: &Connection, key: &str, value: &str) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Nil);
    }
    let mut stmt = conn.prepare("SELECT value FROM list_items WHERE key = ?1 ORDER BY idx ASC")?;
    // Stream rows and stop at the first match instead of loading the whole list.
    let mut rows = stmt.query(params![key])?;
    let mut pos: i64 = 0;
    while let Some(row) = rows.next()? {
        if bytes_col(row, 0)? == value.as_bytes() {
            return Ok(Reply::Int(pos));
        }
        pos += 1;
    }
    Ok(Reply::Nil)
}

// Normalizes a possibly-negative list index to 0-based; None if out of range.
fn normalize_index(index: i64, len: i64) -> Option<i64> {
    let i = if index < 0 { len + index } else { index };
    (0..len).contains(&i).then_some(i)
}

pub(crate) fn cmd_lindex(conn: &Connection, key: &str, index: i64) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Nil);
    }
    let (len, _, _) = list_bounds(conn, key)?;
    let Some(i) = normalize_index(index, len) else {
        return Ok(Reply::Nil);
    };
    let value: Option<Vec<u8>> = conn
        .query_row(
            "SELECT value FROM list_items WHERE key = ?1 ORDER BY idx ASC LIMIT 1 OFFSET ?2",
            params![key, i],
            |row| bytes_col(row, 0),
        )
        .optional()?;
    Ok(value.map_or(Reply::Nil, Reply::Bulk))
}

pub(crate) fn cmd_lset(conn: &Connection, key: &str, index: i64, value: &str) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let (len, _, _) = list_bounds(conn, key)?;
    if len == 0 {
        return Err(CmdError::new("ERR no such key"));
    }
    let i = normalize_index(index, len).ok_or_else(|| CmdError::new("ERR index out of range"))?;
    let rowid: i64 = conn.query_row(
        "SELECT rowid FROM list_items WHERE key = ?1 ORDER BY idx ASC LIMIT 1 OFFSET ?2",
        params![key, i],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE list_items SET value = ?2 WHERE rowid = ?1",
        params![rowid, value],
    )?;
    Ok(Reply::Simple("OK"))
}

pub(crate) fn cmd_ltrim(conn: &Connection, key: &str, start: i64, stop: i64) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let (len, _, _) = list_bounds(conn, key)?;
    if len == 0 {
        return Ok(Reply::Simple("OK"));
    }
    let (s, e) = normalize_range(start, stop, len);
    if s > e {
        // Everything trimmed away: the key ceases to exist.
        remove_key(conn, key)?;
        return Ok(Reply::Simple("OK"));
    }
    // Delete rows outside positions [s, e] by rank.
    conn.execute(
        "DELETE FROM list_items WHERE rowid IN (
            SELECT rowid FROM list_items WHERE key = ?1 ORDER BY idx ASC LIMIT ?2
        )",
        params![key, s],
    )?;
    conn.execute(
        "DELETE FROM list_items WHERE rowid IN (
            SELECT rowid FROM list_items WHERE key = ?1 ORDER BY idx DESC LIMIT ?2
        )",
        params![key, len - 1 - e],
    )?;
    Ok(Reply::Simple("OK"))
}

// Rewrites a list's fractional indexes as sequential integers. Called when
// repeated LINSERTs into the same gap exhaust f64 midpoint precision.
fn renumber_list(conn: &Connection, key: &str) -> rusqlite::Result<()> {
    let rowids: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT rowid FROM list_items WHERE key = ?1 ORDER BY idx ASC")?;
        stmt.query_map(params![key], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    for (i, rowid) in rowids.iter().enumerate() {
        conn.execute(
            "UPDATE list_items SET idx = ?2 WHERE rowid = ?1",
            params![rowid, i as f64],
        )?;
    }
    Ok(())
}

pub(crate) fn cmd_linsert(
    conn: &Connection,
    key: &str,
    place: InsertWhere,
    pivot: &str,
    value: &str,
) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let (len, _, _) = list_bounds(conn, key)?;
    if len == 0 {
        return Ok(Reply::Int(0));
    }
    // Two passes at most: if the midpoint between neighbours is no longer
    // representable, renumber the list to integer indexes and try again.
    for attempt in 0..2 {
        // First occurrence of the pivot, searching from the head.
        let pivot_idx: Option<f64> = conn
            .query_row(
                "SELECT idx FROM list_items WHERE key = ?1 AND value = ?2 ORDER BY idx ASC LIMIT 1",
                params![key, pivot],
                |row| row.get(0),
            )
            .optional()?;
        let Some(p) = pivot_idx else {
            return Ok(Reply::Int(-1));
        };
        let (neighbour_sql, fallback) = match place {
            InsertWhere::Before => (
                "SELECT MAX(idx) FROM list_items WHERE key = ?1 AND idx < ?2",
                p - 1.0,
            ),
            InsertWhere::After => (
                "SELECT MIN(idx) FROM list_items WHERE key = ?1 AND idx > ?2",
                p + 1.0,
            ),
        };
        let neighbour: Option<f64> =
            conn.query_row(neighbour_sql, params![key, p], |row| row.get(0))?;
        let new_idx = neighbour.map_or(fallback, |n| (n + p) / 2.0);
        if new_idx != p && Some(new_idx) != neighbour {
            conn.execute(
                "INSERT INTO list_items (key, idx, value) VALUES (?1, ?2, ?3)",
                params![key, new_idx, value],
            )?;
            return Ok(Reply::Int(len + 1));
        }
        if attempt == 0 {
            renumber_list(conn, key)?;
        }
    }
    // Unreachable: after renumbering, adjacent indexes differ by 1.0 and the
    // midpoint is always representable.
    Err(CmdError::new("ERR could not compute insert position"))
}
