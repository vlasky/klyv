use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::NamedTempFile;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn klyv(db: &str, args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_klyv"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .expect("failed to run klyv");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

fn fresh_db() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let f = NamedTempFile::new().unwrap();
    let path = f.into_temp_path();
    let s = format!("{}.{}", path.display(), n);
    s
}

// === STRING COMMANDS ===

#[test]
fn test_set_get() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["set", "key1", "hello"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");

    let (out, _, ok) = klyv(&db, &["get", "key1"]);
    assert!(ok);
    assert_eq!(out.trim(), "hello");
}

#[test]
fn test_get_nonexistent() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["get", "nosuchkey"]);
    assert!(ok);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_set_overwrite() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "first"]);
    klyv(&db, &["set", "k", "second"]);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "second");
}

#[test]
fn test_del_single() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    let (out, _, ok) = klyv(&db, &["del", "a"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["get", "a"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_del_multiple() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    klyv(&db, &["set", "b", "2"]);
    klyv(&db, &["set", "c", "3"]);
    let (out, _, _) = klyv(&db, &["del", "a", "b", "nonexistent"]);
    assert_eq!(out.trim(), "(integer) 2");
}

#[test]
fn test_del_nonexistent() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["del", "nope"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_del_list_counts_keys_not_rows() {
    let db = fresh_db();
    klyv(&db, &["r-push", "mylist", "a", "b", "c", "d", "e"]);
    let (out, _, _) = klyv(&db, &["del", "mylist"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_del_duplicate_key_counts_once() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, _) = klyv(&db, &["del", "k", "k"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_del_expired_key_counts_zero_but_reclaims_rows() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire-at", "k", "0"]);

    let (out, _, _) = klyv(&db, &["del", "k"]);
    assert_eq!(out.trim(), "(integer) 0");

    // Physical rows are reclaimed even though the count is 0.
    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 0");
    let (out, _, _) = klyv(&db, &["purge"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_incr_new_key() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["incr", "counter"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_incr_existing() {
    let db = fresh_db();
    klyv(&db, &["set", "n", "10"]);
    let (out, _, _) = klyv(&db, &["incr", "n"]);
    assert_eq!(out.trim(), "(integer) 11");
}

#[test]
fn test_incr_non_integer() {
    let db = fresh_db();
    klyv(&db, &["set", "s", "notanumber"]);
    let (_, _, ok) = klyv(&db, &["incr", "s"]);
    assert!(!ok);
}

#[test]
fn test_decr() {
    let db = fresh_db();
    klyv(&db, &["set", "n", "5"]);
    let (out, _, _) = klyv(&db, &["decr", "n"]);
    assert_eq!(out.trim(), "(integer) 4");
}

#[test]
fn test_decr_below_zero() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["decr", "x"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_incrby() {
    let db = fresh_db();
    klyv(&db, &["set", "n", "10"]);
    let (out, _, _) = klyv(&db, &["incr-by", "n", "5"]);
    assert_eq!(out.trim(), "(integer) 15");
}

#[test]
fn test_decrby() {
    let db = fresh_db();
    klyv(&db, &["set", "n", "10"]);
    let (out, _, _) = klyv(&db, &["decr-by", "n", "3"]);
    assert_eq!(out.trim(), "(integer) 7");
}

#[test]
fn test_append_new_key() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["append", "k", "hello"]);
    assert_eq!(out.trim(), "(integer) 5");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "hello");
}

#[test]
fn test_append_existing() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "hello"]);
    let (out, _, _) = klyv(&db, &["append", "k", " world"]);
    assert_eq!(out.trim(), "(integer) 11");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "hello world");
}

#[test]
fn test_strlen() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "hello"]);
    let (out, _, _) = klyv(&db, &["strlen", "k"]);
    assert_eq!(out.trim(), "(integer) 5");
}

#[test]
fn test_strlen_nonexistent() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["strlen", "nope"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_mset_mget() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["m-set", "a", "1", "b", "2", "c", "3"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");

    let (out, _, _) = klyv(&db, &["m-get", "a", "b", "c", "nonexistent"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["1", "2", "3", "(nil)"]);
}

#[test]
fn test_mset_odd_args() {
    let db = fresh_db();
    let (_, stderr, ok) = klyv(&db, &["m-set", "a", "1", "b"]);
    assert!(!ok);
    assert!(stderr.contains("ERR"));
}

// === LIST COMMANDS ===

#[test]
fn test_rpush_lpush() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["r-push", "list", "a", "b", "c"]);
    assert_eq!(out.trim(), "(integer) 3");

    let (out, _, _) = klyv(&db, &["l-push", "list", "z"]);
    assert_eq!(out.trim(), "(integer) 4");

    let (out, _, _) = klyv(&db, &["l-range", "list", "0", "-1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "1) \"z\"");
    assert_eq!(lines[1], "2) \"a\"");
    assert_eq!(lines[2], "3) \"b\"");
    assert_eq!(lines[3], "4) \"c\"");
}

#[test]
fn test_lpop_rpop() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b", "c"]);

    let (out, _, _) = klyv(&db, &["l-pop", "list"]);
    assert_eq!(out.trim(), "a");

    let (out, _, _) = klyv(&db, &["r-pop", "list"]);
    assert_eq!(out.trim(), "c");

    let (out, _, _) = klyv(&db, &["l-len", "list"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_lpop_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["l-pop", "empty"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_rpop_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["r-pop", "empty"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_lrange_negative_indices() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b", "c", "d", "e"]);

    let (out, _, _) = klyv(&db, &["l-range", "list", "-3", "-1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "1) \"c\"");
    assert_eq!(lines[1], "2) \"d\"");
    assert_eq!(lines[2], "3) \"e\"");
}

#[test]
fn test_lrange_out_of_bounds() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b"]);

    let (out, _, _) = klyv(&db, &["l-range", "list", "0", "100"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 2);
}

#[test]
fn test_lrange_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["l-range", "nope", "0", "-1"]);
    assert_eq!(out.trim(), "(empty list)");
}

#[test]
fn test_lrange_inverted() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b", "c"]);
    let (out, _, _) = klyv(&db, &["l-range", "list", "2", "0"]);
    assert_eq!(out.trim(), "(empty list)");
}

#[test]
fn test_llen_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["l-len", "nope"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_lrem_all() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b", "a", "c", "a"]);
    let (out, _, _) = klyv(&db, &["l-rem", "list", "0", "a"]);
    assert_eq!(out.trim(), "(integer) 3");

    let (out, _, _) = klyv(&db, &["l-len", "list"]);
    assert_eq!(out.trim(), "(integer) 2");
}

#[test]
fn test_lrem_count_positive() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b", "a", "c", "a"]);
    let (out, _, _) = klyv(&db, &["l-rem", "list", "2", "a"]);
    assert_eq!(out.trim(), "(integer) 2");

    let (out, _, _) = klyv(&db, &["l-range", "list", "0", "-1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2], "3) \"a\"");
}

#[test]
fn test_lrem_count_negative() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b", "a", "c", "a"]);
    let (out, _, _) = klyv(&db, &["l-rem", "list", "-1", "a"]);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["l-range", "list", "0", "-1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[3], "4) \"c\"");
}

