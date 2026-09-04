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
//! # The Gaps section's one archive-derived string
//!
//! Every other string `report` and `cost` put on a page is this build's own — an aggregate, a tool
//! name the rule pack knows, a `source_hash`. The exception is the harness label on a Gaps line,
//! which is whatever a snapshot manifest said its `source_agent` was. It is therefore clamped
//! *then* scrubbed — in that order and for the reason [`crate::evidence::identifier_field`]'s own
//! rustdoc argues — rather than passed through the clamp alone, so a credential-shaped label in a
//! corrupted or adversarial listing leaves as a marker instead of as itself.
//!
//! One helper does that for every surface stating a skipped session: [`skip_line`], which both Gaps
//! sections and the ask lane's `--verbatim` "this transcript could not be searched" note go
//! through. It spells the two passes out rather than calling `identifier_field`, so that the scrub
//! can be *counted* for the one caller whose footer reports what fired; the two documents here have
//! no such footer and discard the count. The sentence is shared so the same gap cannot come to read
//! two ways in one binary.
//!
//! That is the whole reason [`fold_coaching`] and [`fold_cost`] take a [`Redactor`] at all: neither
//! computes anything else a redactor touches. The dashboard hands them its launch-time one. The CLI
//! lanes build [`Redactor::new`] internally, and there is deliberately no `--no-redact` on `report`
//! or `cost` to turn it back off — those two documents render aggregates and hashes by design, so a
//! harness label is not content anybody asked to see raw, and a flag whose only effect would be to
//! leak one is not worth offering. The CLI's gap labels are scrubbed unconditionally.
//!
//! The standup seam carries one extra property the other two do not need. [`Standup::fold`] scrubs
//! **on the way in**, so [`FoldedStandup`] holds no pre-scrub string at all: [`ReadSummary`] — the
//! parsed, unscrubbed record — is a local of [`fold_standup`] and is dropped before it returns.
//! Any surface reading a [`FoldedStandup`] therefore inherits qanungo #8's guarantee rather than
//! having to re-apply it, which is what lets the dashboard serve standup prose without a second
//! redactor call anywhere on that path.

use std::collections::BTreeMap;
use std::io::{self, BufReader, Write};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::ask::{Ask, Escalation, Query};
use crate::ask_report::{AskInstrumentation, AskReport, VerbatimStats};
use crate::cache::BlobCache;
use crate::cli::{
    ArchiveArgs, AskArgs, CostArgs, DoctorArgs, FlowsArgs, ReportArgs, StandupArgs, Window,
};
use crate::cost::{self, CostTotals, SessionCost};
use crate::cost_report::{CostInstrumentation, CostReport};
use crate::doctor::{Doctor, DoctorSession};
use crate::doctor_report::{DoctorInstrumentation, DoctorReport};
use crate::flows::{Flows, FlowsSession};
use crate::flows_report::{FlowsInstrumentation, FlowsReport};
use crate::format;
use crate::metrics::{self, SessionMetrics};
use crate::patwari::{PatwariError, ReadClient};
use crate::redaction::{RedactionReport, Redactor};
use crate::report::{Instrumentation, Report, SkippedNote};
use crate::rules::{self, Finding};
use crate::scoring::RulePack;
use crate::standup::{Gap, ReadSummary, Standup};
use crate::standup_report::{StandupInstrumentation, StandupReport};
use crate::sync::{self, Artifact, Mirror, MirroredSession, Skip, SkipReason, SyncStats};

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
/// The `redactor` scrubs exactly one thing: the harness label on a gap line, which is the only
/// archive-stated string this fold's output carries. Nothing else here is text anybody wrote.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive window cannot be listed. A single
/// unreadable session is a reported gap, not a failure.
pub fn fold_coaching(
    archive: &ArchiveArgs,
    window: &Window,
    redactor: &Redactor,
) -> Result<Folded, CommandError> {
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
        skipped: summarize(
            &skipped,
            placed.unplaceable,
            redactor,
            &mut RedactionReport::default(),
        ),
        sessions,
        previous,
        findings,
        instrumentation,
    })
}

/// Runs `qanungo report`, writing Markdown to `out`.
///
/// The redactor is built here rather than taken from a flag: `report` exposes no `RedactionArgs`,
/// because the document renders aggregates and hashes and the one archive-stated string in it — a
/// gap line's harness label — is not content a reader asked to see raw. Scrubbing it is therefore
/// unconditional, and there is nothing to turn off.
///
/// # Errors
///
/// Returns an error when the cache is unusable, the archive window cannot be listed, or the
/// report cannot be written. A single unreadable session is a reported gap, not a failure.
pub fn report(args: &ReportArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let folded = fold_coaching(&args.archive, &args.last, &Redactor::new())?;
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
/// The `redactor` is here for the same one string [`fold_coaching`]'s is: a gap line's harness
/// label. A priced window is otherwise arithmetic and model ids.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive window cannot be listed. A single
/// unreadable session is a reported gap, not a failure.
pub fn fold_cost(
    archive: &ArchiveArgs,
    window: &Window,
    redactor: &Redactor,
) -> Result<FoldedCost, CommandError> {
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
        skipped: summarize(
            &skipped,
            placed.unplaceable,
            redactor,
            &mut RedactionReport::default(),
        ),
        instrumentation,
    })
}

