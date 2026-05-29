use clap::{Parser, Subcommand};
use rusqlite::{Connection, params};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "klyv", about = "Redis-compatible embedded KV store backed by SQLite")]
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
    #[command(about = "Set key expiry in seconds from now")]
    Expire { key: String, seconds: i64 },
    #[command(about = "Set key expiry in milliseconds from now")]
    PExpire { key: String, milliseconds: i64 },
    #[command(about = "Set key expiry at Unix timestamp (seconds)")]
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

fn open_db(path: &PathBuf) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;

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

fn is_expired(conn: &Connection, key: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM expiry WHERE key = ?1 AND expires_at < unixepoch()",
        params![key],
        |_| Ok(()),
    ).is_ok()
}

fn main() {
    let cli = Cli::parse();
    let conn = open_db(&cli.db).expect("failed to open database");

    match cli.command {
        Command::Set { key, value } => cmd_set(&conn, &key, &value),
        Command::Get { key } => cmd_get(&conn, &key),
        Command::Del { keys } => cmd_del(&conn, &keys),
        Command::Incr { key } => cmd_incrby(&conn, &key, 1),
        Command::Decr { key } => cmd_incrby(&conn, &key, -1),
        Command::IncrBy { key, amount } => cmd_incrby(&conn, &key, amount),
        Command::DecrBy { key, amount } => cmd_incrby(&conn, &key, -amount),
        Command::Append { key, value } => cmd_append(&conn, &key, &value),
        Command::Strlen { key } => cmd_strlen(&conn, &key),
        Command::MSet { pairs } => cmd_mset(&conn, &pairs),
        Command::MGet { keys } => cmd_mget(&conn, &keys),

        Command::LPush { key, values } => cmd_lpush(&conn, &key, &values),
        Command::RPush { key, values } => cmd_rpush(&conn, &key, &values),
        Command::LPop { key } => cmd_lpop(&conn, &key),
        Command::RPop { key } => cmd_rpop(&conn, &key),
        Command::LRange { key, start, stop } => cmd_lrange(&conn, &key, start, stop),
        Command::LLen { key } => cmd_llen(&conn, &key),
        Command::LRem { key, count, value } => cmd_lrem(&conn, &key, count, &value),
        Command::LPos { key, value } => cmd_lpos(&conn, &key, &value),

        Command::SAdd { key, members } => cmd_sadd(&conn, &key, &members),
        Command::SRem { key, members } => cmd_srem(&conn, &key, &members),
        Command::SMembers { key } => cmd_smembers(&conn, &key),
        Command::SIsMember { key, member } => cmd_sismember(&conn, &key, &member),
        Command::SCard { key } => cmd_scard(&conn, &key),
        Command::SUnion { keys } => cmd_sunion(&conn, &keys),
        Command::SInter { keys } => cmd_sinter(&conn, &keys),
        Command::SDiff { keys } => cmd_sdiff(&conn, &keys),

        Command::HSet { key, pairs } => cmd_hset(&conn, &key, &pairs),
        Command::HGet { key, field } => cmd_hget(&conn, &key, &field),
        Command::HDel { key, fields } => cmd_hdel(&conn, &key, &fields),
        Command::HGetAll { key } => cmd_hgetall(&conn, &key),
        Command::HKeys { key } => cmd_hkeys(&conn, &key),
        Command::HVals { key } => cmd_hvals(&conn, &key),
        Command::HLen { key } => cmd_hlen(&conn, &key),

        Command::Keys { pattern } => cmd_keys(&conn, pattern.as_deref()),
        Command::Exists { key } => cmd_exists(&conn, &key),
        Command::Type { key } => cmd_type(&conn, &key),
        Command::Rename { key, newkey } => cmd_rename(&conn, &key, &newkey),

        Command::Expire { key, seconds } => cmd_expire(&conn, &key, seconds),
        Command::PExpire { key, milliseconds } => cmd_pexpire(&conn, &key, milliseconds),
        Command::ExpireAt { key, timestamp } => cmd_expireat(&conn, &key, timestamp),
        Command::Ttl { key } => cmd_ttl(&conn, &key),
        Command::Persist { key } => cmd_persist(&conn, &key),
        Command::Purge => cmd_purge(&conn),

        Command::DbSize => cmd_dbsize(&conn),
        Command::FlushAll => cmd_flushall(&conn),
    }
}

