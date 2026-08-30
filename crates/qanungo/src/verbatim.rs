//! The verbatim escalation: read the transcripts of the sessions a summary search already found.
//!
//! # What this is, and what it deliberately is not
//!
//! [`crate::ask`] ranks each session's `summary.md` — decision 12 chose that substrate precisely so
//! the lane would not have to mirror every transcript in the archive — and a summary is a
//! *curated* record: it says what the session was about, not everything that was said in it. The
//! funnel's next stage is this module: for the hits `ask` is going to show, and **only** for those,
//! qanungo fetches the transcript behind each one and looks for the same query terms in what the
//! session actually said.
//!
//! That bound is the design, not an optimisation. An unbounded `--verbatim` would be an
//! archive-wide full-text search, which means mirroring every transcript in the archive on a run
//! that might match nothing — the exact cost decision 12 refused. Escalation instead means "dig
//! into the sessions the summaries found", and it inherits the summary lane's coverage boundary as
//! a consequence: a fact that appears in no summary is a session `ask` never ranks, so it is a
//! transcript this module never opens. The document says so out loud rather than letting a reader
//! mistake a bounded dig for a whole-archive answer.
//!
//! # Parsed content, never raw bytes
//!
//! The search runs over `munshi-transcript`'s typed records, exactly as every other fold in this
//! crate does — never over the JSONL bytes. Grepping the file would match JSON keys, base64 blobs,
//! uuids, and escaped punctuation, and would report a "match" for a query that appears nowhere a
//! human said anything. Five text surfaces are searched, and the choice mirrors
//! [`crate::evidence`]'s excerpt rule:
//!
//! - **`user`** and **`assistant`** — [`Event::User`] and [`Event::Assistant`] text, the complete
//!   authored content of the conversation. This is what somebody asking "when did I decide X" is
//!   actually asking about.
//! - **`command`**, **`error`**, **`output`** — a tool event's own typed command string (munshi #77)
//!   and its error and output text: the same three fields an evidence excerpt renders.
//!
//! And the raw `input` / `arguments` blobs are **not** searched, for the reason `evidence` does not
//! excerpt them: they are the whole tool payload — file contents, patches, prompts — so a query
//! term inside one says the term appeared in a file this session read, not that the session was
//! about it. Neither are the identifier fields (tool name, call id, event kind): those are schema
//! metadata rather than anything anybody wrote. Records the parser sets aside (`Ignored`, model
//! reasoning among them) and records it cannot read at all carry no typed text to search; the
//! unreadable ones are counted ([`SessionVerbatim::unreadable_records`]) rather than passed over in
//! silence.
//!
//! # The scrub happens here
//!
//! An excerpt is transcript verbatim — the most exposed surface this crate has — so it is scrubbed
//! on the way into [`VerbatimMatch`], before the collapse and the clamp, and a caller therefore
//! never holds an unscrubbed string. The order is [`crate::evidence`]'s and [`crate::ask`]'s:
//! **scrub, then collapse, then clip**. Clipping first could cut a credential in half and render
//! the surviving head.
//!
//! What decides a *match* is the line as the transcript holds it, unscrubbed — the same rule the
//! summary rubric follows. A secret-shaped token cannot make a session stop matching by being
//! replaced first, so the count is honest and the excerpt beside it is safe.

use std::io::BufRead;

use chrono::{DateTime, Utc};
use munshi_transcript::{
    Classification, Event, Record, Source, ToolEvent, TranscriptStream, UnsupportedVersion,
};

use crate::ask::{MAX_SNIPPET_CHARS, Query, snippet};
use crate::redaction::{RedactionReport, Redactor};

/// Matching lines one session's transcript contributes to the document. **A tunable, not a
/// decision.**
///
/// Five is enough to see *how* a session talked about the thing asked about and few enough that a
/// ten-hit ranking stays a page rather than a transcript dump. A session that said the word two
/// hundred times does not need two hundred excerpts to make its point; what it needs is the total,
/// which [`SessionVerbatim::total_matches`] carries beside the shown ones.
pub const MAX_MATCHES_PER_SESSION: usize = 5;

