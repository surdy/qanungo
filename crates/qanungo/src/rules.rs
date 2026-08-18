//! The six hardcoded coaching rules.
//!
//! P0 deliberately skips the rule DSL (qanungo #3): a handful of rules in Rust are enough to find
//! out whether folded metrics say anything worth acting on, and a DSL built before that answer is
//! known would encode guesses as syntax. Each rule reads the folded [`SessionMetrics`] and, when
//! it fires, states a **Problem**, an **Action**, and **evidence** — one line per session,
//! carrying only aggregates, tool names, and the session's `source_hash`.
//!
//! Each rule's trigger lives in exactly one place, [`RuleId::verdict`], which answers three ways:
//! it fired, it did not fire, or *this session carries no signal this rule can read*. [`evaluate`]
//! filters on the first, and [`crate::scoring`] divides by the first two — so the rule that fires
//! and the fire rate that scores it cannot drift apart, which is the same argument munshi ADR
//! 0011 makes for one parser rather than two.
//!
//! The rules are **not** mutually exclusive, and no pair of them is. One session can be both a
//! marathon and heavily resumed — a month-long transcript with one all-night push inside it is
//! exactly that — and it should then appear under both headings, because the two findings ask
//! for different things. Where a fixture asserts that it trips one rule only, that is a
//! statement about the fixture, not a property of the rule set.
//!
//! # On the thresholds
//!
//! Every constant in [`thresholds`] is **arbitrary until measured**. They are first guesses at
//! where a pattern stops being normal work and starts being a habit worth naming; none of them
//! is a decision, and none should be defended. The instrumentation footer on every run exists so
//! that a later pass can replace these numbers with observed distributions from the real
//! archive. Until then, a rule that fires constantly is evidence its threshold is wrong, not
//! evidence the habit is everywhere.
//!
//! The duration constants are the first to have had that pass run over them (qanungo #14): they
//! are still arbitrary in the sense above — nothing says the archive's p95 is where coaching
//! should start — but they are now arbitrary at a *measured* point rather than at a guessed one,
//! and the measurement is written down beside them.
//!
//! [`thresholds::RETRY_LOOP_REPEATS`] was briefly a third case — set against a proxy while the
//! archive held none of the signal it reads — until munshi#77 typed the `command` field and the
//! live fold confirmed the proxy's numbers. Both rounds are written down beside it.

use crate::format;
use crate::metrics::{SessionMetrics, ToolTally};

/// Tunable rule thresholds. **Arbitrary until measured** — see the module documentation.
pub mod thresholds {
    use chrono::TimeDelta;

    /// Session-wide tool failure fraction above which a session is called out.
    pub const SESSION_TOOL_ERROR_RATE: f64 = 0.20;
    /// Tool calls a session must have reported an outcome for before its rate means anything.
    pub const MIN_SESSION_TOOL_ATTEMPTS: u64 = 10;
    /// Per-tool failure fraction above which one tool is called out by name.
    pub const TOOL_ERROR_RATE: f64 = 0.30;
    /// Calls one tool must have reported an outcome for before its rate means anything.
    pub const MIN_TOOL_ATTEMPTS: u64 = 5;

    /// Times one *exact* command value must run inside a single session before the repetition
    /// reads as a retry loop rather than as ordinary re-checking.
    ///
    /// **Measured against the archive, in two rounds.** First calibrated (2026-08-18) against a
    /// proxy — the same exact-match fold run offline over the command nested inside claude-code's
    /// `tool_use.input` and copilot's `arguments`, before the interpreter typed the field —
    /// giving busiest-run p95 = 5 and 5.1% pooled repeats over 415 command-bearing sessions.
    /// Then munshi#77 promoted the field the same day and the live fold confirmed the proxy
    /// almost exactly: 408 of 623 sessions measurable, 5% pooled repeats, and six-or-more
    /// selecting 20 of 408 (4.9%, busiest run 27) — the same order as the marathon rule's 4.4%.
    /// Still arbitrary in the doctrinal sense (nothing says the p95 is where coaching starts),
    /// but no longer estimated from a different signal than the one the rule reads.
    pub const RETRY_LOOP_REPEATS: u64 = 6;

    /// Gap between consecutive records past which the operator is taken to have walked away.
    ///
    /// **Read this together with [`MARATHON_SITTING_ACTIVE`]: they are one two-part setting, not
    /// two independent knobs.** Moving this alone rescales every sitting in the archive and
    /// changes the marathon fire rate by four to five times.
    ///
    /// Measured over the 2026-08-18 corpus (564 transcripts, 606k records): the pooled gap
    /// distribution has **no valley** — it decays monotonically from ~4s out to days (p50 0.01s,
    /// p99 2m, p99.9 1h51m), so no idle threshold falls out of the data and one has to be chosen
    /// behaviourally. The only real structure in it is a ~6× spike at *exactly* 180s across 206
    /// sessions — a harness-side 3-minute timeout, not a human pausing — plus smaller spikes at
    /// 30/60/120s. 15m is comfortably above every one of those artifacts and below any break a
    /// person would describe as still being in the session.
    pub const IDLE_GAP: TimeDelta = TimeDelta::minutes(15);

