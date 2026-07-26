#![forbid(unsafe_code)]

mod command;
mod render;

use clap::{Parser, error::ErrorKind};

fn main() {
    match command::Cli::try_parse() {
        Ok(cli) => std::process::exit(command::run(cli).code()),
        Err(error)
            if !matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) && std::env::args_os().any(|argument| argument == "--json") =>
        {
            std::process::exit(command::render_argument_failure().code())
        }
        Err(error) => error.exit(),
    }
}