/// Characters of context kept *before* the matched term when a line is too long to quote whole.
///
/// A tunable. The window is otherwise anchored on the match rather than on the start of the line,
/// because a quote of the first 200 characters of a 4,000-character build log would not contain the
/// word the reader searched for.
const EXCERPT_LEAD_CHARS: usize = MAX_SNIPPET_CHARS / 4;

/// Marks an excerpt cut short at either end. The same character [`crate::ask`] cuts a snippet with.
const EXCERPT_TRUNCATED: char = '…';

/// One matching line of one transcript, as the document renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerbatimMatch {
    /// 1-based ordinal among the transcript's **events**, in file order.
    ///
    /// The locator idiom [`crate::evidence`] established, in this lane's own space: that one counts
    /// tool events, because a rule anchors tool events; this one counts every typed event, because
    /// a conversation search matches user and assistant text too. The two spaces are therefore
    /// *not* interchangeable, and nothing redeems a locator minted here against that route — this
    /// one is a coordinate a reader takes to their own copy of the transcript, printed beside the
    /// record and line numbers that make it findable by hand.
    pub locator: u64,
    /// 1-based record ordinal (non-empty lines), as `munshi-transcript` numbers them.
    pub record: u64,
    /// 1-based physical line number.
    pub line: u64,
    /// The record's own timestamp, when it had one.
    pub at: Option<DateTime<Utc>>,
    /// Which text surface matched: `user`, `assistant`, `command`, `error`, or `output`.
    pub surface: &'static str,
    /// The matching line, scrubbed and then cut to a readable length. Never unscrubbed, never
    /// multi-line.
    pub excerpt: String,
}

/// What searching one session's transcript found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionVerbatim {
    /// The matches to render, earliest first, at most [`MAX_MATCHES_PER_SESSION`] of them.
    pub matches: Vec<VerbatimMatch>,
    /// Every matching line, before the cap took the first few — so a bounded block can say how
    /// much it is not showing.
    pub total_matches: usize,
    /// Events the search walked. The size of this transcript's locator space, and the honest
    /// denominator for the count above.
    pub events_searched: u64,
    /// Records `munshi-transcript` could not read at all. They carry no typed text and so were not
    /// searched; counted rather than passed over in silence.
    pub unreadable_records: u64,
    /// What the scrub fired across the excerpts above. Counts only, per qanungo #8.
    pub redaction: RedactionReport,
}

/// Searches one transcript for the query's terms, streaming it rather than reading it whole.
///
/// One pass, one record at a time, nothing buffered but the bounded match list: a 200 MB
/// transcript costs the read and not the memory. Matching is per **line** of a text surface, so a
/// match is a line a person could point at, and case-insensitive against the already-lower-cased
/// [`Query`] terms — a line matches when it contains *any* of them, which is the same OR the
/// summary rubric scores with.
///
/// The result is deterministic for a given transcript and query: matches are kept in file order,
/// within a record in event order, within an event in surface order, and within a surface in line
/// order.
///
/// # Errors
///
/// Returns an error when `artifact_set_version` names a contract this build cannot read — the same
/// refusal [`crate::metrics::fold_transcript`] and [`crate::evidence::extract`] make.
pub fn search(
    source: Source,
    artifact_set_version: u16,
    reader: impl BufRead,
    query: &Query,
    redactor: &Redactor,
) -> Result<SessionVerbatim, UnsupportedVersion> {
    let stream = TranscriptStream::new(source, artifact_set_version, reader)?;
    let mut found = SessionVerbatim::default();
    let mut locator = 0_u64;
    for item in stream {
        let Ok(record) = item else {
            found.unreadable_records += 1;
            continue;
        };
        let Classification::Content { events } = &record.classification else {
            continue;
        };
        for event in events {
            locator += 1;
            for (surface, text) in surfaces(event) {
                for line in text.lines() {
                    // The unscrubbed line decides the match; only the excerpt below is scrubbed.
                    if !matches(query, line) {
                        continue;
                    }
                    found.total_matches += 1;
                    if found.matches.len() < MAX_MATCHES_PER_SESSION {
                        let hit = matched(
                            &record,
                            locator,
                            surface,
                            line,
                            query,
                            redactor,
                            &mut found.redaction,
                        );
                        found.matches.push(hit);
                    }
                }
            }
        }
    }
    found.events_searched = locator;
    Ok(found)
}

