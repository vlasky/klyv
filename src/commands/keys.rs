use crate::db::*;
use crate::reply::*;
use rusqlite::{Connection, params};

/// Redis glob matching, a port of `stringmatchlen` from Redis's util.c so
/// that malformed and escaped patterns behave identically: an unclosed class
/// (`[a`) still matches, `[]` matches nothing, a reversed range (`[c-a]`) is
/// swapped, and `\` escapes inside classes as well as outside. SQLite's GLOB
/// agrees on well-formed patterns but not on these.
///
/// Like the original, a `*` that fails against the whole remainder sets
/// `skip_longer`, which stops enclosing `*`s retrying longer prefixes and
/// keeps matching linear-ish rather than exponential in the number of stars.
/// Also like the original, recursion (one level per `*`) is capped at
/// `MAX_NESTING`; a pattern that needs more simply does not match, so a
/// hostile pattern cannot overflow the stack.
///
/// Ranges compare bytes as *signed*, because Redis compares C `char`s and
/// `char` is signed on x86 and on Apple ARM (verified against a live server:
/// `[a-\xc3]` does not contain `b`, since 0xc3 is -61 and the reversed range
/// is swapped to -61..=97). Linux ARM builds of Redis, where `char` is
/// unsigned, differ; that only affects ranges with an endpoint above 0x7f.
pub(crate) fn glob_match(pattern: &[u8], string: &[u8]) -> bool {
    let mut skip_longer = false;
    glob_match_impl(pattern, string, &mut skip_longer, 0)
}

const MAX_NESTING: usize = 1000;

fn glob_match_impl(mut p: &[u8], mut s: &[u8], skip_longer: &mut bool, nesting: usize) -> bool {
    if nesting > MAX_NESTING {
        return false;
    }
    while !p.is_empty() && !s.is_empty() {
        match p[0] {
            b'*' => {
                while p.len() > 1 && p[1] == b'*' {
                    p = &p[1..];
                }
                if p.len() == 1 {
                    return true;
                }
                while !s.is_empty() {
                    if glob_match_impl(&p[1..], s, skip_longer, nesting + 1) {
                        return true;
                    }
                    if *skip_longer {
                        return false;
                    }
                    s = &s[1..];
                }
                *skip_longer = true;
                return false;
            }
            b'?' => s = &s[1..],
            b'[' => {
                p = &p[1..];
                let negate = p.first() == Some(&b'^');
                if negate {
                    p = &p[1..];
                }
                let c = s[0];
                let mut matched = false;
                // Leaves `p` at the closing `]`, or empty if the class is
                // unclosed (Redis then treats the class as ending there).
                loop {
                    if p.len() >= 2 && p[0] == b'\\' {
                        p = &p[1..];
                        matched |= p[0] == c;
                    } else if p.is_empty() || p[0] == b']' {
                        break;
                    } else if p.len() >= 3 && p[1] == b'-' {
                        let (a, b) = (p[0] as i8, p[2] as i8);
                        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                        p = &p[2..];
                        matched |= (lo..=hi).contains(&(c as i8));
                    } else {
                        matched |= p[0] == c;
                    }
                    p = &p[1..];
                }
                if matched == negate {
                    return false;
                }
                s = &s[1..];
            }
            other => {
                let lit = if other == b'\\' && p.len() >= 2 {
                    p = &p[1..];
                    p[0]
                } else {
                    other
                };
                if lit != s[0] {
                    return false;
                }
                s = &s[1..];
            }
        }
        if !p.is_empty() {
            p = &p[1..];
        }
        if s.is_empty() {
            while p.first() == Some(&b'*') {
                p = &p[1..];
            }
            break;
        }
    }
    p.is_empty() && s.is_empty()
}

pub(crate) fn cmd_keys(conn: &Connection, pattern: Option<Arg<'_>>) -> CmdResult {
    let pattern = pattern.map_or(&b"*"[..], Arg::as_bytes);
    // Redis special-cases a lone `*`, which is why it matches the empty key
    // even though the matcher proper never matches an empty string against
    // a non-empty pattern.
    let all = pattern == b"*";
    let mut stmt = conn.prepare(
        "SELECT key FROM keyspace
         WHERE expires_at IS NULL OR expires_at > ?1
         ORDER BY key",
    )?;
    let mut keys = Vec::new();
    let mut rows = stmt.query(params![now_ms()])?;
    while let Some(row) = rows.next()? {
        let key = bytes_col(row, 0)?;
        if all || glob_match(pattern, &key) {
            keys.push(key);
        }
    }
    Ok(Reply::Array(keys, Empty::List))
}

