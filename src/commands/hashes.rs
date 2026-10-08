use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, OptionalExtension, params};

// --- Hash commands ---

pub(crate) fn cmd_hset(conn: &Connection, key: &str, pairs: &[String]) -> CmdResult {
    if !pairs.len().is_multiple_of(2) {
        return Err(CmdError::new(
            "ERR wrong number of arguments for 'hset' command",
        ));
    }
    ensure_type(conn, key, "hash")?;
    drop_if_expired(conn, key)?;
    let mut count = 0i64;
    for chunk in pairs.chunks(2) {
        let existed = conn
            .query_row(
                "SELECT 1 FROM hash_fields WHERE key = ?1 AND field = ?2",
                params![key, chunk[0]],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        conn.execute(
            "INSERT OR REPLACE INTO hash_fields (key, field, value) VALUES (?1, ?2, ?3)",
            params![key, chunk[0], chunk[1]],
        )?;
        if !existed {
            count += 1;
        }
    }
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_hget(conn: &Connection, key: &str, field: &str) -> CmdResult {
    ensure_type(conn, key, "hash")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Nil);
    }
    let result: Option<String> = conn
        .query_row(
            "SELECT value FROM hash_fields WHERE key = ?1 AND field = ?2",
            params![key, field],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match result {
        Some(v) => Reply::Bulk(v),
        None => Reply::Nil,
    })
}

pub(crate) fn cmd_hexists(conn: &Connection, key: &str, field: &str) -> CmdResult {
    ensure_type(conn, key, "hash")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let exists = conn
        .query_row(
            "SELECT 1 FROM hash_fields WHERE key = ?1 AND field = ?2",
            params![key, field],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(Reply::Int(exists as i64))
}

pub(crate) fn cmd_hincrby(conn: &Connection, key: &str, field: &str, amount: i64) -> CmdResult {
    ensure_type(conn, key, "hash")?;
    drop_if_expired(conn, key)?;
    let current: Option<String> = conn
        .query_row(
            "SELECT value FROM hash_fields WHERE key = ?1 AND field = ?2",
            params![key, field],
            |row| row.get(0),
        )
        .optional()?;
    let val: i64 = match current {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| CmdError::new("ERR hash value is not an integer"))?,
        None => 0,
    };
    let new_val = val
        .checked_add(amount)
        .ok_or_else(|| CmdError::new("ERR increment or decrement would overflow"))?;
    conn.execute(
        "INSERT OR REPLACE INTO hash_fields (key, field, value) VALUES (?1, ?2, ?3)",
        params![key, field, new_val.to_string()],
    )?;
    Ok(Reply::Int(new_val))
}

pub(crate) fn cmd_hdel(conn: &Connection, key: &str, fields: &[String]) -> CmdResult {
    ensure_type(conn, key, "hash")?;
    drop_if_expired(conn, key)?;
    let mut count = 0i64;
    for field in fields {
        let deleted = conn.execute(
            "DELETE FROM hash_fields WHERE key = ?1 AND field = ?2",
            params![key, field],
        )?;
        count += deleted as i64;
    }
    if count > 0 {
        drop_expiry_if_empty(conn, key)?;
    }
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_hgetall(conn: &Connection, key: &str) -> CmdResult {
    ensure_type(conn, key, "hash")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Array(vec![], Empty::Hash));
    }
    let mut stmt = conn.prepare("SELECT field, value FROM hash_fields WHERE key = ?1")?;
    let pairs: Vec<(String, String)> = stmt
        .query_map(params![key], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    // Alternating field, value items, numbered sequentially.
    let mut items = Vec::with_capacity(pairs.len() * 2);
    for (field, value) in pairs {
        items.push(field);
        items.push(value);
    }
    Ok(Reply::Array(items, Empty::Hash))
}

pub(crate) fn hash_column(
    conn: &Connection,
    key: &str,
    column: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("SELECT {column} FROM hash_fields WHERE key = ?1"))?;
    let rows = stmt
        .query_map(params![key], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

pub(crate) fn cmd_hkeys(conn: &Connection, key: &str) -> CmdResult {
    ensure_type(conn, key, "hash")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Array(vec![], Empty::List));
    }
    Ok(Reply::Array(hash_column(conn, key, "field")?, Empty::List))
}

pub(crate) fn cmd_hvals(conn: &Connection, key: &str) -> CmdResult {
    ensure_type(conn, key, "hash")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Array(vec![], Empty::List));
    }
    Ok(Reply::Array(hash_column(conn, key, "value")?, Empty::List))
}

pub(crate) fn cmd_hlen(conn: &Connection, key: &str) -> CmdResult {
    ensure_type(conn, key, "hash")?;
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM hash_fields WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )?;
    Ok(Reply::Int(count))
}