    /// Length of one continuous sitting past which a session is a marathon.
    ///
    /// The other half of the pair. At `IDLE_GAP` = 15m this is the archive's p95 of
    /// longest-sitting (1h55m) and fires on **25 of 564 sessions (4.4%)**; the equivalent cut at
    /// other idle thresholds is ≈3h at 30m and ≈5h at 60m, which is why neither constant means
    /// anything without the other. The rule it feeds tests the longest *sitting*, never the
    /// session's total active time: a 154h project with 35h of work across 53 sittings is a
    /// long-running piece of work, not a marathon.
    ///
    /// The span-based predecessor (`span > 4h`) fired on 41% of the archive — it was measuring
    /// calendar time.
    pub const MARATHON_SITTING_ACTIVE: TimeDelta = TimeDelta::hours(2);

    /// Ratio of wall-clock span to active time above which a transcript is mostly gaps.
    ///
    /// Deliberately far out in the tail: **59.6% of archived sessions are multi-sitting at
    /// `IDLE_GAP` = 15m**, so "was resumed" is the archive's normal shape and cannot be the bar.
    /// Ten times is roughly twice the archive's median dilution (4.9×).
    pub const RESUMED_SPAN_TO_ACTIVE: f64 = 10.0;

    /// Sittings a transcript must have been picked up in before its dilution reads as a habit
    /// rather than as one interrupted afternoon. Paired with [`RESUMED_SPAN_TO_ACTIVE`] so that
    /// neither a long single break nor five brisk sittings fires on its own.
    pub const RESUMED_MIN_SITTINGS: usize = 5;

    /// Tool activities per user request *below* which the agent is being led by the hand.
    pub const BABYSITTING_TOOLS_PER_REQUEST: f64 = 2.0;
    /// User requests a session must carry before a low ratio reads as babysitting rather than
    /// as a short conversation.
    pub const BABYSITTING_MIN_USER_REQUESTS: usize = 15;

    /// Tool activities per user request *above* which one ask turned into an unattended run.
    pub const FIRE_AND_FORGET_TOOLS_PER_REQUEST: f64 = 40.0;
    /// A fire-and-forget session is one the operator never came back to: exactly this many user
    /// requests.
    pub const FIRE_AND_FORGET_USER_REQUESTS: usize = 1;
}

/// Which rule produced a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuleId {
    HighToolErrorRate,
    RetryLoop,
    MarathonSession,
    ResumedSession,
    Babysitting,
    FireAndForget,
}

impl RuleId {
    /// Every rule, in the order [`evaluate`] runs them — which is also report order, and the
    /// order the rule-pack stamp hashes them in.
    pub const ALL: [Self; 6] = [
        Self::HighToolErrorRate,
        Self::RetryLoop,
        Self::MarathonSession,
        Self::ResumedSession,
        Self::Babysitting,
        Self::FireAndForget,
    ];

    /// The finding's heading in the report.
    pub const fn title(self) -> &'static str {
        match self {
            Self::HighToolErrorRate => "High tool error rate",
            Self::RetryLoop => "Retry loop",
            Self::MarathonSession => "Marathon session",
            Self::ResumedSession => "Heavily resumed session",
            Self::Babysitting => "Babysitting pattern",
            Self::FireAndForget => "Fire-and-forget extreme",
        }
    }

    /// A stable machine name, for the rule-pack stamp. Distinct from [`RuleId::title`] on
    /// purpose: a heading is prose and may be reworded, while this is an identity and may not.
    pub const fn key(self) -> &'static str {
        match self {
            Self::HighToolErrorRate => "high-tool-error-rate",
            Self::RetryLoop => "retry-loop",
            Self::MarathonSession => "marathon-session",
            Self::ResumedSession => "resumed-session",
            Self::Babysitting => "babysitting",
            Self::FireAndForget => "fire-and-forget",
        }
    }

    /// Whether this rule's trigger held for one session — or `None` when the session carries no
    /// signal this rule can read.
    ///
    /// The three-valued answer is the whole point, and it is why this lives on the rule rather
    /// than being re-derived by each caller. `Some(false)` is "this rule looked and found
    /// nothing"; `None` is "this rule could not look". A fire rate that confused the two would
    /// dilute its own numerator with sessions whose harness cannot express the signal at all —
    /// [`crate::scoring`] divides by exactly the sessions this returns `Some` for.
    ///
    /// [`evaluate`] filters on `Some(true)` from the same function, so the rule that fires and
    /// the rate that counts it can never drift apart.
    pub fn verdict(self, session: &SessionMetrics) -> Option<bool> {
        match self {
            // Eligible once enough calls reported an outcome for *either* trigger to be able to
            // fire: below the lower of the two minimums, no rate in the session means anything.
            Self::HighToolErrorRate => {
                let minimum =
                    thresholds::MIN_TOOL_ATTEMPTS.min(thresholds::MIN_SESSION_TOOL_ATTEMPTS);
                (session.tools.total.attempts >= minimum)
                    .then(|| !error_rate_reasons(session).is_empty())
            }
            Self::RetryLoop => session
                .commands
                .busiest_runs()
                .map(|runs| runs >= thresholds::RETRY_LOOP_REPEATS),
            Self::MarathonSession => session
                .longest_sitting()
                .map(|longest| longest > thresholds::MARATHON_SITTING_ACTIVE),
            Self::ResumedSession => {
                let dilution = session.span_to_active()?;
                let sittings = session.sittings()?;
                // A session whose span and active time are both zero yields NaN — no dilution was
                // measured, rather than a dilution of none.
                (!dilution.is_nan()).then_some(
                    dilution >= thresholds::RESUMED_SPAN_TO_ACTIVE
                        && sittings >= thresholds::RESUMED_MIN_SITTINGS,
                )
            }
            Self::Babysitting => {
                let ratio = session.tools_per_request()?;
                (session.summary.user_requests >= thresholds::BABYSITTING_MIN_USER_REQUESTS)
                    .then_some(ratio < thresholds::BABYSITTING_TOOLS_PER_REQUEST)
            }
            Self::FireAndForget => {
                let ratio = session.tools_per_request()?;
                (session.summary.user_requests == thresholds::FIRE_AND_FORGET_USER_REQUESTS)
                    .then_some(
                        ratio >= thresholds::FIRE_AND_FORGET_TOOLS_PER_REQUEST
                            && session.tools.total.errors > 0,
                    )
            }
        }
    }

    /// The sessions this rule could read at all, out of a window.
    pub fn eligible(self, sessions: &[SessionMetrics]) -> usize {
        sessions
            .iter()
            .filter(|session| self.verdict(session).is_some())
            .count()
    }
}

