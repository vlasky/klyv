# klyv

[![Version](https://img.shields.io/badge/version-0.4.0-blue)](CHANGELOG.md)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Redis-compatible embedded key-value store backed by SQLite. No server or daemon is needed: it is a CLI and a file. When you do want one, `klyv serve` speaks the Redis wire protocol, so ordinary Redis clients can use the same file.

## Features

- **Four data types** — strings, lists, sets, hashes with Redis-compatible semantics
- **TTL support** — per-key expiry with lazy filtering and explicit purge
- **Single file** — all state lives in one SQLite database (WAL mode)
- **Zero config** — no server process needed, no configuration files
- **Optional Redis-compatible server** — `klyv serve` speaks RESP2 and RESP3 over TCP or a Unix socket; `redis-cli`, redis-py and other clients connect unchanged
- **Portable** — the database file can be shared across any conforming implementation

## Installation

Prebuilt binaries for Linux (x86_64 and ARM64, static), macOS (Apple silicon and Intel) and Windows are attached to each [GitHub release](https://github.com/vlasky/klyv/releases), with SHA-256 checksums. Unpack the archive and put `klyv` on your `PATH`.

With a Rust toolchain:

```sh
cargo install --path .
```

Or build from source:

```sh
cargo build --release
# binary at target/release/klyv
```

## Quick Start

```sh
export KLYV_DB=my.db

klyv set greeting "hello world"
klyv get greeting
# hello world

klyv l-push tasks "write docs" "ship feature"
klyv l-range tasks 0 -1
# 1) "ship feature"
# 2) "write docs"

klyv h-set user:1 name "Alice" role "admin"
klyv h-get-all user:1
# 1) "name"
# 2) "Alice"
# 3) "role"
# 4) "admin"
```

## Usage

```
klyv --db <PATH> [--format <human|raw|json>] <COMMAND> [ARGS...]
```

Either `--db` or the `KLYV_DB` environment variable is required. The database file is created on first use.

To store values that begin with a hyphen, use the standard `--` separator so they aren't parsed as flags: `klyv append -- k -leading-hyphen`. (Numeric arguments like `incr-by k -5` or `l-range k 0 -1` work without it.)

### Interactive shell and pipe mode

Run `klyv` with no command to enter a `redis-cli`-style shell with line editing and history:

```
$ klyv --db my.db
klyv 0.4.0 — 'help' lists commands, 'exit' or Ctrl-D quits
klyv> set greeting "hello world"
OK
klyv> get greeting
hello world
klyv> exit
```

When stdin is not a terminal, the same invocation reads commands one per line — one process and one database open for the whole batch, instead of a process per command:

```sh
printf 'set a 1\nincr counter\nl-push tasks "write docs"\n' | klyv --db my.db
```

Values with spaces use shell-style quoting in both modes. A failing line prints its error to stderr and the session continues; pipe mode exits 1 if any line failed. `--format` applies to every reply in the session.

### Server mode

`klyv serve` runs a Redis-compatible server on the database, so existing Redis clients and tools can use it:

```sh
klyv --db my.db serve                                  # 127.0.0.1:6379
klyv --db my.db serve --bind 127.0.0.1:7000            # another port (repeatable)
klyv --db my.db serve --unixsocket /run/klyv.sock --unixsocketperm 660   # Unix socket only

redis-cli -p 7000 set greeting "hello world"
redis-cli -p 7000 lrange tasks 0 -1
```

```python
import redis
r = redis.Redis(port=7000)
r.set("lock", "1", nx=True, ex=30)
```

- **Protocols:** RESP2 and RESP3 (negotiated with `HELLO`), pipelining, and inline commands.
- **Commands:** everything the CLI offers, under its Redis name, plus a few aliases: `SET` (`NX`, `XX`, `EX`, `PX`, `KEEPTTL`), `SETNX`, `SETEX`, `PSETEX`, `GET`, `GETDEL`, `DEL`, `UNLINK`, `INCR`, `DECR`, `INCRBY`, `DECRBY`, `APPEND`, `STRLEN`, `MSET`, `MGET`, `LPUSH`, `RPUSH`, `LPOP`, `RPOP`, `LRANGE`, `LLEN`, `LREM`, `LPOS`, `LINDEX`, `LSET`, `LTRIM`, `LINSERT`, `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SPOP`, `SUNION`, `SINTER`, `SDIFF`, `HSET`, `HMSET`, `HGET`, `HEXISTS`, `HINCRBY`, `HDEL`, `HGETALL`, `HKEYS`, `HVALS`, `HLEN`, `EXPIRE`, `PEXPIRE`, `EXPIREAT`, `PEXPIREAT`, `TTL`, `PTTL`, `PERSIST`, `KEYS`, `EXISTS` (multiple keys), `TYPE`, `RENAME`, `DBSIZE`, `FLUSHALL`, `FLUSHDB`. `PURGE` is klyv's own. Replies match Redis's types and error messages; this is checked against a real Redis server in the test suite.
- **Connection commands** that clients send while connecting are answered: `PING`, `ECHO`, `QUIT`, `SELECT 0`, `HELLO`, `CLIENT SETNAME/GETNAME/ID/SETINFO`, `COMMAND`, `CONFIG GET`, `INFO`.
- **Binary safe:** keys, fields and values may be any bytes.
- **Same file, live:** the CLI can read and write the database while the server is running, and several servers can serve one file. Each command is one SQLite transaction either way.
- **Shutdown:** stop it with Ctrl-C or `SIGTERM`. Every command is committed before its reply is sent, so there is nothing to flush.

There is **no authentication and no TLS**. The default bind address is loopback only; klyv prints a warning if you bind anything else. Put it behind a Unix socket with restrictive permissions, or a trusted network, never the open internet.

It is a durable store with Redis's interface, not a replacement for Redis's speed: every write is an SQLite commit. As a rough guide, a laptop serves about 100,000 `GET`s and 20,000 `SET`s per second across 20 connections. Not implemented: `SCAN`, `MULTI`/`EXEC`, pub/sub, sorted sets, streams, scripting, blocking commands, and count/option arguments beyond those listed.

### Output formats

The default `human` format mirrors redis-cli (`(integer) 5`, `(nil)`, numbered quoted arrays). For scripting, pass `--format raw` or `--format json`:

```sh
klyv --format raw l-range tasks 0 -1
# ship feature
# write docs

klyv --format json h-get-all user:1
# {"name":"Alice","role":"admin"}

klyv --format json get missing
# null
```

`raw` prints bare values one per line (nil is an empty line, like `redis-cli --raw`). `json` prints a single JSON value: strings, integers as numbers, nil as `null`, list/set results as arrays, `h-get-all` as an object. JSON is the unambiguous choice for scripts — in the other formats a stored value of `(nil)` or an empty string is indistinguishable from a missing key. (`raw` is the only byte-exact format; `json` and `human` render invalid UTF-8 lossily.) Errors go to stderr with exit code 1 in every format.

### Strings

| Command | Description |
|---------|-------------|
| `set <key> <value> [--nx] [--ex <s> \| --px <ms> \| --keep-ttl]` | Store a value (only-if-missing, with TTL, or keeping the existing TTL) |
| `get <key>` | Retrieve a value |
| `get-del <key>` | Retrieve a value and delete the key atomically |
| `del <key> [key ...]` | Delete keys (any type) |
| `incr <key>` | Increment by 1 |
| `decr <key>` | Decrement by 1 |
| `incr-by <key> <n>` | Increment by N |
| `decr-by <key> <n>` | Decrement by N |
| `append <key> <value>` | Append to string, returns new length |
| `strlen <key>` | String length |
| `m-set <k v> [...]` | Set multiple pairs atomically |
| `m-get <k> [...]` | Get multiple values |

### Lists

| Command | Description |
|---------|-------------|
| `l-push <key> <val> [...]` | Push to head |
| `r-push <key> <val> [...]` | Push to tail |
| `l-pop <key>` | Pop from head |
| `r-pop <key>` | Pop from tail |
| `l-range <key> <start> <stop>` | Slice (0-based, negatives supported) |
| `l-len <key>` | List length |
| `l-rem <key> <count> <value>` | Remove occurrences |
| `l-pos <key> <value>` | Index of first occurrence |
| `l-index <key> <index>` | Get element at index |
| `l-set <key> <index> <value>` | Overwrite element at index |
| `l-trim <key> <start> <stop>` | Keep only the given range |
| `l-insert <key> <before\|after> <pivot> <value>` | Insert next to first occurrence of pivot |

### Sets

| Command | Description |
|---------|-------------|
| `s-add <key> <m> [...]` | Add members |
| `s-rem <key> <m> [...]` | Remove members |
| `s-members <key>` | List all members |
| `s-is-member <key> <m>` | Test membership (1/0) |
| `s-card <key>` | Count members |
| `s-pop <key>` | Remove and return a random member |
| `s-union <k> [...]` | Union of sets |
| `s-inter <k> [...]` | Intersection |
| `s-diff <k> [...]` | Difference |

### Hashes

| Command | Description |
|---------|-------------|
| `h-set <key> <f v> [...]` | Set field-value pairs |
| `h-get <key> <field>` | Get field value |
| `h-exists <key> <field>` | Test field existence (1/0) |
| `h-incr-by <key> <field> <n>` | Increment integer field by N |
| `h-del <key> <f> [...]` | Delete fields |
| `h-get-all <key>` | All fields and values |
| `h-keys <key>` | All field names |
| `h-vals <key>` | All values |
| `h-len <key>` | Number of fields |

### TTL / Expiry

| Command | Description |
|---------|-------------|
| `expire <key> <seconds>` | Set TTL in seconds |
| `p-expire <key> <ms>` | Set TTL in milliseconds |
| `expire-at <key> <ts>` | Set expiry at Unix timestamp (seconds) |
| `p-expire-at <key> <ms-ts>` | Set expiry at Unix timestamp (milliseconds) |
| `ttl <key>` | Remaining seconds (-1 no expiry, -2 missing) |
| `p-ttl <key>` | Remaining milliseconds (-1 no expiry, -2 missing) |
| `persist <key>` | Remove expiry |
| `purge` | Delete expired keys from disk |

### Key Operations

| Command | Description |
|---------|-------------|
| `keys [pattern]` | List keys (Redis glob: `*`, `?`, `[abc]`, `[^a]`, `[a-z]`, `\` escapes; matched exactly as Redis does) |
| `exists <key>` | Test existence (1/0) |
| `type <key>` | string / list / set / hash / none |
| `rename <key> <newkey>` | Rename key |
| `db-size` | Live key count |
| `flush-all` | Delete everything |

## Architecture

- **SQLite WAL mode** for concurrent readers and safe writes; `busy_timeout` plus `BEGIN IMMEDIATE` transactions keep read-modify-write commands atomic across processes
- **Fractional indexing** for O(1) list push (no reindexing)
- **Lazy expiry** — expired keys are hidden from reads, treated as absent on write, and stay on disk until `purge`
- **Schema versioning** — the file records its schema version; a database written by klyv 0.2.0 is migrated in place on first open
- **A key catalogue plus one payload table per data type**; a key holds one type at a time (mismatched commands fail with `WRONGTYPE`, Redis-style)
- **Reads take no write lock**, including on open, so a read never waits for a writer and a database file you cannot write to can still be read

## Differences from Redis

1. Every write persists to disk immediately (no in-memory mode)
2. Expired keys require explicit `purge` to reclaim space
3. No pub/sub, transactions (MULTI/EXEC), or Lua scripting
4. Four data types (strings, lists, sets, hashes); no sorted sets or streams
5. The server has no authentication or TLS (see [Server mode](#server-mode))

## License

MIT
