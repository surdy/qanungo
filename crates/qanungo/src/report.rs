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
//! Repeated-command churn is the first metric that has to *compare* transcript content to derive
//! anything, and it does not weaken that: the command values live in the fold's own scratch map
//! for the length of one transcript and are reduced to counts before it returns, so no type this
//! module can see has ever held one. The report says a command ran six times; it never says which
//! command, not truncated and not in evidence. Quoting verbatim is qanungo #8's problem.
//!
//! Evidence is therefore a content hash. Somebody who wants the detail this report refuses to
//! print pulls the transcript from the archive and reads it in full, with the archive's own
//! access story intact. The first qanungo surface that quotes verbatim lands behind its own
//! issue, not by loosening this one.
//!
//! Scores, lane names, deltas, and arrows are aggregates like any other and change none of this:
//! the scoring pack reads counts and rates that the fold already reduced, and nothing it renders
//! has ever held a transcript string.
//!
//! # The instrumentation footer
//!
//! Every run ends with fold wall-time, session counts, bytes folded, cache hits/misses, and the
//! **rule-pack stamp**. It is not decoration: the fold-cost half is the longitudinal measurement
//! the future event-store go/no-go is made against, and the stamp is what makes a score
//! comparable to another report's at all (qanungo ADR 0001). A report without either would make
//! a decision unmeasurable.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

use crate::cli::Window;
use crate::format;
use crate::metrics::{Cadence, SessionMetrics, Totals};
use crate::rules::Finding;
use crate::scoring::{Direction, Lane, LaneScore, RulePack, Scorecard, Trend};
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
    /// Sessions folded for the reported window.
    pub sessions_folded: usize,
    /// Sessions folded for the comparison window *as well* — the price of a trend arrow, kept
    /// separate so the fold-cost decision is argued from the whole number and the coverage
    /// decision from the reported one.
    pub comparison_sessions_folded: usize,
    /// Decompressed transcript bytes the fold actually read, across both windows. Deliberately a
    /// different quantity from [`SyncStats::bytes_transferred`], which is what crossed the wire:
    /// fold cost scales with the former, network cost with the latter, and the footer reports both
    /// so neither decision is argued from the wrong number.
    pub bytes_folded: u64,
    /// The pack every score in this report was computed with. Two reports compare iff their
    /// stamps match.
    pub rule_pack: RulePack,
    pub patwari_url: String,
    pub cache_root: PathBuf,
}

