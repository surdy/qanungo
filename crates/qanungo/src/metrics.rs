//! The fold: exactly three metrics over `munshi-transcript`'s typed events.
//!
//! Everything here is derivable from what the interpreter types *today* — no new transcript
//! parsing, no schema changes upstream:
//!
//! 1. **Tool error rate** — per session and per tool name, from the outcome fields a
//!    [`ToolEvent`] carries (`success`, `is_error`).
//! 2. **Tool-per-request ratio** — [`SessionSummary::tool_activities`] over
//!    [`SessionSummary::user_requests`]: how much the agent does per thing it is asked for.
//! 3. **Cadence and duration** — sessions per day, and each session's *active time*: the sum of
//!    the gaps between consecutive records, with anything longer than
//!    [`IDLE_GAP`](crate::rules::thresholds::IDLE_GAP) treated as the operator having walked
//!    away. Wall-clock span is still derived, but as context rather than as the number a rule
//!    decides on — see [`Activity`].
//!
//! # What counts as a tool outcome
//!
//! Only events that carry an *explicit* outcome signal enter the error-rate denominator:
//!
//! - a `success` field — Copilot's `tool.execution_complete`, which states `true`/`false`;
//! - a `tool_result` event — Claude Code's result block, where an absent `is_error` means the
//!   call succeeded.
//!
//! Invocations (`tool_use`, `tool.execution_start`, `external_tool.requested`), `skill.invoked`,
//! bare completion markers (`external_tool.completed`), and Codex's `function_call_output` carry
//! no outcome signal at all, so they count as tool *activity* but never as an attempt that
//! could have failed. That is a deliberate under-claim: a session whose harness cannot express
//! tool failure reports no error rate rather than a flattering zero. Codex transcripts are
//! entirely in that position today.
//!
//! # Attributing an outcome to a tool name
//!
//! Outcome events name the call, not the tool: Claude Code's `tool_result` carries only
//! `tool_use_id`, and Copilot's completion only `toolCallId`. The fold therefore remembers the
//! call id -> tool name mapping from the invocation events it has already streamed past, which
//! is sound because every harness emits the invocation before its result. An outcome whose call
//! id was never introduced is counted in the session total and in
//! [`ToolOutcomes::unattributed`], never guessed at.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::BufRead;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use munshi_transcript::{
    Classification, Event, SessionSummary, Source, ToolEvent, TranscriptStream, UnsupportedVersion,
};

use crate::rules::thresholds::IDLE_GAP;

/// Upper bound on remembered call-id -> tool-name pairs per session. A transcript is one
/// conversation, so this is orders of magnitude above any real session; it exists so a
/// pathological or adversarial transcript cannot make the fold's memory grow without bound.
const MAX_CORRELATED_CALLS: usize = 100_000;

/// Attempts and failures for one tool, or for a whole session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolTally {
    /// Tool calls that reported an outcome.
    pub attempts: u64,
    /// Of those, the ones that reported failure.
    pub errors: u64,
}

impl ToolTally {
    fn observe(&mut self, succeeded: bool) {
        self.attempts += 1;
        if !succeeded {
            self.errors += 1;
        }
    }

    /// The failure fraction, or `None` when nothing reported an outcome — an undefined rate and
    /// a zero rate must not be confused by a rule.
    pub fn error_rate(&self) -> Option<f64> {
        (self.attempts > 0).then(|| self.errors as f64 / self.attempts as f64)
    }
}

/// Tool outcomes for one session, in total and by tool name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOutcomes {
    pub total: ToolTally,
    /// Keyed by the tool name the harness recorded (`Bash`, `Edit`, `local_shell`, ...). Tool
    /// names are schema metadata, not conversation content, and are the one string a report may
    /// render verbatim.
    pub by_tool: BTreeMap<String, ToolTally>,
    /// Outcomes whose call id was never introduced by an invocation, so no tool name could be
    /// attributed. Counted rather than guessed.
    pub unattributed: u64,
}

