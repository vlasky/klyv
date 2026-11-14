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

fn main() {
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