/// Whether a line carries any of the query's terms, case-insensitively.
///
/// The line is lower-cased once here rather than once per term, the way the summary rubric folds a
/// field: the terms arrive lower-cased from [`Query::parse`], so one fold of the line is all the
/// case-insensitivity costs.
fn matches(query: &Query, line: &str) -> bool {
    let lowered = line.to_lowercase();
    query
        .terms()
        .iter()
        .any(|term| lowered.contains(term.as_str()))
}

/// Builds one match, scrubbing the line on the way in and counting what that fired.
fn matched(
    record: &Record,
    locator: u64,
    surface: &'static str,
    line: &str,
    query: &Query,
    redactor: &Redactor,
    report: &mut RedactionReport,
) -> VerbatimMatch {
    let scrubbed = redactor.scrub(line);
    report.absorb(&scrubbed.report);
    VerbatimMatch {
        locator,
        record: record.record,
        line: record.line,
        at: record.timestamp,
        surface,
        excerpt: excerpt(&scrubbed.text, query),
    }
}

/// The text surfaces of one event, in the order matches within it are reported.
///
/// See the module docs for why this set and not the whole event: user and assistant text is what
/// somebody said, a tool event's typed command and its error and output are what a tool did and
/// answered, and the raw payload blobs are neither.
fn surfaces(event: &Event) -> Vec<(&'static str, &str)> {
    match event {
        Event::User { text } => vec![("user", text.as_str())],
        Event::Assistant { text } => vec![("assistant", text.as_str())],
        Event::Tool(tool) => tool_surfaces(tool),
    }
}

/// The three searchable fields of a tool event — the same three [`crate::evidence::Excerpt`]
/// renders — in a fixed order, skipping the ones this event does not carry.
fn tool_surfaces(tool: &ToolEvent) -> Vec<(&'static str, &str)> {
    let field = |key: &str| tool.fields.get(key).map(String::as_str);
    [
        ("command", tool.command()),
        ("error", field("error")),
        ("output", field("output")),
    ]
    .into_iter()
    .filter_map(|(surface, text)| text.map(|text| (surface, text)))
    .collect()
}

/// Cuts an already-scrubbed line down to a readable one-line excerpt.
///
/// The pipeline is [`crate::ask::snippet`]'s — collapse the whitespace, then clamp to
/// [`MAX_SNIPPET_CHARS`] — with one addition a transcript needs and a summary field does not: a
/// line long enough to be clamped is quoted from a window *around the first query term it carries*
/// rather than from its head, because the first 200 characters of a long build log are not where
/// the word somebody searched for is. On a line short enough to be quoted whole, and on one whose
/// terms the scrub replaced, this is [`crate::ask::snippet`] exactly.
///
/// The window is chosen on a lower-cased copy of the collapsed line, and a case fold can change a
/// character count (`İ` lower-cases to two characters), so the offset found there and the offset in
/// the line itself diverge by one character for each fold-expanding character *before* the term.
/// The honest bound is therefore not "a character or two": a line carrying enough of them can shift
/// the window far enough that the matched term falls outside the characters quoted, leaving an
/// excerpt that does not visibly show what it matched on. That costs a reader a quotation and
/// nothing else — the whole line went through the scrub before any of this ran, so every character
/// this can quote is scrubbed text whichever window it picks, and the match it counted stands.
fn excerpt(scrubbed: &str, query: &Query) -> String {
    let collapsed = scrubbed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_SNIPPET_CHARS {
        return collapsed;
    }
    let lowered = collapsed.to_lowercase();
    let Some(at) = query
        .terms()
        .iter()
        .filter_map(|term| lowered.find(term.as_str()))
        .min()
    else {
        // Nothing to centre on — the terms survive only in the pre-scrub line — so this is the
        // summary lane's own clamp, head-first.
        return snippet(&collapsed);
    };
    clip_from(
        &collapsed,
        lowered[..at]
            .chars()
            .count()
            .saturating_sub(EXCERPT_LEAD_CHARS),
    )
}

