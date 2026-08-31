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

use crate::ask::{Ask, AskHit, Escalation, Query};
use crate::cli::Window;
use crate::format;
use crate::redaction::{PATTERN_REVISION, Redactor};
use crate::report::stamp;
use crate::standup_report::{redaction_counts, redaction_line};
use crate::sync::SyncStats;
use crate::verbatim::MAX_MATCHES_PER_SESSION;

/// What an ask run cost, folded into the footer.
#[derive(Debug, Clone)]
pub struct AskInstrumentation {
    pub sync: SyncStats,
    /// Wall-time of reading, scoring, and ranking the summaries alone, network excluded.
    pub fold_elapsed: Duration,
    /// What the `--verbatim` escalation cost, or `None` when none was asked for — in which case
    /// this document is byte-for-byte the one this lane rendered before the escalation existed.
    pub verbatim: Option<VerbatimStats>,
    /// The redactor the flags asked for, so the footer can say which passes ran.
    pub redactor: Redactor,
    pub patwari_url: String,
    pub cache_root: PathBuf,
}

/// What the `--verbatim` escalation cost and found, in aggregate.
///
/// Its own clause in the footer rather than absorbed into [`SyncStats`], because these requests are
/// not the window mirror's: the mirror moved one `summary.md` per listed session, and this moved
/// one transcript per *shown hit*. A reader deciding whether a `--verbatim` run is worth repeating
/// needs to see the second number on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerbatimStats {
    /// Transcripts fetched, interpreted, and searched.
    pub transcripts_searched: usize,
    /// Shown hits with no transcript this build could search. Each one says why under its own hit;
    /// this is the total, so the two always reconcile against the hits shown.
    pub transcripts_unavailable: usize,
    /// Transcripts that were not already in the cache and had to be downloaded.
    pub transcripts_fetched: u64,
    /// Snapshot documents requested to resolve those transcripts.
    pub snapshots_fetched: u64,
    /// Matching lines found across every searched transcript, before any cap.
    pub matches: usize,
    /// Matching lines actually quoted, after [`crate::verbatim::MAX_MATCHES_PER_SESSION`].
    pub shown: usize,
    /// Decompressed transcript bytes searched.
    pub bytes_searched: u64,
    /// Stored bytes pulled over the wire for them.
    pub bytes_transferred: u64,
    /// Records `munshi-transcript` could not read, across every searched transcript. They carry no
    /// typed text and so were not searched — counted here rather than passed over in silence.
    pub unreadable_records: u64,
    /// Wall-time of the escalation: fetching and searching both, network included.
    pub elapsed: Duration,
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

    /// The one line that states what `--verbatim` did and, just as importantly, what it did not.
    ///
    /// A reader who sees transcript quotations under a ranking will reasonably assume the whole
    /// archive was grepped. It was not: only the shown hits' transcripts were opened, so a session
    /// no summary matched contributed nothing here *and could not have*. Saying it once, above the
    /// blocks, is what keeps the escalation from over-claiming.
    fn render_verbatim_scope(&self, out: &mut String, stats: &VerbatimStats) {
        let shown = self.ask.hits.len();
        let _ = writeln!(
            out,
            "\n`--verbatim` searched the transcripts of the {shown} {} below and nothing else — a \
             session no summary matched was never opened, so it could not contribute a line. {} \
             matching {} found across {} of them; at most {} are quoted per session.",
            plural(shown, "match", "matches"),
            stats.matches,
            plural(stats.matches, "line", "lines"),
            stats.transcripts_searched,
            MAX_MATCHES_PER_SESSION,
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
                 ranked by how much of the query each summary carries — title weighs most, then \
                 repository and tags. This is a search, not a judgement of the work.",
                self.ask.hits.len(),
                self.ask.total_matches,
                self.limit,
            );
        } else {
            let _ = writeln!(
                out,
                "\n**{} {} matched**, ranked by how much of the query each summary carries — title \
                 weighs most, then repository and tags. This is a search, not a judgement of the \
                 work.",
                self.ask.total_matches,
                plural(self.ask.total_matches, "session", "sessions"),
            );
        }
        if let Some(stats) = &self.instrumentation.verbatim {
            self.render_verbatim_scope(out, stats);
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
        // A second line rather than a longer first one, and only when the escalation ran: without
        // `--verbatim` this footer is byte-for-byte the one this lane has always printed.
        if let Some(verbatim) = &instrumentation.verbatim {
            let _ = writeln!(
                out,
                "\n_Verbatim — {} transcripts searched / {} unavailable · {} matches, {} shown · \
                 {} read · {} fetched ({} transferred) · snapshots {} fetched · {} unreadable \
                 records · {}_",
                verbatim.transcripts_searched,
                verbatim.transcripts_unavailable,
                verbatim.matches,
                verbatim.shown,
                format::bytes(verbatim.bytes_searched),
                verbatim.transcripts_fetched,
                format::bytes(verbatim.bytes_transferred),
                verbatim.snapshots_fetched,
                verbatim.unreadable_records,
                format::elapsed(verbatim.elapsed),
            );
        }
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
    if let Some(escalation) = &hit.verbatim {
        render_escalation(out, escalation);
    }
}

