//! Interactive REPL and stdin pipe mode: a session loop over one connection.

use crate::cli::{LineInput, OutputFormat, dispatch};
use crate::reply::{CmdError, render};
use clap::Parser;
use rusqlite::Connection;
use std::io::{BufRead, Write};

pub(crate) enum LineOutcome {
    Ok,
    Failed,
    Quit,
}

/// Shell-style tokenizer for shell/pipe input: words split on unquoted
/// whitespace, with single quotes (literal), double quotes (with \" and \\
/// escapes), and bare backslash escapes. Unlike a real shell — and unlike the
/// shlex crate, which this replaced — `#` has no special meaning: in a data
/// store it is data, never a comment. Returns None on an unterminated quote
/// or trailing backslash.
pub(crate) fn split_line(line: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut it = line.chars();
    while let Some(c) = it.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    tokens.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match it.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return None,
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match it.next() {
                        Some('"') => break,
                        Some('\\') => match it.next() {
                            Some(e @ ('"' | '\\')) => cur.push(e),
                            Some(e) => {
                                cur.push('\\');
                                cur.push(e);
                            }
                            None => return None,
                        },
                        Some(c) => cur.push(c),
                        None => return None,
                    }
                }
            }
            '\\' => {
                in_word = true;
                cur.push(it.next()?);
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        tokens.push(cur);
    }
    Some(tokens)
}

/// Tokenizes and executes one line of shell/pipe input against the open
/// connection. Every error is recoverable: the session continues.
pub(crate) fn run_line(conn: &mut Connection, line: &str, format: OutputFormat) -> LineOutcome {
    let Some(tokens) = split_line(line) else {
        eprintln!("ERR unbalanced quotes in input");
        return LineOutcome::Failed;
    };
    if tokens.is_empty() {
        return LineOutcome::Ok;
    }
    if tokens.len() == 1 && ["exit", "quit"].contains(&tokens[0].to_lowercase().as_str()) {
        return LineOutcome::Quit;
    }
    match LineInput::try_parse_from(&tokens) {
        Ok(input) => match dispatch(conn, input.command) {
            Ok(reply) => {
                let _ = std::io::stdout().write_all(&render(&reply, format));
                LineOutcome::Ok
            }
            Err(CmdError(msg)) => {
                eprintln!("{msg}");
                LineOutcome::Failed
            }
        },
        Err(e) => {
            // Prints help/version requests to stdout, parse errors to stderr.
            let _ = e.print();
            if e.use_stderr() {
                LineOutcome::Failed
            } else {
                LineOutcome::Ok
            }
        }
    }
}

/// Non-interactive loop: executes commands from stdin, one per line, over a
/// single connection (one process, one DB open). Failing lines report to
/// stderr and processing continues; the exit code is 1 if any line failed.
pub(crate) fn pipe(conn: &mut Connection, format: OutputFormat) -> i32 {
    let mut failed = false;
    for line in std::io::stdin().lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("ERR reading stdin: {e}");
                return 1;
            }
        };
        match run_line(conn, &line, format) {
            LineOutcome::Ok => {}
            LineOutcome::Failed => failed = true,
            LineOutcome::Quit => break,
        }
    }
    if failed { 1 } else { 0 }
}

/// Interactive shell with line editing and in-session history.
pub(crate) fn repl(conn: &mut Connection, format: OutputFormat) -> i32 {
    let mut rl = match rustyline::DefaultEditor::new() {
        Ok(rl) => rl,
        Err(e) => {
            eprintln!("ERR failed to initialize line editor: {e}");
            return 1;
        }
    };
    println!(
        "klyv {} — 'help' lists commands, 'exit' or Ctrl-D quits",
        env!("CARGO_PKG_VERSION")
    );
    loop {
        match rl.readline("klyv> ") {
            Ok(line) => {
                if !line.trim().is_empty() {
                    let _ = rl.add_history_entry(&line);
                }
                // Interactive errors are shown, not fatal, and don't affect
                // the exit code.
                if matches!(run_line(conn, &line, format), LineOutcome::Quit) {
                    return 0;
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => return 0,
            Err(e) => {
                eprintln!("ERR reading input: {e}");
                return 1;
            }
        }
    }
}
