//! Command implementations, one module per Redis type family. Every command
//! takes a `&Connection` that is already inside the transaction opened by
//! `cli::dispatch` and returns a typed `Reply` (or a recoverable `CmdError`).

pub(crate) mod hashes;
pub(crate) mod keys;
pub(crate) mod lists;
pub(crate) mod sets;
pub(crate) mod strings;
pub(crate) mod ttl;

/// Binary safety. The CLI can only pass UTF-8, so these drive the command
/// functions directly with the arbitrary bytes a RESP client may send in any
/// key, field or value position.
#[cfg(test)]
mod binary_tests {
    use super::{hashes::*, keys::*, lists::*, sets::*, strings::*, ttl::*};
    use crate::db::{Arg, open_db};
    use crate::reply::Reply;
    use rusqlite::Connection;

    const BIN_KEY: &[u8] = b"k\xff\x00\xfe";
    const BIN_VAL: &[u8] = b"\x80\x81 not utf-8 \x00 tail";
    const BIN_VAL2: &[u8] = b"\xc3\x28";

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_db(&dir.path().join("t.db")).unwrap();
        (dir, conn)
    }

    fn bulk(r: Result<Reply, crate::reply::CmdError>) -> Option<Vec<u8>> {
        match r {
            Ok(Reply::Bulk(v)) => Some(v),
            Ok(Reply::Nil) => None,
            Ok(_) => panic!("expected a bulk or nil reply"),
            Err(e) => panic!("command failed: {}", e.0),
        }
    }

    fn int(r: Result<Reply, crate::reply::CmdError>) -> i64 {
        match r {
            Ok(Reply::Int(n)) => n,
            Ok(_) => panic!("expected an integer reply"),
            Err(e) => panic!("command failed: {}", e.0),
        }
    }

    fn array(r: Result<Reply, crate::reply::CmdError>) -> Vec<Vec<u8>> {
        match r {
            Ok(Reply::Array(mut v, _)) => {
                v.sort();
                v
            }
            Ok(_) => panic!("expected an array reply"),
            Err(e) => panic!("command failed: {}", e.0),
        }
    }

    #[test]
    fn strings_round_trip_arbitrary_bytes() {
        let (_d, c) = db();
        let k = Arg(BIN_KEY);
        assert!(cmd_set(&c, k, Arg(BIN_VAL), SetCond::Always, None, false).is_ok());
        assert_eq!(bulk(cmd_get(&c, k)).as_deref(), Some(BIN_VAL));
        assert_eq!(int(cmd_strlen(&c, k)), BIN_VAL.len() as i64);
        assert_eq!(
            int(cmd_append(&c, k, Arg(BIN_VAL2))),
            (BIN_VAL.len() + BIN_VAL2.len()) as i64
        );
        let mut both = BIN_VAL.to_vec();
        both.extend_from_slice(BIN_VAL2);
        assert_eq!(bulk(cmd_get(&c, k)), Some(both));
        assert_eq!(int(cmd_exists_many(&c, &[k])), 1);
        assert_eq!(int(cmd_del(&c, &[k])), 1);
        assert_eq!(int(cmd_exists_many(&c, &[k])), 0);
    }

    // A value that stops or starts being valid UTF-8 changes storage class
    // (TEXT <-> BLOB); lookups by value must follow it.
    #[test]
    fn append_across_the_utf8_boundary() {
        let (_d, c) = db();
        let k = Arg(b"k");
        cmd_set(&c, k, Arg(b"caf"), SetCond::Always, None, false)
            .ok()
            .unwrap();
        // First byte of a two-byte sequence: now invalid UTF-8.
        cmd_append(&c, k, Arg(b"\xc3")).ok().unwrap();
        assert_eq!(bulk(cmd_get(&c, k)).as_deref(), Some(&b"caf\xc3"[..]));
        // Completing the sequence makes it valid again.
        cmd_append(&c, k, Arg(b"\xa9")).ok().unwrap();
        assert_eq!(bulk(cmd_get(&c, k)).as_deref(), Some("café".as_bytes()));
    }

    #[test]
    fn binary_and_text_keys_are_distinct_and_listed() {
        let (_d, c) = db();
        cmd_set(&c, Arg(BIN_KEY), Arg(b"bin"), SetCond::Always, None, false)
            .ok()
            .unwrap();
        cmd_set(&c, Arg(b"k"), Arg(b"text"), SetCond::Always, None, false)
            .ok()
            .unwrap();
        assert_eq!(bulk(cmd_get(&c, Arg(b"k"))).as_deref(), Some(&b"text"[..]));
        assert_eq!(
            bulk(cmd_get(&c, Arg(BIN_KEY))).as_deref(),
            Some(&b"bin"[..])
        );
        assert_eq!(int(cmd_dbsize(&c)), 2);
        assert_eq!(
            array(cmd_keys(&c, None)),
            vec![b"k".to_vec(), BIN_KEY.to_vec()]
        );
        // Glob matching is bytewise: `?` consumes one byte, not one char.
        assert_eq!(
            array(cmd_keys(&c, Some(Arg(b"k???")))),
            vec![BIN_KEY.to_vec()]
        );
        assert_eq!(
            array(cmd_keys(&c, Some(Arg(b"k\xff*")))),
            vec![BIN_KEY.to_vec()]
        );
    }

    #[test]
    fn rename_ttl_and_purge_with_binary_keys() {
        let (_d, c) = db();
        let (k, k2) = (Arg(BIN_KEY), Arg(b"\xfe\xfd"));
        cmd_rpush(&c, k, &[Arg(BIN_VAL), Arg(b"plain")])
            .ok()
            .unwrap();
        assert!(cmd_rename(&c, k, k2).is_ok());
        assert_eq!(int(cmd_exists_many(&c, &[k])), 0);
        assert_eq!(int(cmd_llen(&c, k2)), 2);
        assert_eq!(int(cmd_expire(&c, k2, 100, ExpireCond::default())), 1);
        assert_eq!(int(cmd_ttl(&c, k2)), 100);
        assert_eq!(int(cmd_persist(&c, k2)), 1);
        assert_eq!(int(cmd_pexpireat(&c, k2, 1, ExpireCond::default())), 1);
        assert_eq!(int(cmd_exists_many(&c, &[k2])), 0);
        assert_eq!(int(cmd_purge(&c)), 1);
        assert_eq!(int(cmd_dbsize(&c)), 0);
    }

    #[test]
    fn lists_match_binary_values() {
        let (_d, c) = db();
        let k = Arg(BIN_KEY);
        let items = [Arg(b"a"), Arg(BIN_VAL), Arg(b"b"), Arg(BIN_VAL)];
        assert_eq!(int(cmd_rpush(&c, k, &items)), 4);
        assert_eq!(int(cmd_lpos(&c, k, Arg(BIN_VAL))), 1);
        assert_eq!(
            int(cmd_linsert(
                &c,
                k,
                crate::cli::InsertWhere::Before,
                Arg(BIN_VAL),
                Arg(BIN_VAL2)
            )),
            5
        );
        assert_eq!(bulk(cmd_lindex(&c, k, 1)).as_deref(), Some(BIN_VAL2));
        assert_eq!(int(cmd_lrem(&c, k, 0, Arg(BIN_VAL))), 2);
        assert!(cmd_lset(&c, k, 0, Arg(BIN_VAL)).is_ok());
        assert_eq!(bulk(cmd_pop(&c, k, "ASC")).as_deref(), Some(BIN_VAL));
        assert_eq!(int(cmd_llen(&c, k)), 2);
    }

    #[test]
    fn sets_with_binary_members() {
        let (_d, c) = db();
        let (s1, s2) = (Arg(BIN_KEY), Arg(b"s2"));
        assert_eq!(
            int(cmd_sadd(&c, s1, &[Arg(BIN_VAL), Arg(b"x"), Arg(BIN_VAL)])),
            2
        );
        assert_eq!(int(cmd_sadd(&c, s2, &[Arg(BIN_VAL), Arg(BIN_VAL2)])), 2);
        assert_eq!(int(cmd_sismember(&c, s1, Arg(BIN_VAL))), 1);
        assert_eq!(int(cmd_sismember(&c, s1, Arg(BIN_VAL2))), 0);
        assert_eq!(array(cmd_sinter(&c, &[s1, s2])), vec![BIN_VAL.to_vec()]);
        assert_eq!(array(cmd_sdiff(&c, &[s1, s2])), vec![b"x".to_vec()]);
        assert_eq!(array(cmd_sunion(&c, &[s1, s2])).len(), 3);
        assert_eq!(int(cmd_srem(&c, s1, &[Arg(BIN_VAL)])), 1);
        assert_eq!(int(cmd_scard(&c, s1)), 1);
    }

    #[test]
    fn hashes_with_binary_fields_and_values() {
        let (_d, c) = db();
        let k = Arg(BIN_KEY);
        let (f, f2) = (Arg(b"\xff\xfe"), Arg(b"n"));
        assert_eq!(int(cmd_hset(&c, k, &[f, Arg(BIN_VAL), f2, Arg(b"5")])), 2);
        assert_eq!(int(cmd_hset(&c, k, &[f, Arg(BIN_VAL2)])), 0);
        assert_eq!(bulk(cmd_hget(&c, k, f)).as_deref(), Some(BIN_VAL2));
        assert_eq!(int(cmd_hexists(&c, k, f)), 1);
        assert_eq!(int(cmd_hincrby(&c, k, f2, 2)), 7);
        assert!(cmd_hincrby(&c, k, f, 1).is_err());
        assert_eq!(
            array(cmd_hkeys(&c, k)),
            vec![b"n".to_vec(), b"\xff\xfe".to_vec()]
        );
        assert_eq!(int(cmd_hdel(&c, k, &[f])), 1);
        assert_eq!(int(cmd_hlen(&c, k)), 1);
    }

    // NUL is valid UTF-8, so these bind as TEXT with an embedded NUL; they
    // must not be truncated at it or confused with their prefix.
    #[test]
    fn embedded_nul_is_preserved() {
        let (_d, c) = db();
        let (k, prefix) = (Arg(b"a\0b"), Arg(b"a"));
        cmd_set(&c, k, Arg(b"v\0w"), SetCond::Always, None, false)
            .ok()
            .unwrap();
        assert_eq!(bulk(cmd_get(&c, k)).as_deref(), Some(&b"v\0w"[..]));
        assert_eq!(bulk(cmd_get(&c, prefix)), None);
        assert_eq!(int(cmd_sadd(&c, Arg(b"s"), &[Arg(b"m\0x"), Arg(b"m")])), 2);
        assert_eq!(int(cmd_sismember(&c, Arg(b"s"), Arg(b"m\0x"))), 1);
        assert_eq!(int(cmd_sismember(&c, Arg(b"s"), Arg(b"m\0"))), 0);
        assert_eq!(
            array(cmd_keys(&c, Some(Arg(b"a?b")))),
            vec![b"a\0b".to_vec()]
        );
    }

    // UTF-8 arguments must keep binding as TEXT, exactly as files written by
    // earlier versions hold them; a BLOB never compares equal to a TEXT.
    #[test]
    fn utf8_arguments_are_stored_as_text() {
        let (_d, c) = db();
        cmd_set(
            &c,
            Arg(b"s"),
            Arg("v\u{e9}".as_bytes()),
            SetCond::Always,
            None,
            false,
        )
        .ok()
        .unwrap();
        cmd_rpush(&c, Arg(b"l"), &[Arg(b"a")]).ok().unwrap();
        cmd_sadd(&c, Arg(b"st"), &[Arg(b"m")]).ok().unwrap();
        cmd_hset(&c, Arg(b"h"), &[Arg(b"f"), Arg(b"v")])
            .ok()
            .unwrap();
        cmd_incrby(&c, Arg(b"n"), 3).ok().unwrap();
        for (table, columns) in [
            ("keyspace", "key"),
            ("strings", "key, value"),
            ("list_items", "key, value"),
            ("set_members", "key, member"),
            ("hash_fields", "key, field, value"),
        ] {
            for column in columns.split(", ") {
                let blobs: i64 = c
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE typeof({column}) != 'text'"),
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(blobs, 0, "{table}.{column} holds a non-TEXT value");
            }
        }
    }
}
