# klyv

Redis-compatible embedded KV store backed by SQLite. No server, no daemon — just a CLI and a file.

## Usage

```
klyv --db <PATH> [--format <human|raw|json>] <COMMAND> [ARGS...]
```

Either `--db` or the `KLYV_DB` env var is required. The database file is created on first use. `--format` selects the rendering: `human` (default, redis-cli style), `raw` (bare values, nil = empty line), or `json` (single JSON value; `h-get-all` renders as an object, nil as `null`).

## Commands

### Strings
```
set <key> <value> [--nx] [--ex <s> | --px <ms>]
                              Store a value (--nx: only if missing; --ex/--px: TTL, atomic)
get <key>                     Retrieve a value (prints "(nil)" if missing)
get-del <key>                 Retrieve a value and delete the key atomically
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
l-pop <key>                   Pop from head
r-pop <key>                   Pop from tail
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
s-pop <key>                   Remove and return a random member
s-union <k1> [k2 ...]         Union of sets
s-inter <k1> [k2 ...]         Intersection of sets
s-diff <k1> [k2 ...]          Members in first set not in others
```

### Hashes
```
h-set <key> <f1> <v1> [f2 v2 ...]  Set field-value pairs
h-get <key> <field>                 Get field value
h-exists <key> <field>              Test field existence (1/0)
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
p-expire <key> <milliseconds>   Set TTL in ms (rounds up to seconds)
expire-at <key> <timestamp>     Set expiry at Unix timestamp
ttl <key>                       Remaining seconds (-1=no expiry, -2=missing/expired)
persist <key>                   Remove expiry
purge                           Delete all expired keys from disk, report count
```

Expired keys are hidden from reads (lazy expiry) but remain on disk until `purge`.

### Key operations
```
keys [pattern]          List keys (* and ? glob, excludes expired)
exists <key>            Test existence (1/0, respects expiry)
type <key>              string | list | set | hash | none
rename <key> <newkey>   Rename (overwrites target, preserves TTL)
db-size                 Total key count (includes expired; purge first for accuracy)
flush-all               Delete everything including expiry data
```

## Build

```
cargo build --release
```

## Test

```
cargo test
```

## Architecture

Single-file Rust binary. All state in one SQLite database with five tables: `strings`, `list_items`, `set_members`, `hash_fields`, `expiry`. Lists use fractional indexing (REAL column) for O(1) push. Expiry uses lazy filtering (reads check `expiry` table, `purge` does physical deletion). WAL mode for concurrent reads.

See SPEC.md for the full portable specification.

## Future Features

### Interactive mode (REPL) — easy, fits the architecture

A `redis-cli`-style shell entered via `klyv --db <PATH>` with no subcommand. All 43 `cmd_*` functions are reusable verbatim (they take `&Connection` and print redis-cli-formatted output), so the work is mostly plumbing:

- Make `command: Command` → `Option<Command>`; `None` enters the REPL loop.
- Tokenize each line with quote handling (`shlex` crate), then re-dispatch via `Command::try_parse_from(["klyv", ...tokens])` — `try_parse_from` returns a `Result` instead of exiting, so bad input is printed and the loop continues.
- Read loop: `rustyline` for history/line-editing (redis-cli quality) or `std::io::stdin().lines()` for a minimal version.
- **Only real refactor:** the 7 `process::exit(1)` sites must become recoverable errors so one bad command doesn't kill the session (in-process you can't catch `process::exit`). One-shot mode still turns the error into an exit code.

Effort: ~half a day for the polished in-process version; ~1–2 hrs for a crude MVP that shells out to its own binary per line (zero error refactor, but reopens the DB each line). No architectural tension.

### Pub/Sub — hard, fights the architecture

The obstacle isn't the commands — it's that real Redis pub/sub needs a long-running broker to fan out messages to persistent subscriber connections, and **SQLite has no cross-process notification** (no Postgres-style `LISTEN/NOTIFY`; `update_hook` is same-process only). klyv is deliberately process-per-command, so there's no daemon-free path to real-time delivery.

Options:
- **A. Polling a log table** (low–medium effort, stays "just a file"): a `pubsub(id, channel, payload, created_at)` table; `publish` inserts; `subscribe` polls `WHERE id > last_seen AND channel IN (...)` on an interval. But this is a **persistent polled stream** (closer to Redis Streams / a durable queue), *not* fire-and-forget pub/sub — poll-interval latency, needs trimming. Don't call it "pub/sub" in docs.
- **B. Server/daemon mode** (`klyv serve`, high effort): the only path to true Redis semantics; requires a socket/protocol, connection lifecycle, in-memory subscriber registry, fan-out. Contradicts the "no server, no daemon" identity. SPEC already gestures at a future server mode.

There is no low-effort path to *real* pub/sub. Recommendation if pursued: Option A, clearly labeled as durable-stream semantics, not Redis-compatible pub/sub.