/// Gap-aware activity for one session: how much of its span was actually worked, and in how many
/// separate sittings.
///
/// # Why span is not the answer (qanungo #14)
///
/// A transcript is one *conversation*, not one *stretch of work*: an operator resumes a session
/// tomorrow and the file goes on where it stopped. Across the 2026-08-18 archive (564
/// transcripts, 575k dated records), **95.8% of summed wall-clock span is idle time** and 59.6%
/// of sessions are resumed at least once, so span overstates the work a session took by a median
/// factor of 4.9 and a p90 factor of 119.
///
/// # The fold
///
/// One pass, in *file order*, carrying a single previous timestamp and four accumulators:
///
/// ```text
/// gap_i           = max(0, t_{i+1} − t_i)      // inversions clamp to zero, records are not sorted
/// active_time     = Σ gap_i where gap_i ≤ IDLE_GAP
/// sitting         = a maximal run whose internal gaps are all ≤ IDLE_GAP
/// longest_sitting = the longest such run's own span
/// sittings        = 1 + count(gap_i > IDLE_GAP)
/// ```
///
/// Records arrive slightly out of order in 30% of sessions (0.52% of adjacencies, almost all
/// sub-second interleaving of a tool result with the message that provoked it). Sorting would
/// mean buffering every timestamp in a transcript that can reach hundreds of megabytes, and it
/// changes which sessions clear a two-hour sitting by **two** across the whole archive — so the
/// fold clamps and stays streaming.
///
/// A run's span is accumulated as the sum of its own internal gaps rather than as last − first,
/// which is the same number for ordered records and cannot go negative for inverted ones. The
/// price of the clamp is on the other side: an inverted pair re-traverses an interval the fold
/// has already counted, so on an out-of-order transcript the summed gaps can *exceed* last −
/// first, and `active_time` or `longest_sitting` can come out larger than
/// [`SessionMetrics::span`]. That is the accepted trade for staying streaming — the affected
/// adjacencies are 0.52% of the archive and almost all sub-second — but it means the two
/// quantities are not guaranteed to order, and nothing here should assume they do.
///
/// Every reading is `None` for a session with fewer than two dated records: with no adjacency
/// there is no gap, and a lone record's activity is undefined rather than zero — the same
/// discipline [`SessionMetrics::span`] already applies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    /// Records that carried a parseable timestamp. Two are needed before anything is defined.
    dated_records: u64,
    /// Σ of the gaps that were short enough to count as work.
    active: TimeDelta,
    /// The longest *finished* run's span; the run in progress is folded in on read.
    longest_finished_sitting: TimeDelta,
    /// The span of the run currently being accumulated.
    current_sitting: TimeDelta,
    /// Gaps over [`IDLE_GAP`], each of which ended a sitting.
    breaks: u64,
    /// The carried previous timestamp — the fold's entire memory.
    previous: Option<DateTime<Utc>>,
}

impl Activity {
    /// Folds the dated records of one transcript, which must be offered in file order.
    pub fn over(timestamps: impl IntoIterator<Item = DateTime<Utc>>) -> Self {
        let mut activity = Self::default();
        for at in timestamps {
            activity.observe(at);
        }
        activity
    }

    /// Folds one dated record.
    pub fn observe(&mut self, at: DateTime<Utc>) {
        if let Some(previous) = self.previous {
            let gap = (at - previous).max(TimeDelta::zero());
            if gap <= IDLE_GAP {
                self.active += gap;
                self.current_sitting += gap;
            } else {
                self.longest_finished_sitting =
                    self.longest_finished_sitting.max(self.current_sitting);
                self.current_sitting = TimeDelta::zero();
                self.breaks += 1;
            }
        }
        self.previous = Some(at);
        self.dated_records += 1;
    }

    /// Whether this session had two dated records to put a gap between.
    fn measurable(&self) -> bool {
        self.dated_records >= 2
    }

    /// Time inside sittings: the session's work, as opposed to its calendar footprint.
    pub fn active_time(&self) -> Option<TimeDelta> {
        self.measurable().then_some(self.active)
    }

    /// The longest continuous stretch of work — what "marathon" actually means.
    pub fn longest_sitting(&self) -> Option<TimeDelta> {
        self.measurable()
            .then(|| self.longest_finished_sitting.max(self.current_sitting))
    }

    /// How many times the session was picked back up, counting the first sitting.
    pub fn sittings(&self) -> Option<usize> {
        self.measurable().then(|| {
            usize::try_from(self.breaks)
                .unwrap_or(usize::MAX)
                .saturating_add(1)
        })
    }
}

/// The metrics folded out of one session's transcript, plus the evidence needed to cite it.
#[derive(Debug, Clone)]
pub struct SessionMetrics {
    /// The content hash Patwari serves for this transcript — the report's only evidence handle.
    pub source_hash: String,
    /// The harness that produced the transcript (`claude-code`, `copilot-cli`, `codex-cli`).
    pub source_agent: String,
    /// The counting fold `munshi-transcript` already defines, restated over the same stream.
    pub summary: SessionSummary,
    pub tools: ToolOutcomes,
    /// The gap-aware reading of the same timestamps the summary took its span from.
    pub activity: Activity,
    /// Transcript bytes read, for the fold-cost footer.
    pub bytes_folded: u64,
}

