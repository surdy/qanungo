//! Markdown rendering, and the redaction line that rendering enforces.
//!
//! # The redaction line (hard)
//!
//! A coaching report renders **aggregates, tool names, and `source_hash` references — nothing
//! else**. No command strings, no error text, no message excerpts, no file paths, no user or
//! assistant prose. Not truncated, not elided, not "just the first line": zero verbatim
//! transcript field content.
//!
//! This is a property of *construction*, not of filtering. Nothing in this module ever receives
//! a transcript string: [`crate::metrics`] folds records into counts and timestamps and drops
//! the content, and [`crate::rules`] renders findings from those counts plus tool names. A tool
//! name (`Bash`, `local_shell`) is schema metadata — the harness's own vocabulary, not anything
//! the operator or the model wrote — and is the single verbatim string a report may carry.
//!
//! Evidence is therefore a content hash. Somebody who wants the detail this report refuses to
//! print pulls the transcript from the archive and reads it in full, with the archive's own
//! access story intact. The first qanungo surface that quotes verbatim lands behind its own
//! issue, not by loosening this one.
//!
//! # The instrumentation footer
//!
//! Every run ends with fold wall-time, session count, bytes folded, and cache hits/misses. It is
//! not decoration: it is the longitudinal fold-cost measurement the future event-store go/no-go
//! is made against. A report without it would make that decision unmeasurable.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::cli::Window;
use crate::format;
use crate::metrics::{Cadence, SessionMetrics, Totals};
use crate::rules::Finding;
use crate::sync::SyncStats;

/// How many tools the tool-use table names. The long tail of once-used tools says nothing.
const TOOL_TABLE_ROWS: usize = 6;

/// One line of the report's "Gaps" section: how many sessions were skipped, and why. Skips are
/// grouped by reason so a systematic gap reads as one fact rather than as forty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedNote {
    pub count: usize,
    pub reason: String,
}

/// What a run cost, folded into the footer.
#[derive(Debug, Clone)]
pub struct Instrumentation {
    pub sync: SyncStats,
    /// Wall-time of the fold alone — the number the event-store decision turns on.
    pub fold_elapsed: Duration,
    pub sessions_folded: usize,
    pub bytes_folded: u64,
    pub patwari_url: String,
    pub cache_root: PathBuf,
}

/// Everything one report is rendered from.
pub struct Report<'a> {
    pub window: &'a Window,
    pub generated_at: DateTime<Utc>,
    pub sessions: &'a [SessionMetrics],
    pub findings: &'a [Finding],
    pub skipped: &'a [SkippedNote],
    pub instrumentation: &'a Instrumentation,
}

