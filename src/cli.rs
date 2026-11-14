//! Command-line definitions and the transactional dispatcher.

use crate::commands::{hashes::*, keys::*, lists::*, sets::*, strings::*, ttl::*};
use crate::db::Arg;
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
    pub(crate) command: Option<TopCommand>,
}

/// What the binary can be asked to do: run one data command, or serve.
/// `serve` is kept out of `Command` so it is not available as a line in the
/// shell or in pipe mode.
#[derive(Subcommand)]
pub(crate) enum TopCommand {
    #[command(about = "Run a Redis-compatible server (RESP2/RESP3) on this database")]
    Serve(crate::server::ServeArgs),
    #[command(flatten)]
    Data(Command),
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

/// The optional condition on `expire` and its relatives (Redis NX/XX/GT/LT).
#[derive(clap::Args, Clone, Copy)]
pub(crate) struct ExpireFlags {
    #[arg(
        long,
        help = "Only if the key has no expiry (not with the other conditions)"
    )]
    nx: bool,
    #[arg(long, help = "Only if the key already has an expiry")]
    xx: bool,
    #[arg(long, help = "Only if the new expiry is later than the current one")]
    gt: bool,
    #[arg(long, help = "Only if the new expiry is earlier than the current one")]
    lt: bool,
}

impl ExpireFlags {
    /// The flags as a condition; Redis's own error if they contradict.
    fn resolve(self) -> Result<ExpireCond, CmdError> {
        ExpireCond {
            nx: self.nx,
            xx: self.xx,
            gt: self.gt,
            lt: self.lt,
        }
        .validate()
    }
}

/// `get-ex`'s expiry options; at most one may be given.
#[derive(clap::Args)]
#[group(multiple = false)]
pub(crate) struct GetExFlags {
    #[arg(
        long,
        value_name = "SECONDS",
        allow_hyphen_values = true,
        help = "Expire in this many seconds"
    )]
    ex: Option<i64>,
    #[arg(
        long,
        value_name = "MILLISECONDS",
        allow_hyphen_values = true,
        help = "Expire in this many milliseconds"
    )]
    px: Option<i64>,
    #[arg(
        long,
        value_name = "UNIX-SECONDS",
        allow_hyphen_values = true,
        help = "Expire at this Unix time (seconds)"
    )]
    exat: Option<i64>,
    #[arg(
        long,
        value_name = "UNIX-MS",
        allow_hyphen_values = true,
        help = "Expire at this Unix time (milliseconds)"
    )]
    pxat: Option<i64>,
    #[arg(long, help = "Remove the expiry")]
    persist: bool,
}

impl GetExFlags {
    fn resolve(&self) -> Result<GetEx, CmdError> {
        match (self.ex, self.px, self.exat, self.pxat) {
            (Some(n), ..) => getex_expiry(GetExUnit::Seconds, n),
            (_, Some(n), ..) => getex_expiry(GetExUnit::Millis, n),
            (_, _, Some(n), _) => getex_expiry(GetExUnit::UnixSeconds, n),
            (_, _, _, Some(n)) => getex_expiry(GetExUnit::UnixMillis, n),
            _ if self.persist => Ok(GetEx::Persist),
            _ => Ok(GetEx::Keep),
        }
    }
}

/// Paging options shared by `scan`, `h-scan` and `s-scan`.
#[derive(clap::Args)]
pub(crate) struct ScanPage {
    #[arg(
        default_value_t = 0,
        help = "Cursor from the previous page (0 starts, and 0 returned means done)"
    )]
    cursor: u64,
    #[arg(
        long = "match",
        value_name = "PATTERN",
        help = "Only names matching this glob"
    )]
    pattern: Option<String>,
    #[arg(
        long,
        default_value_t = 10,
        allow_negative_numbers = true,
        help = "Rows to examine per page"
    )]
    count: i64,
}

impl ScanPage {
    fn cursor(&self) -> i64 {
        i64::try_from(self.cursor).unwrap_or(i64::MAX)
    }

