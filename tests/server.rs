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
                Value::Array((0..n).map(|_| self.read()).collect())
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

#[test]
fn set_options() {
    let server = start();
    let mut c = Client::connect(&server);
    assert_eq!(c.cmd(&["SET", "k", "1", "XX"]), Value::Nil);
    assert_eq!(c.cmd(&["SET", "k", "1", "nx"]), ok());
    assert_eq!(c.cmd(&["SET", "k", "2", "NX"]), Value::Nil);
    assert_eq!(c.cmd(&["SET", "k", "3", "XX", "EX", "100"]), ok());
    assert_eq!(c.cmd(&["TTL", "k"]), Value::Int(100));
    assert_eq!(c.cmd(&["SET", "k", "4", "KEEPTTL"]), ok());
    assert_eq!(c.cmd(&["TTL", "k"]), Value::Int(100));
    assert_eq!(c.cmd(&["SET", "k", "5"]), ok());
    assert_eq!(c.cmd(&["TTL", "k"]), Value::Int(-1));
    assert_eq!(c.cmd(&["SET", "k", "6", "PX", "100000"]), ok());
    assert!(matches!(c.cmd(&["PTTL", "k"]), Value::Int(n) if (99_000..=100_000).contains(&n)));
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
    assert_eq!(c.cmd(&["TTL", "e"]), Value::Int(50));
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
        c.error(&["HELLO", "3"]),
        "NOPROTO unsupported protocol version"
    );
    assert_eq!(
        c.error(&["HELLO", "x"]),
        "ERR Protocol version is not an integer or out of range"
    );
    assert!(
        c.error(&["HELLO", "2", "AUTH", "u", "p"])
            .starts_with("ERR AUTH <password> called without")
    );
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

    assert_eq!(c.cmd(&["CLIENT", "GETNAME"]), Value::Nil);
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
