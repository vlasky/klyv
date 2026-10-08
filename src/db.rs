//! Schema, connection setup and migration, plus the keyspace helpers every
//! command shares.
//!
//! Schema v2 adds a `keyspace` catalogue: one row per key holding its type
//! and (millisecond) expiry. It is the single source of truth for whether a
//! key exists, what type it is, and when it expires; the per-type data
//! tables only hold payload. Invariants every command must keep:
//!
//! - a key has data rows in exactly one data table, the one for its
//!   catalogue type (`data_table`);
//! - a key has a catalogue row iff it has data rows (`claim` when creating,
//!   `release_if_empty` after removing elements);
//! - expiry lives only in the catalogue (`expires_at`, Unix ms, NULL = none).
//!
//! Downgrade protection: a VIEW named `expiry` shadows the v1 table of that
//! name. klyv <= 0.2.0 runs `CREATE INDEX ... ON expiry` on open, which fails
//! on a view ("views may not be indexed"), so an old binary refuses a v2 file
//! outright instead of recreating an empty expiry table and running
//! split-brain against the catalogue.

use crate::reply::CmdError;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current on-disk schema version, stored in `PRAGMA user_version`. A v1
/// database (user_version 0, separate `expiry` table in whole seconds) is
/// migrated in place on first open.
pub(crate) const SCHEMA_VERSION: i64 = 2;

const CREATE_V2: &str = "
    CREATE TABLE IF NOT EXISTS keyspace (
        key TEXT PRIMARY KEY,
        type TEXT NOT NULL,
        expires_at INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_keyspace_expires
        ON keyspace(expires_at) WHERE expires_at IS NOT NULL;

    CREATE TABLE IF NOT EXISTS strings (
        key TEXT PRIMARY KEY,
        value BLOB NOT NULL
    );

    CREATE TABLE IF NOT EXISTS list_items (
        key TEXT NOT NULL,
        idx REAL NOT NULL,
        value BLOB NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_list_key_idx ON list_items(key, idx);

    CREATE TABLE IF NOT EXISTS set_members (
        key TEXT NOT NULL,
        member BLOB NOT NULL,
        UNIQUE(key, member)
    );

    CREATE TABLE IF NOT EXISTS hash_fields (
        key TEXT NOT NULL,
        field TEXT NOT NULL,
        value BLOB NOT NULL,
        UNIQUE(key, field)
    );
";

/// Catalogue rows for any payload rows that lack one. Run for a version-0
/// (v1) file and whenever a v1-style `expiry` table is present — the latter
/// means a pre-v2 binary wrote to this file after migration (it recreates the
/// table and writes payload around the catalogue), and this folds its writes
/// back in. `INSERT OR IGNORE` in this order gives a key that illegally
/// exists in several tables the type the v1 lookup order would have reported.
const POPULATE_CATALOGUE: &str = "
    INSERT OR IGNORE INTO keyspace (key, type) SELECT key, 'string' FROM strings;
    INSERT OR IGNORE INTO keyspace (key, type) SELECT DISTINCT key, 'list' FROM list_items;
    INSERT OR IGNORE INTO keyspace (key, type) SELECT DISTINCT key, 'set' FROM set_members;
    INSERT OR IGNORE INTO keyspace (key, type) SELECT DISTINCT key, 'hash' FROM hash_fields;
";

/// Folds a v1 `expiry` table (whole seconds) into the catalogue as
/// milliseconds, clamped so the multiplication cannot overflow into a REAL,
/// and only for keys the table mentions, so other keys keep their expiry.
const FOLD_V1_EXPIRY: &str = "
    UPDATE keyspace SET expires_at = (
        SELECT CASE
            WHEN e.expires_at >  9223372036854775 THEN  9223372036854775807
            WHEN e.expires_at < -9223372036854775 THEN -9223372036854775808
            ELSE e.expires_at * 1000
        END FROM expiry e WHERE e.key = keyspace.key)
    WHERE key IN (SELECT key FROM expiry);
    DROP TABLE expiry;
";

/// Restores the catalogue invariants after population: payload in the wrong
/// table for its type goes, and catalogue rows with no payload go.
const RECONCILE: &str = "
    DELETE FROM strings     WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'string');
    DELETE FROM list_items  WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'list');
    DELETE FROM set_members WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'set');
    DELETE FROM hash_fields WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'hash');
    DELETE FROM keyspace WHERE
        (type = 'string' AND key NOT IN (SELECT key FROM strings)) OR
        (type = 'list'   AND key NOT IN (SELECT key FROM list_items)) OR
        (type = 'set'    AND key NOT IN (SELECT key FROM set_members)) OR
        (type = 'hash'   AND key NOT IN (SELECT key FROM hash_fields));
