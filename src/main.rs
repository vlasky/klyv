use clap::{Parser, Subcommand};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "klyv", version, about = "Redis-compatible embedded KV store backed by SQLite")]
struct Cli {
    #[arg(short, long, env = "KLYV_DB")]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Set a string value")]
    Set { key: String, value: String },
    #[command(about = "Get a string value (prints '(nil)' if not found)")]
    Get { key: String },
    #[command(about = "Delete one or more keys (any type)")]
    Del { keys: Vec<String> },
    #[command(about = "Increment integer value by 1 (creates key at 0 if missing)")]
    Incr { key: String },
    #[command(about = "Decrement integer value by 1 (creates key at 0 if missing)")]
    Decr { key: String },
    #[command(about = "Increment integer value by amount", allow_hyphen_values = true)]
    IncrBy { key: String, amount: i64 },
    #[command(about = "Decrement integer value by amount", allow_hyphen_values = true)]
    DecrBy { key: String, amount: i64 },
    #[command(about = "Append to string value (creates key if missing), returns new length")]
    Append { key: String, value: String },
    #[command(about = "Get string length (0 if key missing)")]
    Strlen { key: String },
    #[command(about = "Set multiple key-value pairs atomically")]
    MSet {
        #[arg(help = "Alternating key value pairs: key1 val1 key2 val2 ...")]
        pairs: Vec<String>,
    },
    #[command(about = "Get multiple values (prints one per line, '(nil)' for missing)")]
    MGet { keys: Vec<String> },

    #[command(about = "Push values to head of list (leftmost)")]
    LPush { key: String, values: Vec<String> },
    #[command(about = "Push values to tail of list (rightmost)")]
    RPush { key: String, values: Vec<String> },
    #[command(about = "Remove and return element from head of list")]
    LPop { key: String },
    #[command(about = "Remove and return element from tail of list")]
    RPop { key: String },
    #[command(about = "Return elements from index START to STOP (inclusive, 0-based, negatives count from end)", allow_hyphen_values = true)]
    LRange { key: String, start: i64, stop: i64 },
    #[command(about = "Get list length")]
    LLen { key: String },
    #[command(about = "Remove COUNT occurrences of value (0=all, +N=from head, -N=from tail)", allow_hyphen_values = true)]
    LRem {
        key: String,
        #[arg(help = "0=remove all, +N=first N from head, -N=first N from tail")]
        count: i64,
        value: String,
    },
    #[command(about = "Find first occurrence of value in list (returns 0-based index or '(nil)')")]
    LPos { key: String, value: String },

    #[command(about = "Add members to set (ignores duplicates)")]
    SAdd { key: String, members: Vec<String> },
    #[command(about = "Remove members from set")]
    SRem { key: String, members: Vec<String> },
    #[command(about = "List all members of set")]
    SMembers { key: String },
    #[command(about = "Test if member exists in set (returns 1 or 0)")]
    SIsMember { key: String, member: String },
    #[command(about = "Get number of members in set")]
    SCard { key: String },
    #[command(about = "Return union of multiple sets")]
    SUnion { keys: Vec<String> },
    #[command(about = "Return intersection of multiple sets")]
    SInter { keys: Vec<String> },
    #[command(about = "Return members in first set not in any other sets")]
    SDiff { keys: Vec<String> },

    #[command(about = "Set field-value pairs in a hash")]
    HSet {
        key: String,
        #[arg(help = "Alternating field value pairs: field1 val1 field2 val2 ...")]
        pairs: Vec<String>,
    },
    #[command(about = "Get a field's value from a hash")]
    HGet { key: String, field: String },
    #[command(about = "Delete fields from a hash")]
    HDel { key: String, fields: Vec<String> },
    #[command(about = "Get all field-value pairs (alternating lines: field, value)")]
    HGetAll { key: String },
    #[command(about = "List all field names in a hash")]
    HKeys { key: String },
    #[command(about = "List all values in a hash")]
    HVals { key: String },
    #[command(about = "Get number of fields in a hash")]
    HLen { key: String },

    #[command(about = "List keys matching glob pattern (* and ? supported, omit for all)")]
    Keys { pattern: Option<String> },
    #[command(about = "Test if key exists (any type, returns 1 or 0)")]
    Exists { key: String },
    #[command(about = "Get key's type: string, list, set, hash, or none")]
    Type { key: String },
    #[command(about = "Rename a key (overwrites target if it exists)")]
    Rename { key: String, newkey: String },

