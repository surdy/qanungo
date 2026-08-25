//! Evidence anchors, and the excerpts they resolve to.
//!
//! # The gap this closes
//!
//! [`crate::rules`] produced verdicts and counts and nothing else: a finding said *six of twelve
//! calls failed* and named the session by content hash, but nothing anywhere recorded **which
//! events** were counted. There was therefore no span to cut, and no honest way to show a reader
//! the thing a rule reacted to. This module is the missing half — the fold now records, for every
//! rule whose counted signal is an *event*, a bounded set of anchors saying where those events are,
//! and a served surface can turn one anchor back into one redacted excerpt.
//!
//! # Anchors are strictly additive
//!
//! Nothing here participates in a verdict, a score, a fire rate, or a count.
//! [`RuleId::verdict`](crate::rules::RuleId::verdict) never reads an anchor, [`crate::scoring`]
//! never sees one, and the Markdown `qanungo report` writes is byte-for-byte what it was before
//! this module existed. That is why the anchor caps below are **not** in the rule-pack stamp: a
//! rule pack fixes what the rules decide, and these constants change only how much evidence is
//! offered for a decision already taken.
//!
//! # What an anchor is
//!
//! [`EventAnchor`] — the transcript's content hash (carried by the finding, not repeated per
//! anchor), a **locator**, the record and line the event was on, the record's timestamp, and the
//! tool name.
//!
//! The locator is the event's **1-based ordinal among the transcript's tool events, in file
//! order**. A record can carry several tool events, so a record number alone does not identify one;
//! a tool-event ordinal does, is a single bounded integer a URL can be strictly validated against,
//! and is re-derived by walking the same stream with the same interpreter. The record and line
//! numbers ride along because they are what a human takes to their own shell — but they are
//! context, not the key.
//!
//! Because a transcript is addressed by the sha256 of its bytes, a locator cannot come to mean a
//! different event: different bytes are a different hash and a different anchor set.
//!
//! # What an excerpt is
//!
//! **The counted event, and nothing around it.** [`Excerpt`] carries the tool name, the event's own
//! command string, its own error and output text, and its timestamp — every one of the free-text
//! fields through the [`Redactor`] the process was launched with. No
//! neighbouring events, no request that provoked it, no response that followed.
//!
//! That is narrower than it first looks, and deliberately so. Claude Code puts the command on the
//! *invocation* and the error on the *result*, so the excerpt of a counted error carries the error
//! and no command: pairing them means reading a second event, which is surrounding context. Codex
//! and Copilot shell events carry the command on the event the retry-loop rule counts, so those
//! excerpts do have one. Widening this is a later slice, pulled by somebody actually wanting it.
//!
//! The raw `input` / `arguments` blobs are **never** excerpted. They are the whole tool payload —
//! file contents, patches, prompts — and "the event's command string" is what the grilling settled
//! on.
//!
//! # The serving boundary
//!
//! [`EvidenceIndex`] is the whole of what a served surface will resolve: the anchors **the current
//! payload actually names**. A locator that no finding offered is a 404 even when the transcript is
//! cached and the locator is perfectly well-formed. Without that, this route would be a general
//! transcript-browsing API wearing a coaching dashboard's clothes — any tailnet peer could walk any
//! cached session event by event, which is precisely the disclosure the 2026-08-24 grilling refused
//! when it removed the Patwari deep-links.
//!
//! The second half of that boundary lives in [`crate::dashboard_server`]: a blob that is not
//! already in the local cache is a 404, never a fetch. A browser must not be able to make this
//! process talk to the archive.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

use chrono::{DateTime, Utc};
use munshi_transcript::{
    Classification, Event, Source, ToolEvent, TranscriptStream, UnsupportedVersion,
};

use crate::format;
use crate::redaction::{RedactionReport, Redactor};

/// Anchors one finding offers for one session. **A tunable, not a decision.**
///
/// Ten is enough for a reader to see what kind of failure a rule is reacting to and few enough that
/// a window with sixty firing sessions stays a payload rather than a download. A session that
/// failed two hundred calls does not need two hundred anchors to make its point; what it needs is
/// the count, which the finding already carries.
pub const MAX_ANCHORS_PER_FINDING: usize = 10;