#[test]
fn test_lrem_not_found() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b"]);
    let (out, _, _) = klyv(&db, &["l-rem", "list", "0", "z"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_lpos_found() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b", "c", "b"]);
    let (out, _, _) = klyv(&db, &["l-pos", "list", "b"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_lpos_not_found() {
    let db = fresh_db();
    klyv(&db, &["r-push", "list", "a", "b"]);
    let (out, _, _) = klyv(&db, &["l-pos", "list", "z"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_lpos_empty_list() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["l-pos", "empty", "a"]);
    assert_eq!(out.trim(), "(nil)");
}

// === SET COMMANDS ===

#[test]
fn test_sadd_smembers() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["s-add", "myset", "a", "b", "c"]);
    assert_eq!(out.trim(), "(integer) 3");

    let (out, _, _) = klyv(&db, &["s-members", "myset"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 3);
}

#[test]
fn test_sadd_duplicates() {
    let db = fresh_db();
    klyv(&db, &["s-add", "myset", "a", "b"]);
    let (out, _, _) = klyv(&db, &["s-add", "myset", "b", "c"]);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["s-card", "myset"]);
    assert_eq!(out.trim(), "(integer) 3");
}

#[test]
fn test_srem() {
    let db = fresh_db();
    klyv(&db, &["s-add", "myset", "a", "b", "c"]);
    let (out, _, _) = klyv(&db, &["s-rem", "myset", "b", "nonexistent"]);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["s-card", "myset"]);
    assert_eq!(out.trim(), "(integer) 2");
}

#[test]
fn test_sismember() {
    let db = fresh_db();
    klyv(&db, &["s-add", "myset", "a", "b"]);

    let (out, _, _) = klyv(&db, &["s-is-member", "myset", "a"]);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["s-is-member", "myset", "z"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_sismember_nonexistent_set() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["s-is-member", "nope", "a"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_scard_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["s-card", "nope"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_smembers_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["s-members", "nope"]);
    assert_eq!(out.trim(), "(empty set)");
}

#[test]
fn test_sunion() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a", "b", "c"]);
    klyv(&db, &["s-add", "s2", "c", "d", "e"]);

    let (out, _, _) = klyv(&db, &["s-union", "s1", "s2"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 5);
}

#[test]
fn test_sinter() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a", "b", "c"]);
    klyv(&db, &["s-add", "s2", "b", "c", "d"]);

    let (out, _, _) = klyv(&db, &["s-inter", "s1", "s2"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 2);
    let content = out.to_string();
    assert!(content.contains("\"b\""));
    assert!(content.contains("\"c\""));
}

#[test]
fn test_sinter_disjoint() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a", "b"]);
    klyv(&db, &["s-add", "s2", "c", "d"]);

    let (out, _, _) = klyv(&db, &["s-inter", "s1", "s2"]);
    assert_eq!(out.trim(), "(empty set)");
}

#[test]
fn test_sdiff() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a", "b", "c"]);
    klyv(&db, &["s-add", "s2", "b", "c", "d"]);

    let (out, _, _) = klyv(&db, &["s-diff", "s1", "s2"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 1);
    assert!(out.contains("\"a\""));
}

#[test]
fn test_sdiff_single_set() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a", "b", "c"]);

    let (out, _, _) = klyv(&db, &["s-diff", "s1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 3);
}

#[test]
fn test_sunion_with_nonexistent() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a", "b"]);

    let (out, _, _) = klyv(&db, &["s-union", "s1", "nonexistent"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 2);
}

// === HASH COMMANDS ===

#[test]
fn test_hset_hget() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["h-set", "myhash", "name", "alice", "age", "30"]);
    assert_eq!(out.trim(), "(integer) 2");

    let (out, _, _) = klyv(&db, &["h-get", "myhash", "name"]);
    assert_eq!(out.trim(), "alice");

    let (out, _, _) = klyv(&db, &["h-get", "myhash", "age"]);
    assert_eq!(out.trim(), "30");
}

#[test]
fn test_hget_nonexistent_field() {
    let db = fresh_db();
    klyv(&db, &["h-set", "myhash", "a", "1"]);
    let (out, _, _) = klyv(&db, &["h-get", "myhash", "nope"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_hget_nonexistent_key() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["h-get", "nope", "field"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_hset_overwrite() {
    let db = fresh_db();
    klyv(&db, &["h-set", "h", "f", "old"]);
    let (out, _, _) = klyv(&db, &["h-set", "h", "f", "new"]);
    assert_eq!(out.trim(), "(integer) 0");

    let (out, _, _) = klyv(&db, &["h-get", "h", "f"]);
    assert_eq!(out.trim(), "new");
}

#[test]
fn test_hset_odd_args() {
    let db = fresh_db();
    let (_, stderr, ok) = klyv(&db, &["h-set", "h", "field"]);
    assert!(!ok);
    assert!(stderr.contains("ERR"));
}

#[test]
fn test_hdel() {
    let db = fresh_db();
    klyv(&db, &["h-set", "h", "a", "1", "b", "2", "c", "3"]);
    let (out, _, _) = klyv(&db, &["h-del", "h", "a", "nonexistent"]);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["h-len", "h"]);
    assert_eq!(out.trim(), "(integer) 2");
}

#[test]
fn test_hgetall() {
    let db = fresh_db();
    klyv(&db, &["h-set", "h", "a", "1", "b", "2"]);
    let (out, _, _) = klyv(&db, &["h-get-all", "h"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 4);
}

#[test]
fn test_hgetall_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["h-get-all", "nope"]);
    assert_eq!(out.trim(), "(empty hash)");
}

#[test]
fn test_hkeys_hvals() {
    let db = fresh_db();
    klyv(&db, &["h-set", "h", "name", "alice", "age", "30"]);

    let (out, _, _) = klyv(&db, &["h-keys", "h"]);
    assert!(out.contains("\"name\""));
    assert!(out.contains("\"age\""));

    let (out, _, _) = klyv(&db, &["h-vals", "h"]);
    assert!(out.contains("\"alice\""));
    assert!(out.contains("\"30\""));
}

#[test]
fn test_hlen() {
    let db = fresh_db();
    klyv(&db, &["h-set", "h", "a", "1", "b", "2"]);
    let (out, _, _) = klyv(&db, &["h-len", "h"]);
    assert_eq!(out.trim(), "(integer) 2");
}

#[test]
fn test_hlen_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["h-len", "nope"]);
    assert_eq!(out.trim(), "(integer) 0");
}

// === KEY COMMANDS ===

#[test]
fn test_keys_all() {
    let db = fresh_db();
    klyv(&db, &["set", "str1", "v"]);
    klyv(&db, &["r-push", "list1", "a"]);
    klyv(&db, &["s-add", "set1", "x"]);
    klyv(&db, &["h-set", "hash1", "f", "v"]);

    let (out, _, _) = klyv(&db, &["keys"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 4);
}

#[test]
fn test_keys_pattern() {
    let db = fresh_db();
    klyv(&db, &["set", "user:1", "alice"]);
    klyv(&db, &["set", "user:2", "bob"]);
    klyv(&db, &["set", "post:1", "hello"]);

    let (out, _, _) = klyv(&db, &["keys", "user:*"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 2);
}

#[test]
fn test_keys_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["keys"]);
    assert_eq!(out.trim(), "(empty list)");
}

#[test]
fn test_exists() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);

    let (out, _, _) = klyv(&db, &["exists", "k"]);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["exists", "nope"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_exists_all_types() {
    let db = fresh_db();
    klyv(&db, &["r-push", "mylist", "a"]);
    let (out, _, _) = klyv(&db, &["exists", "mylist"]);
    assert_eq!(out.trim(), "(integer) 1");

    klyv(&db, &["s-add", "myset", "a"]);
    let (out, _, _) = klyv(&db, &["exists", "myset"]);
    assert_eq!(out.trim(), "(integer) 1");

    klyv(&db, &["h-set", "myhash", "f", "v"]);
    let (out, _, _) = klyv(&db, &["exists", "myhash"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_type_string() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, _) = klyv(&db, &["type", "k"]);
    assert_eq!(out.trim(), "string");
}

#[test]
fn test_type_list() {
    let db = fresh_db();
    klyv(&db, &["r-push", "k", "a"]);
    let (out, _, _) = klyv(&db, &["type", "k"]);
    assert_eq!(out.trim(), "list");
}

#[test]
fn test_type_set() {
    let db = fresh_db();
    klyv(&db, &["s-add", "k", "a"]);
    let (out, _, _) = klyv(&db, &["type", "k"]);
    assert_eq!(out.trim(), "set");
}

#[test]
fn test_type_hash() {
    let db = fresh_db();
    klyv(&db, &["h-set", "k", "f", "v"]);
    let (out, _, _) = klyv(&db, &["type", "k"]);
    assert_eq!(out.trim(), "hash");
}

#[test]
fn test_type_none() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["type", "nope"]);
    assert_eq!(out.trim(), "none");
}

#[test]
fn test_rename_string() {
    let db = fresh_db();
    klyv(&db, &["set", "old", "value"]);
    let (out, _, ok) = klyv(&db, &["rename", "old", "new"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");

    let (out, _, _) = klyv(&db, &["get", "new"]);
    assert_eq!(out.trim(), "value");

    let (out, _, _) = klyv(&db, &["get", "old"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_rename_nonexistent() {
    let db = fresh_db();
    let (_, stderr, ok) = klyv(&db, &["rename", "nope", "new"]);
    assert!(!ok);
    assert!(stderr.contains("ERR no such key"));
}

#[test]
fn test_rename_overwrites_target() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "keep"]);
    klyv(&db, &["set", "b", "discard"]);
    klyv(&db, &["rename", "a", "b"]);

    let (out, _, _) = klyv(&db, &["get", "b"]);
    assert_eq!(out.trim(), "keep");

    let (out, _, _) = klyv(&db, &["exists", "a"]);
    assert_eq!(out.trim(), "(integer) 0");
}

// === UTILITY COMMANDS ===

#[test]
fn test_dbsize() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    klyv(&db, &["r-push", "b", "x"]);
    klyv(&db, &["s-add", "c", "y"]);
    klyv(&db, &["h-set", "d", "f", "v"]);

    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 4");
}

#[test]
fn test_dbsize_empty() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_flushall() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    klyv(&db, &["r-push", "b", "x"]);
    klyv(&db, &["s-add", "c", "y"]);
    klyv(&db, &["h-set", "d", "f", "v"]);

    let (out, _, ok) = klyv(&db, &["flush-all"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");

    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 0");
}

// === EDGE CASES ===

#[test]
fn test_empty_string_value() {
    let db = fresh_db();
    klyv(&db, &["set", "k", ""]);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "");
    let (out, _, _) = klyv(&db, &["strlen", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_key_with_spaces() {
    let db = fresh_db();
    klyv(&db, &["set", "my key", "my value"]);
    let (out, _, _) = klyv(&db, &["get", "my key"]);
    assert_eq!(out.trim(), "my value");
}

#[test]
fn test_key_with_special_chars() {
    let db = fresh_db();
    klyv(&db, &["set", "key:with:colons", "v1"]);
    klyv(&db, &["set", "key/with/slashes", "v2"]);
    klyv(&db, &["set", "key.with.dots", "v3"]);

    let (out, _, _) = klyv(&db, &["get", "key:with:colons"]);
    assert_eq!(out.trim(), "v1");
    let (out, _, _) = klyv(&db, &["get", "key/with/slashes"]);
    assert_eq!(out.trim(), "v2");
    let (out, _, _) = klyv(&db, &["get", "key.with.dots"]);
    assert_eq!(out.trim(), "v3");
}

#[test]
fn test_value_with_newlines() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "line1\nline2"]);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "line1\nline2");
}

#[test]
fn test_large_incr() {
    let db = fresh_db();
    klyv(&db, &["set", "n", "9223372036854775800"]);
    let (out, _, _) = klyv(&db, &["incr-by", "n", "6"]);
    assert_eq!(out.trim(), "(integer) 9223372036854775806");
}

#[test]
fn test_del_across_types() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "string_val"]);
    klyv(&db, &["r-push", "k2", "list_val"]);

    let (out, _, _) = klyv(&db, &["del", "k", "k2"]);
    assert_eq!(out.trim(), "(integer) 2");

    let (out, _, _) = klyv(&db, &["exists", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
    let (out, _, _) = klyv(&db, &["exists", "k2"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_multiple_lpush_ordering() {
    let db = fresh_db();
    klyv(&db, &["l-push", "list", "c", "b", "a"]);
    let (out, _, _) = klyv(&db, &["l-range", "list", "0", "-1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "1) \"a\"");
    assert_eq!(lines[1], "2) \"b\"");
    assert_eq!(lines[2], "3) \"c\"");
}

#[test]
fn test_set_operations_empty_sets() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a", "b"]);

    let (out, _, _) = klyv(&db, &["s-inter", "s1", "empty"]);
    assert_eq!(out.trim(), "(empty set)");

    let (out, _, _) = klyv(&db, &["s-diff", "empty", "s1"]);
    assert_eq!(out.trim(), "(empty set)");
}

// === TTL COMMANDS ===

#[test]
fn test_ttl_no_expiry() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_ttl_nonexistent_key() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["ttl", "nope"]);
    assert_eq!(out.trim(), "(integer) -2");
}

#[test]
fn test_expire_sets_ttl() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, ok) = klyv(&db, &["expire", "k", "100"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let ttl: i64 = out
        .trim()
        .strip_prefix("(integer) ")
        .unwrap()
        .parse()
        .unwrap();
    assert!(ttl > 0 && ttl <= 100);
}

#[test]
fn test_expire_nonexistent_key() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["expire", "nope", "60"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_expire_key_becomes_invisible() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire-at", "k", "0"]);

    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");

    let (out, _, _) = klyv(&db, &["exists", "k"]);
    assert_eq!(out.trim(), "(integer) 0");

    let (out, _, _) = klyv(&db, &["type", "k"]);
    assert_eq!(out.trim(), "none");

    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -2");
}

#[test]
fn test_expire_hides_from_keys() {
    let db = fresh_db();
    klyv(&db, &["set", "visible", "v"]);
    klyv(&db, &["set", "hidden", "v"]);
    klyv(&db, &["expire-at", "hidden", "0"]);

    let (out, _, _) = klyv(&db, &["keys"]);
    assert!(out.contains("\"visible\""));
    assert!(!out.contains("\"hidden\""));
}

#[test]
fn test_expire_list() {
    let db = fresh_db();
    klyv(&db, &["r-push", "mylist", "a", "b", "c"]);
    klyv(&db, &["expire-at", "mylist", "0"]);

    let (out, _, _) = klyv(&db, &["l-len", "mylist"]);
    assert_eq!(out.trim(), "(integer) 0");

    let (out, _, _) = klyv(&db, &["l-range", "mylist", "0", "-1"]);
    assert_eq!(out.trim(), "(empty list)");

    let (out, _, _) = klyv(&db, &["l-pop", "mylist"]);
    assert_eq!(out.trim(), "(nil)");

    let (out, _, _) = klyv(&db, &["l-pos", "mylist", "a"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_expire_set() {
    let db = fresh_db();
    klyv(&db, &["s-add", "myset", "a", "b"]);
    klyv(&db, &["expire-at", "myset", "0"]);

    let (out, _, _) = klyv(&db, &["s-members", "myset"]);
    assert_eq!(out.trim(), "(empty set)");

    let (out, _, _) = klyv(&db, &["s-is-member", "myset", "a"]);
    assert_eq!(out.trim(), "(integer) 0");

    let (out, _, _) = klyv(&db, &["s-card", "myset"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_expire_hash() {
    let db = fresh_db();
    klyv(&db, &["h-set", "myhash", "f", "v"]);
    klyv(&db, &["expire-at", "myhash", "0"]);

    let (out, _, _) = klyv(&db, &["h-get", "myhash", "f"]);
    assert_eq!(out.trim(), "(nil)");

    let (out, _, _) = klyv(&db, &["h-get-all", "myhash"]);
    assert_eq!(out.trim(), "(empty hash)");

    let (out, _, _) = klyv(&db, &["h-keys", "myhash"]);
    assert_eq!(out.trim(), "(empty list)");

    let (out, _, _) = klyv(&db, &["h-vals", "myhash"]);
    assert_eq!(out.trim(), "(empty list)");

    let (out, _, _) = klyv(&db, &["h-len", "myhash"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_persist_removes_expiry() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "100"]);

    let (out, _, _) = klyv(&db, &["persist", "k"]);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_persist_no_expiry() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, _) = klyv(&db, &["persist", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_pexpire() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, ok) = klyv(&db, &["p-expire", "k", "60000"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let ttl: i64 = out
        .trim()
        .strip_prefix("(integer) ")
        .unwrap()
        .parse()
        .unwrap();
    assert!(ttl > 0 && ttl <= 60);
}

#[test]
fn test_expire_at_future() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, ok) = klyv(&db, &["expire-at", "k", "9999999999"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "v");

    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let ttl: i64 = out
        .trim()
        .strip_prefix("(integer) ")
        .unwrap()
        .parse()
        .unwrap();
    assert!(ttl > 0);
}

#[test]
fn test_expire_at_past() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire-at", "k", "1000"]);

    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_purge_cleans_expired() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    klyv(&db, &["set", "b", "2"]);
    klyv(&db, &["set", "c", "3"]);
    klyv(&db, &["expire-at", "a", "0"]);
    klyv(&db, &["expire-at", "b", "0"]);

    let (out, _, _) = klyv(&db, &["purge"]);
    assert_eq!(out.trim(), "(integer) 2");

    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_purge_nothing_expired() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "9999"]);

    let (out, _, _) = klyv(&db, &["purge"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_del_removes_expiry() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "100"]);
    klyv(&db, &["del", "k"]);

    klyv(&db, &["set", "k", "new"]);
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_set_clears_live_ttl_like_redis() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "100"]);
    klyv(&db, &["set", "k", "new"]);
    // Redis SET discards the TTL unless KEEPTTL is given.
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_set_keep_ttl_preserves_live_ttl() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "100"]);
    let (out, _, ok) = klyv(&db, &["set", "k", "new", "--keep-ttl"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let ttl: i64 = out
        .trim()
        .strip_prefix("(integer) ")
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=100).contains(&ttl), "ttl was {ttl}");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "new");
}