    // TTL commands
    #[command(about = "Set key expiry in seconds from now", allow_hyphen_values = true)]
    Expire { key: String, seconds: i64 },
    #[command(about = "Set key expiry in milliseconds from now", allow_hyphen_values = true)]
    PExpire { key: String, milliseconds: i64 },
    #[command(about = "Set key expiry at Unix timestamp (seconds)", allow_hyphen_values = true)]
    ExpireAt { key: String, timestamp: i64 },
    #[command(about = "Get remaining TTL in seconds (-1=no expiry, -2=key missing)")]
    Ttl { key: String },
    #[command(about = "Remove expiry from key")]
    Persist { key: String },
    #[command(about = "Delete all expired keys and report count")]
    Purge,

    #[command(about = "Count total number of keys across all types")]
    DbSize,
    #[command(about = "Delete all data from all tables")]
    FlushAll,
}

// --- Reply: what a command computes, decoupled from how it is rendered ---

/// Which placeholder an empty array renders as in human output.
#[derive(Clone, Copy)]
enum Empty {
    List,
    Set,
    Hash,
}

/// Typed command result. Commands compute a Reply; renderers turn it into
/// output (today: the human redis-cli format; future: RESP, JSON, raw).
enum Reply {
    /// Status line printed bare ("OK", type names).
    Simple(&'static str),
    /// Integer, printed as "(integer) N".
    Int(i64),
    /// A value, printed bare.
    Bulk(String),
    /// Missing value, printed as "(nil)".
    Nil,
    /// Items printed as a numbered, quoted list; the Empty kind picks the
    /// "(empty list)"/"(empty set)"/"(empty hash)" placeholder.
    Array(Vec<String>, Empty),
    /// One reply per line without numbering (MGET).
    Lines(Vec<Reply>),
}

/// Command failure: the message is printed to stderr and the process exits 1.
struct CmdError(String);

impl CmdError {
    fn new(msg: impl Into<String>) -> Self {
        CmdError(msg.into())
    }
}

impl From<rusqlite::Error> for CmdError {
    fn from(e: rusqlite::Error) -> Self {
        CmdError(format!("ERR database error: {e}"))
    }
}

type CmdResult = Result<Reply, CmdError>;

fn render_human(reply: &Reply, out: &mut String) {
    match reply {
        Reply::Simple(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Reply::Int(n) => {
            out.push_str(&format!("(integer) {n}\n"));
        }
        Reply::Bulk(v) => {
            out.push_str(v);
            out.push('\n');
        }
        Reply::Nil => out.push_str("(nil)\n"),
        Reply::Array(items, empty) => {
            if items.is_empty() {
                out.push_str(match empty {
                    Empty::List => "(empty list)\n",
                    Empty::Set => "(empty set)\n",
                    Empty::Hash => "(empty hash)\n",
                });
            } else {
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&format!("{}) \"{item}\"\n", i + 1));
                }
            }
        }
        Reply::Lines(replies) => {
            for r in replies {
                render_human(r, out);
            }
        }
    }
}

fn open_db(path: &PathBuf) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        PRAGMA busy_timeout=5000;

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

        CREATE TABLE IF NOT EXISTS expiry (
            key TEXT PRIMARY KEY,
            expires_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_expiry_at ON expiry(expires_at);
    ")?;
    Ok(conn)
}

fn is_expired(conn: &Connection, key: &str) -> Result<bool, rusqlite::Error> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM expiry WHERE key = ?1 AND expires_at <= unixepoch()",
            params![key],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn exists_in(conn: &Connection, sql: &str, key: &str) -> Result<bool, rusqlite::Error> {
    Ok(conn.query_row(sql, params![key], |_| Ok(())).optional()?.is_some())
}

