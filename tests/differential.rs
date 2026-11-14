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
//! Deliberately not matched: `SET k v PX 9223372036854775807` (or the largest
//! accepted EX). Redis replies OK but its `now + ttl` wraps negative and the
//! key is gone immediately; klyv saturates and keeps the key.
//!
//! Not exercised here: purge (klyv-only) and db-size (Redis may briefly count
//! a logically expired key until its active sweep runs, so results can race).

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
    // The port is chosen by binding and releasing it, so another process
    // (these tests run in parallel) can take it first; redis-server then
    // exits at once. Notice that and try another port rather than waiting.
    for _attempt in 0..5 {
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
        let mut guard = RedisGuard { child, port };
        // Generous: a loaded machine can take seconds to start a server.
        for _ in 0..600 {
            if guard.child.try_wait().ok().flatten().is_some() {
                break;
            }
            let out = Command::new("redis-cli")
                .args(["-p", &guard.port.to_string(), "PING"])
                .output()
                .expect("failed to run redis-cli");
            if String::from_utf8_lossy(&out.stdout).trim() == "PONG"
                && guard.child.try_wait().ok().flatten().is_none()
            {
                return guard;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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
    /// Server mode only. A SCAN-family command whose `CURSOR` argument is a
    /// placeholder: iterate to completion on both sides and compare the
    /// items as a set. Cursors and page boundaries are each server's own
    /// business; what must agree is what a full iteration yields.
    ScanAll,
    /// As `ScanAll`, for HSCAN with values: items are field/value pairs.
    ScanAllPairs,
}

struct Step {
    klyv: &'static [&'static str],
    redis: &'static [&'static str],
    cmp: Cmp,
}

fn step(klyv: &'static [&'static str], redis: &'static [&'static str], cmp: Cmp) -> Step {
    Step { klyv, redis, cmp }
}

/// Reply lines, keeping empty ones: an empty key, member, field or value is
/// a real (empty) line and dropping it would misalign everything after it.
/// The one unavoidable ambiguity is a lone newline, which redis-cli prints
/// both for an empty array and for an array holding one empty string; it is
/// read as empty, which is what klyv's empty output (no line at all) means.
fn reply_lines(s: &str) -> Vec<&str> {
    if s.is_empty() || s == "\n" {
        return Vec::new();
    }
    s.strip_suffix('\n').unwrap_or(s).split('\n').collect()
}

fn sorted_lines(s: &str) -> Vec<&str> {
    let mut lines = reply_lines(s);
    lines.sort_unstable();
    lines
}

