//! The fold: exactly three metrics over `munshi-transcript`'s typed events.
//!
//! Everything here is derivable from what the interpreter types *today* — no new transcript
//! parsing, no schema changes upstream:
//!
//! 1. **Tool error rate** — per session and per tool name, from the outcome fields a
//!    [`ToolEvent`] carries (`success`, `is_error`).
//! 2. **Tool-per-request ratio** — [`SessionSummary::tool_activities`] over
//!    [`SessionSummary::user_requests`]: how much the agent does per thing it is asked for.
//! 3. **Cadence and duration** — sessions per day, and each session's wall-clock span from the
//!    first and last record timestamps.
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
    /// Transcript bytes read, for the fold-cost footer.
    pub bytes_folded: u64,
}

impl SessionMetrics {
    /// Wall-clock span between the first and last dated record.
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
#[derive(Debug, Clone, Default)]
pub struct Cadence {
    /// Sessions per UTC calendar day, by the day each session's first record landed on.
    pub per_day: BTreeMap<NaiveDate, usize>,
    /// Sessions whose records carried no parseable timestamp at all.
    pub undated: usize,
    /// Every dated session's wall-clock span, ascending.
    pub spans: Vec<TimeDelta>,
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
            }
        }
        cadence.spans.sort_unstable();
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
        self.spans
            .get(self.spans.len().saturating_sub(1) / 2)
            .copied()
    }

    /// The longest session span in the window.
    pub fn longest_span(&self) -> Option<TimeDelta> {
        self.spans.last().copied()
    }
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
            bytes_folded: 0,
        }
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
        let day = |date: &str, span_minutes: i64| SessionMetrics {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            summary: SessionSummary {
                first_timestamp: Some(
                    DateTime::parse_from_rfc3339(date)
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                last_timestamp: Some(
                    DateTime::parse_from_rfc3339(date)
                        .unwrap()
                        .with_timezone(&Utc)
                        + TimeDelta::minutes(span_minutes),
                ),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes::default(),
            bytes_folded: 0,
        };
        let sessions = vec![
            day("2026-08-01T09:00:00Z", 30),
            day("2026-08-01T13:00:00Z", 90),
            day("2026-08-03T09:00:00Z", 60),
        ];
        let cadence = Cadence::fold(&sessions);
        assert_eq!(cadence.active_days(), 2);
        assert_eq!(cadence.sessions_per_active_day(), Some(1.5));
        assert_eq!(cadence.median_span(), Some(TimeDelta::minutes(60)));
        assert_eq!(cadence.longest_span(), Some(TimeDelta::minutes(90)));
        assert_eq!(cadence.undated, 0);
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
