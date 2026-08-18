//! The `report` command: the vertical slice's spine.
//!
//! sync → fold → evaluate → emit, in one pass, with the fold timed separately from the network
//! so the instrumentation footer measures what it claims to measure.
//!
//! # Two windows, one pass
//!
//! `--last 30d` mirrors **sixty** days and folds both halves: the reported window, and the equal
//! length immediately before it that the trend arrows are taken against. There is no store to read
//! last month's numbers out of, and there deliberately is not one (qanungo ADR 0001) — every run
//! recomputes all of it with the current rule pack, which is exactly what makes an arrow mean
//! behaviour drift rather than rule drift.
//!
//! The two halves are cut on **archive time** — the `completed_at` of the snapshot the session was
//! listed by — because that is the clock `activity_from` already selected on. Cutting on transcript
//! time instead would let a session satisfy the listing and land in neither half. The cost of that
//! choice is stated in the report: a long-lived transcript resumed across the boundary is archived
//! again, so it appears in the later window only, carrying its earlier work with it.

use std::collections::BTreeMap;
use std::io::{self, BufReader, Write};
use std::time::Instant;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::cache::BlobCache;
use crate::cli::ReportArgs;
use crate::metrics::{self, SessionMetrics};
use crate::patwari::{PatwariError, ReadClient};
use crate::report::{Instrumentation, Report, SkippedNote};
use crate::rules;
use crate::scoring::RulePack;
use crate::sync::{self, MirroredSession, Skip, SkipReason};

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("could not reach the archive at {url}: {source}")]
    Archive {
        url: String,
        #[source]
        source: PatwariError,
    },
    #[error("could not open the transcript cache: {0}")]
    Cache(#[source] io::Error),
    #[error("could not write the report: {0}")]
    Output(#[source] io::Error),
}

/// Runs `qanungo report`, writing Markdown to `out`.
///
/// # Errors
///
/// Returns an error when the cache is unusable, the archive window cannot be listed, or the
/// report cannot be written. A single unreadable session is a reported gap, not a failure.
pub fn report(args: &ReportArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let cache = match &args.cache_dir {
        Some(dir) => BlobCache::open(dir),
        None => BlobCache::open_default(),
    }
    .map_err(CommandError::Cache)?;

    let client =
        ReadClient::connect(&args.patwari_url).map_err(|source| CommandError::Archive {
            url: args.patwari_url.clone(),
            source,
        })?;

    let generated_at = Utc::now();
    let opens_at = args.last.opens_at(generated_at);
    let comparison_opens_at = args.last.comparison_opens_at(generated_at);
    // The mirror is asked for both windows at once when there is a comparison window to ask for.
    let activity_from = comparison_opens_at
        .unwrap_or(opens_at)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mirror =
        sync::sync(&client, &cache, &activity_from, args.concurrency).map_err(|source| {
            CommandError::Archive {
                url: args.patwari_url.clone(),
                source,
            }
        })?;

    let fold_started = Instant::now();
    let placed = Placement::of(&mirror.sessions, opens_at, comparison_opens_at);
    let mut skipped = mirror.skipped;
    let mut fold_all = |mirrored: &[&MirroredSession]| {
        let mut folded = Vec::with_capacity(mirrored.len());
        for session in mirrored {
            match fold_one(&cache, session) {
                Ok(metrics) => folded.push(metrics),
                Err(reason) => skipped.push(Skip {
                    source_agent: session.source_agent.clone(),
                    reason,
                }),
            }
        }
        folded
    };
    let sessions = fold_all(&placed.reported);
    let previous = fold_all(&placed.comparison);
    let fold_elapsed = fold_started.elapsed();

    let findings = rules::evaluate(&sessions);
    let instrumentation = Instrumentation {
        sync: mirror.stats,
        fold_elapsed,
        sessions_folded: sessions.len(),
        comparison_sessions_folded: previous.len(),
        bytes_folded: sessions
            .iter()
            .chain(&previous)
            .map(|session| session.bytes_folded)
            .sum(),
        rule_pack: RulePack::current(),
        patwari_url: args.patwari_url.clone(),
        cache_root: cache.root().to_path_buf(),
    };
    let markdown = Report {
        window: &args.last,
        generated_at,
        sessions: &sessions,
        previous: &previous,
        compared: comparison_opens_at.is_some(),
        findings: &findings,
        skipped: &summarize(&skipped, placed.unplaceable),
        instrumentation: &instrumentation,
    }
    .render();

    out.write_all(markdown.as_bytes())
        .map_err(CommandError::Output)
}

/// Folds one cached transcript, streaming it off disk rather than reading it whole.
fn fold_one(cache: &BlobCache, mirrored: &MirroredSession) -> Result<SessionMetrics, SkipReason> {
    let source = metrics::source_for_agent(&mirrored.source_agent)
        .ok_or_else(|| SkipReason::UnknownAgent(mirrored.source_agent.clone()))?;
    let blob = cache
        .open_blob(&mirrored.source_hash)
        .map_err(|error| SkipReason::Unreadable(format!("cache read failed: {error}")))?;
    let fold =
        metrics::fold_transcript(source, mirrored.artifact_set_version, BufReader::new(blob))
            .map_err(|error| SkipReason::Unreadable(error.to_string()))?;
    Ok(SessionMetrics {
        source_hash: mirrored.source_hash.clone(),
        source_agent: mirrored.source_agent.clone(),
        summary: fold.summary,
        tools: fold.tools,
        activity: fold.activity,
        commands: fold.commands,
        // The archive's declared original size, already verified against the transferred bytes,
        // so the footer's "bytes folded" needs no second pass over the file to count.
        bytes_folded: mirrored.size_bytes,
    })
}

/// Which window each mirrored session belongs to.
///
/// Sessions keep the archive's newest-first listing order inside each half, so the report is
/// stable across runs.
#[derive(Debug, Default)]
struct Placement<'a> {
    /// Archived inside the reported window.
    reported: Vec<&'a MirroredSession>,
    /// Archived inside the equal-length window before it.
    comparison: Vec<&'a MirroredSession>,
    /// Archived at a time this build could not read, or outside both halves. Neither folded nor
    /// swallowed: counted, and reported as a gap.
    unplaceable: usize,
}

