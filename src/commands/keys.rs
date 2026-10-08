use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, params};

// --- Key commands ---

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

    let mut all_keys: Vec<String> = Vec::new();
    for sql in [
        "SELECT key FROM strings WHERE key GLOB ?1",
        "SELECT DISTINCT key FROM list_items WHERE key GLOB ?1",
        "SELECT DISTINCT key FROM set_members WHERE key GLOB ?1",
        "SELECT DISTINCT key FROM hash_fields WHERE key GLOB ?1",
    ] {
        let mut stmt = conn.prepare(sql)?;
        let keys: Vec<String> = stmt
            .query_map(params![glob], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        all_keys.extend(keys);
    }

    all_keys.sort();
    all_keys.dedup();
    let mut live = Vec::with_capacity(all_keys.len());
    for k in all_keys {
        if !is_expired(conn, &k)? {
            live.push(k);
        }
    }

    Ok(Reply::Array(live, Empty::List))
}

pub(crate) fn cmd_exists(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    Ok(Reply::Int(key_exists_in_data(conn, key)? as i64))
}

pub(crate) fn cmd_type(conn: &Connection, key: &str) -> CmdResult {
    Ok(Reply::Simple(key_type(conn, key)?.unwrap_or("none")))
}

pub(crate) fn cmd_rename(conn: &Connection, key: &str, newkey: &str) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Err(CmdError::new("ERR no such key"));
    }
    // Renaming onto itself is a no-op (must not delete the key).
    if key == newkey {
        return Ok(Reply::Simple("OK"));
    }
    // Overwrite the target across every table so no stale rows of another
    // type survive, then move the source rows. TTL is preserved by moving
    // the expiry row along with the data.
    for t in [
        "strings",
        "list_items",
        "set_members",
        "hash_fields",
        "expiry",
    ] {
        conn.execute(&format!("DELETE FROM {t} WHERE key = ?1"), params![newkey])?;
    }
    for t in [
        "strings",
        "list_items",
        "set_members",
        "hash_fields",
        "expiry",
    ] {
        conn.execute(
            &format!("UPDATE {t} SET key = ?2 WHERE key = ?1"),
            params![key, newkey],
        )?;
    }
    Ok(Reply::Simple("OK"))
}

// --- Utility commands ---

pub(crate) fn cmd_dbsize(conn: &Connection) -> CmdResult {
    let mut count: i64 = 0;
    count += conn.query_row("SELECT COUNT(*) FROM strings", [], |row| {
        row.get::<_, i64>(0)
    })?;
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM list_items", [], |row| {
        row.get::<_, i64>(0)
    })?;
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM set_members", [], |row| {
        row.get::<_, i64>(0)
    })?;
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM hash_fields", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_flushall(conn: &Connection) -> CmdResult {
    conn.execute_batch(
        "
        DELETE FROM strings;
        DELETE FROM list_items;
        DELETE FROM set_members;
        DELETE FROM hash_fields;
        DELETE FROM expiry;
    ",
    )?;
    Ok(Reply::Simple("OK"))
}
