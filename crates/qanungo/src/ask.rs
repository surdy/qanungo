//! The ask fold: rank a window of archived summaries against a plain-language query.
//!
//! `standup` reads the same `summary.md` records and arranges *all* of them; this lane reads them
//! and keeps only the ones that answer a question, in the order of how well they answer it. There
//! is no model here either (that is the `/ask` contrib skill's job, over this output) and no new
//! search service: the scoring is a total, deterministic function of the query and the summaries
//! the cache already holds, so "have I touched the payments API?" is answered by the words munshi
//! wrote when it captured the work, ranked by a fixed rubric a reader can predict.
//!
//! # The scrub happens here, exactly as it does for standup
//!
//! A matched snippet is archived prose — the first field of a summary that the query hit — and this
//! is qanungo #8's third consumer. Every string that lands in an [`AskHit`] has already been
//! through the [`Redactor`], so the renderer never holds an unscrubbed one, and the counts travel
//! with the hits so the footer can say what fired. Two facts keep this honest:
//!
//! - **Scoring reads the unscrubbed text; only the displayed snippet is scrubbed.** A query term is
//!   the user's own word and is matched against what the summary actually says, so a secret-shaped
//!   token in a summary cannot change a ranking by being replaced first. The scrub applies to the
//!   one snippet a hit renders, on the way into the hit — never to the corpus the score reads.
//! - **The identifier fields (repository, branch) are scrubbed and then clamped**, the order
//!   standup uses for the repository its own summary names ([`crate::standup`]): the scrub is about
//!   what the text contains and the clamp about what a peer may put on a rendering surface, and
//!   running the clamp last is what guarantees the surface safety. (The *other* order,
//!   clamp-then-scrub, is for a label lifted off a listing row; these come out of the parsed
//!   summary, so they take standup's summary-field order.)
//!
//! # No signal, no claim
//!
//! A session whose summary the cache could not read, could not parse, or holds only munshi's
//! placeholder for is *not searchable*, and the fold counts it rather than guessing whether it
//! would have matched. A session that is searchable but matches nothing simply does not appear:
//! absence of a hit is the honest answer to "did I do this", and inventing a weak match to fill the
//! page would turn a "no" into a "maybe".

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use munshi_transcript::{SourceKind, StructuredSummary};

use crate::format;
use crate::redaction::{RedactionReport, Redactor};
use crate::standup::ReadSummary;

/// Longest a matched snippet is rendered before it is cut short.
///
/// A tunable, not a decision. A snippet is a *pointer* into a summary — enough to see why the
/// session matched — not the summary itself, which the `source_hash` beside it fetches in full. The
/// bound is on characters of already-scrubbed text, so cutting one short can only ever hide detail,
/// never reveal any.
pub const MAX_SNIPPET_CHARS: usize = 200;

/// Shortest query word the search keeps.
///
/// A tunable. One- and two-character fragments match almost everything and rank nothing, so they
/// are dropped before scoring rather than allowed to tie every session on the strength of "io".
pub const MIN_TERM_CHARS: usize = 3;

/// Words dropped from a query before scoring, because a summary that happens to contain "the" tells
/// a reader nothing about whether it is the session they are looking for.
///
/// Deliberately short and English-only: this is a search over one developer's own session notes,
/// not a general-purpose index, and a long stop list would start quietly refusing to search for
/// real words. It exists only to stop the handful of function words that appear in nearly every
/// summary from flattening the ranking. A tunable, not a decision.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "was", "were", "did", "does", "have", "has", "had", "this",
    "that", "into", "from", "which", "what", "when", "are", "you", "your", "any", "all",
];

/// One weighted field of a summary the query is scored against.
///
/// The weights are arbitrary-but-tunable, like the rule thresholds elsewhere in the crate, and are
/// ordered so a reader can predict a ranking: a term in the title or the repository name says more
/// about what a session was *about* than the same term buried in a validation command, so those
/// score higher, and the snippet a hit shows is drawn from the highest-weighted field that matched.
struct Field {
    /// What the section is called when the fold reports which fields a hit matched in.
    label: &'static str,
    weight: u32,
    /// Whether a match in this field makes a good *snippet* — an explanatory line worth quoting —
    /// as opposed to a bare keyword or identifier that ranks well but reads as one word out of
    /// context. A tag or a repository name is a strong ranking signal and a poor thing to quote, so
    /// the snippet prefers a prose field that matched and only falls back to a keyword when nothing
    /// prose did.
    quotable: bool,
}

