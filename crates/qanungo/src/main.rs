//! The `qanungo` binary.
//!
//! Markdown goes to stdout so a report composes with a pager, a file, or a `/standup` skill;
//! diagnostics go to stderr, and a failure exits non-zero with the whole error chain rather than
//! only its outermost sentence.
//!
//! `dashboard` is the one subcommand that writes no document: it serves one, and every line it
//! prints — the posture statement, the refresh instrumentation, the access log — is a diagnostic
//! and goes to stderr with the rest.

use std::error::Error;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;
use qanungo::cli::{Cli, Command};
use qanungo::{command, dashboard_server};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(error) = run(&cli.command) {
        let mut message = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            message.push_str(&format!("\n  caused by: {cause}"));
            source = cause.source();
        }
        eprintln!("qanungo: {message}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Dispatches one subcommand.
///
/// The document lanes share the locked stdout and the flush that follows them; the dashboard
/// returns from here only when it fails to start, because serving is the rest of the process's
/// life.
fn run(command: &Command) -> Result<(), Box<dyn Error>> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match command {
        Command::Report(args) => command::report(args, &mut out)?,
        Command::Cost(args) => command::cost(args, &mut out)?,
        Command::Standup(args) => command::standup(args, &mut out)?,
        Command::Dashboard(args) => return Ok(dashboard_server::run(args)?),
    }
    out.flush().map_err(command::CommandError::Output)?;
    Ok(())
}