/// Runs `qanungo cost`, writing Markdown to `out`.
///
/// Its redactor is built the same way [`report`]'s is, and for the same reason: this lane has no
/// `--no-redact` either, and a gap line's harness label is scrubbed whatever the operator typed.
///
/// # Errors
///
/// Returns an error on the same three conditions [`report`] does, and for the same reason: a
/// single unreadable session is a gap the document states, never a failed run.
pub fn cost(args: &CostArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let folded = fold_cost(&args.archive, &args.last, &Redactor::new())?;
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

/// What `fold_ask` returns: the ranked search plus what it cost, the same triple the other folds
/// hand their renderer.
pub struct FoldedAsk {
    pub generated_at: DateTime<Utc>,
    pub ask: Ask,
    pub instrumentation: AskInstrumentation,
}

/// Searches one scope of archived summaries against a query and ranks the matches (qanungo #10).
///
/// `window` is `None` for the whole archive and `Some` to narrow to a window on the same clock the
/// other lanes cut on. Everything the fold reads is a `summary.md` the cache already holds; the
/// scoring is [`Ask::fold`]'s, and the scrub happens there, so [`FoldedAsk`] — like [`FoldedStandup`]
/// — carries no pre-scrub string for any surface to leak.
///
/// # The `verbatim` escalation
///
/// With `verbatim` set, the ranking is followed by [`escalate`]: the transcripts of the hits this
/// run is going to *show* are fetched and searched for the same terms. It runs after the fold and
/// outside its timer, because it is network work and the footer's fold figure claims to be the
/// local one. Everything it costs is reported separately, and the hits it could not read say so
/// rather than reading as hits with nothing in them.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive cannot be listed. A session with no
/// readable summary is *unsearchable*, counted on the result and never a failed run; a shown hit
/// whose transcript cannot be read is a stated note under that hit, never a failed run either.
pub fn fold_ask(
    archive: &ArchiveArgs,
    window: Option<&Window>,
    query: &Query,
    redactor: Redactor,
    limit: usize,
    verbatim: bool,
) -> Result<FoldedAsk, CommandError> {
    let prepared = match window {
        Some(window) => Prepared::mirror(archive, window, Artifact::Summary, Reach::WindowOnly)?,
        None => Prepared::mirror_all(archive, Artifact::Summary)?,
    };

    let fold_started = Instant::now();
    let corpus = read_corpus(&prepared);
    let mut ask = Ask::fold(query, &corpus.read, &redactor, limit, corpus.unsearchable);
    let fold_elapsed = fold_started.elapsed();
    // Outside the fold timer on purpose: this is the archive, and the fold figure beside it in the
    // footer claims to measure the local read.
    let verbatim =
        verbatim.then(|| escalate(&prepared, &mut ask, &corpus.searched, query, &redactor));

    let instrumentation = AskInstrumentation {
        sync: prepared.mirror.stats.clone(),
        fold_elapsed,
        verbatim,
        redactor,
        patwari_url: prepared.patwari_url.clone(),
        cache_root: prepared.cache.root().to_path_buf(),
    };
    Ok(FoldedAsk {
        generated_at: prepared.generated_at,
        ask,
        instrumentation,
    })
}

/// Every summary one mirror run put in the cache, parsed, alongside the count of what could not be.
///
/// The `searched` borrows run in lockstep with `read`, which is the whole of what an escalation
/// needs to get from a ranked hit back to *its* session ([`Ask::fold`] records the index it scored a
/// summary at). A lane that never escalates simply drops them.
struct ReadCorpus<'a> {
    read: Vec<ReadSummary>,
    searched: Vec<&'a MirroredSession>,
    unsearchable: usize,
}

/// Reads every mirrored session's cached `summary.md`, counting the ones that never reach a score.
///
/// Every session the listing produced that never reaches a score is counted, never dropped — the
/// same honesty the other three lanes keep. There are three such populations, disjoint: the mirror's
/// own skips (a snapshot with no `summary.md` at all, or one unreadable at fetch time — these are in
/// `mirror.skipped`, not in `mirror.sessions`, so `placement` never sees them), the sessions the
/// archive dated outside the searched window (`placed.unplaceable`), and the ones whose cached
/// summary this build then failed to read or parse (the `Err` arm below).
///
/// Shared by the CLI's [`fold_ask`] and the dashboard's [`fold_ask_corpus`] rather than written
/// twice, so the served endpoint and `qanungo ask` cannot come to disagree about which sessions were
/// searchable — the same reason [`Prepared`] is shared by the document lanes.
fn read_corpus<'a>(prepared: &'a Prepared) -> ReadCorpus<'a> {
    let placed = prepared.placement();
    let mut unsearchable = placed.unplaceable + prepared.mirror.skipped.len();
    let mut read = Vec::with_capacity(placed.reported.len());
    let mut searched: Vec<&MirroredSession> = Vec::with_capacity(placed.reported.len());
    for session in &placed.reported {
        match crate::standup::read_summary(&prepared.cache, session) {
            Ok(summary) => {
                read.push(summary);
                searched.push(session);
            }
            Err(_) => unsearchable += 1,
        }
    }
    ReadCorpus {
        read,
        searched,
        unsearchable,
    }
}

