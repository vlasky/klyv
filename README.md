# klyv

[![Version](https://img.shields.io/badge/version-0.1.0-blue)](CHANGELOG.md)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Redis-compatible embedded key-value store backed by SQLite. No server, no daemon — just a CLI and a file.

## Features

- **Four data types** — strings, lists, sets, hashes with Redis-compatible semantics
- **TTL support** — per-key expiry with lazy filtering and explicit purge
- **Single file** — all state lives in one SQLite database (WAL mode)
- **Zero config** — no server process, no configuration files
- **Portable** — the database file can be shared across any conforming implementation

## Installation

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
klyv --db <PATH> <COMMAND> [ARGS...]
```

Either `--db` or the `KLYV_DB` environment variable is required. The database file is created on first use.

### Strings

| Command | Description |
|---------|-------------|
| `set <key> <value>` | Store a value |
| `get <key>` | Retrieve a value |
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

### Sets

| Command | Description |
|---------|-------------|
| `s-add <key> <m> [...]` | Add members |
| `s-rem <key> <m> [...]` | Remove members |
| `s-members <key>` | List all members |
| `s-is-member <key> <m>` | Test membership (1/0) |
| `s-card <key>` | Count members |
| `s-union <k> [...]` | Union of sets |
| `s-inter <k> [...]` | Intersection |
| `s-diff <k> [...]` | Difference |

### Hashes

| Command | Description |
|---------|-------------|
| `h-set <key> <f v> [...]` | Set field-value pairs |
| `h-get <key> <field>` | Get field value |
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
| `expire-at <key> <ts>` | Set expiry at Unix timestamp |
| `ttl <key>` | Remaining seconds (-1 no expiry, -2 missing) |
| `persist <key>` | Remove expiry |
| `purge` | Delete expired keys from disk |

### Key Operations

| Command | Description |
|---------|-------------|
| `keys [pattern]` | List keys (glob: `*` and `?`) |
| `exists <key>` | Test existence (1/0) |
| `type <key>` | string / list / set / hash / none |
| `rename <key> <newkey>` | Rename key |
| `db-size` | Total key count |
| `flush-all` | Delete everything |

## Architecture

- **SQLite WAL mode** for concurrent readers and safe writes
- **Fractional indexing** for O(1) list push (no reindexing)
- **Lazy expiry** — expired keys are hidden from reads but stay on disk until `purge`
- **Separate tables** per data type for type-specific indexing and constraints

## Differences from Redis

1. Every write persists to disk immediately (no in-memory mode)
2. Expired keys require explicit `purge` to reclaim space
3. No pub/sub, transactions (MULTI/EXEC), or Lua scripting
4. `SET` preserves existing TTL (use `persist` to clear it)
5. Pattern matching uses SQL LIKE (no `[abc]` character classes)

## License

MIT
