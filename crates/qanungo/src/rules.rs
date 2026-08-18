//! The four hardcoded coaching rules.
//!
//! P0 deliberately skips the rule DSL (qanungo #3): four rules in Rust are enough to find out
//! whether folded metrics say anything worth acting on, and a DSL built before that answer is
//! known would encode guesses as syntax. Each rule reads the folded [`SessionMetrics`] and, when
//! it fires, states a **Problem**, an **Action**, and **evidence** — one line per session,
//! carrying only aggregates, tool names, and the session's `source_hash`.
//!
//! # On the thresholds
//!
//! Every constant in [`thresholds`] is **arbitrary until measured**. They are first guesses at
//! where a pattern stops being normal work and starts being a habit worth naming; none of them
//! is a decision, and none should be defended. The instrumentation footer on every run exists so
//! that a later pass can replace these numbers with observed distributions from the real
//! archive. Until then, a rule that fires constantly is evidence its threshold is wrong, not
//! evidence the habit is everywhere.

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

    /// Wall-clock span past which a session is a marathon.
    pub const MARATHON_SPAN: TimeDelta = TimeDelta::hours(4);

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
    MarathonSession,
    Babysitting,
    FireAndForget,
}

impl RuleId {
    /// The finding's heading in the report.
    pub const fn title(self) -> &'static str {
        match self {
            Self::HighToolErrorRate => "High tool error rate",
            Self::MarathonSession => "Marathon session",
            Self::Babysitting => "Babysitting pattern",
            Self::FireAndForget => "Fire-and-forget extreme",
        }
    }
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

/// Runs all four rules over the folded window, in report order. A rule that matched nothing
/// produces no finding.
pub fn evaluate(sessions: &[SessionMetrics]) -> Vec<Finding> {
    [
        high_tool_error_rate(sessions),
        marathon_session(sessions),
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
    let mut evidence = Vec::new();
    for session in sessions {
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
        if !reasons.is_empty() {
            evidence.push(Evidence {
                source_hash: session.source_hash.clone(),
                detail: reasons.join("; "),
            });
        }
    }
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

/// **Marathon session.** Fires on wall-clock span alone. A long session is not itself a mistake,
/// but a context window that has been accumulating for hours is measurably worse at the end than
/// at the start, and the fix is cheap.
fn marathon_session(sessions: &[SessionMetrics]) -> Option<Finding> {
    let evidence: Vec<_> = sessions
        .iter()
        .filter_map(|session| {
            let span = session.span()?;
            (span > thresholds::MARATHON_SPAN).then(|| Evidence {
                source_hash: session.source_hash.clone(),
                detail: format!(
                    "span {}, {} user requests, {} tool activities",
                    format::span(span),
                    session.summary.user_requests,
                    session.summary.tool_activities,
                ),
            })
        })
        .collect();
    (!evidence.is_empty()).then(|| Finding {
        rule: RuleId::MarathonSession,
        problem: format!(
            "{} of {} folded sessions ran longer than {}.",
            evidence.len(),
            sessions.len(),
            format::span(thresholds::MARATHON_SPAN),
        ),
        action: "Split the work at the next natural boundary and start the follow-on in a fresh \
                 session. Write the handoff down first — what is done, what is next, which files \
                 matter — so the new context starts from a summary rather than from hours of \
                 accumulated conversation."
            .to_owned(),
        evidence,
    })
}

/// **Babysitting pattern.** Many user requests, each producing almost no tool work: the operator
/// is driving step by step rather than delegating a task.
fn babysitting(sessions: &[SessionMetrics]) -> Option<Finding> {
    let evidence: Vec<_> = sessions
        .iter()
        .filter_map(|session| {
            let ratio = session.tools_per_request()?;
            (ratio < thresholds::BABYSITTING_TOOLS_PER_REQUEST
                && session.summary.user_requests >= thresholds::BABYSITTING_MIN_USER_REQUESTS)
                .then(|| Evidence {
                    source_hash: session.source_hash.clone(),
                    detail: format!(
                        "{} user requests, {} tool activities ({} per request)",
                        session.summary.user_requests,
                        session.summary.tool_activities,
                        format::ratio(ratio),
                    ),
                })
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
        .filter_map(|session| {
            let ratio = session.tools_per_request()?;
            (ratio >= thresholds::FIRE_AND_FORGET_TOOLS_PER_REQUEST
                && session.summary.user_requests == thresholds::FIRE_AND_FORGET_USER_REQUESTS
                && session.tools.total.errors > 0)
                .then(|| Evidence {
                    source_hash: session.source_hash.clone(),
                    detail: format!(
                        "1 user request, {} tool activities ({} per request), {} of {} calls \
                         failed",
                        session.summary.tool_activities,
                        format::ratio(ratio),
                        session.tools.total.errors,
                        session.tools.total.attempts,
                    ),
                })
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
    use crate::metrics::ToolOutcomes;

    fn timestamp(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn session(
        hash: u8,
        user_requests: usize,
        tool_activities: usize,
        span: TimeDelta,
        total: ToolTally,
        by_tool: &[(&str, ToolTally)],
    ) -> SessionMetrics {
        let first = timestamp("2026-08-01T09:00:00Z");
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
            bytes_folded: 0,
        }
    }

    fn quiet_session() -> SessionMetrics {
        session(
            0,
            4,
            12,
            TimeDelta::minutes(25),
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
            TimeDelta::minutes(10),
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
            TimeDelta::minutes(50),
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

    #[test]
    fn marathon_fires_only_past_the_span_threshold() {
        let long = session(
            3,
            6,
            120,
            thresholds::MARATHON_SPAN + TimeDelta::minutes(1),
            ToolTally::default(),
            &[],
        );
        let short = session(
            4,
            6,
            120,
            thresholds::MARATHON_SPAN - TimeDelta::minutes(1),
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
    }

    #[test]
    fn babysitting_needs_both_a_low_ratio_and_many_requests() {
        let babysat = session(
            5,
            thresholds::BABYSITTING_MIN_USER_REQUESTS,
            10,
            TimeDelta::minutes(90),
            ToolTally::default(),
            &[],
        );
        // Same low ratio, but a short conversation: not a pattern.
        let brief = session(6, 3, 2, TimeDelta::minutes(10), ToolTally::default(), &[]);
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
            TimeDelta::minutes(120),
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
            TimeDelta::minutes(120),
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

    #[test]
    fn findings_come_back_in_rule_order() {
        let everything = session(
            9,
            1,
            200,
            thresholds::MARATHON_SPAN + TimeDelta::hours(1),
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