#[test]
fn test_set_keep_ttl_conflicts_with_ex() {
    let db = fresh_db();
    let (_, err, ok) = klyv(&db, &["set", "k", "v", "--keep-ttl", "--ex", "10"]);
    assert!(!ok);
    assert!(err.contains("cannot be used with"), "stderr: {err}");
}

#[test]
fn test_mset_clears_live_ttl() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "100"]);
    klyv(&db, &["m-set", "k", "new", "j", "1"]);
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_rename_preserves_expiry() {
    let db = fresh_db();
    klyv(&db, &["set", "old", "v"]);
    klyv(&db, &["expire", "old", "100"]);
    klyv(&db, &["rename", "old", "new"]);

    let (out, _, _) = klyv(&db, &["ttl", "new"]);
    let ttl: i64 = out
        .trim()
        .strip_prefix("(integer) ")
        .unwrap()
        .parse()
        .unwrap();
    assert!(ttl > 0 && ttl <= 100);

    let (out, _, _) = klyv(&db, &["ttl", "old"]);
    assert_eq!(out.trim(), "(integer) -2");
}

#[test]
fn test_incr_expired_key_resets() {
    let db = fresh_db();
    klyv(&db, &["set", "n", "50"]);
    klyv(&db, &["expire-at", "n", "0"]);

    let (out, _, _) = klyv(&db, &["incr", "n"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_strlen_expired() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "hello"]);
    klyv(&db, &["expire-at", "k", "0"]);

    let (out, _, _) = klyv(&db, &["strlen", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_mget_mixed_expired() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    klyv(&db, &["set", "b", "2"]);
    klyv(&db, &["expire-at", "a", "0"]);

    let (out, _, _) = klyv(&db, &["m-get", "a", "b"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "(nil)");
    assert_eq!(lines[1], "2");
}

#[test]
fn test_dbsize_excludes_expired() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    klyv(&db, &["set", "b", "2"]);
    klyv(&db, &["expire-at", "a", "0"]);

    // dbsize counts raw rows, purge removes them
    let (out, _, _) = klyv(&db, &["purge"]);
    assert_eq!(out.trim(), "(integer) 1");

    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 1");
}

// === REGRESSION TESTS: type-exclusivity, expiry-on-write, atomicity, error handling ===

#[test]
fn test_wrongtype_lpush_on_string() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (_, err, ok) = klyv(&db, &["l-push", "k", "x"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"), "stderr: {err}");
}

#[test]
fn test_wrongtype_sadd_on_string() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (_, err, ok) = klyv(&db, &["s-add", "k", "m"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"), "stderr: {err}");
}

#[test]
fn test_wrongtype_hset_on_list() {
    let db = fresh_db();
    klyv(&db, &["r-push", "k", "a"]);
    let (_, err, ok) = klyv(&db, &["h-set", "k", "f", "v"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"), "stderr: {err}");
}

#[test]
fn test_wrongtype_incr_on_list() {
    let db = fresh_db();
    klyv(&db, &["r-push", "k", "a"]);
    let (_, err, ok) = klyv(&db, &["incr", "k"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"), "stderr: {err}");
}

#[test]
fn test_set_overwrites_other_type() {
    let db = fresh_db();
    klyv(&db, &["r-push", "k", "a", "b"]);
    klyv(&db, &["set", "k", "v"]);
    let (out, _, _) = klyv(&db, &["type", "k"]);
    assert_eq!(out.trim(), "string");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "v");
    // The list rows are gone; the key is now a string, so l-len is WRONGTYPE.
    let (_, err, ok) = klyv(&db, &["l-len", "k"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"));
}

#[test]
fn test_rename_self_noop() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, ok) = klyv(&db, &["rename", "k", "k"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "v");
}

#[test]
fn test_rename_overwrites_target_cross_type() {
    let db = fresh_db();
    klyv(&db, &["r-push", "dst", "a", "b"]);
    klyv(&db, &["set", "src", "v"]);
    let (out, _, ok) = klyv(&db, &["rename", "src", "dst"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["type", "dst"]);
    assert_eq!(out.trim(), "string");
    let (out, _, _) = klyv(&db, &["get", "dst"]);
    assert_eq!(out.trim(), "v");
    // The list rows are gone; the key is now a string, so l-len is WRONGTYPE.
    let (_, err, ok) = klyv(&db, &["l-len", "dst"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"));
}

#[test]
fn test_incr_after_expiry_resets_and_clears() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "41"]);
    klyv(&db, &["expire-at", "k", "1"]);
    let (out, _, ok) = klyv(&db, &["incr", "k"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "1");
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_append_after_expiry_starts_fresh() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "hello"]);
    klyv(&db, &["expire-at", "k", "1"]);
    klyv(&db, &["append", "k", "x"]);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "x");
}

#[test]
fn test_lpush_after_expiry_drops_old_items() {
    let db = fresh_db();
    klyv(&db, &["r-push", "L", "a", "b", "c"]);
    klyv(&db, &["expire-at", "L", "1"]);
    klyv(&db, &["l-push", "L", "z"]);
    let (out, _, _) = klyv(&db, &["l-len", "L"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_set_over_expired_clears_stale_expiry() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "old"]);
    klyv(&db, &["expire-at", "k", "1"]);
    klyv(&db, &["set", "k", "new"]);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "new");
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_set_over_live_ttl_then_persist_returns_zero() {
    // After SET cleared the TTL there is nothing left for PERSIST to remove.
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "1000"]);
    klyv(&db, &["set", "k", "v2"]);
    let (out, _, _) = klyv(&db, &["persist", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_mset_overwrites_other_type() {
    let db = fresh_db();
    klyv(&db, &["r-push", "k", "a", "b"]);
    klyv(&db, &["m-set", "k", "v"]);
    let (out, _, _) = klyv(&db, &["type", "k"]);
    assert_eq!(out.trim(), "string");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "v");
    // The list rows are gone; the key is now a string, so l-len is WRONGTYPE.
    let (_, err, ok) = klyv(&db, &["l-len", "k"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"));
}

#[test]
fn test_sinter_duplicate_keys() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s", "a", "b"]);
    let (out, _, ok) = klyv(&db, &["s-inter", "s", "s"]);
    assert!(ok);
    assert!(out.contains("\"a\""), "out: {out}");
    assert!(out.contains("\"b\""), "out: {out}");
}

#[test]
fn test_sinter_with_expired_input_is_empty() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a"]);
    klyv(&db, &["s-add", "s2", "a"]);
    klyv(&db, &["expire-at", "s2", "1"]);
    let (out, _, _) = klyv(&db, &["s-inter", "s1", "s2"]);
    assert_eq!(out.trim(), "(empty set)");
}

#[test]
fn test_sunion_skips_expired_input() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a"]);
    klyv(&db, &["s-add", "s2", "b"]);
    klyv(&db, &["expire-at", "s2", "1"]);
    let (out, _, _) = klyv(&db, &["s-union", "s1", "s2"]);
    assert!(out.contains("\"a\""), "out: {out}");
    assert!(!out.contains("\"b\""), "out: {out}");
}

#[test]
fn test_sdiff_with_expired_other_subtracts_nothing() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s1", "a", "b"]);
    klyv(&db, &["s-add", "s2", "a"]);
    klyv(&db, &["expire-at", "s2", "1"]);
    let (out, _, _) = klyv(&db, &["s-diff", "s1", "s2"]);
    assert!(out.contains("\"a\""), "out: {out}");
    assert!(out.contains("\"b\""), "out: {out}");
}

#[test]
fn test_persist_does_not_resurrect_expired() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire-at", "k", "1"]);
    let (out, _, _) = klyv(&db, &["persist", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_persist_nonexistent() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["persist", "nope"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_expire_zero_expires_immediately() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "0"]);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -2");
}

#[test]
fn test_pexpire_negative_expires_immediately() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["p-expire", "k", "-100"]);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_expire_negative_expires_immediately() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (_, _, ok) = klyv(&db, &["expire", "k", "-1"]);
    assert!(ok);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_expireat_past_expires_immediately() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (_, _, ok) = klyv(&db, &["expire-at", "k", "-1"]);
    assert!(ok);
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_incr_overflow_errors() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "9223372036854775807"]);
    let (_, err, ok) = klyv(&db, &["incr", "k"]);
    assert!(!ok);
    assert!(err.contains("overflow"), "stderr: {err}");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "9223372036854775807");
}

#[test]
fn test_decrby_i64_min_errors() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "0"]);
    let (_, err, ok) = klyv(&db, &["decr-by", "k", "-9223372036854775808"]);
    assert!(!ok);
    assert!(err.contains("overflow"), "stderr: {err}");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "0");
}

#[test]
fn test_keys_underscore_is_literal() {
    let db = fresh_db();
    klyv(&db, &["set", "a_b", "1"]);
    klyv(&db, &["set", "axb", "1"]);
    let (out, _, _) = klyv(&db, &["keys", "a_b"]);
    assert!(out.contains("\"a_b\""), "out: {out}");
    assert!(!out.contains("\"axb\""), "out: {out}");
}

#[test]
fn test_keys_percent_is_literal() {
    let db = fresh_db();
    klyv(&db, &["set", "a%b", "1"]);
    klyv(&db, &["set", "axb", "1"]);
    let (out, _, _) = klyv(&db, &["keys", "a%b"]);
    assert!(out.contains("\"a%b\""), "out: {out}");
    assert!(!out.contains("\"axb\""), "out: {out}");
}

#[test]
fn test_keys_backslash_is_the_escape_character() {
    // Redis glob: '\\x' matches a literal x, so a literal backslash in the
    // key needs a doubled backslash in the pattern (verified against Redis).
    let db = fresh_db();
    klyv(&db, &["set", "a\\b", "1"]);
    klyv(&db, &["set", "ab", "2"]);
    klyv(&db, &["set", "axb", "3"]);
    let (out, _, _) = klyv(&db, &["keys", "a\\b"]);
    assert_eq!(out.trim(), "1) \"ab\"");
    let (out, _, _) = klyv(&db, &["keys", "a\\\\b"]);
    assert_eq!(out.trim(), "1) \"a\\b\"");
}

