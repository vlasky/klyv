# Changelog

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
