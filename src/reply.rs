//! Reply: what a command computes, decoupled from how it is rendered, plus the
//! renderers (human/raw/json) that turn a Reply into output.

use crate::cli::OutputFormat;

/// Which placeholder an empty array renders as in human output.
#[derive(Clone, Copy)]
pub(crate) enum Empty {
    List,
    Set,
    Hash,
}

/// Typed command result. Commands compute a Reply; renderers turn it into
/// output (today: the human redis-cli format; future: RESP, JSON, raw).
pub(crate) enum Reply {
    /// Status line printed bare ("OK", type names).
    Simple(&'static str),
    /// Integer, printed as "(integer) N".
    Int(i64),
    /// A value, printed bare.
    Bulk(String),
    /// Missing value, printed as "(nil)".
    Nil,
    /// Items printed as a numbered, quoted list; the Empty kind picks the
    /// "(empty list)"/"(empty set)"/"(empty hash)" placeholder.
    Array(Vec<String>, Empty),
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

pub(crate) fn render_human(reply: &Reply, out: &mut String) {
    match reply {
        Reply::Simple(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Reply::Int(n) => {
            out.push_str(&format!("(integer) {n}\n"));
        }
        Reply::Bulk(v) => {
            out.push_str(v);
            out.push('\n');
        }
        Reply::Nil => out.push_str("(nil)\n"),
        Reply::Array(items, empty) => {
            if items.is_empty() {
                out.push_str(match empty {
                    Empty::List => "(empty list)\n",
                    Empty::Set => "(empty set)\n",
                    Empty::Hash => "(empty hash)\n",
                });
            } else {
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&format!("{}) \"{item}\"\n", i + 1));
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

pub(crate) fn render_raw(reply: &Reply, out: &mut String) {
    match reply {
        Reply::Simple(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Reply::Int(n) => {
            out.push_str(&n.to_string());
            out.push('\n');
        }
        Reply::Bulk(v) => {
            out.push_str(v);
            out.push('\n');
        }
        Reply::Nil => out.push('\n'),
        Reply::Array(items, _) => {
            for item in items {
                out.push_str(item);
                out.push('\n');
            }
        }
        Reply::Lines(replies) => {
            for r in replies {
                render_raw(r, out);
            }
        }
    }
}

pub(crate) fn push_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub(crate) fn render_json(reply: &Reply, out: &mut String) {
    match reply {
        Reply::Simple(s) => push_json_string(s, out),
        Reply::Int(n) => out.push_str(&n.to_string()),
        Reply::Bulk(v) => push_json_string(v, out),
        Reply::Nil => out.push_str("null"),
        // Alternating field/value items (HGETALL) become a JSON object.
        Reply::Array(items, Empty::Hash) => {
            out.push('{');
            for (i, pair) in items.chunks(2).enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_string(&pair[0], out);
                out.push(':');
                push_json_string(pair.get(1).map_or("", |v| v), out);
            }
            out.push('}');
        }
        Reply::Array(items, _) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_string(item, out);
            }
            out.push(']');
        }
        Reply::Lines(replies) => {
            out.push('[');
            for (i, r) in replies.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                render_json(r, out);
            }
            out.push(']');
        }
    }
}

pub(crate) fn render(reply: &Reply, format: OutputFormat) -> String {
    let mut out = String::new();
    match format {
        OutputFormat::Human => render_human(reply, &mut out),
        OutputFormat::Raw => render_raw(reply, &mut out),
        OutputFormat::Json => {
            render_json(reply, &mut out);
            out.push('\n');
        }
    }
    out
}