/// The whole archive's summaries, parsed and held in memory, so a *request* can be ranked against
/// them without the archive being touched (qanungo #10's dashboard ask-box).
///
/// # Why a corpus rather than a fold per request
///
/// [`fold_ask`] is a run: it mirrors, reads, ranks one query, and returns. The dashboard cannot do
/// that on a request path, and not because it would be slow. The evidence route's iron rule is that
/// **a browser must never induce archive traffic** ([`crate::dashboard_server`]) — an unauthenticated
/// peer that could make this process talk to Patwari would be a remote control for somebody else's
/// bandwidth and for what lands on this disk. So the mirroring and the reading happen on the
/// service's own refresh timer, exactly as the three document lanes do, and what a request does is
/// score an in-memory `Vec` it did not fetch.
///
/// # Why all of history
///
/// No window, matching `qanungo ask`'s own no-default-window semantics (decision 12: a lifetime
/// question). It is affordable because a `summary.md` is a rounding error beside a transcript —
/// measured at **~0.4% of transcript bytes** across the production archive — so holding every one of
/// them parsed costs a few megabytes against the 3 GiB of transcripts the other lanes stream past.
///
/// # Nothing here is scrubbed, and nothing here is served
///
/// A [`ReadSummary`] is the archive's own record, pre-scrub — the same value [`fold_ask`] holds as a
/// local. It is *fold input*, never response: every string a reader sees is produced by
/// [`Ask::fold`], which scrubs on the way into the hit it builds. See [`AskCorpus::search`].
pub struct AskCorpus {
    /// When this corpus was read — the instant its provenance is stamped with.
    pub generated_at: DateTime<Utc>,
    /// The summaries a search scores, in the archive's own newest-first listing order.
    read: Vec<ReadSummary>,
    /// Sessions the archive listed that carry no summary this build could read. Counted here so
    /// every answer taken over this corpus can say what it could not look at.
    pub unsearchable: usize,
    /// Decompressed summary bytes held in memory — what the corpus costs, measured rather than
    /// estimated.
    pub bytes_read: u64,
    pub instrumentation: AskCorpusInstrumentation,
}

/// What building one ask corpus cost, for the provenance block that reports the refresh.
#[derive(Debug, Clone)]
pub struct AskCorpusInstrumentation {
    pub sync: SyncStats,
    /// Wall-time of reading and parsing the summaries alone, network excluded — the same split
    /// every other lane's footer makes.
    pub fold_elapsed: Duration,
}

impl AskCorpus {
    /// How many sessions a search over this corpus scores.
    pub fn searchable(&self) -> usize {
        self.read.len()
    }

    /// Every session the listing produced, searchable or not. The denominator an answer's counts
    /// reconcile against.
    pub fn listed(&self) -> usize {
        self.read.len() + self.unsearchable
    }

    /// Ranks one query against the corpus, with the process's launch-time redactor.
    ///
    /// [`Ask::fold`] and nothing else: the rubric, the total order, the snippet rule, and the scrub
    /// are the CLI's, so the same query over the same corpus ranks the same way in both places. No
    /// escalation is possible from here and none is offered — see [`crate::dashboard_server`] for
    /// why `--verbatim` stays a CLI affordance.
    pub fn search(&self, query: &Query, redactor: &Redactor, limit: usize) -> Ask {
        Ask::fold(query, &self.read, redactor, limit, self.unsearchable)
    }

    /// A corpus over summaries a caller already holds, with no mirror behind it.
    ///
    /// The real one comes from [`fold_ask_corpus`], which needs an archive. This exists so the two
    /// surfaces *over* a corpus — the served answer's shape and the provenance line that reports it
    /// — can be pinned without standing one up, the way [`crate::rules::RuleId::eligible`] exists
    /// for the eligibility boundary. Everything the archive would have said about the run is zero
    /// here, which is the honest reading of a corpus nothing was synced for.
    #[cfg(test)]
    pub fn over(generated_at: DateTime<Utc>, read: Vec<ReadSummary>, unsearchable: usize) -> Self {
        Self {
            generated_at,
            bytes_read: read.iter().map(|summary| summary.bytes_read).sum(),
            read,
            unsearchable,
            instrumentation: AskCorpusInstrumentation {
                sync: SyncStats::default(),
                fold_elapsed: Duration::ZERO,
            },
        }
    }
}

/// Mirrors and reads every `summary.md` in the archive, for a service that will rank requests
/// against them.
///
/// The mirroring is [`Prepared::mirror_all`] — the same entry point `qanungo ask` takes with no
/// `--last`, so the served corpus is the CLI's corpus and not a second selection of it.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive cannot be listed, which is what makes
/// the whole refresh fail and the served document go stale: a corpus half-read is a search that
/// would answer "no" for a session it simply did not look at.
pub fn fold_ask_corpus(archive: &ArchiveArgs) -> Result<AskCorpus, CommandError> {
    let prepared = Prepared::mirror_all(archive, Artifact::Summary)?;

    let fold_started = Instant::now();
    let corpus = read_corpus(&prepared);
    let fold_elapsed = fold_started.elapsed();

    Ok(AskCorpus {
        generated_at: prepared.generated_at,
        bytes_read: corpus.read.iter().map(|summary| summary.bytes_read).sum(),
        unsearchable: corpus.unsearchable,
        read: corpus.read,
        instrumentation: AskCorpusInstrumentation {
            sync: prepared.mirror.stats.clone(),
            fold_elapsed,
        },
    })
}