/// What the transcript behind one hit said — or, honestly, that there was none to read.
///
/// The count line is the point of the block: a cap that hid the total would let a reader take five
/// quoted lines for the whole of what a session said about the thing they asked about. And an
/// unavailable transcript gets a sentence rather than an empty block, because "I could not look"
/// and "there is nothing there" are different answers to the same question.
fn render_escalation(out: &mut String, escalation: &Escalation) {
    match escalation {
        Escalation::Searched(found) if found.total_matches == 0 => {
            let _ = writeln!(
                out,
                "\n_Verbatim: the transcript carries no line with these words — this session \
                 matched on its summary alone._",
            );
        }
        Escalation::Searched(found) => {
            let _ = writeln!(
                out,
                "\n_Verbatim — {} matching {} in the transcript, showing {}:_\n",
                found.total_matches,
                plural(found.total_matches, "line", "lines"),
                found.matches.len(),
            );
            for hit in &found.matches {
                let at = match hit.at {
                    Some(at) => stamp(at),
                    None => "an unreadable time".to_owned(),
                };
                let _ = writeln!(
                    out,
                    "- `event {}` · {} · {at} (UTC) — {}",
                    hit.locator, hit.surface, hit.excerpt,
                );
            }
        }
        Escalation::Unavailable(reason) => {
            let _ = writeln!(
                out,
                "\n_Verbatim: this session's transcript could not be searched — {reason}._",
            );
        }
    }
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
            searched_index: 0,
            verbatim: None,
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
            verbatim: None,
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

    fn found(total_matches: usize, shown: usize) -> crate::verbatim::SessionVerbatim {
        crate::verbatim::SessionVerbatim {
            matches: (0..shown)
                .map(|index| crate::verbatim::VerbatimMatch {
                    locator: index as u64 + 1,
                    record: index as u64 + 1,
                    line: index as u64 + 1,
                    at: Some(
                        DateTime::parse_from_rfc3339("2026-08-20T10:00:00Z")
                            .unwrap()
                            .with_timezone(&Utc),
                    ),
                    surface: "user",
                    excerpt: format!("line {index} about payments"),
                })
                .collect(),
            total_matches,
            events_searched: 40,
            unreadable_records: 0,
            redaction: Default::default(),
        }
    }

    fn escalated(ask: &mut Ask, escalation: Escalation) -> VerbatimStats {
        ask.hits[0].verbatim = Some(escalation);
        VerbatimStats {
            transcripts_searched: 1,
            matches: 12,
            shown: 5,
            ..VerbatimStats::default()
        }
    }

    /// The block says what it found *and* how much it is not showing, so five quoted lines are
    /// never mistaken for the whole of what a session said.
    #[test]
    fn a_verbatim_block_quotes_its_matches_and_states_the_total() {
        let query = Query::parse("payments api");
        let mut ask = Ask {
            hits: vec![hit("Payments work", 9)],
            total_matches: 1,
            searched: 100,
            ..Ask::default()
        };
        let mut instrumentation = instrumentation();
        instrumentation.verbatim = Some(escalated(&mut ask, Escalation::Searched(found(12, 2))));
        let rendered = report(&ask, &query, &instrumentation).render();
        assert!(rendered.contains("_Verbatim — 12 matching lines in the transcript, showing 2:_"));
        assert!(
            rendered.contains(
                "- `event 1` · user · 2026-08-20T10:00:00Z (UTC) — line 0 about payments"
            )
        );
        // The bound is stated once, above the blocks, in the document's own voice.
        assert!(rendered.contains(
            "`--verbatim` searched the transcripts of the 1 match below and nothing else"
        ));
        assert!(rendered.contains("_Verbatim — 1 transcripts searched / 0 unavailable"));
    }

    /// A transcript with nothing in it and a transcript that could not be read are different
    /// answers, and the block says which — "I could not look" must never render as "there is
    /// nothing there".
    #[test]
    fn an_unavailable_transcript_says_so_rather_than_reading_as_an_empty_one() {
        let query = Query::parse("payments api");
        let mut empty = Ask {
            hits: vec![hit("Payments work", 9)],
            total_matches: 1,
            searched: 100,
            ..Ask::default()
        };
        let mut empty_run = instrumentation();
        empty_run.verbatim = Some(escalated(&mut empty, Escalation::Searched(found(0, 0))));
        let rendered = report(&empty, &query, &empty_run).render();
        assert!(rendered.contains("the transcript carries no line with these words"));
        assert!(rendered.contains("matched on its summary alone"));

        let mut missing = Ask {
            hits: vec![hit("Payments work", 9)],
            total_matches: 1,
            searched: 100,
            ..Ask::default()
        };
        let mut missing_run = instrumentation();
        missing_run.verbatim = Some(escalated(
            &mut missing,
            Escalation::Unavailable(
                "claude-code: snapshot has no `transcript.jsonl` artifact".to_owned(),
            ),
        ));
        let rendered = report(&missing, &query, &missing_run).render();
        assert!(rendered.contains(
            "this session's transcript could not be searched — claude-code: snapshot has no \
             `transcript.jsonl` artifact"
        ));
        assert!(!rendered.contains("no line with these words"));
    }

    /// The control that keeps the escalation additive: with no `--verbatim`, the document is the
    /// one this lane rendered before any of this existed — same hits, same footer, not a word more.
    #[test]
    fn without_the_escalation_the_document_is_untouched() {
        let query = Query::parse("payments api");
        let ask = Ask {
            hits: vec![hit("Payments work", 9)],
            total_matches: 1,
            searched: 100,
            ..Ask::default()
        };
        let instrumentation = instrumentation();
        assert!(instrumentation.verbatim.is_none());
        let rendered = report(&ask, &query, &instrumentation).render();
        assert!(!rendered.contains("Verbatim"), "{rendered}");
        assert!(!rendered.contains("--verbatim"), "{rendered}");
        assert_eq!(
            rendered.matches("_Instrumentation —").count(),
            1,
            "one footer line, as before",
        );
        // And the same fold rendered twice is byte-identical, escalation or not.
        assert_eq!(rendered, report(&ask, &query, &instrumentation).render());
    }

    /// A query that parsed to nothing is answered without touching the archive, and names why.
    #[test]
    fn an_empty_query_is_answered_without_a_fold() {
        let message = no_searchable_terms("the a of");
        assert!(message.contains("no word to search on"));
        assert!(message.contains("“the a of”"));
    }
}