fn sorted_pairs(s: &str) -> Vec<(&str, &str)> {
    let lines = reply_lines(s);
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
        Cmp::ScanAll | Cmp::ScanAllPairs => unreachable!("scan steps are server-only"),
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
                step(&["p-ttl", "missing"], &["PTTL", "missing"], Exact),
                step(&["persist", "k"], &["PERSIST", "k"], Exact),
                step(&["p-ttl", "k"], &["PTTL", "k"], Exact),
                step(
                    &["p-expire-at", "k", "9999999999999"],
                    &["PEXPIREAT", "k", "9999999999999"],
                    Exact,
                ),
                step(
                    &["p-expire-at", "missing", "9999999999999"],
                    &["PEXPIREAT", "missing", "9999999999999"],
                    Exact,
                ),
                step(&["p-expire", "k", "0"], &["PEXPIRE", "k", "0"], Exact),
                step(&["exists", "k"], &["EXISTS", "k"], Exact),
                step(&["ttl", "k"], &["TTL", "k"], Exact),
            ],
        ),
        (
            "set-ttl-semantics",
            vec![
                step(&["set", "k", "v"], &["SET", "k", "v"], Exact),
                step(&["expire", "k", "1000"], &["EXPIRE", "k", "1000"], Exact),
                // Plain SET discards the TTL.
                step(&["set", "k", "v2"], &["SET", "k", "v2"], Exact),
                step(&["ttl", "k"], &["TTL", "k"], Exact),
                step(&["expire", "k", "1000"], &["EXPIRE", "k", "1000"], Exact),
                // KEEPTTL keeps it: PERSIST then has something to remove.
                step(
                    &["set", "k", "v3", "--keep-ttl"],
                    &["SET", "k", "v3", "KEEPTTL"],
                    Exact,
                ),
                step(&["persist", "k"], &["PERSIST", "k"], Exact),
                step(&["expire", "k", "1000"], &["EXPIRE", "k", "1000"], Exact),
                step(&["m-set", "k", "v4"], &["MSET", "k", "v4"], Exact),
                step(&["ttl", "k"], &["TTL", "k"], Exact),
            ],
        ),
        (
            "keys-glob",
            vec![
                step(&["set", "user:1", "a"], &["SET", "user:1", "a"], Exact),
                step(&["set", "User:2", "b"], &["SET", "User:2", "b"], Exact),
                step(&["set", "USER:3", "c"], &["SET", "USER:3", "c"], Exact),
                step(&["set", "k1", "v"], &["SET", "k1", "v"], Exact),
                step(&["set", "k2", "v"], &["SET", "k2", "v"], Exact),
                step(&["set", "ka", "v"], &["SET", "ka", "v"], Exact),
                step(&["set", "a*b", "v"], &["SET", "a*b", "v"], Exact),
                step(&["set", "axb", "v"], &["SET", "axb", "v"], Exact),
                step(&["keys", "user:*"], &["KEYS", "user:*"], Sorted),
                step(&["keys", "U*"], &["KEYS", "U*"], Sorted),
                step(&["keys", "k[12]"], &["KEYS", "k[12]"], Sorted),
                step(&["keys", "k[^12]"], &["KEYS", "k[^12]"], Sorted),
                step(&["keys", "k[a-z]"], &["KEYS", "k[a-z]"], Sorted),
                step(&["keys", "a\\*b"], &["KEYS", "a\\*b"], Sorted),
                step(&["keys", "a?b"], &["KEYS", "a?b"], Sorted),
                step(&["keys", "?"], &["KEYS", "?"], Sorted),
                step(&["set", "a\\b", "v"], &["SET", "a\\b", "v"], Exact),
                step(&["set", "ab", "v"], &["SET", "ab", "v"], Exact),
                step(&["keys", "a\\b"], &["KEYS", "a\\b"], Sorted),
                step(&["keys", "a\\\\b"], &["KEYS", "a\\\\b"], Sorted),
            ],
        ),
        (
            "counted-pops",
            vec![
                step(
                    &["r-push", "l", "a", "b", "c", "d", "e"],
                    &["RPUSH", "l", "a", "b", "c", "d", "e"],
                    Exact,
                ),
                step(&["l-pop", "l", "2"], &["LPOP", "l", "2"], Exact),
                step(&["l-pop", "l", "0"], &["LPOP", "l", "0"], Exact),
                step(&["l-pop", "l", "-1"], &["LPOP", "l", "-1"], Error),
                step(&[], &["LPOP", "l", "x"], Error),
                step(&[], &["LPOP", "l", "1", "2"], Error),
                step(&["r-pop", "l", "2"], &["RPOP", "l", "2"], Exact),
                step(&["l-len", "l"], &["LLEN", "l"], Exact),
                step(&["r-pop", "l", "10"], &["RPOP", "l", "10"], Exact),
                step(&["exists", "l"], &["EXISTS", "l"], Exact),
                step(&["l-pop", "nokey", "2"], &["LPOP", "nokey", "2"], Exact),
                step(&["r-pop", "nokey", "0"], &["RPOP", "nokey", "0"], Exact),
                step(&["set", "str", "v"], &["SET", "str", "v"], Exact),
                step(&["l-pop", "str", "1"], &["LPOP", "str", "1"], Error),
            ],
        ),
        (
            // Random picks: only cases whose outcome is determined.
            "spop-srandmember-counts",
            vec![
                step(
                    &["s-add", "s", "a", "b", "c"],
                    &["SADD", "s", "a", "b", "c"],
                    Exact,
                ),
                step(&["s-pop", "s", "0"], &["SPOP", "s", "0"], Exact),
                step(&["s-pop", "s", "-1"], &["SPOP", "s", "-1"], Error),
                step(&[], &["SPOP", "s", "x"], Error),
                step(
                    &["s-rand-member", "s", "0"],
                    &["SRANDMEMBER", "s", "0"],
                    Exact,
                ),
                step(
                    &["s-rand-member", "s", "10"],
                    &["SRANDMEMBER", "s", "10"],
                    Sorted,
                ),
                step(
                    &["s-rand-member", "s", "3"],
                    &["SRANDMEMBER", "s", "3"],
                    Sorted,
                ),
                step(&["s-card", "s"], &["SCARD", "s"], Exact),
                step(&["s-pop", "s", "10"], &["SPOP", "s", "10"], Sorted),
                step(&["exists", "s"], &["EXISTS", "s"], Exact),
                step(&["s-pop", "nokey", "2"], &["SPOP", "nokey", "2"], Exact),
                step(
                    &["s-rand-member", "nokey"],
                    &["SRANDMEMBER", "nokey"],
                    Exact,
                ),
                step(
                    &["s-rand-member", "nokey", "3"],
                    &["SRANDMEMBER", "nokey", "3"],
                    Exact,
                ),
                step(
                    &["s-rand-member", "nokey", "-3"],
                    &["SRANDMEMBER", "nokey", "-3"],
                    Exact,
                ),
                step(&["s-add", "one", "x"], &["SADD", "one", "x"], Exact),
                step(&["s-rand-member", "one"], &["SRANDMEMBER", "one"], Exact),
                step(
                    &["s-rand-member", "one", "-3"],
                    &["SRANDMEMBER", "one", "-3"],
                    Exact,
                ),
                step(
                    &["s-rand-member", "one", "5"],
                    &["SRANDMEMBER", "one", "5"],
                    Exact,
                ),
                step(&[], &["SRANDMEMBER", "one", "x"], Error),
                step(&["set", "str", "v"], &["SET", "str", "v"], Exact),
                step(
                    &["s-rand-member", "str", "1"],
                    &["SRANDMEMBER", "str", "1"],
                    Error,
                ),
                step(&["s-pop", "str", "1"], &["SPOP", "str", "1"], Error),
            ],
        ),
        (
            "expire-options",
            vec![
                step(&["set", "k", "v"], &["SET", "k", "v"], Exact),
                step(
                    &["expire", "k", "100", "--nx"],
                    &["EXPIRE", "k", "100", "NX"],
                    Exact,
                ),
                step(
                    &["expire", "k", "200", "--nx"],
                    &["EXPIRE", "k", "200", "NX"],
                    Exact,
                ),
                step(
                    &["expire", "k", "200", "--xx"],
                    &["EXPIRE", "k", "200", "XX"],
                    Exact,
                ),
                step(
                    &["expire", "k", "50", "--gt"],
                    &["EXPIRE", "k", "50", "GT"],
                    Exact,
                ),
                step(
                    &["expire", "k", "500", "--gt"],
                    &["EXPIRE", "k", "500", "GT"],
                    Exact,
                ),
                step(
                    &["expire", "k", "600", "--lt"],
                    &["EXPIRE", "k", "600", "LT"],
                    Exact,
                ),
                step(
                    &["expire", "k", "100", "--lt"],
                    &["EXPIRE", "k", "100", "LT"],
                    Exact,
                ),
                step(
                    &["expire", "k", "300", "--xx", "--gt"],
                    &["EXPIRE", "k", "300", "XX", "GT"],
                    Exact,
                ),
                step(
                    &["p-expire", "k", "1", "--xx", "--gt"],
                    &["PEXPIRE", "k", "1", "XX", "GT"],
                    Exact,
                ),
                step(
                    &["expire", "k", "100", "--nx", "--xx"],
                    &["EXPIRE", "k", "100", "NX", "XX"],
                    Error,
                ),
                step(
                    &["expire", "k", "100", "--gt", "--lt"],
                    &["EXPIRE", "k", "100", "GT", "LT"],
                    Error,
                ),
                step(&[], &["EXPIRE", "k", "100", "BOGUS"], Error),
                step(&[], &["EXPIRE", "k", "100", "nx", "gt"], Error),
                step(&["persist", "k"], &["PERSIST", "k"], Exact),
                // No expiry counts as "never": GT cannot beat it, LT always does.
                step(
                    &["expire", "k", "100", "--gt"],
                    &["EXPIRE", "k", "100", "GT"],
                    Exact,
                ),
                step(
                    &["expire", "k", "100", "--xx"],
                    &["EXPIRE", "k", "100", "XX"],
                    Exact,
                ),
                step(
                    &["p-expire-at", "k", "99999999999999", "--lt"],
                    &["PEXPIREAT", "k", "99999999999999", "LT"],
                    Exact,
                ),
                step(
                    &["expire-at", "k", "9999999999", "--lt"],
                    &["EXPIREAT", "k", "9999999999", "LT"],
                    Exact,
                ),
                step(
                    &["expire-at", "k", "9999999999", "--lt"],
                    &["EXPIREAT", "k", "9999999999", "LT"],
                    Exact,
                ),
                step(&["persist", "k"], &["PERSIST", "k"], Exact),
                step(
                    &["expire", "nokey", "100", "--nx"],
                    &["EXPIRE", "nokey", "100", "NX"],
                    Exact,
                ),
                step(
                    &["p-expire", "k", "-1", "--lt"],
                    &["PEXPIRE", "k", "-1", "LT"],
                    Exact,
                ),
                step(&["exists", "k"], &["EXISTS", "k"], Exact),
            ],
        ),
        (
            "getset-getex",
            vec![
                step(&["set", "str", "v"], &["SET", "str", "v"], Exact),
                step(
                    &["expire", "str", "1000"],
                    &["EXPIRE", "str", "1000"],
                    Exact,
                ),
                step(&["get-set", "str", "new"], &["GETSET", "str", "new"], Exact),
                // GETSET discards the TTL, like SET.
                step(&["persist", "str"], &["PERSIST", "str"], Exact),
                step(&["get-set", "fresh", "v"], &["GETSET", "fresh", "v"], Exact),
                step(&["get", "fresh"], &["GET", "fresh"], Exact),
                step(&["r-push", "l", "a"], &["RPUSH", "l", "a"], Exact),
                step(&["get-set", "l", "v"], &["GETSET", "l", "v"], Error),
                step(&["l-len", "l"], &["LLEN", "l"], Exact),
                step(
                    &["get-ex", "str", "--pxat", "9223372036854775807"],
                    &["GETEX", "str", "PXAT", "9223372036854775807"],
                    Exact,
                ),
                step(&["persist", "str"], &["PERSIST", "str"], Exact),
                step(&[], &["GETEX", "str", "EX"], Error),
                step(&[], &["GETEX", "str", "EX", "abc"], Error),
                step(
                    &["get-ex", "nokey", "--ex", "10"],
                    &["GETEX", "nokey", "EX", "10"],
                    Exact,
                ),
                step(
                    &["get-ex", "l", "--ex", "10"],
                    &["GETEX", "l", "EX", "10"],
                    Error,
                ),
                // An expiry in the past removes the key, after returning it.
                step(
                    &["get-ex", "str", "--exat", "1"],
                    &["GETEX", "str", "EXAT", "1"],
                    Exact,
                ),
                step(&["exists", "str"], &["EXISTS", "str"], Exact),
            ],
        ),
        (
            "hmget",
            vec![
                step(
                    &["h-set", "h", "f", "1", "g", "2"],
                    &["HSET", "h", "f", "1", "g", "2"],
                    Exact,
                ),
                step(
                    &["h-m-get", "h", "f", "nope", "g", "f"],
                    &["HMGET", "h", "f", "nope", "g", "f"],
                    Exact,
                ),
                step(
                    &["h-m-get", "nokey", "a", "b"],
                    &["HMGET", "nokey", "a", "b"],
                    Exact,
                ),
                step(&["set", "str", "v"], &["SET", "str", "v"], Exact),
                step(&["h-m-get", "str", "a"], &["HMGET", "str", "a"], Error),
                step(&[], &["HMGET", "h"], Error),
            ],
        ),
        (
            "smove",
            vec![
                step(&["s-add", "s", "a", "b"], &["SADD", "s", "a", "b"], Exact),
                step(
                    &["s-move", "s", "s2", "a"],
                    &["SMOVE", "s", "s2", "a"],
                    Exact,
                ),
                step(
                    &["s-move", "s", "s2", "nope"],
                    &["SMOVE", "s", "s2", "nope"],
                    Exact,
                ),
                step(
                    &["s-move", "nokey", "s2", "a"],
                    &["SMOVE", "nokey", "s2", "a"],
                    Exact,
                ),
                step(&["set", "str", "v"], &["SET", "str", "v"], Exact),
                step(
                    &["s-move", "s", "str", "b"],
                    &["SMOVE", "s", "str", "b"],
                    Error,
                ),
                step(
                    &["s-move", "s", "str", "nope"],
                    &["SMOVE", "s", "str", "nope"],
                    Error,
                ),
                step(
                    &["s-move", "str", "s", "a"],
                    &["SMOVE", "str", "s", "a"],
                    Error,
                ),
                // A missing source is 0 even when the destination is the wrong type.
                step(
                    &["s-move", "nokey", "str", "a"],
                    &["SMOVE", "nokey", "str", "a"],
                    Exact,
                ),
                step(&["s-move", "s", "s", "b"], &["SMOVE", "s", "s", "b"], Exact),
                step(
                    &["s-move", "s", "s", "zz"],
                    &["SMOVE", "s", "s", "zz"],
                    Exact,
                ),
                step(&["s-add", "s2", "b"], &["SADD", "s2", "b"], Exact),
                // Already in the destination: still moved out of the source,
                // which empties it and deletes the key.
                step(
                    &["s-move", "s", "s2", "b"],
                    &["SMOVE", "s", "s2", "b"],
                    Exact,
                ),
                step(&["exists", "s"], &["EXISTS", "s"], Exact),
                step(&["s-members", "s2"], &["SMEMBERS", "s2"], Sorted),
                step(&["s-card", "s2"], &["SCARD", "s2"], Exact),
            ],
        ),
        (
            "copy",
            vec![
                step(&["set", "c1", "v"], &["SET", "c1", "v"], Exact),
                step(&["expire", "c1", "1000"], &["EXPIRE", "c1", "1000"], Exact),
                step(&["copy", "c1", "c2"], &["COPY", "c1", "c2"], Exact),
                step(&["copy", "c1", "c2"], &["COPY", "c1", "c2"], Exact),
                step(
                    &["copy", "c1", "c2", "--replace"],
                    &["COPY", "c1", "c2", "REPLACE"],
                    Exact,
                ),
                step(&["get", "c2"], &["GET", "c2"], Exact),
                // The TTL was copied: there is one to remove.
                step(&["persist", "c2"], &["PERSIST", "c2"], Exact),
                step(&["persist", "c1"], &["PERSIST", "c1"], Exact),
                step(&["copy", "nokey", "c3"], &["COPY", "nokey", "c3"], Exact),
                step(&["copy", "c1", "c1"], &["COPY", "c1", "c1"], Error),
                step(&[], &["COPY", "c1", "c2", "BOGUS"], Error),
                step(&[], &["COPY", "c1", "c2", "DB"], Error),
                step(&[], &["COPY", "c1", "c2", "DB", "x"], Error),
                step(&[], &["COPY", "c1", "c9", "DB", "0"], Exact),
                step(
                    &["r-push", "l", "a", "b", "c"],
                    &["RPUSH", "l", "a", "b", "c"],
                    Exact,
                ),
                step(&["copy", "l", "l2"], &["COPY", "l", "l2"], Exact),
                step(&["r-push", "l2", "d"], &["RPUSH", "l2", "d"], Exact),
                step(
                    &["l-range", "l2", "0", "-1"],
                    &["LRANGE", "l2", "0", "-1"],
                    Exact,
                ),
                step(&["l-len", "l"], &["LLEN", "l"], Exact),
                step(&["s-add", "s", "a", "b"], &["SADD", "s", "a", "b"], Exact),
                step(&["copy", "s", "s2"], &["COPY", "s", "s2"], Exact),
                step(&["s-members", "s2"], &["SMEMBERS", "s2"], Sorted),
                step(
                    &["h-set", "h", "f", "v", "g", "w"],
                    &["HSET", "h", "f", "v", "g", "w"],
                    Exact,
                ),
                step(&["copy", "h", "h2"], &["COPY", "h", "h2"], Exact),
                step(&["h-get-all", "h2"], &["HGETALL", "h2"], PairSorted),
                step(&["h-len", "h2"], &["HLEN", "h2"], Exact),
                // Replacing a key of another type.
                step(
                    &["copy", "l", "c1", "--replace"],
                    &["COPY", "l", "c1", "REPLACE"],
                    Exact,
                ),
                step(&["type", "c1"], &["TYPE", "c1"], Exact),
                step(&["l-len", "c1"], &["LLEN", "c1"], Exact),
                step(
                    &["copy", "c2", "h2", "--replace"],
                    &["COPY", "c2", "h2", "REPLACE"],
                    Exact,
                ),
                step(&["type", "h2"], &["TYPE", "h2"], Exact),
                step(
                    &["exists", "c1", "c2", "c1", "nokey", "l2"],
                    &["EXISTS", "c1", "c2", "c1", "nokey", "l2"],
                    Exact,
                ),
            ],
        ),
        (
            "scan",
            vec![
                step(
                    &[
                        "m-set", "k1", "v", "k2", "v", "k3", "v", "k4", "v", "k5", "v", "k6", "v",
                        "k7", "v", "k8", "v", "k9", "v", "k10", "v", "k11", "v", "k12", "v", "k13",
                        "v",
                    ],
                    &[
                        "MSET", "k1", "v", "k2", "v", "k3", "v", "k4", "v", "k5", "v", "k6", "v",
                        "k7", "v", "k8", "v", "k9", "v", "k10", "v", "k11", "v", "k12", "v", "k13",
                        "v",
                    ],
                    Exact,
                ),
                step(&["r-push", "list", "a"], &["RPUSH", "list", "a"], Exact),
                step(
                    &[
                        "h-set", "h", "f1", "1", "f2", "2", "f3", "3", "g1", "4", "g2", "5", "g3",
                        "6", "g4", "7",
                    ],
                    &[
                        "HSET", "h", "f1", "1", "f2", "2", "f3", "3", "g1", "4", "g2", "5", "g3",
                        "6", "g4", "7",
                    ],
                    Exact,
                ),
                step(
                    &["s-add", "s", "m1", "m2", "m3", "n1", "n2", "n3", "n4"],
                    &["SADD", "s", "m1", "m2", "m3", "n1", "n2", "n3", "n4"],
                    Exact,
                ),
                step(&["set", "gone", "v"], &["SET", "gone", "v"], Exact),
                step(
                    &["p-expire-at", "gone", "1"],
                    &["PEXPIREAT", "gone", "1"],
                    Exact,
                ),
                step(&["del", "k5", "k6"], &["DEL", "k5", "k6"], Exact),
                step(&[], &["SCAN", "CURSOR"], ScanAll),
                step(&[], &["SCAN", "CURSOR", "COUNT", "1"], ScanAll),
                step(
                    &[],
                    &["SCAN", "CURSOR", "COUNT", "3", "MATCH", "k1*"],
                    ScanAll,
                ),
                step(
                    &[],
                    &["SCAN", "CURSOR", "MATCH", "nothing*", "COUNT", "2"],
                    ScanAll,
                ),
                step(
                    &[],
                    &["SCAN", "CURSOR", "COUNT", "1000", "TYPE", "string"],
                    ScanAll,
                ),
                step(
                    &[],
                    &["SCAN", "CURSOR", "TYPE", "list", "COUNT", "4"],
                    ScanAll,
                ),
                step(&[], &["SCAN", "CURSOR", "TYPE", "bogus"], ScanAll),
                step(
                    &[],
                    &["SCAN", "CURSOR", "TYPE", "STRING", "COUNT", "100"],
                    ScanAll,
                ),
                step(&[], &["SCAN", "CURSOR", "type", "Hash"], ScanAll),
                step(&[], &["HSCAN", "h", "CURSOR"], ScanAllPairs),
                step(&[], &["HSCAN", "h", "CURSOR", "COUNT", "2"], ScanAllPairs),
                step(
                    &[],
                    &["HSCAN", "h", "CURSOR", "COUNT", "2", "MATCH", "g*"],
                    ScanAllPairs,
                ),
                step(
                    &[],
                    &["HSCAN", "h", "CURSOR", "NOVALUES", "COUNT", "3"],
                    ScanAll,
                ),
                step(&[], &["SSCAN", "s", "CURSOR"], ScanAll),
                step(
                    &[],
                    &["SSCAN", "s", "CURSOR", "COUNT", "2", "MATCH", "n*"],
                    ScanAll,
                ),
                step(&[], &["HSCAN", "nokey", "0"], Exact),
                step(&[], &["SSCAN", "nokey", "0"], Exact),
                step(&[], &["SSCAN", "gone", "0"], Exact),
                step(&[], &["SCAN", "x"], Error),
                step(&[], &["SCAN", "-1"], Error),
                step(&[], &["SCAN", "0", "COUNT", "0"], Error),
                step(&[], &["SCAN", "0", "COUNT", "x"], Error),
                step(&[], &["SCAN", "0", "BOGUS"], Error),
                step(&[], &["SCAN", "0", "MATCH"], Error),
                step(&[], &["SCAN", "0", "NOVALUES"], Error),
                step(&[], &["SCAN"], Error),
                step(&[], &["HSCAN", "h", "x"], Error),
                step(&[], &["HSCAN", "h", "0", "TYPE", "hash"], Error),
                step(&[], &["HSCAN", "k1", "0"], Error),
                step(&[], &["SSCAN", "h", "0"], Error),
                step(&[], &["SSCAN", "s", "0", "NOVALUES"], Error),
            ],
        ),
        (
            "server:multi",
            vec![
                step(&[], &["MULTI"], Exact),
                step(&[], &["MULTI"], Error),
                step(&[], &["SET", "m", "1"], Exact),
                step(&[], &["INCR", "m"], Exact),
                step(&[], &["GET"], Error),
                step(&[], &["NOSUCH", "a"], Error),
                // A command rejected while queueing aborts the whole EXEC.
                step(&[], &["EXEC"], Error),
                step(&[], &["EXISTS", "m"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["SET", "m", "5"], Exact),
                step(&[], &["INCR", "m"], Exact),
                // Fails when run: reported in place, the rest still apply.
                step(&[], &["LPUSH", "m", "x"], Exact),
                step(&[], &["PING"], Exact),
                step(&[], &["RPUSH", "l", "a", "b"], Exact),
                step(&[], &["LRANGE", "l", "0", "-1"], Exact),
                step(&[], &["HSET", "h", "f", "v"], Exact),
                step(&[], &["HGETALL", "h"], Exact),
                step(&[], &["SADD", "s", "x"], Exact),
                step(&[], &["SMEMBERS", "s"], Exact),
                step(&[], &["GET", "nokey"], Exact),
                step(&[], &["LPOP", "nokey", "2"], Exact),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "m"], Exact),
                step(&[], &["EXEC"], Error),
                step(&[], &["DISCARD"], Error),
                step(&[], &["MULTI"], Exact),
                step(&[], &["SET", "m", "9"], Exact),
                step(&[], &["DISCARD"], Exact),
                step(&[], &["GET", "m"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["EXEC"], Exact),
                step(&[], &["MULTI", "x"], Error),
                step(&[], &["WATCH"], Error),
                step(&[], &["WATCH", "m", "nokey"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["WATCH", "m"], Error),
                step(&[], &["INCR", "m"], Exact),
                step(&[], &["UNWATCH"], Exact),
                step(&[], &["EXEC"], Exact),
                // Unchanged watched keys do not block the transaction.
                step(&[], &["WATCH", "m"], Exact),
                step(&[], &["GET", "m"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "m"], Exact),
                step(&[], &["EXEC"], Exact),
                // A watched key changed (by this connection, before MULTI).
                step(&[], &["WATCH", "m", "l"], Exact),
                step(&[], &["RPUSH", "l", "c"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "m"], Exact),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "m"], Exact),
                // The aborted EXEC cleared the watch.
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "m"], Exact),
                step(&[], &["EXEC"], Exact),
                step(&[], &["WATCH", "m"], Exact),
                step(&[], &["SET", "m", "100"], Exact),
                step(&[], &["UNWATCH"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "m"], Exact),
                step(&[], &["EXEC"], Exact),
                step(&[], &["UNWATCH", "x"], Error),
            ],
        ),
        (
            // Which mistakes made inside MULTI spoil the EXEC, and which are
            // merely refused. Redis decides; these steps record its answer.
            "server:multi-errors",
            vec![
                step(&[], &["SET", "a", "0"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["MULTI"], Error),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "a"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["EXEC", "x"], Error),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "a"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["DISCARD", "x"], Error),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "a"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["MULTI", "x"], Error),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "a"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["WATCH", "a"], Error),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "a"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["WATCH"], Error),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "a"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["UNWATCH", "x"], Error),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "a"], Exact),
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["INCR", "a", "b"], Error),
                step(&[], &["DISCARD"], Exact),
                // A discarded transaction leaves nothing behind for the next.
                step(&[], &["MULTI"], Exact),
                step(&[], &["INCR", "a"], Exact),
                step(&[], &["EXEC"], Exact),
                step(&[], &["GET", "a"], Exact),
            ],
        ),
        (
            // SPOP picks at random, so only its deterministic cases can be
            // compared: a single-member set, and a missing key.
            "spop",
            vec![
                step(&["s-add", "s", "only"], &["SADD", "s", "only"], Exact),
                step(&["s-pop", "s"], &["SPOP", "s"], Exact),
                step(&["s-pop", "s"], &["SPOP", "s"], Exact),
                step(&["exists", "s"], &["EXISTS", "s"], Exact),
                step(&["s-card", "s"], &["SCARD", "s"], Exact),
                step(&["set", "str", "v"], &["SET", "str", "v"], Exact),
                step(&["s-pop", "str"], &["SPOP", "str"], Error),
            ],
        ),
        (
            "keys-glob-malformed",
            vec![
                step(
                    &["m-set", "a", "1", "a]", "1", "a\\", "1", "[a", "1"],
                    &["MSET", "a", "1", "a]", "1", "a\\", "1", "[a", "1"],
                    Exact,
                ),
                step(
                    &["m-set", "ab", "1", "a-c", "1", "b", "1"],
                    &["MSET", "ab", "1", "a-c", "1", "b", "1"],
                    Exact,
                ),
                step(&["keys", "[a"], &["KEYS", "[a"], Sorted),
                step(&["keys", "a["], &["KEYS", "a["], Sorted),
                step(&["keys", "[a-"], &["KEYS", "[a-"], Sorted),
                step(&["keys", "[^"], &["KEYS", "[^"], Sorted),
                step(&["keys", "[]a]"], &["KEYS", "[]a]"], Sorted),
                step(&["keys", "a[]"], &["KEYS", "a[]"], Sorted),
                step(&["keys", "[c-a]"], &["KEYS", "[c-a]"], Sorted),
                step(&["keys", "[a\\-c]*"], &["KEYS", "[a\\-c]*"], Sorted),
                step(&["keys", "[\\]a]*"], &["KEYS", "[\\]a]*"], Sorted),
                step(&["keys", "[!a]*"], &["KEYS", "[!a]*"], Sorted),
                step(&["keys", "a\\"], &["KEYS", "a\\"], Sorted),
                step(&["keys", "**"], &["KEYS", "**"], Sorted),
                step(&["keys", "*[ab]"], &["KEYS", "*[ab]"], Sorted),
            ],
        ),
        (
            "expire-overflow",
            vec![
                step(&["set", "k", "v"], &["SET", "k", "v"], Exact),
                // Overflow is rejected before the key is looked up.
                step(
                    &["expire", "missing", "9223372036854775807"],
                    &["EXPIRE", "missing", "9223372036854775807"],
                    Error,
                ),
                step(
                    &["expire", "k", "9223372036854775"],
                    &["EXPIRE", "k", "9223372036854775"],
                    Error,
                ),
                step(
                    &["expire", "k", "-9223372036854775808"],
                    &["EXPIRE", "k", "-9223372036854775808"],
                    Error,
                ),
                step(
                    &["p-expire", "k", "9223372036854775807"],
                    &["PEXPIRE", "k", "9223372036854775807"],
                    Error,
                ),
                step(
                    &["expire-at", "k", "9223372036854776"],
                    &["EXPIREAT", "k", "9223372036854776"],
                    Error,
                ),
                step(&["ttl", "k"], &["TTL", "k"], Exact),
                step(
                    &["set", "k", "v", "--ex", "9223372036854776"],
                    &["SET", "k", "v", "EX", "9223372036854776"],
                    Error,
                ),
                // The largest values that are accepted.
                step(
                    &["expire-at", "k", "9223372036854775"],
                    &["EXPIREAT", "k", "9223372036854775"],
                    Exact,
                ),
                step(
                    &["p-expire-at", "k", "9223372036854775807"],
                    &["PEXPIREAT", "k", "9223372036854775807"],
                    Exact,
                ),
                step(&["exists", "k"], &["EXISTS", "k"], Exact),
                step(
                    &["p-expire", "k", "-9223372036854775808"],
                    &["PEXPIRE", "k", "-9223372036854775808"],
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

        // Scenarios named "server:..." hold state on one connection (MULTI)
        // and cannot be driven one process per step. Within other scenarios
        // a step with no klyv arguments is server-only and must be a read,
        // so skipping it leaves both sides in step.
        if name.starts_with("server:") {
            continue;
        }
        for (j, s) in steps.iter().enumerate() {
            if s.klyv.is_empty() {
                continue;
            }
            check_step(db, redis.port, name, j, s);
            assert_len_invariant(std::path::Path::new(db), &|| {
                format!("scenario '{name}' after step {j}: {:?}", s.klyv)
            });
        }
    }
}

// === Randomised sequences ===
//
// The hand-written scenarios above share their author's blind spots (the
// case-insensitive KEYS bug survived them because every key was lowercase).
// This layer draws long command sequences from a deliberately awkward
// alphabet and checks every reply against Redis. A failure prints the seed
// and the recent history; set KLYV_DIFF_SEED to replay one, KLYV_DIFF_STEPS
// to lengthen a run.

/// xorshift64*: tiny, deterministic, good enough for sequence generation.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    fn chance(&mut self, one_in: u64) -> bool {
        self.next().is_multiple_of(one_in)
    }
}