/// Runs `qanungo ask`, writing Markdown to `out`.
///
/// The query is parsed before anything touches the archive: a query with no searchable word in it
/// is answered on the spot ([`crate::ask_report::no_searchable_terms`]) rather than mirroring the
/// whole archive to rank nothing.
///
/// # Errors
///
/// Returns an error on the same conditions the other lanes do. A session with no readable summary
/// is a counted non-match, never a failed run.
pub fn ask(args: &AskArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let query = Query::parse(&args.query);
    if query.is_empty() {
        return out
            .write_all(crate::ask_report::no_searchable_terms(&args.query).as_bytes())
            .map_err(CommandError::Output);
    }
    let folded = fold_ask(
        &args.archive,
        args.last.as_ref(),
        &query,
        args.redaction.redactor(),
        args.limit,
        args.verbatim,
    )?;
    let markdown = AskReport {
        raw_query: &args.query,
        query: &query,
        window: args.last.as_ref(),
        limit: args.limit,
        generated_at: folded.generated_at,
        ask: &folded.ask,
        instrumentation: &folded.instrumentation,
    }
    .render();

    out.write_all(markdown.as_bytes())
        .map_err(CommandError::Output)
}

/// What `fold_doctor` returns: the clustering plus what it cost.
pub struct FoldedDoctor {
    pub generated_at: DateTime<Utc>,
    pub doctor: Doctor,
    pub instrumentation: DoctorInstrumentation,
}

/// Runs the doctor lane's `sync → read → cluster` over one reach (qanungo #11).
///
/// `window` is `None` for the whole archive and `Some` to narrow to a window on the same clock the
/// other lanes cut on — the [`fold_ask`] shape, for the reason [`crate::cli::DoctorArgs`] argues.
///
/// The artifact is the **transcript**, not the summary: what this lane compares is what a person
/// typed, and a `summary.md` is munshi's curated prose about a session rather than the session's own
/// words. So this is a transcript-folding lane like `report` and `cost` — a cold run with no window
/// mirrors the archive, and the footer says what that cost.
///
/// Everything the fold renders is scrubbed inside [`Doctor::fold`], so [`FoldedDoctor`] — like
/// [`FoldedStandup`] and [`FoldedAsk`] — carries no pre-scrub string for any surface to leak. The
/// gap lines are summarized here, where the mirror's own vocabulary lives, and their scrub is handed
/// to the fold so that this document's footer counts it: a marker where a harness name belongs, with
/// "redaction none" beneath it, would be the document contradicting itself.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive cannot be listed. A session whose
/// transcript this build cannot read is a stated gap, never a failed run.
///
/// `clusters_per_repo` is the rendering cut `--clusters-per-repo` sets; it reaches
/// [`Doctor::fold`] and the document alike, from the one argument, so the number the cut used and
/// the number the document states cannot drift apart.
pub fn fold_doctor(
    archive: &ArchiveArgs,
    window: Option<&Window>,
    redactor: Redactor,
    clusters_per_repo: usize,
) -> Result<FoldedDoctor, CommandError> {
    let prepared = match window {
        Some(window) => Prepared::mirror(archive, window, Artifact::Transcript, Reach::WindowOnly)?,
        None => Prepared::mirror_all(archive, Artifact::Transcript)?,
    };

    let fold_started = Instant::now();
    let placed = prepared.placement();
    let mut skipped = prepared.mirror.skipped.clone();
    let mut sessions = Vec::with_capacity(placed.reported.len());
    for session in &placed.reported {
        match read_one_transcript(&prepared.cache, session) {
            Ok(messages) => sessions.push(DoctorSession {
                source_hash: session.source_hash.clone(),
                archived_at: session.archived_at,
                repository: session.repository.clone(),
                // The archive's declared original size, already verified against the transferred
                // bytes, so the footer needs no second pass over the file to count what it read.
                bytes_folded: session.size_bytes,
                messages,
            }),
            Err(reason) => skipped.push(Skip {
                source_agent: session.source_agent.clone(),
                reason,
            }),
        }
    }
    let mut gap_redaction = RedactionReport::default();
    let gaps = summarize(&skipped, placed.unplaceable, &redactor, &mut gap_redaction);
    let doctor = Doctor::fold(
        &sessions,
        gaps,
        &gap_redaction,
        &redactor,
        clusters_per_repo,
    );
    let fold_elapsed = fold_started.elapsed();

    let instrumentation = DoctorInstrumentation {
        sync: prepared.mirror.stats.clone(),
        fold_elapsed,
        redactor,
        patwari_url: prepared.patwari_url.clone(),
        cache_root: prepared.cache.root().to_path_buf(),
    };
    Ok(FoldedDoctor {
        generated_at: prepared.generated_at,
        doctor,
        instrumentation,
    })
}

