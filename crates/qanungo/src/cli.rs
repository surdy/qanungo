//! The command-line surface.
//!
//! P0 is one command — `qanungo report` — because the vertical slice is the point: sync, fold,
//! evaluate, emit, all in one invocation, with nothing between the archive and the Markdown on
//! stdout.

use std::fmt;
use std::path::PathBuf;

use chrono::TimeDelta;
use clap::{Args, Parser, Subcommand};

/// The production archive's published front door (Caddy on 443; raw :8787 is firewalled to the
/// archive's own subnet). Same name session-recall uses. Overridable for a tunnel, a
/// laptop-local server, or a second archive.
pub const DEFAULT_PATWARI_URL: &str = "https://patwari.clusterfault.com";

#[derive(Debug, Parser)]
#[command(
    name = "qanungo",
    version,
    about = "Read-side coaching client over the Patwari archive"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Fold recent archived sessions into a Markdown coaching report on stdout.
    Report(ReportArgs),
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// How far back to report, as `<count><unit>` with unit `h`, `d`, or `w`.
    #[arg(long = "last", default_value = "30d", value_parser = parse_window)]
    pub last: Window,

    /// Base URL of the Patwari archive server.
    #[arg(long = "patwari-url", env = "PATWARI_URL", default_value = DEFAULT_PATWARI_URL)]
    pub patwari_url: String,

    /// Cache root for the mirrored transcripts. Defaults to `$XDG_CACHE_HOME/qanungo`, falling
    /// back to `~/.cache/qanungo`.
    #[arg(long = "cache-dir")]
    pub cache_dir: Option<PathBuf>,

    /// Concurrent requests against the archive, 1 to 8. Kept small on purpose: Patwari is a LAN
    /// server with a modest concurrency limit, and occupying every slot it has does not make a
    /// report faster — it starves the archive's other readers.
    #[arg(long, default_value_t = crate::sync::DEFAULT_CONCURRENCY, value_parser = parse_concurrency)]
    pub concurrency: usize,
}

/// A report window, kept in the spelling the operator typed so the report can echo it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    text: String,
    delta: TimeDelta,
}

impl Window {
    /// How far back the window reaches.
    pub const fn delta(&self) -> TimeDelta {
        self.delta
    }
}

impl fmt::Display for Window {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

/// Refuses a worker count outside `1..=`[`crate::sync::MAX_CONCURRENCY`] rather than silently
/// clamping it: somebody who typed `--concurrency 64` has a belief about what the tool will do,
/// and the honest answer is that this client will not do it.
fn parse_concurrency(value: &str) -> Result<usize, String> {
    let count: usize = value
        .parse()
        .map_err(|_| format!("`{value}` is not a worker count"))?;
    if !(1..=crate::sync::MAX_CONCURRENCY).contains(&count) {
        return Err(format!(
            "must be between 1 and {} — Patwari is a LAN archive with a modest concurrency limit",
            crate::sync::MAX_CONCURRENCY,
        ));
    }
    Ok(count)
}

/// Parses `30d`, `12h`, `2w`. Deliberately not a general duration grammar: a coaching window is
/// always a round number of hours, days, or weeks, and a parser that also accepted `90m` would
/// invite windows too short for any of the metrics to mean anything.
fn parse_window(value: &str) -> Result<Window, String> {
    let (digits, unit) = value.split_at(
        value
            .find(|character: char| !character.is_ascii_digit())
            .ok_or_else(|| format!("`{value}` has no unit; try `30d`"))?,
    );
    let count: i64 = digits
        .parse()
        .map_err(|_| format!("`{value}` does not start with a count; try `30d`"))?;
    if count == 0 {
        return Err("a window of zero covers nothing".to_owned());
    }
    let delta = match unit {
        "h" => TimeDelta::try_hours(count),
        "d" => TimeDelta::try_days(count),
        "w" => TimeDelta::try_weeks(count),
        other => return Err(format!("`{other}` is not a window unit; use h, d, or w")),
    }
    .ok_or_else(|| format!("`{value}` is too long a window"))?;
    Ok(Window {
        text: value.to_owned(),
        delta,
    })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn parses_hour_day_and_week_windows() {
        assert_eq!(parse_window("12h").unwrap().delta(), TimeDelta::hours(12));
        assert_eq!(parse_window("30d").unwrap().delta(), TimeDelta::days(30));
        assert_eq!(parse_window("2w").unwrap().delta(), TimeDelta::weeks(2));
        assert_eq!(parse_window("30d").unwrap().to_string(), "30d");
    }

    #[test]
    fn rejects_windows_that_would_report_on_nothing() {
        for bad in ["", "d", "30", "30m", "0d", "-1d", "30days"] {
            assert!(parse_window(bad).is_err(), "`{bad}` must not parse");
        }
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn concurrency_past_the_archives_capacity_is_refused_not_clamped() {
        assert_eq!(parse_concurrency("1").unwrap(), 1);
        assert_eq!(
            parse_concurrency("8").unwrap(),
            crate::sync::MAX_CONCURRENCY
        );
        for bad in ["0", "9", "64", "-1", "many"] {
            assert!(parse_concurrency(bad).is_err(), "`{bad}` must be refused");
        }
        assert!(Cli::try_parse_from(["qanungo", "report", "--concurrency", "64"]).is_err());
    }

    #[test]
    fn report_defaults_to_the_lan_archive_over_thirty_days() {
        let Command::Report(args) = Cli::parse_from(["qanungo", "report"]).command;
        assert_eq!(args.last.to_string(), "30d");
        assert_eq!(args.patwari_url, DEFAULT_PATWARI_URL);
        assert_eq!(args.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(args.cache_dir.is_none());
    }
}