fn key_type(conn: &Connection, key: &str) -> Result<Option<&'static str>, rusqlite::Error> {
    if is_expired(conn, key)? {
        return Ok(None);
    }
    if exists_in(conn, "SELECT 1 FROM strings WHERE key = ?1", key)? {
        return Ok(Some("string"));
    }
    if exists_in(conn, "SELECT 1 FROM list_items WHERE key = ?1 LIMIT 1", key)? {
        return Ok(Some("list"));
    }
    if exists_in(conn, "SELECT 1 FROM set_members WHERE key = ?1 LIMIT 1", key)? {
        return Ok(Some("set"));
    }
    if exists_in(conn, "SELECT 1 FROM hash_fields WHERE key = ?1 LIMIT 1", key)? {
        return Ok(Some("hash"));
    }
    Ok(None)
}

fn ensure_type(conn: &Connection, key: &str, want: &str) -> Result<(), CmdError> {
    match key_type(conn, key)? {
        Some(t) if t != want => Err(CmdError::new(
            "WRONGTYPE Operation against a key holding the wrong kind of value",
        )),
        _ => Ok(()),
    }
}

fn drop_if_expired(conn: &Connection, key: &str) -> Result<(), rusqlite::Error> {
    if is_expired(conn, key)? {
        conn.execute("DELETE FROM strings WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    Ok(())
}

fn key_exists_in_data(conn: &Connection, key: &str) -> Result<bool, rusqlite::Error> {
    Ok(exists_in(conn, "SELECT 1 FROM strings WHERE key = ?1", key)?
        || exists_in(conn, "SELECT 1 FROM list_items WHERE key = ?1 LIMIT 1", key)?
        || exists_in(conn, "SELECT 1 FROM set_members WHERE key = ?1 LIMIT 1", key)?
        || exists_in(conn, "SELECT 1 FROM hash_fields WHERE key = ?1 LIMIT 1", key)?)
}

/// Whether a command mutates the database (BEGIN IMMEDIATE) or only reads
/// (deferred transaction, giving all its statements one consistent snapshot).
fn is_write(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Set { .. }
            | Command::Del { .. }
            | Command::Incr { .. }
            | Command::Decr { .. }
            | Command::IncrBy { .. }
            | Command::DecrBy { .. }
            | Command::Append { .. }
            | Command::MSet { .. }
            | Command::LPush { .. }
            | Command::RPush { .. }
            | Command::LPop { .. }
            | Command::RPop { .. }
            | Command::LRem { .. }
            | Command::SAdd { .. }
            | Command::SRem { .. }
            | Command::HSet { .. }
            | Command::HDel { .. }
            | Command::Rename { .. }
            | Command::Expire { .. }
            | Command::PExpire { .. }
            | Command::ExpireAt { .. }
            | Command::Persist { .. }
            | Command::Purge
            | Command::FlushAll
    )
}

/// Runs the command inside a single transaction: writes take the write lock
/// up front (BEGIN IMMEDIATE); reads get a consistent snapshot. On error the
/// transaction is dropped and rolls back, leaving the data unchanged.
fn dispatch(conn: &mut Connection, cmd: Command) -> CmdResult {
    let behavior = if is_write(&cmd) {
        TransactionBehavior::Immediate
    } else {
        TransactionBehavior::Deferred
    };
    let tx = conn.transaction_with_behavior(behavior)?;
    let reply = run(&tx, cmd)?;
    tx.commit()?;
    Ok(reply)
}

fn run(conn: &Connection, cmd: Command) -> CmdResult {
    match cmd {
        Command::Set { key, value } => cmd_set(conn, &key, &value),
        Command::Get { key } => cmd_get(conn, &key),
        Command::Del { keys } => cmd_del(conn, &keys),
        Command::Incr { key } => cmd_incrby(conn, &key, 1),
        Command::Decr { key } => cmd_incrby(conn, &key, -1),
        Command::IncrBy { key, amount } => cmd_incrby(conn, &key, amount),
        Command::DecrBy { key, amount } => {
            let neg = amount
                .checked_neg()
                .ok_or_else(|| CmdError::new("ERR increment or decrement would overflow"))?;
            cmd_incrby(conn, &key, neg)
        }
        Command::Append { key, value } => cmd_append(conn, &key, &value),
        Command::Strlen { key } => cmd_strlen(conn, &key),
        Command::MSet { pairs } => cmd_mset(conn, &pairs),
        Command::MGet { keys } => cmd_mget(conn, &keys),

        Command::LPush { key, values } => cmd_lpush(conn, &key, &values),
        Command::RPush { key, values } => cmd_rpush(conn, &key, &values),
        Command::LPop { key } => cmd_pop(conn, &key, "ASC"),
        Command::RPop { key } => cmd_pop(conn, &key, "DESC"),
        Command::LRange { key, start, stop } => cmd_lrange(conn, &key, start, stop),
        Command::LLen { key } => cmd_llen(conn, &key),
        Command::LRem { key, count, value } => cmd_lrem(conn, &key, count, &value),
        Command::LPos { key, value } => cmd_lpos(conn, &key, &value),

        Command::SAdd { key, members } => cmd_sadd(conn, &key, &members),
        Command::SRem { key, members } => cmd_srem(conn, &key, &members),
        Command::SMembers { key } => cmd_smembers(conn, &key),
        Command::SIsMember { key, member } => cmd_sismember(conn, &key, &member),
        Command::SCard { key } => cmd_scard(conn, &key),
        Command::SUnion { keys } => cmd_sunion(conn, &keys),
        Command::SInter { keys } => cmd_sinter(conn, &keys),
        Command::SDiff { keys } => cmd_sdiff(conn, &keys),

        Command::HSet { key, pairs } => cmd_hset(conn, &key, &pairs),
        Command::HGet { key, field } => cmd_hget(conn, &key, &field),
        Command::HDel { key, fields } => cmd_hdel(conn, &key, &fields),
        Command::HGetAll { key } => cmd_hgetall(conn, &key),
        Command::HKeys { key } => cmd_hkeys(conn, &key),
        Command::HVals { key } => cmd_hvals(conn, &key),
        Command::HLen { key } => cmd_hlen(conn, &key),

        Command::Keys { pattern } => cmd_keys(conn, pattern.as_deref()),
        Command::Exists { key } => cmd_exists(conn, &key),
        Command::Type { key } => cmd_type(conn, &key),
        Command::Rename { key, newkey } => cmd_rename(conn, &key, &newkey),

        Command::Expire { key, seconds } => cmd_expire(conn, &key, seconds),
        Command::PExpire { key, milliseconds } => {
            // Round up to whole seconds; non-positive TTLs expire immediately.
            let seconds = if milliseconds <= 0 {
                0
            } else {
                milliseconds.saturating_add(999) / 1000
            };
            cmd_expire(conn, &key, seconds)
        }
        Command::ExpireAt { key, timestamp } => cmd_expireat(conn, &key, timestamp),
        Command::Ttl { key } => cmd_ttl(conn, &key),
        Command::Persist { key } => cmd_persist(conn, &key),
        Command::Purge => cmd_purge(conn),

        Command::DbSize => cmd_dbsize(conn),
        Command::FlushAll => cmd_flushall(conn),
    }
}

fn main() {
    let cli = Cli::parse();
    let mut conn = match open_db(&cli.db) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("ERR failed to open database {}: {e}", cli.db.display());
            std::process::exit(1);
        }
    };
    match dispatch(&mut conn, cli.command) {
        Ok(reply) => {
            let mut out = String::new();
            render_human(&reply, &mut out);
            print!("{out}");
        }
        Err(CmdError(msg)) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    }
}