const KEYS: &[&str] = &["k", "K", "key:1", "key:2", "a-b", "x y", "ключ", "", "k[1]"];
const VALUES: &[&str] = &[
    "v",
    "",
    "hello world",
    "-dash",
    "ünïcödé",
    "0",
    "42",
    "-7",
    "9223372036854775807",
    "#hash",
    "a*b",
    "\"quoted\"",
    "tab\there",
];
const FIELDS: &[&str] = &["f", "F", "field-1", ""];
const AMOUNTS: &[&str] = &[
    "-3",
    "0",
    "1",
    "7",
    "9223372036854775807",
    "-9223372036854775808",
];
const INDEXES: &[&str] = &["-100", "-3", "-1", "0", "1", "2", "5", "100"];
const COUNTS: &[&str] = &["-2", "-1", "0", "1", "2"];
const PATTERNS: &[&str] = &[
    "*", "k*", "K*", "key:?", "[kK]*", "*-*", "k\\[1\\]", "?",
    // Malformed or hostile: unclosed and empty classes, reversed ranges,
    // escapes inside classes, a trailing backslash, runs of stars.
    "[k", "[^", "[]k]*", "[z-a]*", "[k\\-z]*", "k\\", "**", "", "*[", "[^-]*", "*?*",
];
/// Expiry arguments whose outcome does not depend on timing: immediate,
/// far-future, or rejected as overflowing.
const EXPIRIES: &[&str] = &[
    "-9223372036854775808",
    "-1",
    "0",
    "100000",
    "9223372036854775",
    "9223372036854776",
    "9223372036854775807",
];

