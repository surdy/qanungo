//! The command-line surface.
//!
//! P0 was one command — `qanungo report` — because the vertical slice was the point: sync, fold,
//! evaluate, emit, all in one invocation, with nothing between the archive and the Markdown on
//! stdout. `qanungo cost` (qanungo #12) is the second lane over the same slice: the same mirror,
//! the same blob cache, the same [`Window`] and its comparison window, a different fold and a
//! different document.
//!
//! Everything both commands need to reach the archive lives in [`ArchiveArgs`] and is flattened
//! into each of them, so a flag means the same thing wherever it is typed and a third lane
//! inherits it by declaring one field.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, TimeDelta, Utc};
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
    /// Fold recent archived sessions into a Markdown token/cost breakdown on stdout.
    Cost(CostArgs),
}

/// How to reach the archive, shared by every lane. Flattened rather than repeated so
/// `--patwari-url` cannot come to mean two slightly different things in two subcommands.
#[derive(Debug, Args)]
pub struct ArchiveArgs {
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

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// How far back to report, as `<count><unit>` with unit `h`, `d`, or `w`.
    #[arg(long = "last", default_value = "30d", value_parser = parse_window)]
    pub last: Window,

    #[command(flatten)]
    pub archive: ArchiveArgs,
}

#[derive(Debug, Args)]
pub struct CostArgs {
    /// How far back to price, as `<count><unit>` with unit `h`, `d`, or `w`. The default is a
    /// quarter, spelled `12w`: qanungo #12 asks for "3m", and the window grammar deliberately has
    /// no month unit — `m` reads as either minutes or months, and a coaching window that could be
    /// misread by a factor of forty thousand is worse than one that has to be spelled in weeks.
    #[arg(long = "last", default_value = "12w", value_parser = parse_window)]
    pub last: Window,

    #[command(flatten)]
    pub archive: ArchiveArgs,
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

    /// The instant the reported window opens.
    pub fn opens_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - self.delta
    }

    /// The instant the *comparison* window opens: one further window back, so the two are equal
    /// in length and adjacent. `None` when doubling the window would overflow — a report can then
    /// still be written, it simply carries no trend arrows.
    ///
    /// This lives here rather than in the report or the command because three places need the
    /// same boundary and they must agree exactly: the mirror lists from it, the fold partitions
    /// on it, and the report labels the two windows with it.
    pub fn comparison_opens_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        Some(now - self.delta.checked_mul(2)?)
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

    /// The comparison window is the equal-length one immediately before the reported one: two
    /// adjacent halves of twice the window, with no gap and no overlap between them.
    #[test]
    fn the_comparison_window_is_adjacent_and_equal_in_length() {
        let now = DateTime::parse_from_rfc3339("2026-08-18T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let window = parse_window("30d").unwrap();
        let opens = window.opens_at(now);
        let compares = window.comparison_opens_at(now).unwrap();
        assert_eq!(opens, now - TimeDelta::days(30));
        assert_eq!(compares, now - TimeDelta::days(60));
        assert_eq!(opens - compares, now - opens);
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
        for command in ["report", "cost"] {
            assert!(Cli::try_parse_from(["qanungo", command, "--concurrency", "64"]).is_err());
        }
    }

    #[test]
    fn report_defaults_to_the_lan_archive_over_thirty_days() {
        let Command::Report(args) = Cli::parse_from(["qanungo", "report"]).command else {
            panic!("`report` parses as the report command");
        };
        assert_eq!(args.last.to_string(), "30d");
        assert_eq!(args.archive.patwari_url, DEFAULT_PATWARI_URL);
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(args.archive.cache_dir.is_none());
    }

    /// The cost lane's default is a quarter spelled in the units the grammar actually has. The
    /// month unit the issue asked for is *not* accepted, here as anywhere else: `12w` is a window
    /// a reader can only read one way.
    #[test]
    fn cost_defaults_to_a_quarter_spelled_in_weeks() {
        let Command::Cost(args) = Cli::parse_from(["qanungo", "cost"]).command else {
            panic!("`cost` parses as the cost command");
        };
        assert_eq!(args.last.to_string(), "12w");
        assert_eq!(args.last.delta(), TimeDelta::weeks(12));
        assert_eq!(args.archive.patwari_url, DEFAULT_PATWARI_URL);
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(Cli::try_parse_from(["qanungo", "cost", "--last", "3m"]).is_err());
    }

    /// Both lanes reach the archive through the same flattened arguments, so a flag means the
    /// same thing wherever it is typed.
    #[test]
    fn every_lane_takes_the_same_archive_arguments() {
        let arguments = ["--patwari-url", "http://127.0.0.1:9", "--concurrency", "2"];
        let report = Cli::parse_from(
            ["qanungo", "report"]
                .into_iter()
                .chain(arguments)
                .collect::<Vec<_>>(),
        );
        let cost = Cli::parse_from(
            ["qanungo", "cost"]
                .into_iter()
                .chain(arguments)
                .collect::<Vec<_>>(),
        );
        let (Command::Report(report), Command::Cost(cost)) = (report.command, cost.command) else {
            panic!("each subcommand parses as itself");
        };
        assert_eq!(report.archive.patwari_url, cost.archive.patwari_url);
        assert_eq!(report.archive.concurrency, cost.archive.concurrency);
    }
}