/// The scoring rubric, highest-weighted field first. Iteration order is the tie-break for which
/// field a snippet is drawn from among fields of the same quotability, so it is fixed here rather
/// than derived from the weights.
///
/// The snippet a hit renders is prose, scrubbed like every other body string this crate prints and
/// — like the standup lane's goal and decisions — not clamped: [`crate::format::identifier`]'s
/// clamp is for a value that has to be safe *as an identifier* on a structured surface, and a
/// blockquote line is neither. The repository and branch a hit shows in its metadata line, which
/// *are* rendered as inline identifiers, are clamped separately where [`Ask::fold`] builds them.
const FIELDS: &[Field] = &[
    Field {
        label: "title",
        weight: 5,
        quotable: true,
    },
    Field {
        label: "repository",
        weight: 4,
        quotable: false,
    },
    Field {
        label: "tags",
        weight: 4,
        quotable: false,
    },
    Field {
        label: "goal",
        weight: 3,
        quotable: true,
    },
    Field {
        label: "decisions",
        weight: 2,
        quotable: true,
    },
    Field {
        label: "open items",
        weight: 2,
        quotable: true,
    },
    Field {
        label: "files changed",
        weight: 2,
        quotable: true,
    },
    Field {
        label: "work completed",
        weight: 1,
        quotable: true,
    },
    Field {
        label: "commands",
        weight: 1,
        quotable: true,
    },
    Field {
        label: "branch",
        weight: 1,
        quotable: false,
    },
];

/// A parsed query: the distinct, searchable, lower-cased words a summary is scored against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
}

impl Query {
    /// Splits raw query text into the words the search will actually use: lower-cased, broken on
    /// anything that is not a letter or digit, with stop words and fragments shorter than
    /// [`MIN_TERM_CHARS`] dropped and exact repeats collapsed.
    ///
    /// The result can be empty — a query of nothing but "the a of" has no searchable word in it —
    /// and [`Query::is_empty`] is how the command surface tells that apart from a query that found
    /// nothing, so it can say which happened rather than printing a blank ranking either way.
    pub fn parse(raw: &str) -> Self {
        let mut seen = BTreeSet::new();
        let mut terms = Vec::new();
        for word in raw.split(|character: char| !character.is_alphanumeric()) {
            if word.is_empty() {
                continue;
            }
            let lowered = word.to_lowercase();
            if lowered.chars().count() < MIN_TERM_CHARS || STOP_WORDS.contains(&lowered.as_str()) {
                continue;
            }
            if seen.insert(lowered.clone()) {
                terms.push(lowered);
            }
        }
        Self { terms }
    }

    /// Whether nothing searchable survived parsing.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// The searchable terms, for a document that wants to say what it actually searched for.
    pub fn terms(&self) -> &[String] {
        &self.terms
    }
}

/// One session that matched, as the document renders it: scrubbed strings and a score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskHit {
    /// The `summary.md`'s content hash — cache key and the citation a reader redeems for the whole
    /// summary or the transcript behind it, exactly as the other lanes cite a session.
    pub source_hash: String,
    /// When the archive finished the snapshot this session was listed by. Archive time, the clock
    /// the window (if any) was cut on.
    pub archived_at: Option<DateTime<Utc>>,
    /// The harness that captured the session, off the summary's own record — a closed enum this
    /// build owns, never archive free text, so it needs no scrub.
    pub harness: &'static str,
    /// The repository the summary names, scrubbed and clamped. `None` when it names none.
    pub repository: Option<String>,
    /// The branch the summary names, scrubbed and clamped. `None` when it names none.
    pub branch: Option<String>,
    /// The summary's title, scrubbed.
    pub title: String,
    /// The fields the query matched in, in rubric order, for a reader who wants to know *why* a
    /// session ranked where it did.
    pub matched: Vec<&'static str>,
    /// A scrubbed line of the highest-weighted field that matched — enough to see the hit without
    /// fetching the whole summary.
    pub snippet: String,
    /// The rubric score, carried so the document can show how far apart the matches were.
    pub score: u32,
}