/// Takes [`MAX_SNIPPET_CHARS`] characters from `start`, marking each end that was cut.
fn clip_from(collapsed: &str, start: usize) -> String {
    let mut clipped = String::new();
    if start > 0 {
        clipped.push(EXCERPT_TRUNCATED);
    }
    let mut rest = collapsed.chars().skip(start);
    clipped.extend(rest.by_ref().take(MAX_SNIPPET_CHARS));
    if rest.next().is_some() {
        clipped.push(EXCERPT_TRUNCATED);
    }
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Claude Code exchange: a user request, the assistant's reply, an invocation carrying a
    /// typed command, and the failing result that carries the error text.
    const EXCHANGE: &str = concat!(
        r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"user","content":"tighten the redaction layer before the dashboard ships"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:05.000Z","message":{"role":"assistant","content":[{"type":"text","text":"redaction it is\nsecond line about logging"}]}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a2","timestamp":"2026-08-01T10:00:07.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test redaction"}}]}}"#,
        "\n",
        r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:09.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"redaction tests failed","is_error":true}]}}"#,
    );

    fn found(raw_query: &str) -> SessionVerbatim {
        search(
            Source::ClaudeCode,
            2,
            EXCHANGE.as_bytes(),
            &Query::parse(raw_query),
            &Redactor::new(),
        )
        .expect("v2 is supported")
    }

    /// The four surfaces a conversation actually carries are all searched, and each match names
    /// which one it came from — a reader has to be able to tell "I asked for this" from "a build
    /// log mentioned it".
    #[test]
    fn every_conversation_surface_is_searched_and_named() {
        let hits = found("redaction");
        let surfaces: Vec<&str> = hits.matches.iter().map(|hit| hit.surface).collect();
        assert_eq!(surfaces, ["user", "assistant", "command", "output"]);
        assert_eq!(hits.total_matches, 4);
        assert_eq!(hits.events_searched, 4);
        assert_eq!(hits.unreadable_records, 0);
        // File order, and the locator is the event ordinal rather than the record's.
        let locators: Vec<u64> = hits.matches.iter().map(|hit| hit.locator).collect();
        assert_eq!(locators, [1, 2, 3, 4]);
        assert_eq!(hits.matches[0].record, 1);
        assert_eq!(hits.matches[3].record, 4);
        assert_eq!(
            hits.matches[2].excerpt, "cargo test redaction",
            "the typed command is the surface, not the raw input blob",
        );
    }

    /// A match is a *line*, so one long assistant message contributes only the lines that carry the
    /// term — and the excerpt is that line, not the whole message.
    #[test]
    fn a_match_is_one_line_of_a_surface() {
        let hits = found("logging");
        assert_eq!(hits.total_matches, 1);
        assert_eq!(hits.matches[0].surface, "assistant");
        assert_eq!(hits.matches[0].excerpt, "second line about logging");
    }

    /// The raw tool payload is never searched: `input` carries the file contents and patches a
    /// session read, and a term inside one says the term was in a file, not that the session was
    /// about it. The typed command promoted out of that blob *is* searched, which is the whole
    /// point of the distinction.
    #[test]
    fn the_raw_tool_payload_is_not_a_searchable_surface() {
        // "toolu" appears only in the tool_use_id and "cargo" only inside the invocation's typed
        // command; the first is metadata, the second is content.
        assert_eq!(found("toolu").total_matches, 0);
        assert_eq!(found("bash").total_matches, 0, "a tool name is not content");
        assert_eq!(found("cargo").total_matches, 1);
    }

    /// The cap bounds what is rendered without hiding what was found: the total counts every
    /// matching line, and the shown ones are the earliest.
    #[test]
    fn the_cap_bounds_the_shown_matches_and_never_the_count() {
        let mut transcript = String::new();
        for index in 0..20 {
            transcript.push_str(&format!(
                r#"{{"type":"user","uuid":"u{index}","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"user","content":"payments run {index}"}}}}"#,
            ));
            transcript.push('\n');
        }
        let hits = search(
            Source::ClaudeCode,
            2,
            transcript.as_bytes(),
            &Query::parse("payments"),
            &Redactor::new(),
        )
        .expect("v2 is supported");
        assert_eq!(hits.total_matches, 20);
        assert_eq!(hits.matches.len(), MAX_MATCHES_PER_SESSION);
        assert_eq!(hits.matches[0].excerpt, "payments run 0");
        assert_eq!(hits.matches[4].excerpt, "payments run 4");

        // The same transcript searched twice is the same block, byte for byte.
        let again = search(
            Source::ClaudeCode,
            2,
            transcript.as_bytes(),
            &Query::parse("payments"),
            &Redactor::new(),
        )
        .expect("v2 is supported");
        assert_eq!(hits.matches, again.matches);
    }

    /// The canary, at the unit level: a query whose term sits on the very line that carries a
    /// credential still matches — the match is decided on what the transcript said — and the
    /// excerpt beside it is scrubbed, with the replacement counted so a footer can explain it.
    #[test]
    fn a_line_that_carries_a_credential_matches_and_is_still_scrubbed() {
        let secret = "ghp_CANARYCANARYCANARYCANARYCANARYCANARY";
        let transcript = format!(
            r#"{{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"user","content":"the token {secret} was rejected"}}}}"#,
        );
        let hits = search(
            Source::ClaudeCode,
            2,
            transcript.as_bytes(),
            &Query::parse("rejected"),
            &Redactor::new(),
        )
        .expect("v2 is supported");
        assert_eq!(hits.total_matches, 1, "the match survives the scrub");
        assert!(
            !hits.matches[0].excerpt.contains(secret),
            "the excerpt leaked: {}",
            hits.matches[0].excerpt,
        );
        assert!(hits.matches[0].excerpt.contains("[REDACTED:github-token]"));
        assert_eq!(hits.redaction.total(), 1, "and the scrub was counted");

        // A search *for* the secret's own shape still finds the line rather than pretending the
        // session never mentioned it — the scrub decides what is shown, never what is true.
        let hits = search(
            Source::ClaudeCode,
            2,
            transcript.as_bytes(),
            &Query::parse("CANARYCANARYCANARYCANARYCANARYCANARY"),
            &Redactor::new(),
        )
        .expect("v2 is supported");
        assert_eq!(hits.total_matches, 1);
        assert!(!hits.matches[0].excerpt.contains(secret));
    }

    /// The canary the *ordering* actually rests on: a credential that straddles the window's own
    /// edge.
    ///
    /// The test above proves a secret is replaced; it cannot prove the scrub ran *before* the cut,
    /// because its line is short enough to be quoted whole. This one is not. The line is laid out
    /// so the window lands mid-token — the term sits at character 101, so the window opens at 51
    /// and closes at 251, and the token runs 215..255 — which means a cut-then-scrub order would
    /// hand back the token's first 36 characters, four short of the length the `github-token`
    /// pattern needs to recognize it, and those 36 characters would render as themselves.
    ///
    /// The fixture checks its own premise first: it runs the wrong order deliberately and asserts
    /// that it *would* have leaked. So a change to [`MAX_SNIPPET_CHARS`] or [`EXCERPT_LEAD_CHARS`]
    /// that stopped the token straddling the edge fails here rather than quietly turning this into
    /// a test that cannot fail.
    #[test]
    fn a_credential_straddling_the_window_edge_cannot_survive_the_cut() {
        let token = format!("ghp_{}", "CANARY".repeat(6));
        let line = format!(
            "{} needle {} {} {}",
            "x".repeat(100),
            "y".repeat(106),
            token,
            "z".repeat(60),
        );
        assert!(
            line.chars().count() > MAX_SNIPPET_CHARS,
            "the line has to be long enough to be windowed at all",
        );
        let query = Query::parse("needle");

        // The premise, stated as an assertion: cutting first and scrubbing after would leave a long
        // run of the token on the screen. This is the mutation this test exists to catch, run here
        // so the fixture can never drift into proving nothing.
        let wrong_order = Redactor::new().scrub(&excerpt(&line, &query)).text;
        assert!(
            wrong_order.contains(&token[..36]),
            "the token must straddle the window edge for this test to mean anything: {wrong_order}",
        );

        let transcript = format!(
            r#"{{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"user","content":"{line}"}}}}"#,
        );
        let hits = search(
            Source::ClaudeCode,
            2,
            transcript.as_bytes(),
            &query,
            &Redactor::new(),
        )
        .expect("v2 is supported");
        assert_eq!(hits.total_matches, 1);
        let excerpt = &hits.matches[0].excerpt;

        assert!(
            excerpt.contains("[REDACTED:github-token]"),
            "the whole token was replaced before anything was cut: {excerpt}",
        );
        assert!(
            excerpt.contains("needle"),
            "the window still centres: {excerpt}"
        );
        // Not a fragment of it either: no eight consecutive characters of the token survive
        // anywhere in what is rendered.
        let characters: Vec<char> = token.chars().collect();
        for window in characters.windows(8) {
            let fragment: String = window.iter().collect();
            assert!(
                !excerpt.contains(&fragment),
                "a token fragment survived the cut: {fragment} in {excerpt}",
            );
        }
        assert!(excerpt.chars().count() <= MAX_SNIPPET_CHARS + 2);
        assert_eq!(hits.redaction.total(), 1, "and the scrub was counted");
    }

    /// A short line is quoted whole and exactly as the summary lane would quote it; a long one is
    /// quoted from a window around the term, so the reader can see the word they searched for.
    #[test]
    fn a_long_line_is_quoted_around_the_term_and_a_short_one_exactly_as_a_snippet_is() {
        let query = Query::parse("needle");
        for short in ["a needle in here", "collapse\tthe   whitespace", ""] {
            assert_eq!(excerpt(short, &query), snippet(short), "{short:?}");
        }

        let long = format!("{} needle {}", "x".repeat(2_000), "y".repeat(2_000));
        let cut = excerpt(&long, &query);
        assert!(cut.contains("needle"), "the term is in the window: {cut}");
        assert!(cut.starts_with(EXCERPT_TRUNCATED));
        assert!(cut.ends_with(EXCERPT_TRUNCATED));
        assert!(
            cut.chars().count() <= MAX_SNIPPET_CHARS + 2,
            "{}",
            cut.len()
        );

        // A term the scrub replaced leaves nothing to centre on, and the clamp is the summary
        // lane's own, head-first.
        let no_anchor = "z".repeat(2_000);
        assert_eq!(excerpt(&no_anchor, &query), snippet(&no_anchor));
    }

    /// A record this build cannot read carries no typed text, so it is counted rather than
    /// searched — the lane's own "counted, never dropped", inside one transcript.
    #[test]
    fn an_unreadable_record_is_counted_rather_than_searched() {
        let transcript = format!("{{not json at all\n{EXCHANGE}");
        let hits = search(
            Source::ClaudeCode,
            2,
            transcript.as_bytes(),
            &Query::parse("redaction"),
            &Redactor::new(),
        )
        .expect("v2 is supported");
        assert_eq!(hits.unreadable_records, 1);
        assert_eq!(hits.total_matches, 4, "the readable records still matched");
    }

    /// A contract this build does not know is refused rather than half-read, exactly as the fold
    /// and the evidence route refuse it.
    #[test]
    fn an_unsupported_artifact_set_version_is_refused() {
        let refused = search(
            Source::ClaudeCode,
            u16::MAX,
            EXCHANGE.as_bytes(),
            &Query::parse("redaction"),
            &Redactor::new(),
        );
        assert!(refused.is_err());
    }
}
