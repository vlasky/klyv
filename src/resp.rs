//! RESP wire protocol: reading client commands and encoding replies in
//! RESP2 or RESP3.
//!
//! A client command is either a RESP array of bulk strings
//! (`*2\r\n$3\r\nGET\r\n$1\r\nk\r\n`) or an "inline" command, a plain line of
//! space-separated words (`PING\r\n`), which is what telnet and some health
//! checks send. Both produce the same thing: a list of byte-string arguments
//! whose first element is the command name.
//!
//! Limits and error texts follow Redis, so a misbehaving client sees the
//! diagnostics it would see from Redis and cannot make the server allocate
//! unbounded memory from a length header alone.

use crate::reply::{Empty, Reply};
use crate::shell::split_line;
use std::io::{self, BufRead, Read};

/// Redis's `proto-max-bulk-len` default: 512 MB per argument.
const MAX_BULK_LEN: usize = 512 * 1024 * 1024;
/// Redis's cap on the number of arguments in one command.
const MAX_ARGS: usize = 1024 * 1024;
/// Redis's cap on an inline command or a header line.
const MAX_LINE: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) enum ReadError {
    /// The connection failed or closed part-way through a command.
    Closed,
    /// The client broke the protocol. The message is sent as an error reply
    /// and the connection closed, as Redis does.
    Protocol(String),
}

impl From<io::Error> for ReadError {
    fn from(_: io::Error) -> Self {
        ReadError::Closed
    }
}

fn protocol(msg: impl Into<String>) -> ReadError {
    ReadError::Protocol(format!("Protocol error: {}", msg.into()))
}

/// Reads one line, without its terminator. `Ok(None)` means the stream ended
/// before any byte of the line. `too_long` is the protocol error to raise if
/// the line exceeds `MAX_LINE`.
fn read_line<R: BufRead>(r: &mut R, too_long: &str) -> Result<Option<Vec<u8>>, ReadError> {
    let mut line = Vec::new();
    // Bounded so a client that never sends a newline cannot grow the buffer.
    let n = r.take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        if line.len() > MAX_LINE {
            return Err(protocol(too_long));
        }
        return Err(ReadError::Closed);
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

/// Strict decimal parse of a length header: digits with an optional leading
/// minus, nothing else.
fn parse_len(bytes: &[u8]) -> Option<i64> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// Splits an inline command into arguments. Valid UTF-8 goes through the
/// shell tokenizer (quotes and backslash escapes); anything else is split on
/// ASCII whitespace.
fn split_inline(line: &[u8]) -> Result<Vec<Vec<u8>>, ReadError> {
    match std::str::from_utf8(line) {
        Ok(s) => split_line(s)
            .map(|words| words.into_iter().map(String::into_bytes).collect())
            .ok_or_else(|| protocol("unbalanced quotes in request")),
        Err(_) => Ok(line
            .split(|b| b.is_ascii_whitespace())
            .filter(|w| !w.is_empty())
            .map(<[u8]>::to_vec)
            .collect()),
    }
}

/// Reads the next command. `Ok(None)` is a clean end of stream (the client
/// closed the connection between commands). Empty commands (a blank line,
/// `*0`, `*-1`) are skipped, as in Redis.
pub(crate) fn read_command<R: BufRead>(r: &mut R) -> Result<Option<Vec<Vec<u8>>>, ReadError> {
    loop {
        let Some(line) = read_line(r, "too big inline request")? else {
            return Ok(None);
        };
        if line.first() != Some(&b'*') {
            let args = split_inline(&line)?;
            if args.is_empty() {
                continue;
            }
            return Ok(Some(args));
        }
        let count = parse_len(&line[1..])
            .filter(|n| *n <= MAX_ARGS as i64)
            .ok_or_else(|| protocol("invalid multibulk length"))?;
        if count <= 0 {
            continue;
        }
        // Grows as arguments actually arrive rather than trusting the header.
        let mut args = Vec::with_capacity((count as usize).min(64));
        for _ in 0..count {
            let header = read_line(r, "too big bulk count string")?.ok_or(ReadError::Closed)?;
            if header.first() != Some(&b'$') {
                let got = header.first().map_or(' ', |b| *b as char);
                return Err(protocol(format!("expected '$', got '{got}'")));
            }
            let len = parse_len(&header[1..])
                .filter(|n| (0..=MAX_BULK_LEN as i64).contains(n))
                .ok_or_else(|| protocol("invalid bulk length"))? as usize;
            // `take` + `read_to_end` allocates as data arrives, so a huge
            // declared length costs nothing until the bytes are really sent.
            let mut arg = Vec::with_capacity(len.min(64 * 1024));
            r.take(len as u64).read_to_end(&mut arg)?;
            if arg.len() != len {
                return Err(ReadError::Closed);
            }
            let mut crlf = [0u8; 2];
            r.read_exact(&mut crlf)?;
            args.push(arg);
        }
        return Ok(Some(args));
    }
}

fn push_bulk(out: &mut Vec<u8>, v: &[u8]) {
    out.extend_from_slice(format!("${}\r\n", v.len()).as_bytes());
    out.extend_from_slice(v);
    out.extend_from_slice(b"\r\n");
}

/// The protocol version a connection speaks, chosen by HELLO.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Proto {
    Resp2,
    Resp3,
}

