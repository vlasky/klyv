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
    assert!(out.contains("(integer)"));

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
    let ttl: i64 = out.trim().strip_prefix("(integer) ").unwrap().parse().unwrap();
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
    let ttl: i64 = out.trim().strip_prefix("(integer) ").unwrap().parse().unwrap();
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
    let ttl: i64 = out.trim().strip_prefix("(integer) ").unwrap().parse().unwrap();
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
fn test_set_overwrites_clears_expiry_not() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "100"]);
    klyv(&db, &["set", "k", "new"]);

    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let ttl: i64 = out.trim().strip_prefix("(integer) ").unwrap().parse().unwrap();
    // SET does not clear expiry (Redis-compatible: only DEL/PERSIST clear it)
    assert!(ttl > 0);
}

#[test]
fn test_rename_preserves_expiry() {
    let db = fresh_db();
    klyv(&db, &["set", "old", "v"]);
    klyv(&db, &["expire", "old", "100"]);
    klyv(&db, &["rename", "old", "new"]);

    let (out, _, _) = klyv(&db, &["ttl", "new"]);
    let ttl: i64 = out.trim().strip_prefix("(integer) ").unwrap().parse().unwrap();
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
    let (out, _, _) = klyv(&db, &["l-len", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
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
    let (out, _, _) = klyv(&db, &["l-len", "dst"]);
    assert_eq!(out.trim(), "(integer) 0");
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
fn test_set_preserves_live_ttl() {
    let db = fresh_db();
    klyv(&db, &["set", "k", "v"]);
    klyv(&db, &["expire", "k", "1000"]);
    klyv(&db, &["set", "k", "v2"]);
    let (out, _, _) = klyv(&db, &["ttl", "k"]);
    let n: i64 = out.trim().trim_start_matches("(integer) ").parse().unwrap();
    assert!(n > 0 && n <= 1000, "ttl was {n}");
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
    let (out, _, _) = klyv(&db, &["l-len", "k"]);
    assert_eq!(out.trim(), "(integer) 0");
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
fn test_keys_backslash_is_literal() {
    let db = fresh_db();
    klyv(&db, &["set", "a\\b", "1"]);
    klyv(&db, &["set", "axb", "1"]);
    let (out, _, _) = klyv(&db, &["keys", "a\\b"]);
    assert!(out.contains("a\\b"), "out: {out}");
    assert!(!out.contains("\"axb\""), "out: {out}");
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
