//! Markdown rendering for the standup lane, and the redaction line this one actually needs.
//!
//! # The redaction line (the first one that is a filter)
//!
//! `report` and `cost` hold their line by *construction*: no path exists from a transcript's free
//! text to their output, and their tests pin that with canary fixtures. This document is the first
//! that cannot make that claim, because rendering munshi's summaries is the whole point of it.
//!
//! So the line here is different in kind, and stated as such: **every string below that came out
//! of an archive has been through the [`Redactor`](crate::redaction::Redactor)**, before it
//! reached this module at all. [`crate::standup`] scrubs on the way into its own types, so nothing
//! here can render a field that was not scrubbed — there is no unscrubbed copy in scope to render
//! by mistake. The footer names the pattern revision the scrub ran at, says which of the two passes
//! were on, and reports what fired as counts per pattern id and nothing else.
//!
//! A run with `--no-redact` says so in the footer, in the same sentence, in the document itself.
//! That is the point of spelling the flag as a negation: turning the scrub off is a thing a reader
//! of the output can see happened.
//!
//! # No model, no reconstruction
//!
//! Every sentence in the body was written by the harness that captured the session, into
//! `summary.md`, when the session was captured. This lane selects, orders, groups, and deduplicates
//! them. It does not summarize the summaries: the `/standup` contrib skill is where any polishing
//! belongs, and it can only polish honestly if what it is handed is the archive's own words.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::cli::Window;
use crate::format;
use crate::redaction::{PATTERN_REVISION, RedactionReport, Redactor};
use crate::report::{SkippedNote, stamp};
use crate::standup::{RolledUp, Standup, StandupSession};
use crate::sync::SyncStats;

/// What a standup run cost, folded into the footer.
#[derive(Debug, Clone)]
pub struct StandupInstrumentation {
    pub sync: SyncStats,
    /// Wall-time of reading and folding the summaries alone, network excluded.
    pub fold_elapsed: Duration,
    /// The redactor the flags asked for, so the footer can say which passes ran.
    pub redactor: Redactor,
    pub patwari_url: String,
    pub cache_root: PathBuf,
}

/// Everything one standup document is rendered from.
pub struct StandupReport<'a> {
    pub window: &'a Window,
    pub generated_at: DateTime<Utc>,
    pub standup: &'a Standup,
    pub instrumentation: &'a StandupInstrumentation,
}