/// Everything one report is rendered from.
pub struct Report<'a> {
    pub window: &'a Window,
    pub generated_at: DateTime<Utc>,
    /// The reported window's sessions.
    pub sessions: &'a [SessionMetrics],
    /// The equal-length window immediately before it, folded for the trend arrows. Empty when the
    /// archive held nothing there.
    pub previous: &'a [SessionMetrics],
    /// Whether a comparison window was asked for at all. `false` — a window so long that doubling
    /// it overflows — is a different thing from a comparison window that came back empty, and the
    /// report says which.
    pub compared: bool,
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
            self.render_scores(&mut out, &totals, &cadence);
            self.render_findings(&mut out);
        }
        self.render_gaps(&mut out);
        self.render_footer(&mut out);
        out
    }

    /// When the reported window opens.
    fn opens_at(&self) -> DateTime<Utc> {
        self.window.opens_at(self.generated_at)
    }

    /// When the comparison window opens, for a run that asked for one.
    fn comparison_opens_at(&self) -> Option<DateTime<Utc>> {
        self.compared
            .then(|| self.window.comparison_opens_at(self.generated_at))
            .flatten()
    }

    fn render_window(&self, out: &mut String, totals: &Totals) {
        let since = self.opens_at();
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
            "- {} sessions across {} active days, UTC ({per_active_day} per active day).",
            self.sessions.len(),
            cadence.active_days(),
        );
        // Duration is reported as *active* time — gaps over the idle threshold excluded — with
        // raw span kept beside it as context. Across the real archive the two differ by about a
        // factor of twenty, and only the first of them is time somebody spent working.
        if let (Some(median), Some(longest)) = (cadence.median_active(), cadence.longest_active()) {
            let _ = writeln!(
                out,
                "- Median session active {}; longest {}.",
                format::span(median),
                format::span(longest),
            );
        }
        if let (Some(median), Some(longest)) = (cadence.median_span(), cadence.longest_span()) {
            let _ = writeln!(
                out,
                "- Raw spans, for context: median {}; longest {}.",
                format::span(median),
                format::span(longest),
            );
        }
        if cadence.total_sittings > 0 {
            let _ = writeln!(
                out,
                "- {} of active time across {} sittings, inside {} of session span.",
                format::span(cadence.total_active),
                cadence.total_sittings,
                format::span(cadence.total_span),
            );
        }
        if let Some((busiest, count)) = cadence.per_day.iter().max_by_key(|(_, count)| **count) {
            let _ = writeln!(out, "- Busiest day {busiest} (UTC): {count} sessions.");
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
        self.render_churn(out, totals);
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

    /// Repeated-command churn, in counts. Never a command — the busiest one is reported by how
    /// many times it ran and by which session ran it, and a reader who wants to know *what* it
    /// was fetches the transcript.
    ///
    /// A window whose harnesses record no command field says so outright rather than printing a
    /// zero it did not measure.
    fn render_churn(&self, out: &mut String, totals: &Totals) {
        let churn = &totals.churn;
        let Some(share) = churn.repeat_share() else {
            out.push_str(
                "- No tool call in this window recorded a command, so no repeated-command churn \
                 is defined.\n",
            );
            return;
        };
        let _ = writeln!(
            out,
            "- {} of {} command-bearing tool calls re-ran a command the session had already run \
             ({}).",
            churn.repeats,
            churn.command_events,
            format::percent(share),
        );
        let _ = writeln!(
            out,
            "- {} of {} sessions with command activity re-ran one; the busiest ran a single \
             command {} times.",
            churn.sessions_with_repeats, churn.sessions_with_commands, churn.busiest_command_runs,
        );
        if churn.untracked_events > 0 {
            let _ = writeln!(
                out,
                "- {} command-bearing calls exceeded a session's distinct-command cap and were \
                 counted as activity but never as repeats, so the churn above is a floor.",
                churn.untracked_events,
            );
        }
    }

    /// The five practice lanes, scored per harness, with window-over-window arrows.
    ///
    /// Everything here is an aggregate of aggregates: a rate over sessions, a count of sessions,
    /// a score derived from those. Nothing new is quoted, and nothing can be — see the module
    /// docs.
    fn render_scores(&self, out: &mut String, totals: &Totals, cadence: &Cadence) {
        let now = Scorecard::fold(self.sessions);
        let before = self
            .comparison_opens_at()
            .map(|_| Scorecard::fold(self.previous));

        out.push_str("\n## Practice scores\n\n");
        self.render_score_preamble(out);
        self.render_score_table(out, &now, before.as_ref());
        self.render_blend_note(out, totals);
        self.render_score_reasons(out, &now, before.as_ref());
        self.render_headline_metrics(out, totals, cadence);
    }

    fn render_score_preamble(&self, out: &mut String) {
        out.push_str(
            "Five practice lanes, scored 0–100 per harness from this window's own readings. A \
             score is a function of the rule pack stamped in the footer and of nothing else, so \
             **two reports compare only when that stamp matches** — an arrow must mean behaviour \
             drift, never rule drift. 100 means nothing this pack penalizes was observed, not \
             that the practice is perfect. Scores move across windows; they do **not** compare \
             across lanes, because each lane's constants are anchored on its own rules' \
             thresholds rather than on a shared difficulty scale.\n\n",
        );
        match self.comparison_opens_at() {
            Some(comparison_opens_at) if !self.previous.is_empty() => {
                let _ = writeln!(
                    out,
                    "Arrows compare this window with the equal-length one before it, {} → {} \
                     (UTC), and appear only where **both** windows measured the lane. Both windows \
                     are cut on archive time — when the snapshot completed, the same clock that \
                     selected the reported window — so a long-lived transcript resumed across the \
                     boundary is archived again and appears in this window only, carrying its \
                     earlier work with it.\n",
                    stamp(comparison_opens_at),
                    stamp(self.opens_at()),
                );
            }
            Some(comparison_opens_at) => {
                let _ = writeln!(
                    out,
                    "No score carries an arrow: the archive holds no session between {} and {} \
                     (UTC) to compare against.\n",
                    stamp(comparison_opens_at),
                    stamp(self.opens_at()),
                );
            }
            None => out.push_str(
                "No score carries an arrow: this window is too long to place an equal-length one \
                 before it.\n\n",
            ),
        }
    }

    fn render_score_table(&self, out: &mut String, now: &Scorecard, before: Option<&Scorecard>) {
        let columns = harness_columns(now, before);
        out.push_str("| Lane |");
        for column in &columns {
            let _ = write!(out, " {column} |");
        }
        out.push_str(" Fleet |\n| --- |");
        for _ in 0..=columns.len() {
            out.push_str(" ---: |");
        }
        out.push('\n');
        for lane in Lane::ALL {
            let _ = write!(out, "| {} |", lane.title());
            for column in &columns {
                let cell = match now.harness(column) {
                    Some(harness) => cell(
                        harness.lane(lane),
                        before
                            .and_then(|card| card.harness(column))
                            .and_then(|harness| harness.lane(lane).score()),
                    ),
                    None => "no sessions".to_owned(),
                };
                let _ = write!(out, " {cell} |");
            }
            let fleet = match now.fleet(lane) {
                Some(blend) => {
                    // Only a blend over the *same* harnesses can carry an arrow: a roster change
                    // moves an unweighted mean on its own, and reporting that as behaviour is the
                    // exact failure this blend rule exists to avoid.
                    let comparable = blend.comparable(before.and_then(|card| card.fleet(lane)));
                    format!("{}{}", blend.score, arrow(blend.score, comparable))
                }
                // A lane nothing types a signal for reads differently from one whose signals were
                // all silent this window, and the blend column keeps the two apart.
                None if lane.untyped().is_some() => "not scored".to_owned(),
                None => "no reading".to_owned(),
            };
            let _ = writeln!(out, " {fleet} |");
        }
    }

    fn render_blend_note(&self, out: &mut String, totals: &Totals) {
        out.push_str(
            "\nFleet blends a row by taking the **unweighted mean of the per-harness scores in \
             it** — every harness that scored the lane counts once, so the number moves when \
             behaviour does and not when the harness mix does. Its arrow appears only when the \
             same harnesses scored the lane in both windows.\n",
        );
        let codex = totals
            .by_agent
            .iter()
            .find(|(agent, _)| agent.starts_with("codex"))
            .map(|(_, count)| *count);
        let _ = writeln!(
            out,
            "Sampling bias, stated rather than hidden: codex-cli is manual-archive-only in munshi \
             and is under-represented in the archive relative to how much it is used — {}.",
            match codex {
                Some(count) => format!(
                    "it contributed {count} of this window's {} sessions, so read its column as a \
                     sample of the sessions somebody remembered to archive",
                    totals.sessions,
                ),
                None =>
                    "it contributed no session to this window, so nothing above describes it at all"
                        .to_owned(),
            },
        );
    }

    /// The contributing readings, beside the score they produced. A score nobody can take apart
    /// is a number to be argued with rather than acted on.
    fn render_score_reasons(&self, out: &mut String, now: &Scorecard, before: Option<&Scorecard>) {
        out.push_str("\n### Why the scores are what they are\n");
        for harness in &now.harnesses {
            for lane in Lane::ALL {
                let score = harness.lane(lane);
                if lane.untyped().is_some() {
                    continue;
                }
                let earlier = before
                    .and_then(|card| card.harness(&harness.source_agent))
                    .and_then(|harness| harness.lane(lane).score());
                let heading = match score.score() {
                    Some(value) => format!(
                        "{value}{}",
                        earlier.map_or_else(String::new, |earlier| format!(
                            " (was {earlier} last window)"
                        )),
                    ),
                    None => "no reading in this window".to_owned(),
                };
                let _ = writeln!(
                    out,
                    "\n**{} — {}: {heading}**\n",
                    harness.source_agent,
                    lane.title(),
                );
                for component in score.components() {
                    let _ = writeln!(
                        out,
                        "- {}: {} — {}",
                        component.label,
                        component.detail,
                        match component.cost {
                            Some(cost) => format!("{cost:.1} points off"),
                            None => "no say in the score".to_owned(),
                        },
                    );
                }
            }
        }
        for lane in Lane::ALL {
            if let Some(reason) = lane.untyped() {
                let _ = writeln!(out, "\n**{} — not scored.** {reason}.", lane.title());
            }
        }
    }

    /// The headline metrics, window over window.
    ///
    /// Arrows here are **direction only**: up is a larger number, never a better one. Whether a
    /// direction is good is the score's job — a metric table that judged would need a polarity per
    /// row, and one of them silently backwards is exactly what a coaching report cannot afford.
    fn render_headline_metrics(&self, out: &mut String, totals: &Totals, cadence: &Cadence) {
        let Some(comparison_opens_at) = self.comparison_opens_at() else {
            return;
        };
        if self.previous.is_empty() {
            return;
        }
        let earlier = Totals::fold(self.previous);
        let earlier_cadence = Cadence::fold(self.previous);

        out.push_str("\n### Headline metrics\n\n");
        let _ = writeln!(
            out,
            "Pooled across every folded session in each window, harnesses together — so unlike \
             the scores above, these move with the harness mix as well as with behaviour. Arrows \
             are direction only: ▲ is a larger number, not a better one.\n",
        );
        let _ = writeln!(
            out,
            "| Metric | {} → {} | {} → {} | Change |",
            stamp(self.opens_at()),
            stamp(self.generated_at),
            stamp(comparison_opens_at),
            stamp(self.opens_at()),
        );
        out.push_str("| --- | ---: | ---: | ---: |\n");

        // The change is rendered by its own function because a difference is not always in the
        // same unit as the values: a rate that moved from 2% to 5% moved by three *points*, and
        // calling that "3%" would invite reading it as a relative change.
        let mut row = |metric: &str,
                       now: Option<f64>,
                       was: Option<f64>,
                       render: fn(f64) -> String,
                       delta: fn(f64) -> String| {
            let _ = writeln!(
                out,
                "| {metric} | {} | {} | {} |",
                now.map_or_else(|| "—".to_owned(), render),
                was.map_or_else(|| "—".to_owned(), render),
                change(now, was, delta),
            );
        };
        row(
            "Sessions folded",
            Some(totals.sessions as f64),
            Some(earlier.sessions as f64),
            count,
            count,
        );
        row(
            "Tool calls that failed",
            totals.tools.error_rate(),
            earlier.tools.error_rate(),
            format::percent,
            percentage_points,
        );
        row(
            "Tool activities per user request",
            totals.tools_per_request(),
            earlier.tools_per_request(),
            format::ratio,
            format::ratio,
        );
        row(
            "Command-bearing calls that were repeats",
            totals.churn.repeat_share(),
            earlier.churn.repeat_share(),
            format::percent,
            percentage_points,
        );
        row(
            "Median session active time",
            cadence.median_active().map(seconds),
            earlier_cadence.median_active().map(seconds),
            span_of_seconds,
            span_of_seconds,
        );
        row(
            "Sessions per active day",
            cadence.sessions_per_active_day(),
            earlier_cadence.sessions_per_active_day(),
            format::ratio,
            format::ratio,
        );
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
        let comparison = match instrumentation.comparison_sessions_folded {
            0 => String::new(),
            count => format!(" (+{count} for the comparison window)"),
        };
        let _ = writeln!(
            out,
            "_Instrumentation — sync {} · fold {} · {} sessions{comparison} · {} folded · cache \
             {} hits / {} misses ({} transferred) · rule pack {} · archive {} · cache {}_",
            format::elapsed(instrumentation.sync.elapsed),
            format::elapsed(instrumentation.fold_elapsed),
            instrumentation.sessions_folded,
            format::bytes(instrumentation.bytes_folded),
            instrumentation.sync.cache_hits,
            instrumentation.sync.cache_misses,
            format::bytes(instrumentation.sync.bytes_transferred),
            instrumentation.rule_pack.stamp(),
            instrumentation.patwari_url,
            display_path(&instrumentation.cache_root),
        );
    }
}