#[test]
fn test_lrem_ignores_expired() {
    let db = fresh_db();
    klyv(&db, &["r-push", "L", "x", "x", "y"]);
    klyv(&db, &["expire-at", "L", "1"]);
    let (out, _, _) = klyv(&db, &["l-rem", "L", "0", "x"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_srem_ignores_expired() {
    let db = fresh_db();
    klyv(&db, &["s-add", "S", "a", "b"]);
    klyv(&db, &["expire-at", "S", "1"]);
    let (out, _, _) = klyv(&db, &["s-rem", "S", "a"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_hdel_ignores_expired() {
    let db = fresh_db();
    klyv(&db, &["h-set", "H", "f", "v"]);
    klyv(&db, &["expire-at", "H", "1"]);
    let (out, _, _) = klyv(&db, &["h-del", "H", "f"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_incr_non_integer_no_panic() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "abc"]);
    let (_, err, ok) = klyv(&db, &["incr", "k"]);
    assert!(!ok);
    assert!(err.contains("ERR value is not an integer"), "stderr: {err}");
    assert!(!err.contains("panicked"), "stderr: {err}");
}

// --- Fix 2: LREM enforces WRONGTYPE on a non-list key ---

#[test]
fn test_wrongtype_lrem_on_string() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (_, err, ok) = klyv(&db, &["l-rem", "k", "0", "v"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"), "stderr: {err}");
    // the string value must be left intact
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "v");
}

#[test]
fn test_wrongtype_lrem_on_set() {
    let db = fresh_db();
    klyv(&db, &["s-add", "k", "a"]);
    let (_, err, ok) = klyv(&db, &["l-rem", "k", "0", "a"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"), "stderr: {err}");
}

#[test]
fn test_lrem_on_actual_list_still_works() {
    let db = fresh_db();
    klyv(&db, &["r-push", "L", "x", "x", "y"]);
    let (out, _, ok) = klyv(&db, &["l-rem", "L", "0", "x"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 2");
    let (out, _, _) = klyv(&db, &["l-len", "L"]);
    assert_eq!(out.trim(), "(integer) 1");
}

// --- Fix 3b: LPOP/RPOP treat an expired key as absent and drop its stale rows ---

#[test]
fn test_lpop_ignores_expired() {
    let db = fresh_db();
    klyv(&db, &["r-push", "L", "a", "b", "c"]);
    klyv(&db, &["expire-at", "L", "1"]);
    let (out, _, ok) = klyv(&db, &["l-pop", "L"]);
    assert!(ok);
    assert_eq!(out.trim(), "(nil)");
    // stale rows are physically dropped, so the key is gone entirely
    let (out, _, _) = klyv(&db, &["type", "L"]);
    assert_eq!(out.trim(), "none");
}

#[test]
fn test_rpop_ignores_expired() {
    let db = fresh_db();
    klyv(&db, &["r-push", "L", "a", "b", "c"]);
    klyv(&db, &["expire-at", "L", "1"]);
    let (out, _, ok) = klyv(&db, &["r-pop", "L"]);
    assert!(ok);
    assert_eq!(out.trim(), "(nil)");
    let (out, _, _) = klyv(&db, &["type", "L"]);
    assert_eq!(out.trim(), "none");
}

#[test]
fn test_lpop_reuse_after_expiry() {
    let db = fresh_db();
    klyv(&db, &["r-push", "L", "old"]);
    klyv(&db, &["expire-at", "L", "1"]);
    // popping the expired list clears it (stale rows + stale expiry)
    klyv(&db, &["l-pop", "L"]);
    // a fresh push reuses the key with no leftover TTL
    klyv(&db, &["r-push", "L", "new1", "new2"]);
    let (out, _, _) = klyv(&db, &["ttl", "L"]);
    assert_eq!(out.trim(), "(integer) -1");
    let (out, _, _) = klyv(&db, &["l-pop", "L"]);
    assert_eq!(out.trim(), "new1");
}

// --- Fix 1: TTL mutators still behave correctly when wrapped in a transaction ---

#[test]
fn test_expire_on_missing_key_returns_zero() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["expire", "nope", "100"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_expire_on_expired_key_returns_zero() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire-at", "k", "1"]);
    let (out, _, ok) = klyv(&db, &["expire", "k", "100"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_expire_then_ttl_roundtrip() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, ok) = klyv(&db, &["expire", "k", "1000"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let n: i64 = out.trim().trim_start_matches("(integer) ").parse().unwrap();
    assert!(n > 0 && n <= 1000, "ttl was {n}");
}

#[test]
fn test_persist_removes_live_ttl() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "1000"]);
    let (out, _, ok) = klyv(&db, &["persist", "k"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_expireat_on_missing_key_returns_zero() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["expire-at", "nope", "9999999999"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 0");
}

// === OUTPUT FORMATS ===

fn klyv_fmt(db: &str, format: &str, args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_klyv"))
        .arg("--db")
        .arg(db)
        .arg("--format")
        .arg(format)
        .args(args)
        .output()
        .expect("failed to run klyv");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

#[test]
fn test_raw_get_and_nil() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "hello"]);
    let (out, _, ok) = klyv_fmt(&db, "raw", &["get", "k"]);
    assert!(ok);
    assert_eq!(out, "hello\n");

    // Nil is a bare empty line in raw mode.
    let (out, _, ok) = klyv_fmt(&db, "raw", &["get", "missing"]);
    assert!(ok);
    assert_eq!(out, "\n");
}

#[test]
fn test_raw_int_and_array() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b"]);
    let (out, _, _) = klyv_fmt(&db, "raw", &["l-len", "l"]);
    assert_eq!(out, "2\n");

    let (out, _, _) = klyv_fmt(&db, "raw", &["l-range", "l", "0", "-1"]);
    assert_eq!(out, "a\nb\n");

    // Empty array prints nothing in raw mode.
    let (out, _, _) = klyv_fmt(&db, "raw", &["l-range", "nolist", "0", "-1"]);
    assert_eq!(out, "");
}

#[test]
fn test_raw_mget_lines() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    let (out, _, _) = klyv_fmt(&db, "raw", &["m-get", "a", "missing"]);
    assert_eq!(out, "1\n\n");
}

#[test]
fn test_json_scalar_nil_and_array() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "hello"]);
    let (out, _, ok) = klyv_fmt(&db, "json", &["get", "k"]);
    assert!(ok);
    assert_eq!(out, "\"hello\"\n");

    let (out, _, _) = klyv_fmt(&db, "json", &["get", "missing"]);
    assert_eq!(out, "null\n");

    let (out, _, _) = klyv_fmt(&db, "json", &["incr", "n"]);
    assert_eq!(out, "1\n");

    klyv(&db, &["r-push", "l", "a", "b"]);
    let (out, _, _) = klyv_fmt(&db, "json", &["l-range", "l", "0", "-1"]);
    assert_eq!(out, "[\"a\",\"b\"]\n");

    let (out, _, _) = klyv_fmt(&db, "json", &["set", "k2", "v"]);
    assert_eq!(out, "\"OK\"\n");
}

#[test]
fn test_json_hgetall_object_and_mget() {
    let db = fresh_db();
    klyv(&db, &["h-set", "h", "name", "Alice", "role", "admin"]);
    let (out, _, _) = klyv_fmt(&db, "json", &["h-get-all", "h"]);
    assert_eq!(out, "{\"name\":\"Alice\",\"role\":\"admin\"}\n");

    let (out, _, _) = klyv_fmt(&db, "json", &["h-get-all", "missing"]);
    assert_eq!(out, "{}\n");

    klyv(&db, &["set", "a", "1"]);
    let (out, _, _) = klyv_fmt(&db, "json", &["m-get", "a", "missing"]);
    assert_eq!(out, "[\"1\",null]\n");
}

#[test]
fn test_json_escapes_special_characters() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "line1\nline2\t\"quoted\" back\\slash"]);
    let (out, _, _) = klyv_fmt(&db, "json", &["get", "k"]);
    assert_eq!(out, "\"line1\\nline2\\t\\\"quoted\\\" back\\\\slash\"\n");
}

// === SET OPTIONS (--nx, --ex, --px) ===

#[test]
fn test_set_nx_only_sets_missing_key() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["set", "k", "first", "--nx"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");

    let (out, _, ok) = klyv(&db, &["set", "k", "second", "--nx"]);
    assert!(ok);
    assert_eq!(out.trim(), "(nil)");

    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "first");
}

#[test]
fn test_set_nx_succeeds_on_expired_key() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "old"]);
    klyv(&db, &["expire-at", "k", "0"]);
    let (out, _, _) = klyv(&db, &["set", "k", "new", "--nx"]);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "new");
}

#[test]
fn test_set_ex_sets_ttl_atomically() {
    let db = fresh_db();
    let (out, _, _) = klyv(&db, &["set", "k", "v", "--ex", "100"]);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let ttl: i64 = out.trim().trim_start_matches("(integer) ").parse().unwrap();
    assert!((40..=100).contains(&ttl), "ttl was {ttl}");
}

#[test]
fn test_set_px_sets_ttl() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v", "--px", "90500"]);
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let ttl: i64 = out.trim().trim_start_matches("(integer) ").parse().unwrap();
    assert!((30..=91).contains(&ttl), "ttl was {ttl}");
}

#[test]
fn test_set_invalid_expire_errors() {
    let db = fresh_db();
    let (_, err, ok) = klyv(&db, &["set", "k", "v", "--ex", "0"]);
    assert!(!ok);
    assert!(err.contains("ERR invalid expire time"));
    // Nothing was written.
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");
}

// === GETDEL ===

#[test]
fn test_getdel_returns_and_deletes() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "100"]);
    let (out, _, ok) = klyv(&db, &["get-del", "k"]);
    assert!(ok);
    assert_eq!(out.trim(), "v");
    let (out, _, _) = klyv(&db, &["exists", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
    // The expiry row went with it: a fresh SET has no TTL.
    klyv(&db, &["set", "k", "new"]);
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_getdel_missing_and_wrongtype() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["get-del", "nope"]);
    assert!(ok);
    assert_eq!(out.trim(), "(nil)");

    klyv(&db, &["r-push", "l", "a"]);
    let (_, err, ok) = klyv(&db, &["get-del", "l"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"));
}

// === LINDEX / LSET / LTRIM / LINSERT ===

#[test]
fn test_lindex() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b", "c"]);
    let (out, _, _) = klyv(&db, &["l-index", "l", "0"]);
    assert_eq!(out.trim(), "a");
    let (out, _, _) = klyv(&db, &["l-index", "l", "-1"]);
    assert_eq!(out.trim(), "c");
    let (out, _, _) = klyv(&db, &["l-index", "l", "5"]);
    assert_eq!(out.trim(), "(nil)");
    let (out, _, _) = klyv(&db, &["l-index", "nolist", "0"]);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_lset() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b", "c"]);
    let (out, _, ok) = klyv(&db, &["l-set", "l", "1", "B"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["l-set", "l", "-1", "C"]);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["l-range", "l", "0", "-1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, ["1) \"a\"", "2) \"B\"", "3) \"C\""]);

    let (_, err, ok) = klyv(&db, &["l-set", "l", "9", "x"]);
    assert!(!ok);
    assert!(err.contains("ERR index out of range"));
    let (_, err, ok) = klyv(&db, &["l-set", "missing", "0", "x"]);
    assert!(!ok);
    assert!(err.contains("ERR no such key"));
}

