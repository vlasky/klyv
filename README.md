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
klyv --db my.db serve --maxclients 200                 # cap connections (default 10000)
KLYV_REQUIREPASS=s3cret klyv --db my.db serve          # require a password

redis-cli -p 7000 set greeting "hello world"
redis-cli -p 7000 lrange tasks 0 -1
```

```python
import redis
r = redis.Redis(port=7000)
r.set("lock", "1", nx=True, ex=30)
```

- **Protocols:** RESP2 and RESP3 (negotiated with `HELLO`), pipelining, and inline commands.
- **Commands:** everything the CLI offers, under its Redis name, plus a few aliases: `SET` (`NX`, `XX`, `EX`, `PX`, `KEEPTTL`), `SETNX`, `SETEX`, `PSETEX`, `GET`, `GETDEL`, `GETSET`, `GETEX`, `DEL`, `UNLINK`, `INCR`, `DECR`, `INCRBY`, `DECRBY`, `APPEND`, `STRLEN`, `MSET`, `MGET`, `LPUSH`, `RPUSH`, `LPOP`/`RPOP` (with optional count), `LRANGE`, `LLEN`, `LREM`, `LPOS`, `LINDEX`, `LSET`, `LTRIM`, `LINSERT`, `SADD`, `SREM`, `SMEMBERS`, `SISMEMBER`, `SCARD`, `SPOP` (with optional count), `SRANDMEMBER`, `SMOVE`, `SUNION`, `SINTER`, `SDIFF`, `HSET`, `HMSET`, `HGET`, `HMGET`, `HEXISTS`, `HINCRBY`, `HDEL`, `HGETALL`, `HKEYS`, `HVALS`, `HLEN`, `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT` (`NX`, `XX`, `GT`, `LT`), `TTL`, `PTTL`, `PERSIST`, `KEYS`, `SCAN`, `HSCAN`, `SSCAN`, `EXISTS` (multiple keys), `TYPE`, `RENAME`, `COPY`, `DBSIZE`, `FLUSHALL`, `FLUSHDB`. `PURGE` is klyv's own. Replies match Redis's types and error messages; this is checked against a real Redis server in the test suite.
- **Transactions:** `MULTI`, `EXEC`, `DISCARD`, `WATCH` and `UNWATCH`. A transaction runs as one atomic unit that no other connection or process sees half done; as in Redis, a command that fails inside it reports its error in place and the others still apply. `WATCH` notices changes made by any client of the file, including the CLI.
- **Iteration:** `SCAN`, `HSCAN` and `SSCAN` with `MATCH`, `COUNT`, `TYPE` and `NOVALUES`. A key that exists throughout an iteration is returned exactly once.
- **Connection commands** that clients send while connecting are answered: `PING`, `ECHO`, `QUIT`, `SELECT 0`, `HELLO`, `CLIENT SETNAME/GETNAME/ID/SETINFO`, `COMMAND`, `CONFIG GET`, `INFO`.
- **Binary safe:** keys, fields and values may be any bytes.
- **Same file, live:** the CLI can read and write the database while the server is running, and several servers can serve one file. Each command is one SQLite transaction either way.
- **Connections:** one thread and four file descriptors per client. On start the server raises its own open-file limit as far as the system allows; if that is still not enough for `--maxclients` it lowers the client limit to fit and says so. Clients beyond the limit are refused with Redis's error. There is no idle timeout.
- **Shutdown:** stop it with Ctrl-C or `SIGTERM`. Every command is committed before its reply is sent, so there is nothing to flush.

**Security.** By default there is no password and the server listens on loopback only. `--requirepass` (or, better, the `KLYV_REQUIREPASS` environment variable, which does not show up in the process list) makes clients authenticate with `AUTH <password>`, `AUTH default <password>` or `HELLO ... AUTH default <password>`, exactly as with Redis's `requirepass`; until they do, every other command is refused with `NOAUTH`. There is **no TLS**: the password and all data cross the network in the clear, and klyv warns if you bind a non-loopback address. Use a Unix socket with restrictive permissions, a private network or a TLS-terminating proxy, never the open internet.

```sh
redis-cli -p 7000 -a s3cret ping
```

```python
r = redis.Redis(port=7000, password="s3cret")
```

It is a durable store with Redis's interface, not a replacement for Redis's speed: every write is an SQLite commit. As a rough guide, a laptop serves about 100,000 `GET`s and 20,000 `SET`s per second across 20 connections. Not implemented: pub/sub, sorted sets, streams, scripting, blocking commands (`BLPOP`), and `INCRBYFLOAT`.

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
| `get-set <key> <value>` | Store a new value and return the old one |
| `get-ex <key> [--ex <s> \| --px <ms> \| --exat <ts> \| --pxat <ms-ts> \| --persist]` | Retrieve a value and set, change or remove its expiry |
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
| `l-pop <key> [count]` | Pop from head (with a count: up to that many) |
| `r-pop <key> [count]` | Pop from tail (with a count: up to that many) |
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
| `s-pop <key> [count]` | Remove and return a random member (or up to `count`) |
| `s-rand-member <key> [count]` | Random member without removing it (`count` > 0: distinct; < 0: repeats allowed) |
| `s-move <src> <dst> <m>` | Move a member between sets (1/0) |
| `s-scan <key> [cursor] [--match <p>] [--count <n>]` | Iterate members a page at a time |
| `s-union <k> [...]` | Union of sets |
| `s-inter <k> [...]` | Intersection |
| `s-diff <k> [...]` | Difference |

### Hashes

| Command | Description |
|---------|-------------|
| `h-set <key> <f v> [...]` | Set field-value pairs |
| `h-get <key> <field>` | Get field value |
| `h-exists <key> <field>` | Test field existence (1/0) |
| `h-m-get <key> <f> [...]` | Get several fields' values |
| `h-scan <key> [cursor] [--match <p>] [--count <n>] [--no-values]` | Iterate fields a page at a time |
| `h-incr-by <key> <field> <n>` | Increment integer field by N |
| `h-del <key> <f> [...]` | Delete fields |
| `h-get-all <key>` | All fields and values |
| `h-keys <key>` | All field names |
| `h-vals <key>` | All values |
| `h-len <key>` | Number of fields |

### TTL / Expiry

| Command | Description |
|---------|-------------|
| `expire <key> <seconds> [--nx \| --xx] [--gt \| --lt]` | Set TTL in seconds; the conditions also apply to the three commands below |
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
| `exists <key> [key ...]` | Count how many of the keys exist |
| `copy <src> <dst> [--replace]` | Copy a value and its TTL to another key (1/0) |
| `scan [cursor] [--match <p>] [--count <n>] [--type <t>]` | Iterate keys a page at a time (cursor 0 starts; 0 returned means done) |
| `type <key>` | string / list / set / hash / none |
| `rename <key> <newkey>` | Rename key |
| `db-size` | Live key count |
| `flush-all` | Delete everything |

## Architecture

- **SQLite WAL mode** for concurrent readers and safe writes; `busy_timeout` plus `BEGIN IMMEDIATE` transactions keep read-modify-write commands atomic across processes
- **Fractional indexing** for list push without reindexing, and **stored lengths**, so `l-len`, `s-card`, `h-len` and pushes do not count rows; positional list reads walk from the nearer end
- **Lazy expiry** — expired keys are hidden from reads, treated as absent on write, and stay on disk until `purge`
- **Schema versioning** — the file records its schema version; a database written by klyv 0.2.0 is migrated in place on first open
- **A key catalogue plus one payload table per data type**; a key holds one type at a time (mismatched commands fail with `WRONGTYPE`, Redis-style)
- **Reads take no write lock**, including on open, so a read never waits for a writer and a database file you cannot write to can still be read

## Differences from Redis

1. Every write persists to disk immediately (no in-memory mode)
2. Expired keys require explicit `purge` to reclaim space
3. No pub/sub or Lua scripting; transactions (`MULTI`/`EXEC`/`WATCH`) are available through the server
4. Four data types (strings, lists, sets, hashes); no sorted sets or streams
5. The server has password authentication but no TLS, users or ACLs (see [Server mode](#server-mode))

## License

MIT