/// Distinct command values the fold will keep anchors for, per session. **A tunable.**
///
/// The churn fold already tracks up to
/// [`MAX_DISTINCT_COMMANDS`](crate::metrics::MAX_DISTINCT_COMMANDS) values; keeping anchors for
/// every one of them would multiply the scratch memory of a pathological transcript by the anchor
/// cap. A retry loop's command is, by construction, one a session ran early and often, so the
/// values that matter are in the first few hundred distinct ones. Past this cap a value still
/// *counts* exactly as it did — the churn numbers and therefore the verdict are untouched — it
/// simply offers no anchors, which is an under-claim the payload shows as an empty anchor list.
pub const MAX_ANCHORED_COMMANDS: usize = 512;

/// Distinct tool names the fold will keep error anchors for, per session. **A tunable**, on the
/// same reasoning: a real session names tools in the dozens, and a transcript that invents
/// thousands must not make the fold's memory a function of its imagination.
pub const MAX_ANCHORED_TOOLS: usize = 64;

/// Digits a locator may have on the wire. Nine allows every ordinal any real transcript can reach
/// and refuses a value long enough to be an attack on the parser rather than a request.
pub const MAX_LOCATOR_DIGITS: usize = 9;

/// Characters of any one excerpt field that reach a reader.
///
/// Tool output is unbounded — a build log, a file dump, a stack trace — and an excerpt is meant to
/// be read inside a finding's row, not scrolled. The cut is applied **after** the scrub, never
/// before: truncating first could cut a credential in half and hand back the half that survives.
pub const MAX_EXCERPT_CHARS: usize = 2_000;

/// Marks an excerpt field cut short. Ours, appended after the transcript's own bytes.
pub const EXCERPT_TRUNCATED: &str = "…";

/// Where one counted event is, in one transcript.
///
/// Carries no transcript content: a locator, two record positions, a timestamp, and a tool name.
/// The name is schema metadata — the one verbatim string decision 9 blessed for an aggregate
/// surface — but a harness writes it, so every *rendering* path clamps and scrubs it on the way out
/// ([`identifier_field`]). What is stored here is what the transcript said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventAnchor {
    /// 1-based ordinal among the transcript's tool events, in file order. The key.
    pub locator: u64,
    /// 1-based record ordinal (non-empty lines), as `munshi-transcript` numbers them.
    pub record: u64,
    /// 1-based physical line number.
    pub line: u64,
    /// The record's own timestamp, when it had one.
    pub at: Option<DateTime<Utc>>,
    /// The tool the fold attributed the event to, `None` when no invocation ever named it.
    pub tool: Option<String>,
}

/// The bounded anchors one session's fold produced, grouped by the rule component that counts them.
///
/// Grouped rather than pooled because the components mean different things: an error-rate rule that
/// fired *because one tool is failing* should show that tool's failures, not the ten failures that
/// happened to come first. See [`SessionAnchors::errors_for`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionAnchors {
    /// Failing tool outcomes, by the tool name the fold attributed them to. At most
    /// [`MAX_ANCHORS_PER_FINDING`] per tool, at most [`MAX_ANCHORED_TOOLS`] tools.
    pub tool_errors: BTreeMap<String, Vec<EventAnchor>>,
    /// Failing tool outcomes whose call id was never introduced, so no tool name could be
    /// attributed. Counted in [`ToolOutcomes::unattributed`](crate::metrics::ToolOutcomes) and
    /// anchored here rather than guessed into somebody's column.
    pub unattributed_errors: Vec<EventAnchor>,
    /// Runs of the session's busiest command value — the events the retry-loop rule counted.
    pub command_runs: Vec<EventAnchor>,
}

