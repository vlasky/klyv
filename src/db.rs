//! Schema, connection setup and migration, plus the keyspace helpers every
//! command shares.
//!
//! The `keyspace` catalogue has one row per key holding its type, its
//! (millisecond) expiry and, for a list, set or hash, its element count. It
//! is the single source of truth for whether a key exists, what type it is,
//! when it expires and how long it is; the per-type data tables only hold
//! payload. (The catalogue arrived in schema v2; v3 added the count, which
//! makes LLEN/SCARD/HLEN and the length LPUSH/RPUSH return O(1) instead of a
//! COUNT over the key's rows.) Invariants every command must keep:
//!
//! - a key has data rows in exactly one data table, the one for its
//!   catalogue type (`data_table`);
//! - a key has a catalogue row iff it has data rows (`claim` when creating,
//!   `shrink` after removing elements);
//! - `len` equals the number of payload rows for a list, set or hash and is
//!   NULL for a string: every insert goes through `grow`, every delete
//!   through `shrink`, with the number of rows actually changed;
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

/// Current on-disk schema version, stored in `PRAGMA user_version`. Older
/// files are migrated in place on first open: v1 (user_version 0, no
/// catalogue, a separate `expiry` table in whole seconds) and v2 (catalogue
/// without the `len` column).
pub(crate) const SCHEMA_VERSION: i64 = 3;

const CREATE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS keyspace (
        key TEXT PRIMARY KEY,
        type TEXT NOT NULL,
        expires_at INTEGER,
        len INTEGER
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

/// Drops catalogue rows whose declared payload is gone. Runs *before*
/// repopulation so that a key whose type an old binary changed (e.g. SET over
/// a list: list rows deleted, a strings row written) is re-catalogued from
/// the payload that actually exists rather than having that payload deleted
/// as a wrong-table straggler.
const PRUNE_EMPTY_CATALOGUE: &str = "
    DELETE FROM keyspace WHERE
        (type = 'string' AND key NOT IN (SELECT key FROM strings)) OR
        (type = 'list'   AND key NOT IN (SELECT key FROM list_items)) OR
        (type = 'set'    AND key NOT IN (SELECT key FROM set_members)) OR
        (type = 'hash'   AND key NOT IN (SELECT key FROM hash_fields));
";

/// After population every catalogue row has payload of its own type; any
/// payload a key has in *other* tables is a straggler and goes.
const RECONCILE: &str = "
    DELETE FROM strings     WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'string');
    DELETE FROM list_items  WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'list');
    DELETE FROM set_members WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'set');
    DELETE FROM hash_fields WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'hash');
";

/// Sets every key's `len` from its actual payload rows (NULL for strings).
/// Runs after the catalogue has been populated and reconciled, for any file
/// that is being migrated or repaired, so the counts start out true whatever
/// wrote the file before.
const RECOUNT: &str = "
    UPDATE keyspace SET len = CASE type
        WHEN 'list' THEN (SELECT COUNT(*) FROM list_items  WHERE key = keyspace.key)
        WHEN 'set'  THEN (SELECT COUNT(*) FROM set_members WHERE key = keyspace.key)
        WHEN 'hash' THEN (SELECT COUNT(*) FROM hash_fields WHERE key = keyspace.key)
        ELSE NULL
    END;
";

/// See the module docs: makes pre-v2 binaries fail to open the file.
const DOWNGRADE_GUARD: &str = "
    CREATE VIEW IF NOT EXISTS expiry AS
        SELECT key, expires_at / 1000 AS expires_at FROM keyspace
        WHERE expires_at IS NOT NULL;
";