#[test]
fn test_ltrim_keeps_range() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b", "c", "d", "e"]);
    let (out, _, ok) = klyv(&db, &["l-trim", "l", "1", "-2"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["l-range", "l", "0", "-1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, ["1) \"b\"", "2) \"c\"", "3) \"d\""]);
}

#[test]
fn test_ltrim_empty_range_removes_key_and_ttl() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b"]);
    klyv(&db, &["expire", "l", "100"]);
    klyv(&db, &["l-trim", "l", "5", "1"]);
    let (out, _, _) = klyv(&db, &["exists", "l"]);
    assert_eq!(out.trim(), "(integer) 0");
    // No stale TTL survives for a future key.
    klyv(&db, &["set", "l", "v"]);
    let (out, _, _) = klyv(&db, &["ttl", "l"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_linsert_before_and_after() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "c"]);
    let (out, _, ok) = klyv(&db, &["l-insert", "l", "before", "c", "b"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 3");
    let (out, _, _) = klyv(&db, &["l-insert", "l", "after", "c", "d"]);
    assert_eq!(out.trim(), "(integer) 4");
    let (out, _, _) = klyv(&db, &["l-range", "l", "0", "-1"]);
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, ["1) \"a\"", "2) \"b\"", "3) \"c\"", "4) \"d\""]);

    let (out, _, _) = klyv(&db, &["l-insert", "l", "before", "zz", "x"]);
    assert_eq!(out.trim(), "(integer) -1");
    let (out, _, _) = klyv(&db, &["l-insert", "missing", "before", "a", "x"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_linsert_survives_midpoint_precision_exhaustion() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b"]);
    // Repeatedly inserting after the same pivot halves the gap each time;
    // ~55 iterations exhaust f64 precision and force a renumber.
    for i in 0..60 {
        let v = format!("x{i}");
        let (out, _, ok) = klyv(&db, &["l-insert", "l", "after", "a", &v]);
        assert!(ok, "insert {i} failed");
        assert_eq!(out.trim(), format!("(integer) {}", i + 3));
    }
    let (out, _, _) = klyv(&db, &["l-len", "l"]);
    assert_eq!(out.trim(), "(integer) 62");
    let (out, _, _) = klyv(&db, &["l-index", "l", "0"]);
    assert_eq!(out.trim(), "a");
    let (out, _, _) = klyv(&db, &["l-index", "l", "1"]);
    assert_eq!(out.trim(), "x59");
    let (out, _, _) = klyv(&db, &["l-index", "l", "2"]);
    assert_eq!(out.trim(), "x58");
    let (out, _, _) = klyv(&db, &["l-index", "l", "-1"]);
    assert_eq!(out.trim(), "b");
}

// === SPOP ===

#[test]
fn test_spop() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s", "only"]);
    let (out, _, ok) = klyv(&db, &["s-pop", "s"]);
    assert!(ok);
    assert_eq!(out.trim(), "only");
    let (out, _, _) = klyv(&db, &["s-card", "s"]);
    assert_eq!(out.trim(), "(integer) 0");
    let (out, _, _) = klyv(&db, &["s-pop", "s"]);
    assert_eq!(out.trim(), "(nil)");

    klyv(&db, &["set", "str", "v"]);
    let (_, err, ok) = klyv(&db, &["s-pop", "str"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"));
}

// === HEXISTS / HINCRBY ===

#[test]
fn test_hexists() {
    let db = fresh_db();
    klyv(&db, &["h-set", "h", "f", "v"]);
    let (out, _, _) = klyv(&db, &["h-exists", "h", "f"]);
    assert_eq!(out.trim(), "(integer) 1");
    let (out, _, _) = klyv(&db, &["h-exists", "h", "g"]);
    assert_eq!(out.trim(), "(integer) 0");
    let (out, _, _) = klyv(&db, &["h-exists", "missing", "f"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_hincrby() {
    let db = fresh_db();
    let (out, _, ok) = klyv(&db, &["h-incr-by", "h", "n", "5"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 5");
    let (out, _, _) = klyv(&db, &["h-incr-by", "h", "n", "-2"]);
    assert_eq!(out.trim(), "(integer) 3");

    klyv(&db, &["h-set", "h", "s", "notanumber"]);
    let (_, err, ok) = klyv(&db, &["h-incr-by", "h", "s", "1"]);
    assert!(!ok);
    assert!(err.contains("ERR hash value is not an integer"));
}

// === EXPIRY CLEANUP ON KEY-EMPTYING WRITES ===

#[test]
fn test_pop_to_empty_drops_expiry() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "only"]);
    klyv(&db, &["expire", "l", "100"]);
    klyv(&db, &["l-pop", "l"]);
    // A fresh SET must not inherit the old list's TTL.
    klyv(&db, &["set", "l", "v"]);
    let (out, _, _) = klyv(&db, &["ttl", "l"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_srem_to_empty_drops_expiry() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s", "only"]);
    klyv(&db, &["expire", "s", "100"]);
    klyv(&db, &["s-rem", "s", "only"]);
    klyv(&db, &["set", "s", "v"]);
    let (out, _, _) = klyv(&db, &["ttl", "s"]);
    assert_eq!(out.trim(), "(integer) -1");
}

#[test]
fn test_hdel_to_empty_drops_expiry() {
    let db = fresh_db();
    klyv(&db, &["h-set", "h", "f", "v"]);
    klyv(&db, &["expire", "h", "100"]);
    klyv(&db, &["h-del", "h", "f"]);
    klyv(&db, &["set", "h", "v"]);
    let (out, _, _) = klyv(&db, &["ttl", "h"]);
    assert_eq!(out.trim(), "(integer) -1");
}

// === WRONGTYPE ON TYPE-SPECIFIC READS (codex review follow-up) ===

#[test]
fn test_wrongtype_reads_on_string_key() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    for args in [
        vec!["l-pop", "k"],
        vec!["r-pop", "k"],
        vec!["l-len", "k"],
        vec!["l-range", "k", "0", "-1"],
        vec!["l-pos", "k", "v"],
        vec!["l-index", "k", "0"],
        vec!["s-members", "k"],
        vec!["s-is-member", "k", "v"],
        vec!["s-card", "k"],
        vec!["h-get", "k", "f"],
        vec!["h-exists", "k", "f"],
        vec!["h-get-all", "k"],
        vec!["h-keys", "k"],
        vec!["h-vals", "k"],
        vec!["h-len", "k"],
    ] {
        let (_, err, ok) = klyv(&db, &args);
        assert!(!ok, "{args:?} should fail");
        assert!(err.contains("WRONGTYPE"), "{args:?} stderr: {err}");
    }
}

#[test]
fn test_wrongtype_get_and_strlen_on_list() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a"]);
    let (_, err, ok) = klyv(&db, &["get", "l"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"));
    let (_, err, ok) = klyv(&db, &["strlen", "l"]);
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"));
}

#[test]
fn test_wrongtype_set_algebra_input() {
    let db = fresh_db();
    klyv(&db, &["s-add", "s", "a"]);
    klyv(&db, &["set", "str", "v"]);
    for cmd in ["s-union", "s-inter", "s-diff"] {
        let (_, err, ok) = klyv(&db, &[cmd, "s", "str"]);
        assert!(!ok, "{cmd} should fail");
        assert!(err.contains("WRONGTYPE"), "{cmd} stderr: {err}");
    }
}

#[test]
fn test_mget_stays_lenient_on_wrong_type() {
    // Redis MGET returns nil for non-string keys instead of erroring.
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a"]);
    klyv(&db, &["set", "k", "v"]);
    let (out, _, ok) = klyv(&db, &["m-get", "k", "l"]);
    assert!(ok);
    assert_eq!(out, "v\n(nil)\n");
}

#[test]
fn test_expired_key_reads_as_missing_not_wrongtype() {
    // An expired key must never trip the type check for its old type.
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a"]);
    klyv(&db, &["expire-at", "l", "0"]);
    let (out, _, ok) = klyv(&db, &["get", "l"]);
    assert!(ok);
    assert_eq!(out.trim(), "(nil)");
    let (out, _, ok) = klyv(&db, &["h-len", "l"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 0");
}

// === REDIS-STYLE STOP NORMALIZATION (no clamp-to-0 for negative stop) ===

#[test]
fn test_lrange_stop_beyond_head_is_empty() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b", "c"]);
    // stop = 3 + (-5) = -2 < start: Redis returns empty, not element 0.
    let (out, _, ok) = klyv(&db, &["l-range", "l", "0", "-5"]);
    assert!(ok);
    assert_eq!(out.trim(), "(empty list)");
}

#[test]
fn test_ltrim_stop_beyond_head_empties_list() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b", "c"]);
    let (out, _, ok) = klyv(&db, &["l-trim", "l", "0", "-5"]);
    assert!(ok);
    assert_eq!(out.trim(), "OK");
    let (out, _, _) = klyv(&db, &["exists", "l"]);
    assert_eq!(out.trim(), "(integer) 0");
}

// === PIPE MODE (no subcommand, stdin not a tty) ===

fn klyv_pipe(db: &str, extra_args: &[&str], input: &str) -> (String, String, bool) {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_klyv"))
        .arg("--db")
        .arg(db)
        .args(extra_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn klyv");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("failed to wait for klyv");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.success(),
    )
}

#[test]
fn test_pipe_basic_sequence() {
    let db = fresh_db();
    let (out, err, ok) = klyv_pipe(&db, &[], "set k v\nget k\nincr n\nincr n\n");
    assert!(ok, "stderr: {err}");
    assert_eq!(out, "OK\nv\n(integer) 1\n(integer) 2\n");
}

#[test]
fn test_pipe_state_persists_after_session() {
    let db = fresh_db();
    klyv_pipe(&db, &[], "set k piped\n");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "piped");
}

#[test]
fn test_pipe_quoted_values() {
    let db = fresh_db();
    let (out, _, ok) = klyv_pipe(&db, &[], "set k \"hello world\"\nget k\n");
    assert!(ok);
    assert_eq!(out, "OK\nhello world\n");
}

#[test]
fn test_pipe_errors_are_recoverable() {
    let db = fresh_db();
    let (out, err, ok) = klyv_pipe(&db, &[], "set s notanum\nincr s\nset j 1\nget j\n");
    // The failing line reports to stderr, the session continues, and the
    // overall exit code is 1.
    assert!(!ok);
    assert!(err.contains("ERR value is not an integer"));
    assert_eq!(out, "OK\nOK\n1\n");
}

#[test]
fn test_pipe_unknown_command_recoverable() {
    let db = fresh_db();
    let (out, err, ok) = klyv_pipe(&db, &[], "nosuchcmd\nset k v\nget k\n");
    assert!(!ok);
    assert!(err.contains("unrecognized subcommand"));
    assert_eq!(out, "OK\nv\n");
}

#[test]
fn test_pipe_unbalanced_quotes_recoverable() {
    let db = fresh_db();
    let (out, err, ok) = klyv_pipe(&db, &[], "set k \"unterminated\nset k2 fine\nget k2\n");
    assert!(!ok);
    assert!(err.contains("ERR unbalanced quotes"));
    assert_eq!(out, "OK\nfine\n");
}

#[test]
fn test_pipe_quit_stops_processing() {
    let db = fresh_db();
    let (out, _, ok) = klyv_pipe(&db, &[], "set a 1\nquit\nset b 2\n");
    assert!(ok);
    assert_eq!(out, "OK\n");
    let (out, _, _) = klyv(&db, &["exists", "b"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_pipe_empty_and_blank_lines_ignored() {
    let db = fresh_db();
    let (out, _, ok) = klyv_pipe(&db, &[], "\n   \nset k v\n\nget k\n");
    assert!(ok);
    assert_eq!(out, "OK\nv\n");
}

#[test]
fn test_pipe_respects_format_flag() {
    let db = fresh_db();
    let (out, _, ok) = klyv_pipe(
        &db,
        &["--format", "json"],
        "get missing\nh-set h f v\nh-get-all h\n",
    );
    assert!(ok);
    assert_eq!(out, "null\n1\n{\"f\":\"v\"}\n");
}

#[test]
fn test_pipe_wrongtype_recoverable() {
    let db = fresh_db();
    let (out, err, ok) = klyv_pipe(&db, &[], "r-push l a\nget l\nl-len l\n");
    assert!(!ok);
    assert!(err.contains("WRONGTYPE"));
    assert_eq!(out, "(integer) 1\n(integer) 1\n");
}

#[test]
fn test_pipe_hash_char_is_data_not_comment() {
    // Regression: the shlex crate treated word-initial '#' as a shell
    // comment, silently truncating stored data.
    let db = fresh_db();
    let (out, _, ok) = klyv_pipe(
        &db,
        &[],
        "set b #value\nget b\nset t \"value #tagged\"\nget t\n",
    );
    assert!(ok);
    assert_eq!(out, "OK\n#value\nOK\nvalue #tagged\n");
}

#[test]
fn test_pipe_single_quotes_and_backslash_escapes() {
    let db = fresh_db();
    let (out, _, ok) = klyv_pipe(
        &db,
        &[],
        "set q 'sq uoted'\nget q\nset bs hello\\ world\nget bs\nset dq \"a \\\"quote\\\"\"\nget dq\n",
    );
    assert!(ok);
    assert_eq!(out, "OK\nsq uoted\nOK\nhello world\nOK\na \"quote\"\n");
}

#[test]
fn test_pipe_trailing_backslash_recoverable() {
    let db = fresh_db();
    // Line 1 has a trailing backslash, so the set fails and k stays unset.
    let (out, err, ok) = klyv_pipe(&db, &[], "set k v\\\nget k\n");
    assert!(!ok);
    assert!(err.contains("ERR unbalanced quotes"));
    assert_eq!(out, "(nil)\n");
}

// === KEYS: Redis glob semantics via SQLite GLOB ===

#[test]
fn test_keys_is_case_sensitive() {
    let db = fresh_db();
    klyv(&db, &["set", "user:1", "a"]);
    klyv(&db, &["set", "User:2", "b"]);
    klyv(&db, &["set", "USER:3", "c"]);
    let (out, _, _) = klyv(&db, &["keys", "user:*"]);
    assert_eq!(out.trim(), "1) \"user:1\"");
}

#[test]
fn test_keys_character_classes_negation_and_ranges() {
    let db = fresh_db();
    for k in ["k1", "k2", "k3", "ka", "kb"] {
        klyv(&db, &["set", k, "v"]);
    }
    let (out, _, _) = klyv(&db, &["keys", "k[12]"]);
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["1) \"k1\"", "2) \"k2\""]
    );
    let (out, _, _) = klyv(&db, &["keys", "k[^123]"]);
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["1) \"ka\"", "2) \"kb\""]
    );
    let (out, _, _) = klyv(&db, &["keys", "k[a-b]"]);
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["1) \"ka\"", "2) \"kb\""]
    );
}

#[test]
fn test_keys_backslash_escapes_specials() {
    let db = fresh_db();
    klyv(&db, &["set", "a*b", "1"]);
    klyv(&db, &["set", "axb", "2"]);
    klyv(&db, &["set", "a[b", "3"]);
    klyv(&db, &["set", "a?b", "4"]);
    let (out, _, _) = klyv(&db, &["keys", "a\\*b"]);
    assert_eq!(out.trim(), "1) \"a*b\"");
    let (out, _, _) = klyv(&db, &["keys", "a\\[b"]);
    assert_eq!(out.trim(), "1) \"a[b\"");
    let (out, _, _) = klyv(&db, &["keys", "a\\?b"]);
    assert_eq!(out.trim(), "1) \"a?b\"");
    // Unescaped specials still act as wildcards.
    let (out, _, _) = klyv(&db, &["keys", "a?b"]);
    assert_eq!(out.trim().lines().count(), 4);
}

#[test]
fn test_keys_sql_like_metacharacters_are_literal() {
    // '%' and '_' meant something under the old LIKE translation; under
    // GLOB they are ordinary characters and must match themselves only.
    let db = fresh_db();
    klyv(&db, &["set", "100%", "v"]);
    klyv(&db, &["set", "100x", "v"]);
    klyv(&db, &["set", "a_b", "v"]);
    klyv(&db, &["set", "aXb", "v"]);
    let (out, _, _) = klyv(&db, &["keys", "100%"]);
    assert_eq!(out.trim(), "1) \"100%\"");
    let (out, _, _) = klyv(&db, &["keys", "a_b"]);
    assert_eq!(out.trim(), "1) \"a_b\"");
}

// === SCHEMA V2: keyspace catalogue, millisecond expiry, migration ===

fn ttl_of(db: &str, cmd: &str, key: &str) -> i64 {
    let (out, _, _) = klyv(db, &[cmd, key]);
    out.trim()
        .strip_prefix("(integer) ")
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn test_pexpire_has_millisecond_precision() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    // The windows are a minute wide so a slow machine cannot fail them. The
    // upper bound is what pins the precision: rounding 90500 ms up to whole
    // seconds, as klyv <= 0.2.0 did, would report more than 90500.
    let (out, _, ok) = klyv(&db, &["p-expire", "k", "90500"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");
    let pttl = ttl_of(&db, "p-ttl", "k");
    assert!((30_500..=90_500).contains(&pttl), "pttl was {pttl}");
    let ttl = ttl_of(&db, "ttl", "k");
    assert!((30..=91).contains(&ttl), "ttl was {ttl}");
}

#[test]
fn test_ttl_rounds_to_nearest_second() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "100"]);
    // Normally 99.9xx seconds remain and this is exactly 100 (Redis rounds
    // to nearest). The exact rounding is unit-tested in commands/ttl.rs;
    // here the window is wide so a slow machine cannot fail it.
    let ttl = ttl_of(&db, "ttl", "k");
    assert!((40..=100).contains(&ttl), "ttl was {ttl}");
}

#[test]
fn test_pttl_codes() {
    let db = fresh_db();
    assert_eq!(ttl_of(&db, "p-ttl", "missing"), -2);
    klyv(&db, &["set", "k", "v"]);
    assert_eq!(ttl_of(&db, "p-ttl", "k"), -1);
    klyv(&db, &["expire-at", "k", "0"]);
    assert_eq!(ttl_of(&db, "p-ttl", "k"), -2);
}

#[test]
fn test_pexpireat() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let (out, _, ok) = klyv(&db, &["p-expire-at", "k", "9999999999999"]);
    assert!(ok);
    assert_eq!(out.trim(), "(integer) 1");
    assert!(ttl_of(&db, "p-ttl", "k") > 0);
    let (out, _, _) = klyv(&db, &["p-expire-at", "k", "1"]);
    assert_eq!(out.trim(), "(integer) 1");
    let (out, _, _) = klyv(&db, &["get", "k"]);
    assert_eq!(out.trim(), "(nil)");
    let (out, _, _) = klyv(&db, &["p-expire-at", "missing", "9999999999999"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_set_px_is_exact() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v", "--px", "90500"]);
    let pttl = ttl_of(&db, "p-ttl", "k");
    assert!((30_500..=90_500).contains(&pttl), "pttl was {pttl}");
}

#[test]
fn test_dbsize_excludes_expired_without_purge() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    klyv(&db, &["set", "b", "2"]);
    klyv(&db, &["expire-at", "a", "0"]);
    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_migrates_v1_database_in_place() {
    let db = fresh_db();
    {
        // Fabricate a schema-v1 database exactly as klyv 0.2.0 wrote it:
        // per-type tables, a seconds-based expiry table, user_version 0.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE strings (key TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE list_items (key TEXT NOT NULL, idx REAL NOT NULL, value BLOB NOT NULL);
             CREATE TABLE set_members (key TEXT NOT NULL, member BLOB NOT NULL, UNIQUE(key, member));
             CREATE TABLE hash_fields (key TEXT NOT NULL, field TEXT NOT NULL, value BLOB NOT NULL, UNIQUE(key, field));
             CREATE TABLE expiry (key TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
             INSERT INTO strings VALUES ('s', 'hello'), ('gone', 'x');
             INSERT INTO list_items VALUES ('l', 0.0, 'a'), ('l', 1.0, 'b');
             INSERT INTO set_members VALUES ('st', 'm');
             INSERT INTO hash_fields VALUES ('h', 'f', 'v');
             INSERT INTO expiry VALUES ('s', 9999999999), ('gone', 1), ('orphan', 9999999999);",
        )
        .unwrap();
    }

    // Any command triggers the migration.
    let (out, _, ok) = klyv(&db, &["get", "s"]);
    assert!(ok);
    assert_eq!(out.trim(), "hello");
    let (out, _, _) = klyv(&db, &["l-range", "l", "0", "-1"]);
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["1) \"a\"", "2) \"b\""]
    );
    let (out, _, _) = klyv(&db, &["s-members", "st"]);
    assert_eq!(out.trim(), "1) \"m\"");
    let (out, _, _) = klyv(&db, &["h-get", "h", "f"]);
    assert_eq!(out.trim(), "v");
    // A seconds-based TTL became milliseconds and is still live.
    assert!(ttl_of(&db, "p-ttl", "s") > 1_000_000_000);
    // A key that had already expired stays expired.
    let (out, _, _) = klyv(&db, &["get", "gone"]);
    assert_eq!(out.trim(), "(nil)");
    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 4");

    let conn = rusqlite::Connection::open(&db).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    assert_len_invariant(&db);
    let has_expiry: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'expiry'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0;
    assert!(!has_expiry, "v1 expiry table should be dropped");
    assert_v1_binary_cannot_open(&db);
    // The orphan expiry row (no data) did not become a key.
    let orphan: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM keyspace WHERE key = 'orphan'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphan, 0);
}

/// Runs the exact open sequence klyv 0.2.0 executes and asserts it fails, so
/// an old binary refuses a v2 file instead of running split-brain against it.
fn assert_v1_binary_cannot_open(db: &str) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let err = conn
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS strings (key TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS expiry (key TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_expiry_at ON expiry(expires_at);",
        )
        .expect_err("a v1 binary must fail to open a v2 file");
    assert!(
        err.to_string().contains("views may not be indexed"),
        "{err}"
    );
}

#[test]
fn test_v1_binary_cannot_open_fresh_v2_file() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    assert_v1_binary_cannot_open(&db);
}

#[test]
fn test_refuses_newer_schema_version() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("PRAGMA user_version = 99")
        .unwrap();
    let (_, err, ok) = klyv(&db, &["get", "k"]);
    assert!(!ok);
    assert!(err.contains("schema version 99 is newer"), "stderr: {err}");
}

#[test]
fn test_raw_output_is_binary_safe_for_stored_text() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "caf\u{e9} \u{1F600}"]);
    let (out, _, _) = klyv_fmt(&db, "raw", &["get", "k"]);
    assert_eq!(out, "caf\u{e9} \u{1F600}\n");
}