// --- String commands ---

fn cmd_set(conn: &Connection, key: &str, value: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, value],
    ).unwrap();
    println!("OK");
}

fn cmd_get(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(nil)");
        return;
    }
    let result: Option<String> = conn
        .query_row("SELECT value FROM strings WHERE key = ?1", params![key], |row| row.get(0))
        .ok();
    match result {
        Some(v) => println!("{v}"),
        None => println!("(nil)"),
    }
}

fn cmd_del(conn: &Connection, keys: &[String]) {
    let mut count = 0u64;
    for key in keys {
        count += conn.execute("DELETE FROM strings WHERE key = ?1", params![key]).unwrap() as u64;
        count += conn.execute("DELETE FROM list_items WHERE key = ?1", params![key]).unwrap() as u64;
        count += conn.execute("DELETE FROM set_members WHERE key = ?1", params![key]).unwrap() as u64;
        count += conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key]).unwrap() as u64;
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key]).unwrap();
    }
    println!("(integer) {count}");
}

fn cmd_incrby(conn: &Connection, key: &str, amount: i64) {
    let current: Option<String> = if is_expired(conn, key) {
        None
    } else {
        conn.query_row("SELECT value FROM strings WHERE key = ?1", params![key], |row| row.get(0))
            .ok()
    };
    let val: i64 = current
        .map(|s| s.parse::<i64>().expect("ERR value is not an integer"))
        .unwrap_or(0);
    let new_val = val + amount;
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, new_val.to_string()],
    ).unwrap();
    println!("(integer) {new_val}");
}

fn cmd_append(conn: &Connection, key: &str, value: &str) {
    let current: Option<String> = conn
        .query_row("SELECT value FROM strings WHERE key = ?1", params![key], |row| row.get(0))
        .ok();
    let new_val = match current {
        Some(existing) => format!("{existing}{value}"),
        None => value.to_string(),
    };
    let len = new_val.len();
    conn.execute(
        "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
        params![key, new_val],
    ).unwrap();
    println!("(integer) {len}");
}

fn cmd_strlen(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(integer) 0");
        return;
    }
    let result: Option<String> = conn
        .query_row("SELECT value FROM strings WHERE key = ?1", params![key], |row| row.get(0))
        .ok();
    let len = result.map(|s| s.len()).unwrap_or(0);
    println!("(integer) {len}");
}

fn cmd_mset(conn: &Connection, pairs: &[String]) {
    if pairs.len() % 2 != 0 {
        eprintln!("ERR wrong number of arguments for 'mset' command");
        std::process::exit(1);
    }
    let tx = conn.unchecked_transaction().unwrap();
    for chunk in pairs.chunks(2) {
        tx.execute(
            "INSERT OR REPLACE INTO strings (key, value) VALUES (?1, ?2)",
            params![chunk[0], chunk[1]],
        ).unwrap();
    }
    tx.commit().unwrap();
    println!("OK");
}

fn cmd_mget(conn: &Connection, keys: &[String]) {
    for key in keys {
        if is_expired(conn, key) {
            println!("(nil)");
            continue;
        }
        let result: Option<String> = conn
            .query_row("SELECT value FROM strings WHERE key = ?1", params![key], |row| row.get(0))
            .ok();
        match result {
            Some(v) => println!("{v}"),
            None => println!("(nil)"),
        }
    }
}

// --- List commands ---

fn list_min_idx(conn: &Connection, key: &str) -> f64 {
    conn.query_row(
        "SELECT MIN(idx) FROM list_items WHERE key = ?1",
        params![key],
        |row| row.get::<_, Option<f64>>(0),
    ).unwrap().unwrap_or(0.0)
}

fn list_max_idx(conn: &Connection, key: &str) -> f64 {
    conn.query_row(
        "SELECT MAX(idx) FROM list_items WHERE key = ?1",
        params![key],
        |row| row.get::<_, Option<f64>>(0),
    ).unwrap().unwrap_or(0.0)
}

