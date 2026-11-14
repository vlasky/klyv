use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

const KIND: &str = "hash";

fn field_value(
    conn: &Connection,
    key: Arg<'_>,
    field: Arg<'_>,
) -> rusqlite::Result<Option<Vec<u8>>> {
    conn.query_row(
        "SELECT value FROM hash_fields WHERE key = ?1 AND field = ?2",
        params![key, field],
        |row| bytes_col(row, 0),
    )
    .optional()
}

pub(crate) fn cmd_hset(conn: &Connection, key: Arg<'_>, pairs: &[Arg<'_>]) -> CmdResult {
    if pairs.is_empty() || !pairs.len().is_multiple_of(2) {
        return Err(CmdError::new(
            "ERR wrong number of arguments for 'hset' command",
        ));
    }
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let mut count = 0i64;
    for chunk in pairs.chunks(2) {
        let existed = field_value(conn, key, chunk[0])?.is_some();
        conn.execute(
            "INSERT OR REPLACE INTO hash_fields (key, field, value) VALUES (?1, ?2, ?3)",
            params![key, chunk[0], chunk[1]],
        )?;
        if !existed {
            count += 1;
        }
    }
    grow(conn, key, KIND, count)?;
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_hget(conn: &Connection, key: Arg<'_>, field: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Nil);
    }
    Ok(field_value(conn, key, field)?.map_or(Reply::Nil, Reply::Bulk))
}

pub(crate) fn cmd_hexists(conn: &Connection, key: Arg<'_>, field: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Int(0));
    }
    Ok(Reply::Int(field_value(conn, key, field)?.is_some() as i64))
}

pub(crate) fn cmd_hincrby(
    conn: &Connection,
    key: Arg<'_>,
    field: Arg<'_>,
    amount: i64,
) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let existing = field_value(conn, key, field)?;
    let is_new = existing.is_none();
    let val = match existing {
        Some(v) => {
            parse_int(&v).ok_or_else(|| CmdError::new("ERR hash value is not an integer"))?
        }
        None => 0,
    };
    let new_val = val
        .checked_add(amount)
        .ok_or_else(|| CmdError::new("ERR increment or decrement would overflow"))?;
    conn.execute(
        "INSERT OR REPLACE INTO hash_fields (key, field, value) VALUES (?1, ?2, ?3)",
        params![key, field, new_val.to_string()],
    )?;
    // INSERT OR REPLACE added a row only if the field was not there.
    grow(conn, key, KIND, is_new as i64)?;
    Ok(Reply::Int(new_val))
}

pub(crate) fn cmd_hdel(conn: &Connection, key: Arg<'_>, fields: &[Arg<'_>]) -> CmdResult {
    let now = now_ms();
    ensure_type(conn, key, KIND, now)?;
    drop_if_expired(conn, key, now)?;
    let mut count = 0i64;
    for field in fields {
        let deleted = conn.execute(
            "DELETE FROM hash_fields WHERE key = ?1 AND field = ?2",
            params![key, field],
        )?;
        count += deleted as i64;
    }
    shrink(conn, key, count)?;
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_hgetall(conn: &Connection, key: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Array(vec![], Empty::Hash));
    }
    let mut stmt = conn.prepare("SELECT field, value FROM hash_fields WHERE key = ?1")?;
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = stmt
        .query_map(params![key], |row| {
            Ok((bytes_col(row, 0)?, bytes_col(row, 1)?))
        })?
        .collect::<Result<_, _>>()?;
    // Alternating field, value items, numbered sequentially.
    let mut items = Vec::with_capacity(pairs.len() * 2);
    for (field, value) in pairs {
        items.push(field);
        items.push(value);
    }
    Ok(Reply::Array(items, Empty::Hash))
}

fn hash_column(conn: &Connection, key: Arg<'_>, column: &str) -> rusqlite::Result<Vec<Vec<u8>>> {
    let mut stmt = conn.prepare(&format!("SELECT {column} FROM hash_fields WHERE key = ?1"))?;
    let rows = stmt
        .query_map(params![key], |row| bytes_col(row, 0))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

pub(crate) fn cmd_hkeys(conn: &Connection, key: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Array(vec![], Empty::List));
    }
    Ok(Reply::Array(hash_column(conn, key, "field")?, Empty::List))
}

pub(crate) fn cmd_hvals(conn: &Connection, key: Arg<'_>) -> CmdResult {
    if !check_type(conn, key, KIND, now_ms())? {
        return Ok(Reply::Array(vec![], Empty::List));
    }
    Ok(Reply::Array(hash_column(conn, key, "value")?, Empty::List))
}

pub(crate) fn cmd_hlen(conn: &Connection, key: Arg<'_>) -> CmdResult {
    Ok(Reply::Int(
        live_len(conn, key, KIND, now_ms())?.unwrap_or(0),
    ))
}

/// HMGET: the values of several fields, nil for each that is missing (all
/// of them if the key is).
pub(crate) fn cmd_hmget(conn: &Connection, key: Arg<'_>, fields: &[Arg<'_>]) -> CmdResult {
    let live = check_type(conn, key, KIND, now_ms())?;
    let mut replies = Vec::with_capacity(fields.len());
    for &field in fields {
        let value = if live {
            field_value(conn, key, field)?
        } else {
            None
        };
        replies.push(value.map_or(Reply::Nil, Reply::Bulk));
    }
    Ok(Reply::Lines(replies))
}