impl Report<'_> {
    /// Renders the report as Markdown.
    pub fn render(&self) -> String {
        let totals = Totals::fold(self.sessions);
        let cadence = Cadence::fold(self.sessions);
        let mut out = String::new();

        let _ = writeln!(out, "# Coaching report — last {}\n", self.window);
        self.render_window(&mut out, &totals);

        if self.sessions.is_empty() {
            out.push_str(
                "\nNo archived sessions fell in this window, so there is nothing to coach on \
                 yet.\n",
            );
        } else {
            self.render_cadence(&mut out, &cadence);
            self.render_tool_use(&mut out, &totals);
            self.render_findings(&mut out);
        }
        self.render_gaps(&mut out);
        self.render_footer(&mut out);
        out
    }

    fn render_window(&self, out: &mut String, totals: &Totals) {
        let since = self.generated_at - self.window.delta();
        let _ = writeln!(
            out,
            "Sessions archived since {} (UTC), folded at {}.",
            stamp(since),
            stamp(self.generated_at),
        );
        if self.sessions.is_empty() {
            return;
        }
        let agents = totals
            .by_agent
            .iter()
            .map(|(agent, count)| format!("{agent} ({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "\n**{} sessions** — {agents} — {} user requests, {} tool activities.",
            totals.sessions, totals.user_requests, totals.tool_activities,
        );
    }

    fn render_cadence(&self, out: &mut String, cadence: &Cadence) {
        out.push_str("\n## Cadence\n\n");
        let per_active_day = cadence
            .sessions_per_active_day()
            .map_or_else(|| "—".to_owned(), format::ratio);
        let _ = writeln!(
            out,
            "- {} sessions across {} active days ({per_active_day} per active day).",
            self.sessions.len(),
            cadence.active_days(),
        );
        if let (Some(median), Some(longest)) = (cadence.median_span(), cadence.longest_span()) {
            let _ = writeln!(
                out,
                "- Median session span {}; longest {}.",
                format::span(median),
                format::span(longest),
            );
        }
        if let Some((busiest, count)) = cadence.per_day.iter().max_by_key(|(_, count)| **count) {
            let _ = writeln!(out, "- Busiest day {busiest}: {count} sessions.");
        }
        if cadence.undated > 0 {
            let _ = writeln!(
                out,
                "- {} sessions carried no parseable timestamps and are absent from the spans \
                 above.",
                cadence.undated,
            );
        }
    }

    fn render_tool_use(&self, out: &mut String, totals: &Totals) {
        out.push_str("\n## Tool use\n\n");
        match totals.tools.error_rate() {
            Some(rate) => {
                let _ = writeln!(
                    out,
                    "- {} of {} tool calls that reported an outcome failed ({}).",
                    totals.tools.errors,
                    totals.tools.attempts,
                    format::percent(rate),
                );
            }
            None => out.push_str(
                "- No tool call in this window reported an outcome, so no error rate is \
                 defined.\n",
            ),
        }
        if let Some(ratio) = totals.tools_per_request() {
            let _ = writeln!(
                out,
                "- {} tool activities per user request across the window.",
                format::ratio(ratio),
            );
        }
        if totals.malformed_records > 0 {
            let _ = writeln!(
                out,
                "- {} transcript records could not be parsed and were counted, not folded.",
                totals.malformed_records,
            );
        }

        let mut tools: Vec<_> = totals.by_tool.iter().collect();
        tools.sort_by(|(left_name, left), (right_name, right)| {
            right
                .attempts
                .cmp(&left.attempts)
                .then_with(|| left_name.cmp(right_name))
        });
        if tools.is_empty() {
            return;
        }
        out.push_str("\n| Tool | Calls with an outcome | Failed | Rate |\n");
        out.push_str("| --- | ---: | ---: | ---: |\n");
        for (name, tally) in tools.into_iter().take(TOOL_TABLE_ROWS) {
            let rate = tally
                .error_rate()
                .map_or_else(|| "—".to_owned(), format::percent);
            let _ = writeln!(
                out,
                "| {name} | {} | {} | {rate} |",
                tally.attempts, tally.errors,
            );
        }
    }

    fn render_findings(&self, out: &mut String) {
        out.push_str("\n## Findings\n");
        if self.findings.is_empty() {
            out.push_str(
                "\nNothing crossed a rule threshold this window. The thresholds are first \
                 guesses, so a quiet report is as much a statement about them as about the \
                 work.\n",
            );
            return;
        }
        for finding in self.findings {
            let _ = writeln!(out, "\n### {}\n", finding.rule.title());
            let _ = writeln!(out, "**Problem** — {}\n", finding.problem);
            let _ = writeln!(out, "**Action** — {}\n", finding.action);
            out.push_str("**Evidence**\n\n");
            for evidence in &finding.evidence {
                let _ = writeln!(
                    out,
                    "- `sha256:{}` — {}",
                    evidence.source_hash, evidence.detail
                );
            }
        }
    }

    fn render_gaps(&self, out: &mut String) {
        if self.skipped.is_empty() {
            return;
        }
        out.push_str("\n## Gaps\n\n");
        out.push_str("These archived sessions contributed nothing to the fold:\n\n");
        for note in self.skipped {
            let _ = writeln!(out, "- {} — {}", note.count, note.reason);
        }
    }

    fn render_footer(&self, out: &mut String) {
        let instrumentation = self.instrumentation;
        out.push_str("\n---\n\n");
        out.push_str(
            "Evidence is cited by transcript content hash only — this report renders aggregates, \
             tool names, and `source_hash` references, never transcript content. To read one in \
             full, ask the archive for its artifact and fetch the `content_url` that comes back \
             (the filter takes the bare digest, without the `sha256:` prefix):\n\n",
        );
        let _ = writeln!(
            out,
            "    GET {}/api/v1/artifacts?original_sha256=<hash>\n",
            instrumentation.patwari_url.trim_end_matches('/'),
        );
        let _ = writeln!(
            out,
            "_Instrumentation — sync {} · fold {} · {} sessions · {} folded · cache {} hits / {} \
             misses ({} fetched) · archive {} · cache {}_",
            format::elapsed(instrumentation.sync.elapsed),
            format::elapsed(instrumentation.fold_elapsed),
            instrumentation.sessions_folded,
            format::bytes(instrumentation.bytes_folded),
            instrumentation.sync.cache_hits,
            instrumentation.sync.cache_misses,
            format::bytes(instrumentation.sync.bytes_fetched),
            instrumentation.patwari_url,
            display_path(&instrumentation.cache_root),
        );
    }
}