impl StandupReport<'_> {
    /// Renders the standup as Markdown.
    ///
    /// Deterministic in full: the same window over the same archive with the same flags produces
    /// byte-identical output, because every ordering in [`Standup`] is total and nothing here
    /// reads a clock except the two timestamps it prints.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Standup — last {}\n", self.window);
        self.render_window(&mut out);
        self.render_repositories(&mut out);
        self.render_rollup(
            &mut out,
            "Decisions",
            &self.standup.decisions,
            DECISIONS_NOTE,
        );
        self.render_rollup(&mut out, "Open items", &self.standup.open_items, OPEN_NOTE);
        self.render_gaps(&mut out);
        self.render_footer(&mut out);
        out
    }

    fn render_window(&self, out: &mut String) {
        let _ = writeln!(
            out,
            "Sessions archived since {} (UTC), read at {} (UTC).",
            stamp(self.window.opens_at(self.generated_at)),
            stamp(self.generated_at),
        );
        let standup = self.standup;
        if standup.sessions == 0 {
            out.push_str(
                "\nNo archived session in this window carried a summary to narrate. Anything the \
                 archive did hold is in Gaps below.\n",
            );
            return;
        }
        let repositories = standup.repositories_narrated();
        let _ = writeln!(
            out,
            "\n**{} sessions across {repositories} {}**, narrated from the summaries munshi wrote \
             when it captured them — no model reconstructed any of this.",
            standup.sessions,
            plural(repositories, "repository", "repositories"),
        );
    }

    /// The body: one section per repository, sessions newest first inside it.
    fn render_repositories(&self, out: &mut String) {
        for group in &self.standup.repositories {
            let _ = writeln!(out, "\n## {}\n", group.repository);
            for session in &group.sessions {
                render_session(out, session);
            }
        }
    }

    /// One rolled-up list across the whole window.
    ///
    /// Rendered even when empty, with a sentence saying so: "no decisions were recorded this week"
    /// is a finding about the week, and a section that vanishes when it is empty leaves the reader
    /// to guess whether it was empty or forgotten.
    fn render_rollup(&self, out: &mut String, heading: &str, lines: &[RolledUp], note: &str) {
        let _ = writeln!(out, "\n## {heading}\n");
        if lines.is_empty() {
            let _ = writeln!(
                out,
                "No session in this window recorded any under this heading.",
            );
            return;
        }
        for line in lines {
            let _ = writeln!(out, "- {} — `{}`", line.text, line.repository);
        }
        let _ = writeln!(out, "\n{note}");
    }

    fn render_gaps(&self, out: &mut String) {
        let gaps: &[SkippedNote] = &self.standup.gaps;
        if gaps.is_empty() {
            return;
        }
        out.push_str("\n## Gaps\n\n");
        out.push_str("These archived sessions put nothing in the narrative above:\n\n");
        for note in gaps {
            let _ = writeln!(out, "- {} — {}", note.count, note.reason);
        }
    }

    fn render_footer(&self, out: &mut String) {
        let instrumentation = self.instrumentation;
        out.push_str("\n---\n\n");
        out.push_str(&redaction_line(
            instrumentation.redactor,
            &self.standup.redaction,
        ));
        out.push_str(
            "\n\nEvery session above is cited by the content hash of the `summary.md` it was read \
             from. To read one in full — or the transcript behind it — ask the archive for the \
             artifact and fetch the `content_url` that comes back (the filter takes the bare \
             digest, without the `sha256:` prefix):\n\n",
        );
        let _ = writeln!(
            out,
            "    GET {}/api/v1/artifacts?original_sha256=<hash>\n",
            instrumentation.patwari_url.trim_end_matches('/'),
        );
        let _ = writeln!(
            out,
            "_Instrumentation — sync {} · fold {} · {} sessions · {} read · cache {} hits / {} \
             misses ({} transferred) · redaction {} · patterns {PATTERN_REVISION} · archive {} · \
             cache {}_",
            format::elapsed(instrumentation.sync.elapsed),
            format::elapsed(instrumentation.fold_elapsed),
            self.standup.sessions,
            format::bytes(self.standup.bytes_read),
            instrumentation.sync.cache_hits,
            instrumentation.sync.cache_misses,
            format::bytes(instrumentation.sync.bytes_transferred),
            redaction_counts(&self.standup.redaction),
            instrumentation.patwari_url,
            display_path(&instrumentation.cache_root),
        );
    }
}

/// Why a rolled-up decision list is worth reading as one list.
const DECISIONS_NOTE: &str = "_These are the decisions the sessions themselves recorded, in \
                              reading order, with exact repeats within a repository dropped._";

/// The same, for the open items.
const OPEN_NOTE: &str = "_These are the open items the sessions themselves recorded — what was \
                         still owed when each capture ended, not a live task list._";

