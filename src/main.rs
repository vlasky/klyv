mod cli;
mod commands;
mod db;
mod reply;
mod shell;

use clap::Parser;
use cli::{Cli, dispatch};
use db::open_db;
use reply::{CmdError, render};
use shell::{pipe, repl};
use std::io::IsTerminal;

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
        Some(command) => match dispatch(&mut conn, command) {
            Ok(reply) => print!("{}", render(&reply, cli.format)),
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