// --- String commands ---

fn get_string(conn: &Connection, key: &str) -> Result<Option<String>, rusqlite::Error> {
    conn.query_row("SELECT value FROM strings WHERE key = ?1", params![key], |row| row.get(0))
        .optional()
}

fn cmd_set(conn: &Connection, key: &str, value: &str) -> CmdResult {
    // SET overwrites any existing key, regardless of its prior type.
    conn.execute("DELETE FROM list_items WHERE key = ?1", params![key])?;
    conn.execute("DELETE FROM set_members WHERE key = ?1", params![key])?;
    conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key])?;
    // Drop a stale (already-expired) expiry so the new value isn't hidden.
    // A live TTL is intentionally preserved (see test_set_overwrites_clears_expiry_not).
    if is_expired(conn, key)? {
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(Reply::Simple("OK"))
}

fn cmd_get(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Nil);
    }
    Ok(match get_string(conn, key)? {
        Some(v) => Reply::Bulk(v),
        None => Reply::Nil,
    })
}

fn cmd_del(conn: &Connection, keys: &[String]) -> CmdResult {
    let mut count = 0i64;
    for key in keys {
        // Count keys like Redis, not rows; an expired key is already logically
        // gone so it doesn't count, but its physical rows are still reclaimed.
        if key_exists_in_data(conn, key)? && !is_expired(conn, key)? {
            count += 1;
        }
        conn.execute("DELETE FROM strings WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    Ok(Reply::Int(count))
}

fn cmd_incrby(conn: &Connection, key: &str, amount: i64) -> CmdResult {
    ensure_type(conn, key, "string")?;
    drop_if_expired(conn, key)?;
    let val: i64 = match get_string(conn, key)? {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| CmdError::new("ERR value is not an integer"))?,
        None => 0,
    };
    let new_val = val
        .checked_add(amount)
        .ok_or_else(|| CmdError::new("ERR increment or decrement would overflow"))?;
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, new_val.to_string()],
    )?;
    Ok(Reply::Int(new_val))
}