/// RFC 3339 to the second: a coaching window has no business being reported to the millisecond.
///
/// **Every timestamp this report prints is UTC**, and says so where it is printed. The transcripts
/// are the only clock available: `munshi-transcript` types a record's time as
/// `Option<DateTime<Utc>>`, normalized from whatever the harness wrote, and nothing in munshi —
/// not the record, not the snapshot manifest, not the capture provenance — states the capture
/// machine's UTC offset. qanungo #4 asks for UTC *plus* that offset; the offset is a munshi#77
/// candidate pull, and until it is typed there is nothing here to render it from. It is not
/// inferred from the archive's own clock, and it is not guessed from a hostname: a late-evening
/// session west of Greenwich lands on the following UTC day, and the report would rather be
/// visibly off by a day than invisibly wrong about a timezone.
pub(crate) fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The harnesses either window scored, in label order.
///
/// The union rather than the reported window's own: a harness that stopped appearing is a fact
/// worth showing a column for, and hiding it would make a fleet blend silently change what it is
/// a mean of. Shared with [`crate::dashboard`], whose per-harness split is the same set of columns
/// under a different rendering.
pub(crate) fn harness_columns(now: &Scorecard, before: Option<&Scorecard>) -> Vec<String> {
    let mut columns: Vec<String> = now
        .harnesses
        .iter()
        .chain(before.into_iter().flat_map(|card| &card.harnesses))
        .map(|harness| harness.source_agent.clone())
        .collect();
    columns.sort_unstable();
    columns.dedup();
    columns
}

