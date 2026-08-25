//! The commands: the vertical slice's spine, and the lanes over the same spine.
//!
//! sync → fold → emit, in one pass, with the fold timed separately from the network so the
//! instrumentation footer measures what it claims to measure. [`report`] evaluates rules and
//! scores between the fold and the emit; [`cost`] prices instead; [`standup`] renders what the
//! sessions said about themselves. Everything before that fork — opening the cache, listing the
//! window, mirroring an artifact per session, cutting the result into windows — is [`Prepared`]
//! and is shared, so the lanes cannot come to disagree about what "the last 30 days" selected.
//!
//! # Two windows, one pass
//!
//! `--last 30d` mirrors **sixty** days and folds both halves: the reported window, and the equal
//! length immediately before it that the trend arrows are taken against. There is no store to read
//! last month's numbers out of, and there deliberately is not one (qanungo ADR 0001) — every run
//! recomputes all of it with the current rule pack, which is exactly what makes an arrow mean
//! behaviour drift rather than rule drift. The cost lane holds the same discipline against the
//! price table: a delta is drawn between two windows priced by the same table, stamped in the
//! footer.
//!
//! The two halves are cut on **archive time** — the `completed_at` of the snapshot the session was
//! listed by — because that is the clock `activity_from` already selected on. Cutting on transcript
//! time instead would let a session satisfy the listing and land in neither half. The cost of that
//! choice is stated in the report: a long-lived transcript resumed across the boundary is archived
//! again, so it appears in the later window only, carrying its earlier work with it.
//!
//! Archive time is also the clock the cost lane selects a **price row** as of, so a window
//! spanning a price change reports each session at what it cost when it was taken.
//!
//! The standup lane asks for one window rather than two ([`Reach`]). A narrative has no trend
//! arrow to draw, so the second window would be a doubling of the listing, the fetching, and the
//! cache traffic to produce nothing the document renders.
//!
//! # The fold is not the document
//!
//! `report` used to be one function from the archive to Markdown. It is now two — [`fold_coaching`]
//! and the rendering — because the dashboard (qanungo #5) is a *presentation* of the coaching
//! lane's numbers and not a second computation of them. It calls the same [`fold_coaching`], on the
//! same [`Reach::WindowPair`], and serializes the [`Folded`] it gets back instead of rendering it.
//!
//! That seam is the whole guarantee. A dashboard with its own fold would drift from the report
//! beside it the first time either one changed, and "the web page and the CLI disagree about my
//! scores" is not a bug anybody can act on. The split is deliberately a *move*, not a rewrite: the
//! rendering half of `report` receives exactly the fields the old body computed, in the same order,
//! so the Markdown on stdout is byte-for-byte what it was.
//!
//! The dashboard's standup-and-cost slice makes the same cut in the other two lanes: [`fold_cost`]
//! and [`fold_standup`] are the bodies `cost` and `standup` used to have, and each command is now
//! that call plus its renderer. The same reasoning applies unchanged and the same discipline was
//! held — the fields handed to [`CostReport`] and [`StandupReport`] are the ones the old bodies
//! computed, in the same order, so both documents are byte-for-byte what they were.
//!
//! The standup seam carries one extra property the other two do not need. [`Standup::fold`] scrubs
//! **on the way in**, so [`FoldedStandup`] holds no pre-scrub string at all: [`ReadSummary`] — the
//! parsed, unscrubbed record — is a local of [`fold_standup`] and is dropped before it returns.
//! Any surface reading a [`FoldedStandup`] therefore inherits qanungo #8's guarantee rather than
//! having to re-apply it, which is what lets the dashboard serve standup prose without a second
//! redactor call anywhere on that path.

