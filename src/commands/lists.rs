use crate::cli::InsertWhere;
use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

const KIND: &str = "list";

fn empty() -> Reply {
    Reply::Array(vec![], Empty::List)
}

// The idx a new head (or tail) element takes: one beyond the current end,
// or 0.0 for an empty list. A lone MIN or MAX over the (key, idx) index is
// a single seek, not a scan.
fn next_idx(conn: &Connection, key: Arg<'_>, head: bool) -> rusqlite::Result<f64> {
    let sql = if head {
        "SELECT MIN(idx) FROM list_items WHERE key = ?1"
    } else {
        "SELECT MAX(idx) FROM list_items WHERE key = ?1"
    };
    let end: Option<f64> = conn.query_row(sql, params![key], |row| row.get(0))?;
    Ok(match (end, head) {
        (Some(min), true) => min - 1.0,
        (Some(max), false) => max + 1.0,
        (None, _) => 0.0,
    })
}

fn push(conn: &Connection, key: Arg<'_>, values: &[Arg<'_>], head: bool) -> CmdResult {
    if values.is_empty() {
        let name = if head { "lpush" } else { "rpush" };
        return Err(CmdError::new(format!(
            "ERR wrong number of arguments for '{name}' command"
        )));
    }
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let mut idx = next_idx(conn, key, head)?;
    let step = if head { -1.0 } else { 1.0 };
    for value in values {
        conn.execute(
            "INSERT INTO list_items (key, idx, value) VALUES (?1, ?2, ?3)",
            params![key, idx, value],
        )?;
        idx += step;
    }
    Ok(Reply::Int(grow(conn, key, KIND, values.len() as i64)?))
}

pub(crate) fn cmd_lpush(conn: &Connection, key: Arg<'_>, values: &[Arg<'_>]) -> CmdResult {
    push(conn, key, values, true)
}

pub(crate) fn cmd_rpush(conn: &Connection, key: Arg<'_>, values: &[Arg<'_>]) -> CmdResult {
    push(conn, key, values, false)
}

pub(crate) fn cmd_pop(conn: &Connection, key: Arg<'_>, order: &str) -> CmdResult {
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
            shrink(conn, key, 1)?;
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

// Positions are reached by walking the (key, idx) index, so the cost is the
// distance walked. Knowing the length, walk from whichever end is nearer:
// the sort direction and the offset from that end for 0-based position `i`.
// (Redis's lists have the same from-the-nearer-end cost.) The rowid
// tiebreak makes the descending order the exact mirror of the ascending one.
fn seek(i: i64, len: i64) -> (&'static str, i64) {
    if i <= len / 2 {
        ("idx ASC, rowid ASC", i)
    } else {
        ("idx DESC, rowid DESC", len - 1 - i)
    }
}

pub(crate) fn cmd_lrange(conn: &Connection, key: Arg<'_>, start: i64, stop: i64) -> CmdResult {
    let Some(len) = live_len(conn, key, KIND, now_ms())? else {
        return Ok(empty());
    };
    let (s, e) = normalize_range(start, stop, len);
    if s > e {
        return Ok(empty());
    }
    let limit = e - s + 1;
    // Start from the end the range is nearer to; a range read backwards from
    // the tail is reversed afterwards.
    let from_tail = len - 1 - e < s;
    let (order, offset) = if from_tail {
        ("idx DESC, rowid DESC", len - 1 - e)
    } else {
        ("idx ASC, rowid ASC", s)
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT value FROM list_items WHERE key = ?1 ORDER BY {order} LIMIT ?2 OFFSET ?3"
    ))?;
    let mut rows: Vec<Vec<u8>> = stmt
        .query_map(params![key, limit, offset], |row| bytes_col(row, 0))?
        .collect::<Result<_, _>>()?;
    if from_tail {
        rows.reverse();
    }
    Ok(Reply::Array(rows, Empty::List))
}

pub(crate) fn cmd_llen(conn: &Connection, key: Arg<'_>) -> CmdResult {
    Ok(Reply::Int(
        live_len(conn, key, KIND, now_ms())?.unwrap_or(0),
    ))
}

pub(crate) fn cmd_lrem(conn: &Connection, key: Arg<'_>, count: i64, value: Arg<'_>) -> CmdResult {
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
    shrink(conn, key, removed)?;
    Ok(Reply::Int(removed))
}