impl<'a> Placement<'a> {
    /// Cuts the mirror into the reported window and the comparison window, on archive time.
    ///
    /// The halves are half-open — `[opens_at, ∞)` and `[comparison_opens_at, opens_at)` — so a
    /// session archived exactly on the boundary belongs to the later window and to it alone.
    fn of(
        sessions: &'a [MirroredSession],
        opens_at: DateTime<Utc>,
        comparison_opens_at: Option<DateTime<Utc>>,
    ) -> Self {
        let mut placement = Self::default();
        for session in sessions {
            match session.archived_at {
                Some(at) if at >= opens_at => placement.reported.push(session),
                Some(at) if comparison_opens_at.is_some_and(|from| at >= from) => {
                    placement.comparison.push(session);
                }
                _ => placement.unplaceable += 1,
            }
        }
        placement
    }
}

/// Groups skips by reason so a systematic gap reads as one line.
fn summarize(skipped: &[Skip], unplaceable: usize) -> Vec<SkippedNote> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for skip in skipped {
        let reason = match &skip.reason {
            SkipReason::NoTranscript => {
                format!("{}: snapshot has no transcript artifact", skip.source_agent)
            }
            SkipReason::UnknownAgent(agent) => {
                format!("{agent}: no interpreter for this harness in this build")
            }
            SkipReason::Unreadable(detail) => format!("{}: {detail}", skip.source_agent),
        };
        *counts.entry(reason).or_default() += 1;
    }
    let mut notes: Vec<_> = counts
        .into_iter()
        .map(|(reason, count)| SkippedNote { count, reason })
        .collect();
    if unplaceable > 0 {
        notes.push(SkippedNote {
            count: unplaceable,
            reason: "archived at a time this build could not place in either window".to_owned(),
        });
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_are_grouped_by_reason() {
        let skips = vec![
            Skip {
                source_agent: "claude-code".to_owned(),
                reason: SkipReason::NoTranscript,
            },
            Skip {
                source_agent: "claude-code".to_owned(),
                reason: SkipReason::NoTranscript,
            },
            Skip {
                source_agent: "future".to_owned(),
                reason: SkipReason::UnknownAgent("future".to_owned()),
            },
        ];
        let notes = summarize(&skips, 0);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].count, 2);
        assert!(notes[0].reason.contains("no transcript artifact"));
        assert_eq!(notes[1].count, 1);
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn mirrored(archived_at: Option<&str>) -> MirroredSession {
        MirroredSession {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            size_bytes: 0,
            archived_at: archived_at.map(at),
        }
    }

    /// The two halves are adjacent and half-open: a session archived exactly when the reported
    /// window opens belongs to it, not to the comparison window, and nothing lands in both.
    #[test]
    fn sessions_are_cut_into_two_windows_on_archive_time() {
        let opens_at = at("2026-08-01T00:00:00Z");
        let comparison_opens_at = at("2026-07-02T00:00:00Z");
        let sessions = vec![
            mirrored(Some("2026-08-10T00:00:00Z")),
            // Exactly on the boundary: the later window.
            mirrored(Some("2026-08-01T00:00:00Z")),
            mirrored(Some("2026-07-15T00:00:00Z")),
            // Exactly on the comparison window's own opening.
            mirrored(Some("2026-07-02T00:00:00Z")),
            // Older than the listing should have returned, and unreadable: neither is placed.
            mirrored(Some("2026-05-01T00:00:00Z")),
            mirrored(None),
        ];
        let placed = Placement::of(&sessions, opens_at, Some(comparison_opens_at));
        assert_eq!(placed.reported.len(), 2);
        assert_eq!(placed.comparison.len(), 2);
        assert_eq!(placed.unplaceable, 2);
    }

    /// With no comparison window, everything before the reported one is out of scope rather than
    /// quietly folded into it.
    #[test]
    fn without_a_comparison_window_only_the_reported_one_is_folded() {
        let sessions = vec![
            mirrored(Some("2026-08-10T00:00:00Z")),
            mirrored(Some("2026-07-15T00:00:00Z")),
        ];
        let placed = Placement::of(&sessions, at("2026-08-01T00:00:00Z"), None);
        assert_eq!(placed.reported.len(), 1);
        assert!(placed.comparison.is_empty());
        assert_eq!(placed.unplaceable, 1);
    }

    /// A session the archive dated unreadably is named in the report's Gaps rather than dropped.
    #[test]
    fn unplaceable_sessions_become_a_gap_line() {
        let notes = summarize(&[], 3);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].count, 3);
        assert!(notes[0].reason.contains("could not place in either window"));
    }
}