**Note:** real Redis pub/sub becomes feasible *only* once the RESP server below exists — a long-running daemon with persistent connections is exactly what fire-and-forget fan-out needs (a shared subscriber registry behind a `Mutex`). So pub/sub is best deferred until then.

### Redis-compatible server (RESP wire protocol) — large, multi-day

A `klyv serve` daemon that speaks the **RESP** wire protocol over **TCP and/or a Unix domain socket** so real Redis clients (`redis-cli`, redis-py, ioredis, go-redis, jedis) can connect. The protocol itself is easy; the difficulty is concentrated in two places.

Transports (mirrors real Redis): `--bind 127.0.0.1:6379` for TCP and `--unixsocket /path/klyv.sock` for a Unix socket; either or both can be enabled at once (`redis-cli -s /path/klyv.sock` connects to the latter). Listening only on a Unix socket is a common, lower-overhead setup for same-host clients and sidesteps TCP port/firewall concerns.

**The crux — split compute from render (the shared foundation).** Today all 43 `cmd_*` functions compute *and* `println!` human `redis-cli` text (`(integer) 5`, `(nil)`, `1) "foo"`), which is the *display* format, not the wire format (RESP integer `:5\r\n`, nil bulk `$-1\r\n`, simple string `+OK\r\n`, error `-WRONGTYPE …\r\n`). Refactor `cmd_*` to return a typed reply instead of printing:

```rust
enum Reply { Simple(String), Error(String), Int(i64), Bulk(Vec<u8>), Nil, Array(Vec<Reply>) }
fn cmd_get(conn, key) -> Result<Reply, Reply>   // not println!
```

Then two renderers consume `Reply`: a **human** renderer reproducing today's exact `redis-cli` text (the integration tests assert on that stdout — must match byte-for-byte) and a **RESP encoder** for the server. This is mechanical but touches all 43 commands, and it subsumes the 7 `process::exit` → recoverable-error cleanup. **It is also the foundation the REPL needs**, so the two features share it.

**Other key points:**
- RESP2 is ~150 lines to hand-roll, or use the `redis-protocol` crate (RESP2/3). A command arrives as an array of bulk strings → `Vec<Vec<u8>>` = `[name, args…]`.
- Dispatch on the uppercased command name (`SET`, `LPUSH`) — clap can't be reused (it parses kebab-case CLI args); add a name-keyed table feeding the same core functions both CLI and server route into.
- Networking: thread-per-connection with blocking `std::net` (no async runtime needed); each connection gets its own rusqlite handle (`Connection` isn't `Sync`). The hardening already shipped (WAL + `busy_timeout` + per-command `BEGIN IMMEDIATE`) gives correct multi-connection serialization for free.
- Unix socket support is almost free because the RESP read/write loop is transport-agnostic — it operates on any `Read + Write`. `std::os::unix::net::UnixListener`/`UnixStream` mirror `TcpListener`/`TcpStream`, so the accept loop can `Box<dyn Read+Write>` (or be generic) and hand either stream to the same handler. Extra work is small and Unix-only (gate behind `#[cfg(unix)]`): unlink any stale socket file on bind, remove it on graceful shutdown, and optionally set permissions (`--unixsocketperm`, like Redis). Windows builds simply omit the flag.
- **The real long tail:** clients probe on connect, so you need stubs for `PING`/`QUIT`/`HELLO`/`SELECT 0`/`COMMAND`/`CLIENT`/`CONFIG GET`/`INFO`, plus exact reply-*type* fidelity (`TYPE`→`+string`, `HGETALL`→flat array in RESP2 vs map in RESP3, missing-key pops→nil bulk). Error strings (`WRONGTYPE`, `ERR …`) are already wire-correct.

**Phased effort:**
- Phase 0 — compute/render split (`Reply` enum + dual renderer, tests stay green): **1–2 days**. *Shared with the REPL.*
- Phase 1 — minimal server (`serve`, thread-per-conn, RESP2, `PING`/`QUIT`/`SELECT 0` + handshake stubs): **1–2 days**.
- Phase 2 — real-client compat (RESP3/`HELLO`, `SCAN`, `MULTI`/`EXEC`, richer `INFO`/`CONFIG`, reply-type audit): **several days → open-ended**.
- Phase 3 — ops/perf (pipelining, optional faster sync mode, graceful shutdown, max-conns, optional `AUTH`/TLS): **a few days**.

Realistic total for "real clients can use the commands klyv implements": **~1–2 weeks**, dominated by Phase 0 and Phase 2. Passing Redis's full ~200-command test suite is open-ended and not a goal.

**Caveats:**
- Performance is SQLite-bound, not Redis-bound (every write hits WAL — durable but slower than in-memory). Position it as a *durable, Redis-wire-compatible store*, not a drop-in perf replacement; a `synchronous=OFF`/memory-mode flag could narrow the gap.
- This server is the prerequisite for real pub/sub (see above).

**Suggested build order:** Phase 0 first (unblocks both the REPL and the server), then the REPL (cheap win), then server Phases 1→2, then pub/sub on top.
