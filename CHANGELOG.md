# Changelog

## Unreleased

### Fixed

- **Type safety:** type-specific commands now reject a key of a different type with `WRONGTYPE` instead of silently creating a key that exists in multiple type tables. `set`/`m-set` overwrite across types; the check runs inside the write transaction.
- **`rename`:** `rename k k` is now a no-op (previously deleted the key); renaming over an existing key clears the target across all types and preserves the source TTL.
- **Expiry on write:** `incr`/`append`/`l-push`/`r-push`/`s-add`/`h-set` and `l-rem`/`s-rem`/`h-del` treat an expired key as absent and drop its stale rows first; `set`/`m-set` clear a stale expiry while preserving a live TTL; `persist` no longer resurrects an expired key.
- **Set algebra:** `s-union`/`s-inter`/`s-diff` exclude expired input sets; `s-inter` de-duplicates repeated key arguments.
- **Expiry boundary:** uses `<=` so a key expires exactly when its time is reached; `expire`/`p-expire`/`expire-at` accept negative/zero values and expire the key immediately.
- **Integers:** `incr`/`decr` on a non-integer print `ERR value is not an integer` and exit 1 instead of panicking; overflow (including `decr-by i64::MIN`) reports `ERR increment or decrement would overflow` and leaves the value unchanged.
- **`keys`:** glob is translated to SQL `LIKE` with `%`, `_`, and `\` escaped so they match literally.
- **Lists:** an empty list's first element is stored at `idx = 0.0`, matching the storage spec.

### Changed

- Added `PRAGMA busy_timeout=5000` and wrapped read-modify-write commands in `BEGIN IMMEDIATE` transactions for cross-process atomicity.
- Expanded the integration test suite to 138 tests covering the above.

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
