use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, params};

/// Translates a Redis glob into an SQLite GLOB pattern. The two agree on
/// `*`, `?`, `[abc]`, `[^abc]` and `[a-z]`, and both are case-sensitive
/// (unlike SQL LIKE, which ignores ASCII case). The one difference is
/// escaping: Redis uses `\x`; SQLite has no escape character and instead
/// spells a literal special as a one-character class.
pub(crate) fn redis_glob_to_sqlite(pat: &str) -> String {
    let mut out = String::with_capacity(pat.len());
    let mut chars = pat.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(e @ ('*' | '?' | '[')) => {
                out.push('[');
                out.push(e);
                out.push(']');
            }
            Some(e) => out.push(e),
            None => out.push('\\'),
        }
    }
    out
}

pub(crate) fn cmd_keys(conn: &Connection, pattern: Option<&str>) -> CmdResult {
    let glob = redis_glob_to_sqlite(pattern.unwrap_or("*"));
    let mut stmt = conn.prepare(
        "SELECT key FROM keyspace
         WHERE key GLOB ?1 AND (expires_at IS NULL OR expires_at > ?2)
         ORDER BY key",
    )?;
    let keys: Vec<Vec<u8>> = stmt
        .query_map(params![glob, now_ms()], |row| bytes_col(row, 0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(keys, Empty::List))
}

pub(crate) fn cmd_exists(conn: &Connection, key: &str) -> CmdResult {
    Ok(Reply::Int(key_type(conn, key, now_ms())?.is_some() as i64))
}

pub(crate) fn cmd_type(conn: &Connection, key: &str) -> CmdResult {
    Ok(Reply::Simple(
        key_type(conn, key, now_ms())?.unwrap_or("none"),
    ))
}

pub(crate) fn cmd_rename(conn: &Connection, key: &str, newkey: &str) -> CmdResult {
    let now = now_ms();
    let src = match lookup(conn, key)? {
        Some(r) if r.is_live(now) => r,
        _ => return Err(CmdError::new("ERR no such key")),
    };
    // Renaming onto itself is a no-op (must not delete the key).
    if key == newkey {
        return Ok(Reply::Simple("OK"));
    }
    // The target is overwritten whatever its type; the source's catalogue
    // row (and so its TTL) moves with its data.
    remove_key(conn, newkey)?;
    conn.execute(
        "UPDATE keyspace SET key = ?2 WHERE key = ?1",
        params![key, newkey],
    )?;
    conn.execute(
        &format!(
            "UPDATE {} SET key = ?2 WHERE key = ?1",
            data_table(src.kind)
        ),
        params![key, newkey],
    )?;
    Ok(Reply::Simple("OK"))
}

// --- Utility commands ---

/// Live key count. Expired-but-unpurged keys are not counted.
pub(crate) fn cmd_dbsize(conn: &Connection) -> CmdResult {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM keyspace WHERE expires_at IS NULL OR expires_at > ?1",
        params![now_ms()],
        |row| row.get(0),
    )?;
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_flushall(conn: &Connection) -> CmdResult {
    conn.execute_batch(
        "DELETE FROM keyspace;
         DELETE FROM strings;
         DELETE FROM list_items;
         DELETE FROM set_members;
         DELETE FROM hash_fields;",
    )?;
    Ok(Reply::Simple("OK"))
}