pub(crate) fn open_db(path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    let mut conn = Connection::open(path)?;
    // busy_timeout first, so it also covers the journal-mode switch.
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;
    // Fast path: a file that is already at the current schema, in WAL mode
    // and carrying the downgrade guard needs no setup, so opening it takes
    // no write lock. That keeps reads from queueing behind a writer and lets
    // a database on read-only media be read at all.
    if is_current(&conn)? {
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        return Ok(conn);
    }
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;",
    )?;
    // Take the write lock before deciding (and re-read the version under
    // it), so two processes opening a v1 database at once cannot both try to
    // migrate it.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "schema version {version} is newer than this klyv supports ({SCHEMA_VERSION})"
        )
        .into());
    }
    tx.execute_batch(CREATE_SCHEMA)?;
    // A v2 file already has a catalogue, created without `len`.
    let has_len = tx
        .prepare("SELECT 1 FROM pragma_table_info('keyspace') WHERE name = 'len'")?
        .exists([])?;
    if !has_len {
        tx.execute_batch("ALTER TABLE keyspace ADD COLUMN len INTEGER;")?;
    }
    let has_v1_expiry = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'expiry'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if version < SCHEMA_VERSION || has_v1_expiry {
        tx.execute_batch(PRUNE_EMPTY_CATALOGUE)?;
        tx.execute_batch(POPULATE_CATALOGUE)?;
        if has_v1_expiry {
            tx.execute_batch(FOLD_V1_EXPIRY)?;
        }
        tx.execute_batch(RECONCILE)?;
        tx.execute_batch(RECOUNT)?;
    }
    // Idempotent, and only possible once any v1 table of that name is gone.
    tx.execute_batch(DOWNGRADE_GUARD)?;
    if version != SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    Ok(conn)
}

/// True if the file needs no schema work: current `user_version`, WAL
/// journal, and `expiry` present as the downgrade-guard view rather than as
/// a table (a table means a pre-v2 binary wrote to the file and it needs the
/// repair pass). Read-only; takes no write lock.
fn is_current(conn: &Connection) -> rusqlite::Result<bool> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Ok(false);
    }
    let journal: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Ok(false);
    }
    let guard: Option<String> = conn
        .query_row(
            "SELECT type FROM sqlite_master WHERE name = 'expiry'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(guard.as_deref() == Some("view"))
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
    /// Element count of a list, set or hash; 0 for a string.
    pub(crate) len: i64,
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

/// Maps a catalogue `type` value to its static name. An unrecognised value
/// (a corrupted or hand-edited file) is a database error, not a panic.
fn static_kind(kind: &str) -> rusqlite::Result<&'static str> {
    Ok(match kind {
        "string" => "string",
        "list" => "list",
        "set" => "set",
        "hash" => "hash",
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown key type in catalogue: {other:?}").into(),
            ));
        }
    })
}

/// The catalogue row for a key, expired or not.
pub(crate) fn lookup(conn: &Connection, key: Arg<'_>) -> rusqlite::Result<Option<KeyRow>> {
    conn.query_row(
        "SELECT type, expires_at, len FROM keyspace WHERE key = ?1",
        params![key],
        |row| {
            let kind: String = row.get(0)?;
            Ok(KeyRow {
                kind: static_kind(&kind)?,
                expires_at: row.get(1)?,
                len: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            })
        },
    )
    .optional()
}

/// The key's type if it exists and has not expired.
pub(crate) fn key_type(
    conn: &Connection,
    key: Arg<'_>,
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
    key: Arg<'_>,
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

/// The element count of a live list, set or hash of type `want`: `None` if
/// the key is missing or expired, WRONGTYPE if it holds another type. One
/// catalogue read answers existence, type and length together.
pub(crate) fn live_len(
    conn: &Connection,
    key: Arg<'_>,
    want: &str,
    now: i64,
) -> Result<Option<i64>, CmdError> {
    match lookup(conn, key)?.filter(|r| r.is_live(now)) {
        Some(r) if r.kind == want => Ok(Some(r.len)),
        Some(_) => Err(CmdError::new(
            "WRONGTYPE Operation against a key holding the wrong kind of value",
        )),
        None => Ok(None),
    }
}

pub(crate) fn ensure_type(
    conn: &Connection,
    key: Arg<'_>,
    want: &str,
    now: i64,
) -> Result<(), CmdError> {
    check_type(conn, key, want, now).map(|_| ())
}

/// Deletes a key's payload rows (not its catalogue row).
pub(crate) fn clear_data(conn: &Connection, key: Arg<'_>, kind: &str) -> rusqlite::Result<()> {
    conn.execute(
        &format!("DELETE FROM {} WHERE key = ?1", data_table(kind)),
        params![key],
    )?;
    Ok(())
}

/// Physically removes a key: payload rows and catalogue row. Returns the
/// catalogue row that was removed, if any (expired or not).
pub(crate) fn remove_key(conn: &Connection, key: Arg<'_>) -> rusqlite::Result<Option<KeyRow>> {
    let row = lookup(conn, key)?;
    if let Some(r) = &row {
        clear_data(conn, key, r.kind)?;
        conn.execute("DELETE FROM keyspace WHERE key = ?1", params![key])?;
    }
    Ok(row)
}

/// Writes call this before touching a key so a lazily expired key is treated
/// as absent and its stale rows reclaimed.
pub(crate) fn drop_if_expired(conn: &Connection, key: Arg<'_>, now: i64) -> rusqlite::Result<()> {
    if let Some(r) = lookup(conn, key)?
        && !r.is_live(now)
    {
        remove_key(conn, key)?;
    }
    Ok(())
}

/// Registers a string key with no expiry if it has no catalogue row yet.
/// Callers have already type-checked and dropped any expired row, so an
/// existing row is necessarily a string too and is left untouched. (Lists,
/// sets and hashes register through `grow`, which also counts.)
pub(crate) fn claim_string(conn: &Connection, key: Arg<'_>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO keyspace (key, type, expires_at, len) VALUES (?1, 'string', NULL, NULL)",
        params![key],
    )?;
    Ok(())
}