#[test]
fn test_collection_writes_require_values() {
    let db = fresh_db();
    for args in [
        vec!["l-push", "k"],
        vec!["r-push", "k"],
        vec!["s-add", "k"],
        vec!["h-set", "k"],
        vec!["h-del", "k"],
        vec!["s-rem", "k"],
        vec!["del"],
        vec!["m-set"],
        vec!["m-get"],
        vec!["s-union"],
    ] {
        let (_, _, ok) = klyv(&db, &args);
        assert!(!ok, "{args:?} should be rejected");
    }
    // Nothing was registered in the catalogue.
    let (out, _, _) = klyv(&db, &["exists", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 0");
}

#[test]
fn test_split_brain_file_is_repaired_on_open() {
    let db = fresh_db();
    klyv(&db, &["set", "a", "1"]);
    klyv(&db, &["r-push", "l", "x"]);
    {
        // Simulate what a pre-v2 binary would do if it got past the guard:
        // recreate the expiry table and write payload around the catalogue.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP VIEW expiry;
             CREATE TABLE expiry (key TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
             INSERT INTO strings VALUES ('stray', 'v');
             INSERT INTO expiry VALUES ('a', 9999999999);
             DELETE FROM list_items WHERE key = 'l';",
        )
        .unwrap();
    }
    // Next open reconciles: stray payload catalogued, expiry folded in,
    // the emptied list's catalogue row removed, the guard restored.
    let (out, _, _) = klyv(&db, &["get", "stray"]);
    assert_eq!(out.trim(), "v");
    assert!(ttl_of(&db, "p-ttl", "a") > 1_000_000_000);
    let (out, _, _) = klyv(&db, &["exists", "l"]);
    assert_eq!(out.trim(), "(integer) 0");
    assert_v1_binary_cannot_open(&db);
}

#[test]
fn test_v1_file_without_expiry_table_is_catalogued() {
    let db = fresh_db();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE strings (key TEXT PRIMARY KEY, value BLOB NOT NULL);
             INSERT INTO strings VALUES ('s', 'hello');",
        )
        .unwrap();
    let (out, _, _) = klyv(&db, &["get", "s"]);
    assert_eq!(out.trim(), "hello");
    let (out, _, _) = klyv(&db, &["db-size"]);
    assert_eq!(out.trim(), "(integer) 1");
}

#[test]
fn test_migration_clamps_extreme_v1_expiry() {
    let db = fresh_db();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE strings (key TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE expiry (key TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
             INSERT INTO strings VALUES ('far', 'v'), ('past', 'v');
             INSERT INTO expiry VALUES ('far', 9223372036854775807), ('past', -9223372036854775807);",
        )
        .unwrap();
    // No database error: the conversion clamps instead of overflowing to REAL.
    let (out, _, ok) = klyv(&db, &["get", "far"]);
    assert!(ok);
    assert_eq!(out.trim(), "v");
    let (out, _, ok) = klyv(&db, &["get", "past"]);
    assert!(ok);
    assert_eq!(out.trim(), "(nil)");
}

#[test]
fn test_split_brain_type_change_adopts_old_binary_write() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b"]);
    {
        // An old binary did SET over a list: v1 deletes the list rows and
        // writes a strings row, leaving the catalogue saying 'list'.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP VIEW expiry;
             CREATE TABLE expiry (key TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
             DELETE FROM list_items WHERE key = 'l';
             INSERT INTO strings VALUES ('l', 'now-a-string');",
        )
        .unwrap();
    }
    // Repair must adopt the write (re-catalogue as string), not discard it.
    let (out, _, _) = klyv(&db, &["type", "l"]);
    assert_eq!(out.trim(), "string");
    let (out, _, _) = klyv(&db, &["get", "l"]);
    assert_eq!(out.trim(), "now-a-string");
}

// === 0.3.1: open fast path, expiry overflow, glob edge cases ===

#[test]
fn test_read_does_not_wait_for_a_writer() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    // Another connection holds the write lock for the whole test. Opening an
    // up-to-date database must not need it; if it did, this read would sit
    // in the 5 s busy timeout and then fail, so success is the whole check
    // (no wall-clock assertion, which a slow machine could trip).
    let holder = rusqlite::Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (out, err, ok) = klyv(&db, &["get", "k"]);
    assert!(ok, "read failed under a held write lock: {err}");
    assert_eq!(out.trim(), "v");
    holder.execute_batch("ROLLBACK").unwrap();
}