/// Reads one cached transcript's user messages, streaming it off disk rather than reading it whole.
///
/// Shared by `doctor` and `flows`: the two lanes read the same substrate and differ only in how they
/// pool what comes out of it, so a second reader would be a second place for "what a person typed"
/// to come to mean something slightly different.
fn read_one_transcript(
    cache: &BlobCache,
    mirrored: &MirroredSession,
) -> Result<crate::repetition::SessionMessages, SkipReason> {
    let (source, blob) = open_for_fold(cache, mirrored)?;
    crate::repetition::read_messages(source, mirrored.artifact_set_version, BufReader::new(blob))
        .map_err(|error| SkipReason::Unreadable(error.to_string()))
}

/// Runs `qanungo doctor`, writing Markdown to `out`.
///
/// # Errors
///
/// Returns an error on the same conditions the other lanes do. A session whose transcript cannot be
/// read is a stated gap, never a failed run.
pub fn doctor(args: &DoctorArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let folded = fold_doctor(
        &args.archive,
        args.last.as_ref(),
        args.redaction.redactor(),
        args.clusters_per_repo,
    )?;
    let markdown = DoctorReport {
        window: args.last.as_ref(),
        clusters_per_repo: args.clusters_per_repo,
        generated_at: folded.generated_at,
        doctor: &folded.doctor,
        instrumentation: &folded.instrumentation,
    }
    .render();

    out.write_all(markdown.as_bytes())
        .map_err(CommandError::Output)
}

/// What `fold_flows` returns: the clustering, the mined flows, and what they cost.
pub struct FoldedFlows {
    pub generated_at: DateTime<Utc>,
    pub flows: Flows,
    pub instrumentation: FlowsInstrumentation,
}

/// Runs the flows lane's `sync → read → cluster → mine` over one reach (qanungo #13).
///
/// The mirror, the substrate and the gap handling are [`fold_doctor`]'s, line for line: both lanes
/// read `transcript.jsonl` because what they compare is what a *person* typed, both take the whole
/// archive when `window` is `None`, and both hand the mirror's own scrubbed gap summary to the fold
/// so the document's footer counts it. The single difference is downstream of here — [`Flows::fold`]
/// pools every session into one comparison where [`Doctor::fold`] groups by repository first.
///
/// Everything the fold renders is scrubbed inside [`Flows::fold`], so [`FoldedFlows`] carries no
/// pre-scrub string for any surface to leak.
///
/// # Errors
///
/// Returns an error when the cache is unusable or the archive cannot be listed. A session whose
/// transcript this build cannot read is a stated gap, never a failed run.
pub fn fold_flows(
    archive: &ArchiveArgs,
    window: Option<&Window>,
    redactor: Redactor,
    clusters: usize,
    flows: usize,
) -> Result<FoldedFlows, CommandError> {
    let prepared = match window {
        Some(window) => Prepared::mirror(archive, window, Artifact::Transcript, Reach::WindowOnly)?,
        None => Prepared::mirror_all(archive, Artifact::Transcript)?,
    };

    let fold_started = Instant::now();
    let placed = prepared.placement();
    let mut skipped = prepared.mirror.skipped.clone();
    let mut sessions = Vec::with_capacity(placed.reported.len());
    for session in &placed.reported {
        match read_one_transcript(&prepared.cache, session) {
            Ok(messages) => sessions.push(FlowsSession {
                source_hash: session.source_hash.clone(),
                archived_at: session.archived_at,
                repository: session.repository.clone(),
                bytes_folded: session.size_bytes,
                messages,
            }),
            Err(reason) => skipped.push(Skip {
                source_agent: session.source_agent.clone(),
                reason,
            }),
        }
    }
    let mut gap_redaction = RedactionReport::default();
    let gaps = summarize(&skipped, placed.unplaceable, &redactor, &mut gap_redaction);
    let found = Flows::fold(&sessions, gaps, &gap_redaction, &redactor, clusters, flows);
    let fold_elapsed = fold_started.elapsed();

    let instrumentation = FlowsInstrumentation {
        sync: prepared.mirror.stats.clone(),
        fold_elapsed,
        redactor,
        patwari_url: prepared.patwari_url.clone(),
        cache_root: prepared.cache.root().to_path_buf(),
    };
    Ok(FoldedFlows {
        generated_at: prepared.generated_at,
        flows: found,
        instrumentation,
    })
}

/// Runs `qanungo flows`, writing Markdown to `out`.
///
/// # Errors
///
/// Returns an error on the same conditions the other lanes do. A session whose transcript cannot be
/// read is a stated gap, never a failed run.
pub fn flows(args: &FlowsArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let folded = fold_flows(
        &args.archive,
        args.last.as_ref(),
        args.redaction.redactor(),
        args.clusters,
        args.flows,
    )?;
    let markdown = FlowsReport {
        window: args.last.as_ref(),
        clusters: args.clusters,
        flows: args.flows,
        generated_at: folded.generated_at,
        found: &folded.flows,
        instrumentation: &folded.instrumentation,
    }
    .render();

    out.write_all(markdown.as_bytes())
        .map_err(CommandError::Output)
}

