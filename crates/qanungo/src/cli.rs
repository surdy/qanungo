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
//!
//! [`RedactionArgs`] was defined one lane early for the same reason and is now live: `qanungo
//! standup` (qanungo #9) is the first command that renders archived prose, so it is the first —
//! and so far only — command to flatten it. `report` and `cost` still do not, and attaching it to
//! them would be decoration over documents that carry no content at all (see
//! [`crate::redaction`]).

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, TimeDelta, Utc};
use clap::{Args, Parser, Subcommand};

use crate::redaction::{FILTER_PROFANITY_BY_DEFAULT, REDACT_SECRETS_BY_DEFAULT, Redactor};

/// The production archive's published front door (Caddy on 443; raw :8787 is firewalled to the
/// archive's own subnet). Same name session-recall uses. Overridable for a tunnel, a
/// laptop-local server, or a second archive.
pub const DEFAULT_PATWARI_URL: &str = "https://patwari.clusterfault.com";

/// How far back a standup reaches when nobody says otherwise.
///
/// **A tunable, not a decision.** A week is what "what have I been up to" usually means and what
/// a Monday standup covers, and it is short enough that the document stays readable when every
/// session in it is rendered in full — which is the property that separates this lane from
/// `report` and `cost`, whose output is a fixed size however long the window. The issue's own
/// text says `30d`; that produces a document nobody reads to the end of, so the default is a week
/// and `--last 30d` still says exactly what the issue asked for.
pub const DEFAULT_STANDUP_WINDOW: &str = "7d";

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
    /// Narrate recent archived sessions from their own summaries, as Markdown on stdout.
    Standup(StandupArgs),
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

/// How much of what a transcript said may reach the screen, shared by every lane that renders
/// transcript content. Flattened for the same reason [`ArchiveArgs`] is: `--no-redact` must mean
/// exactly one thing, everywhere, forever.
///
/// Both flags are opt-*in* to a change from the shipped default, so the safe reading is the one a
/// person gets by typing nothing:
///
/// - `--no-redact` turns the secrets pass off. Phrased as a negation on purpose — there is no
///   `--redact`, because redaction is not something a person should have to remember to ask for,
///   and the only way to lose it is to say so out loud in a command line somebody can read back.
/// - `--filter-profanity` turns the profanity pass on. Default off; see
///   [`crate::redaction::FILTER_PROFANITY_BY_DEFAULT`] for why that is a tunable rather than a
///   decision.
#[derive(Debug, Args)]
pub struct RedactionArgs {
    /// Render transcript content without scrubbing secrets. A deliberate choice for a trusted
    /// terminal, never the default.
    #[arg(long = "no-redact", default_value_t = !REDACT_SECRETS_BY_DEFAULT)]
    pub no_redact: bool,

    /// Mask a small conservative list of profane words in rendered transcript content.
    #[arg(long = "filter-profanity", default_value_t = FILTER_PROFANITY_BY_DEFAULT)]
    pub filter_profanity: bool,
}

impl RedactionArgs {
    /// The redactor these flags ask for.
    pub const fn redactor(&self) -> Redactor {
        Redactor::new()
            .with_secrets(!self.no_redact)
            .with_profanity(self.filter_profanity)
    }
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

/// The standup lane, and the first command to flatten [`RedactionArgs`].
///
/// Three flattened groups rather than three sets of near-identical flags: `--patwari-url` means
/// what it means in every other lane, and `--no-redact` will mean what it means in every lane
/// after this one.
#[derive(Debug, Args)]
pub struct StandupArgs {
    /// How far back to narrate, as `<count><unit>` with unit `h`, `d`, or `w`.
    #[arg(long = "last", default_value = DEFAULT_STANDUP_WINDOW, value_parser = parse_window)]
    pub last: Window,

    #[command(flatten)]
    pub archive: ArchiveArgs,