struct Gen {
    klyv: Vec<String>,
    redis: Vec<String>,
    /// How to compare replies when neither side errors.
    ok_cmp: Cmp,
}

fn g(klyv: &[&str], redis: &[&str], ok_cmp: Cmp) -> Gen {
    Gen {
        klyv: klyv.iter().map(|s| s.to_string()).collect(),
        redis: redis.iter().map(|s| s.to_string()).collect(),
        ok_cmp,
    }
}

fn random_step(rng: &mut Rng) -> Gen {
    use Cmp::*;
    let k = *rng.pick(KEYS);
    let k2 = *rng.pick(KEYS);
    let k3 = *rng.pick(KEYS);
    let v = *rng.pick(VALUES);
    let v2 = *rng.pick(VALUES);
    let f = *rng.pick(FIELDS);
    let f2 = *rng.pick(FIELDS);
    let n = *rng.pick(AMOUNTS);
    let i = *rng.pick(INDEXES);
    let j = *rng.pick(INDEXES);
    let c = *rng.pick(COUNTS);
    let pat = *rng.pick(PATTERNS);
    let x = *rng.pick(EXPIRIES);
    // 42 and 46..=53 share one arm: its ten commands were each drawn about
    // once in 460 steps, too rarely for a run of the default length to hit,
    // say, an HDEL that actually deletes a field.
    match rng.below(64) {
        0..=2 => g(&["set", "--", k, v], &["SET", k, v], Exact),
        3 => g(&["set", "--nx", "--", k, v], &["SET", k, v, "NX"], Exact),
        4 => g(
            &["set", "--keep-ttl", "--", k, v],
            &["SET", k, v, "KEEPTTL"],
            Exact,
        ),
        5 | 6 => g(&["get", "--", k], &["GET", k], Exact),
        7 => g(&["get-del", "--", k], &["GETDEL", k], Exact),
        8 => g(&["del", "--", k, k2], &["DEL", k, k2], Exact),
        9 => g(&["incr", "--", k], &["INCR", k], Exact),
        10 => g(&["incr-by", "--", k, n], &["INCRBY", k, n], Exact),
        11 => g(&["append", "--", k, v], &["APPEND", k, v], Exact),
        12 => g(&["strlen", "--", k], &["STRLEN", k], Exact),
        13 => g(
            &["m-set", "--", k, v, k2, v2],
            &["MSET", k, v, k2, v2],
            Exact,
        ),
        14 => g(&["m-get", "--", k, k2, k3], &["MGET", k, k2, k3], Exact),
        15 | 16 => g(&["r-push", "--", k, v, v2], &["RPUSH", k, v, v2], Exact),
        17 => g(&["l-push", "--", k, v], &["LPUSH", k, v], Exact),
        18 => g(&["l-pop", "--", k], &["LPOP", k], Exact),
        19 => g(&["r-pop", "--", k], &["RPOP", k], Exact),
        20 => g(&["l-range", "--", k, i, j], &["LRANGE", k, i, j], Exact),
        21 => g(&["l-len", "--", k], &["LLEN", k], Exact),
        22 => g(&["l-rem", "--", k, c, v], &["LREM", k, c, v], Exact),
        23 => g(&["l-pos", "--", k, v], &["LPOS", k, v], Exact),
        24 => g(&["l-index", "--", k, i], &["LINDEX", k, i], Exact),
        25 => g(&["l-set", "--", k, i, v], &["LSET", k, i, v], Exact),
        26 => g(&["l-trim", "--", k, i, j], &["LTRIM", k, i, j], Exact),
        27 => g(
            &["l-insert", "--", k, "before", v, v2],
            &["LINSERT", k, "BEFORE", v, v2],
            Exact,
        ),
        28 | 29 => g(&["s-add", "--", k, v, v2], &["SADD", k, v, v2], Exact),
        30 => g(&["s-rem", "--", k, v], &["SREM", k, v], Exact),
        31 => g(&["s-members", "--", k], &["SMEMBERS", k], Sorted),
        32 => g(&["s-is-member", "--", k, v], &["SISMEMBER", k, v], Exact),
        33 => g(&["s-card", "--", k], &["SCARD", k], Exact),
        34 => g(&["s-union", "--", k, k2], &["SUNION", k, k2], Sorted),
        35 => g(&["s-inter", "--", k, k2], &["SINTER", k, k2], Sorted),
        36 => g(&["s-diff", "--", k, k2], &["SDIFF", k, k2], Sorted),
        37 | 38 => g(
            &["h-set", "--", k, f, v, f2, v2],
            &["HSET", k, f, v, f2, v2],
            Exact,
        ),
        39 => g(&["h-get", "--", k, f], &["HGET", k, f], Exact),
        40 => g(&["h-incr-by", "--", k, f, n], &["HINCRBY", k, f, n], Exact),
        41 => g(&["h-get-all", "--", k], &["HGETALL", k], PairSorted),
        42 | 46..=53 => match rng.below(10) {
            0 => g(&["h-exists", "--", k, f], &["HEXISTS", k, f], Exact),
            1 => g(&["h-del", "--", k, f], &["HDEL", k, f], Exact),
            2 => g(&["h-keys", "--", k], &["HKEYS", k], Sorted),
            3 => g(&["h-vals", "--", k], &["HVALS", k], Sorted),
            4 => g(&["h-len", "--", k], &["HLEN", k], Exact),
            5 => g(&["exists", "--", k], &["EXISTS", k], Exact),
            6 => g(&["type", "--", k], &["TYPE", k], Exact),
            7 => g(&["rename", "--", k, k2], &["RENAME", k, k2], Exact),
            8 => g(&["keys", "--", pat], &["KEYS", pat], Sorted),
            _ => g(&["persist", "--", k], &["PERSIST", k], Exact),
        },
        43 => match rng.below(4) {
            0 => g(&["expire", "--", k, x], &["EXPIRE", k, x], Exact),
            1 => g(&["p-expire", "--", k, x], &["PEXPIRE", k, x], Exact),
            2 => g(&["expire-at", "--", k, x], &["EXPIREAT", k, x], Exact),
            _ => g(&["p-expire-at", "--", k, x], &["PEXPIREAT", k, x], Exact),
        },
        54 => g(&["l-pop", "--", k, c], &["LPOP", k, c], Exact),
        55 => g(&["r-pop", "--", k, c], &["RPOP", k, c], Exact),
        56 => g(
            &["exists", "--", k, k2, k3, k],
            &["EXISTS", k, k2, k3, k],
            Exact,
        ),
        57 => g(&["h-m-get", "--", k, f, f2], &["HMGET", k, f, f2], Exact),
        58 => g(&["s-move", "--", k, k2, v], &["SMOVE", k, k2, v], Exact),
        59 => {
            if rng.chance(2) {
                g(
                    &["copy", "--replace", "--", k, k2],
                    &["COPY", k, k2, "REPLACE"],
                    Exact,
                )
            } else {
                g(&["copy", "--", k, k2], &["COPY", k, k2], Exact)
            }
        }
        60 => g(&["get-set", "--", k, v], &["GETSET", k, v], Exact),
        61 => match rng.below(3) {
            0 => g(
                &["get-ex", "--persist", "--", k],
                &["GETEX", k, "PERSIST"],
                Exact,
            ),
            1 => g(
                &["get-ex", "--ex=100000", "--", k],
                &["GETEX", k, "EX", "100000"],
                Exact,
            ),
            _ => g(
                &["get-ex", "--pxat=1", "--", k],
                &["GETEX", k, "PXAT", "1"],
                Exact,
            ),
        },
        62 | 63 => {
            // Far-future or past targets with a condition: the outcome
            // depends only on whether the key has an expiry and which side
            // of it the target falls, never on timing.
            let (flag, opt) = *rng.pick(&[
                ("--nx", "NX"),
                ("--xx", "XX"),
                ("--gt", "GT"),
                ("--lt", "LT"),
            ]);
            let at = *rng.pick(&["1", "8888888888888", "9999999999999"]);
            g(
                &["p-expire-at", flag, "--", k, at],
                &["PEXPIREAT", k, at, opt],
                Exact,
            )
        }
        44 => {
            // Not the largest EX/PX that Redis accepts: there `now + ttl`
            // wraps negative inside Redis and the key vanishes on the spot,
            // an overflow artefact klyv does not copy (it saturates).
            let x = if x == "9223372036854775" { "100000" } else { x };
            let ex = format!("--ex={x}");
            g(&["set", &ex, "--", k, v], &["SET", k, v, "EX", x], Exact)
        }
        _ => {
            // Expiry with deterministic outcomes only: far future or past.
            // (ttl/p-ttl values, s-pop and db-size are timing- or
            // randomness-dependent and are covered by the fixed scenarios.)
            if rng.chance(3) {
                g(&["expire-at", "--", k, "1"], &["EXPIREAT", k, "1"], Exact)
            } else {
                g(
                    &["expire-at", "--", k, "9999999999"],
                    &["EXPIREAT", k, "9999999999"],
                    Exact,
                )
            }
        }
    }
}