/// Reads the transcripts behind the hits a ranking is about to show, and searches each one for the
/// query's own terms (qanungo #10's `--verbatim`).
///
/// # The bound is the design
///
/// This escalates into `ask.hits` — the sessions the summary ranking selected and `--limit` kept —
/// and nothing else. Decision 12 chose the summary substrate precisely so the lane would not have
/// to mirror every transcript in the archive, and an unbounded `--verbatim` would put that cost
/// back on every run: an archive-scale download to answer a question that might match nothing. So
/// the archive traffic here is at most one session per shown hit, most of them cache hits, and the
/// coverage boundary that comes with it is inherited rather than hidden — a fact no summary
/// mentions belongs to a session `ask` never ranked, so it is a transcript this never opens. The
/// document says exactly that above the blocks.
///
/// Every shown hit gets an answer: what its transcript said, or why it could not be read. A hit the
/// escalation could not look at is never left looking like one with nothing to show.
///
/// One session at a time, deliberately: the mirror runs a worker pool because it moves hundreds of
/// artifacts, and this moves at most `--limit` of them. A second pool for ten sessions would be
/// concurrency against a LAN archive to save a second the reader would not notice — measured
/// against production, a warm escalation over three hits is ~30 ms and a cold one ~760 ms.
fn escalate(
    prepared: &Prepared,
    ask: &mut Ask,
    searched: &[&MirroredSession],
    query: &Query,
    redactor: &Redactor,
) -> VerbatimStats {
    let started = Instant::now();
    let mut stats = VerbatimStats::default();
    // Collected separately and absorbed once, because it belongs to the same footer total the
    // ranking's own scrub reports: a marker in an excerpt under "redaction none" would be the
    // document contradicting itself.
    let mut redaction = RedactionReport::default();
    for hit in &mut ask.hits {
        // In range by construction: `searched_index` is the position `Ask::fold` read this hit
        // from, in the very slice `fold_ask` built alongside `searched`.
        let session = searched
            .get(hit.searched_index)
            .expect("a hit indexes the slice it was folded from");
        hit.verbatim = Some(match dig(prepared, session, query, redactor, &mut stats) {
            Ok(found) => {
                stats.transcripts_searched += 1;
                stats.matches += found.total_matches;
                stats.shown += found.matches.len();
                stats.unreadable_records += found.unreadable_records;
                redaction.absorb(&found.redaction);
                Escalation::Searched(found)
            }
            Err(skip) => {
                stats.transcripts_unavailable += 1;
                Escalation::Unavailable(skip_line(&skip, redactor, &mut redaction))
            }
        });
    }
    ask.redaction.absorb(&redaction);
    stats.elapsed = started.elapsed();
    stats
}

