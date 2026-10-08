//! Reply: what a command computes, decoupled from how it is rendered, plus the
//! renderers (human/raw/json) that turn a Reply into output bytes.

use crate::cli::OutputFormat;

/// Which placeholder an empty array renders as in human output.
#[derive(Clone, Copy)]
pub(crate) enum Empty {
    List,
    Set,
    Hash,
}

/// Typed command result. Commands compute a Reply; renderers turn it into
/// output. Values are bytes: the CLI only ever produces UTF-8, but the
/// storage layer and a future RESP server are binary-safe.
pub(crate) enum Reply {
    /// Status line printed bare ("OK", type names).
    Simple(&'static str),
    /// Integer, printed as "(integer) N".
    Int(i64),
    /// A value, printed bare.
    Bulk(Vec<u8>),
    /// Missing value, printed as "(nil)".
    Nil,
    /// Items printed as a numbered, quoted list; the Empty kind picks the
    /// "(empty list)"/"(empty set)"/"(empty hash)" placeholder.
    Array(Vec<Vec<u8>>, Empty),
    /// One reply per line without numbering (MGET).
    Lines(Vec<Reply>),
}

/// Command failure: the message is printed to stderr and the process exits 1.
pub(crate) struct CmdError(pub(crate) String);

impl CmdError {
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        CmdError(msg.into())
    }
}

impl From<rusqlite::Error> for CmdError {
    fn from(e: rusqlite::Error) -> Self {
        CmdError(format!("ERR database error: {e}"))
    }
}

pub(crate) type CmdResult = Result<Reply, CmdError>;

fn push_lossy(out: &mut Vec<u8>, v: &[u8]) {
    out.extend_from_slice(String::from_utf8_lossy(v).as_bytes());
}

pub(crate) fn render_human(reply: &Reply, out: &mut Vec<u8>) {
    match reply {
        Reply::Simple(s) => {
            out.extend_from_slice(s.as_bytes());
            out.push(b'\n');
        }
        Reply::Int(n) => out.extend_from_slice(format!("(integer) {n}\n").as_bytes()),
        Reply::Bulk(v) => {
            push_lossy(out, v);
            out.push(b'\n');
        }
        Reply::Nil => out.extend_from_slice(b"(nil)\n"),
        Reply::Array(items, empty) => {
            if items.is_empty() {
                out.extend_from_slice(match empty {
                    Empty::List => b"(empty list)\n",
                    Empty::Set => b"(empty set)\n",
                    Empty::Hash => b"(empty hash)\n",
                });
            } else {
                for (i, item) in items.iter().enumerate() {
                    out.extend_from_slice(format!("{}) \"", i + 1).as_bytes());
                    push_lossy(out, item);
                    out.extend_from_slice(b"\"\n");
                }
            }
        }
        Reply::Lines(replies) => {
            for r in replies {
                render_human(r, out);
            }
        }
    }
}

/// Raw mode writes values byte-for-byte, so binary payloads survive a pipe.
pub(crate) fn render_raw(reply: &Reply, out: &mut Vec<u8>) {
    match reply {
        Reply::Simple(s) => {
            out.extend_from_slice(s.as_bytes());
            out.push(b'\n');
        }
        Reply::Int(n) => {
            out.extend_from_slice(n.to_string().as_bytes());
            out.push(b'\n');
        }
        Reply::Bulk(v) => {
            out.extend_from_slice(v);
            out.push(b'\n');
        }
        Reply::Nil => out.push(b'\n'),
        Reply::Array(items, _) => {
            for item in items {
                out.extend_from_slice(item);
                out.push(b'\n');
            }
        }
        Reply::Lines(replies) => {
            for r in replies {
                render_raw(r, out);
            }
        }
    }
}

fn push_json_string(bytes: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    for c in String::from_utf8_lossy(bytes).chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes())
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

pub(crate) fn render_json(reply: &Reply, out: &mut Vec<u8>) {
    match reply {
        Reply::Simple(s) => push_json_string(s.as_bytes(), out),
        Reply::Int(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Reply::Bulk(v) => push_json_string(v, out),
        Reply::Nil => out.extend_from_slice(b"null"),
        // Alternating field/value items (HGETALL) become a JSON object.
        Reply::Array(items, Empty::Hash) => {
            out.push(b'{');
            for (i, pair) in items.chunks(2).enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                push_json_string(&pair[0], out);
                out.push(b':');
                push_json_string(pair.get(1).map_or(&[][..], |v| v), out);
            }
            out.push(b'}');
        }
        Reply::Array(items, _) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                push_json_string(item, out);
            }
            out.push(b']');
        }
        Reply::Lines(replies) => {
            out.push(b'[');
            for (i, r) in replies.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                render_json(r, out);
            }
            out.push(b']');
        }
    }
}

pub(crate) fn render(reply: &Reply, format: OutputFormat) -> Vec<u8> {
    let mut out = Vec::new();
    match format {
        OutputFormat::Human => render_human(reply, &mut out),
        OutputFormat::Raw => render_raw(reply, &mut out),
        OutputFormat::Json => {
            render_json(reply, &mut out);
            out.push(b'\n');
        }
    }
    out
}
