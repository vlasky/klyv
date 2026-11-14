//! Integration tests for `klyv serve`: a real server process, driven over a
//! socket by a minimal RESP2 client written here (so the tests exercise the
//! wire format itself, not another client's interpretation of it).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const KLYV: &str = env!("CARGO_BIN_EXE_klyv");

struct Server {
    child: Child,
    addr: String,
    db: String,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts a server on an ephemeral port (plus any extra arguments) and waits
/// for its "listening on" announcements, one per expected listener.
fn start_with(dir: tempfile::TempDir, extra: &[&str], listeners: usize) -> Server {
    let db = dir.path().join("t.db").to_str().unwrap().to_string();
    let mut child = Command::new(KLYV)
        .args(["--db", &db, "serve", "--bind", "127.0.0.1:0"])
        .args(extra)
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to start klyv serve");
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut addr = String::new();
    for _ in 0..listeners {
        let line = lines
            .next()
            .expect("server exited before listening")
            .unwrap();
        if let Some(a) = line.strip_prefix("listening on tcp://") {
            addr = a.to_string();
        }
    }
    Server {
        child,
        addr,
        db,
        _dir: dir,
    }
}

fn start() -> Server {
    start_with(tempfile::tempdir().unwrap(), &[], 1)
}

#[derive(Debug, PartialEq, Clone)]
enum Value {
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(Vec<u8>),
    Nil,
    Array(Vec<Value>),
    /// RESP2 null array (`*-1`).
    NullArray,
    /// RESP3 null (`_`).
    Null,
    /// RESP3 set (`~`).
    Set(Vec<Value>),
    /// RESP3 map (`%`).
    Map(Vec<(Value, Value)>),
    /// RESP3 verbatim string (`=`), including its `txt:` prefix.
    Verbatim(String),
}

fn bulk(s: &str) -> Value {
    Value::Bulk(s.as_bytes().to_vec())
}

fn ok() -> Value {
    Value::Simple("OK".into())
}

fn strs(items: &[&str]) -> Value {
    Value::Array(items.iter().map(|s| bulk(s)).collect())
}

struct Client<S: Read + Write> {
    reader: BufReader<S>,
    writer: S,
}

impl Client<TcpStream> {
    fn connect(server: &Server) -> Self {
        let stream = TcpStream::connect(&server.addr).expect("connect failed");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        Client {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
        }
    }
}

fn encode(args: &[&[u8]]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        out.extend_from_slice(a);
        out.extend_from_slice(b"\r\n");
    }
    out
}

impl<S: Read + Write> Client<S> {
    fn send_raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read failed");
        assert!(
            line.ends_with("\r\n"),
            "reply line not CRLF-terminated: {line:?}"
        );
        line.truncate(line.len() - 2);
        line
    }

    fn read(&mut self) -> Value {
        let line = self.line();
        let (kind, rest) = line.split_at(1);
        match kind {
            "+" => Value::Simple(rest.to_string()),
            "-" => Value::Error(rest.to_string()),
            ":" => Value::Int(rest.parse().unwrap()),
            "$" => {
                let len: i64 = rest.parse().unwrap();
                if len < 0 {
                    return Value::Nil;
                }
                let mut buf = vec![0u8; len as usize + 2];
                self.reader.read_exact(&mut buf).unwrap();
                assert_eq!(&buf[len as usize..], b"\r\n");
                buf.truncate(len as usize);
                Value::Bulk(buf)
            }
            "*" => {
                let n: i64 = rest.parse().unwrap();
                if n < 0 {
                    return Value::NullArray;
                }
                Value::Array((0..n).map(|_| self.read()).collect())
            }
            "_" => Value::Null,
            "=" => {
                let len: usize = rest.parse().unwrap();
                let mut buf = vec![0u8; len + 2];
                self.reader.read_exact(&mut buf).unwrap();
                buf.truncate(len);
                Value::Verbatim(String::from_utf8(buf).unwrap())
            }
            "~" => {
                let n: i64 = rest.parse().unwrap();
                Value::Set((0..n).map(|_| self.read()).collect())
            }
            "%" => {
                let n: i64 = rest.parse().unwrap();
                Value::Map(
                    (0..n)
                        .map(|_| {
                            let k = self.read();
                            (k, self.read())
                        })
                        .collect(),
                )
            }
            other => panic!("unexpected reply type {other:?} in {line:?}"),
        }
    }

    /// True if the server has closed the connection.
    fn closed(&mut self) -> bool {
        let mut byte = [0u8; 1];
        matches!(self.reader.read(&mut byte), Ok(0))
    }

    fn bin(&mut self, args: &[&[u8]]) -> Value {
        self.send_raw(&encode(args));
        self.read()
    }

    fn cmd(&mut self, args: &[&str]) -> Value {
        let args: Vec<&[u8]> = args.iter().map(|s| s.as_bytes()).collect();
        self.bin(&args)
    }

    fn error(&mut self, args: &[&str]) -> String {
        match self.cmd(args) {
            Value::Error(msg) => msg,
            other => panic!("{args:?}: expected an error, got {other:?}"),
        }
    }
}