impl SessionMetrics {
    /// Wall-clock span between the first and last dated record.
    ///
    /// Kept as **context**, not as an input to any rule: a 119h span across 43 sittings and a
    /// 119h span worked straight through would read identically here, and only one of them is a
    /// finding. See [`Activity`].
    pub fn span(&self) -> Option<TimeDelta> {
        self.summary
            .first_timestamp
            .zip(self.summary.last_timestamp)
            .map(|(first, last)| last - first)
    }

    /// The UTC calendar day the session started on, for the cadence fold.
    pub fn day(&self) -> Option<NaiveDate> {
        self.summary.first_timestamp.map(|first| first.date_naive())
    }

    /// Time spent inside sittings — the duration a coaching report reasons about.
    pub fn active_time(&self) -> Option<TimeDelta> {
        self.activity.active_time()
    }

    /// The session's longest continuous stretch of work.
    pub fn longest_sitting(&self) -> Option<TimeDelta> {
        self.activity.longest_sitting()
    }

    /// How many separate sittings the transcript was worked in.
    pub fn sittings(&self) -> Option<usize> {
        self.activity.sittings()
    }

    /// How much larger the session's calendar footprint is than the work inside it. `None` when
    /// there is no gap to measure. A session whose records are *all* more than [`IDLE_GAP`] apart
    /// has zero active time, and this is then infinite — a maximally diluted transcript, which is
    /// exactly what the resumed-session rule is looking for. A session whose span is also zero
    /// (every record on the same instant) yields `NaN`, which compares false against every
    /// threshold and so fires nothing.
    /// Taken in milliseconds rather than seconds: a session whose entire activity is under a
    /// second is a real shape (a burst of records, then nothing), and truncating it to zero would
    /// report it as having no work at all rather than as having very little.
    pub fn span_to_active(&self) -> Option<f64> {
        let (span, active) = self.span().zip(self.active_time())?;
        Some(span.num_milliseconds() as f64 / active.num_milliseconds() as f64)
    }

    /// Tool activities per user request, or `None` when the session recorded no user request.
    pub fn tools_per_request(&self) -> Option<f64> {
        (self.summary.user_requests > 0)
            .then(|| self.summary.tool_activities as f64 / self.summary.user_requests as f64)
    }
}

/// What one transcript fold produced, before it is paired with its archive identity.
#[derive(Debug, Clone, Default)]
pub struct Fold {
    pub summary: SessionSummary,
    pub tools: ToolOutcomes,
    pub activity: Activity,
}

/// Folds one transcript, streaming: one pass, no buffering of records, memory bounded by the
/// per-tool tallies and the call-id correlation map.
///
/// # Errors
///
/// Returns an error when `artifact_set_version` names an artifact contract this build's
/// interpreter does not support — the transcript is then left uninterpreted rather than read
/// with stale assumptions.
pub fn fold_transcript(
    source: Source,
    artifact_set_version: u16,
    reader: impl BufRead,
) -> Result<Fold, UnsupportedVersion> {
    let stream = TranscriptStream::new(source, artifact_set_version, reader)?;
    let mut fold = Fold::default();
    let mut names: HashMap<String, String> = HashMap::new();
    for item in stream {
        fold.summary.observe(&item);
        let Ok(record) = &item else { continue };
        // In file order, every dated record — content or bookkeeping — is evidence that somebody
        // or something was still at the keyboard, so the gap fold sees all of them.
        if let Some(at) = record.timestamp {
            fold.activity.observe(at);
        }
        if let Classification::Content { events } = &record.classification {
            for event in events {
                if let Event::Tool(tool) = event {
                    observe_tool(&mut fold.tools, &mut names, tool);
                }
            }
        }
    }
    Ok(fold)
}