/// The schema-v3 invariant on a klyv database: the catalogue's stored length
/// equals the real number of payload rows for every list, set and hash, is
/// NULL for strings, and no key is catalogued with nothing in it. Redis
/// agreeing on LLEN/SCARD/HLEN only proves the lengths that were asked for;
/// this proves all of them, after arbitrary command sequences.
fn assert_len_invariant(db: &std::path::Path, ctx: &dyn Fn() -> String) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    let bad: Vec<String> = conn
        .prepare(
            "SELECT quote(key) || ' ' || type || ' len=' || coalesce(len, 'NULL') || ' rows=' || n
             FROM (SELECT key, type, len, CASE type
                     WHEN 'list' THEN (SELECT COUNT(*) FROM list_items  WHERE key = k.key)
                     WHEN 'set'  THEN (SELECT COUNT(*) FROM set_members WHERE key = k.key)
                     WHEN 'hash' THEN (SELECT COUNT(*) FROM hash_fields WHERE key = k.key)
                     ELSE (SELECT COUNT(*) FROM strings WHERE key = k.key)
                   END AS n FROM keyspace k)
             WHERE n = 0 OR (type = 'string' AND len IS NOT NULL)
                OR (type != 'string' AND len IS NOT n)",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        bad.is_empty(),
        "length invariant broken: {bad:?}\n{}",
        ctx()
    );
}