fn klyv_cli(db: &str, args: &[&str]) -> String {
    let out = Command::new(KLYV)
        .args(["--db", db, "--format", "raw"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn basic_commands_and_reply_types() {
    let server = start();
    let mut c = Client::connect(&server);
    assert_eq!(c.cmd(&["PING"]), Value::Simple("PONG".into()));
    assert_eq!(c.cmd(&["PING", "hi"]), bulk("hi"));
    assert_eq!(c.cmd(&["ECHO", "hi"]), bulk("hi"));
    assert_eq!(c.cmd(&["SET", "k", "v"]), ok());
    assert_eq!(
        c.cmd(&["get", "k"]),
        bulk("v"),
        "names are case-insensitive"
    );
    assert_eq!(c.cmd(&["GET", "missing"]), Value::Nil);
    assert_eq!(c.cmd(&["TYPE", "k"]), Value::Simple("string".into()));
    assert_eq!(c.cmd(&["TYPE", "missing"]), Value::Simple("none".into()));
    assert_eq!(c.cmd(&["STRLEN", "k"]), Value::Int(1));
    assert_eq!(c.cmd(&["RPUSH", "l", "a", "b"]), Value::Int(2));
    assert_eq!(c.cmd(&["LRANGE", "l", "0", "-1"]), strs(&["a", "b"]));
    assert_eq!(
        c.cmd(&["LRANGE", "missing", "0", "-1"]),
        Value::Array(vec![])
    );
    assert_eq!(c.cmd(&["LPOP", "missing"]), Value::Nil);
    assert_eq!(c.cmd(&["LPOS", "l", "zzz"]), Value::Nil);
    assert_eq!(c.cmd(&["HSET", "h", "f", "1", "g", "2"]), Value::Int(2));
    assert_eq!(c.cmd(&["HGETALL", "h"]), strs(&["f", "1", "g", "2"]));
    assert_eq!(c.cmd(&["HMSET", "h", "x", "y"]), ok());
    assert_eq!(
        c.cmd(&["MGET", "k", "missing", "l"]),
        Value::Array(vec![bulk("v"), Value::Nil, Value::Nil])
    );
    assert_eq!(c.cmd(&["SADD", "s", "m"]), Value::Int(1));
    assert_eq!(c.cmd(&["SPOP", "s"]), bulk("m"));
    assert_eq!(c.cmd(&["SPOP", "s"]), Value::Nil);
    assert_eq!(c.cmd(&["EXISTS", "k", "k", "missing", "l"]), Value::Int(3));
    assert_eq!(c.cmd(&["DEL", "k", "l", "missing"]), Value::Int(2));
    assert_eq!(c.cmd(&["DBSIZE"]), Value::Int(1));
    assert_eq!(c.cmd(&["FLUSHALL"]), ok());
    assert_eq!(c.cmd(&["KEYS", "*"]), Value::Array(vec![]));
}

#[test]
fn binary_safe_keys_fields_and_values() {
    let server = start();
    let mut c = Client::connect(&server);
    let key: &[u8] = b"k\r\n\x00\xff";
    let val: &[u8] = b"\x80\x00 \r\n$5\r\n tail";
    assert_eq!(c.bin(&[b"SET", key, val]), ok());
    assert_eq!(c.bin(&[b"GET", key]), Value::Bulk(val.to_vec()));
    assert_eq!(c.bin(&[b"STRLEN", key]), Value::Int(val.len() as i64));
    assert_eq!(
        c.bin(&[b"KEYS", b"*"]),
        Value::Array(vec![Value::Bulk(key.to_vec())])
    );
    assert_eq!(c.bin(&[b"HSET", b"h", key, val]), Value::Int(1));
    assert_eq!(
        c.bin(&[b"HGETALL", b"h"]),
        Value::Array(vec![Value::Bulk(key.to_vec()), Value::Bulk(val.to_vec())])
    );
    assert_eq!(c.bin(&[b"SADD", b"s", val, val]), Value::Int(1));
    assert_eq!(c.bin(&[b"SISMEMBER", b"s", val]), Value::Int(1));
    assert_eq!(c.bin(&[b"RPUSH", b"l", val, b""]), Value::Int(2));
    assert_eq!(
        c.bin(&[b"LRANGE", b"l", b"0", b"-1"]),
        Value::Array(vec![Value::Bulk(val.to_vec()), Value::Bulk(vec![])])
    );
    assert_eq!(c.bin(&[b"RENAME", key, b"\xfe"]), ok());
    assert_eq!(c.bin(&[b"GET", b"\xfe"]), Value::Bulk(val.to_vec()));
}

#[test]
fn large_value_round_trips() {
    let server = start();
    let mut c = Client::connect(&server);
    let big: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(c.bin(&[b"SET", b"big", &big]), ok());
    assert_eq!(c.bin(&[b"GET", b"big"]), Value::Bulk(big));
}

#[test]
fn pipelined_commands_reply_in_order() {
    let server = start();
    let mut c = Client::connect(&server);
    let mut batch = Vec::new();
    for _ in 0..1000 {
        batch.extend_from_slice(&encode(&[b"INCR", b"n"]));
    }
    batch.extend_from_slice(&encode(&[b"GET", b"n"]));
    c.send_raw(&batch);
    for i in 1..=1000 {
        assert_eq!(c.read(), Value::Int(i));
    }
    assert_eq!(c.read(), bulk("1000"));
}

#[test]
fn a_failing_command_does_not_disturb_its_neighbours() {
    let server = start();
    let mut c = Client::connect(&server);
    let mut batch = encode(&[b"RPUSH", b"l", b"a"]);
    batch.extend_from_slice(&encode(&[b"LSET", b"l", b"9", b"x"]));
    batch.extend_from_slice(&encode(&[b"INCR", b"l"]));
    batch.extend_from_slice(&encode(&[b"NOSUCH"]));
    batch.extend_from_slice(&encode(&[b"LRANGE", b"l", b"0", b"-1"]));
    c.send_raw(&batch);
    assert_eq!(c.read(), Value::Int(1));
    assert_eq!(c.read(), Value::Error("ERR index out of range".into()));
    assert!(matches!(c.read(), Value::Error(e) if e.starts_with("WRONGTYPE ")));
    assert_eq!(
        c.read(),
        Value::Error("ERR unknown command 'NOSUCH'".into())
    );
    assert_eq!(c.read(), strs(&["a"]));
}

#[test]
fn inline_commands() {
    let server = start();
    let mut c = Client::connect(&server);
    c.send_raw(b"PING\r\n");
    assert_eq!(c.read(), Value::Simple("PONG".into()));
    c.send_raw(b"SET greeting \"hello world\"\r\n\r\nGET greeting\n");
    assert_eq!(c.read(), ok());
    assert_eq!(c.read(), bulk("hello world"));
}

#[test]
fn protocol_errors_reply_then_close() {
    let server = start();
    for (input, message) in [
        (
            &b"*1\r\n:1\r\n"[..],
            "ERR Protocol error: expected '$', got ':'",
        ),
        (b"*x\r\n", "ERR Protocol error: invalid multibulk length"),
        (b"*1\r\n$-5\r\n", "ERR Protocol error: invalid bulk length"),
        (
            b"SET k \"open\r\n",
            "ERR Protocol error: unbalanced quotes in request",
        ),
    ] {
        let mut c = Client::connect(&server);
        c.send_raw(input);
        assert_eq!(c.read(), Value::Error(message.into()));
        assert!(c.closed(), "connection should close after {message}");
    }
    // The server itself is unaffected.
    assert_eq!(
        Client::connect(&server).cmd(&["PING"]),
        Value::Simple("PONG".into())
    );
}

#[test]
fn errors_match_redis_and_leave_the_connection_usable() {
    let server = start();
    let mut c = Client::connect(&server);
    assert_eq!(
        c.error(&["foo", "a", "b"]),
        "ERR unknown command 'foo', with args beginning with: 'a' 'b' "
    );
    assert_eq!(
        c.error(&["GeT"]),
        "ERR wrong number of arguments for 'get' command"
    );
    assert_eq!(
        c.error(&["GET", "a", "b"]),
        "ERR wrong number of arguments for 'get' command"
    );
    assert_eq!(
        c.error(&["PING", "a", "b"]),
        "ERR wrong number of arguments for 'ping' command"
    );
    assert_eq!(
        c.error(&["MSET", "a"]),
        "ERR wrong number of arguments for 'mset' command"
    );
    assert_eq!(
        c.error(&["MSET", "a", "1", "b"]),
        "ERR wrong number of arguments for 'mset' command"
    );
    c.cmd(&["SET", "s", "text"]);
    assert_eq!(
        c.error(&["INCR", "s"]),
        "ERR value is not an integer or out of range"
    );
    assert_eq!(
        c.error(&["LPUSH", "s", "x"]),
        "WRONGTYPE Operation against a key holding the wrong kind of value"
    );
    for bad in ["abc", "+5", "05", " 5", "1.5", "9223372036854775808", ""] {
        assert_eq!(
            c.error(&["INCRBY", "n", bad]),
            "ERR value is not an integer or out of range",
            "INCRBY n {bad:?}"
        );
    }
    assert_eq!(
        c.error(&["LRANGE", "l", "0", "x"]),
        "ERR value is not an integer or out of range"
    );
    assert_eq!(
        c.error(&["LINSERT", "l", "sideways", "a", "b"]),
        "ERR syntax error"
    );
    assert_eq!(
        c.error(&["EXPIRE", "s", "9223372036854775807"]),
        "ERR invalid expire time in 'expire' command"
    );
    assert_eq!(c.cmd(&["GET", "s"]), bulk("text"));
}

/// TTL assertions use wide windows: an exact value depends on how long the
/// machine takes between two commands.
fn ttl_between(c: &mut Client<TcpStream>, key: &str, low: i64, high: i64) -> bool {
    matches!(c.cmd(&["TTL", key]), Value::Int(n) if (low..=high).contains(&n))
}

#[test]
fn set_options() {
    let server = start();
    let mut c = Client::connect(&server);
    assert_eq!(c.cmd(&["SET", "k", "1", "XX"]), Value::Nil);
    assert_eq!(c.cmd(&["SET", "k", "1", "nx"]), ok());
    assert_eq!(c.cmd(&["SET", "k", "2", "NX"]), Value::Nil);
    assert_eq!(c.cmd(&["SET", "k", "3", "XX", "EX", "100"]), ok());
    assert!(ttl_between(&mut c, "k", 40, 100));
    assert_eq!(c.cmd(&["SET", "k", "4", "KEEPTTL"]), ok());
    assert!(ttl_between(&mut c, "k", 40, 100));
    assert_eq!(c.cmd(&["SET", "k", "5"]), ok());
    assert_eq!(c.cmd(&["TTL", "k"]), Value::Int(-1));
    assert_eq!(c.cmd(&["SET", "k", "6", "PX", "100000"]), ok());
    assert!(matches!(c.cmd(&["PTTL", "k"]), Value::Int(n) if (40_000..=100_000).contains(&n)));
    for bad in [
        &["SET", "k", "v", "NX", "XX"][..],
        &["SET", "k", "v", "EX"],
        &["SET", "k", "v", "EX", "10", "PX", "10"],
        &["SET", "k", "v", "EX", "10", "KEEPTTL"],
        &["SET", "k", "v", "KEEPTTL", "EX", "10"],
        &["SET", "k", "v", "bogus"],
    ] {
        assert_eq!(c.error(bad), "ERR syntax error", "{bad:?}");
    }
    assert_eq!(
        c.error(&["SET", "k", "v", "EX", "abc"]),
        "ERR value is not an integer or out of range"
    );
    assert_eq!(
        c.error(&["SET", "k", "v", "EX", "0"]),
        "ERR invalid expire time in 'set' command"
    );
    assert_eq!(
        c.error(&["SET", "k", "v", "EX", "9223372036854776"]),
        "ERR invalid expire time in 'set' command"
    );
    assert_eq!(
        c.cmd(&["GET", "k"]),
        bulk("6"),
        "rejected SETs must not write"
    );

    assert_eq!(c.cmd(&["SETNX", "k", "x"]), Value::Int(0));
    assert_eq!(c.cmd(&["SETNX", "fresh", "x"]), Value::Int(1));
    assert_eq!(c.cmd(&["SETEX", "e", "50", "v"]), ok());
    assert!(ttl_between(&mut c, "e", 20, 50));
    assert_eq!(c.cmd(&["PSETEX", "e", "50000", "v"]), ok());
    assert_eq!(
        c.error(&["SETEX", "e", "0", "v"]),
        "ERR invalid expire time in 'setex' command"
    );
    assert_eq!(
        c.error(&["PSETEX", "e", "-1", "v"]),
        "ERR invalid expire time in 'psetex' command"
    );
}

#[test]
fn connection_commands_clients_send_on_connect() {
    let server = start();
    let mut c = Client::connect(&server);
    let Value::Array(hello) = c.cmd(&["HELLO"]) else {
        panic!("HELLO should return an array");
    };
    assert_eq!(hello.len(), 14);
    assert_eq!(
        hello[..6],
        [
            bulk("server"),
            bulk("redis"),
            bulk("version"),
            bulk("6.2.0"),
            bulk("proto"),
            Value::Int(2)
        ]
    );
    assert_eq!(hello[13], Value::Array(vec![]));
    assert!(matches!(
        c.cmd(&["HELLO", "2", "SETNAME", "me"]),
        Value::Array(_)
    ));
    assert_eq!(
        c.error(&["HELLO", "4"]),
        "NOPROTO unsupported protocol version"
    );
    assert_eq!(
        c.error(&["HELLO", "1"]),
        "NOPROTO unsupported protocol version"
    );
    assert_eq!(
        c.error(&["HELLO", "x"]),
        "ERR Protocol version is not an integer or out of range"
    );
    // No password is configured: Redis's `default` user then accepts any
    // password, and no other user exists.
    assert_eq!(
        c.error(&["HELLO", "2", "AUTH", "u", "p"]),
        "WRONGPASS invalid username-password pair or user is disabled."
    );
    assert!(matches!(
        c.cmd(&["HELLO", "2", "AUTH", "default", "anything"]),
        Value::Array(_)
    ));
    assert_eq!(c.cmd(&["AUTH", "default", "anything"]), ok());
    assert_eq!(
        c.error(&["AUTH", "nouser", "x"]),
        "WRONGPASS invalid username-password pair or user is disabled."
    );
    assert_eq!(c.error(&["AUTH", "a", "b", "c"]), "ERR syntax error");
    assert!(
        c.error(&["AUTH", "pw"])
            .starts_with("ERR AUTH <password> called without")
    );

    assert_eq!(c.cmd(&["SELECT", "0"]), ok());
    assert_eq!(c.error(&["SELECT", "1"]), "ERR DB index is out of range");
    assert_eq!(
        c.error(&["SELECT", "x"]),
        "ERR value is not an integer or out of range"
    );

    // The earlier `HELLO 2 SETNAME me` named the connection.
    assert_eq!(c.cmd(&["CLIENT", "GETNAME"]), bulk("me"));
    assert_eq!(c.cmd(&["CLIENT", "SETNAME", "worker-1"]), ok());
    assert_eq!(c.cmd(&["CLIENT", "GETNAME"]), bulk("worker-1"));
    assert_eq!(c.cmd(&["CLIENT", "SETINFO", "lib-name", "x"]), ok());
    let Value::Int(id) = c.cmd(&["CLIENT", "ID"]) else {
        panic!("CLIENT ID should be an integer");
    };
    let mut other = Client::connect(&server);
    assert_ne!(other.cmd(&["CLIENT", "ID"]), Value::Int(id));
    assert_eq!(
        other.cmd(&["CLIENT", "GETNAME"]),
        Value::Nil,
        "names are per connection"
    );
    assert_eq!(
        c.error(&["CLIENT", "bogus"]),
        "ERR unknown subcommand 'bogus'. Try CLIENT HELP."
    );

    assert_eq!(c.cmd(&["COMMAND"]), Value::Array(vec![]));
    assert_eq!(c.cmd(&["COMMAND", "DOCS"]), Value::Array(vec![]));
    assert!(matches!(c.cmd(&["COMMAND", "COUNT"]), Value::Int(n) if n > 50));
    assert_eq!(c.cmd(&["CONFIG", "GET", "maxmemory"]), Value::Array(vec![]));

    c.cmd(&["SET", "k", "v", "EX", "100"]);
    let Value::Bulk(info) = c.cmd(&["INFO"]) else {
        panic!("INFO should be a bulk string");
    };
    let info = String::from_utf8(info).unwrap();
    assert!(info.contains("redis_version:6.2.0\r\n"), "{info}");
    assert!(info.contains("connected_clients:2\r\n"), "{info}");
    assert!(
        info.contains("db0:keys=1,expires=1,avg_ttl=0\r\n"),
        "{info}"
    );

    assert_eq!(c.cmd(&["QUIT"]), ok());
    assert!(c.closed());
}

#[test]
fn resp3_is_negotiated_per_connection() {
    let server = start();
    let mut c = Client::connect(&server);
    c.cmd(&["HSET", "h", "f", "v"]);
    c.cmd(&["SADD", "s", "m"]);
    c.cmd(&["SET", "k", "v"]);
    // RESP2 until asked otherwise.
    assert_eq!(c.cmd(&["HGETALL", "h"]), strs(&["f", "v"]));
    assert_eq!(c.cmd(&["GET", "missing"]), Value::Nil);

    // A rejected HELLO must not switch the protocol.
    assert_eq!(c.error(&["HELLO", "3", "BOGUS"]), "ERR syntax error");
    assert_eq!(c.cmd(&["GET", "missing"]), Value::Nil);

    let Value::Map(hello) = c.cmd(&["HELLO", "3"]) else {
        panic!("HELLO 3 should reply with a map");
    };
    assert_eq!(hello[2], (bulk("proto"), Value::Int(3)));
    assert_eq!(c.cmd(&["GET", "missing"]), Value::Null);
    assert_eq!(c.cmd(&["GET", "k"]), bulk("v"));
    assert_eq!(
        c.cmd(&["HGETALL", "h"]),
        Value::Map(vec![(bulk("f"), bulk("v"))])
    );
    assert_eq!(c.cmd(&["HGETALL", "missing"]), Value::Map(vec![]));
    assert_eq!(c.cmd(&["HKEYS", "h"]), strs(&["f"]));
    assert_eq!(c.cmd(&["SMEMBERS", "s"]), Value::Set(vec![bulk("m")]));
    assert_eq!(c.cmd(&["SINTER", "s", "missing"]), Value::Set(vec![]));
    assert_eq!(
        c.cmd(&["LRANGE", "missing", "0", "-1"]),
        Value::Array(vec![])
    );
    assert_eq!(
        c.cmd(&["MGET", "k", "missing"]),
        Value::Array(vec![bulk("v"), Value::Null])
    );
    assert_eq!(c.cmd(&["CONFIG", "GET", "save"]), Value::Map(vec![]));
    assert_eq!(c.cmd(&["SISMEMBER", "s", "m"]), Value::Int(1));

    // Other connections are unaffected, and HELLO 2 switches back.
    assert_eq!(
        Client::connect(&server).cmd(&["GET", "missing"]),
        Value::Nil
    );
    assert!(matches!(c.cmd(&["HELLO", "2"]), Value::Array(_)));
    assert_eq!(c.cmd(&["GET", "missing"]), Value::Nil);
}

#[test]
fn concurrent_clients_do_not_lose_updates() {
    let server = start();
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let addr = server.addr.clone();
            std::thread::spawn(move || {
                let stream = TcpStream::connect(&addr).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                let mut c = Client {
                    reader: BufReader::new(stream.try_clone().unwrap()),
                    writer: stream,
                };
                for i in 0..150 {
                    assert!(matches!(c.cmd(&["INCR", "counter"]), Value::Int(_)));
                    let item = format!("{t}:{i}");
                    assert!(matches!(c.cmd(&["RPUSH", "log", &item]), Value::Int(_)));
                    assert!(matches!(c.cmd(&["SADD", "seen", &item]), Value::Int(1)));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let mut c = Client::connect(&server);
    assert_eq!(c.cmd(&["GET", "counter"]), bulk("1200"));
    assert_eq!(c.cmd(&["LLEN", "log"]), Value::Int(1200));
    assert_eq!(c.cmd(&["SCARD", "seen"]), Value::Int(1200));
}

#[test]
fn cli_and_server_share_the_database_live() {
    let server = start();
    let mut c = Client::connect(&server);
    assert_eq!(c.cmd(&["SET", "from-server", "1"]), ok());
    assert_eq!(klyv_cli(&server.db, &["get", "from-server"]), "1\n");
    klyv_cli(&server.db, &["r-push", "from-cli", "a", "b"]);
    assert_eq!(c.cmd(&["LRANGE", "from-cli", "0", "-1"]), strs(&["a", "b"]));
    assert_eq!(c.cmd(&["PURGE"]), Value::Int(0));
}

#[test]
fn serve_is_not_a_shell_command() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut child = Command::new(KLYV)
        .args(["--db", db.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"serve\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unrecognized subcommand"));
}

#[test]
fn bind_failure_is_reported() {
    let server = start();
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(KLYV)
        .args(["--db", dir.path().join("t.db").to_str().unwrap()])
        .args(["serve", "--bind", &server.addr])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("ERR cannot bind "), "{err}");
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::net::UnixStream;

    fn connect(path: &std::path::Path) -> Client<UnixStream> {
        let stream = UnixStream::connect(path).expect("unix connect failed");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        Client {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
        }
    }

    // The socket is given as a relative path (the server runs in the temp
    // dir): absolute temp paths can exceed the OS limit on socket paths.
    #[test]
    fn unix_socket_serves_alongside_tcp() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("k.sock");
        let server = start_with(
            dir,
            &["--unixsocket", "k.sock", "--unixsocketperm", "600"],
            2,
        );
        let mut u = connect(&sock);
        assert_eq!(u.cmd(&["SET", "k", "via-unix"]), ok());
        assert_eq!(
            Client::connect(&server).cmd(&["GET", "k"]),
            bulk("via-unix")
        );
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn stale_socket_is_replaced_and_removed_on_sigterm() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("k.sock");
        // A socket file nobody is listening on, as a crashed server leaves.
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        assert!(
            std::fs::symlink_metadata(&sock)
                .unwrap()
                .file_type()
                .is_socket()
        );
        let mut server = start_with(dir, &["--unixsocket", "k.sock"], 2);
        assert_eq!(connect(&sock).cmd(&["PING"]), Value::Simple("PONG".into()));

        let pid = server.child.id().to_string();
        assert!(
            Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .unwrap()
                .success()
        );
        let status = server.child.wait().unwrap();
        assert!(
            status.success(),
            "SIGTERM should be a clean exit: {status:?}"
        );
        assert!(!sock.exists(), "socket file should be removed on shutdown");
    }

    #[test]
    fn refuses_a_socket_that_is_in_use_or_not_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let _first = start_with(dir, &["--unixsocket", "k.sock"], 2);
        std::fs::write(path.join("plain"), b"data").unwrap();
        for (target, reason) in [
            ("k.sock", "already being served"),
            ("plain", "not a socket"),
        ] {
            let out = Command::new(KLYV)
                .args(["--db", "t.db", "serve", "--unixsocket", target])
                .current_dir(&path)
                .output()
                .unwrap();
            assert!(!out.status.success());
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains(reason), "{target}: {err}");
        }
        assert_eq!(std::fs::read(path.join("plain")).unwrap(), b"data");
    }
}

// A mistyped modifier must be refused, not treated as "flush anyway".
#[test]
fn flush_rejects_unknown_arguments_without_flushing() {
    let server = start();
    let mut c = Client::connect(&server);
    c.cmd(&["SET", "k", "v"]);
    assert_eq!(c.error(&["FLUSHALL", "typo"]), "ERR syntax error");
    assert_eq!(c.error(&["FLUSHDB", "ASYNC", "extra"]), "ERR syntax error");
    assert_eq!(c.cmd(&["GET", "k"]), bulk("v"));
    assert_eq!(c.cmd(&["FLUSHDB", "sync"]), ok());
    assert_eq!(c.cmd(&["DBSIZE"]), Value::Int(0));
    c.cmd(&["SET", "k", "v"]);
    assert_eq!(c.cmd(&["FLUSHALL", "ASYNC"]), ok());
    assert_eq!(c.cmd(&["DBSIZE"]), Value::Int(0));
}

// One whole command plus the start of the next: the first reply must arrive
// without waiting for the second command to be completed.
#[test]
fn a_partial_pipelined_command_does_not_hold_back_earlier_replies() {
    let server = start();
    let mut c = Client::connect(&server);
    c.send_raw(b"PING\r\n*2\r\n$4\r\nECHO\r\n$5\r\nhe");
    assert_eq!(c.read(), Value::Simple("PONG".into()));
    c.send_raw(b"llo\r\n");
    assert_eq!(c.read(), bulk("hello"));
}

#[test]
fn inline_escapes_reach_the_command_as_bytes() {
    let server = start();
    let mut c = Client::connect(&server);
    c.send_raw(b"SET k \"\\x00\\xff\\n\"\r\n");
    assert_eq!(c.read(), ok());
    assert_eq!(c.cmd(&["GET", "k"]), Value::Bulk(vec![0x00, 0xff, b'\n']));
    c.send_raw(b"ECHO \"a\"b\r\n");
    assert_eq!(
        c.read(),
        Value::Error("ERR Protocol error: unbalanced quotes in request".into())
    );
    assert!(c.closed());
}

#[test]
fn info_is_verbatim_text_in_resp3() {
    let server = start();
    let mut c = Client::connect(&server);
    assert!(matches!(c.cmd(&["INFO"]), Value::Bulk(_)));
    c.cmd(&["HELLO", "3"]);
    let Value::Verbatim(info) = c.cmd(&["INFO"]) else {
        panic!("INFO should be a verbatim string in RESP3");
    };
    assert!(info.starts_with("txt:# Server\r\nredis_version:"), "{info}");
}

#[test]
fn connections_beyond_maxclients_are_refused() {
    let server = start_with(tempfile::tempdir().unwrap(), &["--maxclients", "2"], 1);
    let mut a = Client::connect(&server);
    let mut b = Client::connect(&server);
    // A reply from each proves both are established and counted.
    assert_eq!(a.cmd(&["PING"]), Value::Simple("PONG".into()));
    assert_eq!(b.cmd(&["PING"]), Value::Simple("PONG".into()));
    let mut extra = Client::connect(&server);
    assert_eq!(
        extra.read(),
        Value::Error("ERR max number of clients reached".into())
    );
    assert!(extra.closed());
    // Closing one frees a slot (the server notices on its next read).
    assert_eq!(a.cmd(&["QUIT"]), ok());
    assert!(a.closed());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut again = Client::connect(&server);
        again.send_raw(b"PING\r\n");
        if again.read() == Value::Simple("PONG".into()) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "slot was never freed");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(b.cmd(&["PING"]), Value::Simple("PONG".into()));
}

