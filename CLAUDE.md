# klyv

Redis-compatible embedded KV store backed by SQLite. No server, no daemon — just a CLI and a file.

## Usage

```
klyv --db <PATH> <COMMAND> [ARGS...]
```

Either `--db` or the `KLYV_DB` env var is required. The database file is created on first use.

## Commands

### Strings
```
set <key> <value>             Store a value
get <key>                     Retrieve a value (prints "(nil)" if missing)
del <key> [key ...]           Delete keys (any type)
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
```

### Sets
```
s-add <key> <m> [m ...]       Add members
s-rem <key> <m> [m ...]       Remove members
s-members <key>               List all members
s-is-member <key> <member>    Test membership (1/0)
s-card <key>                  Count members
s-union <k1> [k2 ...]         Union of sets
s-inter <k1> [k2 ...]         Intersection of sets
s-diff <k1> [k2 ...]          Members in first set not in others
```

### Hashes
```
h-set <key> <f1> <v1> [f2 v2 ...]  Set field-value pairs
h-get <key> <field>                 Get field value
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
