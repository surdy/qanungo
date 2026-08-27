//! Markdown rendering for the ask lane.
//!
//! Like the standup document, and unlike `report` and `cost`, this one renders prose that came out
//! of the archive — a matched snippet, a title, a repository name — so it holds its redaction line
//! the same way: **every string below was scrubbed on the way into [`Ask`] by [`crate::ask`]**,
//! before it reached this module, and there is no unscrubbed copy in scope to render by mistake.
//! The footer sentence is literally the standup lane's, shared rather than reworded, because both
//! documents make the same promise about the same scrub.
//!
//! What this lane adds over standup is that it renders a *ranking*, so the document says out loud
//! what it ranked on: which words it actually searched for after dropping fragments and stop words,
//! how many sessions it looked at, how many it could not, and — per hit — the score and the fields
//! the query landed in. A search that hides its own rubric invites a reader to trust an order they
//! cannot check; this one shows its work.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::ask::{Ask, AskHit, Query};
use crate::cli::Window;
use crate::format;
use crate::redaction::{PATTERN_REVISION, Redactor};
use crate::report::stamp;
use crate::standup_report::{redaction_counts, redaction_line};
use crate::sync::SyncStats;

/// What an ask run cost, folded into the footer.
#[derive(Debug, Clone)]
pub struct AskInstrumentation {
    pub sync: SyncStats,
    /// Wall-time of reading, scoring, and ranking the summaries alone, network excluded.
    pub fold_elapsed: Duration,
    /// The redactor the flags asked for, so the footer can say which passes ran.
    pub redactor: Redactor,
    pub patwari_url: String,
    pub cache_root: PathBuf,
}

/// Everything one ask document is rendered from.
pub struct AskReport<'a> {
    /// The query exactly as it was typed, echoed in the heading so a reader sees their own words.
    pub raw_query: &'a str,
    /// The parsed query, so the document can say which words it actually searched for.
    pub query: &'a Query,
    /// The window that narrowed the search, or `None` for all of history.
    pub window: Option<&'a Window>,
    /// The requested cap, so a truncated ranking can say it was truncated.
    pub limit: usize,
    pub generated_at: DateTime<Utc>,
    pub ask: &'a Ask,
    pub instrumentation: &'a AskInstrumentation,
}

impl AskReport<'_> {
    /// Renders the ranked search as Markdown.
    ///
    /// Deterministic in full: the same query over the same archive with the same flags produces
    /// byte-identical output, because [`Ask`]'s ordering is total and nothing here reads a clock
    /// except the two timestamps it prints.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# History search — “{}”\n", self.raw_query.trim());
        self.render_scope(&mut out);
        self.render_hits(&mut out);
        self.render_footer(&mut out);
        out
    }

    fn render_scope(&self, out: &mut String) {
        let reach = match self.window {
            Some(window) => format!(
                "in the last {window} (archived since {} UTC)",
                stamp(window.opens_at(self.generated_at)),
            ),
            None => "across all of the archive's history".to_owned(),
        };
        let _ = writeln!(
            out,
            "Searched {} {reach}, read at {} (UTC).",
            self.ask.searched,
            stamp(self.generated_at),
        );
        if self.ask.unsearchable > 0 {
            let _ = writeln!(
                out,
                "\n{} listed {} carried no summary this build could read and so could not be \
                 searched — counted here rather than silently dropped.",
                self.ask.unsearchable,
                plural(self.ask.unsearchable, "session", "sessions"),
            );
        }
        let _ = writeln!(
            out,
            "\nSearched for {}. Very short and very common words are dropped before ranking.",
            quoted_terms(self.query),
        );
    }

    fn render_hits(&self, out: &mut String) {
        if self.ask.hits.is_empty() {
            let _ = writeln!(
                out,
                "\nNo session's summary matched {}. That is the archive's answer, not a truncation \
                 — nothing was ranked and hidden.",
                quoted_terms(self.query),
            );
            return;
        }
        if self.ask.total_matches > self.ask.hits.len() {
            let _ = writeln!(
                out,
                "\n**Showing the {} best of {} matches** (raise `--limit` past {} to see more), \
                 ranked by how much of the query each summary carries — title and repository weigh \
                 most. This is a search, not a judgement of the work.",
                self.ask.hits.len(),
                self.ask.total_matches,
                self.limit,
            );
        } else {
            let _ = writeln!(
                out,
                "\n**{} {} matched**, ranked by how much of the query each summary carries — title \
                 and repository weigh most. This is a search, not a judgement of the work.",
                self.ask.total_matches,
                plural(self.ask.total_matches, "session", "sessions"),
            );
        }
        for (rank, hit) in self.ask.hits.iter().enumerate() {
            render_hit(out, rank + 1, hit);
        }
    }

    fn render_footer(&self, out: &mut String) {
        let instrumentation = self.instrumentation;
        out.push_str("\n---\n\n");
        out.push_str(&redaction_line(
            instrumentation.redactor,
            &self.ask.redaction,
        ));
        out.push_str(
            "\n\nEach match is cited by the content hash of the `summary.md` it was read from. To \
             read one in full — or the transcript behind it — ask the archive for the artifact and \
             fetch the `content_url` that comes back (the filter takes the bare digest, without the \
             `sha256:` prefix):\n\n",
        );
        let _ = writeln!(
            out,
            "    GET {}/api/v1/artifacts?original_sha256=<hash>\n",
            instrumentation.patwari_url.trim_end_matches('/'),
        );
        let _ = writeln!(
            out,
            "_Instrumentation — sync {} · fold {} · {} searched · {} read · cache {} hits / {} \
             misses ({} transferred) · snapshots {} indexed / {} fetched · redaction {} · \
             patterns {PATTERN_REVISION} · archive {} · cache {}_",
            format::elapsed(instrumentation.sync.elapsed),
            format::elapsed(instrumentation.fold_elapsed),
            self.ask.searched,
            format::bytes(self.ask.bytes_read),
            instrumentation.sync.cache_hits,
            instrumentation.sync.cache_misses,
            format::bytes(instrumentation.sync.bytes_transferred),
            instrumentation.sync.snapshots_indexed,
            instrumentation.sync.snapshots_fetched,
            redaction_counts(&self.ask.redaction),
            instrumentation.patwari_url,
            display_path(&instrumentation.cache_root),
        );
    }
}