use std::collections::BTreeMap;
use std::io::{self, BufReader, Write};
use std::time::Instant;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::cache::BlobCache;
use crate::cli::{ArchiveArgs, CostArgs, ReportArgs, StandupArgs, Window};
use crate::cost::{self, CostTotals, SessionCost};
use crate::cost_report::{CostInstrumentation, CostReport};
use crate::format;
use crate::metrics::{self, SessionMetrics};
use crate::patwari::{PatwariError, ReadClient};
use crate::redaction::Redactor;
use crate::report::{Instrumentation, Report, SkippedNote};
use crate::rules::{self, Finding};
use crate::scoring::RulePack;
use crate::standup::{Gap, ReadSummary, Standup};
use crate::standup_report::{StandupInstrumentation, StandupReport};
use crate::sync::{self, Artifact, Mirror, MirroredSession, Skip, SkipReason};

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

/// Everything the coaching lane computes, before anything renders it.
///
/// The two folded windows, the findings over the reported one, the gaps, and what the run cost —
/// which is to say the whole of `sync → fold → evaluate` with `emit` deliberately left off the
/// end. [`report`] turns one of these into Markdown; [`crate::dashboard`] turns the *same* one
/// into a JSON payload.
///
/// It is a type rather than a second copy of the pipeline because a dashboard that recomputed its
/// own numbers would eventually disagree with the report it claims to be a view of, and there is
/// no honest way to explain that to somebody reading both. The scores, the arrows, and the
/// findings on the served page are not "like" the CLI's: they are the CLI's, serialized instead of
/// written.
pub struct Folded {
    /// When this fold was taken — the instant both windows are cut relative to.
    pub generated_at: DateTime<Utc>,
    /// The reported window's sessions.
    pub sessions: Vec<SessionMetrics>,
    /// The equal-length window immediately before it, folded for the trend arrows.
    pub previous: Vec<SessionMetrics>,
    /// Whether a comparison window was asked for at all — a different fact from one that came back
    /// empty. See [`Report::compared`].
    pub compared: bool,
    pub findings: Vec<Finding>,
    pub skipped: Vec<SkippedNote>,
    pub instrumentation: Instrumentation,
}