/// Folds one tool event: remember what it names, then count what it reports.
fn observe_tool(
    outcomes: &mut ToolOutcomes,
    names: &mut HashMap<String, String>,
    tool: &ToolEvent,
) {
    if let (Some(call_id), Some(name)) = (tool.call_id(), tool.name())
        && names.len() < MAX_CORRELATED_CALLS
    {
        names.insert(call_id.to_owned(), name.to_owned());
    }
    let Some(succeeded) = outcome(tool) else {
        return;
    };
    outcomes.total.observe(succeeded);
    let name = tool.name().map(ToOwned::to_owned).or_else(|| {
        tool.call_id()
            .and_then(|call_id| names.get(call_id).cloned())
    });
    match name {
        Some(name) => outcomes.by_tool.entry(name).or_default().observe(succeeded),
        None => outcomes.unattributed += 1,
    }
}

/// Whether this event reports a tool outcome, and whether that outcome succeeded. See the module
/// docs for why only these two shapes count.
fn outcome(tool: &ToolEvent) -> Option<bool> {
    if let Some(success) = tool.fields.get("success") {
        return Some(success == "true");
    }
    if tool.event() == Some("tool_result") {
        return Some(tool.fields.get("is_error").map(String::as_str) != Some("true"));
    }
    None
}

/// Maps a manifest's `session.source_agent` label onto the interpreter that reads that harness's
/// transcripts. An unrecognized harness is reported as such rather than guessed at.
pub fn source_for_agent(source_agent: &str) -> Option<Source> {
    match source_agent {
        "copilot-cli" | "copilot" => Some(Source::Copilot),
        "claude-code" | "claude" => Some(Source::ClaudeCode),
        "codex-cli" | "codex" => Some(Source::Codex),
        _ => None,
    }
}

/// Sessions per day across the reported window, and the span distribution behind it.
///
/// **Days are UTC calendar days**, not the operator's local days, because that is the only
/// boundary the transcripts themselves define — record timestamps are RFC 3339 and normalized to
/// UTC by `munshi-transcript`, and no capture records the harness's local zone. A late-evening
/// session west of Greenwich therefore lands on the following UTC day, so "active days" and
/// "busiest day" can disagree with a local-time intuition by one. The report says UTC where it
/// prints them; correcting for a real local zone is a later question, not a silent guess here.
#[derive(Debug, Clone, Default)]
pub struct Cadence {
    /// Sessions per UTC calendar day, by the day each session's first record landed on.
    pub per_day: BTreeMap<NaiveDate, usize>,
    /// Sessions whose records carried no parseable timestamp at all.
    pub undated: usize,
    /// Every *dated* session's wall-clock span, ascending. Context for the actives below.
    ///
    /// The two series do not cover the same sessions, and the difference is not an oversight: a
    /// session with a single dated record has a span (of zero — first and last are the same
    /// record) but no measurable activity, so it appears here and not in `actives`. Comparing
    /// their lengths, or pairing them off by index, would be wrong.
    pub spans: Vec<TimeDelta>,
    /// Every *measurable* session's active time, ascending — the duration distribution a report
    /// reasons about. Sessions with fewer than two dated records are absent, per [`Activity`].
    pub actives: Vec<TimeDelta>,
    /// Active time summed over the window.
    pub total_active: TimeDelta,
    /// Wall-clock span summed over the window. Larger than [`Cadence::total_active`] by a factor
    /// of roughly twenty across the real archive, which is the whole point of reporting both.
    pub total_span: TimeDelta,
    /// Sittings summed over the window.
    pub total_sittings: usize,
}

impl Cadence {
    /// Folds the cadence metric over already-folded sessions.
    pub fn fold(sessions: &[SessionMetrics]) -> Self {
        let mut cadence = Self::default();
        for session in sessions {
            match session.day() {
                Some(day) => *cadence.per_day.entry(day).or_default() += 1,
                None => cadence.undated += 1,
            }
            if let Some(span) = session.span() {
                cadence.spans.push(span);
                cadence.total_span += span;
            }
            if let Some(active) = session.active_time() {
                cadence.actives.push(active);
                cadence.total_active += active;
            }
            cadence.total_sittings += session.sittings().unwrap_or_default();
        }
        cadence.spans.sort_unstable();
        cadence.actives.sort_unstable();
        cadence
    }

    /// Days on which at least one session started.
    pub fn active_days(&self) -> usize {
        self.per_day.len()
    }

    /// Sessions per *active* day. Reported instead of a flat per-calendar-day rate because a
    /// week with four sessions on one day is a different working pattern from one with four
    /// sessions spread across four days, and the flat rate hides the difference.
    pub fn sessions_per_active_day(&self) -> Option<f64> {
        let days = self.active_days();
        (days > 0).then(|| self.per_day.values().sum::<usize>() as f64 / days as f64)
    }

