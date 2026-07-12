//! Differential harness: runs identical command sequences through klyv and a
//! real Redis server, comparing replies. The cheapest way to catch
//! Redis-compatibility regressions.
//!
//! Requires `redis-server` and `redis-cli` on PATH; the test skips (passes
//! with a notice) when they are absent. An ephemeral, persistence-free Redis
//! is started on a free localhost port and killed when the test ends.
//!
//! Comparison notes, verified empirically against redis-cli 8.x:
//! - redis-cli in a pipe uses raw formatting, matching `klyv --format raw`,
//!   except an empty array prints a bare newline where klyv prints nothing.
//! - redis-cli prints error replies to stdout and exits 0; klyv prints them
//!   to stderr and exits 1. Error steps therefore compare only the error
//!   code (first token, e.g. WRONGTYPE/ERR) — full messages legitimately
//!   differ (e.g. Redis appends "or out of range" to integer-parse errors).
//! - Unordered replies (sets, keys, hashes) are sorted on both sides.
//!
//! Deliberate klyv deviations documented in SPEC.md are not exercised here:
//! SET preserving a live TTL, db-size counting unpurged expired keys, purge
//! itself, p-expire second-rounding, and `[abc]` glob classes in keys.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct RedisGuard {
    child: Child,
    port: u16,
}

impl Drop for RedisGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_redis() -> RedisGuard {
    let port = free_port();
    let child = Command::new("redis-server")
        .args([
            "--port",
            &port.to_string(),
            "--bind",
            "127.0.0.1",
            "--save",
            "",
            "--appendonly",
            "no",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn redis-server");
    let guard = RedisGuard { child, port };
    for _ in 0..100 {
        let out = Command::new("redis-cli")
            .args(["-p", &guard.port.to_string(), "PING"])
            .output()
            .expect("failed to run redis-cli");
        if String::from_utf8_lossy(&out.stdout).trim() == "PONG" {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("redis-server did not become ready");
}

/// (stdout, stderr, success)
fn run_klyv(db: &str, args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_klyv"))
        .args(["--db", db, "--format", "raw"])
        .args(args)
        .output()
        .expect("failed to run klyv");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.success(),
    )
}

fn run_redis(port: u16, args: &[&str]) -> String {
    let output = Command::new("redis-cli")
        .args(["-p", &port.to_string()])
        .args(args)
        .output()
        .expect("failed to run redis-cli");
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[derive(Clone, Copy)]
enum Cmp {
    /// Byte-equal stdout (with the empty-array relaxation).
    Exact,
    /// Line order is irrelevant (set members, keys).
    Sorted,
    /// Alternating field/value lines; pairs compared order-insensitively.
    PairSorted,
    /// Both sides must report an error with the same code (first token).
    Error,
}

struct Step {
    klyv: &'static [&'static str],
    redis: &'static [&'static str],
    cmp: Cmp,
}

fn step(klyv: &'static [&'static str], redis: &'static [&'static str], cmp: Cmp) -> Step {
    Step { klyv, redis, cmp }
}

fn sorted_lines(s: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
    lines.sort_unstable();
    lines
}

fn sorted_pairs(s: &str) -> Vec<(&str, &str)> {
    let lines: Vec<&str> = s.lines().filter(|l| !l.is_empty()).collect();
    let mut pairs: Vec<(&str, &str)> = lines
        .chunks(2)
        .map(|c| (c[0], *c.get(1).unwrap_or(&"")))
        .collect();
    pairs.sort_unstable();
    pairs
}

