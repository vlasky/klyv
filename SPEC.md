# klyv Specification v0.1.1

A Redis-compatible embedded key-value store backed by SQLite. This document specifies the storage format, command semantics, and CLI interface to enable compatible implementations in any language.

> **v0.1.1** clarifies type safety (`WRONGTYPE`), cross-type `SET`/`MSET`/`RENAME` overwrite, expiry-on-write and the `<=` expiry boundary, negative-TTL handling, integer-overflow errors, `KEYS` LIKE escaping, the `idx = 0.0` first-element rule, and `BEGIN IMMEDIATE`/`busy_timeout` atomicity. The on-disk schema is unchanged from v0.1.

## Overview

klyv stores data in a single SQLite database file. It supports four Redis data types (strings, lists, sets, hashes) with Redis-compatible command semantics. It is a CLI tool — no server, no daemon, no protocol. Just a file.

## Storage Format

### Database Configuration

On open, the following PRAGMAs are set:

```sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA busy_timeout=5000;
```

WAL mode enables concurrent readers and improves write performance. `NORMAL` synchronous is safe against corruption from application crashes (though not OS crashes). `busy_timeout` makes a writer wait for a competing lock instead of failing immediately.

### Schema

The database contains four tables:

```sql
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
```

### Design Rationale

- **Separate tables per type** rather than a single table with a `type` column. Allows type-specific indexing and constraints. A key can only exist in one table; this invariant is enforced at the command level (see [Type Safety](#type-safety)) rather than by the schema.
- **`idx REAL` for lists** uses fractional indexing. LPUSH inserts at `MIN(idx) - 1.0`, RPUSH at `MAX(idx) + 1.0`. This avoids O(n) reindexing on push operations. An empty list starts at `idx = 0.0`.
- **BLOB storage** for values and members. All values are stored as-is. Numeric operations parse the blob as UTF-8 text then as an integer.
- **Separate expiry table** rather than a column on each data table. One table to check, works across all types. Uses absolute Unix timestamps (seconds).
- **Lazy expiry** — expired keys are filtered on read (return nil/empty) but not deleted from disk until `PURGE` is called explicitly. This keeps read operations as reads and avoids surprise writes.

## CLI Interface

### Invocation

```
klyv [OPTIONS] <COMMAND> [ARGS...]
```

### Global Options

| Option | Env Var | Default | Description |
|--------|---------|---------|-------------|
| `-d`, `--db <PATH>` | `KLYV_DB` | (required) | Path to the SQLite database file |

Either `--db` or the `KLYV_DB` environment variable must be provided. There is no default — this avoids silently creating database files in the working directory. The file is created automatically on first use if it doesn't exist.

### Command Naming

Subcommands use **kebab-case** on the CLI (e.g. `s-add`, `l-push`, `h-set`). This document uses Redis-style names (SADD, LPUSH, HSET) for clarity; implementations should accept both.

## Commands

### String Commands

#### SET key value

Store a string value at key. Overwrites any existing value **of any type** — if the key currently holds a list, set, or hash, those rows are deleted first so the key becomes a string. A live TTL is preserved; an already-expired TTL row is cleared so the new value is immediately visible.

```
DELETE FROM list_items   WHERE key = ?;
DELETE FROM set_members  WHERE key = ?;
DELETE FROM hash_fields  WHERE key = ?;
-- clear expiry only if already expired
INSERT OR REPLACE INTO strings (key, value) VALUES (?, ?);
```

**Output:** `OK`

#### GET key

Retrieve the string value at key.

**Output:** The value as a line of text, or `(nil)` if the key does not exist.

#### DEL key [key ...]

Delete one or more keys from ALL tables (strings, lists, sets, hashes). The whole operation runs in a single `BEGIN IMMEDIATE` transaction, so a multi-key delete is atomic and no key is ever left half-deleted (e.g. data rows gone but an expiry row surviving).

**Output:** `(integer) N` where N is the number of *keys* that existed and were deleted, matching Redis. A key that has lazily expired is already logically gone, so it does not count toward N — but its physical rows (including the expiry row) are still reclaimed. Repeating a key in the argument list counts it once.

#### INCR key

Increment the integer value at key by 1. If the key does not exist (or has lazily expired), it is initialized to 0 before incrementing.

**Error:** If the value is not a valid integer, print `ERR value is not an integer` to stderr and exit with code 1. If the result would overflow `i64`, print `ERR increment or decrement would overflow` to stderr and exit with code 1. In both cases the stored value is left unchanged.

**Output:** `(integer) N` where N is the new value.

#### DECR key

Decrement by 1. Same semantics as INCR.

#### INCRBY key amount

Increment by a specified integer amount.

#### DECRBY key amount

Decrement by a specified integer amount. Internally: `INCRBY key (-amount)`.

**Error:** If the result would overflow `i64` (including `DECRBY key -9223372036854775808`, where negating the amount itself overflows), print `ERR increment or decrement would overflow` to stderr and exit with code 1. The stored value is left unchanged.

#### APPEND key value

Append `value` to the existing string at key. If key does not exist, creates it with `value` as the content.

**Output:** `(integer) N` where N is the length of the new string in bytes.

#### STRLEN key

Return the length of the string at key.

**Output:** `(integer) N` (0 if key does not exist).

#### MSET key value [key value ...]

Set multiple keys atomically (within a single transaction). Like `SET`, each key is overwritten regardless of its prior type.

**Error:** If an odd number of arguments is provided, print `ERR wrong number of arguments for 'mset' command` to stderr and exit with code 1.

**Output:** `OK`

#### MGET key [key ...]

Get multiple values.

**Output:** One line per key — the value or `(nil)`.

### List Commands

Lists are ordered sequences. Items are stored with a floating-point index (`idx`) that determines order. This allows O(1) push to either end without reindexing.

#### LPUSH key value [value ...]

Insert values at the head (lowest index) of the list. The first element of an empty list is inserted at `idx = 0.0`; subsequent head insertions go at `MIN(idx) - 1.0`. Multiple values are inserted left-to-right, meaning the last value will be at the head.

**Output:** `(integer) N` where N is the length of the list after the operation.

#### RPUSH key value [value ...]

Insert values at the tail (highest index). The first element of an empty list is inserted at `idx = 0.0`; subsequent tail insertions go at `MAX(idx) + 1.0`.

**Output:** `(integer) N`

#### LPOP key

Remove and return the element at the head of the list.

**Output:** The value, or `(nil)` if the list is empty/doesn't exist.

#### RPOP key

Remove and return the element at the tail.

**Output:** The value, or `(nil)`.

#### LRANGE key start stop

Return a range of elements. Indices are zero-based. Negative indices count from the end (`-1` = last element).

**Index normalization:**
```
if start < 0: start = max(0, len + start)
if stop < 0:  stop = max(0, len + stop)
stop = min(stop, len - 1)
```

If `start > stop` after normalization, return empty.

**Output:** Numbered lines: `1) "value"`, `2) "value"`, ..., or `(empty list)`.

#### LLEN key

**Output:** `(integer) N`

#### LREM key count value

Remove occurrences of `value` from the list.

- `count > 0`: Remove first `count` occurrences scanning from head to tail.
- `count < 0`: Remove first `|count|` occurrences scanning from tail to head.
- `count = 0`: Remove all occurrences.

Implementation: select rowids matching `(key, value)` ordered by `idx ASC` (or `DESC` for negative count), limit to `|count|` (or unlimited for 0), then delete those rows.

**Output:** `(integer) N` where N is the number of elements actually removed.

#### LPOS key value

Find the first occurrence of `value` in the list (scanning head to tail).

**Output:** `(integer) N` where N is the zero-based index, or `(nil)` if not found.

### Set Commands

Sets are unordered collections of unique strings.

#### SADD key member [member ...]

Add members to the set. Duplicates are ignored (INSERT OR IGNORE).

**Output:** `(integer) N` where N is the number of members actually added (not already present).

#### SREM key member [member ...]

Remove members from the set.

**Output:** `(integer) N` where N is the number of members actually removed.

#### SMEMBERS key

Return all members of the set.

**Output:** Numbered lines or `(empty set)`.

#### SISMEMBER key member

Test if member is in the set.

**Output:** `(integer) 1` if present, `(integer) 0` if not.

#### SCARD key

Return the number of members (cardinality).

**Output:** `(integer) N`

For all set operations, an expired input set is treated as empty (consistent with lazy expiry).

#### SUNION key [key ...]

Return the union of all specified sets. Expired input sets contribute no members.

```sql
SELECT DISTINCT member FROM set_members WHERE key IN (?, ?, ...)
```

**Output:** Numbered lines or `(empty set)`.

#### SINTER key [key ...]

Return the intersection of all specified sets. Duplicate key arguments are de-duplicated first, so `SINTER s s` returns the members of `s`. If any input set is expired/missing, the result is empty.

```sql
SELECT member FROM set_members WHERE key IN (?, ?, ...)
GROUP BY member HAVING COUNT(DISTINCT key) = <num_distinct_keys>
```

**Output:** Numbered lines or `(empty set)`.

#### SDIFF key [key ...]

Return members in the first set that are not in any of the other sets. If the first set is expired/missing the result is empty; expired subsequent sets subtract nothing.

```sql
SELECT member FROM set_members WHERE key = ?
AND member NOT IN (SELECT member FROM set_members WHERE key IN (?, ...))
```

If only one key is specified, return all its members.

**Output:** Numbered lines or `(empty set)`.

### Hash Commands

Hashes are maps of field-value pairs stored under a single key.

#### HSET key field value [field value ...]

Set fields in the hash. Creates the hash if it doesn't exist. Overwrites existing fields.

**Output:** `(integer) N` where N is the number of NEW fields added (not fields that were updated).

#### HGET key field

Get a single field's value.

**Output:** The value, or `(nil)`.

#### HDEL key field [field ...]

Delete fields from the hash.

**Output:** `(integer) N` where N is the number of fields actually deleted.

#### HGETALL key

Return all field-value pairs.

**Output:** Alternating numbered lines (field, value, field, value, ...) or `(empty hash)`.
```
1) "field1"
2) "value1"
3) "field2"
4) "value2"
```

#### HKEYS key

Return all field names.

**Output:** Numbered lines or `(empty list)`.

#### HVALS key

Return all values.

**Output:** Numbered lines or `(empty list)`.

#### HLEN key

Return the number of fields.

**Output:** `(integer) N`

### Key Commands

These operate across all data types.

#### KEYS [pattern]

Return all keys matching a glob-style pattern. `*` matches any sequence, `?` matches one character. If no pattern is given, return all keys. Expired keys are excluded.

Implementation: translate `*` to `%` and `?` to `_` for SQL LIKE, escaping any literal `%`, `_`, or `\` in the pattern (via `ESCAPE '\'`) so they match themselves. Query all four tables and deduplicate.

**Output:** Numbered lines or `(empty list)`.

#### EXISTS key

Test if a key exists in any table.

**Output:** `(integer) 1` if exists, `(integer) 0` if not.

#### TYPE key

Return the data type of the key.

**Output:** One of `string`, `list`, `set`, `hash`, or `none`. A key exists in only one table (enforced via the type-safety check below), so the result is unambiguous; an expired key reports `none`.

#### RENAME key newkey

Rename a key, carrying its TTL with it. If `newkey` already exists it is overwritten across **all** tables (so no rows of a different type survive at the target). Renaming a key onto itself (`key == newkey`) is a no-op that returns `OK`. Operates inside a single transaction.

**Error:** If `key` does not exist (or has lazily expired): `ERR no such key` to stderr, exit code 1.

**Output:** `OK`

### TTL Commands

Expiry uses lazy filtering: expired keys are not deleted from disk but are invisible to all read commands, and are treated as absent by write commands (which drop the stale rows before proceeding). Use `PURGE` to reclaim disk space.

A key is expired once the current time **reaches** its `expires_at` — the check is `expires_at <= unixepoch()` (used by both reads and `PURGE`).

#### EXPIRE key seconds

Set a key to expire `seconds` from now. The key must exist and not already be expired. A zero or negative `seconds` stores an already-past timestamp, so the key becomes immediately invisible.

**Output:** `(integer) 1` if the timeout was set, `(integer) 0` if the key does not exist.

#### PEXPIRE key milliseconds

Set a key to expire `milliseconds` from now. Internally rounds up to the nearest second (the expiry table stores seconds). A zero or negative value expires the key immediately.

**Output:** `(integer) 1` or `(integer) 0`.

#### EXPIREAT key timestamp

Set a key to expire at an absolute Unix timestamp (seconds since epoch). A timestamp in the past expires the key immediately.

**Output:** `(integer) 1` or `(integer) 0`.

#### TTL key

Get the remaining time-to-live in seconds.

**Output:**
- `(integer) N` — seconds remaining (positive)
- `(integer) -1` — key exists but has no expiry
- `(integer) -2` — key does not exist (or is expired)

#### PERSIST key

Remove the expiry from a key, making it persist indefinitely. A key that has already lazily expired is treated as non-existent: `PERSIST` returns `(integer) 0` and does **not** resurrect it.

**Output:** `(integer) 1` if the timeout was removed, `(integer) 0` if the key had no expiry, doesn't exist, or has expired.

#### PURGE

Delete all expired keys from disk (data tables + expiry table). This is the only command that physically removes expired data. Runs in a single `BEGIN IMMEDIATE` transaction, so the scan for expired keys and their deletion see one consistent snapshot and cannot race a concurrent `EXPIRE`/`PERSIST`.

**Output:** `(integer) N` where N is the number of keys purged.

### Utility Commands

#### DBSIZE

Return the total number of distinct keys across all tables. Note: counts raw rows including expired keys that haven't been purged. Use `PURGE` first for an accurate count.

**Output:** `(integer) N`

#### FLUSHALL

Delete all data from all tables, atomically (single transaction).

**Output:** `OK`

## Type Safety

A key may hold only one of the four types at a time. Because each type lives in its own table, this invariant is enforced at the command level rather than by the schema:

- A type-specific mutating command first checks whether the key already exists as a different type. If so it prints `WRONGTYPE Operation against a key holding the wrong kind of value` to stderr, exits with code 1, and leaves the data unchanged. This covers `INCR`/`INCRBY`/`DECR`/`DECRBY`/`APPEND` (string), `LPUSH`/`RPUSH`/`LREM` (list), `SADD`/`SREM` (set), and `HSET`/`HDEL` (hash).
- `SET` and `MSET` are the exception: they overwrite the key regardless of its current type, deleting any list/set/hash rows first.
- An expired key counts as absent for this check, so a write may freely reuse the key as a new type.

The check is performed inside the same transaction as the write so it cannot race a concurrent writer.

## Output Format

All output follows Redis CLI conventions:

| Type | Format | Example |
|------|--------|---------|
| OK status | `OK` | `OK` |
| Integer | `(integer) N` | `(integer) 42` |
| String value | raw text on one line | `hello world` |
| Nil | `(nil)` | `(nil)` |
| List/set items | `N) "value"` | `1) "foo"` |
| Empty collection | `(empty list)`, `(empty set)`, `(empty hash)` | |
| Error | `ERR message` to stderr | `ERR value is not an integer` |

Exit code is 0 on success, 1 on error.

## Concurrency

SQLite in WAL mode supports multiple concurrent readers and a single writer. klyv does not implement its own locking — it relies on SQLite's built-in locking. Multiple processes can safely read from the same database simultaneously. Writes are serialized by SQLite's write lock.

On open, `PRAGMA busy_timeout=5000` is set so a writer waits (up to 5s) for a competing lock instead of failing immediately with `SQLITE_BUSY`. Read-modify-write commands (`INCR`/`INCRBY`/`DECR`/`DECRBY`, `APPEND`, `LPOP`/`RPOP`, `LREM`, `SET`/`MSET`, `LPUSH`/`RPUSH`, `SADD`/`SREM`, `HSET`/`HDEL`, `DEL`, `RENAME`, `EXPIRE`/`PEXPIRE`/`EXPIREAT`, `PERSIST`, `PURGE`, `FLUSHALL`) run inside a `BEGIN IMMEDIATE` transaction so the write lock is taken up front and the operation is atomic against other processes. The type-safety check (below) runs inside this transaction so it cannot race a concurrent writer. For the TTL mutators, the existence/expiry check and the expiry write are serialized together, so a concurrent writer cannot leave an orphan TTL on a key that was deleted between the check and the write.

For CLI usage (one command per invocation), this is sufficient. A long-running server mode (future) would hold a single connection and serialize commands.

## Compatibility Notes

### Differences from Redis

1. **Persistence is default** — every command writes to disk immediately (via SQLite WAL). There is no in-memory-only mode.
2. **Lazy expiry only** — expired keys are hidden from reads but not deleted until `PURGE` is called. Redis uses both lazy expiry and an active background sweep. PEXPIRE rounds up to seconds (no millisecond precision in storage).
3. **No pub/sub** — no server means no subscribers.
4. **No transactions (MULTI/EXEC)** — each CLI invocation is implicitly atomic. (Future: a batch/pipe mode could wrap multiple commands in a SQLite transaction.)
5. **No Lua scripting.**
6. **Pattern matching** uses SQL LIKE semantics, which differs from Redis glob in edge cases (e.g. character classes `[abc]` are not supported). `*`/`?` map to `%`/`_`; literal `%`, `_`, and `\` are escaped so they match themselves.
7. **SET keeps a live TTL** — unlike Redis where SET removes the TTL, klyv preserves a still-valid TTL across a `SET`/`MSET` (use PERSIST to remove it). A *stale* (already-expired) TTL is cleared so the new value is visible.

### Implementation Requirements for Ports

A conforming implementation must:

1. Use the exact SQLite schema above (for database file compatibility across implementations).
2. Set WAL mode and NORMAL synchronous.
3. Require `--db` / `KLYV_DB` for database path (no default).
4. Produce output matching the format table above (for script compatibility).
5. Use the fractional index scheme for lists (not integer indices).
6. Handle the edge cases: INCR on non-existent key (init to 0), MSET with odd args (error), RENAME of non-existent key (error).

A conforming implementation may:

1. Add additional commands beyond this spec.
2. Add additional CLI flags (e.g. `--json` output mode).
3. Add a server/RESP mode.
4. Support TTL/EXPIRE via an `expires_at INTEGER` column on the strings table (lazy or active expiry).