    /// The median session span, taken as the lower median so the value is always an observed
    /// span rather than an interpolated one.
    pub fn median_span(&self) -> Option<TimeDelta> {
        median(&self.spans)
    }

    /// The longest session span in the window.
    pub fn longest_span(&self) -> Option<TimeDelta> {
        self.spans.last().copied()
    }

    /// The median session's active time, on the same lower-median convention.
    pub fn median_active(&self) -> Option<TimeDelta> {
        median(&self.actives)
    }

    /// The most active time any one session in the window carried.
    pub fn longest_active(&self) -> Option<TimeDelta> {
        self.actives.last().copied()
    }
}

/// The lower median of an ascending series, so the reported value is always one that was
/// observed.
fn median(ascending: &[TimeDelta]) -> Option<TimeDelta> {
    ascending
        .get(ascending.len().saturating_sub(1) / 2)
        .copied()
}

/// Window-wide totals, for the report's aggregate section.
#[derive(Debug, Clone, Default)]
pub struct Totals {
    pub sessions: usize,
    pub user_requests: usize,
    pub assistant_messages: usize,
    pub tool_activities: usize,
    pub malformed_records: usize,
    pub tools: ToolTally,
    pub by_tool: BTreeMap<String, ToolTally>,
    pub by_agent: BTreeMap<String, usize>,
    pub bytes_folded: u64,
    /// The window's first and last observed record timestamps.
    pub first_timestamp: Option<DateTime<Utc>>,
    pub last_timestamp: Option<DateTime<Utc>>,
}

impl Totals {
    /// Sums the folded sessions.
    pub fn fold(sessions: &[SessionMetrics]) -> Self {
        let mut totals = Self {
            sessions: sessions.len(),
            ..Self::default()
        };
        for session in sessions {
            totals.user_requests += session.summary.user_requests;
            totals.assistant_messages += session.summary.assistant_messages;
            totals.tool_activities += session.summary.tool_activities;
            totals.malformed_records += session.summary.malformed_records;
            totals.tools.attempts += session.tools.total.attempts;
            totals.tools.errors += session.tools.total.errors;
            totals.bytes_folded += session.bytes_folded;
            *totals
                .by_agent
                .entry(session.source_agent.clone())
                .or_default() += 1;
            for (name, tally) in &session.tools.by_tool {
                let entry = totals.by_tool.entry(name.clone()).or_default();
                entry.attempts += tally.attempts;
                entry.errors += tally.errors;
            }
            if let Some(first) = session.summary.first_timestamp {
                totals.first_timestamp =
                    Some(totals.first_timestamp.map_or(first, |old| old.min(first)));
            }
            if let Some(last) = session.summary.last_timestamp {
                totals.last_timestamp =
                    Some(totals.last_timestamp.map_or(last, |old| old.max(last)));
            }
        }
        totals
    }