    fn pattern(&self) -> Option<Arg<'_>> {
        self.pattern.as_deref().map(Arg::from)
    }
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
    #[command(about = "Set a new string value and return the old one")]
    GetSet { key: String, value: String },
    #[command(about = "Get a string value and set, change or remove its expiry")]
    GetEx {
        key: String,
        #[command(flatten)]
        expiry: GetExFlags,
    },
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
    #[command(about = "Remove and return element from head of list (or up to COUNT elements)")]
    LPop {
        key: String,
        #[arg(allow_negative_numbers = true)]
        count: Option<i64>,
    },
    #[command(about = "Remove and return element from tail of list (or up to COUNT elements)")]
    RPop {
        key: String,
        #[arg(allow_negative_numbers = true)]
        count: Option<i64>,
    },
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
    #[command(about = "Remove and return a random member from set (or up to COUNT members)")]
    SPop {
        key: String,
        #[arg(allow_negative_numbers = true)]
        count: Option<i64>,
    },
    #[command(
        about = "Return a random member without removing it (COUNT > 0: distinct members; COUNT < 0: that many picks, repeats allowed)"
    )]
    SRandMember {
        key: String,
        #[arg(allow_negative_numbers = true)]
        count: Option<i64>,
    },
    #[command(about = "Move a member from one set to another (returns 1 or 0)")]
    SMove {
        source: String,
        destination: String,
        member: String,
    },
    #[command(about = "Iterate a set's members a page at a time")]
    SScan {
        key: String,
        #[command(flatten)]
        page: ScanPage,
    },
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
        name = "h-m-get",
        about = "Get several fields' values ('(nil)' for missing fields)"
    )]
    HMGet {
        key: String,
        #[arg(required = true)]
        fields: Vec<String>,
    },
    #[command(about = "Iterate a hash's fields a page at a time")]
    HScan {
        key: String,
        #[command(flatten)]
        page: ScanPage,
        #[arg(long, help = "Return field names only")]
        no_values: bool,
    },
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
    #[command(about = "Count how many of the given keys exist (any type)")]
    Exists {
        #[arg(required = true)]
        keys: Vec<String>,
    },
    #[command(about = "Copy a key's value and TTL to another key (returns 1 or 0)")]
    Copy {
        source: String,
        destination: String,
        #[arg(long, help = "Overwrite the destination if it exists")]
        replace: bool,
    },
    #[command(about = "Iterate the keyspace a page at a time")]
    Scan {
        #[command(flatten)]
        page: ScanPage,
        #[arg(long = "type", value_name = "TYPE", help = "Only keys of this type")]
        kind: Option<String>,
    },
    #[command(about = "Get key's type: string, list, set, hash, or none")]
    Type { key: String },
    #[command(about = "Rename a key (overwrites target if it exists)")]
    Rename { key: String, newkey: String },

    // TTL commands
    #[command(
        about = "Set key expiry in seconds from now",
        allow_hyphen_values = true
    )]
    Expire {
        key: String,
        seconds: i64,
        #[command(flatten)]
        cond: ExpireFlags,
    },
    #[command(
        about = "Set key expiry in milliseconds from now",
        allow_hyphen_values = true
    )]
    PExpire {
        key: String,
        milliseconds: i64,
        #[command(flatten)]
        cond: ExpireFlags,
    },
    #[command(
        about = "Set key expiry at Unix timestamp (seconds)",
        allow_hyphen_values = true
    )]
    ExpireAt {
        key: String,
        timestamp: i64,
        #[command(flatten)]
        cond: ExpireFlags,
    },
    #[command(
        about = "Set key expiry at Unix timestamp (milliseconds)",
        allow_hyphen_values = true
    )]
    PExpireAt {
        key: String,
        timestamp_ms: i64,
        #[command(flatten)]
        cond: ExpireFlags,
    },
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
            | Command::GetSet { .. }
            | Command::GetEx { .. }
            | Command::SMove { .. }
            | Command::Copy { .. }
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

/// A CLI string argument as a byte argument.
fn a(s: &str) -> Arg<'_> {
    Arg::from(s)
}