/// Everything one ask document is rendered from.
#[derive(Debug, Clone, Default)]
pub struct Ask {
    /// The matches, best first, capped at the requested limit.
    pub hits: Vec<AskHit>,
    /// How many matched in total, before the limit cut the list — so the document can say "showing
    /// 10 of 34" rather than pretending the tenth was the last.
    pub total_matches: usize,
    /// Sessions whose summary was read and scored, matched or not.
    pub searched: usize,
    /// Sessions listed in the window that could not be searched — no readable summary, an
    /// unparseable one, a placeholder, or a date this build could not place. Counted, never a
    /// silent drop.
    pub unsearchable: usize,
    /// What the scrub fired across every string in the hits above. Counts only.
    pub redaction: RedactionReport,
    /// Decompressed summary bytes read to search them.
    pub bytes_read: u64,
}

impl Ask {
    /// Scores every readable summary against the query, keeps the matches, and returns the top
    /// `limit` of them.
    ///
    /// `unsearchable` is the count of listed sessions that never reached a score — the mirror's
    /// skips plus the window's unplaceable sessions — carried through so the footer can account for
    /// every session the window listed.
    pub fn fold(
        query: &Query,
        read: &[ReadSummary],
        redactor: &Redactor,
        limit: usize,
        unsearchable: usize,
    ) -> Self {
        let mut redaction = RedactionReport::default();
        let mut scrub = |text: &str| {
            let scrubbed = redactor.scrub(text);
            redaction.absorb(&scrubbed.report);
            scrubbed.text
        };

        let mut bytes_read = 0;
        let mut hits: Vec<AskHit> = Vec::new();
        for summary in read {
            bytes_read += summary.bytes_read;
            let Some(scored) = score(query, &summary.archived.summary, &summary.archived.project)
            else {
                continue;
            };
            // Scrub only now, and only what this hit will render — the score above read the
            // summary's own bytes.
            let repository = summary
                .archived
                .project
                .repository
                .as_deref()
                .map(|repository| format::identifier(&scrub(repository)));
            let branch = summary
                .archived
                .project
                .branch
                .as_deref()
                .map(|branch| format::identifier(&scrub(branch)));
            hits.push(AskHit {
                source_hash: summary.source_hash.clone(),
                archived_at: summary.archived_at,
                harness: harness_label(summary.archived.source),
                repository,
                branch,
                title: scrub(&summary.archived.summary.title),
                matched: scored.matched,
                snippet: snippet(&scrub(&scored.snippet)),
                score: scored.score,
            });
        }

        hits.sort_by(most_relevant_first);
        let total_matches = hits.len();
        hits.truncate(limit);

        Self {
            hits,
            total_matches,
            searched: read.len(),
            unsearchable,
            redaction,
            bytes_read,
        }
    }
}

/// One session's score together with the field the snippet should be drawn from.
struct Scored {
    score: u32,
    matched: Vec<&'static str>,
    /// The unscrubbed representative line of the highest-weighted matched field. Scrubbed by the
    /// caller before it is rendered — never stored on a hit as-is.
    snippet: String,
}