/// Fetches one hit's transcript and searches it, counting what the archive was asked for.
///
/// The fetch is [`sync::fetch`] — the mirror's own resolution walk for a single session, so a
/// transcript is found, verified, and cached here exactly as `report` would have found it — and the
/// search is [`verbatim::search`] over the typed records, never the file's bytes.
///
/// # Errors
///
/// Returns the [`Skip`] a mirror run for this session's transcript would have recorded: no snapshot
/// carries one, no interpreter reads it, the archive could not be reached, or the cached blob could
/// not be opened or interpreted. The caller states it under the hit.
fn dig(
    prepared: &Prepared,
    session: &MirroredSession,
    query: &Query,
    redactor: &Redactor,
    stats: &mut VerbatimStats,
) -> Result<crate::verbatim::SessionVerbatim, Skip> {
    let fetched = sync::fetch(
        &prepared.client,
        &prepared.cache,
        Artifact::Transcript,
        session,
    )?;
    // Counted whether or not the search below succeeds: the traffic happened either way, and a
    // footer that only counted successful digs would under-report what the run cost the archive.
    if fetched.lookup == crate::cache::Lookup::Miss {
        stats.transcripts_fetched += 1;
    }
    stats.bytes_transferred += fetched.transferred_bytes;
    stats.snapshots_fetched += fetched.snapshots_fetched;

    let unreadable = |detail: String| Skip {
        source_agent: fetched.source_agent.clone(),
        reason: SkipReason::Unreadable(detail),
    };
    // Unreachable through `sync::fetch`, which refuses a snapshot whose transcript this build has
    // no interpreter for — stated rather than asserted, so the two never disagree silently.
    let source = metrics::source_for_agent(&fetched.source_agent).ok_or_else(|| Skip {
        source_agent: fetched.source_agent.clone(),
        reason: SkipReason::UnknownAgent(fetched.source_agent.clone()),
    })?;
    let blob = prepared
        .cache
        .open_blob(&fetched.source_hash)
        .map_err(|error| unreadable(format!("cache read failed: {error}")))?;
    let found = crate::verbatim::search(
        source,
        fetched.artifact_set_version,
        BufReader::new(blob),
        query,
        redactor,
    )
    .map_err(|error| unreadable(error.to_string()))?;
    // The archive's declared original size, already verified against the transferred bytes, so the
    // footer needs no second pass over the file to say how much transcript was read.
    stats.bytes_searched += fetched.size_bytes;
    Ok(found)
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
    /// The connected read client, kept rather than dropped with the mirror that used it: a lane
    /// that decides *after* the fold that it wants one more artifact — `ask --verbatim` — asks the
    /// same client for it, on the same connection settings, rather than opening a second one that
    /// could be pointed somewhere else.
    client: ReadClient,
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
        let activity_from = comparison_opens_at.unwrap_or(opens_at);
        Self::mirror_since(
            archive,
            artifact,
            generated_at,
            opens_at,
            comparison_opens_at,
            activity_from,
        )
    }

    /// Opens the cache and mirrors *all* of history: list from the epoch, no comparison window, and
    /// — because `opens_at` is the epoch too — [`Placement`] then puts every listed session in the
    /// reported half. What a lane that ranks across the whole archive rather than trending two
    /// windows needs (qanungo #10). It is a distinct entry point rather than a `--last` sentinel so
    /// the window grammar keeps meaning one thing, and the "search everything" intent is a shape in
    /// the code rather than a magic duration.
    fn mirror_all(archive: &ArchiveArgs, artifact: Artifact) -> Result<Self, CommandError> {
        let generated_at = Utc::now();
        let epoch = DateTime::<Utc>::from_timestamp(0, 0)
            .expect("the Unix epoch is a representable instant");
        Self::mirror_since(archive, artifact, generated_at, epoch, None, epoch)
    }

    /// The shared tail of both constructors: open the cache, connect, and mirror from an explicit
    /// lower bound. The three instants the window is cut on are passed in so the two callers agree
    /// exactly on how a session is placed, the way the two document lanes already had to.
    fn mirror_since(
        archive: &ArchiveArgs,
        artifact: Artifact,
        generated_at: DateTime<Utc>,
        opens_at: DateTime<Utc>,
        comparison_opens_at: Option<DateTime<Utc>>,
        activity_from: DateTime<Utc>,
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

        let activity_from = activity_from.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
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
            client,
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
        // The listing's own projection, carried the way the cost lane already carries it, so a
        // presentation can cut the window by repository without a second fold. Nothing below reads
        // it: no rule, no score, no pack entry, no Markdown cell.
        repository: mirrored.repository.clone(),
        // The same listing row's archive time, taken from the same place and for the same reason
        // the cost lane already takes it: this is the clock `Placement` cut the window on, so it is
        // the only clock a per-day count can be made to reconcile against the window's own session
        // count. Nothing below reads it either.
        archived_at: mirrored.archived_at,
        // Off the snapshot's manifest, carried the same presentation-only way: a per-device scope
        // and an activity heatmap read them, nothing below does.
        hostname: mirrored.hostname.clone(),
        utc_offset: mirrored.utc_offset.clone(),
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
/// reaches [`SkipReason::UnknownAgent`] — so it goes through [`crate::evidence::identifier_field`], which
/// clamps it and *then* scrubs it, before it is interpolated. Both lanes print these lines verbatim
/// in their Gaps section, so treating the label here rather than at either rendering site is what
/// keeps the two from drifting apart, and is why the coaching report's own claim to render
/// "aggregates, tool names, and `source_hash` references" survives contact with a hostile archive.
///
/// The clamp alone was not enough. It refuses a label that could break the line's *shape* — a pipe,
/// a backtick, a newline, anything over the identifier ceiling — but a credential is shaped exactly
/// like an ordinary identifier, so a `source_agent` holding one would have rendered raw in all
/// three documents and in the dashboard's `provenance.gaps`. Clamping first is still what keeps an
/// over-length token from laundering itself into a renderable marker; see
/// [`crate::evidence::identifier_field`] for the ordering argument in full.
///
/// The rest of the sentence is this build's own text: a logical path from [`Artifact`], a fixed
/// clause, or the locally generated error prose a [`SkipReason::Unreadable`] carries — paths, URLs,
/// and transport failures, never transcript content, and with every archive-stated value inside it
/// (Patwari's `error.code`, a compression header, a content digest) already clamped where it was
/// parsed.
fn summarize(
    skipped: &[Skip],
    unplaceable: usize,
    redactor: &Redactor,
    redaction: &mut RedactionReport,
) -> Vec<SkippedNote> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for skip in skipped {
        // The coaching and cost documents have no footer that says what the scrub fired and pass a
        // throwaway report; the doctor lane does have one, and carries what a label cost into it —
        // see [`skip_line`] for why the two passes are spelled out rather than delegated.
        let reason = skip_line(skip, redactor, redaction);
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

/// One skipped session as a sentence, with the archive's harness label clamped and then scrubbed.
///
/// The body of what [`summarize`] used to do inline, lifted out because a second surface now states
/// exactly the same fact about exactly one session: a hit whose transcript the `--verbatim`
/// escalation could not read ([`escalate`]). Two spellings of "no snapshot has a transcript" in one
/// binary would be two ways for the same gap to read differently, so there is one.
///
/// It spells [`crate::evidence::identifier_field`] out rather than calling it — the clamp first, then the
/// scrub, for the ordering reason that helper argues — so that the scrub can be *counted*. The ask
/// lane's footer says what fired, and a marker where a harness name belongs with `redaction none`
/// beneath it would be the document contradicting itself. [`summarize`]'s own callers have no such
/// footer and throw the count away.
fn skip_line(skip: &Skip, redactor: &Redactor, redaction: &mut RedactionReport) -> String {
    let mut label = |value: &str| {
        let scrubbed = redactor.scrub(&format::identifier(value));
        redaction.absorb(&scrubbed.report);
        scrubbed.text
    };
    let agent = label(&skip.source_agent);
    match &skip.reason {
        SkipReason::MissingArtifact(artifact) => format!(
            "{agent}: snapshot has no `{}` artifact",
            artifact.logical_path()
        ),
        SkipReason::UnknownAgent(named) => format!(
            "{}: no interpreter for this harness in this build",
            label(named),
        ),
        SkipReason::Unreadable(detail) => format!("{agent}: {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GitHub token's shape — `ghp_` and exactly 36 base62 characters — and not a real one.
    /// Well inside [`format::MAX_IDENTIFIER_CHARS`] and carrying nothing the clamp refuses, which
    /// is the whole point: it is exactly as renderable as `claude-code` is.
    const TOKEN_SHAPED: &str = "ghp_FAKEfake0123456789ABCDEFabcdef012345";

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
        let notes = summarize(&skips, 0, &Redactor::new(), &mut RedactionReport::default());
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
            &Redactor::new(),
            &mut RedactionReport::default(),
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
            &Redactor::new(),
            &mut RedactionReport::default(),
        );
        assert_eq!(
            ordinary[0].reason,
            "claude-code: snapshot has no `transcript.jsonl` artifact",
        );
    }

    /// The clamp's blind spot, and the reason this line takes a redactor at all.
    ///
    /// A credential is shaped exactly like a harness name — no pipe, no backtick, no control
    /// character, far under the length ceiling — so [`format::identifier`] hands it straight back
    /// and a listing whose `source_agent` holds one would print it, verbatim, in the Gaps section
    /// of all three documents and in the dashboard's `provenance.gaps`.
    ///
    /// This test is the mutation guard for that: swap [`crate::evidence::identifier_field`] back to
    /// [`format::identifier`] and the token appears where the marker is asserted, so the assertion
    /// below fails rather than the property quietly regressing.
    #[test]
    fn a_credential_shaped_harness_label_is_scrubbed_and_not_only_clamped() {
        // The clamp on its own is the state this test exists to refuse: it is a no-op here.
        assert_eq!(format::identifier(TOKEN_SHAPED), TOKEN_SHAPED);

        let notes = summarize(
            &[
                Skip {
                    source_agent: TOKEN_SHAPED.to_owned(),
                    reason: SkipReason::MissingArtifact(Artifact::Transcript),
                },
                Skip {
                    source_agent: TOKEN_SHAPED.to_owned(),
                    reason: SkipReason::UnknownAgent(TOKEN_SHAPED.to_owned()),
                },
                Skip {
                    source_agent: TOKEN_SHAPED.to_owned(),
                    reason: SkipReason::Unreadable("cache read failed".to_owned()),
                },
            ],
            0,
            &Redactor::new(),
            &mut RedactionReport::default(),
        );
        assert_eq!(notes.len(), 3, "one line per reason");
        for note in &notes {
            assert!(
                !note.reason.contains(TOKEN_SHAPED),
                "the token survived: {}",
                note.reason,
            );
            assert!(
                note.reason.starts_with("[REDACTED:github-token]: "),
                "the label is a marker: {}",
                note.reason,
            );
        }
    }

    /// Why the clamp runs *first*, pinned rather than only argued.
    ///
    /// An over-length label is not a renderable identifier whatever it turns out to contain, so it
    /// is replaced wholesale. Scrubbing first would let this one launder itself: two markers and a
    /// space is 47 characters, comfortably renderable, and the clamp would then wave through a
    /// value it exists to refuse.
    #[test]
    fn an_over_length_credential_shaped_label_still_clamps() {
        let over_length = format!("{TOKEN_SHAPED} {TOKEN_SHAPED}");
        assert!(over_length.chars().count() > format::MAX_IDENTIFIER_CHARS);

        let notes = summarize(
            &[Skip {
                source_agent: over_length,
                reason: SkipReason::MissingArtifact(Artifact::Transcript),
            }],
            0,
            &Redactor::new(),
            &mut RedactionReport::default(),
        );
        assert_eq!(
            notes[0].reason,
            format!(
                "{}: snapshot has no `transcript.jsonl` artifact",
                format::INVALID_IDENTIFIER
            ),
        );
        assert!(
            !notes[0].reason.contains("[REDACTED:"),
            "{}",
            notes[0].reason
        );
        assert!(
            !notes[0].reason.contains(TOKEN_SHAPED),
            "{}",
            notes[0].reason
        );
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn mirrored(archived_at: Option<&str>) -> MirroredSession {
        MirroredSession {
            session_id: "1".repeat(32),
            snapshot_id: "2".repeat(32),
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            size_bytes: 0,
            archived_at: archived_at.map(at),
            repository: None,
            hostname: None,
            utc_offset: None,
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
        let notes = summarize(&[], 3, &Redactor::new(), &mut RedactionReport::default());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].count, 3);
        assert!(notes[0].reason.contains("could not place in either window"));
    }
}
