# klyv

Redis-compatible embedded KV store backed by SQLite. No server or daemon needed: a CLI and a file. `klyv serve` optionally exposes the same commands over the Redis wire protocol (RESP2/RESP3).

## Usage

```
klyv --db <PATH> [--format <human|raw|json>] <COMMAND> [ARGS...]
```

Either `--db` or the `KLYV_DB` env var is required. The database file is created on first use. `--format` selects the rendering: `human` (default, redis-cli style), `raw` (bare values, nil = empty line), or `json` (single JSON value; `h-get-all` renders as an object, nil as `null`).

Run with no command to enter the interactive shell (rustyline REPL when stdin is a tty) or pipe mode (commands one per line from stdin, one process and one DB open for the batch). Quoting is shell-style via the hand-rolled `split_line` tokenizer (single/double quotes, backslash escapes; `#` is ordinary data, never a comment — the shlex crate was dropped for treating it as one); errors are recoverable per line; `exit`/`quit` ends the session; pipe mode exits 1 if any line failed.

## Commands

### Strings
```
set <key> <value> [--nx] [--ex <s> | --px <ms> | --keep-ttl]
                              Store a value (--nx: only if missing; --ex/--px: TTL, atomic;
                              plain SET discards any TTL like Redis, --keep-ttl retains it)
get <key>                     Retrieve a value (prints "(nil)" if missing)
get-del <key>                 Retrieve a value and delete the key atomically
get-set <key> <value>         Store a new value, return the old one (discards TTL)
get-ex <key> [--ex <s> | --px <ms> | --exat <ts> | --pxat <ms-ts> | --persist]
                              Retrieve a value and set, change or remove its expiry
del <key> [key ...]           Delete keys (any type), returns number of keys deleted
incr <key>                    Increment by 1 (inits to 0 if missing)
decr <key>                    Decrement by 1
incr-by <key> <amount>        Increment by N
decr-by <key> <amount>        Decrement by N
append <key> <value>          Append to string, returns new length
strlen <key>                  String length (0 if missing)
m-set <k1> <v1> [k2 v2 ...]  Set multiple pairs atomically
m-get <k1> [k2 ...]          Get multiple values
```

### Lists
```
l-push <key> <val> [val ...]  Push to head
r-push <key> <val> [val ...]  Push to tail
l-pop <key> [count]           Pop from head (count: up to that many, as a list; nil if missing)
r-pop <key> [count]           Pop from tail
l-range <key> <start> <stop>  Slice (0-based, negatives from end, inclusive)
l-len <key>                   List length
l-rem <key> <count> <value>   Remove occurrences (0=all, +N=head, -N=tail)
l-pos <key> <value>           Index of first occurrence (nil if missing)
l-index <key> <index>         Get element at index (negatives from end)
l-set <key> <index> <value>   Overwrite element at index
l-trim <key> <start> <stop>   Keep only elements in range (inclusive)
l-insert <key> <before|after> <pivot> <value>
                              Insert next to first occurrence of pivot
```

### Sets
```
s-add <key> <m> [m ...]       Add members
s-rem <key> <m> [m ...]       Remove members
s-members <key>               List all members
s-is-member <key> <member>    Test membership (1/0)
s-card <key>                  Count members
s-pop <key> [count]           Remove and return a random member (or up to count)
s-rand-member <key> [count]   Random member, not removed (count > 0 distinct, < 0 with repeats)
s-move <src> <dst> <member>   Move a member between sets (1/0)
s-scan <key> [cursor] [--match <p>] [--count <n>]   Iterate members
s-union <k1> [k2 ...]         Union of sets
s-inter <k1> [k2 ...]         Intersection of sets
s-diff <k1> [k2 ...]          Members in first set not in others
```

### Hashes
```
h-set <key> <f1> <v1> [f2 v2 ...]  Set field-value pairs
h-get <key> <field>                 Get field value
h-exists <key> <field>              Test field existence (1/0)
h-m-get <key> <f1> [f2 ...]         Get several fields' values
h-scan <key> [cursor] [--match <p>] [--count <n>] [--no-values]   Iterate fields
h-incr-by <key> <field> <n>         Increment integer field by N
h-del <key> <f1> [f2 ...]          Delete fields
h-get-all <key>                     All fields and values
h-keys <key>                        All field names
h-vals <key>                        All values
h-len <key>                         Number of fields
```