/// Scores one summary against the query, or `None` when no term matched any field.
///
/// A field contributes its weight once per query term that appears anywhere in it, so a term in
/// several fields is worth more than one buried in a single field, and a query whose every word
/// appears outscores one that matched on a single word. The snippet is taken from the
/// highest-weighted *quotable* field that matched — falling back to the highest-weighted field of
/// any kind when only a keyword like a tag matched — because [`FIELDS`] iterates weight-descending
/// and the first candidate set is therefore the most telling place the query landed.
fn score(
    query: &Query,
    summary: &StructuredSummary,
    project: &munshi_transcript::ProjectIdentity,
) -> Option<Scored> {
    let mut score = 0;
    let mut matched = Vec::new();
    // Two snippet candidates, both taken from the highest-weighted field of their kind that matched
    // (FIELDS iterates weight-descending, so the first to be set is the best): a quotable one is
    // preferred, and the any-kind one is the fallback for a hit that matched only a keyword like a
    // tag or the repository name — where quoting the bare word is still better than quoting nothing.
    let mut quotable_snippet: Option<String> = None;
    let mut fallback_snippet: Option<String> = None;

    for field in FIELDS {
        let entries = field_entries(field.label, summary, project);
        // Lower-case each entry once, here, rather than once per term inside the match test: the
        // terms are already lower-cased by `Query::parse`, so a single fold of the field's own text
        // is all the case-insensitivity costs. `matches` runs parallel to `entries`.
        let matches: Vec<String> = entries.iter().map(|entry| entry.to_lowercase()).collect();
        let mut field_hit = false;
        for term in query.terms() {
            if matches.iter().any(|entry| entry.contains(term.as_str())) {
                score += field.weight;
                field_hit = true;
            }
        }
        if field_hit {
            matched.push(field.label);
            // The first entry any term landed in — found on the lower-cased copies and returned as
            // the original text — so the line shown is one the reader's own words appear in.
            let hit_entry = || {
                entries
                    .iter()
                    .zip(&matches)
                    .find(|(_, lowered)| {
                        query
                            .terms()
                            .iter()
                            .any(|term| lowered.contains(term.as_str()))
                    })
                    .map(|(entry, _)| (*entry).to_owned())
            };
            if fallback_snippet.is_none() {
                fallback_snippet = hit_entry();
            }
            if field.quotable && quotable_snippet.is_none() {
                quotable_snippet = hit_entry();
            }
        }
    }

    quotable_snippet.or(fallback_snippet).map(|snippet| Scored {
        score,
        matched,
        snippet,
    })
}

/// The text entries of one named field, in the order a snippet would prefer them.
fn field_entries<'a>(
    label: &str,
    summary: &'a StructuredSummary,
    project: &'a munshi_transcript::ProjectIdentity,
) -> Vec<&'a str> {
    let single = |value: &'a str| {
        if value.is_empty() {
            Vec::new()
        } else {
            vec![value]
        }
    };
    let list = |items: &'a [String]| items.iter().map(String::as_str).collect();
    match label {
        "title" => single(&summary.title),
        "goal" => single(&summary.goal),
        "tags" => list(&summary.tags),
        "decisions" => list(&summary.decisions),
        "open items" => list(&summary.open_items),
        "files changed" => list(&summary.files_changed),
        "work completed" => list(&summary.work_completed),
        "commands" => list(&summary.commands_and_validation),
        "repository" => project
            .repository
            .as_deref()
            .map(single)
            .unwrap_or_default(),
        "branch" => project.branch.as_deref().map(single).unwrap_or_default(),
        // The FIELDS table is the only caller and every label above is one of its entries; an
        // unrecognized label is a table edit that forgot this match, caught by the exhaustiveness
        // test rather than silently scoring nothing.
        other => unreachable!("no field entries for `{other}`"),
    }
}

/// Cuts an already-scrubbed snippet to [`MAX_SNIPPET_CHARS`], on a character boundary, marking a
/// cut with an ellipsis. Collapses internal whitespace so a multi-line summary entry renders as one
/// readable line.
fn snippet(scrubbed: &str) -> String {
    let collapsed = scrubbed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_SNIPPET_CHARS {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(MAX_SNIPPET_CHARS).collect();
    format!("{cut}…")
}

/// Best match first, with a total order so the same query over the same archive ranks the same way
/// every run: score descending, then newest archived, then the content hash to break a tie no clock
/// can. A session the archive dated unreadably sorts after any dated one at the same score.
fn most_relevant_first(left: &AskHit, right: &AskHit) -> std::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| right.archived_at.cmp(&left.archived_at))
        .then_with(|| left.source_hash.cmp(&right.source_hash))
}