    /// Window-wide tool activities per user request.
    pub fn tools_per_request(&self) -> Option<f64> {
        (self.user_requests > 0).then(|| self.tool_activities as f64 / self.user_requests as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold_claude(transcript: &str) -> Fold {
        fold_transcript(Source::ClaudeCode, 2, transcript.as_bytes()).unwrap()
    }

    fn metrics(fold: Fold) -> SessionMetrics {
        SessionMetrics {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            summary: fold.summary,
            tools: fold.tools,
            activity: fold.activity,
            bytes_folded: 0,
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// An activity folded from a start time plus the gaps, in minutes, between records.
    fn activity(gaps_minutes: &[i64]) -> Activity {
        let mut at = at("2026-08-01T09:00:00Z");
        let mut timestamps = vec![at];
        for gap in gaps_minutes {
            at += TimeDelta::minutes(*gap);
            timestamps.push(at);
        }
        Activity::over(timestamps)
    }

    /// A Claude Code transcript with `calls` tool calls, the first `failures` of which fail.
    fn claude_tool_transcript(tool: &str, calls: usize, failures: usize) -> String {
        let mut lines = vec![format!(
            r#"{{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"user","content":"kickoff"}}}}"#
        )];
        for index in 0..calls {
            lines.push(format!(
                r#"{{"type":"assistant","uuid":"a{index}","timestamp":"2026-08-01T10:0{}:00.000Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_{index}","name":"{tool}","input":{{}}}}]}}}}"#,
                index % 10
            ));
            let is_error = index < failures;
            lines.push(format!(
                r#"{{"type":"user","uuid":"r{index}","timestamp":"2026-08-01T10:0{}:30.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_{index}","content":"result","is_error":{is_error}}}]}}}}"#,
                index % 10
            ));
        }
        lines.join("\n")
    }

    #[test]
    fn tool_error_rate_counts_only_outcome_bearing_events() {
        let fold = fold_claude(&claude_tool_transcript("Bash", 4, 1));
        // Eight tool events (four invocations, four results); only the results report outcomes.
        assert_eq!(fold.summary.tool_activities, 8);
        assert_eq!(fold.tools.total.attempts, 4);
        assert_eq!(fold.tools.total.errors, 1);
        assert!((fold.tools.total.error_rate().unwrap() - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn tool_error_rate_is_attributed_per_tool_name_through_the_call_id() {
        let mut transcript = claude_tool_transcript("Bash", 2, 2);
        transcript.push('\n');
        transcript.push_str(&claude_tool_transcript("Read", 2, 0));
        let fold = fold_claude(&transcript);
        assert_eq!(
            fold.tools.by_tool["Bash"],
            ToolTally {
                attempts: 2,
                errors: 2
            }
        );
        assert_eq!(
            fold.tools.by_tool["Read"],
            ToolTally {
                attempts: 2,
                errors: 0
            }
        );
        assert_eq!(fold.tools.unattributed, 0);
    }

    /// Claude Code omits `is_error` entirely on a successful tool result — the classifier only
    /// ever inserts the field when it is `true`. An absent field must therefore read as success,
    /// not as "no outcome", or every clean session would report an undefined error rate.
    #[test]
    fn a_tool_result_without_an_is_error_field_counts_as_a_success() {
        let transcript = concat!(
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:01.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"file contents"}]}}"#,
        );
        let fold = fold_claude(transcript);
        assert_eq!(
            fold.tools.total,
            ToolTally {
                attempts: 1,
                errors: 0
            }
        );
        assert_eq!(fold.tools.total.error_rate(), Some(0.0));
        assert_eq!(
            fold.tools.by_tool["Read"],
            ToolTally {
                attempts: 1,
                errors: 0
            }
        );
    }

    /// The inverse of the above, so the pair pins both readings of the same absent field.
    #[test]
    fn a_tool_result_with_is_error_true_counts_as_a_failure() {
        let transcript = concat!(
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:01.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"no such file","is_error":true}]}}"#,
        );
        let fold = fold_claude(transcript);
        assert_eq!(
            fold.tools.total,
            ToolTally {
                attempts: 1,
                errors: 1
            }
        );
    }

    #[test]
    fn an_outcome_whose_call_was_never_introduced_is_counted_not_guessed() {
        let transcript = r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_orphan","content":"result","is_error":true}]}}"#;
        let fold = fold_claude(transcript);
        assert_eq!(
            fold.tools.total,
            ToolTally {
                attempts: 1,
                errors: 1
            }
        );
        assert_eq!(fold.tools.unattributed, 1);
        assert!(fold.tools.by_tool.is_empty());
    }

    #[test]
    fn an_empty_tally_has_no_error_rate_rather_than_a_zero_one() {
        assert_eq!(ToolTally::default().error_rate(), None);
        assert_eq!(
            ToolTally {
                attempts: 4,
                errors: 0
            }
            .error_rate(),
            Some(0.0)
        );
    }

    #[test]
    fn copilot_success_fields_drive_the_outcome_directly() {
        let transcript = concat!(
            r#"{"type":"tool.execution_start","timestamp":"2026-08-01T10:00:00.000Z","data":{"toolCallId":"c1","toolName":"local_shell"}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-08-01T10:00:01.000Z","data":{"toolCallId":"c1","success":false,"error":{"message":"boom"}}}"#,
            "\n",
            r#"{"type":"tool.execution_start","timestamp":"2026-08-01T10:00:02.000Z","data":{"toolCallId":"c2","toolName":"local_shell"}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-08-01T10:00:03.000Z","data":{"toolCallId":"c2","success":true}}"#,
        );
        let fold = fold_transcript(Source::Copilot, 2, transcript.as_bytes()).unwrap();
        assert_eq!(
            fold.tools.by_tool["local_shell"],
            ToolTally {
                attempts: 2,
                errors: 1
            }
        );
    }

    #[test]
    fn tools_per_request_is_undefined_without_a_user_request() {
        let one_request = metrics(fold_claude(&claude_tool_transcript("Bash", 3, 0)));
        // One user request, three invocations plus three results.
        assert_eq!(one_request.tools_per_request(), Some(6.0));

        let toolless = metrics(fold_claude(
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
        ));
        assert_eq!(toolless.tools_per_request(), None);
    }

    #[test]
    fn span_comes_from_the_first_and_last_record_timestamps() {
        let transcript = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T09:00:00.000Z","message":{"role":"user","content":"start"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T14:30:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"end"}]}}"#,
        );
        let session = metrics(fold_claude(transcript));
        assert_eq!(session.span(), Some(TimeDelta::minutes(330)));
        assert_eq!(
            session.day(),
            Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap())
        );
    }

    #[test]
    fn cadence_groups_sessions_by_their_starting_day() {
        let day = |date: &str, span_minutes: i64, gaps: &[i64]| SessionMetrics {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            summary: SessionSummary {
                first_timestamp: Some(at(date)),
                last_timestamp: Some(at(date) + TimeDelta::minutes(span_minutes)),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes::default(),
            activity: activity(gaps),
            bytes_folded: 0,
        };
        let sessions = vec![
            // 30 minutes of span, worked straight through.
            day("2026-08-01T09:00:00Z", 30, &[10, 10, 10]),
            // 90 minutes of span, but two sittings of 10 minutes inside it.
            day("2026-08-01T13:00:00Z", 90, &[10, 60, 10]),
            day("2026-08-03T09:00:00Z", 60, &[5, 5, 5]),
        ];
        let cadence = Cadence::fold(&sessions);
        assert_eq!(cadence.active_days(), 2);
        assert_eq!(cadence.sessions_per_active_day(), Some(1.5));
        assert_eq!(cadence.median_span(), Some(TimeDelta::minutes(60)));
        assert_eq!(cadence.longest_span(), Some(TimeDelta::minutes(90)));
        // The duration distribution the report leads with is the active one, and it ranks the
        // sessions differently from their spans: the 90-minute span is the *least* active of the
        // three.
        assert_eq!(cadence.median_active(), Some(TimeDelta::minutes(20)));
        assert_eq!(cadence.longest_active(), Some(TimeDelta::minutes(30)));
        assert_eq!(cadence.total_active, TimeDelta::minutes(65));
        assert_eq!(cadence.total_span, TimeDelta::minutes(180));
        assert_eq!(cadence.total_sittings, 4);
        assert_eq!(cadence.undated, 0);
    }

    /// The gap fold's arithmetic, over a session that works, walks away, comes back, and works
    /// again.
    #[test]
    fn active_time_counts_only_the_gaps_under_the_idle_threshold() {
        // 10m + 5m of work, a 3h break, then 12m + 10m of work. The break is idle time and is
        // counted nowhere; it is not shortened, capped, or averaged in.
        let session = activity(&[10, 5, 180, 12, 10]);
        assert_eq!(session.active_time(), Some(TimeDelta::minutes(37)));
        assert_eq!(session.longest_sitting(), Some(TimeDelta::minutes(22)));
        assert_eq!(session.sittings(), Some(2));
    }

    /// `≤ IDLE_GAP` keeps a sitting whole and `> IDLE_GAP` breaks it — pinned to the second,
    /// because every sitting in the archive is decided by this comparison.
    #[test]
    fn a_gap_of_exactly_the_idle_threshold_stays_inside_the_sitting() {
        let start = at("2026-08-01T09:00:00Z");
        let on_the_line = Activity::over([start, start + IDLE_GAP]);
        assert_eq!(on_the_line.sittings(), Some(1));
        assert_eq!(on_the_line.active_time(), Some(IDLE_GAP));
        assert_eq!(on_the_line.longest_sitting(), Some(IDLE_GAP));

        let over_the_line = Activity::over([start, start + IDLE_GAP + TimeDelta::seconds(1)]);
        assert_eq!(over_the_line.sittings(), Some(2));
        assert_eq!(over_the_line.active_time(), Some(TimeDelta::zero()));
        assert_eq!(over_the_line.longest_sitting(), Some(TimeDelta::zero()));
    }

    /// Out-of-order records are real (30% of archived sessions carry at least one), and the fold
    /// clamps rather than sorts: sorting would mean buffering every timestamp in a transcript
    /// that can reach hundreds of megabytes, to move two sessions across the marathon line in the
    /// whole archive. A negative gap must never subtract from active time.
    #[test]
    fn an_inverted_pair_of_records_clamps_to_a_zero_gap() {
        let start = at("2026-08-01T09:00:00Z");
        let inverted = Activity::over([
            start,
            start + TimeDelta::minutes(10),
            // The tool result the harness wrote a moment *before* the message that provoked it.
            start + TimeDelta::minutes(9),
            start + TimeDelta::minutes(12),
        ]);
        assert_eq!(inverted.active_time(), Some(TimeDelta::minutes(13)));
        assert_eq!(inverted.sittings(), Some(1));
        // And the cost of clamping rather than sorting, pinned rather than left to be discovered:
        // the fold re-traverses the minute it walked back over, so the summed gaps come out one
        // minute *longer* than the 12m between the earliest and latest record.
        assert!(inverted.active_time().unwrap() > TimeDelta::minutes(12));
    }

    /// Sub-second work is real — a burst of records and then nothing — and the span-to-active
    /// ratio has to stay finite for it, or a rule reads "no work at all" off a session that had
    /// some.
    #[test]
    fn a_sub_second_active_time_still_divides_into_a_finite_ratio() {
        let start = at("2026-08-01T09:00:00Z");
        let session = SessionMetrics {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            summary: SessionSummary {
                first_timestamp: Some(start),
                last_timestamp: Some(start + TimeDelta::hours(9)),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes::default(),
            activity: Activity::over([start, start + TimeDelta::milliseconds(900)]),
            bytes_folded: 0,
        };
        assert_eq!(
            session.active_time(),
            Some(TimeDelta::milliseconds(900)),
            "the fold itself keeps sub-second gaps"
        );
        let ratio = session.span_to_active().expect("both are measurable");
        assert!(ratio.is_finite(), "{ratio}");
        assert!((ratio - 36_000.0).abs() < 1.0, "{ratio}");
    }

    /// One record has no adjacency, so it has no gap: activity is undefined rather than zero,
    /// exactly as `span()` is.
    #[test]
    fn a_session_with_fewer_than_two_dated_records_has_no_activity() {
        let empty = Activity::default();
        assert_eq!(empty.active_time(), None);
        assert_eq!(empty.longest_sitting(), None);
        assert_eq!(empty.sittings(), None);

        let lone = Activity::over([at("2026-08-01T09:00:00Z")]);
        assert_eq!(lone.active_time(), None);
        assert_eq!(lone.sittings(), None);
    }

    /// The fold reads timestamps off the stream in file order, so a real transcript's activity
    /// comes out of `fold_transcript` rather than out of a hand-built `Activity`.
    #[test]
    fn folding_a_transcript_derives_its_activity_from_the_record_timestamps() {
        let transcript = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T09:00:00.000Z","message":{"role":"user","content":"start"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T09:05:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}"#,
            "\n",
            // Four hours away from the keyboard.
            r#"{"type":"user","uuid":"u2","timestamp":"2026-08-01T13:05:00.000Z","message":{"role":"user","content":"back"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a2","timestamp":"2026-08-01T13:07:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
        );
        let session = metrics(fold_claude(transcript));
        assert_eq!(session.span(), Some(TimeDelta::minutes(247)));
        assert_eq!(session.active_time(), Some(TimeDelta::minutes(7)));
        assert_eq!(session.longest_sitting(), Some(TimeDelta::minutes(5)));
        assert_eq!(session.sittings(), Some(2));
        assert_eq!(session.span_to_active(), Some(247.0 / 7.0));
    }

    #[test]
    fn totals_sum_per_tool_tallies_across_sessions() {
        let sessions = vec![
            metrics(fold_claude(&claude_tool_transcript("Bash", 2, 2))),
            metrics(fold_claude(&claude_tool_transcript("Bash", 2, 0))),
        ];
        let totals = Totals::fold(&sessions);
        assert_eq!(totals.sessions, 2);
        assert_eq!(
            totals.by_tool["Bash"],
            ToolTally {
                attempts: 4,
                errors: 2
            }
        );
        assert_eq!(totals.by_agent["claude-code"], 2);
    }

    #[test]
    fn agent_labels_map_onto_interpreters() {
        assert_eq!(source_for_agent("claude-code"), Some(Source::ClaudeCode));
        assert_eq!(source_for_agent("copilot-cli"), Some(Source::Copilot));
        assert_eq!(source_for_agent("codex-cli"), Some(Source::Codex));
        assert_eq!(source_for_agent("some-future-harness"), None);
    }

    #[test]
    fn an_unsupported_artifact_set_version_refuses_the_fold() {
        assert!(fold_transcript(Source::ClaudeCode, 99, &b""[..]).is_err());
    }
}