// === Transactions ===

fn null_array() -> Value {
    Value::NullArray
}

#[test]
fn exec_replies_with_each_result_and_keeps_going_after_a_failure() {
    let server = start();
    let mut c = Client::connect(&server);
    assert_eq!(c.cmd(&["MULTI"]), ok());
    assert_eq!(c.cmd(&["SET", "n", "5"]), Value::Simple("QUEUED".into()));
    assert_eq!(c.cmd(&["INCR", "n"]), Value::Simple("QUEUED".into()));
    assert_eq!(c.cmd(&["LPUSH", "n", "x"]), Value::Simple("QUEUED".into()));
    assert_eq!(c.cmd(&["INCR", "n"]), Value::Simple("QUEUED".into()));
    assert_eq!(
        c.cmd(&["EXEC"]),
        Value::Array(vec![
            ok(),
            Value::Int(6),
            Value::Error(
                "WRONGTYPE Operation against a key holding the wrong kind of value".into()
            ),
            Value::Int(7),
        ])
    );
    assert_eq!(c.cmd(&["GET", "n"]), bulk("7"));
}

// A failing command inside EXEC is undone on its own, even when it had
// already written something before failing.
#[test]
fn a_failed_command_in_exec_leaves_no_partial_write() {
    let server = start();
    let mut c = Client::connect(&server);
    c.cmd(&["SET", "a", "1"]);
    c.cmd(&["RPUSH", "b", "x"]);
    c.cmd(&["MULTI"]);
    // MSET writes `a` and then... succeeds; the failure is the INCR on a list.
    c.cmd(&["MSET", "a", "2", "c", "3"]);
    c.cmd(&["INCR", "b"]);
    c.cmd(&["APPEND", "a", "!"]);
    assert_eq!(
        c.cmd(&["EXEC"]),
        Value::Array(vec![
            ok(),
            Value::Error(
                "WRONGTYPE Operation against a key holding the wrong kind of value".into()
            ),
            Value::Int(2),
        ])
    );
    assert_eq!(c.cmd(&["MGET", "a", "c"]), strs(&["2!", "3"]));
    assert_eq!(c.cmd(&["LRANGE", "b", "0", "-1"]), strs(&["x"]));
}