/// The harness that captured a session, named from the summary's own closed-enum source rather than
/// from the archive's free-text `source_agent`, so this string is always one of exactly three and
/// never needs a scrub.
fn harness_label(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Copilot => "copilot",
        SourceKind::ClaudeCode => "claude-code",
        SourceKind::Codex => "codex",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munshi_transcript::{ArchivedMarkdown, ProjectIdentity, ProjectOrigin};

    /// A summary with only the fields the search reads set, everything else empty.
    #[derive(Default)]
    struct Fields {
        title: &'static str,
        goal: &'static str,
        repository: Option<&'static str>,
        branch: Option<&'static str>,
        tags: &'static [&'static str],
        decisions: &'static [&'static str],
        files_changed: &'static [&'static str],
        work_completed: &'static [&'static str],
    }

    fn structured(fields: &Fields) -> StructuredSummary {
        let list = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        StructuredSummary {
            title: fields.title.to_owned(),
            goal: fields.goal.to_owned(),
            work_completed: list(fields.work_completed),
            decisions: list(fields.decisions),
            files_changed: list(fields.files_changed),
            commands_and_validation: Vec::new(),
            open_items: Vec::new(),
            tags: list(fields.tags),
        }
    }

    fn project(fields: &Fields) -> ProjectIdentity {
        ProjectIdentity {
            identity: "id".to_owned(),
            component: "component".to_owned(),
            project: "project".to_owned(),
            repository: fields.repository.map(str::to_owned),
            branch: fields.branch.map(str::to_owned),
            origin: ProjectOrigin::Live,
        }
    }

    fn read_summary(hash: &str, at: &str, source: SourceKind, fields: Fields) -> ReadSummary {
        let archived = ArchivedMarkdown {
            schema_version: 1,
            source,
            session_id: "session".to_owned(),
            project: project(&fields),
            summary_revision: 1,
            completion_reason: "done".to_owned(),
            cursor_fallback_reason: None,
            cursor: None,
            source_cursor: 0,
            source_hash: hash.to_owned(),
            started_at: None,
            updated_at: None,
            summary_placeholder: false,
            artifact_set_version: Some(2),
            transcript_sha256: None,
            extracted_outputs: Vec::new(),
            summary: structured(&fields),
        };
        ReadSummary {
            source_hash: hash.to_owned(),
            archived_at: Some(
                DateTime::parse_from_rfc3339(at)
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            bytes_read: 1_000,
            archived,
        }
    }

    /// Parsing lower-cases, breaks on non-alphanumerics, and drops fragments and stop words while
    /// keeping real words once each.
    #[test]
    fn a_query_keeps_only_its_searchable_words() {
        let query = Query::parse("The Payments API, payments!");
        assert_eq!(query.terms(), &["payments".to_owned(), "api".to_owned()]);
        assert!(!query.is_empty());

        assert!(Query::parse("the a of to").is_empty());
        assert!(Query::parse("").is_empty());
        // "api" is three characters, the floor, so it survives; "io" does not.
        assert_eq!(Query::parse("io api").terms(), &["api".to_owned()]);
    }

    /// A term in a heavier field outscores the same term in a lighter one, and a query whose every
    /// word appears outscores one that matched a single word — the two properties a reader relies on
    /// when they read the ranking top-down.
    #[test]
    fn score_rewards_field_weight_and_query_coverage() {
        let title_hit = read_summary(
            &"a".repeat(64),
            "2026-08-20T10:00:00Z",
            SourceKind::ClaudeCode,
            Fields {
                title: "payments api work",
                ..Default::default()
            },
        );
        let commands_hit = read_summary(
            &"b".repeat(64),
            "2026-08-20T10:00:00Z",
            SourceKind::ClaudeCode,
            Fields {
                work_completed: &["touched the payments api"],
                ..Default::default()
            },
        );
        let query = Query::parse("payments api");
        let title = score(
            &query,
            &title_hit.archived.summary,
            &title_hit.archived.project,
        )
        .unwrap();
        let commands = score(
            &query,
            &commands_hit.archived.summary,
            &commands_hit.archived.project,
        )
        .unwrap();
        // Two terms in the title (weight 5) each, versus two in work-completed (weight 1).
        assert_eq!(title.score, 10);
        assert_eq!(commands.score, 2);
        assert!(title.score > commands.score);

        // Coverage: one word matched scores less than both, in the same field.
        let one = Query::parse("payments");
        let two = Query::parse("payments api");
        let s_one = score(
            &one,
            &title_hit.archived.summary,
            &title_hit.archived.project,
        )
        .unwrap();
        let s_two = score(
            &two,
            &title_hit.archived.summary,
            &title_hit.archived.project,
        )
        .unwrap();
        assert!(s_two.score > s_one.score);
    }

    /// A term that lands in both a tag and a prose field draws its snippet from the prose, not the
    /// bare keyword — a snippet reading only "redaction" tells a reader nothing the score did not.
    /// A term that matched *only* a keyword field still gets that keyword as a last resort.
    #[test]
    fn a_snippet_prefers_prose_over_a_bare_keyword() {
        let both = read_summary(
            &"a".repeat(64),
            "2026-08-20T10:00:00Z",
            SourceKind::ClaudeCode,
            Fields {
                title: "logging work",
                goal: "harden the redaction layer before the dashboard renders verbatim",
                tags: &["redaction"],
                ..Default::default()
            },
        );
        let query = Query::parse("redaction");
        let scored = score(&query, &both.archived.summary, &both.archived.project).unwrap();
        assert!(scored.matched.contains(&"tags"));
        assert!(scored.matched.contains(&"goal"));
        assert!(
            scored.snippet.starts_with("harden the redaction layer"),
            "{}",
            scored.snippet
        );

        // Keyword-only: the tag is all there is, so it is the snippet.
        let keyword_only = read_summary(
            &"b".repeat(64),
            "2026-08-20T10:00:00Z",
            SourceKind::ClaudeCode,
            Fields {
                title: "logging work",
                tags: &["redaction"],
                ..Default::default()
            },
        );
        let scored = score(
            &query,
            &keyword_only.archived.summary,
            &keyword_only.archived.project,
        )
        .unwrap();
        assert_eq!(scored.matched, vec!["tags"]);
        assert_eq!(scored.snippet, "redaction");
    }

    /// A summary no term touches is not a weak match, it is not a match at all.
    #[test]
    fn a_summary_that_matches_nothing_does_not_score() {
        let miss = read_summary(
            &"c".repeat(64),
            "2026-08-20T10:00:00Z",
            SourceKind::ClaudeCode,
            Fields {
                title: "logging refactor",
                ..Default::default()
            },
        );
        let query = Query::parse("payments api");
        assert!(score(&query, &miss.archived.summary, &miss.archived.project).is_none());
    }

    /// The fold reads the summary's real bytes to score it, then scrubs every string it renders. A
    /// secret sitting in the very field that becomes the snippet is replaced there, the search term
    /// beside it still found the session, and the scrub is counted — while a secret in a field that
    /// is scored but never rendered is neither leaked nor, correctly, counted.
    #[test]
    fn scoring_reads_the_real_text_and_every_rendered_string_is_scrubbed() {
        let secret = "ghp_0123456789012345678901234567890123456";
        let leaky = read_summary(
            &"d".repeat(64),
            "2026-08-20T10:00:00Z",
            SourceKind::ClaudeCode,
            Fields {
                // The secret is in the title, which is always rendered and always the snippet when
                // it matches, so the scrub has to reach it.
                title: "rotate ghp_0123456789012345678901234567890123456 out of CI",
                // A second secret in a lower field that will NOT become the snippet: scored,
                // never rendered, so never scrubbed and never leaked.
                decisions: &["also rotate ghp_9999999999999999999999999999999999999 someday"],
                ..Default::default()
            },
        );
        let query = Query::parse("rotate");
        let ask = Ask::fold(
            &query,
            std::slice::from_ref(&leaky),
            &Redactor::new(),
            10,
            0,
        );
        assert_eq!(ask.hits.len(), 1);
        let hit = &ask.hits[0];
        assert!(hit.matched.contains(&"title"));
        assert!(hit.matched.contains(&"decisions"));
        // Every rendered string is clean of the secret, and the title's scrub was counted.
        assert!(
            !hit.title.contains(secret),
            "title leaked a secret: {}",
            hit.title
        );
        assert!(
            !hit.snippet.contains(secret),
            "snippet leaked a secret: {}",
            hit.snippet
        );
        assert!(
            !ask.redaction.is_empty(),
            "the rendered secret was not counted"
        );
        // The second secret rode in on a scored-but-unrendered field: it is nowhere in the hit.
        let rendered = format!("{hit:?}");
        assert!(
            !rendered.contains("ghp_9999"),
            "an unrendered field leaked: {rendered}"
        );
    }

    /// Ranking is total and the limit caps the output while `total_matches` still counts them all,
    /// so a truncated list can say how much it hid.
    #[test]
    fn ranking_is_total_and_the_limit_caps_without_losing_the_count() {
        let summaries: Vec<ReadSummary> = (0..5)
            .map(|index| {
                // Same title so all five match equally; the hash and date break the tie totally.
                read_summary(
                    &format!("{index}").repeat(64),
                    "2026-08-20T10:00:00Z",
                    SourceKind::ClaudeCode,
                    Fields {
                        title: "payments api",
                        ..Default::default()
                    },
                )
            })
            .collect();
        let query = Query::parse("payments api");
        let ask = Ask::fold(&query, &summaries, &Redactor::new(), 2, 3);
        assert_eq!(ask.total_matches, 5);
        assert_eq!(ask.hits.len(), 2, "the limit caps the rendered hits");
        assert_eq!(ask.searched, 5);
        assert_eq!(ask.unsearchable, 3);
        // Equal scores tie-break on the content hash ascending, deterministically.
        assert!(ask.hits[0].source_hash < ask.hits[1].source_hash);

        // The same fold twice is byte-for-byte the same ordering.
        let again = Ask::fold(&query, &summaries, &Redactor::new(), 2, 3);
        assert_eq!(ask.hits, again.hits);
    }

    /// A snippet is collapsed to one line and cut to the ceiling, so a long multi-line summary entry
    /// cannot turn a search result into a wall of text.
    #[test]
    fn a_snippet_is_one_line_and_bounded() {
        let long = "word ".repeat(200);
        let cut = snippet(&long);
        assert!(
            cut.chars().count() <= MAX_SNIPPET_CHARS + 1,
            "including the ellipsis"
        );
        assert!(cut.ends_with('…'));
        assert!(!cut.contains('\n'));
        assert_eq!(snippet("a\n  b\tc"), "a b c");
    }

    /// The field-entry table answers every label the rubric names — a table edit that added a
    /// weight without an entry accessor would panic here rather than silently scoring nothing.
    #[test]
    fn every_rubric_field_has_an_entry_accessor() {
        let fields = Fields {
            title: "t",
            goal: "g",
            repository: Some("r"),
            branch: Some("b"),
            tags: &["tag"],
            decisions: &["d"],
            files_changed: &["f"],
            work_completed: &["w"],
        };
        let summary = structured(&fields);
        let identity = project(&fields);
        // Every label in the rubric must be handled by the accessor: an unrecognized one hits the
        // `unreachable!` and panics, so iterating all of them without panic is the exhaustiveness
        // proof. The fixture leaves open-items and commands empty on purpose, so a field can
        // legitimately yield nothing — what must not happen is a label the accessor forgot.
        for field in FIELDS {
            let _ = field_entries(field.label, &summary, &identity);
        }
        // The populated identifier and text fields do yield their entries.
        assert_eq!(field_entries("title", &summary, &identity), vec!["t"]);
        assert_eq!(field_entries("repository", &summary, &identity), vec!["r"]);
        assert_eq!(field_entries("branch", &summary, &identity), vec!["b"]);
        assert!(field_entries("open items", &summary, &identity).is_empty());
    }
}