/// One session: its title, when it was archived, where, and what it says it did.
fn render_session(out: &mut String, session: &StandupSession) {
    let _ = writeln!(out, "### {}\n", session.title);
    let archived = match session.archived_at {
        Some(at) => stamp(at),
        // Unreachable for a session inside the reported window — placement selects on this very
        // field — but the type admits it, and inventing a timestamp for a session whose date this
        // build could not read is exactly the guess the mirror refuses to make.
        None => "an unreadable time".to_owned(),
    };
    let branch = session
        .branch
        .as_deref()
        .map(|branch| format!(" · branch `{branch}`"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "_Archived {archived} (UTC){branch} · `{}`_\n",
        session.source_hash,
    );
    let _ = writeln!(out, "{}\n", session.goal);
    for item in &session.work_completed {
        let _ = writeln!(out, "- {item}");
    }
    if session.work_completed.is_empty() {
        let _ = writeln!(out, "- (this session's summary lists no completed work)");
    }
    out.push('\n');
}

/// The sentence the document says about its own scrub, stated from the redactor that ran rather
/// than from what was asked for.
fn redaction_line(redactor: Redactor, report: &RedactionReport) -> String {
    let scrub = match (redactor.redacts_secrets(), redactor.filters_profanity()) {
        (true, true) => "scrubbed for secrets and masked for profanity",
        (true, false) => "scrubbed for secrets",
        (false, true) => "**not scrubbed for secrets** (`--no-redact`), masked for profanity",
        (false, false) => "**not scrubbed for secrets** (`--no-redact`)",
    };
    let fired = if report.is_empty() {
        "Nothing matched.".to_owned()
    } else {
        format!(
            "{} replacements were made: {}.",
            report.total(),
            redaction_counts(report),
        )
    };
    format!(
        "Every line of prose above came out of the archive and was {scrub} before it was \
         rendered, against pattern revision {PATTERN_REVISION} \
         (`docs/redaction-patterns-{PATTERN_REVISION}.md`). {fired} What a pattern matched is \
         never recorded, printed, or counted per value — only that it fired.",
    )
}

/// What fired, as `id×count` pairs in pattern order. Counts only, by construction: a
/// [`RedactionReport`] has nothing else to render.
fn redaction_counts(report: &RedactionReport) -> String {
    if report.is_empty() {
        return "none".to_owned();
    }
    report
        .fired()
        .map(|(pattern, count)| format!("{pattern}×{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redaction::PatternId;

    /// The footer has to state the scrub that *ran*, and a run without one has to be visible in
    /// the document rather than only in the shell history that produced it.
    #[test]
    fn the_footer_says_which_passes_ran() {
        let clean = RedactionReport::default();
        let on = redaction_line(Redactor::new(), &clean);
        assert!(on.contains("scrubbed for secrets"));
        assert!(!on.contains("--no-redact"));
        assert!(on.contains(PATTERN_REVISION));
        assert!(on.contains("Nothing matched."));

        let off = redaction_line(Redactor::new().with_secrets(false), &clean);
        assert!(off.contains("**not scrubbed for secrets** (`--no-redact`)"));

        let both = redaction_line(Redactor::new().with_profanity(true), &clean);
        assert!(both.contains("masked for profanity"));
    }

    /// The footer's account of the scrub is counts and pattern ids. There is no shape it could
    /// take that carried a matched value, because the type it renders does not hold one.
    #[test]
    fn the_footer_counts_what_fired_and_names_no_value() {
        let mut report = RedactionReport::default();
        for _ in 0..3 {
            report.absorb(
                &Redactor::new()
                    .scrub("ghp_0123456789012345678901234567890123456")
                    .report,
            );
        }
        let rendered = redaction_counts(&report);
        assert_eq!(rendered, format!("{}×3", PatternId::GithubToken));
        assert!(!rendered.contains("ghp_"));
        assert!(redaction_line(Redactor::new(), &report).contains("3 replacements"));
    }

    #[test]
    fn empty_counts_read_as_none() {
        assert_eq!(redaction_counts(&RedactionReport::default()), "none");
    }

    #[test]
    fn one_repository_is_not_pluralized() {
        assert_eq!(plural(1, "repository", "repositories"), "repository");
        assert_eq!(plural(0, "repository", "repositories"), "repositories");
        assert_eq!(plural(4, "repository", "repositories"), "repositories");
    }

    /// A session with no completed work says so rather than rendering an empty bullet list that
    /// reads as a rendering bug.
    #[test]
    fn a_session_that_completed_nothing_says_so() {
        let mut out = String::new();
        render_session(
            &mut out,
            &StandupSession {
                source_hash: "a".repeat(64),
                archived_at: None,
                branch: None,
                title: "Nothing much".to_owned(),
                goal: "Find out why.".to_owned(),
                work_completed: Vec::new(),
                decisions: Vec::new(),
                open_items: Vec::new(),
            },
        );
        assert!(out.contains("### Nothing much"));
        assert!(out.contains("lists no completed work"));
        assert!(
            out.contains("an unreadable time"),
            "an undated session is not given a date: {out}",
        );
    }
}