fn cmd_append(conn: &Connection, key: &str, value: &str) -> CmdResult {
    ensure_type(conn, key, "string")?;
    drop_if_expired(conn, key)?;
    let new_val = match get_string(conn, key)? {
        Some(existing) => format!("{existing}{value}"),
        None => value.to_string(),
    };
    let len = new_val.len();
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, new_val],
    )?;
    Ok(Reply::Int(len as i64))
}

fn cmd_strlen(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let len = get_string(conn, key)?.map(|s| s.len()).unwrap_or(0);
    Ok(Reply::Int(len as i64))
}

fn cmd_mset(conn: &Connection, pairs: &[String]) -> CmdResult {
    if !pairs.len().is_multiple_of(2) {
        return Err(CmdError::new("ERR wrong number of arguments for 'mset' command"));
    }
    for chunk in pairs.chunks(2) {
        // MSET overwrites each key regardless of its prior type.
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![chunk[0]])?;
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![chunk[0]])?;
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![chunk[0]])?;
        if is_expired(conn, &chunk[0])? {
            conn.execute("DELETE FROM expiry WHERE key = ?1", params![chunk[0]])?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
            params![chunk[0], chunk[1]],
        )?;
    }
    Ok(Reply::Simple("OK"))
}

fn cmd_mget(conn: &Connection, keys: &[String]) -> CmdResult {
    let mut replies = Vec::with_capacity(keys.len());
    for key in keys {
        if is_expired(conn, key)? {
            replies.push(Reply::Nil);
            continue;
        }
        replies.push(match get_string(conn, key)? {
            Some(v) => Reply::Bulk(v),
            None => Reply::Nil,
        });
    }
    Ok(Reply::Lines(replies))
}

// --- List commands ---

// Returns (element count, min idx, max idx) for a list in one query.
fn list_bounds(conn: &Connection, key: &str) -> Result<(i64, Option<f64>, Option<f64>), rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*), MIN(idx), MAX(idx) FROM list_items WHERE key = ?1",
        params![key],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
}

fn cmd_lpush(conn: &Connection, key: &str, values: &[String]) -> CmdResult {
    ensure_type(conn, key, "list")?;
    drop_if_expired(conn, key)?;
    let (count, min_idx, _) = list_bounds(conn, key)?;
    let mut idx = min_idx.map_or(0.0, |m| m - 1.0);
    for value in values {
        conn.execute(
            "INSERT INTO list_items (key, idx, value) VALUES (?1, ?2, ?3)",
            params![key, idx, value],
        )?;
        idx -= 1.0;
    }
    Ok(Reply::Int(count + values.len() as i64))
}

fn cmd_rpush(conn: &Connection, key: &str, values: &[String]) -> CmdResult {
    ensure_type(conn, key, "list")?;
    drop_if_expired(conn, key)?;
    let (count, _, max_idx) = list_bounds(conn, key)?;
    let mut idx = max_idx.map_or(0.0, |m| m + 1.0);
    for value in values {
        conn.execute(
            "INSERT INTO list_items (key, idx, value) VALUES (?1, ?2, ?3)",
            params![key, idx, value],
        )?;
        idx += 1.0;
    }
    Ok(Reply::Int(count + values.len() as i64))
}

