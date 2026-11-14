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

pub(crate) fn cmd_keys(conn: &Connection, pattern: Option<&str>) -> CmdResult {
    let pattern = pattern.unwrap_or("*").as_bytes();
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