fn is_redis_error(out: &str) -> bool {
    out.starts_with("WRONGTYPE ") || out.starts_with("ERR ")
}

fn check_generated(db: &str, port: u16, g: &Gen, ctx: &dyn Fn() -> String) {
    let kargs: Vec<&str> = g.klyv.iter().map(String::as_str).collect();
    let rargs: Vec<&str> = g.redis.iter().map(String::as_str).collect();
    let (k_out, k_err, k_ok) = run_klyv(db, &kargs);
    let r_out = run_redis(port, &rargs);
    let detail = || {
        format!(
            "{}\n  klyv {:?}\n  redis {:?}\n  klyv stdout: {k_out:?}\n  klyv stderr: {k_err:?}\n  redis stdout: {r_out:?}",
            ctx(),
            g.klyv,
            g.redis
        )
    };
    let r_err = is_redis_error(&r_out);
    assert_eq!(
        !k_ok,
        r_err,
        "one side errored and the other did not\n{}",
        detail()
    );
    if !k_ok {
        assert_eq!(
            first_token(&k_err),
            first_token(&r_out),
            "error code mismatch\n{}",
            detail()
        );
        return;
    }
    match g.ok_cmp {
        Cmp::Exact => {
            let equal = k_out == r_out || (r_out == "\n" && k_out.is_empty());
            assert!(equal, "output mismatch\n{}", detail());
        }
        Cmp::Sorted => assert_eq!(
            sorted_lines(&k_out),
            sorted_lines(&r_out),
            "sorted mismatch\n{}",
            detail()
        ),
        Cmp::PairSorted => assert_eq!(
            sorted_pairs(&k_out),
            sorted_pairs(&r_out),
            "pair mismatch\n{}",
            detail()
        ),
        Cmp::Error | Cmp::ScanAll | Cmp::ScanAllPairs => {
            unreachable!("generated steps never pre-declare an error or a scan")
        }
    }
}

