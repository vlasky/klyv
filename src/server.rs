//! `klyv serve`: a Redis-compatible server speaking RESP2 and RESP3 over TCP and/or a
//! Unix domain socket, so ordinary Redis clients can use a klyv database.
//!
//! One thread per client connection, each with its own SQLite handle. There
//! is no shared in-memory state to coordinate: every command is one SQLite
//! transaction (writes `BEGIN IMMEDIATE`, reads deferred), exactly as in the
//! CLI, so connections serialise through SQLite's own locking and the CLI
//! can be used against the same file while the server runs.
//!
//! Commands arrive as byte-string arguments and are routed by `lookup` to the
//! same `cmd_*` functions the CLI uses. Connection-level commands that
//! clients send while connecting (PING, HELLO, SELECT, CLIENT, ...) are
//! answered in `connection_command` without touching the database.

use crate::commands::{hashes::*, keys::*, lists::*, sets::*, strings::*, ttl::*};
use crate::db::{Arg, open_db};
use crate::reply::{CmdError, CmdResult, Empty, Reply};
use crate::resp::{Proto, ReadError, encode, encode_error, read_command};
use rusqlite::{Connection, TransactionBehavior};
use std::cell::RefCell;
use std::io::{BufReader, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// The Redis version reported to clients (HELLO, INFO). Some clients gate
/// features on it, so it names the newest Redis whose commands klyv's subset
/// is drawn from rather than anything newer.
const REDIS_COMPAT_VERSION: &str = "6.2.0";

#[derive(clap::Args)]
pub(crate) struct ServeArgs {
    /// TCP address to listen on; repeatable. Defaults to 127.0.0.1:6379
    /// unless only --unixsocket is given.
    #[arg(long, value_name = "ADDR:PORT")]
    pub(crate) bind: Vec<String>,

    /// Unix domain socket path to listen on (Unix only).
    #[arg(long, value_name = "PATH")]
    pub(crate) unixsocket: Option<PathBuf>,

    /// Permissions for the Unix socket, in octal (e.g. 700).
    #[arg(long, value_name = "MODE", requires = "unixsocket")]
    pub(crate) unixsocketperm: Option<String>,

    /// Maximum simultaneous client connections; further ones are refused.
    /// Each client uses a thread and four file descriptors, so the usable
    /// maximum is also bounded by the process's open-file limit.
    #[arg(long, value_name = "N", default_value_t = 10_000)]
    pub(crate) maxclients: usize,

    /// Require clients to authenticate with this password (AUTH, or HELLO
    /// ... AUTH). Prefer the environment variable: a command-line argument
    /// is visible to other users in the process list.
    #[arg(
        long,
        value_name = "PASSWORD",
        env = "KLYV_REQUIREPASS",
        hide_env_values = true
    )]
    pub(crate) requirepass: Option<String>,
}

struct Shared {
    db: PathBuf,
    next_id: AtomicU64,
    clients: AtomicUsize,
    max_clients: usize,
    tcp_port: u16,
    /// The password clients must present, if one is configured.
    password: Option<Vec<u8>>,
}

/// Per-connection state.
struct Session {
    id: u64,
    name: Option<Vec<u8>>,
    /// RESP2 until the client asks for RESP3 with HELLO 3.
    proto: Proto,
    /// False until the client presents the password, if one is configured.
    authenticated: bool,
    /// `Some` between MULTI and EXEC/DISCARD: the commands queued so far.
    multi: Option<Vec<Vec<Vec<u8>>>>,
    /// A command was rejected while queueing, so EXEC must abort.
    multi_error: bool,
    /// Keys under WATCH, each with a fingerprint of its state when watched.
    watched: Vec<(Vec<u8>, Option<u64>)>,
}

fn err(msg: impl Into<String>) -> CmdError {
    CmdError::new(msg)
}

fn syntax_error() -> CmdError {
    err("ERR syntax error")
}

/// An integer argument, in Redis's strict syntax (see `parse_int`).
fn int(a: Arg<'_>) -> Result<i64, CmdError> {
    crate::db::parse_int(a.as_bytes())
        .ok_or_else(|| err("ERR value is not an integer or out of range"))
}

fn is(a: Arg<'_>, word: &str) -> bool {
    a.as_bytes().eq_ignore_ascii_case(word.as_bytes())
}

fn invalid_expire(name: &str) -> CmdError {
    err(format!("ERR invalid expire time in '{name}' command"))
}

/// A TTL argument in `unit_ms` units as milliseconds; like Redis, zero,
/// negative and unrepresentable values are all "invalid expire time".
fn ttl_ms(a: Arg<'_>, unit_ms: i64, name: &str) -> Result<i64, CmdError> {
    let n = int(a)?;
    if n <= 0 {
        return Err(invalid_expire(name));
    }
    n.checked_mul(unit_ms).ok_or_else(|| invalid_expire(name))
}

