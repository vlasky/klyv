# Changelog

## 0.5.0 (2026-11-14)

### Added

- **Transactions in the server: `MULTI`, `EXEC`, `DISCARD`, `WATCH`, `UNWATCH`.** A transaction runs inside one SQLite write transaction, so no other connection or process sees it half done. As in Redis, a command that fails inside it reports its error in place and the others still apply (each runs under its own savepoint). `WATCH` makes `EXEC` conditional on the watched keys being unchanged, and notices changes from any client of the file, including the CLI. Client libraries that wrap pipelines in `MULTI`/`EXEC` by default (redis-py does) now work unmodified.
- **`SCAN`, `HSCAN`, `SSCAN`** (CLI: `scan`, `h-scan`, `s-scan`) with `MATCH`, `COUNT`, `TYPE` and `NOVALUES`. A key that exists throughout an iteration is returned exactly once, whatever is created, deleted or rewritten meanwhile.
- **Counts and options:** `LPOP`/`RPOP`/`SPOP` with a count, `SRANDMEMBER`, and the `NX`/`XX`/`GT`/`LT` conditions on `EXPIRE`, `PEXPIRE`, `EXPIREAT` and `PEXPIREAT` (CLI: `l-pop k 3`, `s-rand-member`, `expire k 60 --gt`).
- **New commands:** `GETSET`, `GETEX`, `HMGET`, `SMOVE`, `COPY` (CLI: `get-set`, `get-ex`, `h-m-get`, `s-move`, `copy`), and `exists` now takes several keys in the CLI as it already did in the server.
- **Password authentication for the server:** `--requirepass` or the `KLYV_REQUIREPASS` environment variable, behaving as Redis's `requirepass`. Clients authenticate with `AUTH` or `HELLO ... AUTH`; until they do, every other command is refused with `NOAUTH`. There is still no TLS.
- **The server raises its open-file limit on start.** Each client needs four file descriptors, so the default limit on macOS (256) allowed only about 56 clients. If the system's hard limit is still too low for `--maxclients`, the client limit is reduced to fit, with a notice.
- `HELLO ... SETNAME` now sets the connection name (it was accepted and ignored).
- `INCRBYFLOAT` is deliberately not implemented: Redis formats its result from a C `long double`, which differs between platforms, so the replies cannot be matched reproducibly.

### Changed

- **Schema v3 (automatic in-place migration): lengths are stored, not counted.** The catalogue now records the element count of every list, set and hash. `l-len`, `s-card` and `h-len` are a single indexed read, and `l-push`/`r-push` no longer count the list to report its new length. Previously each of these scanned all of the key's rows. Files written by klyv 0.3.x and 0.4.0 (and 0.2.0) are migrated the first time they are opened; those older versions refuse a migrated file rather than write to it without keeping the counts.
- **`l-index`, `l-set` and `l-range` walk from the nearer end** of the list, so reading near the tail of a long list is as cheap as reading near the head.

Measured through `klyv serve` on a 20,000-element list with 20 connections:

| Operation | Before | After |
|-----------|--------|-------|
| `LPUSH` while the list grows to 20,000 | about 1,200/s | about 15,000/s |
| `LLEN` | 5,100/s | 99,000/s |
| `LINDEX key -5` | 3,100/s | 100,000/s |
| `LRANGE key -10 -1` | 3,600/s | 86,000/s |

- Tests: the stored length is compared with the real row count for every key after each step of the differential scenarios and throughout the random sequences; rarely drawn commands are sampled more often; a deterministic `SPOP` scenario was added.

## 0.4.0 (2026-11-14)

### Added