fn cmd_pop(conn: &Connection, key: &str, order: &str) -> CmdResult {
    drop_if_expired(conn, key)?;
    let result: Option<(i64, String)> = conn
        .query_row(
            &format!("SELECT rowid, value FROM list_items WHERE key = ?1 ORDER BY idx {order} LIMIT 1"),
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match result {
        Some((rowid, value)) => {
            conn.execute("DELETE FROM list_items WHERE rowid = ?1", params![rowid])?;
            Ok(Reply::Bulk(value))
        }
        None => Ok(Reply::Nil),
    }
}

fn cmd_lrange(conn: &Connection, key: &str, start: i64, stop: i64) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Array(vec![], Empty::List));
    }
    let len: i64 = conn.query_row(
        "SELECT COUNT(*) FROM list_items WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )?;

    if len == 0 {
        return Ok(Reply::Array(vec![], Empty::List));
    }

    let s = if start < 0 { (len + start).max(0) } else { start.min(len) };
    let e = if stop < 0 { (len + stop).max(0) } else { stop.min(len - 1) };

    if s > e {
        return Ok(Reply::Array(vec![], Empty::List));
    }

    let limit = e - s + 1;
    let mut stmt = conn.prepare(
        "SELECT value FROM list_items WHERE key = ?1 ORDER BY idx ASC LIMIT ?2 OFFSET ?3"
    )?;
    let rows: Vec<String> = stmt
        .query_map(params![key, limit, s], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::List))
}

fn cmd_llen(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let len: i64 = conn.query_row(
        "SELECT COUNT(*) FROM list_items WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )?;
    Ok(Reply::Int(len))
}

fn cmd_lrem(conn: &Connection, key: &str, count: i64, value: &str) -> CmdResult {
    let (order, limit) = match count.cmp(&0) {
        std::cmp::Ordering::Greater => ("ASC", count.unsigned_abs() as usize),
        std::cmp::Ordering::Less => ("DESC", count.unsigned_abs() as usize),
        std::cmp::Ordering::Equal => ("ASC", usize::MAX),
    };

    ensure_type(conn, key, "list")?;
    drop_if_expired(conn, key)?;
    let sql = format!(
        "SELECT rowid FROM list_items WHERE key = ?1 AND value = ?2 ORDER BY idx {order}"
    );
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
    Ok(Reply::Int(removed))
}

fn cmd_lpos(conn: &Connection, key: &str, value: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Nil);
    }
    let mut stmt = conn.prepare(
        "SELECT value FROM list_items WHERE key = ?1 ORDER BY idx ASC"
    )?;
    // Stream rows and stop at the first match instead of loading the whole list.
    let mut rows = stmt.query(params![key])?;
    let mut pos: i64 = 0;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(0)? == value {
            return Ok(Reply::Int(pos));
        }
        pos += 1;
    }
    Ok(Reply::Nil)
}

// --- Set commands ---

fn cmd_sadd(conn: &Connection, key: &str, members: &[String]) -> CmdResult {
    ensure_type(conn, key, "set")?;
    drop_if_expired(conn, key)?;
    let mut count = 0i64;
    for member in members {
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO set_members (key, member) VALUES (?1, ?2)",
            params![key, member],
        )?;
        count += inserted as i64;
    }
    Ok(Reply::Int(count))
}

fn cmd_srem(conn: &Connection, key: &str, members: &[String]) -> CmdResult {
    ensure_type(conn, key, "set")?;
    drop_if_expired(conn, key)?;
    let mut count = 0i64;
    for member in members {
        let deleted = conn.execute(
            "DELETE FROM set_members WHERE key = ?1 AND member = ?2",
            params![key, member],
        )?;
        count += deleted as i64;
    }
    Ok(Reply::Int(count))
}