";

/// See the module docs: makes pre-v2 binaries fail to open the file.
const DOWNGRADE_GUARD: &str = "
    CREATE VIEW IF NOT EXISTS expiry AS
        SELECT key, expires_at / 1000 AS expires_at FROM keyspace
        WHERE expires_at IS NOT NULL;
";

pub(crate) fn open_db(path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;",
    )?;
    // Take the write lock before deciding, so two processes opening a v1
    // database at once cannot both try to migrate it.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "schema version {version} is newer than this klyv supports ({SCHEMA_VERSION})"
        )
        .into());
    }
    tx.execute_batch(CREATE_V2)?;
    let has_v1_expiry = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'expiry'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if version < SCHEMA_VERSION || has_v1_expiry {
        tx.execute_batch(POPULATE_CATALOGUE)?;
        if has_v1_expiry {
            tx.execute_batch(FOLD_V1_EXPIRY)?;
        }
        tx.execute_batch(RECONCILE)?;
    }
    // Idempotent, and only possible once any v1 table of that name is gone.
    tx.execute_batch(DOWNGRADE_GUARD)?;
    if version != SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(conn)
}

/// Current Unix time in milliseconds. Commands take it once so every
/// statement in one command agrees on "now".
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A catalogue row.
pub(crate) struct KeyRow {
    pub(crate) kind: &'static str,
    pub(crate) expires_at: Option<i64>,
}

impl KeyRow {
    pub(crate) fn is_live(&self, now: i64) -> bool {
        self.expires_at.is_none_or(|t| t > now)
    }
}

/// The data table holding payload rows for a catalogue type.
pub(crate) fn data_table(kind: &str) -> &'static str {
    match kind {
        "string" => "strings",
        "list" => "list_items",
        "set" => "set_members",
        "hash" => "hash_fields",
        other => unreachable!("unknown key type in catalogue: {other}"),
    }
}

fn static_kind(kind: &str) -> &'static str {
    match kind {
        "string" => "string",
        "list" => "list",
        "set" => "set",
        "hash" => "hash",
        other => unreachable!("unknown key type in catalogue: {other}"),
    }
}

/// The catalogue row for a key, expired or not.
pub(crate) fn lookup(conn: &Connection, key: &str) -> rusqlite::Result<Option<KeyRow>> {
    conn.query_row(
        "SELECT type, expires_at FROM keyspace WHERE key = ?1",
        params![key],
        |row| {
            let kind: String = row.get(0)?;
            Ok(KeyRow {
                kind: static_kind(&kind),
                expires_at: row.get(1)?,
            })
        },
    )
    .optional()
}

/// The key's type if it exists and has not expired.
pub(crate) fn key_type(
    conn: &Connection,
    key: &str,
    now: i64,
) -> rusqlite::Result<Option<&'static str>> {
    Ok(lookup(conn, key)?
        .filter(|r| r.is_live(now))
        .map(|r| r.kind))
}