#[test]
fn queued_commands_are_invisible_until_exec_and_atomic_when_run() {
    let server = start();
    let mut writer = Client::connect(&server);
    let mut reader = Client::connect(&server);
    writer.cmd(&["SET", "a", "0"]);
    writer.cmd(&["SET", "b", "0"]);
    writer.cmd(&["MULTI"]);
    writer.cmd(&["INCR", "a"]);
    writer.cmd(&["INCR", "b"]);
    // Nothing has happened yet, and MULTI state belongs to one connection.
    assert_eq!(reader.cmd(&["MGET", "a", "b"]), strs(&["0", "0"]));
    assert_eq!(reader.cmd(&["INCR", "other"]), Value::Int(1));
    assert_eq!(
        writer.cmd(&["EXEC"]),
        Value::Array(vec![Value::Int(1), Value::Int(1)])
    );
    assert_eq!(reader.cmd(&["MGET", "a", "b"]), strs(&["1", "1"]));

    // Many transfers between two counters from several connections: every
    // observer must see them sum to the same total at all times.
    writer.cmd(&["MSET", "x", "1000", "y", "0"]);
    let addr = server.addr.clone();
    let movers: Vec<_> = (0..4)
        .map(|_| {
            let addr = addr.clone();
            std::thread::spawn(move || {
                let stream = TcpStream::connect(&addr).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                let mut c = Client {
                    reader: BufReader::new(stream.try_clone().unwrap()),
                    writer: stream,
                };
                for _ in 0..50 {
                    c.cmd(&["MULTI"]);
                    c.cmd(&["DECR", "x"]);
                    c.cmd(&["INCR", "y"]);
                    assert!(matches!(c.cmd(&["EXEC"]), Value::Array(v) if v.len() == 2));
                }
            })
        })
        .collect();
    for _ in 0..200 {
        let Value::Array(pair) = reader.cmd(&["MGET", "x", "y"]) else {
            panic!("MGET should return an array");
        };
        let total: i64 = pair
            .iter()
            .map(|v| match v {
                Value::Bulk(b) => String::from_utf8_lossy(b).parse::<i64>().unwrap(),
                other => panic!("unexpected {other:?}"),
            })
            .sum();
        assert_eq!(total, 1000, "a transfer was observed half done");
    }
    for m in movers {
        m.join().unwrap();
    }
    assert_eq!(reader.cmd(&["MGET", "x", "y"]), strs(&["800", "200"]));
}