/// EXISTS with several keys: how many of them exist, counting a key once
/// per mention, as Redis does.
pub(crate) fn cmd_exists_many(conn: &Connection, keys: &[Arg<'_>]) -> CmdResult {
    let now = now_ms();
    let mut count = 0;
    for &key in keys {
        count += key_type(conn, key, now)?.is_some() as i64;
    }
    Ok(Reply::Int(count))
}

pub(crate) fn cmd_type(conn: &Connection, key: Arg<'_>) -> CmdResult {
    Ok(Reply::Simple(
        key_type(conn, key, now_ms())?.unwrap_or("none"),
    ))
}

pub(crate) fn cmd_rename(conn: &Connection, key: Arg<'_>, newkey: Arg<'_>) -> CmdResult {
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

/// COPY: duplicate a key's value and expiry under another name. 0 if the
/// source is missing, or the destination exists and `replace` is false.
pub(crate) fn cmd_copy(conn: &Connection, src: Arg<'_>, dst: Arg<'_>, replace: bool) -> CmdResult {
    if src == dst {
        return Err(CmdError::new(
            "ERR source and destination objects are the same",
        ));
    }
    let now = now_ms();
    let Some(row) = lookup(conn, src)?.filter(|r| r.is_live(now)) else {
        return Ok(Reply::Int(0));
    };
    if key_type(conn, dst, now)?.is_some() && !replace {
        return Ok(Reply::Int(0));
    }
    // Whatever is there (live and being replaced, or expired) goes first.
    remove_key(conn, dst)?;
    conn.execute(
        "INSERT INTO keyspace (key, type, expires_at, len)
         SELECT ?2, type, expires_at, len FROM keyspace WHERE key = ?1",
        params![src, dst],
    )?;
    let columns = match row.kind {
        "string" => "value",
        "list" => "idx, value",
        "set" => "member",
        _ => "field, value",
    };
    let table = data_table(row.kind);
    conn.execute(
        &format!(
            "INSERT INTO {table} (key, {columns}) SELECT ?2, {columns} FROM {table} WHERE key = ?1"
        ),
        params![src, dst],
    )?;
    Ok(Reply::Int(1))
}

/// The reply every SCAN-family command gives: the cursor for the next call
/// (0 when the iteration is complete) and this call's items.
fn scan_reply(next: i64, items: Vec<Vec<u8>>) -> Reply {
    Reply::Lines(vec![
        Reply::Bulk(next.to_string().into_bytes()),
        Reply::Array(items, Empty::List),
    ])
}

/// A page must examine at least one row; Redis calls anything less a syntax
/// error.
fn check_scan_count(count: i64) -> Result<(), CmdError> {
    if count < 1 {
        return Err(CmdError::new("ERR syntax error"));
    }
    Ok(())
}

/// The cursor to hand back after a page: the last rowid seen, or 0 once a
/// page comes back short, meaning the end was reached.
fn next_cursor(last_rowid: i64, rows: i64, count: i64) -> i64 {
    if rows < count { 0 } else { last_rowid }
}

/// SCAN: iterate the keyspace a page at a time.
///
/// The cursor is the SQLite rowid of the last catalogue row examined, and a
/// page is the next `count` rows in rowid order. A key keeps its rowid for
/// as long as it exists (writes update its row in place), so a key present
/// for the whole iteration is returned exactly once; keys created or deleted
/// meanwhile may or may not appear. That is the guarantee Redis gives. As
/// in Redis, `count` bounds the rows examined rather than the rows returned:
/// MATCH and TYPE filter within the page, so a page may come back empty with
/// a non-zero cursor.
pub(crate) fn cmd_scan(
    conn: &Connection,
    cursor: i64,
    pattern: Option<Arg<'_>>,
    count: i64,
    kind: Option<Arg<'_>>,
) -> CmdResult {
    check_scan_count(count)?;
    let now = now_ms();
    let mut stmt = conn.prepare(
        "SELECT rowid, key, type, expires_at FROM keyspace
         WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
    )?;
    let mut rows = stmt.query(params![cursor, count])?;
    let (mut keys, mut last, mut seen) = (Vec::new(), 0, 0);
    while let Some(row) = rows.next()? {
        last = row.get(0)?;
        seen += 1;
        let expires_at: Option<i64> = row.get(3)?;
        if expires_at.is_some_and(|t| t <= now) {
            continue;
        }
        if let Some(kind) = kind
            && row.get_ref(2)?.as_bytes().ok() != Some(kind.as_bytes())
        {
            continue;
        }
        let key = bytes_col(row, 1)?;
        if pattern.is_none_or(|p| p.as_bytes() == b"*" || glob_match(p.as_bytes(), &key)) {
            keys.push(key);
        }
    }
    Ok(scan_reply(next_cursor(last, seen, count), keys))
}

/// HSCAN / SSCAN: iterate one hash's fields (with their values unless
/// `values` is false) or one set's members. The cursor is a rowid in the
/// payload table, as for SCAN. Each call selects the key's remaining rows in
/// rowid order, which is linear in the size of the collection: fine for the
/// collections this is normally used on, slow for a full iteration of a
/// very large one.
pub(crate) fn cmd_scan_within(
    conn: &Connection,
    kind: &'static str,
    key: Arg<'_>,
    cursor: i64,
    pattern: Option<Arg<'_>>,
    count: i64,
    values: bool,
) -> CmdResult {
    check_scan_count(count)?;
    if !check_type(conn, key, kind, now_ms())? {
        return Ok(scan_reply(0, vec![]));
    }
    let hash = kind == "hash";
    let sql = if hash {
        "SELECT rowid, field, value FROM hash_fields
         WHERE key = ?1 AND rowid > ?2 ORDER BY rowid LIMIT ?3"
    } else {
        "SELECT rowid, member FROM set_members
         WHERE key = ?1 AND rowid > ?2 ORDER BY rowid LIMIT ?3"
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params![key, cursor, count])?;
    let (mut items, mut last, mut seen) = (Vec::new(), 0, 0);
    while let Some(row) = rows.next()? {
        last = row.get(0)?;
        seen += 1;
        let name = bytes_col(row, 1)?;
        if pattern.is_some_and(|p| p.as_bytes() != b"*" && !glob_match(p.as_bytes(), &name)) {
            continue;
        }
        items.push(name);
        if hash && values {
            items.push(bytes_col(row, 2)?);
        }
    }
    Ok(scan_reply(next_cursor(last, seen, count), items))
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    fn m(pattern: &str, s: &str) -> bool {
        glob_match(pattern.as_bytes(), s.as_bytes())
    }

    #[test]
    fn well_formed_patterns() {
        assert!(m("user:*", "user:1"));
        assert!(!m("user:*", "User:1"));
        assert!(m("k[12]", "k1"));
        assert!(!m("k[^12]", "k1"));
        assert!(m("k[a-z]", "kq"));
        assert!(m("a?b", "axb"));
        assert!(m(r"a\*b", "a*b"));
        assert!(!m(r"a\*b", "axb"));
        assert!(m("*a*b*", "xxaxxbxx"));
    }

    // Each expectation below was checked against a live Redis server.
    #[test]
    fn malformed_and_escaped_classes_match_redis() {
        assert!(m("[a", "a"));
        assert!(m("[a-", "a"));
        assert!(m("[^", "a"));
        assert!(!m("a[", "a"));
        assert!(!m("[]a]", "a"));
        assert!(m("[c-a]", "b"));
        assert!(m(r"[a\-c]*", "a-c"));
        assert!(!m(r"[a\-c]*", "b"));
        assert!(m(r"[\]a]*", "a]"));
        assert!(m(r"a\", r"a\"));
        assert!(m(r"a\\", r"a\"));
    }

    #[test]
    fn empty_strings() {
        assert!(m("", ""));
        assert!(!m("**", ""));
        assert!(!m("?*", ""));
        assert!(!m("", "a"));
    }

    // Checked against a live Redis: the range is a..0xc3 as signed bytes,
    // i.e. -61..=97, followed by the literal 0xa9.
    #[test]
    fn high_byte_ranges_compare_signed_like_redis() {
        assert!(!m("[a-é]*", "b"));
        assert!(m("[a-é]*", "é"));
        assert!(m("[a-é]*", "a"));
        assert!(m("[a-é]*", "A"));
    }

    #[test]
    fn nesting_is_capped_like_redis() {
        // 1000 stars nest exactly to the cap and still match; two more do not.
        assert!(m(&"*a".repeat(1000), &"a".repeat(1000)));
        assert!(!m(&"*a".repeat(1002), &"a".repeat(1002)));
        // Far beyond the cap: must return, not overflow the stack.
        assert!(!m(&"*a".repeat(200_000), &"a".repeat(200_000)));
    }

    #[test]
    fn many_stars_do_not_blow_up() {
        let pattern = "a*".repeat(40) + "b";
        assert!(!m(&pattern, &"a".repeat(200)));
    }
}