fn args(v: &[String]) -> Vec<Arg<'_>> {
    v.iter().map(|s| a(s)).collect()
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
                (Some(s), _) => Some(
                    s.checked_mul(1000)
                        .ok_or_else(|| CmdError::new("ERR invalid expire time in 'set' command"))?,
                ),
                (None, Some(ms)) => Some(ms),
                (None, None) => None,
            };
            let cond = if nx {
                SetCond::IfAbsent
            } else {
                SetCond::Always
            };
            cmd_set(conn, a(&key), a(&value), cond, ttl_ms, keep_ttl)
        }
        Command::Get { key } => cmd_get(conn, a(&key)),
        Command::GetDel { key } => cmd_getdel(conn, a(&key)),
        Command::GetSet { key, value } => cmd_getset(conn, a(&key), a(&value)),
        Command::GetEx { key, expiry } => cmd_getex(conn, a(&key), expiry.resolve()?),
        Command::Del { keys } => cmd_del(conn, &args(&keys)),
        Command::Incr { key } => cmd_incrby(conn, a(&key), 1),
        Command::Decr { key } => cmd_incrby(conn, a(&key), -1),
        Command::IncrBy { key, amount } => cmd_incrby(conn, a(&key), amount),
        Command::DecrBy { key, amount } => {
            let neg = amount
                .checked_neg()
                .ok_or_else(|| CmdError::new("ERR increment or decrement would overflow"))?;
            cmd_incrby(conn, a(&key), neg)
        }
        Command::Append { key, value } => cmd_append(conn, a(&key), a(&value)),
        Command::Strlen { key } => cmd_strlen(conn, a(&key)),
        Command::MSet { pairs } => cmd_mset(conn, &args(&pairs)),
        Command::MGet { keys } => cmd_mget(conn, &args(&keys)),

        Command::LPush { key, values } => cmd_lpush(conn, a(&key), &args(&values)),
        Command::RPush { key, values } => cmd_rpush(conn, a(&key), &args(&values)),
        Command::LPop { key, count: None } => cmd_pop(conn, a(&key), "ASC"),
        Command::RPop { key, count: None } => cmd_pop(conn, a(&key), "DESC"),
        Command::LPop {
            key,
            count: Some(n),
        } => cmd_pop_count(conn, a(&key), "ASC", n),
        Command::RPop {
            key,
            count: Some(n),
        } => cmd_pop_count(conn, a(&key), "DESC", n),
        Command::LRange { key, start, stop } => cmd_lrange(conn, a(&key), start, stop),
        Command::LLen { key } => cmd_llen(conn, a(&key)),
        Command::LRem { key, count, value } => cmd_lrem(conn, a(&key), count, a(&value)),
        Command::LPos { key, value } => cmd_lpos(conn, a(&key), a(&value)),
        Command::LIndex { key, index } => cmd_lindex(conn, a(&key), index),
        Command::LSet { key, index, value } => cmd_lset(conn, a(&key), index, a(&value)),
        Command::LTrim { key, start, stop } => cmd_ltrim(conn, a(&key), start, stop),
        Command::LInsert {
            key,
            r#where,
            pivot,
            value,
        } => cmd_linsert(conn, a(&key), r#where, a(&pivot), a(&value)),

        Command::SAdd { key, members } => cmd_sadd(conn, a(&key), &args(&members)),
        Command::SRem { key, members } => cmd_srem(conn, a(&key), &args(&members)),
        Command::SMembers { key } => cmd_smembers(conn, a(&key)),
        Command::SIsMember { key, member } => cmd_sismember(conn, a(&key), a(&member)),
        Command::SCard { key } => cmd_scard(conn, a(&key)),
        Command::SPop { key, count: None } => cmd_spop(conn, a(&key)),
        Command::SPop {
            key,
            count: Some(n),
        } => cmd_spop_count(conn, a(&key), n),
        Command::SRandMember { key, count } => cmd_srandmember(conn, a(&key), count),
        Command::SMove {
            source,
            destination,
            member,
        } => cmd_smove(conn, a(&source), a(&destination), a(&member)),
        Command::SScan { key, page } => cmd_scan_within(
            conn,
            "set",
            a(&key),
            page.cursor(),
            page.pattern(),
            page.count,
            true,
        ),
        Command::SUnion { keys } => cmd_sunion(conn, &args(&keys)),
        Command::SInter { keys } => cmd_sinter(conn, &args(&keys)),
        Command::SDiff { keys } => cmd_sdiff(conn, &args(&keys)),

        Command::HSet { key, pairs } => cmd_hset(conn, a(&key), &args(&pairs)),
        Command::HGet { key, field } => cmd_hget(conn, a(&key), a(&field)),
        Command::HExists { key, field } => cmd_hexists(conn, a(&key), a(&field)),
        Command::HMGet { key, fields } => cmd_hmget(conn, a(&key), &args(&fields)),
        Command::HScan {
            key,
            page,
            no_values,
        } => cmd_scan_within(
            conn,
            "hash",
            a(&key),
            page.cursor(),
            page.pattern(),
            page.count,
            !no_values,
        ),
        Command::HIncrBy { key, field, amount } => cmd_hincrby(conn, a(&key), a(&field), amount),
        Command::HDel { key, fields } => cmd_hdel(conn, a(&key), &args(&fields)),
        Command::HGetAll { key } => cmd_hgetall(conn, a(&key)),
        Command::HKeys { key } => cmd_hkeys(conn, a(&key)),
        Command::HVals { key } => cmd_hvals(conn, a(&key)),
        Command::HLen { key } => cmd_hlen(conn, a(&key)),

        Command::Keys { pattern } => cmd_keys(conn, pattern.as_deref().map(Arg::from)),
        Command::Exists { keys } => cmd_exists_many(conn, &args(&keys)),
        Command::Copy {
            source,
            destination,
            replace,
        } => cmd_copy(conn, a(&source), a(&destination), replace),
        Command::Scan { page, kind } => cmd_scan(
            conn,
            page.cursor(),
            page.pattern(),
            page.count,
            kind.as_deref().map(Arg::from),
        ),
        Command::Type { key } => cmd_type(conn, a(&key)),
        Command::Rename { key, newkey } => cmd_rename(conn, a(&key), a(&newkey)),

        Command::Expire { key, seconds, cond } => {
            cmd_expire(conn, a(&key), seconds, cond.resolve()?)
        }
        Command::PExpire {
            key,
            milliseconds,
            cond,
        } => cmd_pexpire(conn, a(&key), milliseconds, cond.resolve()?),
        Command::ExpireAt {
            key,
            timestamp,
            cond,
        } => cmd_expireat(conn, a(&key), timestamp, cond.resolve()?),
        Command::PExpireAt {
            key,
            timestamp_ms,
            cond,
        } => cmd_pexpireat(conn, a(&key), timestamp_ms, cond.resolve()?),
        Command::Ttl { key } => cmd_ttl(conn, a(&key)),
        Command::PTtl { key } => cmd_pttl(conn, a(&key)),
        Command::Persist { key } => cmd_persist(conn, a(&key)),
        Command::Purge => cmd_purge(conn),

        Command::DbSize => cmd_dbsize(conn),
        Command::FlushAll => cmd_flushall(conn),
    }
}