/// One lane's cell: its score and arrow, or the reason it has neither.
fn cell(score: &LaneScore, before: Option<u8>) -> String {
    match score {
        LaneScore::Scored { score, .. } => format!("{score}{}", arrow(*score, before)),
        LaneScore::NoReading { .. } => "no reading".to_owned(),
        LaneScore::Untyped(_) => "not scored".to_owned(),
    }
}

/// A score's movement against the comparison window — **rendered only when both windows measured
/// the lane**. An arrow drawn against a window that could not measure it would be reporting the
/// archive's shape as behaviour.
///
/// The *rule* is [`Trend::between`]'s, shared with the dashboard, which draws the same movement as
/// a direction and a magnitude rather than as a glyph. This function is only the Markdown of it.
fn arrow(now: u8, before: Option<u8>) -> String {
    let Some(trend) = Trend::between(now, before) else {
        return String::new();
    };
    match trend.direction() {
        Direction::Flat => " =".to_owned(),
        direction => format!(" {} {}", direction.glyph(), trend.magnitude()),
    }
}

/// A metric's movement, on the same rule: no reading on either side, no arrow.
///
/// Shared with [`crate::cost_report`], which draws its window-over-window delta on exactly this
/// rule — a total that moved is an arrow, a total the other window could not measure is a dash,
/// and a move too small for the renderer to show is flat.
pub(crate) fn change(now: Option<f64>, before: Option<f64>, render: fn(f64) -> String) -> String {
    let (Some(now), Some(before)) = (now, before) else {
        return "—".to_owned();
    };
    let delta = now - before;
    // A move too small for the renderer to distinguish from zero is flat: an arrow beside
    // a rendered zero reads as a contradiction.
    let magnitude = render(delta.abs());
    if magnitude == render(0.0) {
        return "=".to_owned();
    }
    let direction = if delta > 0.0 { "▲" } else { "▼" };
    format!("{direction} {magnitude}")
}