fn push_header(out: &mut Vec<u8>, kind: char, len: usize) {
    out.extend_from_slice(format!("{kind}{len}\r\n").as_bytes());
}

/// Encodes a reply. RESP3 differs from RESP2 in three ways that matter here:
/// nil is `_` rather than a null bulk string, a set reply is `~` rather than
/// an array, and a hash or map reply is `%` (counting pairs) rather than a
/// flat array of alternating keys and values.
pub(crate) fn encode(reply: &Reply, proto: Proto, out: &mut Vec<u8>) {
    let v3 = proto == Proto::Resp3;
    match reply {
        Reply::Simple(s) => {
            out.push(b'+');
            out.extend_from_slice(s.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        Reply::Int(n) => out.extend_from_slice(format!(":{n}\r\n").as_bytes()),
        Reply::Bulk(v) => push_bulk(out, v),
        Reply::Nil if v3 => out.extend_from_slice(b"_\r\n"),
        Reply::Nil => out.extend_from_slice(b"$-1\r\n"),
        Reply::Array(items, kind) => {
            match kind {
                Empty::Set if v3 => push_header(out, '~', items.len()),
                Empty::Hash if v3 => push_header(out, '%', items.len() / 2),
                _ => push_header(out, '*', items.len()),
            }
            for item in items {
                push_bulk(out, item);
            }
        }
        Reply::Lines(replies) => {
            push_header(out, '*', replies.len());
            for r in replies {
                encode(r, proto, out);
            }
        }
        Reply::Map(pairs) => {
            if v3 {
                push_header(out, '%', pairs.len());
            } else {
                push_header(out, '*', pairs.len() * 2);
            }
            for (k, v) in pairs {
                encode(k, proto, out);
                encode(v, proto, out);
            }
        }
    }
}

/// Encodes an error reply. An error is a single line, so any line break in
/// the message (a database error can carry one) becomes a space.
pub(crate) fn encode_error(msg: &str, out: &mut Vec<u8>) {
    out.push(b'-');
    out.extend(
        msg.bytes()
            .map(|b| if b == b'\r' || b == b'\n' { b' ' } else { b }),
    );
    out.extend_from_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(input: &[u8]) -> Result<Vec<Vec<Vec<u8>>>, ReadError> {
        let mut r = io::BufReader::new(input);
        let mut out = Vec::new();
        while let Some(cmd) = read_command(&mut r)? {
            out.push(cmd);
        }
        Ok(out)
    }

    fn words(cmd: &[Vec<u8>]) -> Vec<&[u8]> {
        cmd.iter().map(Vec::as_slice).collect()
    }

    fn protocol_error(input: &[u8]) -> String {
        match read_all(input) {
            Err(ReadError::Protocol(msg)) => msg,
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }

    #[test]
    fn multibulk_command() {
        let cmds = read_all(b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n").unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(words(&cmds[0]), [&b"GET"[..], b"k"]);
    }

    #[test]
    fn arguments_are_binary_safe() {
        let cmds =
            read_all(b"*3\r\n$3\r\nSET\r\n$4\r\nk\r\n\x00\r\n$5\r\n\xff\r\n\xfe\x00\r\n").unwrap();
        assert_eq!(
            words(&cmds[0]),
            [&b"SET"[..], b"k\r\n\x00", b"\xff\r\n\xfe\x00"]
        );
    }

    #[test]
    fn pipelined_and_empty_commands() {
        let cmds = read_all(b"PING\r\n\r\n*0\r\n*-1\r\n*1\r\n$4\r\nPING\r\nECHO hi\n").unwrap();
        assert_eq!(cmds.len(), 3);
        assert_eq!(words(&cmds[2]), [&b"ECHO"[..], b"hi"]);
    }

    #[test]
    fn inline_quoting() {
        let cmds = read_all(b"SET k \"hello world\" \r\nSET k 'a b'\r\n").unwrap();
        assert_eq!(words(&cmds[0]), [&b"SET"[..], b"k", b"hello world"]);
        assert_eq!(words(&cmds[1]), [&b"SET"[..], b"k", b"a b"]);
        assert_eq!(
            protocol_error(b"SET k \"open\r\n"),
            "Protocol error: unbalanced quotes in request"
        );
    }

    #[test]
    fn empty_bulk_argument() {
        let cmds = read_all(b"*2\r\n$3\r\nGET\r\n$0\r\n\r\n").unwrap();
        assert_eq!(words(&cmds[0]), [&b"GET"[..], b""]);
    }

    #[test]
    fn protocol_errors_match_redis() {
        assert_eq!(
            protocol_error(b"*abc\r\n"),
            "Protocol error: invalid multibulk length"
        );
        assert_eq!(
            protocol_error(b"*1048577\r\n"),
            "Protocol error: invalid multibulk length"
        );
        assert_eq!(
            protocol_error(b"*1\r\n:3\r\n"),
            "Protocol error: expected '$', got ':'"
        );
        assert_eq!(
            protocol_error(b"*1\r\n$-1\r\n"),
            "Protocol error: invalid bulk length"
        );
        assert_eq!(
            protocol_error(b"*1\r\n$536870913\r\n"),
            "Protocol error: invalid bulk length"
        );
        let long = vec![b'a'; MAX_LINE + 10];
        assert_eq!(
            protocol_error(&long),
            "Protocol error: too big inline request"
        );
    }

    #[test]
    fn truncated_input_is_an_io_error_not_a_command() {
        for input in [
            &b"*2\r\n$3\r\nGET\r\n"[..],
            b"*1\r\n$10\r\nshort",
            b"*1\r\n$3\r\nGET",
            b"PING",
        ] {
            assert!(
                matches!(read_all(input), Err(ReadError::Closed)),
                "{input:?}"
            );
        }
    }

    // A huge declared length must not allocate until the bytes arrive.
    #[test]
    fn large_declared_length_without_data() {
        assert!(matches!(
            read_all(b"*1\r\n$536870912\r\nabc"),
            Err(ReadError::Closed)
        ));
    }

    fn enc(reply: &Reply) -> Vec<u8> {
        let mut out = Vec::new();
        encode(reply, Proto::Resp2, &mut out);
        out
    }

    fn enc3(reply: &Reply) -> Vec<u8> {
        let mut out = Vec::new();
        encode(reply, Proto::Resp3, &mut out);
        out
    }

    #[test]
    fn resp3_encoding_differs_only_for_nil_sets_and_maps() {
        assert_eq!(enc3(&Reply::Simple("OK")), b"+OK\r\n");
        assert_eq!(enc3(&Reply::Int(3)), b":3\r\n");
        assert_eq!(enc3(&Reply::Bulk(b"v".to_vec())), b"$1\r\nv\r\n");
        assert_eq!(enc3(&Reply::Nil), b"_\r\n");
        let items = vec![b"a".to_vec(), b"b".to_vec()];
        assert_eq!(
            enc3(&Reply::Array(items.clone(), Empty::List)),
            b"*2\r\n$1\r\na\r\n$1\r\nb\r\n"
        );
        assert_eq!(
            enc3(&Reply::Array(items.clone(), Empty::Set)),
            b"~2\r\n$1\r\na\r\n$1\r\nb\r\n"
        );
        assert_eq!(
            enc3(&Reply::Array(items, Empty::Hash)),
            b"%1\r\n$1\r\na\r\n$1\r\nb\r\n"
        );
        assert_eq!(enc3(&Reply::Array(vec![], Empty::Hash)), b"%0\r\n");
        assert_eq!(
            enc3(&Reply::Lines(vec![Reply::Nil, Reply::Int(1)])),
            b"*2\r\n_\r\n:1\r\n"
        );
        let map = Reply::Map(vec![(Reply::Bulk(b"proto".to_vec()), Reply::Int(3))]);
        assert_eq!(enc3(&map), b"%1\r\n$5\r\nproto\r\n:3\r\n");
        assert_eq!(enc(&map), b"*2\r\n$5\r\nproto\r\n:3\r\n");
    }

    #[test]
    fn encoding() {
        assert_eq!(enc(&Reply::Simple("OK")), b"+OK\r\n");
        assert_eq!(enc(&Reply::Int(-7)), b":-7\r\n");
        assert_eq!(enc(&Reply::Bulk(b"a\r\nb".to_vec())), b"$4\r\na\r\nb\r\n");
        assert_eq!(enc(&Reply::Bulk(vec![])), b"$0\r\n\r\n");
        assert_eq!(enc(&Reply::Nil), b"$-1\r\n");
        assert_eq!(enc(&Reply::Array(vec![], Empty::List)), b"*0\r\n");
        assert_eq!(
            enc(&Reply::Array(vec![b"a".to_vec(), b"".to_vec()], Empty::Set)),
            b"*2\r\n$1\r\na\r\n$0\r\n\r\n"
        );
        assert_eq!(
            enc(&Reply::Lines(vec![
                Reply::Bulk(b"v".to_vec()),
                Reply::Nil,
                Reply::Int(2),
                Reply::Lines(vec![])
            ])),
            b"*4\r\n$1\r\nv\r\n$-1\r\n:2\r\n*0\r\n"
        );
    }

    #[test]
    fn errors_stay_on_one_line() {
        let mut out = Vec::new();
        encode_error("ERR database error: a\nb\r\nc", &mut out);
        assert_eq!(out, b"-ERR database error: a b  c\r\n");
    }
}