// A database file the user may not write (but in a directory where SQLite
// can still create its WAL side files) is readable. A fully read-only
// directory is not: SQLite cannot open a WAL database there at all.
#[cfg(unix)]
#[test]
fn test_read_only_database_file_is_readable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ro.db");
    let db_str = db.to_str().unwrap();
    klyv(db_str, &["set", "k", "v"]);
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    let (out, err, ok) = klyv(db_str, &["get", "k"]);
    assert!(ok, "read of a read-only database file failed: {err}");
    assert_eq!(out.trim(), "v");
    let (_, err, ok) = klyv(db_str, &["set", "k", "w"]);
    assert!(!ok);
    assert!(err.contains("readonly"), "got: {err}");
}

#[test]
fn test_unknown_catalogue_type_is_an_error_not_a_panic() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE keyspace SET type = 'zset' WHERE key = 'k'", [])
        .unwrap();
    drop(conn);
    let (_, err, ok) = klyv(&db, &["get", "k"]);
    assert!(!ok);
    assert!(err.starts_with("ERR database error"), "got: {err}");
    assert!(err.contains("unknown key type"), "got: {err}");
    assert!(!err.contains("panicked"), "got: {err}");
}

#[test]
fn test_expire_overflow_is_rejected() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    for args in [
        ["expire", "k", "9223372036854775807"],
        ["expire", "k", "-9223372036854775808"],
        ["expire", "missing", "9223372036854775807"],
        ["p-expire", "k", "9223372036854775807"],
        ["expire-at", "k", "9223372036854776"],
    ] {
        let (_, err, ok) = klyv(&db, &args);
        assert!(!ok, "{args:?} should fail");
        assert!(
            err.starts_with("ERR invalid expire time in '"),
            "{args:?} gave: {err}"
        );
    }
    assert_eq!(
        ttl_of(&db, "ttl", "k"),
        -1,
        "a rejected expiry must change nothing"
    );

    let (_, err, ok) = klyv(&db, &["set", "k", "v", "--ex", "9223372036854776"]);
    assert!(!ok);
    assert_eq!(err.trim(), "ERR invalid expire time in 'set' command");
}