fn cmd_smembers(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    let mut stmt = conn.prepare("SELECT member FROM set_members WHERE key = ?1")?;
    let rows: Vec<String> = stmt
        .query_map(params![key], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}

fn cmd_sismember(conn: &Connection, key: &str, member: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let exists = conn
        .query_row(
            "SELECT 1 FROM set_members WHERE key = ?1 AND member = ?2",
            params![key, member],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(Reply::Int(exists as i64))
}

fn cmd_scard(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM set_members WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )?;
    Ok(Reply::Int(count))
}

fn cmd_sunion(conn: &Connection, keys: &[String]) -> CmdResult {
    // Expired input sets are treated as empty and contribute nothing.
    let mut live: Vec<&String> = Vec::with_capacity(keys.len());
    for k in keys {
        if !is_expired(conn, k)? {
            live.push(k);
        }
    }
    if live.is_empty() {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    let placeholders: Vec<String> = live.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT DISTINCT member FROM set_members WHERE key IN ({})",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = live.iter().map(|k| *k as &dyn rusqlite::ToSql).collect();
    let rows: Vec<String> = stmt
        .query_map(params.as_slice(), |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}

fn cmd_sinter(conn: &Connection, keys: &[String]) -> CmdResult {
    if keys.is_empty() {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    // Any expired/missing input set makes the intersection empty.
    for k in keys {
        if is_expired(conn, k)? {
            return Ok(Reply::Array(vec![], Empty::Set));
        }
    }
    // Dedup keys so repeated args don't break the COUNT(DISTINCT key) test.
    let mut keys: Vec<&String> = keys.iter().collect();
    keys.sort();
    keys.dedup();
    let num_keys = keys.len();
    let placeholders: Vec<String> = keys.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT member FROM set_members WHERE key IN ({}) GROUP BY member HAVING COUNT(DISTINCT key) = ?{}",
        placeholders.join(", "),
        num_keys + 1
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = keys.iter().map(|k| Box::new((*k).clone()) as Box<dyn rusqlite::ToSql>).collect();
    params.push(Box::new(num_keys as i64));
    let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows: Vec<String> = stmt
        .query_map(params_ref.as_slice(), |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}

fn cmd_sdiff(conn: &Connection, keys: &[String]) -> CmdResult {
    if keys.is_empty() {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    let first = &keys[0];
    if is_expired(conn, first)? {
        return Ok(Reply::Array(vec![], Empty::Set));
    }
    if keys.len() == 1 {
        return cmd_smembers(conn, first);
    }
    // Expired "other" sets subtract nothing, so drop them.
    let mut rest: Vec<&String> = Vec::with_capacity(keys.len() - 1);
    for k in &keys[1..] {
        if !is_expired(conn, k)? {
            rest.push(k);
        }
    }
    if rest.is_empty() {
        return cmd_smembers(conn, first);
    }
    let placeholders: Vec<String> = rest.iter().enumerate().map(|(i, _)| format!("?{}", i + 2)).collect();
    let sql = format!(
        "SELECT member FROM set_members WHERE key = ?1 AND member NOT IN (SELECT member FROM set_members WHERE key IN ({}))",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![first as &dyn rusqlite::ToSql];
    for k in &rest {
        params.push(*k as &dyn rusqlite::ToSql);
    }
    let rows: Vec<String> = stmt
        .query_map(params.as_slice(), |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(Reply::Array(rows, Empty::Set))
}

// --- Hash commands ---

fn cmd_hset(conn: &Connection, key: &str, pairs: &[String]) -> CmdResult {
    if !pairs.len().is_multiple_of(2) {
        return Err(CmdError::new("ERR wrong number of arguments for 'hset' command"));
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

fn cmd_hget(conn: &Connection, key: &str, field: &str) -> CmdResult {
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

fn cmd_hdel(conn: &Connection, key: &str, fields: &[String]) -> CmdResult {
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
    Ok(Reply::Int(count))
}

fn cmd_hgetall(conn: &Connection, key: &str) -> CmdResult {
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

fn hash_column(conn: &Connection, key: &str, column: &str) -> Result<Vec<String>, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("SELECT {column} FROM hash_fields WHERE key = ?1"))?;
    let rows = stmt
        .query_map(params![key], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

fn cmd_hkeys(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Array(vec![], Empty::List));
    }
    Ok(Reply::Array(hash_column(conn, key, "field")?, Empty::List))
}

fn cmd_hvals(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Array(vec![], Empty::List));
    }
    Ok(Reply::Array(hash_column(conn, key, "value")?, Empty::List))
}

fn cmd_hlen(conn: &Connection, key: &str) -> CmdResult {
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

// --- Key commands ---

fn cmd_keys(conn: &Connection, pattern: Option<&str>) -> CmdResult {
    let pat = pattern.unwrap_or("*");
    // Translate Redis glob (* and ?) into a SQL LIKE pattern, escaping the
    // LIKE metacharacters % and _ (and the escape char itself) so they match
    // literally.
    let mut like = String::new();
    for ch in pat.chars() {
        match ch {
            '*' => like.push('%'),
            '?' => like.push('_'),
            '%' | '_' | '\\' => {
                like.push('\\');
                like.push(ch);
            }
            c => like.push(c),
        }
    }

    let mut all_keys: Vec<String> = Vec::new();
    for sql in [
        "SELECT key FROM strings WHERE key LIKE ?1 ESCAPE '\\'",
        "SELECT DISTINCT key FROM list_items WHERE key LIKE ?1 ESCAPE '\\'",
        "SELECT DISTINCT key FROM set_members WHERE key LIKE ?1 ESCAPE '\\'",
        "SELECT DISTINCT key FROM hash_fields WHERE key LIKE ?1 ESCAPE '\\'",
    ] {
        let mut stmt = conn.prepare(sql)?;
        let keys: Vec<String> = stmt
            .query_map(params![like], |row| row.get(0))?
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

fn cmd_exists(conn: &Connection, key: &str) -> CmdResult {
    if is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    Ok(Reply::Int(key_exists_in_data(conn, key)? as i64))
}

fn cmd_type(conn: &Connection, key: &str) -> CmdResult {
    Ok(Reply::Simple(key_type(conn, key)?.unwrap_or("none")))
}

fn cmd_rename(conn: &Connection, key: &str, newkey: &str) -> CmdResult {
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
    for t in ["strings", "list_items", "set_members", "hash_fields", "expiry"] {
        conn.execute(&format!("DELETE FROM {t} WHERE key = ?1"), params![newkey])?;
    }
    for t in ["strings", "list_items", "set_members", "hash_fields", "expiry"] {
        conn.execute(&format!("UPDATE {t} SET key = ?2 WHERE key = ?1"), params![key, newkey])?;
    }
    Ok(Reply::Simple("OK"))
}

// --- Utility commands ---

fn cmd_dbsize(conn: &Connection) -> CmdResult {
    let mut count: i64 = 0;
    count += conn.query_row("SELECT COUNT(*) FROM strings", [], |row| row.get::<_, i64>(0))?;
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM list_items", [], |row| row.get::<_, i64>(0))?;
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM set_members", [], |row| row.get::<_, i64>(0))?;
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM hash_fields", [], |row| row.get::<_, i64>(0))?;
    Ok(Reply::Int(count))
}

fn cmd_flushall(conn: &Connection) -> CmdResult {
    conn.execute_batch("
        DELETE FROM strings;
        DELETE FROM list_items;
        DELETE FROM set_members;
        DELETE FROM hash_fields;
        DELETE FROM expiry;
    ")?;
    Ok(Reply::Simple("OK"))
}

// --- TTL commands ---

fn cmd_expire(conn: &Connection, key: &str, seconds: i64) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    conn.execute(
        "INSERT OR REPLACE INTO expiry (key, expires_at) VALUES (?1, unixepoch() + ?2)",
        params![key, seconds],
    )?;
    Ok(Reply::Int(1))
}

fn cmd_expireat(conn: &Connection, key: &str, timestamp: i64) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    conn.execute(
        "INSERT OR REPLACE INTO expiry (key, expires_at) VALUES (?1, ?2)",
        params![key, timestamp],
    )?;
    Ok(Reply::Int(1))
}

fn cmd_ttl(conn: &Connection, key: &str) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Ok(Reply::Int(-2));
    }
    let remaining: Option<i64> = conn
        .query_row(
            "SELECT expires_at - unixepoch() FROM expiry WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Reply::Int(remaining.unwrap_or(-1)))
}

fn cmd_persist(conn: &Connection, key: &str) -> CmdResult {
    if !key_exists_in_data(conn, key)? || is_expired(conn, key)? {
        return Ok(Reply::Int(0));
    }
    let removed = conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    Ok(Reply::Int((removed > 0) as i64))
}

fn cmd_purge(conn: &Connection) -> CmdResult {
    let expired_keys: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT key FROM expiry WHERE expires_at <= unixepoch()"
        )?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    let count = expired_keys.len() as i64;
    for key in &expired_keys {
        conn.execute("DELETE FROM strings WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    Ok(Reply::Int(count))
}