pub(crate) fn cmd_lpos(conn: &Connection, key: Arg<'_>, value: Arg<'_>) -> CmdResult {
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

pub(crate) fn cmd_lindex(conn: &Connection, key: Arg<'_>, index: i64) -> CmdResult {
    let Some(len) = live_len(conn, key, KIND, now_ms())? else {
        return Ok(Reply::Nil);
    };
    let Some(i) = normalize_index(index, len) else {
        return Ok(Reply::Nil);
    };
    let (order, offset) = seek(i, len);
    let value: Option<Vec<u8>> = conn
        .query_row(
            &format!(
                "SELECT value FROM list_items WHERE key = ?1 ORDER BY {order} LIMIT 1 OFFSET ?2"
            ),
            params![key, offset],
            |row| bytes_col(row, 0),
        )
        .optional()?;
    Ok(value.map_or(Reply::Nil, Reply::Bulk))
}

pub(crate) fn cmd_lset(conn: &Connection, key: Arg<'_>, index: i64, value: Arg<'_>) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let len = live_len(conn, key, KIND, now)?.unwrap_or(0);
    if len == 0 {
        return Err(CmdError::new("ERR no such key"));
    }
    let i = normalize_index(index, len).ok_or_else(|| CmdError::new("ERR index out of range"))?;
    let (order, offset) = seek(i, len);
    let rowid: i64 = conn.query_row(
        &format!("SELECT rowid FROM list_items WHERE key = ?1 ORDER BY {order} LIMIT 1 OFFSET ?2"),
        params![key, offset],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE list_items SET value = ?2 WHERE rowid = ?1",
        params![rowid, value],
    )?;
    Ok(Reply::Simple("OK"))
}

pub(crate) fn cmd_ltrim(conn: &Connection, key: Arg<'_>, start: i64, stop: i64) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let len = live_len(conn, key, KIND, now)?.unwrap_or(0);
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
    let head = conn.execute(
        "DELETE FROM list_items WHERE rowid IN (
            SELECT rowid FROM list_items WHERE key = ?1 ORDER BY idx ASC LIMIT ?2
        )",
        params![key, s],
    )?;
    let tail = conn.execute(
        "DELETE FROM list_items WHERE rowid IN (
            SELECT rowid FROM list_items WHERE key = ?1 ORDER BY idx DESC LIMIT ?2
        )",
        params![key, len - 1 - e],
    )?;
    shrink(conn, key, (head + tail) as i64)?;
    Ok(Reply::Simple("OK"))
}

// Rewrites a list's fractional indexes as sequential integers. Called when
// repeated LINSERTs into the same gap exhaust f64 midpoint precision.
fn renumber_list(conn: &Connection, key: Arg<'_>) -> rusqlite::Result<()> {
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
    key: Arg<'_>,
    place: InsertWhere,
    pivot: Arg<'_>,
    value: Arg<'_>,
) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let len = live_len(conn, key, KIND, now)?.unwrap_or(0);
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
            return Ok(Reply::Int(grow(conn, key, KIND, 1)?));
        }
        if attempt == 0 {
            renumber_list(conn, key)?;
        }
    }
    // Unreachable: after renumbering, adjacent indexes differ by 1.0 and the
    // midpoint is always representable.
    Err(CmdError::new("ERR could not compute insert position"))
}

fn out_of_range() -> CmdError {
    CmdError::new("ERR value is out of range, must be positive")
}

/// LPOP/RPOP with a count: up to `count` elements, in pop order. A missing
/// key is a null array (not an empty one), as in Redis.
pub(crate) fn cmd_pop_count(conn: &Connection, key: Arg<'_>, order: &str, count: i64) -> CmdResult {
    if count < 0 {
        return Err(out_of_range());
    }
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    if live_len(conn, key, KIND, now)?.is_none() {
        return Ok(Reply::NullArray);
    }
    let popped: Vec<(i64, Vec<u8>)> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT rowid, value FROM list_items WHERE key = ?1 ORDER BY idx {order} LIMIT ?2"
        ))?;
        stmt.query_map(params![key, count], |row| {
            Ok((row.get(0)?, bytes_col(row, 1)?))
        })?
        .collect::<Result<_, _>>()?
    };
    for (rowid, _) in &popped {
        conn.execute("DELETE FROM list_items WHERE rowid = ?1", params![rowid])?;
    }
    shrink(conn, key, popped.len() as i64)?;
    Ok(Reply::Array(
        popped.into_iter().map(|(_, v)| v).collect(),
        Empty::List,
    ))
}