    #[command(flatten)]
    pub redaction: RedactionArgs,
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
        for command in ["report", "cost", "standup"] {
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

    /// The standup lane narrates one window a person can read to the end of. The issue's `30d` is
    /// still typeable; it is simply not what a run with no flags means.
    #[test]
    fn standup_defaults_to_a_week_of_the_lan_archive() {
        let Command::Standup(args) = Cli::parse_from(["qanungo", "standup"]).command else {
            panic!("`standup` parses as the standup command");
        };
        assert_eq!(args.last.to_string(), DEFAULT_STANDUP_WINDOW);
        assert_eq!(args.last.delta(), TimeDelta::days(7));
        assert_eq!(args.archive.patwari_url, DEFAULT_PATWARI_URL);
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(args.archive.cache_dir.is_none());

        let Command::Standup(month) =
            Cli::parse_from(["qanungo", "standup", "--last", "30d"]).command
        else {
            panic!("`standup` parses as the standup command");
        };
        assert_eq!(month.last.delta(), TimeDelta::days(30));
    }

    /// Every lane reaches the archive through the same flattened arguments, so a flag means the
    /// same thing wherever it is typed.
    #[test]
    fn every_lane_takes_the_same_archive_arguments() {
        let arguments = ["--patwari-url", "http://127.0.0.1:9", "--concurrency", "2"];
        let parse = |command: &str| {
            Cli::parse_from(
                ["qanungo", command]
                    .into_iter()
                    .chain(arguments)
                    .collect::<Vec<_>>(),
            )
            .command
        };
        let (Command::Report(report), Command::Cost(cost), Command::Standup(standup)) =
            (parse("report"), parse("cost"), parse("standup"))
        else {
            panic!("each subcommand parses as itself");
        };
        for archive in [&cost.archive, &standup.archive] {
            assert_eq!(report.archive.patwari_url, archive.patwari_url);
            assert_eq!(report.archive.concurrency, archive.concurrency);
        }
    }

    /// The redaction flags go live on `standup` and nowhere else: `report` and `cost` render no
    /// archived prose, so a `--no-redact` on either would be a switch over nothing.
    #[test]
    fn only_the_lane_that_renders_prose_takes_the_redaction_flags() {
        assert!(Cli::try_parse_from(["qanungo", "standup", "--no-redact"]).is_ok());
        assert!(Cli::try_parse_from(["qanungo", "report", "--no-redact"]).is_err());
        assert!(Cli::try_parse_from(["qanungo", "cost", "--filter-profanity"]).is_err());
    }

    /// [`RedactionArgs`] is exercised through the same stand-in the redaction lane pinned it with,
    /// so the flag surface stays pinned independently of which commands happen to flatten it.
    #[derive(Debug, Parser)]
    struct RenderingLane {
        #[command(flatten)]
        redaction: RedactionArgs,
    }

    #[test]
    fn the_redaction_flags_are_well_formed() {
        RenderingLane::command().debug_assert();
    }

    /// What `standup` builds out of the flags is the same redactor the stand-in builds out of
    /// them: typing nothing scrubs secrets and leaves swearing alone.
    #[test]
    fn the_standup_lane_redacts_by_default_and_stops_only_when_told_to() {
        let redactor = |flags: &[&str]| {
            let Command::Standup(args) = Cli::parse_from(
                ["qanungo", "standup"]
                    .into_iter()
                    .chain(flags.iter().copied())
                    .collect::<Vec<_>>(),
            )
            .command
            else {
                panic!("`standup` parses as the standup command");
            };
            args.redaction.redactor()
        };
        assert_eq!(redactor(&[]), crate::redaction::Redactor::new());
        assert!(redactor(&[]).redacts_secrets());
        assert!(!redactor(&[]).filters_profanity());
        assert!(!redactor(&["--no-redact"]).redacts_secrets());
        assert!(redactor(&["--filter-profanity"]).filters_profanity());
    }

    /// Typing nothing must be the safe reading: secrets scrubbed, profanity left alone.
    #[test]
    fn rendering_defaults_to_redacting_secrets_and_not_filtering_profanity() {
        let lane = RenderingLane::parse_from(["lane"]);
        assert!(!lane.redaction.no_redact);
        assert!(!lane.redaction.filter_profanity);
        let redactor = lane.redaction.redactor();
        assert!(redactor.redacts_secrets());
        assert!(!redactor.filters_profanity());
        assert_eq!(redactor, crate::redaction::Redactor::new());
    }

    /// The two passes are switched independently on the command line as well as in the library:
    /// four flag combinations, four distinct redactors.
    #[test]
    fn the_two_redaction_flags_move_independently() {
        let cases = [
            (vec![], true, false),
            (vec!["--no-redact"], false, false),
            (vec!["--filter-profanity"], true, true),
            (vec!["--no-redact", "--filter-profanity"], false, true),
        ];
        for (flags, secrets, profanity) in cases {
            let lane = RenderingLane::parse_from(["lane"].into_iter().chain(flags.iter().copied()));
            let redactor = lane.redaction.redactor();
            assert_eq!(redactor.redacts_secrets(), secrets, "{flags:?}");
            assert_eq!(redactor.filters_profanity(), profanity, "{flags:?}");
        }
    }

    /// There is no `--redact`: redaction is not a thing to remember to ask for, and losing it has
    /// to be said out loud.
    #[test]
    fn there_is_no_flag_that_turns_redaction_on() {
        assert!(RenderingLane::try_parse_from(["lane", "--redact"]).is_err());
        assert!(RenderingLane::try_parse_from(["lane", "--no-filter-profanity"]).is_err());
    }
}