impl SessionAnchors {
    /// The failing events to offer for an error-rate finding.
    ///
    /// When the rule named tools, the anchors come from those tools, round-robin in name order, so
    /// a session called out for two failing tools shows both rather than ten of whichever failed
    /// first. When it fired only session-wide, every failing event is a candidate and they are
    /// offered in file order.
    ///
    /// Either way the result is capped at [`MAX_ANCHORS_PER_FINDING`] and sorted by locator, so the
    /// reader gets them in the order the session ran them.
    pub fn errors_for(&self, tools: &[&str]) -> Vec<EventAnchor> {
        let mut picked: Vec<EventAnchor> = Vec::new();
        if tools.is_empty() {
            for anchor in self
                .tool_errors
                .values()
                .flatten()
                .chain(&self.unattributed_errors)
            {
                picked.push(anchor.clone());
            }
        } else {
            // Round-robin: one from each named tool, then a second from each, until the cap. A
            // straight concatenation would spend the whole budget on the first tool.
            let named: Vec<&Vec<EventAnchor>> = tools
                .iter()
                .filter_map(|tool| self.tool_errors.get(*tool))
                .collect();
            let deepest = named.iter().map(|anchors| anchors.len()).max().unwrap_or(0);
            for round in 0..deepest {
                for anchors in &named {
                    if let Some(anchor) = anchors.get(round) {
                        picked.push(anchor.clone());
                    }
                }
                if picked.len() >= MAX_ANCHORS_PER_FINDING {
                    break;
                }
            }
        }
        picked.sort_by_key(|anchor| anchor.locator);
        picked.truncate(MAX_ANCHORS_PER_FINDING);
        picked
    }

    /// Records a failing outcome, under the tool it was attributed to.
    pub(crate) fn observe_error(&mut self, tool: Option<&str>, anchor: EventAnchor) {
        let Some(tool) = tool else {
            if self.unattributed_errors.len() < MAX_ANCHORS_PER_FINDING {
                self.unattributed_errors.push(anchor);
            }
            return;
        };
        if let Some(anchors) = self.tool_errors.get_mut(tool) {
            if anchors.len() < MAX_ANCHORS_PER_FINDING {
                anchors.push(anchor);
            }
        } else if self.tool_errors.len() < MAX_ANCHORED_TOOLS {
            self.tool_errors.insert(tool.to_owned(), vec![anchor]);
        }
    }
}

/// Which of a rule's components its evidence can come from.
///
/// A rule that counts events can point at them. A rule that measures a *shape* — how long the
/// longest sitting was, how many sittings there were, how many requests it took — cannot, and
/// inventing an excerpt for it would be dishonest: the rule did not read an utterance, it read a
/// duration. Rules with both natures say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    /// Every component counts concrete events; the evidence is those events.
    Event,
    /// Every component is structural; the evidence is timestamps and counts.
    Structural,
    /// One component counts events and the rest are structural — fire-and-forget, whose error
    /// component counts failures and whose ratio component is a shape.
    Mixed,
}

impl EvidenceKind {
    /// The payload's spelling.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Structural => "structural",
            Self::Mixed => "mixed",
        }
    }

    /// Whether a finding of this kind offers anchors.
    pub const fn anchors(self) -> bool {
        matches!(self, Self::Event | Self::Mixed)
    }

    /// Whether a finding of this kind offers structural statistics.
    pub const fn structural(self) -> bool {
        matches!(self, Self::Structural | Self::Mixed)
    }
}

/// One counted event, as the transcript holds it — before any scrub.
///
/// Deliberately not public to construct: the only way to get one is [`extract`], and the only thing
/// to do with one is [`RawExcerpt::redacted`]. A type that could be built with content and rendered
/// without passing the redactor would be exactly the accident this module exists to make
/// impossible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawExcerpt {
    locator: u64,
    record: u64,
    line: u64,
    at: Option<DateTime<Utc>>,
    tool: Option<String>,
    event: Option<String>,
    outcome: Option<bool>,
    command: Option<String>,
    error: Option<String>,
    output: Option<String>,
}

