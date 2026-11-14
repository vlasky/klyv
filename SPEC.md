# klyv Specification v0.1.1

A Redis-compatible embedded key-value store backed by SQLite. This document specifies the storage format, command semantics, and CLI interface to enable compatible implementations in any language.

> **v0.1.1** clarifies type safety (`WRONGTYPE`), cross-type `SET`/`MSET`/`RENAME` overwrite, expiry-on-write and the `<=` expiry boundary, negative-TTL handling, integer-overflow errors, `KEYS` LIKE escaping, the `idx = 0.0` first-element rule, and `BEGIN IMMEDIATE`/`busy_timeout` atomicity.
>
> **Schema v2** (klyv 0.3) adds the `keyspace` catalogue and millisecond expiry, with an in-place migration from v1 files; see [Schema](#schema) and [Migration](#migration-from-schema-v1).
>
> **Schema v3** adds an element count (`len`) to the catalogue, so `LLEN`, `SCARD`, `HLEN` and the length returned by `LPUSH`/`RPUSH` no longer count rows. v1 and v2 files migrate in place.

## Overview

klyv stores data in a single SQLite database file. It supports four Redis data types (strings, lists, sets, hashes) with Redis-compatible command semantics. It is first a CLI tool: no server, daemon or protocol is needed, just a file. An optional [server mode](#server-mode) exposes the same commands over the Redis wire protocol.

## Storage Format

### Database Configuration

On open, the following PRAGMAs are set:

```sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA busy_timeout=5000;
```

WAL mode enables concurrent readers and improves write performance. `NORMAL` synchronous is safe against corruption from application crashes (though not OS crashes). `busy_timeout` makes a writer wait for a competing lock instead of failing immediately.

**Opening must not take the write lock when there is nothing to do.** An implementation first checks, with plain reads, that `PRAGMA user_version` equals the current schema version, that `PRAGMA journal_mode` is already `wal`, and that `sqlite_master` lists `expiry` as a *view* (the downgrade guard below). If all three hold, no schema statement is run. Only otherwise does it switch the journal mode and enter the `BEGIN IMMEDIATE` setup transaction (re-reading the version under the lock). Without this fast path every read would queue behind any writer and a database file without write permission could not be read.

### Schema

Schema version **3**, recorded in `PRAGMA user_version = 3`. A `keyspace` catalogue holds one row per key — its type, its expiry and, for a list, set or hash, its element count — and is the single source of truth for whether a key exists, what type it is, when it expires and how long it is; the four per-type data tables hold only payload.

```sql
CREATE TABLE IF NOT EXISTS keyspace (
    key TEXT PRIMARY KEY,
    type TEXT NOT NULL,          -- 'string' | 'list' | 'set' | 'hash'
    expires_at INTEGER,          -- Unix time in milliseconds; NULL = no expiry
    len INTEGER                  -- element count of a list/set/hash; NULL for a string
);
CREATE INDEX IF NOT EXISTS idx_keyspace_expires
    ON keyspace(expires_at) WHERE expires_at IS NOT NULL;

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
```

**Invariants** every command must maintain:

1. A key has payload rows in exactly one data table — the one for its catalogue `type`.
2. A key has a catalogue row **iff** it has payload rows. Writes that create a key insert the catalogue row (`INSERT OR IGNORE`); writes that remove the last element of a list/set/hash delete it (and with it any TTL).
3. Expiry lives only in `keyspace.expires_at`.
4. For a list, set or hash, `len` equals the number of its payload rows; for a string it is NULL. Every command that inserts or deletes elements adjusts `len` by the number of rows it *actually* changed, in the same transaction: `SADD` of a member already present adds 0, `HSET`/`HINCRBY` count only fields that did not exist, `LREM`/`LTRIM`/`SREM`/`HDEL` subtract the rows they removed. A command that changes a key's type (`SET`/`MSET` over a list, set or hash) resets `len` to NULL; `RENAME` carries it with the row. `len` reaching 0 is invariant 2's "last element removed": the catalogue row is deleted.

`len` is what makes length queries cheap. `LLEN`, `SCARD` and `HLEN` are one catalogue read (the same read that checks existence, type and expiry), and `LPUSH`/`RPUSH` return the updated `len` rather than counting. An implementation must never answer these from `COUNT(*)` on a healthy file, and must never let `len` drift: the reference test suite compares `len` with the real row count for every key after each step of its scenarios and throughout long random command sequences.

**Storage class (keys, fields, members and values alike):** a byte string that is valid UTF-8 is stored as TEXT, anything else as BLOB, whatever the column's declared type. The rule is deterministic per byte string, which matters because SQLite never treats a BLOB as equal to a TEXT of the same bytes: every lookup and comparison (key lookup, `LREM`, `LPOS`, set membership, hash fields) must bind its argument by the same rule, and readers must accept either class. An implementation that binds byte strings as BLOB unconditionally will fail to find keys written as text. A string with an embedded NUL is valid UTF-8 and is stored as TEXT with its full length.

### Migration from schema v1 and v2

An older database is migrated in place, inside a `BEGIN IMMEDIATE` transaction, the first time it is opened: a v1 database (klyv ≤ 0.2.0: `user_version` 0, no catalogue, a separate `expiry` table in whole seconds) or a v2 database (klyv 0.3.x–0.4.0: a catalogue without `len`). First, if the `keyspace` table exists without a `len` column, add it: `ALTER TABLE keyspace ADD COLUMN len INTEGER`. Then run the steps below; for a v2 file steps 1–4 find nothing to do and step 5 fills in the counts. The same steps double as a **repair** and run whenever a v1-style `expiry` *table* is found in a current file (which only happens if a pre-v2 binary wrote to the file after migration — it recreates the table and writes payload around the catalogue). In order:

```sql
-- 1. Prune catalogue rows whose declared payload is gone (so a key whose type
--    an old binary changed is re-catalogued from the payload that exists).
DELETE FROM keyspace WHERE
    (type = 'string' AND key NOT IN (SELECT key FROM strings)) OR
    (type = 'list'   AND key NOT IN (SELECT key FROM list_items)) OR
    (type = 'set'    AND key NOT IN (SELECT key FROM set_members)) OR
    (type = 'hash'   AND key NOT IN (SELECT key FROM hash_fields));

-- 2. Catalogue any payload that lacks a row. This order reproduces v1's
--    type-lookup precedence for a key that illegally existed in several tables.
INSERT OR IGNORE INTO keyspace (key, type) SELECT key, 'string' FROM strings;
INSERT OR IGNORE INTO keyspace (key, type) SELECT DISTINCT key, 'list' FROM list_items;
INSERT OR IGNORE INTO keyspace (key, type) SELECT DISTINCT key, 'set' FROM set_members;
INSERT OR IGNORE INTO keyspace (key, type) SELECT DISTINCT key, 'hash' FROM hash_fields;

-- 3. Only if an expiry table exists: fold it in as milliseconds, clamped so the
--    multiplication cannot overflow into a REAL, touching only keys it names
--    (other keys keep their v2 expiry). Then drop it. Rows for keys with no
--    payload (orphans) are thereby discarded.
UPDATE keyspace SET expires_at = (
    SELECT CASE
        WHEN e.expires_at >  9223372036854775 THEN  9223372036854775807
        WHEN e.expires_at < -9223372036854775 THEN -9223372036854775808
        ELSE e.expires_at * 1000
    END FROM expiry e WHERE e.key = keyspace.key)
WHERE key IN (SELECT key FROM expiry);
DROP TABLE expiry;

-- 4. Delete payload a key holds in tables other than its catalogue type.
DELETE FROM strings     WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'string');
DELETE FROM list_items  WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'list');
DELETE FROM set_members WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'set');
DELETE FROM hash_fields WHERE key NOT IN (SELECT key FROM keyspace WHERE type = 'hash');

-- 5. Set every key's len from its actual payload rows (NULL for strings).
UPDATE keyspace SET len = CASE type
    WHEN 'list' THEN (SELECT COUNT(*) FROM list_items  WHERE key = keyspace.key)
    WHEN 'set'  THEN (SELECT COUNT(*) FROM set_members WHERE key = keyspace.key)
    WHEN 'hash' THEN (SELECT COUNT(*) FROM hash_fields WHERE key = keyspace.key)
    ELSE NULL
END;

PRAGMA user_version = 3;
```

A database whose `user_version` is **newer** than the implementation supports must be refused, not opened. A healthy current file (version 3, no `expiry` table) runs none of the steps above on open; the per-open cost is independent of database size. Implementations of schema v2 (klyv 0.3.x–0.4.0) refuse a v3 file by this rule, so they cannot write to it without maintaining `len`.

**Downgrade guard.** Every v2 or later file (fresh or migrated) also carries a VIEW named `expiry`, re-created if missing on every open:

```sql
CREATE VIEW IF NOT EXISTS expiry AS
    SELECT key, expires_at / 1000 AS expires_at FROM keyspace WHERE expires_at IS NOT NULL;
```

Its only purpose is to make pre-v2 implementations fail loudly: a v1 binary runs `CREATE INDEX IF NOT EXISTS ... ON expiry(...)` on open, which SQLite rejects on a view ("views may not be indexed"), so the old binary exits with an open error instead of recreating an empty `expiry` table and silently writing around the catalogue. Ports must create it too.

### Design Rationale

- **Catalogue plus per-type payload tables.** The `keyspace` table makes "does this key exist, what type is it, has it expired" one indexed read, and gives `KEYS`, `EXISTS`, `TYPE`, `DBSIZE` and every TTL command a single table to consult. Payload stays in per-type tables for type-specific indexing and constraints. The one-type-per-key invariant is kept by the catalogue (see [Type Safety](#type-safety)).
- **`idx REAL` for lists** uses fractional indexing. LPUSH inserts at `MIN(idx) - 1.0`, RPUSH at `MAX(idx) + 1.0`, and LINSERT at the midpoint between the pivot and its neighbour. This avoids O(n) reindexing on pushes and mid-list inserts. An empty list starts at `idx = 0.0`. (See LINSERT for the renumbering fallback when midpoint precision runs out.)
- **Value storage class.** Columns are declared BLOB; a value that is valid UTF-8 is stored as TEXT and any other byte string as BLOB (deterministic, so SQL equality on values is consistent). Readers accept either. Numeric operations parse the bytes as UTF-8 text then as an integer.
- **Expiry in the catalogue** as an absolute Unix timestamp in **milliseconds** (`NULL` = never), so `PEXPIRE`/`PTTL` have real millisecond precision and a key's TTL cannot outlive the key.
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
| `-f`, `--format <FMT>` | | `human` | Output rendering: `human`, `raw`, or `json` (see Output Format) |

Either `--db` or the `KLYV_DB` environment variable must be provided. There is no default — this avoids silently creating database files in the working directory. The file is created automatically on first use if it doesn't exist.

### Command Naming

Subcommands use **kebab-case** on the CLI (e.g. `s-add`, `l-push`, `h-set`). This document uses Redis-style names (SADD, LPUSH, HSET) for clarity; implementations should accept both.

## Commands

### String Commands

#### SET key value [--nx] [--ex seconds | --px milliseconds | --keep-ttl]

Store a string value at key. Overwrites any existing value **of any type** — if the key currently holds a list, set, or hash, those rows are deleted first so the key becomes a string. Like Redis, a plain `SET` discards any existing TTL (`--keep-ttl` preserves a live one; a stale, already-expired TTL row is always cleared so the new value is immediately visible).

```
DELETE FROM list_items   WHERE key = ?;
DELETE FROM set_members  WHERE key = ?;
DELETE FROM hash_fields  WHERE key = ?;
-- clear expiry only if already expired
INSERT OR REPLACE INTO strings (key, value) VALUES (?, ?);
```

Options (mirroring Redis SET options):

- `--nx` — only set if the key does not already exist (an expired key counts as absent). If the key exists, nothing is written.
- `--ex seconds` / `--px milliseconds` — set a TTL atomically with the value, in the same transaction (the two-command `set` + `expire` sequence is not atomic across processes). The two options are mutually exclusive. A non-positive TTL, or an `--ex` value too large to convert to milliseconds, is rejected with `ERR invalid expire time in 'set' command` before anything is written. (A TTL so large that `now + ttl` overflows saturates to the maximum timestamp; Redis accepts such a value too, but its sum wraps negative and the key expires at once, which is not copied.)
- `--keep-ttl` — retain the key's existing TTL instead of discarding it (Redis `KEEPTTL`). Mutually exclusive with `--ex`/`--px`. `MSET` has no such option and always discards TTLs, as in Redis.

**Output:** `OK`, or `(nil)` if `--nx` was given and the key already exists.

#### GET key

Retrieve the string value at key.

**Output:** The value as a line of text, or `(nil)` if the key does not exist.

#### GETSET key value

Store `value` and return the previous value, or nil if there was none. Like `SET` it discards any TTL. Unlike `SET` it fails with `WRONGTYPE` on a key of another type, since the old value must be returned as a string.

#### GETEX key [EX seconds | PX milliseconds | EXAT unix-seconds | PXAT unix-ms | PERSIST]

Return the value, like `GET`, and change the key's expiry in the same step: set it, or with `PERSIST` remove it. With no option it is `GET`. At most one option; a zero, negative or unrepresentable time is `ERR invalid expire time in 'getex' command`. An absolute time in the past expires the key at once, after returning its value. A missing key gives nil and nothing is set.

#### GETDEL key

Retrieve the string value at key and delete the key (including its expiry row) in the same transaction.

**Error:** `WRONGTYPE` if the key holds a non-string value.

**Output:** The value, or `(nil)` if the key does not exist (nothing is deleted in that case).

#### DEL key [key ...]

Delete one or more keys from ALL tables (strings, lists, sets, hashes). The whole operation runs in a single `BEGIN IMMEDIATE` transaction, so a multi-key delete is atomic and no key is ever left half-deleted (e.g. data rows gone but an expiry row surviving).

**Output:** `(integer) N` where N is the number of *keys* that existed and were deleted, matching Redis. A key that has lazily expired is already logically gone, so it does not count toward N — but its physical rows (including the expiry row) are still reclaimed. Repeating a key in the argument list counts it once.

#### INCR key

Increment the integer value at key by 1. If the key does not exist (or has lazily expired), it is initialized to 0 before incrementing.

**Error:** If the value is not a valid integer, print `ERR value is not an integer or out of range` to stderr and exit with code 1. "Valid" means Redis's canonical form: an optional `-`, no `+`, no leading zeros, no surrounding whitespace, within a signed 64-bit range. `042`, `+5`, `-0` and ` 5` are *not* integers (a general-purpose integer parser is usually too lenient here). The same rule applies to hash fields under `HINCRBY`. If the result would overflow `i64`, print `ERR increment or decrement would overflow` to stderr and exit with code 1. In both cases the stored value is left unchanged.

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

Lists are ordered sequences. Items are stored with a floating-point index (`idx`) that determines order. This allows a push to either end without reindexing: one index seek for the current `MIN(idx)` or `MAX(idx)` (query them separately; a lone `MIN` or `MAX` over the `(key, idx)` index is a seek, while combining them with `COUNT(*)` forces a scan), the inserts, and an update of `len`.

Commands that address an element by position (`LINDEX`, `LSET`, `LRANGE`) reach it by walking the index, so their cost is the distance walked. Since the length is known from the catalogue, walk from whichever end is nearer: for 0-based position `i` of `len` elements, read ascending with offset `i` when `i <= len / 2`, otherwise descending with offset `len - 1 - i`; for a range, start from the end the range is nearer to and reverse the rows if that was the tail. Order by `(idx, rowid)` so that the descending order is the exact mirror of the ascending one. This gives the same cost profile as Redis's lists, where these commands are also linear from the nearer end.

#### LPUSH key value [value ...]

Insert values at the head (lowest index) of the list. The first element of an empty list is inserted at `idx = 0.0`; subsequent head insertions go at `MIN(idx) - 1.0`. Multiple values are inserted left-to-right, meaning the last value will be at the head.

**Output:** `(integer) N` where N is the length of the list after the operation.

#### RPUSH key value [value ...]

Insert values at the tail (highest index). The first element of an empty list is inserted at `idx = 0.0`; subsequent tail insertions go at `MAX(idx) + 1.0`.

**Output:** `(integer) N`

#### LPOP key [count]

Remove and return the element at the head of the list.

**Output:** The value, or `(nil)` if the list is empty/doesn't exist.

With `count`: remove and return up to `count` elements, in the order they are popped, as a list. A missing key gives nil, not an empty list (RESP2 `*-1`, RESP3 `_`); a count of 0 on an existing list gives an empty list. A negative or non-numeric count is `ERR value is out of range, must be positive`. `RPOP` takes the same argument.

#### RPOP key

Remove and return the element at the tail.

**Output:** The value, or `(nil)`.

#### LRANGE key start stop

Return a range of elements. Indices are zero-based. Negative indices count from the end (`-1` = last element).

**Index normalization** (matches Redis):
```
if start < 0: start = max(0, len + start)
if stop < 0:  stop = len + stop        -- NOT clamped to 0
else:         stop = min(stop, len - 1)
```

If `start > stop` after normalization, return empty. A `stop` that is still negative after adding `len` therefore yields an empty range (e.g. `LRANGE l 0 -5` on a 3-element list is empty, not element 0) — clamping `stop` to 0 here would be a Redis deviation.

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

#### LINDEX key index

Get the element at `index` (0-based; negative counts from the end).

**Output:** The value, or `(nil)` if the key or index does not exist.

#### LSET key index value

Overwrite the element at `index` (0-based; negative counts from the end).

**Error:** `ERR no such key` if the list does not exist; `ERR index out of range` if the index is out of bounds.

**Output:** `OK`

#### LTRIM key start stop

Trim the list so only elements at positions `start` through `stop` (inclusive) remain. Index normalization matches LRANGE. If the resulting range is empty, the key is deleted entirely — including its expiry row, so a future key of the same name cannot inherit a stale TTL.

**Output:** `OK` (also for a missing key).

#### LINSERT key <before|after> pivot value

Insert `value` immediately before or after the first occurrence of `pivot` (scanning head to tail). This is where fractional indexing earns its keep: the new element's `idx` is the midpoint between the pivot's and its neighbour's (or pivot ± 1 at a boundary). If repeated inserts into the same gap exhaust `f64` midpoint precision, the implementation must renumber the list's indexes (sequential integers, preserving order) and retry.

**Output:** `(integer) N` with the new list length, `(integer) -1` if the pivot was not found, `(integer) 0` if the key does not exist.

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

#### SPOP key [count]

Remove and return a random member (`ORDER BY RANDOM() LIMIT 1`). With `count`: up to `count` random members as a set (empty for a missing key); a negative or non-numeric count is `ERR value is out of range, must be positive`.

#### SRANDMEMBER key [count]

Return a random member without removing it, or nil if the key is missing. With `count` > 0: up to that many *distinct* members. With `count` < 0: exactly `|count|` picks, with repeats allowed. With a count the reply is a list (an array even in RESP3, since it may contain repeats), empty for a missing key.

#### SMOVE source destination member

Move `member` from one set to another. A missing source is `(integer) 0` without the destination being examined; otherwise both keys must be sets (`WRONGTYPE`). Returns 1 if the member was in the source (it is removed from there even if the destination already held it), 0 if not. Moving the last member deletes the source. With source equal to destination the reply is whether the member is present.

#### SSCAN key cursor [MATCH pattern] [COUNT count]

See [SCAN](#scan-cursor-match-pattern-count-count-type-type).

**Error:** `WRONGTYPE` if the key holds a non-set value.

**Output:** The removed member, or `(nil)` if the set is empty/missing.

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

#### HEXISTS key field

Test whether a field exists in the hash.

**Output:** `(integer) 1` if present, `(integer) 0` if not (or if the key is missing/expired).

#### HINCRBY key field amount

Increment the integer value of a hash field. A missing key or field is initialized to 0 before incrementing.

**Error:** `ERR hash value is not an integer` if the field holds a non-integer; `ERR increment or decrement would overflow` on i64 overflow. The stored value is left unchanged in both cases.

**Output:** `(integer) N` where N is the new value.

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

Return all keys matching a Redis-style glob: `*` matches any sequence, `?` one character, `[abc]` a class, `[^abc]` a negated class, `[a-z]` a range, and `\x` a literal `x` (so a literal backslash is `\\`). Matching is case-sensitive. If no pattern is given, return all keys. Expired keys are excluded.

Matching follows Redis's `stringmatchlen` byte for byte, including on malformed patterns: an unclosed class ends at the end of the pattern (`[a` matches `a`, `[^` matches any one character); `]` directly after `[` closes the class, so `[]a]` matches nothing; a reversed range is swapped (`[c-a]` is `[a-c]`); `\` escapes the next character inside a class as well as outside (`[a\-c]` is the three characters `a`, `-`, `c`); a trailing lone backslash is a literal backslash. A lone `*` matches every key including the empty key, but no other pattern containing a character matches the empty key (`**` does not). Matching is bytewise (`?` consumes one byte, not one character), and range endpoints compare as *signed* bytes, as in Redis on x86 and Apple ARM, which only matters for an endpoint above 0x7f. Recursion is one level per `*` and is capped at 1000 levels as in Redis: a pattern needing more does not match (`*a` repeated 1000 times can match, 1001 times cannot).

Implementation note: port the Redis matcher and filter keys in application code. Do **not** use SQL `LIKE` (it ignores ASCII case), and do not rely on SQLite `GLOB`: it agrees with Redis on well-formed patterns only.

**Output:** Numbered lines or `(empty list)`.

#### EXISTS key [key ...]

Count how many of the given keys exist. A key named twice is counted twice.

**Output:** `(integer) N`.

#### COPY source destination [REPLACE]

Copy the value at `source`, and its expiry, to `destination`. Returns 0 if the source is missing, or if the destination exists and `REPLACE` was not given; 1 otherwise. With `REPLACE` the destination is overwritten whatever its type. `ERR source and destination objects are the same` if they are equal. (Over the wire, `DB 0` is accepted and any other index is `ERR DB index is out of range`.) The copy's `len` is copied with its catalogue row.

#### SCAN cursor [MATCH pattern] [COUNT count] [TYPE type]

Iterate the keyspace a page at a time. The reply is a two-element list: the cursor to pass to the next call, and this page's keys. An iteration starts with cursor 0 and is complete when 0 comes back. `HSCAN key cursor [MATCH] [COUNT] [NOVALUES]` iterates a hash's fields (each followed by its value unless `NOVALUES`) and `SSCAN key cursor [MATCH] [COUNT]` a set's members, with the same shape; both give `[0, []]` for a missing key and `WRONGTYPE` for a key of another type.

**Guarantee** (the one Redis gives): an element that exists from the start of an iteration to its end is returned at least once, and exactly once if it is not replaced during it. Elements created or deleted meanwhile may or may not appear.

**Cursor.** Cursors are opaque to clients and need not match Redis's. The reference implementation uses the SQLite `rowid` of the last row examined: a page is the next `COUNT` rows (default 10) with `rowid` greater than the cursor, in `rowid` order, from `keyspace` for `SCAN` and from the payload table for `HSCAN`/`SSCAN`. The next cursor is the last rowid examined, or 0 if the page came back short. This meets the guarantee because a catalogue row keeps its rowid for as long as the key exists: writes to an existing key must update its row in place (`INSERT ... ON CONFLICT DO UPDATE`, never `INSERT OR REPLACE`, which would assign a new rowid). `COUNT` bounds the rows *examined*; `MATCH`, `TYPE` and expiry filter within the page, so a page may be empty with a non-zero cursor. Any unsigned decimal is a valid cursor (one beyond every rowid yields `[0, []]`); anything else is `ERR invalid cursor`. `COUNT` below 1 is `ERR syntax error`; `NOVALUES` outside `HSCAN` is `ERR NOVALUES option can only be used in HSCAN`.

#### TYPE key

Return the data type of the key.

**Output:** One of `string`, `list`, `set`, `hash`, or `none`. A key exists in only one table (enforced via the type-safety check below), so the result is unambiguous; an expired key reports `none`.

#### RENAME key newkey

Rename a key, carrying its TTL with it. If `newkey` already exists it is overwritten across **all** tables (so no rows of a different type survive at the target). Renaming a key onto itself (`key == newkey`) is a no-op that returns `OK`. Operates inside a single transaction.

**Error:** If `key` does not exist (or has lazily expired): `ERR no such key` to stderr, exit code 1.

**Output:** `OK`

### TTL Commands

Expiry uses lazy filtering: expired keys are not deleted from disk but are invisible to all read commands, and are treated as absent by write commands (which drop the stale rows before proceeding). Use `PURGE` to reclaim disk space.

A write that removes the last element of a list, set, or hash (`LPOP`/`RPOP`, `LREM`, `LTRIM`, `SREM`, `SPOP`, `HDEL`) deletes the key, and must delete its catalogue row (and so its TTL) along with it — otherwise a later `SET` of the same key would silently inherit the stale TTL.

A key is expired once the current time **reaches** its `expires_at` — the check is `expires_at <= now_ms` (used by reads, writes and `PURGE`). "Now" is taken once per command, in milliseconds, so every statement in a command agrees on it.

#### EXPIRE key seconds [NX | XX | GT | LT]

The four expiry setters accept a condition (CLI: `--nx`, `--xx`, `--gt`, `--lt`); if it does not hold, nothing changes and the reply is `(integer) 0`. `NX`: only if the key has no expiry. `XX`: only if it has one. `GT`: only if the new expiry is later than the current one; a key with no expiry counts as never expiring, so `GT` never applies to it. `LT`: only if the new expiry is earlier, which for a key with no expiry is always. `XX` may be combined with `GT` or `LT`. `NX` with any other is `ERR NX and XX, GT or LT options at the same time are not compatible`; `GT` with `LT` is `ERR GT and LT options at the same time are not compatible`; anything else is `ERR Unsupported option <name>`.

Set a key to expire `seconds` from now. The key must exist and not already be expired. A zero or negative `seconds` stores an already-past timestamp, so the key becomes immediately invisible.

**Error (all four expiry setters):** if converting to milliseconds or adding the current time would overflow a signed 64-bit integer, print `ERR invalid expire time in '<name>' command` (`expire`, `pexpire`, `expireat`, `pexpireat`) and change nothing. As in Redis, this is checked before the key is looked up, so it is reported even for a missing key.

**Output:** `(integer) 1` if the timeout was set, `(integer) 0` if the key does not exist.

#### PEXPIRE key milliseconds

Set a key to expire `milliseconds` from now, with millisecond precision. A zero or negative value expires the key immediately.

**Output:** `(integer) 1` or `(integer) 0`.

#### EXPIREAT key timestamp

Set a key to expire at an absolute Unix timestamp (seconds since epoch). A timestamp in the past expires the key immediately.

**Output:** `(integer) 1` or `(integer) 0`.

#### PEXPIREAT key timestamp-ms

As `EXPIREAT`, with the timestamp in milliseconds since the epoch.

**Output:** `(integer) 1` or `(integer) 0`.

#### TTL key

Get the remaining time-to-live in seconds. The millisecond remainder is rounded to the nearest second, as in Redis (`(remaining_ms + 500) / 1000`), so `EXPIRE k 100` followed immediately by `TTL k` reports 100, not 99.

**Output:**
- `(integer) N` — seconds remaining (positive)
- `(integer) -1` — key exists but has no expiry
- `(integer) -2` — key does not exist (or is expired)

#### PTTL key

As `TTL`, in milliseconds (no rounding). Same `-1`/`-2` codes.

#### PERSIST key

Remove the expiry from a key, making it persist indefinitely. A key that has already lazily expired is treated as non-existent: `PERSIST` returns `(integer) 0` and does **not** resurrect it.

**Output:** `(integer) 1` if the timeout was removed, `(integer) 0` if the key had no expiry, doesn't exist, or has expired.

#### PURGE

Delete all expired keys from disk (payload rows + catalogue rows). This is the only command that physically removes expired data. Runs in a single `BEGIN IMMEDIATE` transaction, so the scan for expired keys and their deletion see one consistent snapshot and cannot race a concurrent `EXPIRE`/`PERSIST`.

**Output:** `(integer) N` where N is the number of keys purged.

### Utility Commands

#### DBSIZE

Return the number of **live** keys: a single count over the catalogue excluding rows whose expiry has passed. (Unlike Redis, which may briefly count a logically expired key until its active sweep removes it, this is exact.)

**Output:** `(integer) N`

#### FLUSHALL

Delete all data from all tables, atomically (single transaction).

**Output:** `OK`

## Type Safety

A key may hold only one of the four types at a time. Because each type lives in its own table, this invariant is enforced at the command level rather than by the schema:

- Every type-specific command — reads and writes alike — first checks whether the key exists as a different type. If so it prints `WRONGTYPE Operation against a key holding the wrong kind of value` to stderr, exits with code 1, and (for writes) leaves the data unchanged. This matches Redis: `GET` on a list, `LLEN` on a string, or `SUNION` over a hash all raise `WRONGTYPE` rather than pretending the key is absent. For the set-algebra commands (`SUNION`/`SINTER`/`SDIFF`) every input key is checked. An expired key has no type, so it never trips the check.
- Exceptions, matching Redis:
  - `SET` and `MSET` overwrite the key regardless of its current type, deleting any list/set/hash rows first.
  - `MGET` returns `(nil)` for keys of the wrong type instead of erroring.
  - Key-generic commands (`DEL`, `EXISTS`, `TYPE`, `RENAME`, `KEYS`, the TTL family) operate on any type.
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
| Error | `ERR message` to stderr | `ERR value is not an integer or out of range` |

Exit code is 0 on success, 1 on error.

### Alternative renderings

The table above describes the default `human` format, which is the normative output for this spec's examples. The same typed reply can also be rendered with `--format raw` or `--format json`; errors are unaffected (stderr + exit 1):

| Reply | `raw` | `json` |
|-------|-------|--------|
| OK status | `OK` | `"OK"` |
| Integer | bare number | number |
| String value | bare text | JSON string |
| Nil | empty line | `null` |
| List/set items | one bare value per line | array of strings |
| HGETALL | alternating field/value lines | object `{"field":"value"}` |
| Empty collection | (nothing) | `[]` / `{}` |

`raw` matches `redis-cli --raw` conventions, including its ambiguities (nil vs empty string), and is the only byte-exact format. `json` is the unambiguous machine-readable format for text values; a value that is not valid UTF-8 is rendered lossily (invalid sequences become U+FFFD). Ports must implement `human`; `raw` and `json` are recommended but optional.

## Concurrency

SQLite in WAL mode supports multiple concurrent readers and a single writer. klyv does not implement its own locking — it relies on SQLite's built-in locking. Multiple processes can safely read from the same database simultaneously. Writes are serialized by SQLite's write lock.

On open, `PRAGMA busy_timeout=5000` is set so a writer waits (up to 5s) for a competing lock instead of failing immediately with `SQLITE_BUSY`. Every command runs inside a single transaction. Mutating commands (`SET`/`MSET`, `GETDEL`, `DEL`, `INCR`/`INCRBY`/`DECR`/`DECRBY`, `APPEND`, `LPUSH`/`RPUSH`, `LPOP`/`RPOP`, `LREM`, `LSET`/`LTRIM`/`LINSERT`, `SADD`/`SREM`/`SPOP`, `HSET`/`HINCRBY`/`HDEL`, `RENAME`, `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`, `PERSIST`, `PURGE`, `FLUSHALL`) use `BEGIN IMMEDIATE` so the write lock is taken up front and the whole operation is atomic against other processes; on any error the transaction rolls back, leaving the data unchanged. Read-only commands use a deferred transaction, so a command that issues several queries (expiry check plus data reads, or scans across the per-type tables) sees one consistent snapshot rather than racing a concurrent writer between statements. The type-safety check (below) runs inside the write transaction so it cannot race a concurrent writer. For the TTL mutators, the existence/expiry check and the expiry write are serialized together, so a concurrent writer cannot leave an orphan TTL on a key that was deleted between the check and the write.

For CLI usage (one command per invocation), this is sufficient. Server mode uses the same scheme: one SQLite connection per client connection, one transaction per command, with SQLite serialising writers. No in-process locking is added, which is why the CLI and any number of servers can share a file.

## Server Mode

`klyv --db <PATH> serve [--bind ADDR:PORT]... [--unixsocket PATH [--unixsocketperm MODE]] [--maxclients N]` serves the database over the Redis wire protocol. With no option it listens on `127.0.0.1:6379`; with only `--unixsocket` it opens no TCP listener. It announces each listener on stdout as `listening on tcp://ADDR:PORT` or `listening on unix://PATH` (so a supervisor can discover an ephemeral port requested with `:0`). Server mode is optional for ports.

**Transport.** One thread and one SQLite connection per client. A connection beyond `--maxclients` (default 10000) receives `ERR max number of clients reached` and is closed. A stale Unix socket file is replaced on start, where stale means a connection attempt is refused; a socket that is being served, a path that is not a socket, or a socket whose state cannot be determined is left alone and the server does not start. The socket file is removed on `SIGINT`/`SIGTERM` if the path still refers to the socket this process created. There is no authentication or TLS; binding a non-loopback address prints a warning.

**Protocol.** RESP2 by default; `HELLO 3` switches the connection to RESP3 and `HELLO 2` back. A rejected `HELLO` leaves the protocol unchanged. Commands arrive as arrays of bulk strings or as inline commands: a line of whitespace-separated words, quoted as in Redis's `sdssplitargs` (a quote may open anywhere in a word, and its closing quote ends the word; in double quotes `\xHH` is a byte and `\n \r \t \b \a` are control characters; in single quotes only `\'` is an escape). A NUL byte in an inline command is ordinary data here; Redis never replies to such a command. Pipelined commands are answered in order. Replies may be batched, but must be flushed before the server blocks waiting for input, including when it holds only part of the next command. Protocol violations get Redis's own diagnostics (`ERR Protocol error: invalid multibulk length`, `... invalid bulk length`, `... expected '$', got 'x'`, `... unbalanced quotes in request`, `... too big inline request`) and the connection is closed. Limits match Redis: 512 MB per argument, 1,048,576 arguments per command, 64 KB per inline command or header line, and 1 GB for all the arguments of one command together (`ERR Protocol error: command exceeds the 1 GB limit`; Redis closes such a client without a reply).

**Reply encoding.** Status replies are simple strings, counts are integers, values are bulk strings. The three places RESP3 differs:

| Reply | RESP2 | RESP3 |
|-------|-------|-------|
| nil (`GET` miss, `LPOP` on empty, failed `SET NX`, `MGET` gaps) | `$-1` | `_` |
| set (`SMEMBERS`, `SUNION`, `SINTER`, `SDIFF`) | array | set `~` |
| hash (`HGETALL`), and the `HELLO` / `CONFIG GET` maps | flat array of pairs | map `%` |

`HKEYS`, `HVALS`, `KEYS`, `LRANGE` and `MGET` are arrays in both. Errors are `-` lines whose text matches Redis in full (`WRONGTYPE ...`, `ERR wrong number of arguments for 'get' command`, `ERR unknown command 'foo', with args beginning with: 'a' 'b' `, `ERR syntax error`, `ERR value is not an integer or out of range`).

**Commands.** Names are case-insensitive. Each maps to the CLI command of the same meaning and runs in one transaction: `SET` (`NX`, `XX`, `EX`, `PX`, `KEEPTTL`), `SETNX`, `SETEX`, `PSETEX`, `GET`, `GETDEL`, `GETSET`, `GETEX`, `DEL`, `UNLINK`, `INCR`, `DECR`, `INCRBY`, `DECRBY`, `APPEND`, `STRLEN`, `MSET`, `MGET`, `LPUSH`, `RPUSH`, `LPOP`/`RPOP` (with optional count), `LRANGE`, `LLEN`, `LREM`, `LPOS`, `LINDEX`, `LSET`, `LTRIM`, `LINSERT`, `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SPOP` (with optional count), `SRANDMEMBER`, `SMOVE`, `SUNION`, `SINTER`, `SDIFF`, `HSET`, `HMSET`, `HGET`, `HMGET`, `HEXISTS`, `HINCRBY`, `HDEL`, `HGETALL`, `HKEYS`, `HVALS`, `HLEN`, `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT` (`NX`, `XX`, `GT`, `LT`), `TTL`, `PTTL`, `PERSIST`, `KEYS`, `SCAN`, `HSCAN`, `SSCAN`, `EXISTS` (multiple keys), `TYPE`, `RENAME`, `COPY`, `DBSIZE`, `FLUSHALL`, `FLUSHDB`, and klyv's own `PURGE`. Arity is checked as in Redis; optional arguments that klyv does not implement are arity or syntax errors rather than being ignored. Integer arguments use the strict syntax described under `INCR`. `FLUSHALL`/`FLUSHDB` accept a single `ASYNC` or `SYNC` (which has no effect) and reject anything else with `ERR syntax error` without flushing.

**Transactions.** `MULTI` starts queueing: each following command is checked for existence and arity and answered `+QUEUED`; one that fails that check is answered with its error and marks the transaction, so that `EXEC` replies `EXECABORT Transaction discarded because of previous errors.` and runs nothing. `EXEC` runs the queue inside a single SQLite write transaction, so no other connection or process observes it half done, and replies with a list of each command's result. Redis does not undo a transaction when a command fails at run time, and neither does this: each queued command runs under its own savepoint, a failing one is rolled back alone, its error appears in its place in the list, and the rest still apply. `DISCARD` drops the queue. Errors: `ERR MULTI calls can not be nested`, `ERR EXEC without MULTI`, `ERR DISCARD without MULTI`, `ERR WATCH inside MULTI is not allowed`. An argument to `MULTI`, `EXEC` or `DISCARD` is an arity error and changes nothing.

`WATCH key [key ...]` makes the next `EXEC` conditional: if any watched key has changed since it was watched, `EXEC` runs nothing and replies nil (RESP2 `*-1`, RESP3 `_`). `EXEC`, `DISCARD` and `UNWATCH` clear the watches. The reference implementation records a fingerprint of each watched key's whole state (type, expiry and contents, read in a canonical order) and compares it inside `EXEC`'s write transaction. This differs from Redis in one respect: Redis aborts if the key was *touched*, this aborts if it *differs*, so a key changed and changed back does not abort. No check-and-set logic depends on that case, and comparing state needs nothing stored and sees writes from every process using the file, including the CLI.

**Connection commands.** `PING [message]`, `ECHO`, `QUIT`, `SELECT` (only index 0 exists; others give `ERR DB index is out of range`), `HELLO [2|3 [AUTH ...] [SETNAME name]]`, `CLIENT SETNAME|GETNAME|ID|SETINFO`, `COMMAND` (an empty array; `COMMAND COUNT` an integer), `CONFIG GET` (always empty), `INFO` (a short `# Server` / `# Clients` / `# Keyspace` report; a bulk string in RESP2 and a verbatim `txt` string in RESP3), and `AUTH`, which fails with Redis's "called without any password configured" error. The reported `redis_version` is `6.2.0`.

**Verification.** The reference test suite sends every scenario and long random command sequences to both this server and a real Redis over raw sockets, in RESP2 and RESP3, and requires the parsed replies to be identical including their wire types.

## Compatibility Notes

### Differences from Redis

1. **Persistence is default** — every command writes to disk immediately (via SQLite WAL). There is no in-memory-only mode.
2. **Lazy expiry only** — expired keys are hidden from reads but not deleted until `PURGE` is called. Redis uses both lazy expiry and an active background sweep.
3. **No pub/sub.**
4. **Transactions (MULTI/EXEC) exist only in server mode**; each CLI command is implicitly atomic. `WATCH` compares state rather than tracking touches (see [Server Mode](#server-mode)).
4a. **No `INCRBYFLOAT`/`HINCRBYFLOAT`.** Redis formats the result from a C `long double`, whose precision differs between platforms (80-bit on x86, 64-bit on ARM), so its replies are not reproducible from one Redis build to another, let alone from another implementation.
5. **No Lua scripting.**
6. **One oversized TTL is not copied.** `SET k v PX 9223372036854775807` replies OK in Redis but the key vanishes at once (its `now + ttl` wraps); klyv keeps the key.
7. **Server mode has no authentication or TLS**, and implements only the commands listed under [Server Mode](#server-mode).

### Implementation Requirements for Ports

A conforming implementation must:

1. Use the exact SQLite schema above, keep its invariants, stamp `PRAGMA user_version = 3`, migrate v1 and v2 files as described, and refuse files with a newer version (for database file compatibility across implementations).
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
