#![forbid(unsafe_code)]

mod command;
mod render;

use clap::Parser;

fn main() {
    let json = std::env::args_os().any(|argument| argument == "--json");
    match command::Cli::try_parse() {
        Ok(cli) => std::process::exit(command::run(cli).code()),
        Err(_) if json => std::process::exit(command::render_argument_failure().code()),
        Err(error) => error.exit(),
    }
}
