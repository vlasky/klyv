mod cli;
mod commands;
mod db;
mod reply;
mod resp;
mod server;
mod shell;

use clap::Parser;
use cli::{Cli, TopCommand, dispatch};
use db::open_db;
use reply::{CmdError, render};
use shell::{pipe, repl};
use std::io::{IsTerminal, Write};

/// Stack for the thread that does all the work. The main thread's stack is
/// whatever the platform gives it (1 MB on Windows), and a debug build of
/// the argument parser for this many subcommands needs more than that; the
/// release build needs far less. Running on a thread of known size removes
/// the dependence on the platform default.
const STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() {
    let worker = std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(run)
        .expect("failed to start the main thread");
    // `run` normally ends the process itself; reaching here means it
    // returned or panicked.
    if worker.join().is_err() {
        std::process::exit(101);
    }
}

fn run() {
    let cli = Cli::parse();
    let mut conn = match open_db(&cli.db) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("ERR failed to open database {}: {e}", cli.db.display());
            std::process::exit(1);
        }
    };
    match cli.command {
        Some(TopCommand::Serve(args)) => {
            // Opening above created or migrated the file; each client
            // connection opens its own handle.
            drop(conn);
            std::process::exit(server::serve(&cli.db, args));
        }
        Some(TopCommand::Data(command)) => match dispatch(&mut conn, command) {
            Ok(reply) => {
                let _ = std::io::stdout().write_all(&render(&reply, cli.format));
            }
            Err(CmdError(msg)) => {
                eprintln!("{msg}");
                std::process::exit(1);
            }
        },
        // No subcommand: interactive shell on a terminal, pipe mode otherwise.
        None => {
            let code = if std::io::stdin().is_terminal() {
                repl(&mut conn, cli.format)
            } else {
                pipe(&mut conn, cli.format)
            };
            std::process::exit(code);
        }
    }
}