impl RawExcerpt {
    /// Scrubs every free-text field and reports what that cost.
    ///
    /// Tool names and event discriminators are schema metadata — decision 9 blessed them as the one
    /// verbatim string an *aggregate* surface may render — but this is not an aggregate surface, and
    /// a harness writes its own tool names. A name shaped like a credential is a credential on the
    /// screen, so they go through [`identifier_field`] rather than through the clamp alone.
    pub fn redacted(self, redactor: &Redactor) -> Excerpt {
        let mut report = RedactionReport::default();
        let mut truncated = false;
        let mut field = |text: Option<String>| -> Option<String> {
            let text = text?;
            let scrubbed = redactor.scrub(&text);
            report.absorb(&scrubbed.report);
            let (clipped, cut) = clip(&scrubbed.text);
            truncated |= cut;
            Some(clipped)
        };
        let command = field(self.command);
        let error = field(self.error);
        let output = field(self.output);
        // The clamp-then-scrub [`identifier_field`] states, spelled out here rather than called,
        // so a name that fired the scrub is *counted* in this excerpt's report — a reader seeing a
        // marker where a tool name belongs needs the total to explain it.
        let mut identifier = |text: Option<String>| -> Option<String> {
            let scrubbed = redactor.scrub(&format::identifier(text.as_deref()?));
            report.absorb(&scrubbed.report);
            Some(scrubbed.text)
        };
        let tool = identifier(self.tool);
        let event = identifier(self.event);
        Excerpt {
            locator: self.locator,
            record: self.record,
            line: self.line,
            at: self.at,
            tool,
            event,
            outcome: self.outcome,
            command,
            error,
            output,
            truncated,
            report,
        }
    }
}

/// An archive-stated identifier on a surface that renders verbatim: clamped, then scrubbed.
///
/// # Why the clamp runs first
///
/// The two do different jobs and only this order keeps both of them. [`format::identifier`] judges
/// **what the archive actually said** — it replaces a value carrying a control character, a pipe, a
/// backtick, or more than [`format::MAX_IDENTIFIER_CHARS`] characters *wholesale*, because a prefix
/// of arbitrary text is still arbitrary text. Scrubbing first would let a hostile value launder
/// itself past that judgement: a 200-character token is not a renderable identifier, but the marker
/// it scrubs down to is, so the clamp would wave through a value it exists to refuse. Clamping
/// first means the clamp sees the archive's own bytes, and the scrub then works on text already
/// known to be renderable.
///
/// It costs nothing in the other direction: a name shaped like a credential is well under the
/// clamp's ceiling and carries none of its forbidden characters, so it reaches the scrub intact and
/// leaves as a marker. And scrubbing a marker is a no-op ([`crate::redaction`] is idempotent), so
/// nothing here can nest.
pub fn identifier_field(value: &str, redactor: &Redactor) -> String {
    redactor.scrub_text(&format::identifier(value))
}

/// One counted event, scrubbed and clipped, ready to be serialized to a reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    pub locator: u64,
    pub record: u64,
    pub line: u64,
    pub at: Option<DateTime<Utc>>,
    pub tool: Option<String>,
    /// The interpreter's own event discriminator (`tool_result`, `local_shell_call`, ...).
    pub event: Option<String>,
    /// Whether the event reported success, when it reported an outcome at all.
    pub outcome: Option<bool>,
    pub command: Option<String>,
    pub error: Option<String>,
    pub output: Option<String>,
    /// Whether any field was cut at [`MAX_EXCERPT_CHARS`].
    pub truncated: bool,
    /// What the scrub fired, by pattern. Counts only — see [`crate::redaction`].
    pub report: RedactionReport,
}

/// Cuts a field to [`MAX_EXCERPT_CHARS`] on a character boundary, saying whether it cut.
fn clip(text: &str) -> (String, bool) {
    let mut clipped: String = text.chars().take(MAX_EXCERPT_CHARS).collect();
    let cut = text.chars().nth(MAX_EXCERPT_CHARS).is_some();
    if cut {
        clipped.push_str(EXCERPT_TRUNCATED);
    }
    (clipped, cut)
}