#[test]
fn test_keys_malformed_patterns_follow_redis() {
    let db = fresh_db();
    klyv(&db, &["m-set", "a", "1", "b", "1", "a-c", "1", "", "1"]);
    let keys = |pattern: &str| klyv_fmt(&db, "json", &["keys", pattern]).0;
    assert_eq!(keys("[a").trim(), r#"["a"]"#);
    assert_eq!(keys("[c-a]").trim(), r#"["a","b"]"#);
    assert_eq!(keys("[]a]").trim(), "[]");
    assert_eq!(keys(r"[a\-c]*").trim(), r#"["a","a-c"]"#);
    // Only a lone `*` matches the empty key.
    assert_eq!(keys("*").trim(), r#"["","a","a-c","b"]"#);
    assert_eq!(keys("**").trim(), r#"["a","a-c","b"]"#);
}

// Redis only treats a stored value as an integer if it is in canonical form.
// APPEND can build values that a lenient parser would accept.
#[test]
fn test_incr_rejects_non_canonical_integers() {
    let db = fresh_db();
    for (start, suffix) in [("0", "42"), ("+", "5"), ("-", "0"), ("4", " ")] {
        klyv(&db, &["set", "n", start]);
        klyv(&db, &["append", "n", suffix]);
        let (_, err, ok) = klyv(&db, &["incr", "n"]);
        assert!(!ok, "incr on {start}{suffix:?} should fail");
        assert_eq!(err.trim(), "ERR value is not an integer or out of range");
        klyv(&db, &["h-set", "h", "f", &format!("{start}{suffix}")]);
        let (_, err, ok) = klyv(&db, &["h-incr-by", "h", "f", "1"]);
        assert!(!ok, "h-incr-by on {start}{suffix:?} should fail");
        assert_eq!(err.trim(), "ERR hash value is not an integer");
    }
    klyv(&db, &["set", "n", "-"]);
    klyv(&db, &["append", "n", "7"]);
    assert_eq!(klyv(&db, &["incr", "n"]).0.trim(), "(integer) -6");
}

// === SCHEMA V3: element counts in the catalogue ===

/// The schema-v3 invariant: `keyspace.len` equals the number of payload rows
/// for every list, set and hash, is NULL for every string, and no key is
/// catalogued with nothing in it.
fn assert_len_invariant(db: &str) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT key, type, len, CASE type
                 WHEN 'list' THEN (SELECT COUNT(*) FROM list_items  WHERE key = k.key)
                 WHEN 'set'  THEN (SELECT COUNT(*) FROM set_members WHERE key = k.key)
                 WHEN 'hash' THEN (SELECT COUNT(*) FROM hash_fields WHERE key = k.key)
                 WHEN 'string' THEN (SELECT COUNT(*) FROM strings   WHERE key = k.key)
             END FROM keyspace k",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (key, kind, len, actual) = row.unwrap();
        assert!(
            actual > 0,
            "{kind} key {key:?} is catalogued but has no rows"
        );
        if kind == "string" {
            assert_eq!(len, None, "string key {key:?} has a len");
        } else {
            assert_eq!(
                len,
                Some(actual),
                "{kind} key {key:?}: stored len vs actual rows"
            );
        }
    }
}

fn int_of(db: &str, args: &[&str]) -> i64 {
    let (out, err, ok) = klyv(db, args);
    assert!(ok, "{args:?} failed: {err}");
    out.trim()
        .strip_prefix("(integer) ")
        .unwrap_or_else(|| panic!("{args:?} gave {out:?}"))
        .parse()
        .unwrap()
}

#[test]
fn test_migrates_v2_database_in_place() {
    let db = fresh_db();
    {
        // A schema-v2 database as klyv 0.3.x / 0.4.0 wrote it: a catalogue
        // without `len`, the downgrade-guard view, user_version 2.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE keyspace (key TEXT PRIMARY KEY, type TEXT NOT NULL, expires_at INTEGER);
             CREATE INDEX idx_keyspace_expires ON keyspace(expires_at) WHERE expires_at IS NOT NULL;
             CREATE TABLE strings (key TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE list_items (key TEXT NOT NULL, idx REAL NOT NULL, value BLOB NOT NULL);
             CREATE INDEX idx_list_key_idx ON list_items(key, idx);
             CREATE TABLE set_members (key TEXT NOT NULL, member BLOB NOT NULL, UNIQUE(key, member));
             CREATE TABLE hash_fields (key TEXT NOT NULL, field TEXT NOT NULL, value BLOB NOT NULL, UNIQUE(key, field));
             CREATE VIEW expiry AS SELECT key, expires_at / 1000 AS expires_at FROM keyspace WHERE expires_at IS NOT NULL;
             INSERT INTO keyspace VALUES ('s', 'string', NULL), ('l', 'list', 99999999999999),
                                         ('st', 'set', NULL), ('h', 'hash', NULL), ('gone', 'list', 1);
             INSERT INTO strings VALUES ('s', 'hello');
             INSERT INTO list_items VALUES ('l', 0.0, 'a'), ('l', 1.0, 'b'), ('l', 2.0, 'c'), ('gone', 0.0, 'x');
             INSERT INTO set_members VALUES ('st', 'm'), ('st', 'n');
             INSERT INTO hash_fields VALUES ('h', 'f', 'v');
             PRAGMA user_version = 2;",
        )
        .unwrap();
    }

    assert_eq!(int_of(&db, &["l-len", "l"]), 3);
    assert_eq!(int_of(&db, &["s-card", "st"]), 2);
    assert_eq!(int_of(&db, &["h-len", "h"]), 1);
    assert_eq!(int_of(&db, &["l-len", "gone"]), 0);
    assert_eq!(klyv(&db, &["get", "s"]).0.trim(), "hello");
    // The TTL survived, and new writes count from the migrated length.
    assert!(ttl_of(&db, "p-ttl", "l") > 1_000_000_000);
    assert_eq!(int_of(&db, &["r-push", "l", "d"]), 4);
    assert_eq!(int_of(&db, &["s-add", "st", "n", "o"]), 1);
    assert_eq!(int_of(&db, &["s-card", "st"]), 3);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    drop(conn);
    assert_len_invariant(&db);
    assert_v1_binary_cannot_open(&db);
}

// Every command that adds or removes elements, plus the ways a key can
// change type or disappear, leaves the stored lengths true.
#[test]
fn test_len_invariant_across_every_mutation() {
    let db = fresh_db();
    let steps: &[&[&str]] = &[
        &["r-push", "l", "a", "b", "c", "b", "d"],
        &["l-push", "l", "z"],
        &["l-pop", "l"],
        &["r-pop", "l"],
        &["l-rem", "l", "0", "b"],
        &["l-rem", "l", "5", "nope"],
        &["l-insert", "l", "before", "c", "x"],
        &["l-insert", "l", "after", "nope", "x"],
        &["l-set", "l", "0", "A"],
        &["r-push", "l", "1", "2", "3", "4", "5"],
        &["l-trim", "l", "1", "-2"],
        &["l-trim", "l", "0", "100"],
        &["s-add", "s", "a", "b", "c"],
        &["s-add", "s", "a", "d"],
        &["s-add", "s", "a"],
        &["s-rem", "s", "a", "nope"],
        &["s-rem", "s", "nope"],
        &["s-pop", "s"],
        &["h-set", "h", "f", "1", "g", "2"],
        &["h-set", "h", "f", "9", "new", "3"],
        &["h-incr-by", "h", "g", "5"],
        &["h-incr-by", "h", "fresh", "5"],
        &["h-incr-by", "h2", "n", "1"],
        &["h-del", "h", "f", "nope"],
        &["h-del", "h", "nope"],
        &["rename", "l", "l2"],
        &["rename", "s", "h2"],
        &["set", "h", "now-a-string"],
        &["m-set", "l2", "also-a-string", "fresh", "v"],
        &["append", "fresh", "more"],
        &["incr", "counter"],
        &["r-push", "t", "a", "b"],
        &["l-trim", "t", "5", "9"],
        &["r-push", "e", "a"],
        &["p-expire-at", "e", "1"],
        &["r-push", "e", "b", "c"],
        &["s-add", "e2", "a", "b"],
        &["p-expire-at", "e2", "1"],
        &["h-set", "e2", "f", "v"],
        &["r-push", "last", "only"],
        &["l-pop", "last"],
        &["s-add", "last", "only"],
        &["s-rem", "last", "only"],
        &["h-set", "last", "only", "1"],
        &["h-del", "last", "only"],
    ];
    for step in steps {
        klyv(&db, step);
        assert_len_invariant(&db);
    }
    // And the lengths reported are the ones the invariant just vouched for.
    assert_eq!(int_of(&db, &["l-len", "e"]), 2);
    assert_eq!(int_of(&db, &["h-len", "e2"]), 1);
    assert_eq!(int_of(&db, &["s-card", "h2"]), 2);
    assert_eq!(klyv(&db, &["type", "h"]).0.trim(), "string");
    assert_eq!(klyv(&db, &["exists", "last"]).0.trim(), "(integer) 0");
    assert_eq!(klyv(&db, &["exists", "t"]).0.trim(), "(integer) 0");
}

// Positional reads walk from whichever end is nearer. Check every position
// and a spread of ranges on a list long enough to exercise both directions,
// against a plain Vec.
#[test]
fn test_list_positions_from_either_end_match_a_model() {
    let db = fresh_db();
    let n = 41i64;
    let mut model: Vec<String> = (0..n).map(|i| format!("v{i}")).collect();
    let mut script = String::from("r-push big");
    for v in &model {
        script.push(' ');
        script.push_str(v);
    }
    script.push('\n');
    // Overwrite a few positions from both ends.
    for i in [0i64, 1, 19, 20, 21, 39, 40, -1, -2, -41] {
        let pos = if i < 0 { n + i } else { i } as usize;
        model[pos] = format!("w{i}");
        script.push_str(&format!("l-set big {i} w{i}\n"));
    }
    let mut expected = String::from("41\n");
    expected.push_str(&"OK\n".repeat(10));
    for i in -n..n {
        let pos = if i < 0 { n + i } else { i } as usize;
        script.push_str(&format!("l-index big {i}\n"));
        expected.push_str(&model[pos]);
        expected.push('\n');
    }
    for (start, stop) in [
        (0, -1),
        (0, 0),
        (40, 40),
        (-1, -1),
        (35, 39),
        (1, 3),
        (10, 30),
        (30, 39),
        (-5, -2),
        (38, 100),
        (-100, 2),
        (20, 20),
        (21, 21),
        (5, 4),
    ] {
        script.push_str(&format!("l-range big {start} {stop}\n"));
        let lo = (if start < 0 {
            (n + start).max(0)
        } else {
            start.min(n)
        }) as usize;
        let hi = if stop < 0 { n + stop } else { stop.min(n - 1) };
        if hi >= lo as i64 {
            for v in &model[lo..=hi as usize] {
                expected.push_str(v);
                expected.push('\n');
            }
        }
    }
    let (out, err, ok) = klyv_pipe(&db, &["--format", "raw"], &script);
    assert!(ok, "{err}");
    assert_eq!(out, expected);
    assert_len_invariant(&db);
}

// === Phase 2 commands from the CLI ===

#[test]
fn test_cli_scan_iterates_everything() {
    let db = fresh_db();
    let mut script = String::new();
    for i in 0..23 {
        script.push_str(&format!("set key:{i} v\n"));
    }
    script.push_str("r-push list a\nh-set h f1 1 f2 2 f3 3\ns-add s m1 m2 m3\n");
    assert!(klyv_pipe(&db, &[], &script).2);

    // JSON gives [cursor, [items]].
    let page = |args: &[&str]| -> (String, Vec<String>) {
        let (out, err, ok) = klyv_fmt(&db, "json", args);
        assert!(ok, "{args:?}: {err}");
        let out = out.trim();
        let (cursor, items) = out
            .strip_prefix("[\"")
            .and_then(|s| s.split_once("\",["))
            .unwrap_or_else(|| panic!("unexpected scan output {out:?}"));
        let items = items.trim_end_matches("]]");
        let items = if items.is_empty() {
            vec![]
        } else {
            items
                .split(',')
                .map(|s| s.trim_matches('"').to_string())
                .collect()
        };
        (cursor.to_string(), items)
    };
    let mut cursor = String::from("0");
    let mut keys = Vec::new();
    loop {
        let (next, items) = page(&["scan", &cursor, "--count", "5"]);
        assert!(items.len() <= 5);
        keys.extend(items);
        if next == "0" {
            break;
        }
        cursor = next;
    }
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 26);

    assert_eq!(
        page(&["scan", "--match", "key:1?", "--count", "100"])
            .1
            .len(),
        10
    );
    assert_eq!(
        page(&["scan", "--type", "list", "--count", "100"]).1,
        ["list"]
    );
    assert_eq!(page(&["h-scan", "h", "--no-values"]).1, ["f1", "f2", "f3"]);
    assert_eq!(page(&["h-scan", "h", "--match", "f2"]).1, ["f2", "2"]);
    assert_eq!(page(&["s-scan", "s"]).1.len(), 3);

    // Human output: the cursor on its own line, then the items.
    let (out, _, _) = klyv(&db, &["scan", "--type", "hash", "--count", "100"]);
    assert_eq!(out, "0\n1) \"h\"\n");
    let (_, err, ok) = klyv(&db, &["scan", "--count", "0"]);
    assert!(!ok);
    assert_eq!(err.trim(), "ERR syntax error");
}

#[test]
fn test_cli_counted_pops_and_random_members() {
    let db = fresh_db();
    klyv(&db, &["r-push", "l", "a", "b", "c", "d"]);
    assert_eq!(klyv(&db, &["l-pop", "l", "2"]).0, "1) \"a\"\n2) \"b\"\n");
    assert_eq!(
        klyv_fmt(&db, "json", &["r-pop", "l", "5"]).0.trim(),
        r#"["d","c"]"#
    );
    // A missing key is nil, not an empty list.
    assert_eq!(klyv(&db, &["l-pop", "l", "2"]).0, "(nil)\n");
    assert_eq!(klyv_fmt(&db, "json", &["l-pop", "l", "2"]).0.trim(), "null");
    let (_, err, ok) = klyv(&db, &["l-pop", "l", "-1"]);
    assert!(!ok);
    assert_eq!(err.trim(), "ERR value is out of range, must be positive");

    klyv(&db, &["s-add", "s", "a", "b", "c"]);
    let (out, _, _) = klyv_fmt(&db, "json", &["s-rand-member", "s", "-7"]);
    assert_eq!(
        out.matches('"').count(),
        14,
        "seven picks with repeats: {out}"
    );
    let (out, _, _) = klyv_fmt(&db, "raw", &["s-rand-member", "s", "2"]);
    let picked: Vec<&str> = out.lines().collect();
    assert_eq!(picked.len(), 2);
    assert_ne!(
        picked[0], picked[1],
        "a positive count gives distinct members"
    );
    assert_eq!(klyv(&db, &["s-card", "s"]).0.trim(), "(integer) 3");
    let (out, _, _) = klyv_fmt(&db, "raw", &["s-pop", "s", "2"]);
    assert_eq!(out.lines().count(), 2);
    assert_eq!(klyv(&db, &["s-card", "s"]).0.trim(), "(integer) 1");
    assert_eq!(klyv(&db, &["s-pop", "nokey", "3"]).0.trim(), "(empty set)");
    assert_len_invariant(&db);
}

#[test]
fn test_cli_option_conflicts_and_multi_key_exists() {
    let db = fresh_db();
    klyv(&db, &["m-set", "a", "1", "b", "2"]);
    assert_eq!(
        klyv(&db, &["exists", "a", "b", "a", "nope"]).0.trim(),
        "(integer) 3"
    );

    // get-ex takes at most one expiry option.
    let (_, err, ok) = klyv(&db, &["get-ex", "a", "--ex", "10", "--persist"]);
    assert!(!ok);
    assert!(err.contains("cannot be used with"), "{err}");
    // expire's conditions report Redis's own error when they contradict.
    let (_, err, ok) = klyv(&db, &["expire", "a", "10", "--nx", "--gt"]);
    assert!(!ok);
    assert_eq!(
        err.trim(),
        "ERR NX and XX, GT or LT options at the same time are not compatible"
    );
    assert_eq!(klyv(&db, &["ttl", "a"]).0.trim(), "(integer) -1");
    assert_eq!(
        klyv(&db, &["expire", "a", "1000", "--nx"]).0.trim(),
        "(integer) 1"
    );
    assert_eq!(
        klyv(&db, &["expire", "a", "5", "--gt"]).0.trim(),
        "(integer) 0"
    );
    assert_eq!(
        klyv(&db, &["h-m-get", "a", "f"]).1.trim().split(' ').next(),
        Some("WRONGTYPE")
    );
    assert_eq!(klyv(&db, &["copy", "a", "b"]).0.trim(), "(integer) 0");
    assert_eq!(
        klyv(&db, &["copy", "a", "b", "--replace"]).0.trim(),
        "(integer) 1"
    );
    assert_eq!(klyv(&db, &["get-set", "b", "new"]).0.trim(), "1");
}

// The main thread's stack is the platform's choice (1 MB on Windows), and a
// debug build of the argument parser needs more than that. The work runs on
// a thread with an explicit stack, so a small limit must not matter.
#[cfg(unix)]
#[test]
fn test_runs_with_a_small_main_thread_stack() {
    let db = fresh_db();
    let out = Command::new("sh")
        .arg("-c")
        .arg("ulimit -s 512 && exec \"$0\" --db \"$1\" set k v")
        .arg(env!("CARGO_BIN_EXE_klyv"))
        .arg(&db)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "crashed with a 512 KB stack: {:?} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "OK");
}