/// Records that `added` elements were inserted into a list, set or hash,
/// registering the key (with no expiry) if this created it. Returns the new
/// length. Callers have already type-checked and dropped any expired row, so
/// an existing row is necessarily of the same type.
///
/// `added` must be the number of rows actually inserted. It may be 0 for an
/// existing key (SADD of members already present), never for a new one: a
/// catalogue row with no payload would break the invariant.
pub(crate) fn grow(
    conn: &Connection,
    key: Arg<'_>,
    kind: &str,
    added: i64,
) -> rusqlite::Result<i64> {
    conn.query_row(
        "INSERT INTO keyspace (key, type, expires_at, len) VALUES (?1, ?2, NULL, ?3)
         ON CONFLICT(key) DO UPDATE SET len = len + excluded.len
         RETURNING len",
        params![key, kind, added],
        |row| row.get(0),
    )
}

/// Records that `removed` elements were deleted from a list, set or hash.
/// Removing the last element deletes the key: the catalogue row (and with it
/// any TTL) goes too, or a later SET of the same name would inherit a stale
/// expiry. `removed` must be the number of rows actually deleted.
pub(crate) fn shrink(conn: &Connection, key: Arg<'_>, removed: i64) -> rusqlite::Result<()> {
    if removed == 0 {
        return Ok(());
    }
    conn.execute(
        "UPDATE keyspace SET len = len - ?2 WHERE key = ?1",
        params![key, removed],
    )?;
    conn.execute(
        "DELETE FROM keyspace WHERE key = ?1 AND len <= 0",
        params![key],
    )?;
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

/// A key, field or value argument: arbitrary bytes, as a RESP client may send
/// them. The CLI only ever produces UTF-8.
///
/// It is a newtype rather than a bare `&[u8]` because of how it must bind:
/// valid UTF-8 as TEXT, anything else as BLOB (the same rule as
/// `bind_value`). rusqlite binds a bare byte slice as BLOB always, and SQLite
/// never considers a BLOB equal to a TEXT of the same bytes, so a slice bound
/// directly would silently miss every key written as text. Passing `Arg`
/// everywhere makes that mistake unrepresentable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct Arg<'a>(pub(crate) &'a [u8]);

impl<'a> Arg<'a> {
    pub(crate) fn as_bytes(self) -> &'a [u8] {
        self.0
    }
}

impl<'a> From<&'a str> for Arg<'a> {
    fn from(s: &'a str) -> Self {
        Arg(s.as_bytes())
    }
}

impl rusqlite::ToSql for Arg<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(ToSqlOutput::Borrowed(
            if std::str::from_utf8(self.0).is_ok() {
                ValueRef::Text(self.0)
            } else {
                ValueRef::Blob(self.0)
            },
        ))
    }
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

/// Parses bytes as an i64 with Redis's strict syntax (`string2ll`): an
/// optional minus, no plus sign, no leading zeros, no surrounding whitespace,
/// and within range. Used both for stored values (INCR on a value that
/// APPEND turned into `042` must fail, as in Redis) and for integer
/// arguments arriving over the wire. Rust's own parser is too lenient: it
/// accepts `+5` and `05`.
pub(crate) fn parse_int(bytes: &[u8]) -> Option<i64> {
    let digits = bytes.strip_prefix(b"-").unwrap_or(bytes);
    let well_formed = match digits {
        [] => false,
        [b'0'] => bytes.len() == 1,
        [first, rest @ ..] => (b'1'..=b'9').contains(first) && rest.iter().all(u8::is_ascii_digit),
    };
    if !well_formed {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}