- **`klyv serve`: a Redis-compatible server.** Speaks RESP2 and RESP3 (negotiated with `HELLO`) over TCP (`--bind`, default `127.0.0.1:6379`) and/or a Unix socket (`--unixsocket`, `--unixsocketperm`), so `redis-cli`, redis-py and other Redis clients can use a klyv database unchanged. Every CLI command is available under its Redis name, plus `SETNX`, `SETEX`, `PSETEX`, `HMSET`, `UNLINK`, `FLUSHDB`, multi-key `EXISTS` and `SET ... XX`. Pipelining and inline commands are supported, keys and values are binary safe, and the connection commands clients send on connect (`PING`, `HELLO`, `SELECT`, `CLIENT`, `COMMAND`, `CONFIG GET`, `INFO`) are answered. One thread and one SQLite connection per client, one transaction per command, so the CLI can use the same file while the server runs. `--maxclients` caps connections (default 10000). No authentication or TLS: it binds loopback by default and warns otherwise. See the README and SPEC.md "Server Mode".
- **Prebuilt binaries.** Each release now ships static Linux (x86_64, ARM64), macOS (Apple silicon, Intel) and Windows builds with SHA-256 checksums, built by a release workflow when a version tag is pushed.
- The MIT `LICENSE` file, which `Cargo.toml` and the README referred to but which had never been committed.
- Wire-level differential tests: every scenario and long random command sequences are sent to klyv's server and to a real Redis over raw sockets, in both protocol versions, and the replies must be identical including their wire types and full error text.

### Fixed

- **`incr`/`decr`/`incr-by`/`h-incr-by` accepted non-canonical integers.** A stored value such as `042` or `+5` (reachable with `append`) was treated as a number; Redis rejects it. Integers now follow Redis's strict syntax: an optional minus, no plus sign, no leading zeros. Found by the new wire-level tests after about 28,000 random steps.

### Changed

- The integer error now reads `ERR value is not an integer or out of range`, as in Redis (previously `ERR value is not an integer`).
- Internal: every command function takes its keys, fields and values as bytes rather than strings, bound to SQLite as TEXT when valid UTF-8 and BLOB otherwise, making the core binary-safe end to end.

## 0.3.1 (2026-11-14)

### Fixed

- **Opening a database no longer takes the write lock.** Since 0.3.0 every invocation, even a `get`, began with a write transaction to check the schema, so reads queued behind any writer (up to the 5 s busy timeout) and a database file without write permission could not be read at all. A file that is already at the current schema is now opened with read-only checks; the write lock is taken only to create, migrate or repair. (A WAL database in a directory where SQLite cannot create its side files remains unreadable; that is an SQLite limitation.)
- **`keys` patterns now match exactly as in Redis**, including malformed ones. klyv ports Redis's own matcher instead of translating to SQLite `GLOB`, which disagreed on unclosed classes (`[a` matches `a`), empty classes (`[]a]` matches nothing), reversed ranges (`[c-a]` is `[a-c]`) and backslash escapes inside a class (`[a\-c]`). The port keeps Redis's 1000-level recursion cap and its signed-byte range comparison.
- **Expiry overflow is rejected like Redis**: `expire`, `p-expire`, `expire-at` and `set --ex` report `ERR invalid expire time in '<command>' command` when the time cannot be represented, instead of silently saturating. The check happens before the key is looked up, as in Redis.
- A database whose catalogue holds an unrecognised type (corrupted or hand-edited) reports `ERR database error` instead of panicking.

### Changed

- Dependencies: rusqlite 0.31 → 0.42 (bundled SQLite updated with it), rustyline 18 → 19.
- Differential tests: malformed-glob and expiry-overflow scenarios; the randomised layer now draws hostile patterns and boundary expiry values; the comparator no longer drops empty fields and members. One Redis artefact is deliberately not copied: `SET k v PX 9223372036854775807` replies OK in Redis but the key vanishes at once because `now + ttl` wraps; klyv saturates and keeps the key.

## 0.3.0 (2026-10-08)

### Changed

- **Schema v2 (automatic in-place migration).** A `keyspace` catalogue table (key, type, expiry) now records every key; the per-type tables hold payload only. Type lookup, `exists`, `type`, `keys`, `db-size` and every TTL command are single indexed reads instead of probes across four tables, and the one-type-per-key invariant is structural rather than per-command. Databases from klyv 0.2.0 are migrated the first time they are opened; files with a newer schema version are refused, and a v2 file carries a guard (a view named `expiry`) that makes klyv ≤ 0.2.0 fail to open it rather than run split-brain against it. Documented in SPEC.md.
- **Millisecond expiry.** Expiry is stored in Unix milliseconds. `p-expire` and `set --px` no longer round up to whole seconds; `ttl` rounds the remainder to the nearest second like Redis (so `expire k 100` then `ttl k` reports 100, not 99).
- **`db-size` counts live keys only**; expired-but-unpurged keys are no longer included.
- **`set` now discards any existing TTL, like Redis.** Previously a live TTL survived `set`/`m-set`, a deviation that stemmed from a mistaken belief (recorded in the original test) that Redis behaves that way. New `--keep-ttl` flag (Redis `KEEPTTL`) retains it; `m-set` always clears, as in Redis.
- Internal: replies carry bytes rather than strings, and raw output is written byte-for-byte, so binary values from a future RESP client will survive the storage and output layers.