/// Runs the coaching lane's `sync → fold → evaluate` over the window pair.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive window cannot be listed. A single
/// unreadable session is a reported gap, not a failure.
pub fn fold_coaching(archive: &ArchiveArgs, window: &Window) -> Result<Folded, CommandError> {
    let prepared = Prepared::mirror(archive, window, Artifact::Transcript, Reach::WindowPair)?;

    let fold_started = Instant::now();
    let placed = prepared.placement();
    let mut skipped = prepared.mirror.skipped.clone();
    let mut fold_all = |mirrored: &[&MirroredSession]| {
        let mut folded = Vec::with_capacity(mirrored.len());
        for session in mirrored {
            match fold_one(&prepared.cache, session) {
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
        sync: prepared.mirror.stats.clone(),
        fold_elapsed,
        sessions_folded: sessions.len(),
        comparison_sessions_folded: previous.len(),
        bytes_folded: sessions
            .iter()
            .chain(&previous)
            .map(|session| session.bytes_folded)
            .sum(),
        rule_pack: RulePack::current(),
        patwari_url: prepared.patwari_url.clone(),
        cache_root: prepared.cache.root().to_path_buf(),
    };
    Ok(Folded {
        generated_at: prepared.generated_at,
        compared: prepared.comparison_opens_at.is_some(),
        skipped: summarize(&skipped, placed.unplaceable),
        sessions,
        previous,
        findings,
        instrumentation,
    })
}

/// Runs `qanungo report`, writing Markdown to `out`.
///
/// # Errors
///
/// Returns an error when the cache is unusable, the archive window cannot be listed, or the
/// report cannot be written. A single unreadable session is a reported gap, not a failure.
pub fn report(args: &ReportArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let folded = fold_coaching(&args.archive, &args.last)?;
    let markdown = Report {
        window: &args.last,
        generated_at: folded.generated_at,
        sessions: &folded.sessions,
        previous: &folded.previous,
        compared: folded.compared,
        findings: &folded.findings,
        skipped: &folded.skipped,
        instrumentation: &folded.instrumentation,
    }
    .render();

    out.write_all(markdown.as_bytes())
        .map_err(CommandError::Output)
}

/// Everything the cost lane computes, before anything renders it.
///
/// The cost half of what [`Folded`] is for the coaching lane, and it exists for the same reason:
/// [`cost()`] turns one of these into Markdown and [`crate::dashboard`] turns the *same* one into a
/// JSON section, so a served dollar figure is the CLI's dollar figure rather than a second
/// arithmetic that agrees with it today.
pub struct FoldedCost {
    /// When this fold was taken — the instant both windows are cut relative to.
    pub generated_at: DateTime<Utc>,
    pub totals: CostTotals,
    /// The equal-length window immediately before it, priced by the same table. `None` when no
    /// comparison window was asked for at all — a window so long that doubling it overflows —
    /// which is a different fact from one that came back empty, and every surface says which.
    pub previous: Option<CostTotals>,
    pub skipped: Vec<SkippedNote>,
    pub instrumentation: CostInstrumentation,
}

/// Runs the cost lane's `sync → fold → price` over the window pair.
///
/// The same mirror and the same window pair the coaching lane uses, a different fold:
/// [`cost::fold_cost`] reads only what each assistant record says about the API message behind it,
/// deduplicated by message id, and [`CostTotals`] prices the result as of each session's own
/// archive time.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive window cannot be listed. A single
/// unreadable session is a reported gap, not a failure.
pub fn fold_cost(archive: &ArchiveArgs, window: &Window) -> Result<FoldedCost, CommandError> {
    let prepared = Prepared::mirror(archive, window, Artifact::Transcript, Reach::WindowPair)?;

    let fold_started = Instant::now();
    let placed = prepared.placement();
    let mut skipped = prepared.mirror.skipped.clone();
    let mut fold_all = |mirrored: &[&MirroredSession]| {
        let mut folded = Vec::with_capacity(mirrored.len());
        for session in mirrored {
            match fold_one_cost(&prepared.cache, session) {
                Ok(session) => folded.push(session),
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

    let totals = CostTotals::fold(&sessions);
    // Folded only when a comparison window was asked for at all: an empty comparison window and
    // an absent one are different facts, and the report says which.
    let earlier = prepared
        .comparison_opens_at
        .map(|_| CostTotals::fold(&previous));

    let instrumentation = CostInstrumentation {
        sync: prepared.mirror.stats.clone(),
        fold_elapsed,
        sessions_folded: sessions.len(),
        comparison_sessions_folded: previous.len(),
        records_read: totals.records_read
            + earlier.as_ref().map_or(0, |earlier| earlier.records_read),
        bytes_folded: totals.bytes_folded
            + earlier.as_ref().map_or(0, |earlier| earlier.bytes_folded),
        patwari_url: prepared.patwari_url.clone(),
        cache_root: prepared.cache.root().to_path_buf(),
    };
    Ok(FoldedCost {
        generated_at: prepared.generated_at,
        totals,
        previous: earlier,
        skipped: summarize(&skipped, placed.unplaceable),
        instrumentation,
    })
}

/// Runs `qanungo cost`, writing Markdown to `out`.
///
/// # Errors
///
/// Returns an error on the same three conditions [`report`] does, and for the same reason: a
/// single unreadable session is a gap the document states, never a failed run.
pub fn cost(args: &CostArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let folded = fold_cost(&args.archive, &args.last)?;
    let markdown = CostReport {
        window: &args.last,
        generated_at: folded.generated_at,
        totals: &folded.totals,
        previous: folded.previous.as_ref(),
        skipped: &folded.skipped,
        instrumentation: &folded.instrumentation,
    }
    .render();

    out.write_all(markdown.as_bytes())
        .map_err(CommandError::Output)
}

/// Everything the standup lane computes, before anything renders it.
///
/// **Nothing in here is unscrubbed.** [`Standup::fold`] runs the redactor on the way into its own
/// types, and the [`ReadSummary`] values it folded — the parsed archive records, still carrying
/// whatever the harness wrote — are locals of [`fold_standup`] that never leave it. So a consumer
/// of this type cannot render a pre-scrub string by mistake: there is no such string in scope. That
/// is the property the dashboard's standup section rests on, and it is a property of *construction*
/// rather than of anybody remembering to call a redactor twice.
pub struct FoldedStandup {
    /// When this fold was taken — the instant the window is cut relative to.
    pub generated_at: DateTime<Utc>,
    pub standup: Standup,
    pub instrumentation: StandupInstrumentation,
}

/// Runs the standup lane's `sync → read → scrub → group` over one window.
///
/// The same mirror and the same window machinery the other lanes use, pointed at each session's
/// `summary.md` instead of its transcript, and the first lane whose output carries text somebody
/// typed into a terminal. Everything it produces is scrubbed by [`Standup::fold`] with the
/// `redactor` passed in, and [`Standup::redaction`] carries what fired as counts so a footer can
/// say so.
///
/// One window rather than a pair: a narrative has no trend arrow to draw, so a second would double
/// the listing and the transfers to produce nothing any surface renders.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive window cannot be listed. A session
/// with no summary, an unparseable one, and a placeholder are each a stated gap, never a failure.
pub fn fold_standup(
    archive: &ArchiveArgs,
    window: &Window,
    redactor: Redactor,
) -> Result<FoldedStandup, CommandError> {
    let prepared = Prepared::mirror(archive, window, Artifact::Summary, Reach::WindowOnly)?;

    let fold_started = Instant::now();
    let placed = prepared.placement();
    let mut gaps: Vec<Gap> = prepared.mirror.skipped.iter().map(Gap::from_skip).collect();
    let mut read: Vec<ReadSummary> = Vec::with_capacity(placed.reported.len());
    for session in &placed.reported {
        match crate::standup::read_summary(&prepared.cache, session) {
            Ok(summary) => read.push(summary),
            Err(reason) => gaps.push(Gap {
                source_agent: session.source_agent.clone(),
                reason,
            }),
        }
    }
    let standup = Standup::fold(&read, &gaps, placed.unplaceable, &redactor);
    let fold_elapsed = fold_started.elapsed();

    let instrumentation = StandupInstrumentation {
        sync: prepared.mirror.stats.clone(),
        fold_elapsed,
        redactor,
        patwari_url: prepared.patwari_url.clone(),
        cache_root: prepared.cache.root().to_path_buf(),
    };
    Ok(FoldedStandup {
        generated_at: prepared.generated_at,
        standup,
        instrumentation,
    })
}

/// Runs `qanungo standup`, writing Markdown to `out`.
///
/// # Errors
///
/// Returns an error on the same three conditions the other lanes do. A session with no summary, an
/// unparseable one, and a placeholder are each a stated gap, never a failed run.
pub fn standup(args: &StandupArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let folded = fold_standup(&args.archive, &args.last, args.redaction.redactor())?;
    let markdown = StandupReport {
        window: &args.last,
        generated_at: folded.generated_at,
        standup: &folded.standup,
        instrumentation: &folded.instrumentation,
    }
    .render();

    out.write_all(markdown.as_bytes())
        .map_err(CommandError::Output)
}

/// How much history a lane asks the mirror for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// The reported window and the equal-length one before it — what a trend arrow costs, paid by
    /// the two lanes that draw one.
    WindowPair,
    /// The reported window alone. A standup narrates one window; there is no arrow to draw, so
    /// mirroring a second window would double the listing and the transfers to render nothing.
    WindowOnly,
}

/// Everything a lane does before it decides what to fold: the cache, the mirrored window pair,
/// and the three instants both halves are cut on.
///
/// Shared rather than repeated because the alternative is two commands that could quietly select
/// different sessions for the same `--last`, and a cost report that priced a different window
/// from the one the coaching report scored would be worse than either on its own.
struct Prepared {
    cache: BlobCache,
    mirror: Mirror,
    generated_at: DateTime<Utc>,
    opens_at: DateTime<Utc>,
    comparison_opens_at: Option<DateTime<Utc>>,
    patwari_url: String,
}

impl Prepared {
    /// Opens the cache and mirrors the window `reach` asks for.
    fn mirror(
        archive: &ArchiveArgs,
        window: &Window,
        artifact: Artifact,
        reach: Reach,
    ) -> Result<Self, CommandError> {
        let cache = match &archive.cache_dir {
            Some(dir) => BlobCache::open(dir),
            None => BlobCache::open_default(),
        }
        .map_err(CommandError::Cache)?;

        let client =
            ReadClient::connect(&archive.patwari_url).map_err(|source| CommandError::Archive {
                url: archive.patwari_url.clone(),
                source,
            })?;

        let generated_at = Utc::now();
        let opens_at = window.opens_at(generated_at);
        // A lane that draws no comparison has none, which is the same state a window too long to
        // double is already in: `placement` then puts anything older in neither half, and the
        // listing never asked for it in the first place.
        let comparison_opens_at = match reach {
            Reach::WindowPair => window.comparison_opens_at(generated_at),
            Reach::WindowOnly => None,
        };
        // The mirror is asked for both windows at once when there is a comparison window to ask
        // for.
        let activity_from = comparison_opens_at
            .unwrap_or(opens_at)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mirror = sync::sync(
            &client,
            &cache,
            artifact,
            &activity_from,
            archive.concurrency,
        )
        .map_err(|source| CommandError::Archive {
            url: archive.patwari_url.clone(),
            source,
        })?;

        Ok(Self {
            cache,
            mirror,
            generated_at,
            opens_at,
            comparison_opens_at,
            patwari_url: archive.patwari_url.clone(),
        })
    }

    /// Cuts the mirror into the two windows.
    fn placement(&self) -> Placement<'_> {
        Placement::of(
            &self.mirror.sessions,
            self.opens_at,
            self.comparison_opens_at,
        )
    }
}

/// Folds one cached transcript, streaming it off disk rather than reading it whole.
fn fold_one(cache: &BlobCache, mirrored: &MirroredSession) -> Result<SessionMetrics, SkipReason> {
    let (source, blob) = open_for_fold(cache, mirrored)?;
    let fold =
        metrics::fold_transcript(source, mirrored.artifact_set_version, BufReader::new(blob))
            .map_err(|error| SkipReason::Unreadable(error.to_string()))?;
    Ok(SessionMetrics {
        source_hash: mirrored.source_hash.clone(),
        source_agent: mirrored.source_agent.clone(),
        // Carried, not re-derived: the excerpt route reads the anchored event back with the same
        // interpreter this fold used, and the snapshot is what states which contract that is.
        artifact_set_version: mirrored.artifact_set_version,
        summary: fold.summary,
        tools: fold.tools,
        activity: fold.activity,
        commands: fold.commands,
        compactions: fold.compactions,
        reviews: fold.reviews,
        anchors: fold.anchors,
        // The archive's declared original size, already verified against the transferred bytes,
        // so the footer's "bytes folded" needs no second pass over the file to count.
        bytes_folded: mirrored.size_bytes,
    })
}

/// The same, for the billing records: one pass over the same cached blob, reading only what each
/// assistant record says about the API message behind it.
fn fold_one_cost(cache: &BlobCache, mirrored: &MirroredSession) -> Result<SessionCost, SkipReason> {
    let (source, blob) = open_for_fold(cache, mirrored)?;
    let fold = cost::fold_cost(source, mirrored.artifact_set_version, BufReader::new(blob))
        .map_err(|error| SkipReason::Unreadable(error.to_string()))?;
    Ok(SessionCost {
        source_hash: mirrored.source_hash.clone(),
        source_agent: mirrored.source_agent.clone(),
        repository: mirrored.repository.clone(),
        archived_at: mirrored.archived_at,
        fold,
        bytes_folded: mirrored.size_bytes,
    })
}

/// Resolves the interpreter for a mirrored session and opens its cached blob, or says which of
/// the two could not be done.
fn open_for_fold(
    cache: &BlobCache,
    mirrored: &MirroredSession,
) -> Result<(munshi_transcript::Source, std::fs::File), SkipReason> {
    let source = metrics::source_for_agent(&mirrored.source_agent)
        .ok_or_else(|| SkipReason::UnknownAgent(mirrored.source_agent.clone()))?;
    let blob = cache
        .open_blob(&mirrored.source_hash)
        .map_err(|error| SkipReason::Unreadable(format!("cache read failed: {error}")))?;
    Ok((source, blob))
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
///
/// The harness label is the archive's string, not this build's — a snapshot manifest states
/// whatever `session.source_agent` it likes, and an unrecognized one is precisely the case that
/// reaches [`SkipReason::UnknownAgent`] — so it goes through [`format::identifier`] before it is
/// interpolated. Both lanes print these lines verbatim in their Gaps section, so clamping here
/// rather than at either rendering site is what keeps the two from drifting apart, and is why the
/// coaching report's own claim to render "aggregates, tool names, and `source_hash` references"
/// survives contact with a hostile archive.
fn summarize(skipped: &[Skip], unplaceable: usize) -> Vec<SkippedNote> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for skip in skipped {
        let agent = format::identifier(&skip.source_agent);
        let reason = match &skip.reason {
            SkipReason::MissingArtifact(artifact) => {
                format!(
                    "{agent}: snapshot has no `{}` artifact",
                    artifact.logical_path()
                )
            }
            SkipReason::UnknownAgent(named) => format!(
                "{}: no interpreter for this harness in this build",
                format::identifier(named),
            ),
            SkipReason::Unreadable(detail) => format!("{agent}: {detail}"),
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
                reason: SkipReason::MissingArtifact(Artifact::Transcript),
            },
            Skip {
                source_agent: "claude-code".to_owned(),
                reason: SkipReason::MissingArtifact(Artifact::Transcript),
            },
            Skip {
                source_agent: "future".to_owned(),
                reason: SkipReason::UnknownAgent("future".to_owned()),
            },
        ];
        let notes = summarize(&skips, 0);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].count, 2);
        assert!(notes[0].reason.contains("no `transcript.jsonl` artifact"));
        assert_eq!(notes[1].count, 1);
    }

    /// A skip line names a harness the *archive* named, so a manifest can put anything it likes
    /// in it — and both lanes print these lines verbatim in their Gaps section. The clamp is
    /// therefore a redaction control on the coaching report as much as on the cost one, which is
    /// why it lives here rather than at either rendering site.
    #[test]
    fn a_hostile_harness_label_cannot_break_out_of_a_gaps_line() {
        let hostile = |agent: &str, reason: SkipReason| Skip {
            source_agent: agent.to_owned(),
            reason,
        };
        let notes = summarize(
            &[
                hostile(
                    "claude-code | evil",
                    SkipReason::MissingArtifact(Artifact::Transcript),
                ),
                hostile(
                    "fine",
                    SkipReason::UnknownAgent("newline\ninjected".to_owned()),
                ),
                hostile(
                    "back`tick",
                    SkipReason::Unreadable("cache read failed".to_owned()),
                ),
            ],
            0,
        );
        let rendered = notes
            .iter()
            .map(|note| note.reason.clone())
            .collect::<Vec<_>>()
            .join("\n");
        for hostile in ["claude-code | evil", "newline\ninjected", "back`tick"] {
            assert!(
                !rendered.contains(hostile),
                "{hostile:?} survived: {rendered}"
            );
        }
        assert_eq!(
            rendered.matches(format::INVALID_IDENTIFIER).count(),
            3,
            "each of the three is replaced wholesale, not truncated: {rendered}",
        );
        // An ordinary label is untouched, so the clamp costs the common case nothing.
        let ordinary = summarize(
            &[hostile(
                "claude-code",
                SkipReason::MissingArtifact(Artifact::Transcript),
            )],
            0,
        );
        assert_eq!(
            ordinary[0].reason,
            "claude-code: snapshot has no `transcript.jsonl` artifact",
        );
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
            repository: None,
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