fn cmd_lpush(conn: &Connection, key: &str, values: &[String]) {
    let tx = conn.unchecked_transaction().unwrap();
    for value in values {
        let min = list_min_idx(&tx, key);
        tx.execute(
            "INSERT INTO list_items (key, idx, value) VALUES (?1, ?2, ?3)",
            params![key, min - 1.0, value],
        ).unwrap();
    }
    let len: i64 = tx.query_row(
        "SELECT COUNT(*) FROM list_items WHERE key = ?1",
        params![key],
        |row| row.get(0),
    ).unwrap();
    tx.commit().unwrap();
    println!("(integer) {len}");
}

fn cmd_rpush(conn: &Connection, key: &str, values: &[String]) {
    let tx = conn.unchecked_transaction().unwrap();
    for value in values {
        let max = list_max_idx(&tx, key);
        tx.execute(
            "INSERT INTO list_items (key, idx, value) VALUES (?1, ?2, ?3)",
            params![key, max + 1.0, value],
        ).unwrap();
    }
    let len: i64 = tx.query_row(
        "SELECT COUNT(*) FROM list_items WHERE key = ?1",
        params![key],
        |row| row.get(0),
    ).unwrap();
    tx.commit().unwrap();
    println!("(integer) {len}");
}

fn cmd_lpop(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(nil)");
        return;
    }
    let result: Option<(i64, String)> = conn
        .query_row(
            "SELECT rowid, value FROM list_items WHERE key = ?1 ORDER BY idx ASC LIMIT 1",
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    match result {
        Some((rowid, value)) => {
            conn.execute("DELETE FROM list_items WHERE rowid = ?1", params![rowid]).unwrap();
            println!("{value}");
        }
        None => println!("(nil)"),
    }
}

fn cmd_rpop(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(nil)");
        return;
    }
    let result: Option<(i64, String)> = conn
        .query_row(
            "SELECT rowid, value FROM list_items WHERE key = ?1 ORDER BY idx DESC LIMIT 1",
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    match result {
        Some((rowid, value)) => {
            conn.execute("DELETE FROM list_items WHERE rowid = ?1", params![rowid]).unwrap();
            println!("{value}");
        }
        None => println!("(nil)"),
    }
}

fn cmd_lrange(conn: &Connection, key: &str, start: i64, stop: i64) {
    if is_expired(conn, key) {
        println!("(empty list)");
        return;
    }
    let len: i64 = conn.query_row(
        "SELECT COUNT(*) FROM list_items WHERE key = ?1",
        params![key],
        |row| row.get(0),
    ).unwrap();

    if len == 0 {
        println!("(empty list)");
        return;
    }

    let s = if start < 0 { (len + start).max(0) } else { start.min(len) };
    let e = if stop < 0 { (len + stop).max(0) } else { stop.min(len - 1) };

    if s > e {
        println!("(empty list)");
        return;
    }

    let limit = e - s + 1;
    let mut stmt = conn.prepare(
        "SELECT value FROM list_items WHERE key = ?1 ORDER BY idx ASC LIMIT ?2 OFFSET ?3"
    ).unwrap();
    let rows: Vec<String> = stmt
        .query_map(params![key, limit, s], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    for (i, val) in rows.iter().enumerate() {
        println!("{}) \"{val}\"", i + 1);
    }
}

fn cmd_llen(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(integer) 0");
        return;
    }
    let len: i64 = conn.query_row(
        "SELECT COUNT(*) FROM list_items WHERE key = ?1",
        params![key],
        |row| row.get(0),
    ).unwrap();
    println!("(integer) {len}");
}