fn first_token(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

fn check_step(db: &str, port: u16, scenario: &str, i: usize, s: &Step) {
    let (k_out, k_err, k_ok) = run_klyv(db, s.klyv);
    let r_out = run_redis(port, s.redis);
    let ctx = format!(
        "scenario '{scenario}' step {i}: klyv {:?} vs redis {:?}\n  klyv stdout: {k_out:?}\n  klyv stderr: {k_err:?}\n  redis stdout: {r_out:?}",
        s.klyv, s.redis
    );
    match s.cmp {
        Cmp::Exact => {
            assert!(k_ok, "klyv errored unexpectedly\n{ctx}");
            // redis-cli prints a bare newline for an empty array; klyv raw
            // prints nothing.
            let equal = k_out == r_out || (r_out == "\n" && k_out.is_empty());
            assert!(equal, "output mismatch\n{ctx}");
        }
        Cmp::Sorted => {
            assert!(k_ok, "klyv errored unexpectedly\n{ctx}");
            assert_eq!(
                sorted_lines(&k_out),
                sorted_lines(&r_out),
                "sorted mismatch\n{ctx}"
            );
        }
        Cmp::PairSorted => {
            assert!(k_ok, "klyv errored unexpectedly\n{ctx}");
            assert_eq!(
                sorted_pairs(&k_out),
                sorted_pairs(&r_out),
                "pair mismatch\n{ctx}"
            );
        }
        Cmp::Error => {
            assert!(!k_ok, "klyv should have errored\n{ctx}");
            let k_code = first_token(&k_err);
            let r_code = first_token(&r_out);
            assert!(!k_code.is_empty(), "empty klyv error\n{ctx}");
            assert_eq!(k_code, r_code, "error code mismatch\n{ctx}");
        }
    }
}

fn scenarios() -> Vec<(&'static str, Vec<Step>)> {
    use Cmp::*;
    vec![
        (
            "strings",
            vec![
                step(&["set", "k", "v"], &["SET", "k", "v"], Exact),
                step(&["get", "k"], &["GET", "k"], Exact),
                step(&["get", "missing"], &["GET", "missing"], Exact),
                step(&["set", "k", "v2"], &["SET", "k", "v2"], Exact),
                step(&["get", "k"], &["GET", "k"], Exact),
                step(
                    &["set", "sp", "hello world"],
                    &["SET", "sp", "hello world"],
                    Exact,
                ),
                step(&["get", "sp"], &["GET", "sp"], Exact),
                // Hyphen-leading values need the standard `--` escape in klyv.
                step(
                    &["append", "--", "k", "-more"],
                    &["APPEND", "k", "-more"],
                    Exact,
                ),
                step(
                    &["append", "fresh", "abc"],
                    &["APPEND", "fresh", "abc"],
                    Exact,
                ),
                step(&["strlen", "k"], &["STRLEN", "k"], Exact),
                step(&["strlen", "missing"], &["STRLEN", "missing"], Exact),
                step(&["exists", "k"], &["EXISTS", "k"], Exact),
                step(&["exists", "missing"], &["EXISTS", "missing"], Exact),
                step(&["type", "k"], &["TYPE", "k"], Exact),
                step(&["type", "missing"], &["TYPE", "missing"], Exact),
            ],
        ),
        (
            "counters",
            vec![
                step(&["incr", "n"], &["INCR", "n"], Exact),
                step(&["incr", "n"], &["INCR", "n"], Exact),
                step(&["decr", "n"], &["DECR", "n"], Exact),
                step(&["incr-by", "n", "40"], &["INCRBY", "n", "40"], Exact),
                step(&["decr-by", "n", "-2"], &["DECRBY", "n", "-2"], Exact),
                step(&["incr-by", "n", "-100"], &["INCRBY", "n", "-100"], Exact),
                step(
                    &["set", "s", "notanumber"],
                    &["SET", "s", "notanumber"],
                    Exact,
                ),
                step(&["incr", "s"], &["INCR", "s"], Error),
                step(&["get", "n"], &["GET", "n"], Exact),
            ],
        ),
        (
            "mset-mget-del",
            vec![
                step(
                    &["m-set", "a", "1", "b", "2", "c", "3"],
                    &["MSET", "a", "1", "b", "2", "c", "3"],
                    Exact,
                ),
                step(
                    &["m-get", "a", "missing", "b", "c"],
                    &["MGET", "a", "missing", "b", "c"],
                    Exact,
                ),
                step(&["del", "a"], &["DEL", "a"], Exact),
                step(
                    &["del", "b", "c", "missing"],
                    &["DEL", "b", "c", "missing"],
                    Exact,
                ),
                step(&["del", "missing"], &["DEL", "missing"], Exact),
                step(
                    &["r-push", "biglist", "x", "y", "z", "w"],
                    &["RPUSH", "biglist", "x", "y", "z", "w"],
                    Exact,
                ),
                // DEL of a multi-element list must count 1 key, not 4 rows.
                step(&["del", "biglist"], &["DEL", "biglist"], Exact),
                step(&["m-get", "l"], &["MGET", "l"], Exact),
            ],
        ),
        (
            "set-options",
            vec![
                step(
                    &["set", "k", "first", "--nx"],
                    &["SET", "k", "first", "NX"],
                    Exact,
                ),
                step(
                    &["set", "k", "second", "--nx"],
                    &["SET", "k", "second", "NX"],
                    Exact,
                ),
                step(&["get", "k"], &["GET", "k"], Exact),
                step(&["get-del", "k"], &["GETDEL", "k"], Exact),
                step(&["get", "k"], &["GET", "k"], Exact),
                step(&["get-del", "missing"], &["GETDEL", "missing"], Exact),
                step(&["r-push", "l", "a"], &["RPUSH", "l", "a"], Exact),
                step(&["get-del", "l"], &["GETDEL", "l"], Error),
            ],
        ),
        (
            "lists",
            vec![
                step(
                    &["r-push", "l", "a", "b", "c"],
                    &["RPUSH", "l", "a", "b", "c"],
                    Exact,
                ),
                step(&["l-push", "l", "z", "y"], &["LPUSH", "l", "z", "y"], Exact),
                step(&["l-len", "l"], &["LLEN", "l"], Exact),
                step(
                    &["l-range", "l", "0", "-1"],
                    &["LRANGE", "l", "0", "-1"],
                    Exact,
                ),
                step(
                    &["l-range", "l", "1", "3"],
                    &["LRANGE", "l", "1", "3"],
                    Exact,
                ),
                step(
                    &["l-range", "l", "-2", "-1"],
                    &["LRANGE", "l", "-2", "-1"],
                    Exact,
                ),
                step(
                    &["l-range", "l", "-100", "100"],
                    &["LRANGE", "l", "-100", "100"],
                    Exact,
                ),
                step(
                    &["l-range", "l", "3", "1"],
                    &["LRANGE", "l", "3", "1"],
                    Exact,
                ),
                // Still-negative stop after normalization: empty, not clamped.
                step(
                    &["l-range", "l", "0", "-100"],
                    &["LRANGE", "l", "0", "-100"],
                    Exact,
                ),
                step(
                    &["l-range", "missing", "0", "-1"],
                    &["LRANGE", "missing", "0", "-1"],
                    Exact,
                ),
                step(&["l-pop", "l"], &["LPOP", "l"], Exact),
                step(&["r-pop", "l"], &["RPOP", "l"], Exact),
                step(
                    &["l-range", "l", "0", "-1"],
                    &["LRANGE", "l", "0", "-1"],
                    Exact,
                ),
                step(&["l-pop", "missing"], &["LPOP", "missing"], Exact),
                step(&["l-pos", "l", "b"], &["LPOS", "l", "b"], Exact),
                step(&["l-pos", "l", "zz"], &["LPOS", "l", "zz"], Exact),
                step(&["l-index", "l", "0"], &["LINDEX", "l", "0"], Exact),
                step(&["l-index", "l", "-1"], &["LINDEX", "l", "-1"], Exact),
                step(&["l-index", "l", "99"], &["LINDEX", "l", "99"], Exact),
                step(
                    &["l-index", "missing", "0"],
                    &["LINDEX", "missing", "0"],
                    Exact,
                ),
            ],
        ),
        (
            "list-mutation",
            vec![
                step(
                    &["r-push", "l", "a", "b", "a", "c", "a", "b"],
                    &["RPUSH", "l", "a", "b", "a", "c", "a", "b"],
                    Exact,
                ),
                step(&["l-rem", "l", "1", "a"], &["LREM", "l", "1", "a"], Exact),
                step(
                    &["l-range", "l", "0", "-1"],
                    &["LRANGE", "l", "0", "-1"],
                    Exact,
                ),
                step(&["l-rem", "l", "-1", "b"], &["LREM", "l", "-1", "b"], Exact),
                step(
                    &["l-range", "l", "0", "-1"],
                    &["LRANGE", "l", "0", "-1"],
                    Exact,
                ),
                step(&["l-rem", "l", "0", "a"], &["LREM", "l", "0", "a"], Exact),
                step(
                    &["l-range", "l", "0", "-1"],
                    &["LRANGE", "l", "0", "-1"],
                    Exact,
                ),
                step(&["l-set", "l", "0", "B"], &["LSET", "l", "0", "B"], Exact),
                step(&["l-set", "l", "-1", "C"], &["LSET", "l", "-1", "C"], Exact),
                step(
                    &["l-range", "l", "0", "-1"],
                    &["LRANGE", "l", "0", "-1"],
                    Exact,
                ),
                step(&["l-set", "l", "99", "x"], &["LSET", "l", "99", "x"], Error),
                step(
                    &["l-set", "missing", "0", "x"],
                    &["LSET", "missing", "0", "x"],
                    Error,
                ),
                step(
                    &["l-insert", "l", "before", "C", "mid"],
                    &["LINSERT", "l", "BEFORE", "C", "mid"],
                    Exact,
                ),
                step(
                    &["l-insert", "l", "after", "B", "tail"],
                    &["LINSERT", "l", "AFTER", "B", "tail"],
                    Exact,
                ),
                step(
                    &["l-range", "l", "0", "-1"],
                    &["LRANGE", "l", "0", "-1"],
                    Exact,
                ),
                step(
                    &["l-insert", "l", "before", "zz", "x"],
                    &["LINSERT", "l", "BEFORE", "zz", "x"],
                    Exact,
                ),
                step(
                    &["l-insert", "missing", "before", "a", "x"],
                    &["LINSERT", "missing", "BEFORE", "a", "x"],
                    Exact,
                ),
                step(
                    &["r-push", "t", "a", "b", "c", "d", "e"],
                    &["RPUSH", "t", "a", "b", "c", "d", "e"],
                    Exact,
                ),
                step(
                    &["l-trim", "t", "1", "-2"],
                    &["LTRIM", "t", "1", "-2"],
                    Exact,
                ),
                step(
                    &["l-range", "t", "0", "-1"],
                    &["LRANGE", "t", "0", "-1"],
                    Exact,
                ),
                step(
                    &["l-trim", "t", "0", "-100"],
                    &["LTRIM", "t", "0", "-100"],
                    Exact,
                ),
                step(&["exists", "t"], &["EXISTS", "t"], Exact),
            ],
        ),
        (
            "sets",
            vec![
                step(
                    &["s-add", "s", "a", "b", "c", "a"],
                    &["SADD", "s", "a", "b", "c", "a"],
                    Exact,
                ),
                step(&["s-add", "s", "a"], &["SADD", "s", "a"], Exact),
                step(&["s-card", "s"], &["SCARD", "s"], Exact),
                step(&["s-is-member", "s", "a"], &["SISMEMBER", "s", "a"], Exact),
                step(
                    &["s-is-member", "s", "zz"],
                    &["SISMEMBER", "s", "zz"],
                    Exact,
                ),
                step(&["s-members", "s"], &["SMEMBERS", "s"], Sorted),
                step(&["s-members", "missing"], &["SMEMBERS", "missing"], Sorted),
                step(&["s-rem", "s", "a", "zz"], &["SREM", "s", "a", "zz"], Exact),
                step(&["s-card", "s"], &["SCARD", "s"], Exact),
                step(&["s-add", "s2", "b", "d"], &["SADD", "s2", "b", "d"], Exact),
                step(&["s-union", "s", "s2"], &["SUNION", "s", "s2"], Sorted),
                step(&["s-inter", "s", "s2"], &["SINTER", "s", "s2"], Sorted),
                step(&["s-diff", "s", "s2"], &["SDIFF", "s", "s2"], Sorted),
                step(&["s-diff", "s2", "s"], &["SDIFF", "s2", "s"], Sorted),
                step(
                    &["s-union", "s", "missing"],
                    &["SUNION", "s", "missing"],
                    Sorted,
                ),
                step(
                    &["s-inter", "s", "missing"],
                    &["SINTER", "s", "missing"],
                    Sorted,
                ),
                step(
                    &["s-diff", "s", "missing"],
                    &["SDIFF", "s", "missing"],
                    Sorted,
                ),
            ],
        ),
        (
            "hashes",
            vec![
                step(
                    &["h-set", "h", "f1", "v1", "f2", "v2"],
                    &["HSET", "h", "f1", "v1", "f2", "v2"],
                    Exact,
                ),
                step(
                    &["h-set", "h", "f1", "updated"],
                    &["HSET", "h", "f1", "updated"],
                    Exact,
                ),
                step(&["h-get", "h", "f1"], &["HGET", "h", "f1"], Exact),
                step(&["h-get", "h", "nope"], &["HGET", "h", "nope"], Exact),
                step(&["h-get", "missing", "f"], &["HGET", "missing", "f"], Exact),
                step(&["h-exists", "h", "f1"], &["HEXISTS", "h", "f1"], Exact),
                step(&["h-exists", "h", "nope"], &["HEXISTS", "h", "nope"], Exact),
                step(&["h-len", "h"], &["HLEN", "h"], Exact),
                step(&["h-get-all", "h"], &["HGETALL", "h"], PairSorted),
                step(
                    &["h-get-all", "missing"],
                    &["HGETALL", "missing"],
                    PairSorted,
                ),
                step(&["h-keys", "h"], &["HKEYS", "h"], Sorted),
                step(&["h-vals", "h"], &["HVALS", "h"], Sorted),
                step(
                    &["h-incr-by", "h", "n", "5"],
                    &["HINCRBY", "h", "n", "5"],
                    Exact,
                ),
                step(
                    &["h-incr-by", "h", "n", "-2"],
                    &["HINCRBY", "h", "n", "-2"],
                    Exact,
                ),
                step(
                    &["h-incr-by", "h", "f1", "1"],
                    &["HINCRBY", "h", "f1", "1"],
                    Error,
                ),
                step(
                    &["h-del", "h", "f1", "nope"],
                    &["HDEL", "h", "f1", "nope"],
                    Exact,
                ),
                step(&["h-len", "h"], &["HLEN", "h"], Exact),
            ],
        ),
        (
            "wrongtype",
            vec![
                step(&["set", "str", "v"], &["SET", "str", "v"], Exact),
                step(&["r-push", "list", "a"], &["RPUSH", "list", "a"], Exact),
                step(&["s-add", "set", "a"], &["SADD", "set", "a"], Exact),
                step(
                    &["h-set", "hash", "f", "v"],
                    &["HSET", "hash", "f", "v"],
                    Exact,
                ),
                step(&["get", "list"], &["GET", "list"], Error),
                step(&["strlen", "set"], &["STRLEN", "set"], Error),
                step(&["incr", "hash"], &["INCR", "hash"], Error),
                step(&["append", "list", "x"], &["APPEND", "list", "x"], Error),
                step(&["l-push", "str", "x"], &["LPUSH", "str", "x"], Error),
                step(&["l-pop", "str"], &["LPOP", "str"], Error),
                step(&["l-len", "str"], &["LLEN", "str"], Error),
                step(
                    &["l-range", "set", "0", "-1"],
                    &["LRANGE", "set", "0", "-1"],
                    Error,
                ),
                step(&["l-index", "hash", "0"], &["LINDEX", "hash", "0"], Error),
                step(&["s-add", "str", "x"], &["SADD", "str", "x"], Error),
                step(&["s-members", "list"], &["SMEMBERS", "list"], Error),
                step(&["s-card", "str"], &["SCARD", "str"], Error),
                step(&["s-pop", "str"], &["SPOP", "str"], Error),
                step(&["s-union", "set", "str"], &["SUNION", "set", "str"], Error),
                step(
                    &["s-inter", "set", "list"],
                    &["SINTER", "set", "list"],
                    Error,
                ),
                step(&["s-diff", "set", "hash"], &["SDIFF", "set", "hash"], Error),
                step(
                    &["h-set", "list", "f", "v"],
                    &["HSET", "list", "f", "v"],
                    Error,
                ),
                step(&["h-get", "str", "f"], &["HGET", "str", "f"], Error),
                step(&["h-len", "set"], &["HLEN", "set"], Error),
                step(&["h-get-all", "list"], &["HGETALL", "list"], Error),
            ],
        ),
        (
            "key-ops",
            vec![
                step(&["set", "a", "1"], &["SET", "a", "1"], Exact),
                step(&["set", "b", "2"], &["SET", "b", "2"], Exact),
                step(
                    &["rename", "a", "renamed"],
                    &["RENAME", "a", "renamed"],
                    Exact,
                ),
                step(&["get", "renamed"], &["GET", "renamed"], Exact),
                step(&["exists", "a"], &["EXISTS", "a"], Exact),
                step(
                    &["rename", "missing", "x"],
                    &["RENAME", "missing", "x"],
                    Error,
                ),
                step(
                    &["rename", "renamed", "b"],
                    &["RENAME", "renamed", "b"],
                    Exact,
                ),
                step(&["get", "b"], &["GET", "b"], Exact),
                step(&["rename", "b", "b"], &["RENAME", "b", "b"], Exact),
                step(&["get", "b"], &["GET", "b"], Exact),
                step(&["set", "user:1", "x"], &["SET", "user:1", "x"], Exact),
                step(&["set", "user:2", "y"], &["SET", "user:2", "y"], Exact),
                step(&["keys", "user:*"], &["KEYS", "user:*"], Sorted),
                step(&["keys", "user:?"], &["KEYS", "user:?"], Sorted),
                step(&["keys", "*"], &["KEYS", "*"], Sorted),
                step(&["keys", "nomatch*"], &["KEYS", "nomatch*"], Sorted),
                step(&["db-size"], &["DBSIZE"], Exact),
                step(&["flush-all"], &["FLUSHALL"], Exact),
                step(&["db-size"], &["DBSIZE"], Exact),
                step(&["keys", "*"], &["KEYS", "*"], Sorted),
            ],
        ),
        (
            "ttl-codes",
            vec![
                step(&["set", "k", "v"], &["SET", "k", "v"], Exact),
                step(&["ttl", "k"], &["TTL", "k"], Exact),
                step(&["ttl", "missing"], &["TTL", "missing"], Exact),
                step(
                    &["expire", "missing", "100"],
                    &["EXPIRE", "missing", "100"],
                    Exact,
                ),
                step(&["persist", "k"], &["PERSIST", "k"], Exact),
                step(&["expire", "k", "1000"], &["EXPIRE", "k", "1000"], Exact),
                step(&["persist", "k"], &["PERSIST", "k"], Exact),
                step(&["ttl", "k"], &["TTL", "k"], Exact),
                step(
                    &["expire-at", "k", "9999999999"],
                    &["EXPIREAT", "k", "9999999999"],
                    Exact,
                ),
                step(&["exists", "k"], &["EXISTS", "k"], Exact),
            ],
        ),
    ]
}

#[test]
fn differential_vs_real_redis() {
    if !have("redis-server") || !have("redis-cli") {
        eprintln!("skipping differential test: redis-server/redis-cli not on PATH");
        return;
    }
    let redis = start_redis();
    let dir = tempfile::tempdir().unwrap();

    for (i, (name, steps)) in scenarios().into_iter().enumerate() {
        // Fresh state on both sides per scenario.
        let db = dir.path().join(format!("diff-{i}.db"));
        let db = db.to_str().unwrap();
        run_redis(redis.port, &["FLUSHALL"]);

        for (j, s) in steps.iter().enumerate() {
            check_step(db, redis.port, name, j, s);
        }
    }
}