/// RFC 3339 to the second: a coaching window has no business being reported to the millisecond.
fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use munshi_transcript::SessionSummary;

    use super::*;
    use crate::metrics::{ToolOutcomes, ToolTally};
    use crate::rules;

    fn instrumentation() -> Instrumentation {
        Instrumentation {
            sync: SyncStats {
                sessions_listed: 1,
                cache_hits: 1,
                cache_misses: 0,
                bytes_fetched: 0,
                elapsed: Duration::from_millis(120),
            },
            fold_elapsed: Duration::from_millis(7),
            sessions_folded: 1,
            bytes_folded: 4096,
            patwari_url: "http://127.0.0.1:8080".to_owned(),
            cache_root: PathBuf::from("/tmp/qanungo"),
        }
    }

    fn window() -> Window {
        use clap::Parser;

        let crate::cli::Command::Report(args) =
            crate::cli::Cli::parse_from(["qanungo", "report", "--last", "7d"]).command;
        args.last
    }

    fn session() -> SessionMetrics {
        let first = DateTime::parse_from_rfc3339("2026-08-10T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        SessionMetrics {
            source_hash: "ab".repeat(32),
            source_agent: "claude-code".to_owned(),
            summary: SessionSummary {
                user_requests: 4,
                assistant_messages: 9,
                tool_activities: 40,
                first_timestamp: Some(first),
                last_timestamp: Some(first + TimeDelta::hours(6)),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes {
                total: ToolTally {
                    attempts: 20,
                    errors: 9,
                },
                by_tool: [(
                    "Bash".to_owned(),
                    ToolTally {
                        attempts: 20,
                        errors: 9,
                    },
                )]
                .into_iter()
                .collect(),
                unattributed: 0,
            },
            bytes_folded: 4096,
        }
    }

    fn render(sessions: &[SessionMetrics], skipped: &[SkippedNote]) -> String {
        let findings = rules::evaluate(sessions);
        let instrumentation = instrumentation();
        let window = window();
        Report {
            window: &window,
            generated_at: DateTime::parse_from_rfc3339("2026-08-17T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            sessions,
            findings: &findings,
            skipped,
            instrumentation: &instrumentation,
        }
        .render()
    }

    #[test]
    fn an_empty_window_still_reports_and_still_instruments() {
        let markdown = render(&[], &[]);
        assert!(markdown.starts_with("# Coaching report — last 7d"));
        assert!(markdown.contains("No archived sessions fell in this window"));
        assert!(markdown.contains("_Instrumentation —"));
        assert!(markdown.contains("cited by transcript content hash only"));
        // Nothing to say about cadence or tools when there is nothing folded.
        assert!(!markdown.contains("## Cadence"));
        assert!(!markdown.contains("## Findings"));
    }

    #[test]
    fn a_finding_renders_problem_action_and_hash_evidence() {
        let markdown = render(&[session()], &[]);
        assert!(markdown.contains("### High tool error rate"));
        assert!(markdown.contains("**Problem** —"));
        assert!(markdown.contains("**Action** —"));
        assert!(markdown.contains(&format!("`sha256:{}`", "ab".repeat(32))));
        assert!(markdown.contains("### Marathon session"));
    }

    #[test]
    fn the_footer_carries_every_instrumented_quantity() {
        let markdown = render(&[session()], &[]);
        let footer = markdown
            .lines()
            .find(|line| line.starts_with("_Instrumentation"))
            .expect("the footer is always present");
        assert!(footer.contains("sync 120 ms"));
        assert!(footer.contains("fold 7 ms"));
        assert!(footer.contains("1 sessions"));
        assert!(footer.contains("4.0 KiB folded"));
        assert!(footer.contains("cache 1 hits / 0 misses"));
        assert!(footer.contains("http://127.0.0.1:8080"));
    }

    #[test]
    fn gaps_are_stated_rather_than_swallowed() {
        let markdown = render(
            &[session()],
            &[SkippedNote {
                count: 2,
                reason: "no transcript artifact".to_owned(),
            }],
        );
        assert!(markdown.contains("## Gaps"));
        assert!(markdown.contains("- 2 — no transcript artifact"));
    }

    #[test]
    fn the_tool_table_names_tools_and_their_rates() {
        let markdown = render(&[session()], &[]);
        assert!(markdown.contains("| Tool | Calls with an outcome | Failed | Rate |"));
        assert!(markdown.contains("| Bash | 20 | 9 | 45% |"));
    }
}