/// Reads one anchored event back out of a transcript.
///
/// Walks the same stream the fold walked, with the same interpreter and the same call-id
/// correlation, counting tool events until it reaches `locator` — then stops. It is one pass, it
/// buffers nothing, and for an early locator it stops reading the file part-way through.
///
/// `Ok(None)` means the transcript has no such event: fewer tool events than the locator asks for.
/// That should be unreachable for an anchor the fold itself minted against this very content hash,
/// and is answered rather than asserted because the caller is a network route.
///
/// # Errors
///
/// Returns an error when `artifact_set_version` names a contract this build cannot read — the same
/// refusal [`fold_transcript`](crate::metrics::fold_transcript) makes, for the same reason.
pub fn extract(
    source: Source,
    artifact_set_version: u16,
    reader: impl BufRead,
    locator: u64,
) -> Result<Option<RawExcerpt>, UnsupportedVersion> {
    let stream = TranscriptStream::new(source, artifact_set_version, reader)?;
    // The same correlation the fold keeps, for the same reason: an outcome event names the call,
    // not the tool, so the invocation that introduced the call id is what supplies the name.
    let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut ordinal = 0_u64;
    for item in stream {
        let Ok(record) = item else { continue };
        let Classification::Content { events } = &record.classification else {
            continue;
        };
        for event in events {
            let Event::Tool(tool) = event else { continue };
            ordinal += 1;
            if let (Some(call_id), Some(name)) = (tool.call_id(), tool.name())
                && names.len() < crate::metrics::MAX_CORRELATED_CALLS
            {
                names.insert(call_id.to_owned(), name.to_owned());
            }
            if ordinal == locator {
                return Ok(Some(excerpt_of(tool, &names, &record, ordinal)));
            }
        }
    }
    Ok(None)
}

/// Builds the excerpt of one tool event: its own fields, and nothing from any other event.
fn excerpt_of(
    tool: &ToolEvent,
    names: &std::collections::HashMap<String, String>,
    record: &munshi_transcript::Record,
    locator: u64,
) -> RawExcerpt {
    let name = tool.name().map(ToOwned::to_owned).or_else(|| {
        tool.call_id()
            .and_then(|call_id| names.get(call_id).cloned())
    });
    RawExcerpt {
        locator,
        record: record.record,
        line: record.line,
        at: record.timestamp,
        tool: name,
        event: tool.event().map(ToOwned::to_owned),
        outcome: crate::metrics::outcome(tool),
        command: tool.command().map(ToOwned::to_owned),
        error: tool.fields.get("error").cloned(),
        output: tool.fields.get("output").cloned(),
    }
}

/// A locator as it may arrive on a URL, or `None` when the text is not one.
///
/// Strict on purpose, because this is a path segment an unauthenticated peer chooses: ASCII digits
/// only, at most [`MAX_LOCATOR_DIGITS`] of them, no leading zero, no sign, no separators, and never
/// zero — locators are 1-based. Everything else is not a malformed locator to be repaired, it is
/// not a locator, and the route answers 404.
pub fn parse_locator(text: &str) -> Option<u64> {
    let usable = !text.is_empty()
        && text.len() <= MAX_LOCATOR_DIGITS
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && !text.starts_with('0');
    usable.then(|| text.parse().ok()).flatten()
}

/// What one session contributes to the servable set: how to re-read it, and which locators the
/// payload named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedSession {
    pub source_agent: String,
    pub artifact_set_version: u16,
    locators: BTreeSet<u64>,
}

/// The anchors the currently-served payload names — and therefore the entire set of excerpts this
/// process will resolve.
///
/// Rebuilt with every refresh and swapped with the payload it belongs to, so a locator is servable
/// exactly as long as the finding that offered it is on the page. See the module docs for why this
/// boundary is the difference between an evidence route and a transcript-browsing API.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidenceIndex {
    sessions: BTreeMap<String, IndexedSession>,
}

impl EvidenceIndex {
    /// Records that `locator` in `source_hash` is offered by the payload being built.
    pub fn offer(
        &mut self,
        source_hash: &str,
        source_agent: &str,
        artifact_set_version: u16,
        locator: u64,
    ) {
        self.sessions
            .entry(source_hash.to_owned())
            .or_insert_with(|| IndexedSession {
                source_agent: source_agent.to_owned(),
                artifact_set_version,
                locators: BTreeSet::new(),
            })
            .locators
            .insert(locator);
    }

    /// The session to read `locator` out of, or `None` when the payload never named that pair.
    pub fn servable(&self, source_hash: &str, locator: u64) -> Option<&IndexedSession> {
        self.sessions
            .get(source_hash)
            .filter(|session| session.locators.contains(&locator))
    }

    /// Sessions with at least one servable anchor.
    pub fn sessions(&self) -> usize {
        self.sessions.len()
    }

