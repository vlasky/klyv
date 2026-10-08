//! Command-line definitions and the transactional dispatcher.

use crate::commands::{hashes::*, keys::*, lists::*, sets::*, strings::*, ttl::*};
use crate::reply::*;
use clap::{Parser, Subcommand, ValueEnum};
use rusqlite::{Connection, TransactionBehavior};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "klyv",
    version,
    about = "Redis-compatible embedded KV store backed by SQLite",
    after_help = "Run without a command to enter the interactive shell (or pipe \
                  commands, one per line, into stdin)."
)]
pub(crate) struct Cli {
    #[arg(short, long, env = "KLYV_DB")]
    pub(crate) db: PathBuf,

    #[arg(
        short,
        long,
        value_enum,
        default_value = "human",
        help = "Output format: human (redis-cli style), raw (bare values), json"
    )]
    pub(crate) format: OutputFormat,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

/// Parser for one line of shell/pipe input: just the subcommand, no --db or
/// --format (those belong to the session, not the line).
#[derive(Parser)]
#[command(name = "klyv", no_binary_name = true, disable_version_flag = true)]
pub(crate) struct LineInput {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum OutputFormat {
    /// redis-cli-style text: (integer) N, (nil), numbered quoted arrays
    Human,
    /// Bare values, one per line; nil is an empty line, like redis-cli --raw
    Raw,
    /// A single JSON value: strings, numbers, null, arrays
    Json,
}

/// Where LINSERT places the new element relative to the pivot.
#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum InsertWhere {
    Before,
    After,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "Set a string value")]
    Set {
        key: String,
        value: String,
        #[arg(long, help = "Only set if the key does not already exist")]
        nx: bool,
        #[arg(
            long,
            value_name = "SECONDS",
            conflicts_with = "px",
            allow_hyphen_values = true,
            help = "Set TTL in seconds, atomically with the value"
        )]
        ex: Option<i64>,
        #[arg(
            long,
            value_name = "MILLISECONDS",
            allow_hyphen_values = true,
            help = "Set TTL in milliseconds"
        )]
        px: Option<i64>,
        #[arg(
            long,
            conflicts_with_all = ["ex", "px"],
            help = "Keep the key's existing TTL (by default SET clears it, like Redis)"
        )]
        keep_ttl: bool,
    },
    #[command(about = "Get a string value (prints '(nil)' if not found)")]
    Get { key: String },
    #[command(about = "Get a string value and delete the key atomically")]
    GetDel { key: String },
    #[command(about = "Delete one or more keys (any type)")]
    Del {
        #[arg(required = true)]
        keys: Vec<String>,
    },
    #[command(about = "Increment integer value by 1 (creates key at 0 if missing)")]
    Incr { key: String },
    #[command(about = "Decrement integer value by 1 (creates key at 0 if missing)")]
    Decr { key: String },
    #[command(
        about = "Increment integer value by amount",
        allow_hyphen_values = true
    )]
    IncrBy { key: String, amount: i64 },
    #[command(
        about = "Decrement integer value by amount",
        allow_hyphen_values = true
    )]
    DecrBy { key: String, amount: i64 },
    #[command(about = "Append to string value (creates key if missing), returns new length")]
    Append { key: String, value: String },
    #[command(about = "Get string length (0 if key missing)")]
    Strlen { key: String },
    #[command(about = "Set multiple key-value pairs atomically")]
    MSet {
        #[arg(
            required = true,
            help = "Alternating key value pairs: key1 val1 key2 val2 ..."
        )]
        pairs: Vec<String>,
    },
    #[command(about = "Get multiple values (prints one per line, '(nil)' for missing)")]
    MGet {
        #[arg(required = true)]
        keys: Vec<String>,
    },

    #[command(about = "Push values to head of list (leftmost)")]
    LPush {
        key: String,
        #[arg(required = true)]
        values: Vec<String>,
    },
    #[command(about = "Push values to tail of list (rightmost)")]
    RPush {
        key: String,
        #[arg(required = true)]
        values: Vec<String>,
    },
    #[command(about = "Remove and return element from head of list")]
    LPop { key: String },
    #[command(about = "Remove and return element from tail of list")]
    RPop { key: String },
    #[command(
        about = "Return elements from index START to STOP (inclusive, 0-based, negatives count from end)",
        allow_hyphen_values = true
    )]
    LRange { key: String, start: i64, stop: i64 },
    #[command(about = "Get list length")]
    LLen { key: String },
    #[command(
        about = "Remove COUNT occurrences of value (0=all, +N=from head, -N=from tail)",
        allow_hyphen_values = true
    )]
    LRem {
        key: String,
        #[arg(help = "0=remove all, +N=first N from head, -N=first N from tail")]
        count: i64,
        value: String,
    },
    #[command(about = "Find first occurrence of value in list (returns 0-based index or '(nil)')")]
    LPos { key: String, value: String },
    #[command(
        about = "Get element at index (0-based, negatives count from end)",
        allow_hyphen_values = true
    )]
    LIndex { key: String, index: i64 },
    #[command(
        about = "Set element at index to value (errors if key or index missing)",
        allow_hyphen_values = true
    )]
    LSet {
        key: String,
        index: i64,
        value: String,
    },
    #[command(
        about = "Trim list to elements from index START to STOP (inclusive)",
        allow_hyphen_values = true
    )]
    LTrim { key: String, start: i64, stop: i64 },
    #[command(about = "Insert value before/after first occurrence of pivot")]
    LInsert {
        key: String,
        #[arg(value_enum)]
        r#where: InsertWhere,
        pivot: String,
        value: String,
    },

    #[command(about = "Add members to set (ignores duplicates)")]
    SAdd {
        key: String,
        #[arg(required = true)]
        members: Vec<String>,
    },
    #[command(about = "Remove members from set")]
    SRem {
        key: String,
        #[arg(required = true)]
        members: Vec<String>,
    },
    #[command(about = "List all members of set")]
    SMembers { key: String },
    #[command(about = "Test if member exists in set (returns 1 or 0)")]
    SIsMember { key: String, member: String },
    #[command(about = "Get number of members in set")]
    SCard { key: String },
    #[command(about = "Remove and return a random member from set")]
    SPop { key: String },
    #[command(about = "Return union of multiple sets")]
    SUnion {
        #[arg(required = true)]
        keys: Vec<String>,
    },
    #[command(about = "Return intersection of multiple sets")]
    SInter {
        #[arg(required = true)]
        keys: Vec<String>,
    },
    #[command(about = "Return members in first set not in any other sets")]
    SDiff {
        #[arg(required = true)]
        keys: Vec<String>,
    },

    #[command(about = "Set field-value pairs in a hash")]
    HSet {
        key: String,
        #[arg(
            required = true,
            help = "Alternating field value pairs: field1 val1 field2 val2 ..."
        )]
        pairs: Vec<String>,
    },
    #[command(about = "Get a field's value from a hash")]
    HGet { key: String, field: String },
    #[command(about = "Test if field exists in hash (returns 1 or 0)")]
    HExists { key: String, field: String },
    #[command(
        about = "Increment integer hash field by amount (creates field at 0 if missing)",
        allow_hyphen_values = true
    )]
    HIncrBy {
        key: String,
        field: String,
        amount: i64,
    },
    #[command(about = "Delete fields from a hash")]
    HDel {
        key: String,
        #[arg(required = true)]
        fields: Vec<String>,
    },
    #[command(about = "Get all field-value pairs (alternating lines: field, value)")]
    HGetAll { key: String },
    #[command(about = "List all field names in a hash")]
    HKeys { key: String },
    #[command(about = "List all values in a hash")]
    HVals { key: String },
    #[command(about = "Get number of fields in a hash")]
    HLen { key: String },

    #[command(
        about = "List keys matching a Redis glob (* ? [abc] [^a] [a-z], \\ escapes; omit for all)"
    )]
    Keys { pattern: Option<String> },
    #[command(about = "Test if key exists (any type, returns 1 or 0)")]
    Exists { key: String },
    #[command(about = "Get key's type: string, list, set, hash, or none")]
    Type { key: String },
    #[command(about = "Rename a key (overwrites target if it exists)")]
    Rename { key: String, newkey: String },

    // TTL commands
    #[command(
        about = "Set key expiry in seconds from now",
        allow_hyphen_values = true
    )]
    Expire { key: String, seconds: i64 },
    #[command(
        about = "Set key expiry in milliseconds from now",
        allow_hyphen_values = true
    )]
    PExpire { key: String, milliseconds: i64 },
    #[command(
        about = "Set key expiry at Unix timestamp (seconds)",
        allow_hyphen_values = true
    )]
    ExpireAt { key: String, timestamp: i64 },
    #[command(
        about = "Set key expiry at Unix timestamp (milliseconds)",
        allow_hyphen_values = true
    )]
    PExpireAt { key: String, timestamp_ms: i64 },
    #[command(about = "Get remaining TTL in seconds (-1=no expiry, -2=key missing)")]
    Ttl { key: String },
    #[command(about = "Get remaining TTL in milliseconds (-1=no expiry, -2=key missing)")]
    PTtl { key: String },
    #[command(about = "Remove expiry from key")]
    Persist { key: String },
    #[command(about = "Delete all expired keys and report count")]
    Purge,

    #[command(about = "Count live keys (expired keys are not counted)")]
    DbSize,
    #[command(about = "Delete all data from all tables")]
    FlushAll,
}