### Added

- Interactive shell (REPL): `klyv --db <PATH>` with no command opens a `redis-cli`-style shell with line editing and in-session history (rustyline). `help` lists commands; `exit`/`quit`/Ctrl-D leaves; errors are shown and the session continues.
- Pipe mode: the same no-command invocation with stdin not a terminal executes commands one per line over a single connection — one process and one database open for the whole batch. Shell-style quoting (single/double quotes, backslash escapes; `#` is ordinary data, never a comment); a failing line reports to stderr and processing continues; exit code 1 if any line failed. `--format` applies to the whole session.
- `p-ttl` (remaining TTL in milliseconds) and `p-expire-at` (absolute expiry in milliseconds).
- Differential test harness (`tests/differential.rs`): runs ~190 identical command steps through klyv and a real Redis server and diffs the replies. Self-skips when redis is not installed; CI runs it in a dedicated ubuntu job.
- Randomised differential layer: seeded command sequences over an awkward alphabet (mixed-case and empty keys, unicode, hyphen-leading values, boundary indexes, overflowing integers), every reply checked against Redis; failures print a replayable seed (`KLYV_DIFF_SEED`).
- Documented the `--` escape for storing values that begin with a hyphen (`klyv append -- k -value`).

### Fixed

- **`keys` was case-insensitive** (SQLite `LIKE` ignores ASCII case): `keys 'user:*'` matched `User:2`. Now uses SQLite `GLOB`, which is case-sensitive and implements Redis glob semantics natively, so `[abc]`, `[^a]` and `[a-z]` classes work and `\` is the escape character exactly as in Redis. Removes two documented deviations.
- `l-push`/`r-push`/`s-add`/`h-set`/`h-del`/`s-rem`/`del`/`m-set`/`m-get` and the set-algebra commands now require their variadic arguments (Redis "wrong number of arguments"); previously `l-push key` with no values was accepted and, under schema v2, would have registered a key with no contents.

## 0.2.0 (2026-07-12)

### Added

- New commands: `get-del`, `l-index`, `l-set`, `l-trim`, `l-insert` (before/after a pivot, using fractional-index midpoints with an automatic renumber when f64 precision runs out), `s-pop`, `h-exists`, `h-incr-by`.
- `set` options: `--nx` (only set if missing) and `--ex <seconds>` / `--px <milliseconds>` (TTL set atomically with the value; non-positive TTLs are rejected).
- `--format raw|json` output modes alongside the default redis-cli-style `human` format. `raw` matches `redis-cli --raw` conventions; `json` is unambiguous for scripting (nil is `null`, `h-get-all` renders as an object).
- `--version` flag.
- GitHub Actions CI: rustfmt, clippy `-D warnings`, and tests on Linux/macOS/Windows.

### Fixed

- **WRONGTYPE on reads:** type-specific read commands (`get`, `strlen`, `l-len`, `l-range`, `l-pos`, `l-index`, `s-members`, `s-is-member`, `s-card`, `s-union`/`s-inter`/`s-diff` inputs, `h-get`, `h-exists`, `h-get-all`, `h-keys`, `h-vals`, `h-len`) and `l-pop`/`r-pop` now raise `WRONGTYPE` on a key of another type, matching Redis, instead of silently reporting nil/empty/0. `m-get` stays lenient like Redis MGET.
- **Range normalization:** `l-range`/`l-trim` no longer clamp a still-negative stop index to 0 — `l-range l 0 -5` on a 3-element list is now empty (Redis behavior) instead of returning the first element.
- **`del` return value:** now counts *keys* deleted like Redis, not rows — deleting a 5-element list reports `(integer) 1`, not `5`. Expired keys count 0 but their physical rows are still reclaimed.
- **Stale TTL inheritance:** a write that empties a list/set/hash (`l-pop`/`r-pop` of the last element, `l-rem`, `l-trim`, `s-rem`, `s-pop`, `h-del`) now deletes the key's expiry row with it, so a later `set` of the same key no longer silently inherits the old TTL.
- **Swallowed database errors:** genuine SQLite failures (busy timeout, I/O errors) previously rendered as `(nil)`/`0` on read commands; they now report `ERR database error: ...` and exit 1. Opening an unreadable database reports a clean error instead of panicking.
- **Atomicity:** `del`, `purge`, and `flush-all` now run in a single transaction (a crash mid-`del` could previously leave data rows deleted but a live expiry row behind). All read commands run in a deferred transaction so multi-statement reads see one consistent snapshot.

### Changed

- Internal: commands now compute a typed `Reply` consumed by pluggable renderers (human/raw/json), with recoverable errors instead of `process::exit` — the groundwork for the planned REPL and RESP server. Human output is byte-for-byte unchanged.
- `l-push`/`r-push` query list bounds once per invocation instead of twice per pushed value; `l-pos` streams instead of loading the whole list.
- Integration test suite expanded to 177 tests.

### Fixed (earlier hardening pass)

- **Type safety:** type-specific commands now reject a key of a different type with `WRONGTYPE` instead of silently creating a key that exists in multiple type tables. `set`/`m-set` overwrite across types; the check runs inside the write transaction. `l-rem` is now covered too (previously returned `(integer) 0` on a non-list key).
- **`rename`:** `rename k k` is now a no-op (previously deleted the key); renaming over an existing key clears the target across all types and preserves the source TTL.
- **Expiry on write:** `incr`/`append`/`l-push`/`r-push`/`l-pop`/`r-pop`/`s-add`/`h-set` and `l-rem`/`s-rem`/`h-del` treat an expired key as absent and drop its stale rows first; `set`/`m-set` clear a stale expiry while preserving a live TTL; `persist` no longer resurrects an expired key. `l-pop`/`r-pop` now decide expiry inside their write transaction so they cannot return a stale item at the expiry boundary.
- **TTL atomicity:** `expire`/`p-expire`/`expire-at`/`persist` now run their existence check and expiry write inside a single `BEGIN IMMEDIATE` transaction, so a concurrent writer can no longer leave an orphan TTL on a deleted key.
- **Set algebra:** `s-union`/`s-inter`/`s-diff` exclude expired input sets; `s-inter` de-duplicates repeated key arguments.
- **Expiry boundary:** uses `<=` so a key expires exactly when its time is reached; `expire`/`p-expire`/`expire-at` accept negative/zero values and expire the key immediately.
- **Integers:** `incr`/`decr` on a non-integer print `ERR value is not an integer` and exit 1 instead of panicking; overflow (including `decr-by i64::MIN`) reports `ERR increment or decrement would overflow` and leaves the value unchanged.
- **`keys`:** glob is translated to SQL `LIKE` with `%`, `_`, and `\` escaped so they match literally.
- **Lists:** an empty list's first element is stored at `idx = 0.0`, matching the storage spec.

### Changed (earlier hardening pass)

- Added `PRAGMA busy_timeout=5000` and wrapped read-modify-write commands (now including the TTL mutators) in `BEGIN IMMEDIATE` transactions for cross-process atomicity.

## 0.1.0 (2026-05-29)

### Added

- String commands: `set`, `get`, `del`, `incr`, `decr`, `incr-by`, `decr-by`, `append`, `strlen`, `m-set`, `m-get`
- List commands: `l-push`, `r-push`, `l-pop`, `r-pop`, `l-range`, `l-len`, `l-rem`, `l-pos`
- Set commands: `s-add`, `s-rem`, `s-members`, `s-is-member`, `s-card`, `s-union`, `s-inter`, `s-diff`
- Hash commands: `h-set`, `h-get`, `h-del`, `h-get-all`, `h-keys`, `h-vals`, `h-len`
- TTL commands: `expire`, `p-expire`, `expire-at`, `ttl`, `persist`, `purge`
- Key commands: `keys`, `exists`, `type`, `rename`, `db-size`, `flush-all`
- SQLite WAL mode with fractional indexing for lists
- Lazy expiry with explicit `purge` for disk reclamation
- `--db` flag and `KLYV_DB` environment variable for database path