    /// Anchors servable in total.
    pub fn anchors(&self) -> usize {
        self.sessions
            .values()
            .map(|session| session.locators.len())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(locator: u64, tool: &str) -> EventAnchor {
        EventAnchor {
            locator,
            record: locator * 2,
            line: locator * 2,
            at: None,
            tool: Some(tool.to_owned()),
        }
    }

    /// A locator is a path segment an unauthenticated peer chooses, so the grammar is exactly what
    /// the fold mints and nothing else.
    #[test]
    fn only_a_bare_positive_bounded_integer_is_a_locator() {
        assert_eq!(parse_locator("1"), Some(1));
        assert_eq!(parse_locator("999999999"), Some(999_999_999));
        for bad in [
            "",
            "0",
            "01",
            "-1",
            "+1",
            " 1",
            "1 ",
            "1.0",
            "1e3",
            "0x10",
            "1_000",
            "١٢٣",
            "1000000000",
            "99999999999999999999999999",
        ] {
            assert_eq!(parse_locator(bad), None, "{bad:?} must not parse");
        }
    }

    /// The cap is per tool and the offer is round-robin, so a session called out for two failing
    /// tools shows both instead of ten of whichever happened to fail first.
    #[test]
    fn named_tools_share_the_anchor_budget() {
        let mut anchors = SessionAnchors::default();
        for locator in 1..=20 {
            anchors.observe_error(Some("Bash"), anchor(locator, "Bash"));
        }
        for locator in 21..=25 {
            anchors.observe_error(Some("Read"), anchor(locator, "Read"));
        }
        assert_eq!(anchors.tool_errors["Bash"].len(), MAX_ANCHORS_PER_FINDING);
        assert_eq!(anchors.tool_errors["Read"].len(), 5);

        let offered = anchors.errors_for(&["Bash", "Read"]);
        assert_eq!(offered.len(), MAX_ANCHORS_PER_FINDING);
        assert!(
            offered
                .iter()
                .any(|anchor| anchor.tool.as_deref() == Some("Read")),
            "the second named tool must not be crowded out: {offered:?}",
        );
        // In file order whatever the interleaving was.
        let locators: Vec<u64> = offered.iter().map(|anchor| anchor.locator).collect();
        let mut sorted = locators.clone();
        sorted.sort_unstable();
        assert_eq!(locators, sorted);

        // With no tool named — the session-wide trigger — every failure is a candidate, still
        // bounded and still in file order.
        let pooled = anchors.errors_for(&[]);
        assert_eq!(pooled.len(), MAX_ANCHORS_PER_FINDING);
        assert_eq!(pooled[0].locator, 1);
    }

    /// A failure nobody could attribute is anchored under no name rather than guessed into a
    /// column, and is offered by the session-wide selection.
    #[test]
    fn an_unattributed_failure_is_anchored_without_a_tool_name() {
        let mut anchors = SessionAnchors::default();
        anchors.observe_error(
            None,
            EventAnchor {
                locator: 7,
                record: 7,
                line: 7,
                at: None,
                tool: None,
            },
        );
        assert!(anchors.tool_errors.is_empty());
        let offered = anchors.errors_for(&[]);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].tool, None);
    }

    /// The tool cap bounds the fold's memory against a transcript that invents tool names, and it
    /// under-claims rather than dropping the count: the tally still counted the failure.
    #[test]
    fn the_anchored_tool_cap_stops_adding_tools_rather_than_growing() {
        let mut anchors = SessionAnchors::default();
        for tool in 0..MAX_ANCHORED_TOOLS + 10 {
            anchors.observe_error(Some(&format!("tool-{tool}")), anchor(tool as u64 + 1, "x"));
        }
        assert_eq!(anchors.tool_errors.len(), MAX_ANCHORED_TOOLS);
    }

    /// Only the pair the payload named is servable. A well-formed locator against a cached
    /// transcript the payload does not carry is not evidence, it is transcript browsing.
    #[test]
    fn the_index_serves_only_what_the_payload_named() {
        let mut index = EvidenceIndex::default();
        index.offer(&"a".repeat(64), "claude-code", 2, 4);
        index.offer(&"a".repeat(64), "claude-code", 2, 9);
        assert_eq!(index.sessions(), 1);
        assert_eq!(index.anchors(), 2);
        assert!(index.servable(&"a".repeat(64), 4).is_some());
        assert!(
            index.servable(&"a".repeat(64), 5).is_none(),
            "a locator between two anchored ones is not anchored",
        );
        assert!(index.servable(&"b".repeat(64), 4).is_none());
    }

    /// A Claude Code exchange: an invocation naming the tool, then the result that reports the
    /// outcome and carries the text.
    const EXCHANGE: &str = concat!(
        r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"make release"}}]}}"#,
        "\n",
        r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:09.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"linker failed","is_error":true}]}}"#,
    );

    fn read(locator: u64) -> Option<RawExcerpt> {
        extract(Source::ClaudeCode, 2, EXCHANGE.as_bytes(), locator).expect("v2 is supported")
    }

    /// An outcome event names the *call*, not the tool, so the extraction carries the same call-id
    /// correlation the fold does. Without it every excerpt of a counted failure — which is most of
    /// them — would come back with no tool name at all.
    #[test]
    fn an_outcome_event_is_named_through_the_invocation_that_introduced_it() {
        let invocation = read(1).expect("the first tool event");
        assert_eq!(invocation.tool.as_deref(), Some("Bash"));
        assert_eq!(invocation.event.as_deref(), Some("tool_use"));
        assert_eq!(invocation.command.as_deref(), Some("make release"));
        assert_eq!(invocation.outcome, None, "an invocation reports no outcome");

        let result = read(2).expect("the second tool event");
        assert_eq!(
            result.tool.as_deref(),
            Some("Bash"),
            "the result carries only a call id, and is named through it",
        );
        assert_eq!(result.outcome, Some(false));
        assert_eq!(result.output.as_deref(), Some("linker failed"));
        assert_eq!(
            result.command, None,
            "the command was on the other event, and the other event is not this excerpt",
        );
        assert_eq!((result.record, result.line), (2, 2));
    }

    /// Past the end of the transcript there is no event, and the answer is that rather than an
    /// empty excerpt: the caller is a network route, and "there is nothing there" is a 404.
    #[test]
    fn a_locator_past_the_last_tool_event_resolves_to_nothing() {
        assert!(read(3).is_none());
        assert!(read(999_999).is_none());
    }

    /// A harness writes its own tool names, and this is a surface that renders verbatim. Decision 9
    /// blessed tool names for the aggregate lines; a name shaped like a credential is a credential
    /// on the screen, so on this path they are clamped *and* scrubbed — and the scrub is counted, so
    /// the marker a reader sees where a tool name belongs is explained by the excerpt's own total.
    #[test]
    fn a_tool_name_shaped_like_a_credential_is_scrubbed_like_one() {
        let token = format!("ghp_{}", "CANARY".repeat(6));
        let raw = RawExcerpt {
            locator: 1,
            record: 1,
            line: 1,
            at: None,
            tool: Some(token.clone()),
            event: Some("tool_result".to_owned()),
            outcome: Some(false),
            command: None,
            error: Some("exit status 1".to_owned()),
            output: None,
        };
        let excerpt = raw.redacted(&Redactor::new());
        assert_eq!(excerpt.tool.as_deref(), Some("[REDACTED:github-token]"));
        assert_eq!(
            excerpt.event.as_deref(),
            Some("tool_result"),
            "an ordinary name is untouched"
        );
        assert_eq!(
            excerpt.report.total(),
            1,
            "the scrub is counted, not silent"
        );
        assert_eq!(excerpt.error.as_deref(), Some("exit status 1"));

        // `--no-redact` means raw here too, or the flag would be lying about a different field.
        let bare = RawExcerpt {
            tool: Some(token.clone()),
            ..raw_of(&excerpt)
        }
        .redacted(&Redactor::new().with_secrets(false));
        assert_eq!(bare.tool, Some(token));
    }

    /// Rebuilds a raw excerpt from a scrubbed one's positions, so the test above can run the same
    /// event through a second redactor without restating every field.
    fn raw_of(excerpt: &Excerpt) -> RawExcerpt {
        RawExcerpt {
            locator: excerpt.locator,
            record: excerpt.record,
            line: excerpt.line,
            at: excerpt.at,
            tool: None,
            event: excerpt.event.clone(),
            outcome: excerpt.outcome,
            command: None,
            error: None,
            output: None,
        }
    }

    /// The order is the argument, so it is pinned rather than left to the comment. Clamping first
    /// means the clamp judges what the archive said; scrubbing first would let a value too long to
    /// be an identifier launder itself into one.
    #[test]
    fn an_identifier_is_clamped_before_it_is_scrubbed() {
        let redactor = Redactor::new();
        // Inside the clamp, shaped like a secret: the clamp passes it and the scrub takes it.
        let token = format!("ghp_{}", "CANARY".repeat(6));
        assert_eq!(
            identifier_field(&token, &redactor),
            "[REDACTED:github-token]"
        );

        // Too long to be an identifier at all: replaced wholesale, and *not* rescued into
        // renderability by scrubbing it down to a marker first.
        let overlong = format!("ghp_{}", "CANARY".repeat(20));
        assert!(overlong.chars().count() > format::MAX_IDENTIFIER_CHARS);
        assert_eq!(
            identifier_field(&overlong, &redactor),
            format::INVALID_IDENTIFIER,
        );

        // A control character is the clamp's business and stays the clamp's business.
        assert_eq!(
            identifier_field("Bash\nSPOOFED", &redactor),
            format::INVALID_IDENTIFIER,
        );
        // And an ordinary tool name costs nothing on the way through either pass.
        for ordinary in [
            "Bash",
            "local_shell",
            "mcp__server__search_code",
            "<synthetic>",
        ] {
            assert_eq!(identifier_field(ordinary, &redactor), ordinary);
        }
        // Scrubbing a marker is a no-op, so nothing here can nest on a second pass.
        assert_eq!(
            identifier_field(&identifier_field(&token, &redactor), &redactor),
            "[REDACTED:github-token]",
        );
    }

    /// The scrub runs before the cut, so a credential that straddles the ceiling is replaced by a
    /// marker rather than having its head survive it.
    #[test]
    fn a_field_is_scrubbed_before_it_is_clipped() {
        let token = format!("ghp_{}", "a".repeat(36));
        // The credential sits just inside the ceiling and the field runs well past it, so the
        // ordering is what decides whether it survives: scrubbed first, the marker is what gets
        // clipped; clipped first, the token's head would be what got rendered.
        let padding = format!(
            "{} {token} {}",
            "x".repeat(MAX_EXCERPT_CHARS - 100),
            "y".repeat(200),
        );
        let raw = RawExcerpt {
            locator: 1,
            record: 1,
            line: 1,
            at: None,
            tool: Some("Bash".to_owned()),
            event: Some("tool_result".to_owned()),
            outcome: Some(false),
            command: None,
            error: None,
            output: Some(padding),
        };
        let excerpt = raw.redacted(&Redactor::new());
        let output = excerpt.output.expect("the field survives");
        assert!(
            !output.contains("ghp_"),
            "the token did not survive the cut"
        );
        assert!(output.contains("[REDACTED:github-token]"));
        assert!(excerpt.truncated, "the field was longer than the ceiling");
        assert_eq!(excerpt.report.total(), 1);
    }

    /// `--no-redact` is a person's documented choice, and it has to actually mean raw — a redactor
    /// that scrubbed anyway would make the flag a lie, and one that clipped nothing would make the
    /// ceiling one.
    #[test]
    fn a_redactor_with_the_secrets_pass_off_hands_back_what_the_transcript_held() {
        let token = format!("ghp_{}", "a".repeat(36));
        let raw = RawExcerpt {
            locator: 1,
            record: 1,
            line: 1,
            at: None,
            tool: Some("Bash".to_owned()),
            event: Some("tool_result".to_owned()),
            outcome: Some(false),
            command: None,
            error: Some(token.clone()),
            output: None,
        };
        let excerpt = raw.redacted(&Redactor::new().with_secrets(false));
        assert_eq!(excerpt.error, Some(token));
        assert!(excerpt.report.is_empty());
        assert!(!excerpt.truncated);
    }
}