/// SET key value [NX | XX] [EX seconds | PX milliseconds | KEEPTTL]
fn set(c: &Connection, a: &[Arg<'_>]) -> CmdResult {
    let mut cond = SetCond::Always;
    let mut ttl = None;
    let mut keep_ttl = false;
    let mut opts = a[2..].iter().copied();
    while let Some(opt) = opts.next() {
        if is(opt, "NX") && cond == SetCond::Always {
            cond = SetCond::IfAbsent;
        } else if is(opt, "XX") && cond == SetCond::Always {
            cond = SetCond::IfExists;
        } else if is(opt, "KEEPTTL") && ttl.is_none() && !keep_ttl {
            keep_ttl = true;
        } else if (is(opt, "EX") || is(opt, "PX")) && ttl.is_none() && !keep_ttl {
            let unit = if is(opt, "EX") { 1000 } else { 1 };
            let value = opts.next().ok_or_else(syntax_error)?;
            ttl = Some(ttl_ms(value, unit, "set")?);
        } else {
            return Err(syntax_error());
        }
    }
    cmd_set(c, a[0], a[1], cond, ttl, keep_ttl)
}

/// SETEX / PSETEX key ttl value
fn set_with_ttl(c: &Connection, a: &[Arg<'_>], unit_ms: i64, name: &str) -> CmdResult {
    let ttl = ttl_ms(a[1], unit_ms, name)?;
    cmd_set(c, a[0], a[2], SetCond::Always, Some(ttl), false)
}

fn linsert(c: &Connection, a: &[Arg<'_>]) -> CmdResult {
    let place = if is(a[1], "BEFORE") {
        crate::cli::InsertWhere::Before
    } else if is(a[1], "AFTER") {
        crate::cli::InsertWhere::After
    } else {
        return Err(syntax_error());
    };
    cmd_linsert(c, a[0], place, a[2], a[3])
}

/// The optional count of LPOP/RPOP/SPOP. Redis reports a count that is not
/// a non-negative integer as out of range rather than as a bad integer.
fn pop_count(a: &[Arg<'_>], name: &[u8]) -> Result<Option<i64>, CmdError> {
    match a {
        [_] => Ok(None),
        [_, count] => crate::db::parse_int(count.as_bytes())
            .filter(|n| *n >= 0)
            .map(Some)
            .ok_or_else(|| err("ERR value is out of range, must be positive")),
        _ => Err(wrong_arity(name)),
    }
}

/// The NX | XX | GT | LT options of EXPIRE and its relatives.
fn expire_cond(opts: &[Arg<'_>]) -> Result<ExpireCond, CmdError> {
    let mut cond = ExpireCond::default();
    for &opt in opts {
        if is(opt, "NX") {
            cond.nx = true;
        } else if is(opt, "XX") {
            cond.xx = true;
        } else if is(opt, "GT") {
            cond.gt = true;
        } else if is(opt, "LT") {
            cond.lt = true;
        } else {
            return Err(err(format!(
                "ERR Unsupported option {}",
                String::from_utf8_lossy(opt.as_bytes())
            )));
        }
    }
    cond.validate()
}

/// GETEX key [EX s | PX ms | EXAT ts | PXAT ms | PERSIST]
fn getex(c: &Connection, a: &[Arg<'_>]) -> CmdResult {
    let expiry = match &a[1..] {
        [] => GetEx::Keep,
        [opt] if is(*opt, "PERSIST") => GetEx::Persist,
        [opt, n] => {
            let unit = if is(*opt, "EX") {
                GetExUnit::Seconds
            } else if is(*opt, "PX") {
                GetExUnit::Millis
            } else if is(*opt, "EXAT") {
                GetExUnit::UnixSeconds
            } else if is(*opt, "PXAT") {
                GetExUnit::UnixMillis
            } else {
                return Err(syntax_error());
            };
            getex_expiry(unit, int(*n)?)?
        }
        _ => return Err(syntax_error()),
    };
    cmd_getex(c, a[0], expiry)
}

/// COPY source destination [DB index] [REPLACE]. There is one database, so
/// only index 0 is accepted.
fn copy(c: &Connection, a: &[Arg<'_>]) -> CmdResult {
    let mut replace = false;
    let mut opts = a[2..].iter().copied();
    while let Some(opt) = opts.next() {
        if is(opt, "REPLACE") {
            replace = true;
        } else if is(opt, "DB") {
            if int(opts.next().ok_or_else(syntax_error)?)? != 0 {
                return Err(err("ERR DB index is out of range"));
            }
        } else {
            return Err(syntax_error());
        }
    }
    cmd_copy(c, a[0], a[1], replace)
}

/// A SCAN cursor: an unsigned decimal. Anything else is Redis's "invalid
/// cursor"; a value beyond any rowid simply finds nothing.
fn cursor(a: Arg<'_>) -> Result<i64, CmdError> {
    std::str::from_utf8(a.as_bytes())
        .ok()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .map(|n| i64::try_from(n).unwrap_or(i64::MAX))
        .ok_or_else(|| err("ERR invalid cursor"))
}

/// The options of the SCAN family: MATCH pattern, COUNT n, and where the
/// command has them, TYPE name (SCAN) and NOVALUES (HSCAN).
struct ScanOpts<'a> {
    pattern: Option<Arg<'a>>,
    count: i64,
    kind: Option<Arg<'a>>,
    values: bool,
}

fn scan_opts<'a>(opts: &[Arg<'a>], scan: bool, hash: bool) -> Result<ScanOpts<'a>, CmdError> {
    let mut parsed = ScanOpts {
        pattern: None,
        count: 10,
        kind: None,
        values: true,
    };
    let mut opts = opts.iter().copied();
    while let Some(opt) = opts.next() {
        if is(opt, "MATCH") {
            parsed.pattern = Some(opts.next().ok_or_else(syntax_error)?);
        } else if is(opt, "COUNT") {
            parsed.count = int(opts.next().ok_or_else(syntax_error)?)?;
            if parsed.count < 1 {
                return Err(syntax_error());
            }
        } else if scan && is(opt, "TYPE") {
            parsed.kind = Some(opts.next().ok_or_else(syntax_error)?);
        } else if is(opt, "NOVALUES") {
            if !hash {
                return Err(err("ERR NOVALUES option can only be used in HSCAN"));
            }
            parsed.values = false;
        } else {
            return Err(syntax_error());
        }
    }
    Ok(parsed)
}

fn scan_within(c: &Connection, kind: &'static str, a: &[Arg<'_>]) -> CmdResult {
    let at = cursor(a[1])?;
    let opts = scan_opts(&a[2..], false, kind == "hash")?;
    cmd_scan_within(c, kind, a[0], at, opts.pattern, opts.count, opts.values)
}

type Handler = fn(&Connection, &[Arg<'_>]) -> CmdResult;

/// A data command. `arity` counts the command name, Redis-style: positive is
/// exact, negative is a minimum. Handlers receive the arguments *after* the
/// name and may index up to the arity without checking.
struct Spec {
    arity: i32,
    write: bool,
    run: Handler,
}

const fn r(arity: i32, run: Handler) -> Spec {
    Spec {
        arity,
        write: false,
        run,
    }
}

const fn w(arity: i32, run: Handler) -> Spec {
    Spec {
        arity,
        write: true,
        run,
    }
}

/// Names of the data commands, for COMMAND COUNT and the completeness test.
const DATA_COMMANDS: &[&str] = &[
    "SET",
    "SETNX",
    "SETEX",
    "PSETEX",
    "GET",
    "GETDEL",
    "DEL",
    "UNLINK",
    "INCR",
    "DECR",
    "INCRBY",
    "DECRBY",
    "APPEND",
    "STRLEN",
    "MSET",
    "MGET",
    "LPUSH",
    "RPUSH",
    "LPOP",
    "RPOP",
    "LRANGE",
    "LLEN",
    "LREM",
    "LPOS",
    "LINDEX",
    "LSET",
    "LTRIM",
    "LINSERT",
    "SADD",
    "SREM",
    "SMEMBERS",
    "SISMEMBER",
    "SCARD",
    "SPOP",
    "SUNION",
    "SINTER",
    "SDIFF",
    "HSET",
    "HMSET",
    "HGET",
    "HEXISTS",
    "HINCRBY",
    "HDEL",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "EXPIRE",
    "PEXPIRE",
    "EXPIREAT",
    "PEXPIREAT",
    "TTL",
    "PTTL",
    "PERSIST",
    "PURGE",
    "KEYS",
    "EXISTS",
    "TYPE",
    "RENAME",
    "DBSIZE",
    "FLUSHALL",
    "FLUSHDB",
    "GETSET",
    "GETEX",
    "SRANDMEMBER",
    "SMOVE",
    "SSCAN",
    "HMGET",
    "HSCAN",
    "COPY",
    "SCAN",
];

/// The data command table, keyed by upper-cased name.
fn lookup(name: &[u8]) -> Option<Spec> {
    Some(match name {
        b"SET" => w(-3, set),
        b"SETNX" => w(3, |c, a| {
            let reply = cmd_set(c, a[0], a[1], SetCond::IfAbsent, None, false)?;
            Ok(Reply::Int(!matches!(reply, Reply::Nil) as i64))
        }),
        b"SETEX" => w(4, |c, a| set_with_ttl(c, a, 1000, "setex")),
        b"PSETEX" => w(4, |c, a| set_with_ttl(c, a, 1, "psetex")),
        b"GET" => r(2, |c, a| cmd_get(c, a[0])),
        b"GETDEL" => w(2, |c, a| cmd_getdel(c, a[0])),
        b"GETSET" => w(3, |c, a| cmd_getset(c, a[0], a[1])),
        b"GETEX" => w(-2, getex),
        b"DEL" | b"UNLINK" => w(-2, cmd_del),
        b"INCR" => w(2, |c, a| cmd_incrby(c, a[0], 1)),
        b"DECR" => w(2, |c, a| cmd_incrby(c, a[0], -1)),
        b"INCRBY" => w(3, |c, a| cmd_incrby(c, a[0], int(a[1])?)),
        b"DECRBY" => w(3, |c, a| {
            let amount = int(a[1])?
                .checked_neg()
                .ok_or_else(|| err("ERR decrement would overflow"))?;
            cmd_incrby(c, a[0], amount)
        }),
        b"APPEND" => w(3, |c, a| cmd_append(c, a[0], a[1])),
        b"STRLEN" => r(2, |c, a| cmd_strlen(c, a[0])),
        b"MSET" => w(-3, cmd_mset),
        b"MGET" => r(-2, cmd_mget),

        b"LPUSH" => w(-3, |c, a| cmd_lpush(c, a[0], &a[1..])),
        b"RPUSH" => w(-3, |c, a| cmd_rpush(c, a[0], &a[1..])),
        b"LPOP" => w(-2, |c, a| match pop_count(a, b"LPOP")? {
            None => cmd_pop(c, a[0], "ASC"),
            Some(n) => cmd_pop_count(c, a[0], "ASC", n),
        }),
        b"RPOP" => w(-2, |c, a| match pop_count(a, b"RPOP")? {
            None => cmd_pop(c, a[0], "DESC"),
            Some(n) => cmd_pop_count(c, a[0], "DESC", n),
        }),
        b"LRANGE" => r(4, |c, a| cmd_lrange(c, a[0], int(a[1])?, int(a[2])?)),
        b"LLEN" => r(2, |c, a| cmd_llen(c, a[0])),
        b"LREM" => w(4, |c, a| cmd_lrem(c, a[0], int(a[1])?, a[2])),
        b"LPOS" => r(3, |c, a| cmd_lpos(c, a[0], a[1])),
        b"LINDEX" => r(3, |c, a| cmd_lindex(c, a[0], int(a[1])?)),
        b"LSET" => w(4, |c, a| cmd_lset(c, a[0], int(a[1])?, a[2])),
        b"LTRIM" => w(4, |c, a| cmd_ltrim(c, a[0], int(a[1])?, int(a[2])?)),
        b"LINSERT" => w(5, linsert),

        b"SADD" => w(-3, |c, a| cmd_sadd(c, a[0], &a[1..])),
        b"SREM" => w(-3, |c, a| cmd_srem(c, a[0], &a[1..])),
        b"SMEMBERS" => r(2, |c, a| cmd_smembers(c, a[0])),
        b"SISMEMBER" => r(3, |c, a| cmd_sismember(c, a[0], a[1])),
        b"SCARD" => r(2, |c, a| cmd_scard(c, a[0])),
        b"SPOP" => w(-2, |c, a| match pop_count(a, b"SPOP")? {
            None => cmd_spop(c, a[0]),
            Some(n) => cmd_spop_count(c, a[0], n),
        }),
        b"SRANDMEMBER" => r(-2, |c, a| match a {
            [key] => cmd_srandmember(c, *key, None),
            [key, count] => cmd_srandmember(c, *key, Some(int(*count)?)),
            _ => Err(wrong_arity(b"SRANDMEMBER")),
        }),
        b"SMOVE" => w(4, |c, a| cmd_smove(c, a[0], a[1], a[2])),
        b"SSCAN" => r(-3, |c, a| scan_within(c, "set", a)),
        b"SUNION" => r(-2, cmd_sunion),
        b"SINTER" => r(-2, cmd_sinter),
        b"SDIFF" => r(-2, cmd_sdiff),

        b"HSET" => w(-4, |c, a| cmd_hset(c, a[0], &a[1..])),
        b"HMSET" => w(-4, |c, a| {
            cmd_hset(c, a[0], &a[1..])?;
            Ok(Reply::Simple("OK"))
        }),
        b"HGET" => r(3, |c, a| cmd_hget(c, a[0], a[1])),
        b"HEXISTS" => r(3, |c, a| cmd_hexists(c, a[0], a[1])),
        b"HINCRBY" => w(4, |c, a| cmd_hincrby(c, a[0], a[1], int(a[2])?)),
        b"HDEL" => w(-3, |c, a| cmd_hdel(c, a[0], &a[1..])),
        b"HGETALL" => r(2, |c, a| cmd_hgetall(c, a[0])),
        b"HKEYS" => r(2, |c, a| cmd_hkeys(c, a[0])),
        b"HVALS" => r(2, |c, a| cmd_hvals(c, a[0])),
        b"HLEN" => r(2, |c, a| cmd_hlen(c, a[0])),
        b"HMGET" => r(-3, |c, a| cmd_hmget(c, a[0], &a[1..])),
        b"HSCAN" => r(-3, |c, a| scan_within(c, "hash", a)),

        b"EXPIRE" => w(-3, |c, a| {
            cmd_expire(c, a[0], int(a[1])?, expire_cond(&a[2..])?)
        }),
        b"PEXPIRE" => w(-3, |c, a| {
            cmd_pexpire(c, a[0], int(a[1])?, expire_cond(&a[2..])?)
        }),
        b"EXPIREAT" => w(-3, |c, a| {
            cmd_expireat(c, a[0], int(a[1])?, expire_cond(&a[2..])?)
        }),
        b"PEXPIREAT" => w(-3, |c, a| {
            cmd_pexpireat(c, a[0], int(a[1])?, expire_cond(&a[2..])?)
        }),
        b"TTL" => r(2, |c, a| cmd_ttl(c, a[0])),
        b"PTTL" => r(2, |c, a| cmd_pttl(c, a[0])),
        b"PERSIST" => w(2, |c, a| cmd_persist(c, a[0])),
        // klyv-specific: physically delete expired keys.
        b"PURGE" => w(1, |c, _| cmd_purge(c)),

        b"KEYS" => r(2, |c, a| cmd_keys(c, Some(a[0]))),
        b"EXISTS" => r(-2, cmd_exists_many),
        b"COPY" => w(-3, copy),
        b"SCAN" => r(-2, |c, a| {
            let at = cursor(a[0])?;
            let opts = scan_opts(&a[1..], true, false)?;
            cmd_scan(c, at, opts.pattern, opts.count, opts.kind)
        }),
        b"TYPE" => r(2, |c, a| cmd_type(c, a[0])),
        b"RENAME" => w(3, |c, a| cmd_rename(c, a[0], a[1])),
        b"DBSIZE" => r(1, |c, _| cmd_dbsize(c)),
        // The ASYNC/SYNC modifier is accepted and has no effect. Anything
        // else must be refused: a mistyped argument must not flush.
        b"FLUSHALL" | b"FLUSHDB" => w(-1, |c, a| match a {
            [] => cmd_flushall(c),
            [mode] if is(*mode, "ASYNC") || is(*mode, "SYNC") => cmd_flushall(c),
            _ => Err(syntax_error()),
        }),
        _ => return None,
    })
}

fn arity_ok(arity: i32, argc: usize) -> bool {
    if arity >= 0 {
        argc == arity as usize
    } else {
        argc >= arity.unsigned_abs() as usize
    }
}

fn wrong_arity(name: &[u8]) -> CmdError {
    err(format!(
        "ERR wrong number of arguments for '{}' command",
        String::from_utf8_lossy(name).to_lowercase()
    ))
}

/// Redis's unknown-command error, which echoes the name and (a bounded
/// amount of) the arguments.
fn unknown_command(args: &[Vec<u8>]) -> CmdError {
    let clip = |b: &[u8]| -> String { String::from_utf8_lossy(b).chars().take(128).collect() };
    let mut msg = format!("ERR unknown command '{}'", clip(&args[0]));
    if args.len() > 1 {
        msg.push_str(", with args beginning with: ");
        for arg in &args[1..] {
            if msg.len() > 256 {
                break;
            }
            msg.push_str(&format!("'{}' ", clip(arg)));
        }
    }
    err(msg)
}

fn bulk(s: impl Into<Vec<u8>>) -> Reply {
    Reply::Bulk(s.into())
}

fn wrong_pass() -> CmdError {
    err("WRONGPASS invalid username-password pair or user is disabled.")
}

/// Compares a presented password with the expected one in time that depends
/// only on the length of what was presented: not on where the two differ,
/// and not on how long the expected password is, so response timing reveals
/// neither how much of a guess was right nor the password's length.
fn constant_time_eq(expected: &[u8], presented: &[u8]) -> bool {
    let mut diff = expected.len() ^ presented.len();
    for (i, byte) in presented.iter().enumerate() {
        // Cycles through the expected bytes; a length mismatch is already
        // recorded in `diff`, so what these compare against cannot rescue it.
        let other = expected
            .get(i % expected.len().max(1))
            .copied()
            .unwrap_or(0);
        diff |= usize::from(byte ^ other);
    }
    diff == 0
}

/// Checks a username and password as Redis does for its `default` user,
/// the only one there is. With no password configured that user accepts any
/// password.
fn check_credentials(shared: &Shared, user: &[u8], pass: &[u8]) -> Result<(), CmdError> {
    if user != b"default" {
        return Err(wrong_pass());
    }
    match &shared.password {
        Some(expected) if !constant_time_eq(expected, pass) => Err(wrong_pass()),
        _ => Ok(()),
    }
}

/// AUTH password | AUTH username password
fn auth(session: &mut Session, shared: &Shared, args: &[Arg<'_>]) -> CmdResult {
    match args {
        [pass] => {
            if shared.password.is_none() {
                return Err(err(
                    "ERR AUTH <password> called without any password configured for the \
                     default user. Are you sure your configuration is correct?",
                ));
            }
            check_credentials(shared, b"default", pass.as_bytes())?;
        }
        [user, pass] => check_credentials(shared, user.as_bytes(), pass.as_bytes())?,
        _ => return Err(syntax_error()),
    }
    // A failed AUTH above leaves an authenticated connection authenticated.
    session.authenticated = true;
    Ok(Reply::Simple("OK"))
}

/// HELLO [protover [AUTH username password] [SETNAME clientname]]
fn hello(session: &mut Session, shared: &Shared, args: &[Arg<'_>]) -> CmdResult {
    let mut proto = session.proto;
    let mut credentials = None;
    let mut name = None;
    if let Some(&version) = args.first() {
        proto = match int(version)
            .map_err(|_| err("ERR Protocol version is not an integer or out of range"))?
        {
            2 => Proto::Resp2,
            3 => Proto::Resp3,
            _ => return Err(err("NOPROTO unsupported protocol version")),
        };
        let mut rest = args[1..].iter().copied();
        while let Some(opt) = rest.next() {
            if is(opt, "AUTH") {
                let user = rest.next().ok_or_else(syntax_error)?;
                let pass = rest.next().ok_or_else(syntax_error)?;
                credentials = Some((user, pass));
            } else if is(opt, "SETNAME") {
                name = Some(client_name(rest.next().ok_or_else(syntax_error)?)?);
            } else {
                return Err(syntax_error());
            }
        }
    }
    // Everything is validated before anything changes, so a rejected HELLO
    // leaves the connection exactly as it was.
    match credentials {
        Some((user, pass)) => check_credentials(shared, user.as_bytes(), pass.as_bytes())?,
        None if !session.authenticated => {
            return Err(err(
                "NOAUTH HELLO must be called with the client already authenticated, otherwise \
                 the HELLO <proto> AUTH <user> <pass> option can be used to authenticate the \
                 client and select the RESP protocol version at the same time",
            ));
        }
        None => {}
    }
    session.authenticated = true;
    session.proto = proto;
    if let Some(name) = name {
        session.name = name;
    }
    let proto = match session.proto {
        Proto::Resp2 => 2,
        Proto::Resp3 => 3,
    };
    Ok(Reply::Map(vec![
        (bulk("server"), bulk("redis")),
        (bulk("version"), bulk(REDIS_COMPAT_VERSION)),
        (bulk("proto"), Reply::Int(proto)),
        (bulk("id"), Reply::Int(session.id as i64)),
        (bulk("mode"), bulk("standalone")),
        (bulk("role"), bulk("master")),
        (bulk("modules"), Reply::Lines(vec![])),
    ]))
}

/// Redis's rule for client names: printable ASCII without spaces (they are
/// shown in space-separated CLIENT LIST output). An empty name clears it.
fn client_name(name: Arg<'_>) -> Result<Option<Vec<u8>>, CmdError> {
    let bytes = name.as_bytes();
    if !bytes.iter().all(|b| (b'!'..=b'~').contains(b)) {
        return Err(err(
            "ERR Client names cannot contain spaces, newlines or special characters.",
        ));
    }
    Ok((!bytes.is_empty()).then(|| bytes.to_vec()))
}

fn client(session: &mut Session, args: &[Arg<'_>]) -> CmdResult {
    let sub = args[0];
    if is(sub, "SETNAME") && args.len() == 2 {
        session.name = client_name(args[1])?;
        Ok(Reply::Simple("OK"))
    } else if is(sub, "GETNAME") && args.len() == 1 {
        Ok(session.name.clone().map_or(Reply::Nil, Reply::Bulk))
    } else if is(sub, "ID") && args.len() == 1 {
        Ok(Reply::Int(session.id as i64))
    } else if is(sub, "SETINFO") && args.len() == 3 {
        Ok(Reply::Simple("OK"))
    } else {
        Err(err(format!(
            "ERR unknown subcommand '{}'. Try CLIENT HELP.",
            String::from_utf8_lossy(sub.as_bytes())
        )))
    }
}

fn info(conn: &Connection, shared: &Shared) -> CmdResult {
    let (keys, expires): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COUNT(expires_at) FROM keyspace
         WHERE expires_at IS NULL OR expires_at > ?1",
        [crate::db::now_ms()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut text = format!(
        "# Server\r\nredis_version:{REDIS_COMPAT_VERSION}\r\nklyv_version:{}\r\n\
         redis_mode:standalone\r\nprocess_id:{}\r\ntcp_port:{}\r\n\r\n\
         # Clients\r\nconnected_clients:{}\r\n\r\n# Keyspace\r\n",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        shared.tcp_port,
        shared.clients.load(Ordering::Relaxed),
    );
    if keys > 0 {
        text.push_str(&format!("db0:keys={keys},expires={expires},avg_ttl=0\r\n"));
    }
    Ok(Reply::Verbatim(text.into_bytes()))
}

enum Outcome {
    Reply(CmdResult),
    /// Reply, then close the connection (QUIT).
    Close(CmdResult),
}

/// Arity check for a connection command: `None` if `name` is not one.
fn connection_arity(name: &[u8], argc: usize) -> Option<Result<(), CmdError>> {
    let arity = match name {
        b"ECHO" | b"SELECT" => 2,
        b"CLIENT" | b"CONFIG" | b"AUTH" => -2,
        b"PING" | b"QUIT" | b"HELLO" | b"COMMAND" | b"INFO" => -1,
        _ => return None,
    };
    Some(if !arity_ok(arity, argc) || (name == b"PING" && argc > 2) {
        Err(wrong_arity(name))
    } else {
        Ok(())
    })
}

/// Commands about the connection rather than the data. `None` means the
/// command is not one of these.
fn connection_command(
    conn: &Connection,
    session: &mut Session,
    shared: &Shared,
    name: &[u8],
    args: &[Arg<'_>],
) -> Option<Outcome> {
    if let Err(e) = connection_arity(name, args.len() + 1)? {
        return Some(Outcome::Reply(Err(e)));
    }
    let result = match name {
        b"PING" => Ok(match args.first() {
            Some(msg) => bulk(msg.as_bytes()),
            None => Reply::Simple("PONG"),
        }),
        b"ECHO" => Ok(bulk(args[0].as_bytes())),
        b"QUIT" => return Some(Outcome::Close(Ok(Reply::Simple("OK")))),
        // There is one database, index 0.
        b"SELECT" => match int(args[0]) {
            Ok(0) => Ok(Reply::Simple("OK")),
            Ok(_) => Err(err("ERR DB index is out of range")),
            Err(e) => Err(e),
        },
        b"HELLO" => hello(session, shared, args),
        b"CLIENT" => client(session, args),
        b"COMMAND" => Ok(match args.first() {
            Some(sub) if is(*sub, "COUNT") => Reply::Int(DATA_COMMANDS.len() as i64),
            // COMMAND, COMMAND DOCS, COMMAND INFO: introspection is not
            // provided; an empty array is what clients treat as "unknown".
            _ => Reply::Array(vec![], Empty::List),
        }),
        b"CONFIG" => {
            if is(args[0], "GET") {
                // No Redis configuration parameters exist here; an empty
                // result is what Redis gives for an unknown parameter.
                Ok(Reply::Map(vec![]))
            } else {
                Err(err(format!(
                    "ERR unknown subcommand '{}'. Try CONFIG HELP.",
                    String::from_utf8_lossy(args[0].as_bytes())
                )))
            }
        }
        b"INFO" => info(conn, shared),
        b"AUTH" => auth(session, shared, args),
        _ => unreachable!("arity table and dispatch disagree"),
    };
    Some(Outcome::Reply(result))
}

/// Runs a data command in its own transaction, as the CLI's `dispatch` does.
fn data_command(
    conn: &mut Connection,
    name: &[u8],
    raw: &[Vec<u8>],
    args: &[Arg<'_>],
) -> CmdResult {
    let spec = lookup(name).ok_or_else(|| unknown_command(raw))?;
    if !arity_ok(spec.arity, args.len() + 1) {
        return Err(wrong_arity(name));
    }
    let behavior = if spec.write {
        TransactionBehavior::Immediate
    } else {
        TransactionBehavior::Deferred
    };
    let tx = conn.transaction_with_behavior(behavior)?;
    let reply = (spec.run)(&tx, args)?;
    tx.commit()?;
    Ok(reply)
}

/// A fingerprint of a key's whole state (type, expiry and contents), or
/// `None` if it does not exist. WATCH records one and EXEC compares.
///
/// Redis aborts a transaction if a watched key was *touched*; this aborts if
/// it *differs*. The two disagree only when a key is changed and changed
/// back in between, which no check-and-set logic can be hurt by, and
/// comparing state works across processes: the CLI or another server
/// writing to the same file is noticed too, with nothing extra stored.
fn fingerprint(conn: &Connection, key: Arg<'_>, now: i64) -> rusqlite::Result<Option<u64>> {
    use std::hash::{Hash, Hasher};
    let Some(row) = crate::db::lookup(conn, key)?.filter(|r| r.is_live(now)) else {
        return Ok(None);
    };
    // Canonical row orders, so equal contents always hash equally.
    let sql = match row.kind {
        "string" => "SELECT value FROM strings WHERE key = ?1",
        "list" => "SELECT value FROM list_items WHERE key = ?1 ORDER BY idx, rowid",
        "set" => "SELECT member FROM set_members WHERE key = ?1 ORDER BY member",
        _ => "SELECT field, value FROM hash_fields WHERE key = ?1 ORDER BY field",
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    row.kind.hash(&mut hasher);
    row.expires_at.hash(&mut hasher);
    let mut stmt = conn.prepare(sql)?;
    let columns = stmt.column_count();
    let mut rows = stmt.query([key])?;
    while let Some(r) = rows.next()? {
        for i in 0..columns {
            // Length-prefixed (slices hash their length), so ("ab", "c")
            // and ("a", "bc") differ.
            crate::db::bytes_col(r, i)?.hash(&mut hasher);
        }
    }
    Ok(Some(hasher.finish()))
}

/// WATCH key [key ...]
fn watch(conn: &mut Connection, session: &mut Session, keys: &[Arg<'_>]) -> CmdResult {
    // One snapshot for all the keys.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let now = crate::db::now_ms();
    for key in keys {
        // Watching a key again keeps the first fingerprint: what matters is
        // its state when it was first watched, and repeats must not grow the
        // list without bound.
        if session.watched.iter().any(|(k, _)| k == key.as_bytes()) {
            continue;
        }
        let state = fingerprint(&tx, *key, now)?;
        session.watched.push((key.as_bytes().to_vec(), state));
    }
    tx.commit()?;
    Ok(Reply::Simple("OK"))
}

/// Checks a command when it is queued inside MULTI, so that an unknown
/// command or a wrong argument count is reported at once (and aborts the
/// EXEC), as in Redis. Errors that depend on the data surface at EXEC.
fn check_queueable(name: &[u8], raw: &[Vec<u8>]) -> Result<(), CmdError> {
    if name == b"UNWATCH" {
        return if raw.len() == 1 {
            Ok(())
        } else {
            Err(wrong_arity(name))
        };
    }
    if let Some(checked) = connection_arity(name, raw.len()) {
        return checked;
    }
    let spec = lookup(name).ok_or_else(|| unknown_command(raw))?;
    if !arity_ok(spec.arity, raw.len()) {
        return Err(wrong_arity(name));
    }
    Ok(())
}

/// EXEC: run the queued commands as one atomic unit.
///
/// The whole queue runs inside a single SQLite write transaction, so no
/// other connection or process sees it half done. Within it each command
/// gets a savepoint: Redis does not roll a transaction back when one command
/// fails, so a failing command is undone alone, its error takes its place in
/// the reply, and the rest still run.
fn exec(conn: &mut Connection, session: &mut Session, shared: &Shared) -> CmdResult {
    let Some(queue) = session.multi.take() else {
        return Err(err("ERR EXEC without MULTI"));
    };
    let watched = std::mem::take(&mut session.watched);
    if std::mem::take(&mut session.multi_error) {
        return Err(err(
            "EXECABORT Transaction discarded because of previous errors.",
        ));
    }
    let mut tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = crate::db::now_ms();
    for (key, state) in &watched {
        if fingerprint(&tx, Arg(key), now)? != *state {
            return Ok(Reply::NullArray);
        }
    }
    let mut replies = Vec::with_capacity(queue.len());
    for raw in &queue {
        let name = raw[0].to_ascii_uppercase();
        let args: Vec<Arg<'_>> = raw[1..].iter().map(|a| Arg(a)).collect();
        let result = if name == b"UNWATCH" {
            Ok(Reply::Simple("OK"))
        } else if let Some(outcome) = connection_command(&tx, session, shared, &name, &args) {
            match outcome {
                Outcome::Reply(result) | Outcome::Close(result) => result,
            }
        } else {
            // Queueing verified that the command exists and its arity.
            let spec = lookup(&name).ok_or_else(|| unknown_command(raw))?;
            let savepoint = tx.savepoint()?;
            match (spec.run)(&savepoint, &args) {
                Ok(reply) => savepoint.commit().map(|()| reply).map_err(CmdError::from),
                // Dropping the savepoint rolls back this command alone.
                Err(e) => Err(e),
            }
        };
        replies.push(result.unwrap_or_else(|CmdError(msg)| Reply::Error(msg)));
    }
    tx.commit()?;
    Ok(Reply::Lines(replies))
}

/// MULTI, EXEC, DISCARD and WATCH, and the queueing of everything else while
/// a MULTI is open. `None` means the command should run normally.
fn transaction_command(
    conn: &mut Connection,
    session: &mut Session,
    shared: &Shared,
    name: &[u8],
    raw: &[Vec<u8>],
    args: &[Arg<'_>],
) -> Option<CmdResult> {
    // Arity first: `MULTI x` must not open a transaction on its way to
    // being rejected.
    if matches!(name, b"MULTI" | b"EXEC" | b"DISCARD") && !args.is_empty() {
        // Like any other command rejected inside MULTI, a malformed one
        // spoils the transaction. A malformed EXEC is still an EXEC: Redis
        // ends the transaction there and says why.
        if session.multi.is_some() {
            if name == b"EXEC" {
                session.multi = None;
                session.multi_error = false;
                session.watched.clear();
                return Some(Err(err(
                    "EXECABORT Transaction discarded because of: wrong number of arguments \
                     for 'exec' command",
                )));
            }
            session.multi_error = true;
        }
        return Some(Err(wrong_arity(name)));
    }
    Some(match name {
        b"MULTI" if session.multi.is_some() => Err(err("ERR MULTI calls can not be nested")),
        b"MULTI" => {
            session.multi = Some(Vec::new());
            session.multi_error = false;
            Ok(Reply::Simple("OK"))
        }
        b"EXEC" => exec(conn, session, shared),
        b"DISCARD" if session.multi.take().is_some() => {
            session.multi_error = false;
            session.watched.clear();
            Ok(Reply::Simple("OK"))
        }
        b"DISCARD" => Err(err("ERR DISCARD without MULTI")),
        // Arity is checked first, and like any arity error inside MULTI it
        // spoils the transaction.
        b"WATCH" if args.is_empty() => {
            session.multi_error |= session.multi.is_some();
            Err(wrong_arity(name))
        }
        // Refused, but (as in Redis) without spoiling the transaction.
        b"WATCH" if session.multi.is_some() => Err(err("ERR WATCH inside MULTI is not allowed")),
        b"WATCH" => watch(conn, session, args),
        // QUIT ends the connection whether or not a MULTI is open.
        b"QUIT" => return None,
        _ => match &mut session.multi {
            Some(queue) => match check_queueable(name, raw) {
                Ok(()) => {
                    queue.push(raw.to_vec());
                    Ok(Reply::Simple("QUEUED"))
                }
                Err(e) => {
                    session.multi_error = true;
                    Err(e)
                }
            },
            None if name == b"UNWATCH" && !args.is_empty() => Err(wrong_arity(name)),
            None if name == b"UNWATCH" => {
                session.watched.clear();
                Ok(Reply::Simple("OK"))
            }
            None => return None,
        },
    })
}

/// Executes one client command and appends its encoded reply to `out`.
/// Returns true if the connection should then be closed.
fn execute(
    conn: &mut Connection,
    session: &mut Session,
    shared: &Shared,
    raw: &[Vec<u8>],
    out: &mut Vec<u8>,
) -> bool {
    let name = raw[0].to_ascii_uppercase();
    let args: Vec<Arg<'_>> = raw[1..].iter().map(|a| Arg(a)).collect();
    // Until the password has been presented, only the commands that can
    // present it (or leave) are available.
    if !session.authenticated && !matches!(name.as_slice(), b"AUTH" | b"HELLO" | b"QUIT") {
        encode_error("NOAUTH Authentication required.", out);
        return false;
    }
    let (result, close) =
        if let Some(result) = transaction_command(conn, session, shared, &name, raw, &args) {
            (result, false)
        } else {
            match connection_command(conn, session, shared, &name, &args) {
                Some(Outcome::Reply(result)) => (result, false),
                Some(Outcome::Close(result)) => (result, true),
                None => (data_command(conn, &name, raw, &args), false),
            }
        };
    match result {
        Ok(reply) => encode(&reply, session.proto, out),
        Err(CmdError(msg)) => encode_error(&msg, out),
    }
    close
}

/// The read half of a connection, flushing pending replies before it blocks.
///
/// Replies to pipelined commands are batched in the writer's buffer, and
/// must be sent before the server waits for more input: otherwise a client
/// that has sent one whole command and part of the next would wait for a
/// reply that is sitting unsent while the server waits for the rest. Sitting
/// under the `BufReader`, this is consulted exactly when buffered input has
/// run out, which is the only time the server can block.
struct FlushBeforeRead<'a, R, W: Write> {
    inner: R,
    writer: &'a RefCell<BufWriter<W>>,
}

impl<R: Read, W: Write> Read for FlushBeforeRead<'_, R, W> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.writer.borrow_mut().flush()?;
        self.inner.read(buf)
    }
}

/// Serves one client until it disconnects, quits or breaks the protocol.
fn serve_connection(reader: impl Read, writer: impl Write, shared: &Shared, admitted: bool) {
    let writer = RefCell::new(BufWriter::new(writer));
    let send = |bytes: &[u8]| writer.borrow_mut().write_all(bytes);
    let fail = |msg: &str| {
        let mut out = Vec::new();
        encode_error(msg, &mut out);
        let _ = send(&out);
    };
    let mut reader = BufReader::new(FlushBeforeRead {
        inner: reader,
        writer: &writer,
    });
    // Redis's reply to a client over the limit, sent before closing.
    if !admitted {
        fail("ERR max number of clients reached");
        let _ = writer.borrow_mut().flush();
        return;
    }
    let mut conn = match open_db(&shared.db) {
        Ok(conn) => conn,
        Err(e) => {
            // The detail (paths, SQLite and schema diagnostics) goes to the
            // server's log, not to a client that has not even authenticated.
            eprintln!("error: cannot open the database for a client: {e}");
            fail("ERR the server cannot open its database");
            let _ = writer.borrow_mut().flush();
            return;
        }
    };
    let mut session = Session {
        id: shared.next_id.fetch_add(1, Ordering::Relaxed),
        name: None,
        proto: Proto::Resp2,
        authenticated: shared.password.is_none(),
        multi: None,
        multi_error: false,
        watched: Vec::new(),
    };
    let mut out = Vec::new();
    loop {
        let raw = match read_command(&mut reader) {
            Ok(Some(raw)) => raw,
            Ok(None) | Err(ReadError::Closed) => break,
            Err(ReadError::Protocol(msg)) => {
                fail(&format!("ERR {msg}"));
                break;
            }
        };
        out.clear();
        let close = execute(&mut conn, &mut session, shared, &raw, &mut out);
        if send(&out).is_err() || close {
            break;
        }
    }
    let _ = writer.borrow_mut().flush();
}

/// Runs a client on its own thread. The stream is shared by reference
/// between the read and write halves rather than duplicated, which saves a
/// file descriptor per client.
fn spawn_connection<S>(stream: S, shared: &Arc<Shared>)
where
    S: Send + 'static,
    for<'a> &'a S: Read + Write,
{
    let shared = Arc::clone(shared);
    // If the thread cannot be created (resource exhaustion) the stream is
    // dropped, which closes the connection.
    let _ = std::thread::Builder::new()
        .name("klyv-client".into())
        .spawn(move || {
            // Counted down on drop, so the count stays right even if a
            // command panics and unwinds this thread.
            struct Connected<'a>(&'a AtomicUsize);
            impl Drop for Connected<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::Relaxed);
                }
            }
            // Count and decide in one atomic step: with a separate load,
            // two connections racing for the last slot could each see the
            // other's increment and both be turned away.
            let count = shared.clients.fetch_add(1, Ordering::Relaxed) + 1;
            let _connected = Connected(&shared.clients);
            serve_connection(&stream, &stream, &shared, count <= shared.max_clients);
        });
}

fn accept_tcp(listener: TcpListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // Replies are small and latency-sensitive.
                let _ = stream.set_nodelay(true);
                spawn_connection::<TcpStream>(stream, &shared);
            }
            // Typically out of file descriptors: back off instead of spinning.
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};

    /// Binds the socket, replacing a stale socket file left by a server that
    /// did not shut down cleanly, but never one that is still being served
    /// and never a file that is not a socket. Only "connection refused"
    /// counts as stale: any other failure to connect (permissions, resource
    /// limits) proves nothing, and the file is left alone.
    pub(super) fn bind(
        path: &Path,
        perm: Option<&str>,
    ) -> Result<(UnixListener, Option<(u64, u64)>), String> {
        let mode = match perm {
            Some(perm) => Some(
                u32::from_str_radix(perm, 8)
                    .ok()
                    .filter(|m| *m <= 0o777)
                    .ok_or_else(|| {
                        format!("invalid --unixsocketperm '{perm}' (octal, e.g. 700)")
                    })?,
            ),
            None => None,
        };
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if !meta.file_type().is_socket() {
                return Err(format!("{} exists and is not a socket", path.display()));
            }
            match UnixStream::connect(path) {
                Ok(_) => return Err(format!("{} is already being served", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                Err(e) => {
                    return Err(format!(
                        "{} exists and could not be checked for a live server: {e}",
                        path.display()
                    ));
                }
            }
        }
        let listener = UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Taken at once, so it describes the file this bind created.
        let created = identity(path);
        if let Some(mode) = mode {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok((listener, created))
    }

    /// Identifies the socket file this process created, so that shutdown
    /// removes it only if the path still refers to it.
    pub(super) fn identity(path: &Path) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some((meta.dev(), meta.ino()))
    }

    pub(super) fn accept(listener: UnixListener, shared: Arc<Shared>) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => spawn_connection::<UnixStream>(stream, &shared),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }
}

/// File descriptors each client connection holds: its socket, and SQLite's
/// database, WAL and shared-memory files.
#[cfg(unix)]
const FDS_PER_CLIENT: u64 = 4;
/// Descriptors kept back for everything else: listeners, stdio, the files
/// SQLite opens briefly, and headroom.
#[cfg(unix)]
const FDS_RESERVED: u64 = 32;

/// Raises the open-file limit as far as needed for `wanted` clients, as far
/// as the system allows, and returns how many clients the result supports.
/// The default soft limit is often far too low for a server (256 on macOS
/// allows about 56 clients); the hard limit is usually much higher and a
/// process may raise its own soft limit up to it. If even that is not
/// enough the client limit is lowered to fit, with a notice, rather than
/// have connections fail unpredictably once descriptors run out.
#[cfg(unix)]
// rlim_t is u64 on the common platforms but not on all of them.
#[allow(clippy::unnecessary_cast)]
fn usable_clients(wanted: usize) -> usize {
    let needed = (wanted as u64)
        .saturating_mul(FDS_PER_CLIENT)
        .saturating_add(FDS_RESERVED);
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one rlimit struct through a valid pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return wanted;
    }
    let ceiling = limit.rlim_max as u64;
    // macOS refuses a soft limit above OPEN_MAX (10240, from
    // <sys/syslimits.h>) even when it reports the hard limit as unlimited.
    #[cfg(target_os = "macos")]
    let ceiling = ceiling.min(10_240);
    let mut current = limit.rlim_cur as u64;
    if current < needed && current < ceiling {
        let target = needed.min(ceiling);
        let raised = libc::rlimit {
            rlim_cur: target as libc::rlim_t,
            rlim_max: limit.rlim_max,
        };
        // SAFETY: setrlimit reads one rlimit struct through a valid pointer.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            current = target;
        }
    }
    if current >= needed {
        return wanted;
    }
    let usable = (current.saturating_sub(FDS_RESERVED) / FDS_PER_CLIENT).max(1) as usize;
    eprintln!(
        "notice: the open-file limit ({current}) allows about {usable} clients, not the \
         {wanted} requested with --maxclients; raise it with ulimit -n to serve more"
    );
    usable
}

#[cfg(not(unix))]
fn usable_clients(wanted: usize) -> usize {
    wanted
}

/// Runs the server until the process is terminated. Returns an exit code
/// only on a startup failure.
pub(crate) fn serve(db: &Path, args: ServeArgs) -> i32 {
    match start(db, args) {
        Ok(handles) => {
            for handle in handles {
                let _ = handle.join();
            }
            0
        }
        Err(msg) => {
            eprintln!("ERR {msg}");
            1
        }
    }
}

fn start(db: &Path, args: ServeArgs) -> Result<Vec<std::thread::JoinHandle<()>>, String> {
    let mut binds = args.bind;
    if binds.is_empty() && args.unixsocket.is_none() {
        binds.push("127.0.0.1:6379".into());
    }
    #[cfg(not(unix))]
    if args.unixsocket.is_some() {
        return Err("--unixsocket is only available on Unix".into());
    }

    let mut tcp = Vec::new();
    for addr in &binds {
        let listener = TcpListener::bind(addr).map_err(|e| format!("cannot bind {addr}: {e}"))?;
        tcp.push(listener);
    }
    #[cfg(unix)]
    let unix_listener = match &args.unixsocket {
        Some(path) => Some(unix::bind(path, args.unixsocketperm.as_deref())?),
        None => None,
    };

    let max_clients = usable_clients(args.maxclients);
    let password = args.requirepass.clone().map(String::into_bytes);
    if password.as_ref().is_some_and(Vec::is_empty) {
        return Err("--requirepass must not be empty".into());
    }
    let has_password = password.is_some();
    let shared = Arc::new(Shared {
        db: db.to_path_buf(),
        password,
        next_id: AtomicU64::new(1),
        clients: AtomicUsize::new(0),
        max_clients,
        tcp_port: tcp
            .first()
            .and_then(|l| l.local_addr().ok())
            .map_or(0, |a| a.port()),
    });

    // Remove the socket file on Ctrl-C / SIGTERM. Nothing else needs an
    // orderly shutdown: every command is its own committed transaction.
    #[cfg(unix)]
    let socket = unix_listener
        .as_ref()
        .zip(args.unixsocket.clone())
        .map(|((_, created), path)| (*created, path));
    ctrlc::set_handler(move || {
        // Only if the path is still the socket this process bound: another
        // server may have replaced it since.
        #[cfg(unix)]
        if let Some((created, path)) = &socket
            && created.is_some()
            && unix::identity(path) == *created
        {
            let _ = std::fs::remove_file(path);
        }
        std::process::exit(0);
    })
    .map_err(|e| format!("cannot install signal handler: {e}"))?;

    // Announce on stdout, one line per listener, so a supervisor (or a test)
    // can learn the actual address when port 0 was requested.
    let mut out = std::io::stdout().lock();
    let mut handles = Vec::new();
    for listener in tcp {
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        if !addr.ip().is_loopback() {
            if has_password {
                eprintln!(
                    "warning: listening on {addr}, which is not a loopback address; there is \
                     no TLS, so the password and all data cross the network unencrypted"
                );
            } else {
                eprintln!(
                    "warning: listening on {addr}, which is not a loopback address, with no \
                     password (--requirepass): anyone who can reach it can read and change \
                     the database"
                );
            }
        }
        let _ = writeln!(out, "listening on tcp://{addr}");
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || accept_tcp(listener, shared)));
    }
    #[cfg(unix)]
    if let (Some((listener, _)), Some(path)) = (unix_listener, &args.unixsocket) {
        let _ = writeln!(out, "listening on unix://{}", path.display());
        let shared = Arc::clone(&shared);
        handles.push(std::thread::spawn(move || unix::accept(listener, shared)));
    }
    let _ = out.flush();
    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_command_is_in_the_table() {
        for name in DATA_COMMANDS {
            assert!(lookup(name.as_bytes()).is_some(), "{name} is not routed");
        }
        assert!(lookup(b"NOSUCH").is_none());
    }

    #[test]
    fn strict_integer_syntax() {
        let ok = |s: &str| int(Arg(s.as_bytes())).ok();
        assert_eq!(ok("0"), Some(0));
        assert_eq!(ok("-5"), Some(-5));
        assert_eq!(ok("9223372036854775807"), Some(i64::MAX));
        assert_eq!(ok("-9223372036854775808"), Some(i64::MIN));
        for bad in [
            "",
            "-",
            "+5",
            "05",
            "-0",
            " 5",
            "5 ",
            "1.0",
            "9223372036854775808",
            "abc",
        ] {
            assert_eq!(ok(bad), None, "{bad:?} should be rejected");
        }
    }

    #[test]
    fn arity_rules() {
        assert!(arity_ok(2, 2));
        assert!(!arity_ok(2, 3));
        assert!(arity_ok(-2, 2));
        assert!(arity_ok(-2, 9));
        assert!(!arity_ok(-2, 1));
    }
}

#[cfg(test)]
mod auth_tests {
    use super::constant_time_eq;

    #[test]
    fn password_comparison() {
        assert!(constant_time_eq(b"s3cret", b"s3cret"));
        assert!(constant_time_eq(b"", b""));
        for wrong in [
            &b""[..],
            b"s",
            b"s3cre",
            b"s3cret!",
            b"S3cret",
            b"s3crets3cret",
            b"s3cret\0",
            b"xxxxxx",
        ] {
            assert!(!constant_time_eq(b"s3cret", wrong), "{wrong:?} accepted");
        }
        // An empty expected password matches nothing but the empty one.
        assert!(!constant_time_eq(b"", b"x"));
        assert!(!constant_time_eq(b"", b"\0"));
    }
}