/// Whether one session tripped a rule, as a predicate over a window.
fn fired(rule: RuleId) -> impl Fn(&&SessionMetrics) -> bool {
    move |session| rule.verdict(session) == Some(true)
}

/// One evidence line: a session, named only by the content hash of its transcript, and the
/// aggregates that made it match.
///
/// The `source_hash` is the whole point — a human who wants the detail this report refuses to
/// print pulls the transcript themselves and reads it in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// Bare lowercase sha256 hex of the transcript, as Patwari serves it.
    pub source_hash: String,
    /// Aggregates and tool names only. Never transcript content.
    pub detail: String,
}

/// One rule's verdict over the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: RuleId,
    /// What the fold saw, stated over the whole window.
    pub problem: String,
    /// What to do differently.
    pub action: String,
    /// One line per matching session, newest-first in the order sessions were folded.
    pub evidence: Vec<Evidence>,
}

/// Runs every rule over the folded window, in report order. A rule that matched nothing produces
/// no finding.
pub fn evaluate(sessions: &[SessionMetrics]) -> Vec<Finding> {
    [
        high_tool_error_rate(sessions),
        retry_loop(sessions),
        marathon_session(sessions),
        resumed_session(sessions),
        babysitting(sessions),
        fire_and_forget(sessions),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// **High tool error rate.** Fires when a session's overall tool failure rate, or any single
/// tool's failure rate within it, is over threshold. Both are reported because they mean
/// different things: a session-wide rate is a bad-context problem, while one tool failing
/// repeatedly is usually a wrong-tool or wrong-invocation problem.
fn high_tool_error_rate(sessions: &[SessionMetrics]) -> Option<Finding> {
    let evidence: Vec<_> = sessions
        .iter()
        .filter(fired(RuleId::HighToolErrorRate))
        .map(|session| Evidence {
            source_hash: session.source_hash.clone(),
            detail: error_rate_reasons(session).join("; "),
        })
        .collect();
    (!evidence.is_empty()).then(|| Finding {
        rule: RuleId::HighToolErrorRate,
        problem: format!(
            "{} of {} folded sessions ran tool failure rates over threshold \
             ({} session-wide, {} for a single tool).",
            evidence.len(),
            sessions.len(),
            format::percent(thresholds::SESSION_TOOL_ERROR_RATE),
            format::percent(thresholds::TOOL_ERROR_RATE),
        ),
        action: "Failing calls are re-work: the agent spends a turn discovering what the \
                 environment already knew. Pin the failing tool's preconditions where the agent \
                 reads them — a CLAUDE.md note, a skill, or a wrapper that fails loudly — rather \
                 than correcting the same call again next session."
            .to_owned(),
        evidence,
    })
}

/// Every reason this session's tool outcomes are over threshold — session-wide first, then one
/// per tool named. Empty when nothing is: the rule's trigger and its evidence are the same list.
fn error_rate_reasons(session: &SessionMetrics) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(detail) = over_rate(
        &session.tools.total,
        thresholds::MIN_SESSION_TOOL_ATTEMPTS,
        thresholds::SESSION_TOOL_ERROR_RATE,
    ) {
        reasons.push(format!("session-wide {detail}"));
    }
    for (name, tally) in &session.tools.by_tool {
        if let Some(detail) = over_rate(
            tally,
            thresholds::MIN_TOOL_ATTEMPTS,
            thresholds::TOOL_ERROR_RATE,
        ) {
            reasons.push(format!("{name} {detail}"));
        }
    }
    reasons
}

/// Formats a tally that is over threshold, or `None` when it is not — including when too few
/// calls reported an outcome for the rate to mean anything.
fn over_rate(tally: &ToolTally, min_attempts: u64, threshold: f64) -> Option<String> {
    let rate = tally.error_rate()?;
    (tally.attempts >= min_attempts && rate > threshold).then(|| {
        format!(
            "{} of {} calls failed ({})",
            tally.errors,
            tally.attempts,
            format::percent(rate)
        )
    })
}

/// **Retry loop.** Fires when one exact command value ran [`thresholds::RETRY_LOOP_REPEATS`]
/// times or more inside a single session.
///
/// One trigger, deliberately: the busiest command's run count, and nothing else. A share-based
/// trigger ("repeats are over x% of command activity") was the alternative and it measures a
/// different thing — a session that runs two commands twice each is 50% repeats and is not a
/// retry loop — so mixing the two would produce a rule whose firing nobody could explain. The
/// share rides along in the evidence line as context.
///
/// A session whose harness records no command field is skipped, not scored: no signal, no claim.
/// See [`CommandChurn`](crate::metrics::CommandChurn).
fn retry_loop(sessions: &[SessionMetrics]) -> Option<Finding> {
    let evidence: Vec<_> = sessions
        .iter()
        .filter(fired(RuleId::RetryLoop))
        .map(|session| {
            let churn = &session.commands;
            Evidence {
                source_hash: session.source_hash.clone(),
                detail: format!(
                    "one command run {} times; {} of {} command-bearing calls were repeats \
                     ({}), across {} repeated commands",
                    churn.busiest_command_runs,
                    churn.repeats,
                    churn.command_events,
                    churn
                        .repeat_share()
                        .map_or_else(|| "—".to_owned(), format::percent),
                    churn.repeated_commands,
                ),
            }
        })
        .collect();
    (!evidence.is_empty()).then(|| Finding {
        rule: RuleId::RetryLoop,
        problem: format!(
            "{} of {} folded sessions ran one identical command {}+ times.",
            evidence.len(),
            sessions.len(),
            thresholds::RETRY_LOOP_REPEATS,
        ),
        action: "Repetition is the cheapest signal that a loop is not closing: the same command, \
                 the same disagreement, another turn. Fix what the command keeps arguing with — \
                 the stale config, the missing dependency, the wrong working directory — and say \
                 so where the agent reads it, rather than letting it rediscover the answer by \
                 running the command again. Where the repeat is legitimate re-checking after each \
                 edit, it is a watch mode or a single script waiting to be written."
            .to_owned(),
        evidence,
    })
}

/// **Marathon session.** Fires on the longest continuous *sitting*, not on wall-clock span and
/// not on total active time. A long session is not itself a mistake, but a context window that
/// has been accumulating without a break is measurably worse at the end than at the start, and
/// the fix is cheap.
///
/// Span was the P0 reading of this and it was wrong: 41% of the archive cleared `span > 4h`,
/// almost all of it transcripts resumed over days. Total active time would be wrong in the other
/// direction — it would call a month-long project with fifty short sittings a marathon. What
/// coaching is about here is one unbroken push, so that is what the rule measures. The span and
/// the sitting count ride along as evidence, because "2h04m inside a 330h transcript" is the
/// sentence that makes the finding legible.
fn marathon_session(sessions: &[SessionMetrics]) -> Option<Finding> {
    let evidence: Vec<_> = sessions
        .iter()
        .filter(fired(RuleId::MarathonSession))
        .map(|session| Evidence {
            source_hash: session.source_hash.clone(),
            detail: format!(
                "longest sitting {} within a {} span across {} sittings, {} user requests, \
                 {} tool activities",
                session
                    .longest_sitting()
                    .map_or_else(|| "—".to_owned(), format::span),
                session.span().map_or_else(|| "—".to_owned(), format::span),
                session.sittings().unwrap_or_default(),
                session.summary.user_requests,
                session.summary.tool_activities,
            ),
        })
        .collect();
    (!evidence.is_empty()).then(|| Finding {
        rule: RuleId::MarathonSession,
        problem: format!(
            "{} of {} folded sessions worked for more than {} without a break longer than {}.",
            evidence.len(),
            sessions.len(),
            format::span(thresholds::MARATHON_SITTING_ACTIVE),
            format::span(thresholds::IDLE_GAP),
        ),
        action: "Split the work at the next natural boundary and start the follow-on in a fresh \
                 session. Write the handoff down first — what is done, what is next, which files \
                 matter — so the new context starts from a summary rather than from hours of \
                 accumulated conversation."
            .to_owned(),
        evidence,
    })
}

/// **Heavily resumed session.** One transcript, picked up again and again over days, with very
/// little of its calendar footprint spent working.
///
/// This is the archive's dominant shape (59.6% of sessions are multi-sitting), which is exactly
/// why it gets its own rule instead of being folded into Marathon: the two say opposite things
/// about the same span, and a rule that fired on both would be reporting "this session exists".
/// The threshold pair therefore sits well out in the tail — ten times more calendar than work,
/// across at least five sittings — so that ordinary "picked it up after lunch" never appears.
///
/// The coaching point is not that resuming is bad. It is that a transcript resumed a dozen times
/// carries a dozen work items' worth of accumulated context into each of them, and that
/// everything session-scoped — this report's own metrics included — gets less meaningful the
/// longer one transcript stands in for many separate pieces of work.
fn resumed_session(sessions: &[SessionMetrics]) -> Option<Finding> {
    let evidence: Vec<_> = sessions
        .iter()
        .filter(fired(RuleId::ResumedSession))
        .map(|session| Evidence {
            source_hash: session.source_hash.clone(),
            detail: format!(
                "active {} across {} sittings, span {} ({})",
                session
                    .active_time()
                    .map_or_else(|| "—".to_owned(), format::span),
                session.sittings().unwrap_or_default(),
                session.span().map_or_else(|| "—".to_owned(), format::span),
                session
                    .span_to_active()
                    .map_or_else(|| "—".to_owned(), dilution_multiple),
            ),
        })
        .collect();
    (!evidence.is_empty()).then(|| Finding {
        rule: RuleId::ResumedSession,
        problem: format!(
            "{} of {} folded sessions were worked in {}+ separate sittings, with a calendar \
             footprint at least {}x their active time.",
            evidence.len(),
            sessions.len(),
            thresholds::RESUMED_MIN_SITTINGS,
            format::ratio(thresholds::RESUMED_SPAN_TO_ACTIVE),
        ),
        action: "Start a fresh session per work item rather than returning to a standing one. \
                 The archive keeps the old transcript, so nothing is lost by leaving it closed — \
                 and a session that maps onto one piece of work is the unit every summary, \
                 metric, and coaching finding here is actually about."
            .to_owned(),
        evidence,
    })
}

/// How much larger a session's span is than the work in it, rendered — or the plain statement
/// that a session with no time inside any sitting has no finite multiple to render.
fn dilution_multiple(dilution: f64) -> String {
    if dilution.is_finite() {
        format!("{}x", format::ratio(dilution))
    } else {
        "no gap short enough to count as work".to_owned()
    }
}

/// **Babysitting pattern.** Many user requests, each producing almost no tool work: the operator
/// is driving step by step rather than delegating a task.
fn babysitting(sessions: &[SessionMetrics]) -> Option<Finding> {
    let evidence: Vec<_> = sessions
        .iter()
        .filter(fired(RuleId::Babysitting))
        .map(|session| Evidence {
            source_hash: session.source_hash.clone(),
            detail: format!(
                "{} user requests, {} tool activities ({} per request)",
                session.summary.user_requests,
                session.summary.tool_activities,
                session
                    .tools_per_request()
                    .map_or_else(|| "—".to_owned(), format::ratio),
            ),
        })
        .collect();
    (!evidence.is_empty()).then(|| Finding {
        rule: RuleId::Babysitting,
        problem: format!(
            "{} of {} folded sessions carried {}+ user requests at under {} tool activities each \
             — turn-by-turn steering rather than delegation.",
            evidence.len(),
            sessions.len(),
            thresholds::BABYSITTING_MIN_USER_REQUESTS,
            format::ratio(thresholds::BABYSITTING_TOOLS_PER_REQUEST),
        ),
        action: "Ask bigger. State the outcome and the constraints once and let the agent plan \
                 the steps. Where the same sequence of small asks keeps recurring, it is a skill \
                 waiting to be written — capture it once instead of retyping it every session."
            .to_owned(),
        evidence,
    })
}

/// **Fire-and-forget extreme.** One ask, an enormous amount of unattended tool work, and errors
/// along the way — nobody was watching when it went wrong.
fn fire_and_forget(sessions: &[SessionMetrics]) -> Option<Finding> {
    let evidence: Vec<_> = sessions
        .iter()
        .filter(fired(RuleId::FireAndForget))
        .map(|session| Evidence {
            source_hash: session.source_hash.clone(),
            detail: format!(
                "1 user request, {} tool activities ({} per request), {} of {} calls failed",
                session.summary.tool_activities,
                session
                    .tools_per_request()
                    .map_or_else(|| "—".to_owned(), format::ratio),
                session.tools.total.errors,
                session.tools.total.attempts,
            ),
        })
        .collect();
    (!evidence.is_empty()).then(|| Finding {
        rule: RuleId::FireAndForget,
        problem: format!(
            "{} of {} folded sessions ran {}+ tool activities on a single request and hit errors \
             on the way.",
            evidence.len(),
            sessions.len(),
            format::ratio(thresholds::FIRE_AND_FORGET_TOOLS_PER_REQUEST),
        ),
        action: "Put a checkpoint in the middle. Ask for a plan before the work, or for a report \
                 at a named milestone, so a wrong turn surfaces while it is still one wrong turn \
                 rather than at the end of an hour of unattended tool calls."
            .to_owned(),
        evidence,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, TimeDelta, Utc};
    use munshi_transcript::SessionSummary;

    use super::*;
    use crate::metrics::{Activity, CommandChurn, ToolOutcomes};

    fn timestamp(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A session whose records are `gaps_minutes` apart, in file order — which is what decides
    /// its span, its active time, and its sittings all at once, exactly as a real fold does.
    fn worked(gaps_minutes: &[i64]) -> (TimeDelta, Activity) {
        let mut at = timestamp("2026-08-01T09:00:00Z");
        let mut timestamps = vec![at];
        for gap in gaps_minutes {
            at += TimeDelta::minutes(*gap);
            timestamps.push(at);
        }
        (
            at - timestamps[0],
            Activity::over(timestamps.iter().copied()),
        )
    }

    fn session(
        hash: u8,
        user_requests: usize,
        tool_activities: usize,
        gaps_minutes: &[i64],
        total: ToolTally,
        by_tool: &[(&str, ToolTally)],
    ) -> SessionMetrics {
        let first = timestamp("2026-08-01T09:00:00Z");
        let (span, activity) = worked(gaps_minutes);
        SessionMetrics {
            source_hash: format!("{hash:02x}").repeat(32),
            source_agent: "claude-code".to_owned(),
            summary: SessionSummary {
                user_requests,
                tool_activities,
                first_timestamp: Some(first),
                last_timestamp: Some(first + span),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes {
                total,
                by_tool: by_tool
                    .iter()
                    .map(|(name, tally)| ((*name).to_owned(), *tally))
                    .collect::<BTreeMap<_, _>>(),
                unattributed: 0,
            },
            activity,
            commands: CommandChurn::default(),
            bytes_folded: 0,
        }
    }

    /// Minutes of continuous work, as a run of gaps the fold will keep inside one sitting.
    fn continuous(minutes: i64) -> Vec<i64> {
        let step = thresholds::IDLE_GAP.num_minutes();
        let mut gaps = vec![step; usize::try_from(minutes / step).unwrap_or_default()];
        if minutes % step != 0 {
            gaps.push(minutes % step);
        }
        gaps
    }

    fn quiet_session() -> SessionMetrics {
        session(
            0,
            4,
            12,
            &continuous(25),
            ToolTally {
                attempts: 6,
                errors: 0,
            },
            &[(
                "Read",
                ToolTally {
                    attempts: 6,
                    errors: 0,
                },
            )],
        )
    }

    #[test]
    fn a_healthy_session_fires_nothing() {
        assert!(evaluate(&[quiet_session()]).is_empty());
    }

    #[test]
    fn a_below_threshold_sample_never_fires_the_error_rule() {
        // 100% failure, but only three calls: too few to mean anything.
        let sparse = session(
            1,
            3,
            6,
            &continuous(10),
            ToolTally {
                attempts: 3,
                errors: 3,
            },
            &[(
                "Bash",
                ToolTally {
                    attempts: 3,
                    errors: 3,
                },
            )],
        );
        assert!(evaluate(&[sparse]).is_empty());
    }

    #[test]
    fn a_single_tool_over_threshold_fires_and_is_named() {
        let noisy = session(
            2,
            8,
            40,
            &continuous(50),
            ToolTally {
                attempts: 20,
                errors: 3,
            },
            &[
                (
                    "Bash",
                    ToolTally {
                        attempts: 8,
                        errors: 5,
                    },
                ),
                (
                    "Read",
                    ToolTally {
                        attempts: 12,
                        errors: 0,
                    },
                ),
            ],
        );
        let findings = evaluate(&[noisy]);
        let finding = findings
            .iter()
            .find(|finding| finding.rule == RuleId::HighToolErrorRate)
            .expect("the error-rate rule fires");
        assert_eq!(finding.evidence.len(), 1);
        assert!(finding.evidence[0].detail.contains("Bash"));
        // The session-wide rate (15%) is under threshold, so only the tool is named.
        assert!(!finding.evidence[0].detail.contains("session-wide"));
    }

    /// The same quiet session, given a churn reading — the rule reads nothing else about it.
    fn with_churn(hash: u8, churn: CommandChurn) -> SessionMetrics {
        let mut session = quiet_session();
        session.source_hash = format!("{hash:02x}").repeat(32);
        session.commands = churn;
        session
    }

    #[test]
    fn retry_loop_fires_only_past_the_repeat_threshold() {
        let looping = with_churn(
            20,
            CommandChurn {
                command_events: 20,
                repeats: 8,
                distinct_commands: 12,
                repeated_commands: 2,
                busiest_command_runs: thresholds::RETRY_LOOP_REPEATS,
                untracked_events: 0,
            },
        );
        // One run short of the threshold, and busier overall: the rule tests one command's runs,
        // not the session's total repetition.
        let persistent = with_churn(
            21,
            CommandChurn {
                command_events: 60,
                repeats: 30,
                distinct_commands: 30,
                repeated_commands: 10,
                busiest_command_runs: thresholds::RETRY_LOOP_REPEATS - 1,
                untracked_events: 0,
            },
        );
        let findings = evaluate(&[looping, persistent]);
        let finding = findings
            .iter()
            .find(|finding| finding.rule == RuleId::RetryLoop)
            .expect("the retry-loop rule fires");
        assert_eq!(finding.evidence.len(), 1);
        assert_eq!(finding.evidence[0].source_hash, "14".repeat(32));
        assert_eq!(
            finding.evidence[0].detail,
            format!(
                "one command run {} times; 8 of 20 command-bearing calls were repeats (40%), \
                 across 2 repeated commands",
                thresholds::RETRY_LOOP_REPEATS,
            )
        );
    }

    /// No signal, no claim: a churn record with no command-bearing event is undefined, and the
    /// rule must skip it rather than reading whatever the count fields happen to hold.
    #[test]
    fn a_session_that_recorded_no_command_never_fires_the_retry_rule() {
        let blind = with_churn(
            22,
            CommandChurn {
                command_events: 0,
                busiest_command_runs: thresholds::RETRY_LOOP_REPEATS * 10,
                ..CommandChurn::default()
            },
        );
        assert!(evaluate(&[blind]).is_empty());
    }

    #[test]
    fn marathon_fires_only_past_the_sitting_threshold() {
        let marathon = thresholds::MARATHON_SITTING_ACTIVE.num_minutes();
        let long = session(
            3,
            6,
            120,
            &continuous(marathon + 1),
            ToolTally::default(),
            &[],
        );
        let short = session(
            4,
            6,
            120,
            &continuous(marathon - 1),
            ToolTally::default(),
            &[],
        );
        let findings = evaluate(&[long, short]);
        let finding = findings
            .iter()
            .find(|finding| finding.rule == RuleId::MarathonSession)
            .expect("the marathon rule fires");
        assert_eq!(finding.evidence.len(), 1);
        assert_eq!(finding.evidence[0].source_hash, "03".repeat(32));
        assert!(
            finding.evidence[0].detail.starts_with("longest sitting 2h"),
            "{}",
            finding.evidence[0].detail
        );
    }

    /// The regression the whole change exists for (qanungo #14): a transcript resumed over days
    /// used to be the report's loudest "marathon", and its longest continuous push was under an
    /// hour. It must now fire the resumed rule and only the resumed rule.
    #[test]
    fn a_resumed_session_is_not_a_marathon_however_long_its_span() {
        // Six sittings of half an hour each, a day apart: 3h of work inside a 5-day span.
        let mut gaps = Vec::new();
        for sitting in 0..6 {
            if sitting > 0 {
                gaps.push(24 * 60 - 30);
            }
            gaps.extend(continuous(30));
        }
        let resumed = session(11, 30, 400, &gaps, ToolTally::default(), &[]);
        assert_eq!(resumed.sittings(), Some(6));
        assert_eq!(resumed.active_time(), Some(TimeDelta::hours(3)));

        let rules: Vec<_> = evaluate(&[resumed])
            .into_iter()
            .map(|finding| finding.rule)
            .collect();
        assert_eq!(rules, vec![RuleId::ResumedSession]);
    }

    /// The two duration rules are not alternatives, and a session that is genuinely both must
    /// appear under both headings: a long-lived transcript with one all-night push inside it has
    /// a marathon sitting *and* a diluted, heavily resumed shape, and the coaching for the two is
    /// different — break the push up, and stop reusing the transcript.
    #[test]
    fn a_marathon_inside_a_heavily_resumed_transcript_fires_both_rules() {
        let mut gaps = continuous(thresholds::MARATHON_SITTING_ACTIVE.num_minutes() + 30);
        for _ in 0..5 {
            gaps.push(3 * 24 * 60);
            gaps.extend(continuous(10));
        }
        let both = session(14, 20, 300, &gaps, ToolTally::default(), &[]);
        assert_eq!(both.sittings(), Some(6));
        assert!(both.longest_sitting().unwrap() > thresholds::MARATHON_SITTING_ACTIVE);
        assert!(both.span_to_active().unwrap() >= thresholds::RESUMED_SPAN_TO_ACTIVE);

        let rules: Vec<_> = evaluate(&[both])
            .into_iter()
            .map(|finding| finding.rule)
            .collect();
        assert_eq!(
            rules,
            vec![RuleId::MarathonSession, RuleId::ResumedSession],
            "the rule set is not a partition of sessions"
        );
    }

    /// Both halves of the resumed rule have to hold: the archive is full of sessions that were
    /// merely picked up again, and a rule that fired on those would be reporting the weather.
    #[test]
    fn the_resumed_rule_needs_both_dilution_and_repetition() {
        // Diluted 24x, but only two sittings: one long interruption, not a habit.
        let mut interrupted = continuous(30);
        interrupted.push(23 * 60);
        interrupted.extend(continuous(30));
        let interrupted = session(12, 8, 40, &interrupted, ToolTally::default(), &[]);
        assert_eq!(interrupted.sittings(), Some(2));

        // Six sittings, but back-to-back-ish: barely diluted at all.
        let mut brisk = Vec::new();
        for sitting in 0..6 {
            if sitting > 0 {
                brisk.push(20);
            }
            brisk.extend(continuous(60));
        }
        let brisk = session(13, 8, 40, &brisk, ToolTally::default(), &[]);
        assert_eq!(brisk.sittings(), Some(6));
        assert!(brisk.span_to_active().unwrap() < thresholds::RESUMED_SPAN_TO_ACTIVE);

        let findings = evaluate(&[interrupted, brisk]);
        assert!(
            !findings
                .iter()
                .any(|finding| finding.rule == RuleId::ResumedSession),
            "{findings:?}"
        );
    }

    #[test]
    fn babysitting_needs_both_a_low_ratio_and_many_requests() {
        let babysat = session(
            5,
            thresholds::BABYSITTING_MIN_USER_REQUESTS,
            10,
            &continuous(90),
            ToolTally::default(),
            &[],
        );
        // Same low ratio, but a short conversation: not a pattern.
        let brief = session(6, 3, 2, &continuous(10), ToolTally::default(), &[]);
        let findings = evaluate(&[babysat, brief]);
        let finding = findings
            .iter()
            .find(|finding| finding.rule == RuleId::Babysitting)
            .expect("the babysitting rule fires");
        assert_eq!(finding.evidence.len(), 1);
        assert_eq!(finding.evidence[0].source_hash, "05".repeat(32));
    }

    #[test]
    fn fire_and_forget_needs_a_single_request_a_high_ratio_and_errors() {
        let unattended = session(
            7,
            1,
            200,
            &continuous(119),
            ToolTally {
                attempts: 100,
                errors: 4,
            },
            &[(
                "Bash",
                ToolTally {
                    attempts: 100,
                    errors: 4,
                },
            )],
        );
        // Identical shape, but nothing failed: a long clean run is not a finding.
        let clean = session(
            8,
            1,
            200,
            &continuous(119),
            ToolTally {
                attempts: 100,
                errors: 0,
            },
            &[(
                "Bash",
                ToolTally {
                    attempts: 100,
                    errors: 0,
                },
            )],
        );
        let findings = evaluate(&[unattended, clean]);
        let finding = findings
            .iter()
            .find(|finding| finding.rule == RuleId::FireAndForget)
            .expect("the fire-and-forget rule fires");
        assert_eq!(finding.evidence.len(), 1);
        assert_eq!(finding.evidence[0].source_hash, "07".repeat(32));
    }

    /// The distinction every fire rate divides by: a rule that looked and found nothing is not a
    /// rule that could not look. A quiet session answers `Some(false)` to the rules whose signal
    /// it carries and `None` to the ones it does not.
    #[test]
    fn a_verdict_separates_not_firing_from_having_no_signal() {
        let quiet = quiet_session();
        assert_eq!(RuleId::MarathonSession.verdict(&quiet), Some(false));
        assert_eq!(RuleId::ResumedSession.verdict(&quiet), Some(false));
        assert_eq!(RuleId::FireAndForget.verdict(&quiet), None, "4 requests");
        assert_eq!(
            RuleId::Babysitting.verdict(&quiet),
            None,
            "too few requests"
        );
        assert_eq!(
            RuleId::RetryLoop.verdict(&quiet),
            None,
            "the session recorded no command, so the rule cannot look"
        );

        // The same session with a churn reading: now the rule can look, and finds nothing.
        let measured = with_churn(
            30,
            CommandChurn {
                command_events: 4,
                distinct_commands: 4,
                busiest_command_runs: 1,
                ..CommandChurn::default()
            },
        );
        assert_eq!(RuleId::RetryLoop.verdict(&measured), Some(false));
        assert_eq!(RuleId::RetryLoop.eligible(&[quiet, measured]), 1);
    }

    /// A session with too few outcome-bearing calls for either error-rate trigger to fire is one
    /// the rule *could not look at*, not one it cleared.
    #[test]
    fn the_error_rate_rule_cannot_look_below_its_own_minimum() {
        let sparse = session(
            40,
            3,
            6,
            &continuous(10),
            ToolTally {
                attempts: thresholds::MIN_TOOL_ATTEMPTS - 1,
                errors: thresholds::MIN_TOOL_ATTEMPTS - 1,
            },
            &[],
        );
        assert_eq!(RuleId::HighToolErrorRate.verdict(&sparse), None);

        let enough = session(
            41,
            3,
            6,
            &continuous(10),
            ToolTally {
                attempts: thresholds::MIN_SESSION_TOOL_ATTEMPTS,
                errors: 0,
            },
            &[],
        );
        assert_eq!(RuleId::HighToolErrorRate.verdict(&enough), Some(false));
    }

    #[test]
    fn findings_come_back_in_rule_order() {
        let everything = session(
            9,
            1,
            200,
            &continuous(thresholds::MARATHON_SITTING_ACTIVE.num_minutes() + 60),
            ToolTally {
                attempts: 100,
                errors: 60,
            },
            &[(
                "Bash",
                ToolTally {
                    attempts: 100,
                    errors: 60,
                },
            )],
        );
        let rules: Vec<_> = evaluate(&[everything])
            .into_iter()
            .map(|finding| finding.rule)
            .collect();
        assert_eq!(
            rules,
            vec![
                RuleId::HighToolErrorRate,
                RuleId::MarathonSession,
                RuleId::FireAndForget,
            ]
        );
    }
}