fn cmd_lrem(conn: &Connection, key: &str, count: i64, value: &str) {
    let (order, limit) = match count.cmp(&0) {
        std::cmp::Ordering::Greater => ("ASC", count as usize),
        std::cmp::Ordering::Less => ("DESC", (-count) as usize),
        std::cmp::Ordering::Equal => ("ASC", usize::MAX),
    };

    let sql = format!(
        "SELECT rowid FROM list_items WHERE key = ?1 AND value = ?2 ORDER BY idx {}",
        order
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let rowids: Vec<i64> = stmt
        .query_map(params![key, value], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .take(limit)
        .collect();

    let removed = rowids.len() as i64;
    for rowid in &rowids {
        conn.execute("DELETE FROM list_items WHERE rowid = ?1", params![rowid]).unwrap();
    }
    println!("(integer) {removed}");
}

fn cmd_lpos(conn: &Connection, key: &str, value: &str) {
    if is_expired(conn, key) {
        println!("(nil)");
        return;
    }
    let mut stmt = conn.prepare(
        "SELECT value FROM list_items WHERE key = ?1 ORDER BY idx ASC"
    ).unwrap();
    let rows: Vec<String> = stmt
        .query_map(params![key], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    match rows.iter().position(|v| v == value) {
        Some(pos) => println!("(integer) {pos}"),
        None => println!("(nil)"),
    }
}

// --- Set commands ---

fn cmd_sadd(conn: &Connection, key: &str, members: &[String]) {
    let mut count = 0i64;
    for member in members {
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO set_members (key, member) VALUES (?1, ?2)",
            params![key, member],
        ).unwrap();
        count += inserted as i64;
    }
    println!("(integer) {count}");
}

fn cmd_srem(conn: &Connection, key: &str, members: &[String]) {
    let mut count = 0i64;
    for member in members {
        let deleted = conn.execute(
            "DELETE FROM set_members WHERE key = ?1 AND member = ?2",
            params![key, member],
        ).unwrap();
        count += deleted as i64;
    }
    println!("(integer) {count}");
}

fn cmd_smembers(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(empty set)");
        return;
    }
    let mut stmt = conn.prepare("SELECT member FROM set_members WHERE key = ?1").unwrap();
    let rows: Vec<String> = stmt
        .query_map(params![key], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    if rows.is_empty() {
        println!("(empty set)");
    } else {
        for (i, member) in rows.iter().enumerate() {
            println!("{}) \"{member}\"", i + 1);
        }
    }
}

fn cmd_sismember(conn: &Connection, key: &str, member: &str) {
    if is_expired(conn, key) {
        println!("(integer) 0");
        return;
    }
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM set_members WHERE key = ?1 AND member = ?2",
            params![key, member],
            |_| Ok(()),
        )
        .is_ok();
    println!("(integer) {}", if exists { 1 } else { 0 });
}

fn cmd_scard(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(integer) 0");
        return;
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM set_members WHERE key = ?1",
        params![key],
        |row| row.get(0),
    ).unwrap();
    println!("(integer) {count}");
}