#[test]
fn watch_aborts_exec_when_another_client_or_the_cli_changes_the_key() {
    let server = start();
    let mut a = Client::connect(&server);
    let mut b = Client::connect(&server);
    a.cmd(&["SET", "k", "1"]);

    // Changed by another connection.
    assert_eq!(a.cmd(&["WATCH", "k"]), ok());
    assert_eq!(b.cmd(&["SET", "k", "2"]), ok());
    a.cmd(&["MULTI"]);
    a.cmd(&["SET", "k", "from-a"]);
    assert_eq!(a.cmd(&["EXEC"]), null_array());
    assert_eq!(a.cmd(&["GET", "k"]), bulk("2"));

    // Changed by the CLI, a different process altogether.
    assert_eq!(a.cmd(&["WATCH", "k"]), ok());
    klyv_cli(&server.db, &["append", "k", "!"]);
    a.cmd(&["MULTI"]);
    a.cmd(&["SET", "k", "from-a"]);
    assert_eq!(a.cmd(&["EXEC"]), null_array());
    assert_eq!(a.cmd(&["GET", "k"]), bulk("2!"));

    // Created, deleted, or given a TTL: all count as changes.
    for change in [
        &["SET", "absent", "now-here"][..],
        &["DEL", "k"],
        &["EXPIRE", "list", "1000"],
        &["RPUSH", "list", "more"],
    ] {
        b.cmd(&["SET", "k", "v"]);
        b.cmd(&["DEL", "absent", "list"]);
        b.cmd(&["RPUSH", "list", "x"]);
        assert_eq!(a.cmd(&["WATCH", "k", "absent", "list"]), ok());
        b.cmd(change);
        a.cmd(&["MULTI"]);
        a.cmd(&["INCR", "ran"]);
        assert_eq!(a.cmd(&["EXEC"]), null_array(), "{change:?} went unnoticed");
    }
    assert_eq!(a.cmd(&["EXISTS", "ran"]), Value::Int(0));

    // Untouched watched keys do not get in the way, and a change to an
    // unwatched key is irrelevant.
    assert_eq!(a.cmd(&["WATCH", "k"]), ok());
    b.cmd(&["SET", "unrelated", "x"]);
    a.cmd(&["MULTI"]);
    a.cmd(&["INCR", "ran"]);
    assert_eq!(a.cmd(&["EXEC"]), Value::Array(vec![Value::Int(1)]));

    // The classic check-and-set loop converges under contention.
    a.cmd(&["SET", "counter", "0"]);
    let addr = server.addr.clone();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let addr = addr.clone();
            std::thread::spawn(move || {
                let stream = TcpStream::connect(&addr).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                let mut c = Client {
                    reader: BufReader::new(stream.try_clone().unwrap()),
                    writer: stream,
                };
                for _ in 0..25 {
                    loop {
                        c.cmd(&["WATCH", "counter"]);
                        let Value::Bulk(v) = c.cmd(&["GET", "counter"]) else {
                            panic!("counter should exist");
                        };
                        let next = String::from_utf8(v).unwrap().parse::<i64>().unwrap() + 1;
                        c.cmd(&["MULTI"]);
                        c.cmd(&["SET", "counter", &next.to_string()]);
                        if c.cmd(&["EXEC"]) == Value::Array(vec![ok()]) {
                            break;
                        }
                    }
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    assert_eq!(a.cmd(&["GET", "counter"]), bulk("100"));
}

// === SCAN ===

fn scan_all(c: &mut Client<TcpStream>, args: &[&str]) -> Vec<String> {
    let mut cursor = String::from("0");
    let mut seen = Vec::new();
    loop {
        let full: Vec<&str> = args
            .iter()
            .map(|a| if *a == "CURSOR" { cursor.as_str() } else { a })
            .collect();
        let Value::Array(page) = c.cmd(&full) else {
            panic!("scan page should be an array");
        };
        let (Value::Bulk(next), Value::Array(items)) = (&page[0], &page[1]) else {
            panic!("scan page should be [cursor, items]");
        };
        for item in items {
            let Value::Bulk(b) = item else {
                panic!("scan item should be a bulk string");
            };
            seen.push(String::from_utf8(b.clone()).unwrap());
        }
        cursor = String::from_utf8(next.clone()).unwrap();
        if cursor == "0" {
            return seen;
        }
    }
}

// The guarantee that matters: a key that exists for the whole iteration is
// returned exactly once, whatever else is created, deleted, renamed or
// rewritten while the iteration is in progress.
#[test]
fn scan_returns_every_stable_key_once_despite_concurrent_changes() {
    let server = start();
    let mut c = Client::connect(&server);
    let mut other = Client::connect(&server);
    for i in 0..200 {
        c.cmd(&["SET", &format!("stable:{i}"), "v"]);
        c.cmd(&["SET", &format!("doomed:{i}"), "v"]);
    }
    let mut cursor = String::from("0");
    let mut seen = Vec::new();
    let mut round = 0;
    loop {
        let Value::Array(page) = c.cmd(&["SCAN", &cursor, "COUNT", "7"]) else {
            panic!("scan page should be an array");
        };
        let (Value::Bulk(next), Value::Array(items)) = (&page[0], &page[1]) else {
            panic!("scan page should be [cursor, items]");
        };
        assert!(items.len() <= 7, "COUNT bounds the page");
        for item in items {
            if let Value::Bulk(b) = item {
                seen.push(String::from_utf8(b.clone()).unwrap());
            }
        }
        // Churn between pages: delete, create, overwrite in place (including
        // a change of type), and give keys a TTL.
        other.cmd(&["DEL", &format!("doomed:{round}")]);
        other.cmd(&["SET", &format!("new:{round}"), "v"]);
        other.cmd(&["SET", &format!("stable:{}", round % 200), "rewritten"]);
        other.cmd(&["DEL", &format!("stable-as-list:{round}")]);
        other.cmd(&["EXPIRE", &format!("stable:{}", (round * 3) % 200), "1000"]);
        if round % 5 == 0 {
            let key = format!("stable:{}", (round * 7) % 200);
            other.cmd(&["DEL", "tmp"]);
            other.cmd(&["RPUSH", "tmp", "a"]);
            other.cmd(&["COPY", "tmp", &key, "REPLACE"]);
        }
        round += 1;
        cursor = String::from_utf8(next.clone()).unwrap();
        if cursor == "0" {
            break;
        }
    }
    for i in 0..200 {
        let key = format!("stable:{i}");
        let times = seen.iter().filter(|k| **k == key).count();
        assert!(times >= 1, "{key} was never returned");
    }
    // Keys never touched by a replace are returned exactly once.
    let once = seen.iter().filter(|k| *k == "stable:1").count();
    assert_eq!(once, 1);
}

#[test]
fn scan_family_pages_filters_and_edge_cases() {
    let server = start();
    let mut c = Client::connect(&server);
    for i in 0..25 {
        c.cmd(&["SET", &format!("s{i}"), "v"]);
        c.cmd(&["HSET", "h", &format!("f{i}"), &format!("v{i}")]);
        c.cmd(&["SADD", "set", &format!("m{i}")]);
    }
    c.cmd(&["RPUSH", "l", "a"]);
    c.cmd(&["SET", "expired", "v"]);
    c.cmd(&["PEXPIREAT", "expired", "1"]);

    let mut all = scan_all(&mut c, &["SCAN", "CURSOR", "COUNT", "4"]);
    all.sort();
    assert_eq!(
        all.len(),
        28,
        "25 strings, h, set and l; not the expired key"
    );
    assert!(!all.contains(&"expired".to_string()));
    assert_eq!(
        scan_all(&mut c, &["SCAN", "CURSOR", "MATCH", "s1?"]).len(),
        10
    );
    assert_eq!(scan_all(&mut c, &["SCAN", "CURSOR", "TYPE", "hash"]), ["h"]);
    assert_eq!(
        scan_all(&mut c, &["SCAN", "CURSOR", "TYPE", "zset"]).len(),
        0
    );
    assert_eq!(
        scan_all(&mut c, &["HSCAN", "h", "CURSOR", "COUNT", "3"]).len(),
        50
    );
    assert_eq!(
        scan_all(&mut c, &["HSCAN", "h", "CURSOR", "NOVALUES"]).len(),
        25
    );
    assert_eq!(
        scan_all(&mut c, &["HSCAN", "h", "CURSOR", "MATCH", "f2*"]).len(),
        12
    );
    assert_eq!(
        scan_all(&mut c, &["SSCAN", "set", "CURSOR", "COUNT", "6"]).len(),
        25
    );

    // Any numeric cursor is accepted, including ones no page ever returned.
    for cursor in ["99999999", "18446744073709551615"] {
        assert_eq!(
            c.cmd(&["SCAN", cursor]),
            Value::Array(vec![bulk("0"), Value::Array(vec![])])
        );
    }
    assert_eq!(c.error(&["SCAN", "abc"]), "ERR invalid cursor");
    assert_eq!(c.error(&["SCAN", "-1"]), "ERR invalid cursor");
    assert_eq!(c.error(&["SCAN", "0", "COUNT", "0"]), "ERR syntax error");
    assert_eq!(
        c.error(&["SSCAN", "set", "0", "NOVALUES"]),
        "ERR NOVALUES option can only be used in HSCAN"
    );

    // In RESP3 the items are still a plain array, as in Redis.
    c.cmd(&["HELLO", "3"]);
    assert!(matches!(
        c.cmd(&["HSCAN", "h", "0", "COUNT", "100"]),
        Value::Array(page) if matches!(&page[1], Value::Array(items) if items.len() == 50)
    ));
    assert_eq!(c.cmd(&["LPOP", "nokey", "2"]), Value::Null);
    assert_eq!(c.cmd(&["SPOP", "set", "0"]), Value::Set(vec![]));
}

// Overwriting an existing key by RENAME or COPY ... REPLACE must not move it
// behind a SCAN cursor: it existed before, during and after.
#[test]
fn scan_does_not_lose_a_key_overwritten_by_rename_or_copy() {
    for overwrite in [&["RENAME", "a", "b"][..], &["COPY", "a", "b", "REPLACE"]] {
        let server = start();
        let mut c = Client::connect(&server);
        c.cmd(&["SET", "a", "x"]);
        c.cmd(&["SET", "b", "y"]);
        let Value::Array(page) = c.cmd(&["SCAN", "0", "COUNT", "1"]) else {
            panic!("scan page should be an array");
        };
        assert_eq!(page[1], strs(&["a"]));
        let Value::Bulk(cursor) = &page[0] else {
            panic!("cursor should be a bulk string");
        };
        let cursor = String::from_utf8(cursor.clone()).unwrap();
        c.cmd(overwrite);
        let mut rest = Vec::new();
        let mut cursor = cursor;
        while cursor != "0" {
            let Value::Array(page) = c.cmd(&["SCAN", &cursor, "COUNT", "1"]) else {
                panic!("scan page should be an array");
            };
            if let (Value::Bulk(next), Value::Array(items)) = (&page[0], &page[1]) {
                rest.extend(items.iter().cloned());
                cursor = String::from_utf8(next.clone()).unwrap();
            }
        }
        assert!(
            rest.contains(&bulk("b")),
            "{overwrite:?} hid b from the scan"
        );
        assert_eq!(c.cmd(&["GET", "b"]), bulk("x"));
    }
}

// A client that rewrites each field as it receives it must still finish:
// updating a field must not move it ahead of the cursor.
#[test]
fn hscan_terminates_when_fields_are_updated_during_the_iteration() {
    let server = start();
    let mut c = Client::connect(&server);
    c.cmd(&["HSET", "h", "a", "1", "b", "2", "c", "3"]);
    let mut cursor = String::from("0");
    let mut seen = Vec::new();
    for round in 0.. {
        assert!(round < 20, "HSCAN is not terminating: {seen:?}");
        let Value::Array(page) = c.cmd(&["HSCAN", "h", &cursor, "COUNT", "1"]) else {
            panic!("scan page should be an array");
        };
        let (Value::Bulk(next), Value::Array(items)) = (&page[0], &page[1]) else {
            panic!("scan page should be [cursor, items]");
        };
        for pair in items.chunks(2) {
            let Value::Bulk(field) = &pair[0] else {
                panic!("field should be a bulk string");
            };
            let field = String::from_utf8(field.clone()).unwrap();
            // Rewrite it, both ways a field's value can change.
            assert_eq!(c.cmd(&["HSET", "h", &field, "rewritten"]), Value::Int(0));
            c.cmd(&["HSET", "h", &field, "10"]);
            assert_eq!(c.cmd(&["HINCRBY", "h", &field, "1"]), Value::Int(11));
            seen.push(field);
        }
        cursor = String::from_utf8(next.clone()).unwrap();
        if cursor == "0" {
            break;
        }
    }
    seen.sort();
    assert_eq!(seen, ["a", "b", "c"]);
    assert_eq!(c.cmd(&["HLEN", "h"]), Value::Int(3));
}

#[test]
fn watching_a_key_repeatedly_keeps_its_first_state() {
    let server = start();
    let mut a = Client::connect(&server);
    let mut b = Client::connect(&server);
    a.cmd(&["SET", "k", "1"]);
    assert_eq!(a.cmd(&["WATCH", "k"]), ok());
    b.cmd(&["SET", "k", "2"]);
    // Watching again after the change must not forgive it.
    for _ in 0..1000 {
        assert_eq!(a.cmd(&["WATCH", "k"]), ok());
    }
    a.cmd(&["MULTI"]);
    a.cmd(&["SET", "k", "mine"]);
    assert_eq!(a.cmd(&["EXEC"]), null_array());
    assert_eq!(a.cmd(&["GET", "k"]), bulk("2"));
}

// === Authentication ===

const NOAUTH: &str = "NOAUTH Authentication required.";
const WRONGPASS: &str = "WRONGPASS invalid username-password pair or user is disabled.";

fn start_with_password() -> Server {
    start_with(
        tempfile::tempdir().unwrap(),
        &["--requirepass", "s3cret"],
        1,
    )
}

// Every expectation here was taken from a real Redis started with
// --requirepass.
#[test]
fn a_password_gates_every_command_until_auth() {
    let server = start_with_password();
    let mut c = Client::connect(&server);
    for cmd in [
        &["PING"][..],
        &["GET", "k"],
        &["SET", "k", "v"],
        &["ECHO", "hi"],
        &["MULTI"],
        &["INFO"],
        &["NOSUCHCOMMAND"],
        &["FLUSHALL"],
    ] {
        assert_eq!(c.error(cmd), NOAUTH, "{cmd:?}");
    }
    assert!(
        c.error(&["HELLO"])
            .starts_with("NOAUTH HELLO must be called with the client already authenticated")
    );
    assert!(
        c.error(&["HELLO", "3"])
            .starts_with("NOAUTH HELLO must be called")
    );
    assert_eq!(
        c.error(&["HELLO", "2", "AUTH", "default", "wrong"]),
        WRONGPASS
    );
    assert_eq!(
        c.error(&["AUTH"]),
        "ERR wrong number of arguments for 'auth' command"
    );
    assert_eq!(c.error(&["AUTH", "wrong"]), WRONGPASS);
    assert_eq!(c.error(&["AUTH", "s3cre"]), WRONGPASS);
    assert_eq!(c.error(&["AUTH", "s3cret!"]), WRONGPASS);
    assert_eq!(c.error(&["AUTH", "default", "wrong"]), WRONGPASS);
    assert_eq!(c.error(&["AUTH", "nouser", "s3cret"]), WRONGPASS);
    assert_eq!(c.error(&["AUTH", "a", "b", "c"]), "ERR syntax error");
    // Still locked out, and nothing leaked through.
    assert_eq!(c.error(&["GET", "k"]), NOAUTH);

    assert_eq!(c.cmd(&["AUTH", "s3cret"]), ok());
    assert_eq!(c.cmd(&["GET", "k"]), Value::Nil);
    assert_eq!(c.cmd(&["SET", "k", "v"]), ok());
    // A later failed AUTH does not lock an authenticated connection out.
    assert_eq!(c.error(&["AUTH", "wrong"]), WRONGPASS);
    assert_eq!(c.cmd(&["GET", "k"]), bulk("v"));

    // Authentication belongs to the connection.
    let mut other = Client::connect(&server);
    assert_eq!(other.error(&["GET", "k"]), NOAUTH);
    assert_eq!(other.cmd(&["AUTH", "default", "s3cret"]), ok());
    assert_eq!(other.cmd(&["GET", "k"]), bulk("v"));

    // QUIT works without authenticating.
    let mut leaving = Client::connect(&server);
    assert_eq!(leaving.cmd(&["QUIT"]), ok());
    assert!(leaving.closed());
}

#[test]
fn hello_can_authenticate_and_switch_protocol_in_one_step() {
    let server = start_with_password();
    let mut c = Client::connect(&server);
    // A rejected HELLO changes nothing: not the protocol, not the name.
    assert_eq!(
        c.error(&["HELLO", "3", "AUTH", "default", "wrong", "SETNAME", "x"]),
        WRONGPASS
    );
    assert_eq!(c.error(&["PING"]), NOAUTH);
    let Value::Map(hello) = c.cmd(&[
        "HELLO", "3", "AUTH", "default", "s3cret", "SETNAME", "worker",
    ]) else {
        panic!("HELLO 3 with valid credentials should reply with a map");
    };
    assert_eq!(hello[2], (bulk("proto"), Value::Int(3)));
    assert_eq!(c.cmd(&["GET", "missing"]), Value::Null);
    assert_eq!(c.cmd(&["CLIENT", "GETNAME"]), bulk("worker"));
    // Once authenticated, HELLO needs no credentials.
    assert!(matches!(c.cmd(&["HELLO", "2"]), Value::Array(_)));
}

#[test]
fn the_password_can_come_from_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db").to_str().unwrap().to_string();
    let mut child = Command::new(KLYV)
        .args(["--db", &db, "serve", "--bind", "127.0.0.1:0"])
        .env("KLYV_REQUIREPASS", "from-env")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let server = Server {
        child,
        addr: line
            .trim()
            .strip_prefix("listening on tcp://")
            .unwrap()
            .to_string(),
        db,
        _dir: dir,
    };
    let mut c = Client::connect(&server);
    assert_eq!(c.error(&["PING"]), NOAUTH);
    assert_eq!(c.cmd(&["AUTH", "from-env"]), ok());
    assert_eq!(c.cmd(&["PING"]), Value::Simple("PONG".into()));

    let out = Command::new(KLYV)
        .args([
            "--db",
            &server.db,
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--requirepass",
            "",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--requirepass must not be empty"));
}

// The default soft limit on open files is often far too low for a server.
// Started under a limit that would allow about a dozen clients, the server
// must raise it and serve many more.
#[cfg(unix)]
#[test]
fn the_server_raises_a_low_open_file_limit() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db").to_str().unwrap().to_string();
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("ulimit -S -n 64 && exec \"$0\" --db \"$1\" serve --bind 127.0.0.1:0 --maxclients 100")
        .arg(KLYV)
        .arg(&db)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let server = Server {
        child,
        addr: line
            .trim()
            .strip_prefix("listening on tcp://")
            .unwrap()
            .to_string(),
        db,
        _dir: dir,
    };
    // 60 clients need about 240 descriptors, far beyond the 64 it began with.
    let mut clients: Vec<_> = (0..60).map(|_| Client::connect(&server)).collect();
    for (i, c) in clients.iter_mut().enumerate() {
        assert_eq!(
            c.cmd(&["INCR", "n"]),
            Value::Int(i as i64 + 1),
            "client {i} could not be served"
        );
    }
}