#[test]
fn differential_random_sequences() {
    if !have("redis-server") || !have("redis-cli") {
        eprintln!("skipping differential test: redis-server/redis-cli not on PATH");
        return;
    }
    let steps: usize = std::env::var("KLYV_DIFF_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(250);
    let seeds: Vec<u64> = match std::env::var("KLYV_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        Some(seed) => vec![seed],
        None => vec![0x5EED_0001, 0x5EED_0002, 0x5EED_0003],
    };
    let redis = start_redis();
    let dir = tempfile::tempdir().unwrap();

    for seed in seeds {
        let db = dir.path().join(format!("random-{seed:x}.db"));
        let db = db.to_str().unwrap();
        run_redis(redis.port, &["FLUSHALL"]);
        let mut rng = Rng(seed);
        let mut history: Vec<String> = Vec::new();
        for i in 0..steps {
            let g = random_step(&mut rng);
            history.push(format!("{i:>4}: {}", g.klyv.join(" ")));
            let ctx = || {
                let from = history.len().saturating_sub(15);
                format!(
                    "seed {seed:#x} step {i} (replay with KLYV_DIFF_SEED={seed}); recent history:\n{}",
                    history[from..].join("\n")
                )
            };
            check_generated(db, redis.port, &g, &ctx);
            if i % 25 == 24 || i + 1 == steps {
                assert_len_invariant(std::path::Path::new(db), &ctx);
            }
        }
    }
}

// === Server mode ===
//
// The same scenarios and random sequences again, but through `klyv serve`,
// compared with real Redis at the wire level: a minimal RESP client here
// sends each command to both servers over a socket and compares the parsed
// replies, *including their types*. That is stricter than anything a client
// program can show: `$-1` versus `*-1`, a simple string versus a bulk string,
// or (in RESP3) an array versus a set or a map are all differences here, and
// error messages must match in full, not just by code. Each test runs once
// per protocol version.

struct KlyvServer {
    child: Child,
    port: u16,
}

impl Drop for KlyvServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_klyv_server(db: &std::path::Path) -> KlyvServer {
    use std::io::BufRead;
    let mut child = Command::new(env!("CARGO_BIN_EXE_klyv"))
        .arg("--db")
        .arg(db)
        .args(["serve", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start klyv serve");
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line
        .trim()
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("unexpected server announcement: {line:?}"));
    KlyvServer { child, port }
}

/// A reply as it appears on the wire, type tag and all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Wire {
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(Vec<u8>),
    /// RESP2 `$-1`.
    NullBulk,
    /// RESP2 `*-1`.
    NullArray,
    /// RESP3 `_`.
    Null,
    Array(Vec<Wire>),
    /// RESP3 `~`.
    Set(Vec<Wire>),
    /// RESP3 `%`.
    Map(Vec<(Wire, Wire)>),
    /// Any other RESP3 scalar (boolean, double, big number), kept verbatim.
    Other(char, String),
}

struct WireClient {
    reader: std::io::BufReader<std::net::TcpStream>,
    writer: std::net::TcpStream,
}

impl WireClient {
    fn connect(port: u16, resp3: bool) -> Self {
        let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let mut client = WireClient {
            reader: std::io::BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
        };
        if resp3 {
            let hello = client.cmd(&["HELLO", "3"]);
            assert!(matches!(hello, Wire::Map(_)), "HELLO 3 gave {hello:?}");
        }
        client
    }

    fn line(&mut self) -> String {
        use std::io::BufRead;
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read failed");
        assert!(line.ends_with("\r\n"), "unterminated reply line {line:?}");
        line.truncate(line.len() - 2);
        line
    }

    fn read(&mut self) -> Wire {
        use std::io::Read;
        let line = self.line();
        let kind = line.chars().next().expect("empty reply line");
        let rest = &line[1..];
        let count = |rest: &str| rest.parse::<i64>().expect("bad length");
        match kind {
            '+' => Wire::Simple(rest.to_string()),
            '-' => Wire::Error(rest.to_string()),
            ':' => Wire::Int(rest.parse().expect("bad integer")),
            '_' => Wire::Null,
            '$' | '=' | '!' => {
                let len = count(rest);
                if len < 0 {
                    return Wire::NullBulk;
                }
                let mut buf = vec![0u8; len as usize + 2];
                self.reader.read_exact(&mut buf).unwrap();
                buf.truncate(len as usize);
                match kind {
                    '$' => Wire::Bulk(buf),
                    '!' => Wire::Error(String::from_utf8_lossy(&buf).into_owned()),
                    _ => Wire::Other(kind, String::from_utf8_lossy(&buf).into_owned()),
                }
            }
            '*' | '~' | '>' => {
                let n = count(rest);
                if n < 0 {
                    return Wire::NullArray;
                }
                let items = (0..n).map(|_| self.read()).collect();
                if kind == '~' {
                    Wire::Set(items)
                } else {
                    Wire::Array(items)
                }
            }
            '%' => Wire::Map(
                (0..count(rest))
                    .map(|_| {
                        let k = self.read();
                        (k, self.read())
                    })
                    .collect(),
            ),
            other => Wire::Other(other, rest.to_string()),
        }
    }

    fn cmd(&mut self, args: &[&str]) -> Wire {
        use std::io::Write;
        let mut out = format!("*{}\r\n", args.len()).into_bytes();
        for a in args {
            out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
            out.extend_from_slice(a.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        self.writer.write_all(&out).unwrap();
        self.read()
    }
}

/// Runs a SCAN-family command to completion, substituting the cursor for the
/// `CURSOR` placeholder, and returns everything it yielded, sorted. Also
/// checks the shape of every page and that the iteration terminates.
fn scan_all(client: &mut WireClient, template: &[&str], pairs: bool) -> Wire {
    let mut cursor = String::from("0");
    let mut items = Vec::new();
    for _ in 0..10_000 {
        let args: Vec<&str> = template
            .iter()
            .map(|a| if *a == "CURSOR" { cursor.as_str() } else { a })
            .collect();
        let Wire::Array(page) = client.cmd(&args) else {
            panic!("{args:?}: a scan page must be an array");
        };
        let [Wire::Bulk(next), Wire::Array(found)] = page.as_slice() else {
            panic!("{args:?}: a scan page must be [cursor, items], got {page:?}");
        };
        items.extend(found.iter().cloned());
        cursor = String::from_utf8(next.clone()).unwrap();
        if cursor == "0" {
            if pairs {
                let mut pairs: Vec<Vec<Wire>> = items.chunks(2).map(<[Wire]>::to_vec).collect();
                pairs.sort();
                pairs.dedup();
                return Wire::Array(pairs.into_iter().flatten().collect());
            }
            items.sort();
            // A full iteration may repeat an item; it may not omit one.
            items.dedup();
            return Wire::Array(items);
        }
    }
    panic!("{template:?}: scan did not terminate");
}

/// Puts an unordered reply into a canonical order, keeping its type.
fn canonical(reply: Wire, cmp: Cmp) -> Wire {
    match (cmp, reply) {
        (Cmp::Sorted, Wire::Array(mut items)) => {
            items.sort();
            Wire::Array(items)
        }
        (Cmp::Sorted | Cmp::PairSorted, Wire::Set(mut items)) => {
            items.sort();
            Wire::Set(items)
        }
        (Cmp::PairSorted, Wire::Map(mut pairs)) => {
            pairs.sort();
            Wire::Map(pairs)
        }
        // RESP2 hash reply: a flat array of alternating fields and values.
        (Cmp::PairSorted, Wire::Array(items)) => {
            let mut pairs: Vec<Vec<Wire>> = items.chunks(2).map(<[Wire]>::to_vec).collect();
            pairs.sort();
            Wire::Array(pairs.into_iter().flatten().collect())
        }
        (_, other) => other,
    }
}

fn server_scenarios(resp3: bool) {
    if !have("redis-server") || !have("redis-cli") {
        eprintln!("skipping differential test: redis-server/redis-cli not on PATH");
        return;
    }
    let redis = start_redis();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("server.db");
    let server = start_klyv_server(&db_path);
    let mut k = WireClient::connect(server.port, resp3);
    let mut r = WireClient::connect(redis.port, resp3);

    for (name, steps) in scenarios() {
        r.cmd(&["FLUSHALL"]);
        k.cmd(&["FLUSHALL"]);
        for (j, s) in steps.iter().enumerate() {
            let (k_out, r_out) = match s.cmp {
                Cmp::ScanAll | Cmp::ScanAllPairs => {
                    let pairs = matches!(s.cmp, Cmp::ScanAllPairs);
                    (
                        scan_all(&mut k, s.redis, pairs),
                        scan_all(&mut r, s.redis, pairs),
                    )
                }
                _ => (
                    canonical(k.cmd(s.redis), s.cmp),
                    canonical(r.cmd(s.redis), s.cmp),
                ),
            };
            assert_eq!(
                k_out, r_out,
                "scenario '{name}' step {j} (resp3={resp3}): {:?}\n  left: klyv server, right: redis",
                s.redis
            );
            if matches!(s.cmp, Cmp::Error) {
                assert!(
                    matches!(r_out, Wire::Error(_)),
                    "expected an error: {r_out:?}"
                );
            }
            assert_len_invariant(&db_path, &|| {
                format!(
                    "scenario '{name}' after step {j} (resp3={resp3}): {:?}",
                    s.redis
                )
            });
        }
    }
}

#[test]
fn differential_server_resp2() {
    server_scenarios(false);
}

#[test]
fn differential_server_resp3() {
    server_scenarios(true);
}

fn server_random(resp3: bool) {
    if !have("redis-server") || !have("redis-cli") {
        eprintln!("skipping differential test: redis-server/redis-cli not on PATH");
        return;
    }
    // A socket round trip is far cheaper than spawning a process per step,
    // so this layer runs four times as many steps as the CLI one.
    let steps: usize = std::env::var("KLYV_DIFF_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(250)
        * 4;
    let seeds: Vec<u64> = match std::env::var("KLYV_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        Some(seed) => vec![seed],
        None => vec![0x5EED_0001, 0x5EED_0002, 0x5EED_0003],
    };
    let redis = start_redis();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("server-random.db");
    let server = start_klyv_server(&db_path);
    let mut k = WireClient::connect(server.port, resp3);
    let mut r = WireClient::connect(redis.port, resp3);

    for seed in seeds {
        r.cmd(&["FLUSHALL"]);
        k.cmd(&["FLUSHALL"]);
        let mut rng = Rng(seed);
        let mut history: Vec<String> = Vec::new();
        for i in 0..steps {
            let g = random_step(&mut rng);
            let args: Vec<&str> = g.redis.iter().map(String::as_str).collect();
            history.push(format!("{i:>4}: {}", g.redis.join(" ")));
            let k_out = canonical(k.cmd(&args), g.ok_cmp);
            let r_out = canonical(r.cmd(&args), g.ok_cmp);
            let recent = || {
                let from = history.len().saturating_sub(15);
                format!(
                    "seed {seed:#x} step {i} resp3={resp3} (replay with KLYV_DIFF_SEED={seed}); recent history:\n{}",
                    history[from..].join("\n")
                )
            };
            if k_out != r_out {
                panic!(
                    "{}\n  klyv server: {k_out:?}\n  redis:       {r_out:?}",
                    recent()
                );
            }
            if i % 50 == 49 || i + 1 == steps {
                assert_len_invariant(&db_path, &recent);
            }
        }
    }
}

#[test]
fn differential_server_random_resp2() {
    server_random(false);
}

#[test]
fn differential_server_random_resp3() {
    server_random(true);
}