/// Type check shared by every type-specific command: Ok(true) if the key is
/// live and of type `want`, Ok(false) if it is missing or expired, and the
/// Redis WRONGTYPE error if it holds another type.
pub(crate) fn check_type(
    conn: &Connection,
    key: &str,
    want: &str,
    now: i64,
) -> Result<bool, CmdError> {
    match key_type(conn, key, now)? {
        Some(t) if t == want => Ok(true),
        Some(_) => Err(CmdError::new(
            "WRONGTYPE Operation against a key holding the wrong kind of value",
        )),
        None => Ok(false),
    }
}

pub(crate) fn ensure_type(
    conn: &Connection,
    key: &str,
    want: &str,
    now: i64,
) -> Result<(), CmdError> {
    check_type(conn, key, want, now).map(|_| ())
}

/// Deletes a key's payload rows (not its catalogue row).
pub(crate) fn clear_data(conn: &Connection, key: &str, kind: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!("DELETE FROM {} WHERE key = ?1", data_table(kind)),
        params![key],
    )?;
    Ok(())
}

/// Physically removes a key: payload rows and catalogue row. Returns the
/// catalogue row that was removed, if any (expired or not).
pub(crate) fn remove_key(conn: &Connection, key: &str) -> rusqlite::Result<Option<KeyRow>> {
    let row = lookup(conn, key)?;
    if let Some(r) = &row {
        clear_data(conn, key, r.kind)?;
        conn.execute("DELETE FROM keyspace WHERE key = ?1", params![key])?;
    }
    Ok(row)
}

/// Writes call this before touching a key so a lazily expired key is treated
/// as absent and its stale rows reclaimed.
pub(crate) fn drop_if_expired(conn: &Connection, key: &str, now: i64) -> rusqlite::Result<()> {
    if let Some(r) = lookup(conn, key)?
        && !r.is_live(now)
    {
        remove_key(conn, key)?;
    }
    Ok(())
}

/// Registers a key as `kind` with no expiry if it has no catalogue row yet.
/// Callers have already type-checked and dropped any expired row, so an
/// existing row is necessarily the same type and is left untouched.
pub(crate) fn claim(conn: &Connection, key: &str, kind: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO keyspace (key, type, expires_at) VALUES (?1, ?2, NULL)",
        params![key, kind],
    )?;
    Ok(())
}

/// Removing the last element of a list/set/hash deletes the key: the
/// catalogue row (and with it any TTL) must go too, or a later SET of the
/// same name would inherit a stale expiry.
pub(crate) fn release_if_empty(conn: &Connection, key: &str, kind: &str) -> rusqlite::Result<()> {
    let has_rows = conn
        .query_row(
            &format!("SELECT 1 FROM {} WHERE key = ?1 LIMIT 1", data_table(kind)),
            params![key],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_rows {
        conn.execute("DELETE FROM keyspace WHERE key = ?1", params![key])?;
    }
    Ok(())
}

/// Reads a value column as bytes whether it was stored as TEXT (valid UTF-8,
/// the CLI's case) or BLOB (binary payloads from a future RESP client).
pub(crate) fn bytes_col(row: &Row, idx: usize) -> rusqlite::Result<Vec<u8>> {
    let v = row.get_ref(idx)?;
    v.as_bytes()
        .map(<[u8]>::to_vec)
        .map_err(|_| rusqlite::Error::InvalidColumnType(idx, "value".into(), v.data_type()))
}

/// Storage class for a value: valid UTF-8 is stored as TEXT, anything else
/// as BLOB. Deterministic per byte string, so SQL equality comparisons on
/// values (LREM, LPOS, set membership) stay consistent.
pub(crate) fn bind_value(bytes: Vec<u8>) -> Value {
    match String::from_utf8(bytes) {
        Ok(s) => Value::Text(s),
        Err(e) => Value::Blob(e.into_bytes()),
    }
}

/// Parses a stored value as an i64 for INCR-style commands.
pub(crate) fn parse_int(bytes: &[u8]) -> Option<i64> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}