### TTL / Expiry
```
expire <key> <seconds>          Set TTL in seconds
p-expire <key> <milliseconds>   Set TTL in milliseconds
expire-at <key> <timestamp>     Set expiry at Unix timestamp (seconds)
p-expire-at <key> <ms-ts>       Set expiry at Unix timestamp (milliseconds)
                                (all four accept --nx | --xx | --gt | --lt conditions)
ttl <key>                       Remaining seconds, rounded to nearest (-1=no expiry, -2=missing/expired)
p-ttl <key>                     Remaining milliseconds (-1=no expiry, -2=missing/expired)
persist <key>                   Remove expiry
purge                           Delete all expired keys from disk, report count
```

Expired keys are hidden from reads (lazy expiry) but remain on disk until `purge`.

### Key operations
```
keys [pattern]          List keys (Redis glob: * ? [abc] [^a] [a-z], \ escapes; case-sensitive;
                        a port of Redis's matcher, so malformed patterns behave identically)
exists <key> [key ...]  Count how many of the keys exist (respects expiry)
copy <src> <dst> [--replace]   Copy a value and its TTL (1/0)
scan [cursor] [--match <p>] [--count <n>] [--type <t>]
                        Iterate keys a page at a time (0 starts; 0 returned = done)
type <key>              string | list | set | hash | none
rename <key> <newkey>   Rename (overwrites target, preserves TTL)
db-size                 Live key count (expired keys excluded)
flush-all               Delete everything
```

## Build

```
cargo build --release
```

## Test

```
cargo test
```

`tests/differential.rs` runs the same command sequences through klyv and a real Redis server (started ephemerally on a free port) and diffs the replies — the authoritative check for Redis compatibility. It self-skips when `redis-server`/`redis-cli` are not on PATH (`brew install redis`); CI runs it in a dedicated ubuntu job. When adding or changing a command, add differential steps for it, and mind the comparator notes at the top of that file (error-code-only comparison, empty-array quirk, sorted modes for unordered replies). The same file also has a randomised layer (`differential_random_sequences`): seeded command sequences over an awkward alphabet (mixed-case and empty keys, unicode, hyphen-leading values, boundary indexes, overflowing integers), three seeds of 250 steps by default. A failure prints the seed and recent history; replay with `KLYV_DIFF_SEED=<seed>`, lengthen with `KLYV_DIFF_STEPS=<n>`. New commands with deterministic replies should be added to `random_step` too.

Values beginning with a hyphen need the standard `--` escape on the CLI (`klyv append -- k -value`); keep that in mind when writing tests.

## Architecture

Module layout under `src/`:

- `main.rs` — entry point only: open the DB, then one-shot `dispatch` or hand off to the shell.
- `cli.rs` — clap definitions (`Cli`, `Command`, `OutputFormat`), `is_write`, and the transactional `dispatch`/`run` that routes a `Command` to its implementation.
- `reply.rs` — the typed `Reply`/`CmdError` and the human/raw/json renderers.
- `db.rs` — schema creation and the v1/v2→v3 migration, plus the catalogue helpers every command shares (`lookup`, `key_type`, `check_type`/`ensure_type`, `live_len`, `drop_if_expired`, `grow`, `shrink`, `claim_string`, `remove_key`).
- `shell.rs` — REPL and pipe mode (`split_line`, `run_line`, `repl`, `pipe`).
- `resp.rs` — the wire protocol: `read_command` (multibulk and inline, with Redis's limits and error texts) and `encode` (RESP2/RESP3). Pure functions over `Read`/`Vec<u8>`, unit-tested without sockets.
- `server.rs` — `klyv serve`: listeners (TCP, Unix socket), thread-per-connection loop, the name-keyed command table (`lookup`) that routes Redis commands to the same `cmd_*` functions, and the connection-level commands (`PING`, `HELLO`, `SELECT`, `CLIENT`, ...).
- `commands/{strings,lists,sets,hashes,keys,ttl}.rs` — one module per type family; each `cmd_*` takes a `&Connection` already inside the dispatcher's transaction, and its keys/fields/values as `Arg`.

**Always pass `Arg` to SQL, never a bare `&[u8]`.** `Arg` binds valid UTF-8 as TEXT and anything else as BLOB; rusqlite binds a bare byte slice as BLOB always, and SQLite never considers a BLOB equal to a TEXT of the same bytes, so a bare slice silently misses every key written as text.

All state in one SQLite database, schema v3 (`PRAGMA user_version = 3`): a `keyspace` catalogue (key, type, expires_at in Unix ms, len) that is the single source of truth for existence/type/expiry/length, plus four payload tables `strings`, `list_items`, `set_members`, `hash_fields`. Invariants: payload in exactly the table for the catalogue type; catalogue row iff payload rows; `len` equals the payload row count for a list/set/hash and is NULL for a string.

**Every insert of elements goes through `grow`, every delete through `shrink`, passing the number of rows actually changed** (`conn.execute`'s return value, not the number of arguments: `SADD` of an existing member changes 0 rows). `grow` registers a new key and returns the new length; `shrink` deletes the catalogue row when the length reaches 0. A command that changes a key's type must reset `len` (see the `SET`/`MSET` upserts). Lengths are read with `live_len`, never `COUNT(*)`. The differential tests check `len` against the real row count after every scenario step and throughout the random runs, and a mutation pass (eight deliberate counting mistakes) confirmed that check catches them. Lists use fractional indexing (REAL column) for O(1) push. Expiry uses lazy filtering (`purge` does physical deletion). v1 files (klyv ≤ 0.2.0) and v2 files (0.3.x–0.4.0) migrate in place on first open; those older binaries refuse a v3 file. WAL mode for concurrent reads. `open_db` has a fast path: a file already at the current schema, in WAL mode and carrying the downgrade guard is opened with read-only checks and no write lock (keep it that way: any schema work added to open must stay behind `is_current`).

See SPEC.md for the full portable specification.

## Future Features

### Interactive mode (REPL) and pipe mode — ✅ SHIPPED

`klyv --db <PATH>` with no subcommand enters the loop: a rustyline REPL when stdin is a tty, non-interactive pipe mode (one command per line from stdin) otherwise. One process, one DB open, per-line recoverable errors via `LineInput::try_parse_from` + `dispatch`. See `run_line`/`repl`/`pipe` in src/shell.rs.

### Pub/Sub — hard, fights the architecture

The obstacle isn't the commands — it's that real Redis pub/sub needs a long-running broker to fan out messages to persistent subscriber connections, and **SQLite has no cross-process notification** (no Postgres-style `LISTEN/NOTIFY`; `update_hook` is same-process only). klyv is deliberately process-per-command, so there's no daemon-free path to real-time delivery.

Options:
- **A. Polling a log table** (low–medium effort, stays "just a file"): a `pubsub(id, channel, payload, created_at)` table; `publish` inserts; `subscribe` polls `WHERE id > last_seen AND channel IN (...)` on an interval. But this is a **persistent polled stream** (closer to Redis Streams / a durable queue), *not* fire-and-forget pub/sub — poll-interval latency, needs trimming. Don't call it "pub/sub" in docs.
- **B. Server/daemon mode** (`klyv serve`, high effort): the only path to true Redis semantics; requires a socket/protocol, connection lifecycle, in-memory subscriber registry, fan-out. Contradicts the "no server, no daemon" identity. SPEC already gestures at a future server mode.

There is no low-effort path to *real* pub/sub. Recommendation if pursued: Option A, clearly labeled as durable-stream semantics, not Redis-compatible pub/sub.

**Note:** real Redis pub/sub becomes feasible *only* once the RESP server below exists — a long-running daemon with persistent connections is exactly what fire-and-forget fan-out needs (a shared subscriber registry behind a `Mutex`). So pub/sub is best deferred until then.

### Redis-compatible server (RESP wire protocol) — Phase 1 ✅ SHIPPED (0.4.0), with RESP3

`klyv --db <PATH> serve [--bind ADDR:PORT]... [--unixsocket PATH [--unixsocketperm MODE]]`. Thread per connection with blocking `std::net`, one rusqlite handle per connection, one transaction per command (same as the CLI, so the CLI and the server can share a file live). RESP2 and RESP3 (`HELLO`), pipelining, inline commands, Redis's protocol limits and error texts. See `src/server.rs` and `src/resp.rs`; behaviour is specified in SPEC.md "Server Mode".

Things worth knowing before changing it:

- **Adding a command** means: implement `cmd_*` in `commands/`, add the CLI variant in `cli.rs`, add a row to `lookup` *and* to `DATA_COMMANDS` in `server.rs` (a unit test checks they agree), then add steps to `scenarios()` and `random_step` in `tests/differential.rs`. The server differential tests pick the new steps up automatically.
- **RESP3 was pulled forward from Phase 2** because redis-py 8 sends `HELLO 3` by default and does not fall back when refused. The `Empty` tag on `Reply::Array` doubles as the collection kind: `Set` encodes as `~`, `Hash` as `%`. A new command returning a set or a hash must use the right tag.
- **Integers are strict** (`db::parse_int`, Redis `string2ll`): no `+`, no leading zeros. Rust's `str::parse` is too lenient; never use it for stored values or wire arguments.
- **Reply fidelity is tested at the wire level.** `tests/differential.rs` server mode sends each command to klyv and to a real Redis over raw sockets, in both protocols, and compares parsed replies including type tags and full error text. Run it long before trusting a change: `KLYV_DIFF_STEPS=40000 cargo test --test differential differential_server_random` is about a million steps in under a minute.
- Unix socket paths have a short OS limit (about 104 bytes on macOS); tests pass a relative path and run the server in the temp dir.

**Known limitations / what remains:**

- Phase 2 is largely done: `SCAN`/`HSCAN`/`SSCAN`, `MULTI`/`EXEC`/`DISCARD`/`WATCH`, counted `LPOP`/`RPOP`/`SPOP`, `SRANDMEMBER`, `EXPIRE` `NX|XX|GT|LT`, `GETSET`, `GETEX`, `HMGET`, `SMOVE`, `COPY`. Still missing: sorted sets, blocking pops, `LMOVE`/`RPOPLPUSH`, `SETRANGE`/`GETRANGE`, `HSETNX`, `MSETNX`. `INCRBYFLOAT` is deliberately absent: Redis's output depends on the platform's `long double`, so it cannot be matched reproducibly.
- **SCAN cursors are rowids.** The guarantee (a key present throughout is returned exactly once) rests on a catalogue row keeping its rowid while the key exists. Writes to an existing key must therefore update `keyspace` in place (`ON CONFLICT DO UPDATE`), never `INSERT OR REPLACE`, which assigns a new rowid. `HSCAN`/`SSCAN` select the key's rows ordered by rowid on each call, which is linear in the collection; an index on `(key)` alone would make that a seek if it ever matters.
- **`EXEC` is one SQLite write transaction with a savepoint per queued command**, so it is atomic to every other reader while a failing command is undone alone (Redis semantics). **`WATCH` compares a fingerprint of the key's state** rather than tracking touches; see `fingerprint` in `server.rs` for why that is the right trade here (it sees writes from the CLI and other servers too).
- Phase 3 — operations: `AUTH`/TLS, idle timeouts (`--maxclients` exists), raising the open-file limit at start (each client costs four descriptors: one socket and three SQLite files), cached prepared statements (`prepare_cached`; every command currently re-prepares its SQL), an optional faster sync mode.
- Positional list access (`LINDEX`, `LSET`, `LRANGE`) walks from the nearer end, so the middle of a very long list is still linear, as in Redis. `LREM`, `LPOS` and `LINSERT` scan for a value and are linear too.
- Expiry is still lazy only; a long-running server could purge in the background.
- Real pub/sub is now feasible (a shared subscriber registry behind a `Mutex`), per the note above.