/// Whether a command mutates the database (BEGIN IMMEDIATE) or only reads
/// (deferred transaction, giving all its statements one consistent snapshot).
pub(crate) fn is_write(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Set { .. }
            | Command::GetDel { .. }
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
            | Command::LSet { .. }
            | Command::LTrim { .. }
            | Command::LInsert { .. }
            | Command::SAdd { .. }
            | Command::SRem { .. }
            | Command::SPop { .. }
            | Command::HSet { .. }
            | Command::HIncrBy { .. }
            | Command::HDel { .. }
            | Command::Rename { .. }
            | Command::Expire { .. }
            | Command::PExpire { .. }
            | Command::ExpireAt { .. }
            | Command::PExpireAt { .. }
            | Command::Persist { .. }
            | Command::Purge
            | Command::FlushAll
    )
}

/// Runs the command inside a single transaction: writes take the write lock
/// up front (BEGIN IMMEDIATE); reads get a consistent snapshot. On error the
/// transaction is dropped and rolls back, leaving the data unchanged.
pub(crate) fn dispatch(conn: &mut Connection, cmd: Command) -> CmdResult {
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

pub(crate) fn run(conn: &Connection, cmd: Command) -> CmdResult {
    match cmd {
        Command::Set {
            key,
            value,
            nx,
            ex,
            px,
            keep_ttl,
        } => {
            let ttl_ms = match (ex, px) {
                (Some(s), _) => Some(s.saturating_mul(1000)),
                (None, Some(ms)) => Some(ms),
                (None, None) => None,
            };
            cmd_set(conn, &key, &value, nx, ttl_ms, keep_ttl)
        }
        Command::Get { key } => cmd_get(conn, &key),
        Command::GetDel { key } => cmd_getdel(conn, &key),
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
        Command::LIndex { key, index } => cmd_lindex(conn, &key, index),
        Command::LSet { key, index, value } => cmd_lset(conn, &key, index, &value),
        Command::LTrim { key, start, stop } => cmd_ltrim(conn, &key, start, stop),
        Command::LInsert {
            key,
            r#where,
            pivot,
            value,
        } => cmd_linsert(conn, &key, r#where, &pivot, &value),

        Command::SAdd { key, members } => cmd_sadd(conn, &key, &members),
        Command::SRem { key, members } => cmd_srem(conn, &key, &members),
        Command::SMembers { key } => cmd_smembers(conn, &key),
        Command::SIsMember { key, member } => cmd_sismember(conn, &key, &member),
        Command::SCard { key } => cmd_scard(conn, &key),
        Command::SPop { key } => cmd_spop(conn, &key),
        Command::SUnion { keys } => cmd_sunion(conn, &keys),
        Command::SInter { keys } => cmd_sinter(conn, &keys),
        Command::SDiff { keys } => cmd_sdiff(conn, &keys),

        Command::HSet { key, pairs } => cmd_hset(conn, &key, &pairs),
        Command::HGet { key, field } => cmd_hget(conn, &key, &field),
        Command::HExists { key, field } => cmd_hexists(conn, &key, &field),
        Command::HIncrBy { key, field, amount } => cmd_hincrby(conn, &key, &field, amount),
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
        Command::PExpire { key, milliseconds } => cmd_pexpire(conn, &key, milliseconds),
        Command::ExpireAt { key, timestamp } => cmd_expireat(conn, &key, timestamp),
        Command::PExpireAt { key, timestamp_ms } => cmd_pexpireat(conn, &key, timestamp_ms),
        Command::Ttl { key } => cmd_ttl(conn, &key),
        Command::PTtl { key } => cmd_pttl(conn, &key),
        Command::Persist { key } => cmd_persist(conn, &key),
        Command::Purge => cmd_purge(conn),

        Command::DbSize => cmd_dbsize(conn),
        Command::FlushAll => cmd_flushall(conn),
    }
}
