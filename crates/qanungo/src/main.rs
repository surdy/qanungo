//! The `qanungo` binary.
//!
//! Markdown goes to stdout so the report composes with a pager, a file, or a future `/standup`
//! skill; diagnostics go to stderr, and a failure exits non-zero with the whole error chain
//! rather than only its outermost sentence.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;
use qanungo::cli::{Cli, Command};
use qanungo::command;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let result = match &cli.command {
        Command::Report(args) => command::report(args, &mut out),
    };
    if let Err(error) = result.and_then(|()| out.flush().map_err(command::CommandError::Output)) {
        let mut message = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            message.push_str(&format!("\n  caused by: {cause}"));
            source = cause.source();
        }
        eprintln!("qanungo: {message}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
