# Changelog

## Unreleased

### Added

- New commands: `get-del`, `l-index`, `l-set`, `l-trim`, `l-insert` (before/after a pivot, using fractional-index midpoints with an automatic renumber when f64 precision runs out), `s-pop`, `h-exists`, `h-incr-by`.
- `set` options: `--nx` (only set if missing) and `--ex <seconds>` / `--px <milliseconds>` (TTL set atomically with the value; non-positive TTLs are rejected).
- `--format raw|json` output modes alongside the default redis-cli-style `human` format. `raw` matches `redis-cli --raw` conventions; `json` is unambiguous for scripting (nil is `null`, `h-get-all` renders as an object).
- `--version` flag.
- GitHub Actions CI: rustfmt, clippy `-D warnings`, and tests on Linux/macOS/Windows.

### Fixed

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