/// A whole count.
fn count(value: f64) -> String {
    format!("{value:.0}")
}

/// A difference between two fractions, in percentage points rather than in percent — see
/// [`Report::render_headline_metrics`].
///
/// Carried to one decimal where the values themselves are whole percentages, because a real move
/// smaller than a point is common here. A move smaller than the decimal can show is rendered
/// flat by [`change`], never as an arrow beside a zero.
fn percentage_points(value: f64) -> String {
    format!("{:.1}pp", value * 100.0)
}

/// A duration as a plain number of seconds, so a span can go through the same `f64` comparison
/// every other metric does.
fn seconds(span: TimeDelta) -> f64 {
    span.num_seconds() as f64
}

/// The inverse, for rendering.
fn span_of_seconds(value: f64) -> String {
    format::span(TimeDelta::try_seconds(value.round() as i64).unwrap_or_else(TimeDelta::zero))
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use munshi_transcript::SessionSummary;

    use super::*;
    use crate::metrics::{Activity, CommandChurn, ToolOutcomes, ToolTally};
    use crate::rules;

    fn instrumentation() -> Instrumentation {
        Instrumentation {
            sync: SyncStats {
                sessions_listed: 1,
                cache_hits: 1,
                cache_misses: 0,
                bytes_transferred: 0,
                elapsed: Duration::from_millis(120),
            },
            fold_elapsed: Duration::from_millis(7),
            sessions_folded: 1,
            comparison_sessions_folded: 0,
            bytes_folded: 4096,
            rule_pack: RulePack::current(),
            patwari_url: "http://127.0.0.1:8080".to_owned(),
            cache_root: PathBuf::from("/tmp/qanungo"),
        }
    }

    fn window() -> Window {
        use clap::Parser;

        let crate::cli::Command::Report(args) =
            crate::cli::Cli::parse_from(["qanungo", "report", "--last", "7d"]).command
        else {
            panic!("`report` parses as the report command");
        };
        args.last
    }

    /// A six-hour session with a two-and-a-half-hour push inside it: enough to fire the marathon
    /// rule on the sitting rather than on the span, which is the distinction the report now
    /// renders.
    fn session() -> SessionMetrics {
        let first = DateTime::parse_from_rfc3339("2026-08-10T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut timestamps: Vec<_> = (0..=10)
            .map(|step| first + TimeDelta::minutes(step * 15))
            .collect();
        timestamps.push(first + TimeDelta::hours(6));
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
            activity: Activity::over(timestamps),
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
            commands: CommandChurn::default(),
            bytes_folded: 4096,
        }
    }

    fn render(sessions: &[SessionMetrics], skipped: &[SkippedNote]) -> String {
        render_against(sessions, &[], skipped)
    }

    fn render_against(
        sessions: &[SessionMetrics],
        previous: &[SessionMetrics],
        skipped: &[SkippedNote],
    ) -> String {
        let findings = rules::evaluate(sessions);
        let instrumentation = instrumentation();
        let window = window();
        Report {
            window: &window,
            generated_at: DateTime::parse_from_rfc3339("2026-08-17T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            sessions,
            previous,
            compared: true,
            findings: &findings,
            skipped,
            instrumentation: &instrumentation,
        }
        .render()
    }

    /// A window of `count` claude-code sessions, the first `marathons` of which work one long
    /// unbroken push — enough sessions for a fire rate to be a reading at all.
    fn hygiene_window(count: usize, marathons: usize) -> Vec<SessionMetrics> {
        let first = DateTime::parse_from_rfc3339("2026-08-10T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let step = crate::rules::thresholds::IDLE_GAP;
        let marathon = crate::rules::thresholds::MARATHON_SITTING_ACTIVE;
        (0..count)
            .map(|index| {
                let worked = if index < marathons {
                    marathon + step
                } else {
                    marathon / 4
                };
                let steps = worked.num_minutes() / step.num_minutes();
                let timestamps: Vec<_> = (0..=steps).map(|n| first + step * n as i32).collect();
                let last = *timestamps.last().expect("at least the first record");
                SessionMetrics {
                    source_hash: format!("{index:02x}").repeat(32),
                    source_agent: "claude-code".to_owned(),
                    summary: SessionSummary {
                        user_requests: 4,
                        tool_activities: 20,
                        first_timestamp: Some(first),
                        last_timestamp: Some(last),
                        ..SessionSummary::default()
                    },
                    tools: ToolOutcomes::default(),
                    activity: Activity::over(timestamps),
                    commands: CommandChurn::default(),
                    bytes_folded: 1024,
                }
            })
            .collect()
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

    /// Duration is reported as work done, not as calendar time occupied — with the span kept
    /// visible so a reader can see the difference the idle threshold made.
    #[test]
    fn cadence_leads_with_active_time_and_keeps_the_span_as_context() {
        let markdown = render(&[session()], &[]);
        assert!(
            markdown.contains("- Median session active 2h 30m; longest 2h 30m."),
            "{markdown}"
        );
        assert!(
            markdown.contains("- Raw spans, for context: median 6h 00m; longest 6h 00m."),
            "{markdown}"
        );
        assert!(
            markdown.contains(
                "- 2h 30m of active time across 2 sittings, inside 6h 00m of \
                               session span."
            ),
            "{markdown}"
        );
    }

    /// A window whose harnesses record no command says so, rather than printing a zero share it
    /// never measured.
    #[test]
    fn churn_is_reported_as_undefined_when_nothing_recorded_a_command() {
        let markdown = render(&[session()], &[]);
        assert!(
            markdown.contains(
                "- No tool call in this window recorded a command, so no repeated-command churn \
                 is defined."
            ),
            "{markdown}"
        );
    }

    #[test]
    fn churn_is_reported_in_counts_and_never_in_commands() {
        let mut churned = session();
        churned.commands = CommandChurn {
            command_events: 40,
            repeats: 10,
            distinct_commands: 30,
            repeated_commands: 3,
            busiest_command_runs: 7,
            untracked_events: 0,
        };
        let markdown = render(&[churned], &[]);
        assert!(
            markdown.contains(
                "- 10 of 40 command-bearing tool calls re-ran a command the session had already \
                 run (25%)."
            ),
            "{markdown}"
        );
        assert!(
            markdown.contains(
                "- 1 of 1 sessions with command activity re-ran one; the busiest ran a single \
                 command 7 times."
            ),
            "{markdown}"
        );
    }

    #[test]
    fn the_tool_table_names_tools_and_their_rates() {
        let markdown = render(&[session()], &[]);
        assert!(markdown.contains("| Tool | Calls with an outcome | Failed | Rate |"));
        assert!(markdown.contains("| Bash | 20 | 9 | 45% |"));
    }

    /// A lane no signal feeds says so, in every column, and never carries a number. The two rows
    /// below are the whole no-signal-no-claim discipline as a reader sees it.
    #[test]
    fn an_unfed_lane_renders_as_not_scored_in_every_column() {
        let markdown = render(&hygiene_window(20, 5), &[]);
        assert!(
            markdown.contains("| Code Review | not scored | not scored |"),
            "{markdown}"
        );
        assert!(
            markdown.contains("| Context Management | not scored | not scored |"),
            "{markdown}"
        );
        assert!(
            markdown.contains("**Code Review — not scored.** no signal typed for this lane yet"),
            "{markdown}"
        );
        // And a fed one does carry a number, with the readings that produced it beside it.
        assert!(
            markdown.contains("| Session Hygiene | 50 | 50 |"),
            "{markdown}"
        );
        assert!(
            markdown.contains(
                "- Marathon session: fired on 5 of 20 sessions with a measurable \
                               sitting (25%) — 50.0 points off"
            ),
            "{markdown}"
        );
    }

    /// An arrow appears where both windows measured the lane, points the right way, and carries
    /// the size of the move.
    #[test]
    fn a_lane_measured_in_both_windows_carries_an_arrow() {
        let improved = render_against(&hygiene_window(20, 2), &hygiene_window(20, 5), &[]);
        // 80 this window against 50 last: a thirty-point improvement.
        assert!(
            improved.contains("| Session Hygiene | 80 ▲ 30 | 80 ▲ 30 |"),
            "{improved}"
        );
        let worsened = render_against(&hygiene_window(20, 5), &hygiene_window(20, 2), &[]);
        assert!(
            worsened.contains("| Session Hygiene | 50 ▼ 30 | 50 ▼ 30 |"),
            "{worsened}"
        );
        let flat = render_against(&hygiene_window(20, 3), &hygiene_window(20, 3), &[]);
        assert!(flat.contains("| Session Hygiene | 70 = | 70 = |"), "{flat}");
    }

    /// The rule the arrows live by: a lane the comparison window could not measure gets no arrow,
    /// rather than an arrow drawn against nothing.
    #[test]
    fn a_lane_unmeasurable_in_either_window_carries_no_arrow() {
        // The comparison window is below the minimum eligible-session count, so its hygiene lane
        // has no reading at all.
        let markdown = render_against(&hygiene_window(20, 5), &hygiene_window(2, 2), &[]);
        assert!(
            markdown.contains("| Session Hygiene | 50 | 50 |"),
            "{markdown}"
        );
        assert!(!markdown.contains("Session Hygiene | 50 ▲"), "{markdown}");
        assert!(!markdown.contains("Session Hygiene | 50 ▼"), "{markdown}");

        // And with no comparison window at all, the report says so outright.
        let alone = render(&hygiene_window(20, 5), &[]);
        assert!(
            alone.contains("No score carries an arrow: the archive holds no session between"),
            "{alone}"
        );
        assert!(!alone.contains("### Headline metrics"), "{alone}");
    }

    /// Both windows are named where the arrows are, so a reader knows what is being compared with
    /// what — and both are UTC, because UTC is the only clock the transcripts carry.
    #[test]
    fn the_comparison_windows_are_labelled_explicitly() {
        let markdown = render_against(&hygiene_window(20, 2), &hygiene_window(20, 5), &[]);
        assert!(
            markdown.contains(
                "Arrows compare this window with the equal-length one before it, \
                 2026-08-03T12:00:00Z → 2026-08-10T12:00:00Z (UTC)"
            ),
            "{markdown}"
        );
        assert!(
            markdown.contains(
                "| Metric | 2026-08-10T12:00:00Z → 2026-08-17T12:00:00Z | \
                 2026-08-03T12:00:00Z → 2026-08-10T12:00:00Z | Change |"
            ),
            "{markdown}"
        );
        assert!(
            markdown.contains("| Sessions folded | 20 | 20 | = |"),
            "{markdown}"
        );
    }

    /// The blend states its own rule, and the sampling bias travels with it.
    #[test]
    fn the_fleet_blend_states_its_rule_and_its_sampling_bias() {
        let markdown = render(&hygiene_window(20, 5), &[]);
        assert!(
            markdown.contains("unweighted mean of the per-harness scores in it"),
            "{markdown}"
        );
        assert!(
            markdown.contains("codex-cli is manual-archive-only in munshi"),
            "{markdown}"
        );
        assert!(
            markdown.contains("it contributed no session to this window"),
            "{markdown}"
        );
    }

    /// The stamp is what makes any of the numbers above comparable to another report's, so it is
    /// in the footer of every run.
    #[test]
    fn the_footer_carries_the_rule_pack_stamp() {
        let markdown = render(&[session()], &[]);
        let footer = markdown
            .lines()
            .find(|line| line.starts_with("_Instrumentation"))
            .expect("the footer is always present");
        assert!(
            footer.contains(&format!("rule pack {}", RulePack::current().stamp())),
            "{footer}"
        );
    }
}
