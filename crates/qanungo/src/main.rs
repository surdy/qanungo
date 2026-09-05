//! The `qanungo` binary.
//!
//! Markdown goes to stdout so a report composes with a pager, a file, or a `/standup` skill;
//! diagnostics go to stderr, and a failure exits non-zero with the whole error chain rather than
//! only its outermost sentence.
//!
//! `dashboard` is the one subcommand that writes no document: it serves one, and every line it
//! prints — the posture statement, the refresh instrumentation, the access log — is a diagnostic
//! and goes to stderr with the rest.
//!
//! One usage error is answered in prose rather than in clap's shorthand: a run with no archive URL
//! is not a typo, it is an install that has not been finished, so [`parse`] catches that single
//! case and prints [`qanungo::cli::MISSING_PATWARI_URL`] instead.

use std::error::Error;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use qanungo::cli::{Cli, Command, MISSING_PATWARI_URL};
use qanungo::{command, dashboard_server};

fn main() -> ExitCode {
    let cli = match parse() {
        Ok(cli) => cli,
        Err(code) => return code,
    };
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

/// Parses the command line, or exits the way clap would — except for the one error a person is
/// most likely to meet on their first run.
///
/// A missing `--patwari-url` is the difference between "you typed the flag wrong" and "qanungo
/// does not know where your archive is", and clap's one-line rendering cannot tell a reader the
/// second thing: there is no default to fall back to, the answer is a URL only they have, and the
/// environment variable that saves them typing it again is not visible in a usage line. Every
/// other parse failure — including `--help` and `--version`, which clap reports as errors — is
/// still clap's to print, unchanged.
fn parse() -> Result<Cli, ExitCode> {
    match Cli::try_parse() {
        Ok(cli) => Ok(cli),
        Err(error) if only_the_archive_url_is_missing(&error) => {
            eprintln!("qanungo: {MISSING_PATWARI_URL}");
            // The code clap itself uses for a usage error, so a script cannot tell the friendlier
            // sentence apart from the parser's own.
            Err(ExitCode::from(2))
        }
        Err(error) => {
            // `--help` and `--version` are "errors" that belong on stdout with a zero exit; clap's
            // own exit knows which is which.
            error.exit()
        }
    }
}

/// Whether the archive URL is the *only* thing the parser found missing.
///
/// Deliberately narrow. If a run is also missing `ask`'s query, clap's own line is the one that
/// names both, and swapping it for a paragraph about the archive would hide half the problem — so
/// the friendlier sentence is printed only when it is the whole story.
fn only_the_archive_url_is_missing(error: &clap::Error) -> bool {
    error.kind() == ErrorKind::MissingRequiredArgument
        && matches!(
            error.get(ContextKind::InvalidArg),
            Some(ContextValue::Strings(missing))
                if missing.len() == 1 && missing[0].starts_with("--patwari-url")
        )
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
        Command::Ask(args) => command::ask(args, &mut out)?,
        Command::Doctor(args) => command::doctor(args, &mut out)?,
        Command::Flows(args) => command::flows(args, &mut out)?,
        Command::Dashboard(args) => return Ok(dashboard_server::run(args)?),
        Command::Rules(args) => command::rules(args, &mut out)?,
    }
    out.flush().map_err(command::CommandError::Output)?;
    Ok(())
}