fn cmd_sunion(conn: &Connection, keys: &[String]) {
    if keys.is_empty() { return; }
    let placeholders: Vec<String> = keys.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT DISTINCT member FROM set_members WHERE key IN ({})",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let params: Vec<&dyn rusqlite::ToSql> = keys.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
    let rows: Vec<String> = stmt
        .query_map(params.as_slice(), |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    if rows.is_empty() {
        println!("(empty set)");
    } else {
        for (i, member) in rows.iter().enumerate() {
            println!("{}) \"{member}\"", i + 1);
        }
    }
}

fn cmd_sinter(conn: &Connection, keys: &[String]) {
    if keys.is_empty() { return; }
    let num_keys = keys.len();
    let placeholders: Vec<String> = keys.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT member FROM set_members WHERE key IN ({}) GROUP BY member HAVING COUNT(DISTINCT key) = ?{}",
        placeholders.join(", "),
        num_keys + 1
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = keys.iter().map(|k| Box::new(k.clone()) as Box<dyn rusqlite::ToSql>).collect();
    params.push(Box::new(num_keys as i64));
    let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows: Vec<String> = stmt
        .query_map(params_ref.as_slice(), |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    if rows.is_empty() {
        println!("(empty set)");
    } else {
        for (i, member) in rows.iter().enumerate() {
            println!("{}) \"{member}\"", i + 1);
        }
    }
}

fn cmd_sdiff(conn: &Connection, keys: &[String]) {
    if keys.is_empty() { return; }
    let first = &keys[0];
    if keys.len() == 1 {
        cmd_smembers(conn, first);
        return;
    }
    let rest = &keys[1..];
    let placeholders: Vec<String> = rest.iter().enumerate().map(|(i, _)| format!("?{}", i + 2)).collect();
    let sql = format!(
        "SELECT member FROM set_members WHERE key = ?1 AND member NOT IN (SELECT member FROM set_members WHERE key IN ({}))",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![first as &dyn rusqlite::ToSql];
    for k in rest {
        params.push(k as &dyn rusqlite::ToSql);
    }
    let rows: Vec<String> = stmt
        .query_map(params.as_slice(), |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    if rows.is_empty() {
        println!("(empty set)");
    } else {
        for (i, member) in rows.iter().enumerate() {
            println!("{}) \"{member}\"", i + 1);
        }
    }
}

// --- Hash commands ---

fn cmd_hset(conn: &Connection, key: &str, pairs: &[String]) {
    if pairs.len() % 2 != 0 {
        eprintln!("ERR wrong number of arguments for 'hset' command");
        std::process::exit(1);
    }
    let mut count = 0i64;
    let tx = conn.unchecked_transaction().unwrap();
    for chunk in pairs.chunks(2) {
        let existed: bool = tx
            .query_row(
                "SELECT 1 FROM hash_fields WHERE key = ?1 AND field = ?2",
                params![key, chunk[0]],
                |_| Ok(()),
            )
            .is_ok();
        tx.execute(
            "INSERT OR REPLACE INTO hash_fields (key, field, value) VALUES (?1, ?2, ?3)",
            params![key, chunk[0], chunk[1]],
        ).unwrap();
        if !existed {
            count += 1;
        }
    }
    tx.commit().unwrap();
    println!("(integer) {count}");
}

fn cmd_hget(conn: &Connection, key: &str, field: &str) {
    if is_expired(conn, key) {
        println!("(nil)");
        return;
    }
    let result: Option<String> = conn
        .query_row(
            "SELECT value FROM hash_fields WHERE key = ?1 AND field = ?2",
            params![key, field],
            |row| row.get(0),
        )
        .ok();
    match result {
        Some(v) => println!("{v}"),
        None => println!("(nil)"),
    }
}

fn cmd_hdel(conn: &Connection, key: &str, fields: &[String]) {
    let mut count = 0i64;
    for field in fields {
        let deleted = conn.execute(
            "DELETE FROM hash_fields WHERE key = ?1 AND field = ?2",
            params![key, field],
        ).unwrap();
        count += deleted as i64;
    }
    println!("(integer) {count}");
}

fn cmd_hgetall(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(empty hash)");
        return;
    }
    let mut stmt = conn.prepare("SELECT field, value FROM hash_fields WHERE key = ?1").unwrap();
    let rows: Vec<(String, String)> = stmt
        .query_map(params![key], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    if rows.is_empty() {
        println!("(empty hash)");
    } else {
        for (i, (field, value)) in rows.iter().enumerate() {
            println!("{}) \"{field}\"", i * 2 + 1);
            println!("{}) \"{value}\"", i * 2 + 2);
        }
    }
}

fn cmd_hkeys(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(empty list)");
        return;
    }
    let mut stmt = conn.prepare("SELECT field FROM hash_fields WHERE key = ?1").unwrap();
    let rows: Vec<String> = stmt
        .query_map(params![key], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    if rows.is_empty() {
        println!("(empty list)");
    } else {
        for (i, field) in rows.iter().enumerate() {
            println!("{}) \"{field}\"", i + 1);
        }
    }
}

fn cmd_hvals(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(empty list)");
        return;
    }
    let mut stmt = conn.prepare("SELECT value FROM hash_fields WHERE key = ?1").unwrap();
    let rows: Vec<String> = stmt
        .query_map(params![key], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    if rows.is_empty() {
        println!("(empty list)");
    } else {
        for (i, value) in rows.iter().enumerate() {
            println!("{}) \"{value}\"", i + 1);
        }
    }
}

fn cmd_hlen(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(integer) 0");
        return;
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM hash_fields WHERE key = ?1",
        params![key],
        |row| row.get(0),
    ).unwrap();
    println!("(integer) {count}");
}

// --- Key commands ---

fn cmd_keys(conn: &Connection, pattern: Option<&str>) {
    let like_pattern = pattern
        .map(|p| p.replace('*', "%").replace('?', "_"))
        .unwrap_or_else(|| "%".to_string());

    let mut all_keys: Vec<String> = Vec::new();

    let mut stmt = conn.prepare("SELECT key FROM strings WHERE key LIKE ?1").unwrap();
    all_keys.extend(stmt.query_map(params![like_pattern], |row| row.get(0)).unwrap().map(|r| r.unwrap()));

    let mut stmt = conn.prepare("SELECT DISTINCT key FROM list_items WHERE key LIKE ?1").unwrap();
    all_keys.extend(stmt.query_map(params![like_pattern], |row| row.get(0)).unwrap().map(|r| r.unwrap()));

    let mut stmt = conn.prepare("SELECT DISTINCT key FROM set_members WHERE key LIKE ?1").unwrap();
    all_keys.extend(stmt.query_map(params![like_pattern], |row| row.get(0)).unwrap().map(|r| r.unwrap()));

    let mut stmt = conn.prepare("SELECT DISTINCT key FROM hash_fields WHERE key LIKE ?1").unwrap();
    all_keys.extend(stmt.query_map(params![like_pattern], |row| row.get(0)).unwrap().map(|r| r.unwrap()));

    all_keys.sort();
    all_keys.dedup();
    all_keys.retain(|k| !is_expired(conn, k));

    if all_keys.is_empty() {
        println!("(empty list)");
    } else {
        for (i, key) in all_keys.iter().enumerate() {
            println!("{}) \"{key}\"", i + 1);
        }
    }
}

fn cmd_exists(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("(integer) 0");
        return;
    }
    let in_strings: bool = conn.query_row("SELECT 1 FROM strings WHERE key = ?1", params![key], |_| Ok(())).is_ok();
    let in_lists: bool = conn.query_row("SELECT 1 FROM list_items WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok();
    let in_sets: bool = conn.query_row("SELECT 1 FROM set_members WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok();
    let in_hashes: bool = conn.query_row("SELECT 1 FROM hash_fields WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok();
    let exists = in_strings || in_lists || in_sets || in_hashes;
    println!("(integer) {}", if exists { 1 } else { 0 });
}

fn cmd_type(conn: &Connection, key: &str) {
    if is_expired(conn, key) {
        println!("none");
        return;
    }
    if conn.query_row("SELECT 1 FROM strings WHERE key = ?1", params![key], |_| Ok(())).is_ok() {
        println!("string");
    } else if conn.query_row("SELECT 1 FROM list_items WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok() {
        println!("list");
    } else if conn.query_row("SELECT 1 FROM set_members WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok() {
        println!("set");
    } else if conn.query_row("SELECT 1 FROM hash_fields WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok() {
        println!("hash");
    } else {
        println!("none");
    }
}

fn cmd_rename(conn: &Connection, key: &str, newkey: &str) {
    let tx = conn.unchecked_transaction().unwrap();

    let mut found = false;

    if tx.query_row("SELECT 1 FROM strings WHERE key = ?1", params![key], |_| Ok(())).is_ok() {
        tx.execute("DELETE FROM strings WHERE key = ?1", params![newkey]).unwrap();
        tx.execute("UPDATE strings SET key = ?2 WHERE key = ?1", params![key, newkey]).unwrap();
        found = true;
    }
    if tx.query_row("SELECT 1 FROM list_items WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok() {
        tx.execute("DELETE FROM list_items WHERE key = ?1", params![newkey]).unwrap();
        tx.execute("UPDATE list_items SET key = ?2 WHERE key = ?1", params![key, newkey]).unwrap();
        found = true;
    }
    if tx.query_row("SELECT 1 FROM set_members WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok() {
        tx.execute("DELETE FROM set_members WHERE key = ?1", params![newkey]).unwrap();
        tx.execute("UPDATE set_members SET key = ?2 WHERE key = ?1", params![key, newkey]).unwrap();
        found = true;
    }
    if tx.query_row("SELECT 1 FROM hash_fields WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok() {
        tx.execute("DELETE FROM hash_fields WHERE key = ?1", params![newkey]).unwrap();
        tx.execute("UPDATE hash_fields SET key = ?2 WHERE key = ?1", params![key, newkey]).unwrap();
        found = true;
    }

    if !found {
        eprintln!("ERR no such key");
        std::process::exit(1);
    }

    tx.execute("DELETE FROM expiry WHERE key = ?1", params![newkey]).unwrap();
    tx.execute("UPDATE expiry SET key = ?2 WHERE key = ?1", params![key, newkey]).unwrap();

    tx.commit().unwrap();
    println!("OK");
}

// --- Utility commands ---

fn cmd_dbsize(conn: &Connection) {
    let mut count: i64 = 0;
    count += conn.query_row("SELECT COUNT(*) FROM strings", [], |row| row.get::<_, i64>(0)).unwrap();
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM list_items", [], |row| row.get::<_, i64>(0)).unwrap();
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM set_members", [], |row| row.get::<_, i64>(0)).unwrap();
    count += conn.query_row("SELECT COUNT(DISTINCT key) FROM hash_fields", [], |row| row.get::<_, i64>(0)).unwrap();
    println!("(integer) {count}");
}

fn cmd_flushall(conn: &Connection) {
    conn.execute_batch("
        DELETE FROM strings;
        DELETE FROM list_items;
        DELETE FROM set_members;
        DELETE FROM hash_fields;
        DELETE FROM expiry;
    ").unwrap();
    println!("OK");
}

// --- TTL commands ---

fn key_exists_in_data(conn: &Connection, key: &str) -> bool {
    conn.query_row("SELECT 1 FROM strings WHERE key = ?1", params![key], |_| Ok(())).is_ok()
        || conn.query_row("SELECT 1 FROM list_items WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok()
        || conn.query_row("SELECT 1 FROM set_members WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok()
        || conn.query_row("SELECT 1 FROM hash_fields WHERE key = ?1 LIMIT 1", params![key], |_| Ok(())).is_ok()
}

fn cmd_expire(conn: &Connection, key: &str, seconds: i64) {
    if !key_exists_in_data(conn, key) || is_expired(conn, key) {
        println!("(integer) 0");
        return;
    }
    conn.execute(
        "INSERT OR REPLACE INTO expiry (key, expires_at) VALUES (?1, unixepoch() + ?2)",
        params![key, seconds],
    ).unwrap();
    println!("(integer) 1");
}

fn cmd_pexpire(conn: &Connection, key: &str, milliseconds: i64) {
    let seconds = (milliseconds + 999) / 1000;
    cmd_expire(conn, key, seconds);
}

fn cmd_expireat(conn: &Connection, key: &str, timestamp: i64) {
    if !key_exists_in_data(conn, key) || is_expired(conn, key) {
        println!("(integer) 0");
        return;
    }
    conn.execute(
        "INSERT OR REPLACE INTO expiry (key, expires_at) VALUES (?1, ?2)",
        params![key, timestamp],
    ).unwrap();
    println!("(integer) 1");
}

fn cmd_ttl(conn: &Connection, key: &str) {
    if !key_exists_in_data(conn, key) || is_expired(conn, key) {
        println!("(integer) -2");
        return;
    }
    let result: Option<i64> = conn
        .query_row(
            "SELECT expires_at - unixepoch() FROM expiry WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .ok();
    match result {
        Some(remaining) => println!("(integer) {remaining}"),
        None => println!("(integer) -1"),
    }
}

fn cmd_persist(conn: &Connection, key: &str) {
    let removed = conn.execute("DELETE FROM expiry WHERE key = ?1", params![key]).unwrap();
    println!("(integer) {}", if removed > 0 { 1 } else { 0 });
}

fn cmd_purge(conn: &Connection) {
    let expired_keys: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT key FROM expiry WHERE expires_at < unixepoch()"
        ).unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };

    let count = expired_keys.len() as i64;
    for key in &expired_keys {
        conn.execute("DELETE FROM strings WHERE key = ?1", params![key]).unwrap();
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![key]).unwrap();
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![key]).unwrap();
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key]).unwrap();
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key]).unwrap();
    }
    println!("(integer) {count}");
}