/// The message a run with no searchable query prints, without touching the archive at all.
///
/// A query of nothing but fragments and stop words ("the a of") has no word to search on, and the
/// honest thing is to say so before a single request is made — not to mirror the whole archive and
/// then rank nothing. This is why the command surface parses the query first and, when it is empty,
/// renders this instead of a fold.
pub fn no_searchable_terms(raw_query: &str) -> String {
    format!(
        "# History search — “{}”\n\nThis query has no word to search on: everything in it is either \
         shorter than {} characters or a very common word the search drops. Try a more specific \
         term — a repository name, an API, a file, a feature.\n",
        raw_query.trim(),
        crate::ask::MIN_TERM_CHARS,
    )
}

/// One match: its rank, title, where and when it happened, why it ranked, and the line it hit on.
fn render_hit(out: &mut String, rank: usize, hit: &AskHit) {
    let _ = writeln!(out, "\n### {rank}. {}\n", hit.title);
    let archived = match hit.archived_at {
        Some(at) => stamp(at),
        // Unreachable for a dated session, but the type admits an undated one and inventing a date
        // for it would be exactly the guess the mirror refuses to make.
        None => "an unreadable time".to_owned(),
    };
    let repository = hit
        .repository
        .as_deref()
        .unwrap_or("no repository recorded");
    let branch = hit
        .branch
        .as_deref()
        .map(|branch| format!(" · branch `{branch}`"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "_{} · `{repository}`{branch} · archived {archived} (UTC) · score {} · `{}`_\n",
        hit.harness, hit.score, hit.source_hash,
    );
    let _ = writeln!(out, "> {}\n", hit.snippet);
    let _ = writeln!(out, "Matched in {}.", hit.matched.join(", "));
}

/// The searched terms as a readable list — `"payments", "api"` — or a plain phrase when there is
/// only one. Purely presentational; the terms are the user's own words, already lower-cased.
fn quoted_terms(query: &Query) -> String {
    query
        .terms()
        .iter()
        .map(|term| format!("“{term}”"))
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
    use crate::ask::AskHit;

    fn hit(title: &str, score: u32) -> AskHit {
        AskHit {
            source_hash: "a".repeat(64),
            archived_at: Some(
                DateTime::parse_from_rfc3339("2026-08-20T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            harness: "claude-code",
            repository: Some("payments".to_owned()),
            branch: None,
            title: title.to_owned(),
            matched: vec!["title", "repository"],
            snippet: "the payments API refactor".to_owned(),
            score,
        }
    }

    fn report<'a>(
        ask: &'a Ask,
        query: &'a Query,
        instrumentation: &'a AskInstrumentation,
    ) -> AskReport<'a> {
        AskReport {
            raw_query: "payments api",
            query,
            window: None,
            limit: 10,
            generated_at: DateTime::parse_from_rfc3339("2026-08-26T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ask,
            instrumentation,
        }
    }

    fn instrumentation() -> AskInstrumentation {
        AskInstrumentation {
            sync: SyncStats::default(),
            fold_elapsed: Duration::from_millis(5),
            redactor: Redactor::new(),
            patwari_url: "https://patwari.example".to_owned(),
            cache_root: PathBuf::from("/cache"),
        }
    }

    /// A search that matched nothing says so as the archive's answer, and does not read as a
    /// truncated list — the difference matters to somebody asking "have I ever done this".
    #[test]
    fn no_matches_is_stated_as_an_answer_not_a_truncation() {
        let query = Query::parse("payments api");
        let ask = Ask {
            searched: 40,
            ..Ask::default()
        };
        let instrumentation = instrumentation();
        let rendered = report(&ask, &query, &instrumentation).render();
        assert!(rendered.contains("No session's summary matched"));
        assert!(rendered.contains("not a truncation"));
        assert!(rendered.contains("Searched 40"));
        assert!(rendered.contains("across all of the archive's history"));
    }

    /// When more matched than the limit shows, the document says so rather than pretending the last
    /// line printed was the last match found.
    #[test]
    fn a_truncated_ranking_says_it_was_truncated() {
        let query = Query::parse("payments api");
        let ask = Ask {
            hits: vec![hit("Payments work", 9)],
            total_matches: 34,
            searched: 100,
            ..Ask::default()
        };
        let instrumentation = instrumentation();
        let rendered = report(&ask, &query, &instrumentation).render();
        assert!(rendered.contains("Showing the 1 best of 34 matches"));
        assert!(rendered.contains("### 1. Payments work"));
        assert!(rendered.contains("Matched in title, repository."));
        assert!(rendered.contains("score 9"));
    }

    /// A query that parsed to nothing is answered without touching the archive, and names why.
    #[test]
    fn an_empty_query_is_answered_without_a_fold() {
        let message = no_searchable_terms("the a of");
        assert!(message.contains("no word to search on"));
        assert!(message.contains("“the a of”"));
    }
}
