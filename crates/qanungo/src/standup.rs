//! The standup fold: what a window of archived summaries says, scrubbed and arranged.
//!
//! `report` and `cost` read `transcript.jsonl` and emit numbers. This lane reads `summary.md` —
//! the record munshi already wrote about each session (ADR 0009/0010) — and emits what it says.
//! There is no model in the loop and no reconstruction: the narrative was written when the session
//! was captured, by the harness that was in it, and qanungo's whole job here is to select a
//! window, put the pieces in an order a person can read, and be honest about the pieces it could
//! not get.
//!
//! # The scrub happens here, not in the renderer
//!
//! This is qanungo #8's first consumer, and the way it satisfies that issue is structural: every
//! string this module puts into a [`StandupSession`], a [`RolledUp`] line, or a group heading has
//! *already* been through the [`Redactor`], and the renderer never sees an unscrubbed one. A
//! filter at the rendering site would be one forgotten `push_str` away from leaking; a fold that
//! only ever produces scrubbed text cannot be. The counts travel with the text
//! ([`Standup::redaction`]) so the footer can say what fired, and — per that issue's counts-only
//! invariant — nothing anywhere carries what it matched.
//!
//! Archive-stated identifiers (the repository and branch a summary names) are scrubbed *and then*
//! clamped through [`crate::format::identifier`], in that order: the scrub is about what the text
//! contains, the clamp is about what a peer may put on a rendering surface, and both apply.
//!
//! # No signal, no claim
//!
//! A session whose snapshots carry no `summary.md`, one whose summary this build cannot parse, and
//! one carrying munshi's machine-generated placeholder (issue #43) are all *gaps*, named with
//! their reason in the document. None of them is silently dropped and none is guessed at. A
//! placeholder in particular is a real archive state that means "munshi still owes a summary for
//! this session", and rendering its stand-in text as though it were a narrative would be the lane
//! inventing work that nobody did.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use chrono::{DateTime, Utc};
use munshi_transcript::{ArchivedMarkdown, RenderError, parse_archive_markdown};

use crate::cache::BlobCache;
use crate::format;
use crate::patwari::SUMMARY_LOGICAL_PATH;
use crate::redaction::{RedactionReport, Redactor};
use crate::report::SkippedNote;
use crate::sync::{Artifact, MirroredSession, SkipReason};

/// The group a session lands in when its own summary names no repository.
///
/// A real state rather than a defect: munshi records a repository only for a session captured
/// inside a checkout, and the cost lane already gives that case its own row rather than folding it
/// into a named repository's. Spelled as a sentence so it cannot be mistaken for a repository
/// actually called that.
pub const NO_REPOSITORY: &str = "no repository recorded";

/// One archived summary, read off the cache and parsed, before it is scrubbed or grouped.
#[derive(Debug, Clone)]
pub struct ReadSummary {
    /// The `summary.md`'s content hash — cache key and cited evidence in one, exactly as the
    /// transcript's is in the other lanes.
    pub source_hash: String,
    /// When the archive finished the snapshot this session was listed by. Archive time, the clock
    /// the window was cut on.
    pub archived_at: Option<DateTime<Utc>>,
    /// Decompressed bytes of the summary the fold read.
    pub bytes_read: u64,
    pub archived: ArchivedMarkdown,
}

/// One session as the document renders it: scrubbed prose and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandupSession {
    pub source_hash: String,
    pub archived_at: Option<DateTime<Utc>>,
    /// The branch the summary names, scrubbed and clamped. `None` when it names none.
    pub branch: Option<String>,
    pub title: String,
    pub goal: String,
    pub work_completed: Vec<String>,
    pub decisions: Vec<String>,
    pub open_items: Vec<String>,
}

/// The sessions of one repository, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryGroup {
    /// The repository the summaries themselves named, or [`NO_REPOSITORY`].
    pub repository: String,
    pub sessions: Vec<StandupSession>,
}

/// One line of a cross-window rollup, attributed to the repository it came out of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolledUp {
    pub repository: String,
    pub text: String,
}

/// Why one archived session put nothing in the narrative.
///
/// Distinct from [`SkipReason`], which is the *mirror's* vocabulary: two of these describe a
/// summary that was fetched and then turned out not to be usable, which is a stage the mirror
/// knows nothing about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GapReason {
    /// No snapshot of this session carries a `summary.md` at all.
    MissingSummary,
    /// The summary is munshi's machine-generated placeholder (issue #43): a real one is still
    /// owed, and a later capture replaces it.
    Placeholder,
    /// A `summary.md` this build could not read back as a munshi archive record.
    Unparseable(&'static str),
    /// The archive or the local cache could not be read for this session.
    Unreadable(String),
}

impl GapReason {
    /// The mirror's own verdict, in this lane's vocabulary.
    fn from_skip(reason: &SkipReason) -> Self {
        match reason {
            // The only artifact this lane mirrors is the summary, so that is the only one that can
            // be missing; the variant carries which one anyway, and it is read rather than assumed.
            SkipReason::MissingArtifact(Artifact::Summary) => Self::MissingSummary,
            SkipReason::MissingArtifact(artifact) => {
                Self::Unreadable(format!("no {} artifact", artifact.logical_path()))
            }
            // Unreachable in this lane: the standup mirror never consults an interpreter, because
            // `summary.md` is munshi's own format whatever harness produced the session (see
            // `Artifact::usable`). The mirror's reason type is shared with the transcript lanes, so
            // the case is mapped honestly rather than asserted away.
            SkipReason::UnknownAgent(_) => {
                Self::Unreadable("this build has no interpreter for this harness".to_owned())
            }
            SkipReason::Unreadable(detail) => Self::Unreadable(detail.clone()),
        }
    }

    /// The sentence the Gaps section prints, after the harness label.
    fn sentence(&self) -> String {
        match self {
            Self::MissingSummary => {
                format!("no snapshot of this session carries a `{SUMMARY_LOGICAL_PATH}`")
            }
            Self::Placeholder => {
                "munshi wrote a placeholder summary here and still owes a real one".to_owned()
            }
            Self::Unparseable(detail) => (*detail).to_owned(),
            Self::Unreadable(detail) => detail.clone(),
        }
    }
}

/// One session that contributed nothing, named by what could be named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// The harness the archive named for it. Clamped on the way into a rendered line, never here.
    pub source_agent: String,
    pub reason: GapReason,
}

impl Gap {
    /// A gap taken from the mirror's own skip list.
    pub fn from_skip(skip: &crate::sync::Skip) -> Self {
        Self {
            source_agent: skip.source_agent.clone(),
            reason: GapReason::from_skip(&skip.reason),
        }
    }
}

/// Everything one standup document is rendered from.
#[derive(Debug, Clone, Default)]
pub struct Standup {
    /// Sessions grouped by the repository their own summary names.
    pub repositories: Vec<RepositoryGroup>,
    /// Every decision across the window, in reading order, exact duplicates dropped.
    pub decisions: Vec<RolledUp>,
    /// Every open item across the window, on the same rule.
    pub open_items: Vec<RolledUp>,
    /// Sessions that contributed nothing, grouped by reason.
    pub gaps: Vec<SkippedNote>,
    /// What the scrub fired across every string above. Counts only.
    pub redaction: RedactionReport,
    /// Sessions actually narrated.
    pub sessions: usize,
    /// Decompressed summary bytes read to narrate them.
    pub bytes_read: u64,
}

impl Standup {
    /// Scrubs, groups, orders, and rolls up one window's summaries.
    ///
    /// `unplaceable` is the count of listed sessions the archive dated in a way this build could
    /// not read; they are reported as a gap rather than placed somewhere convenient, on the same
    /// rule the other two lanes apply.
    pub fn fold(
        read: &[ReadSummary],
        gaps: &[Gap],
        unplaceable: usize,
        redactor: &Redactor,
    ) -> Self {
        let mut redaction = RedactionReport::default();
        let mut scrub = |text: &str| {
            let scrubbed = redactor.scrub(text);
            redaction.absorb(&scrubbed.report);
            scrubbed.text
        };

        // Grouped first, ordered second: the group key is the repository the *summary* states,
        // which is a different fact from the repository Patwari projected onto the session row —
        // the summary's is the one written by the capture that produced the prose beside it.
        let mut grouped: BTreeMap<String, Vec<StandupSession>> = BTreeMap::new();
        let mut bytes_read = 0;
        for summary in read {
            let project = &summary.archived.project;
            let repository = project
                .repository
                .as_deref()
                .map(|repository| format::identifier(&scrub(repository)))
                .unwrap_or_else(|| NO_REPOSITORY.to_owned());
            let branch = project
                .branch
                .as_deref()
                .map(|branch| format::identifier(&scrub(branch)));
            let structured = &summary.archived.summary;
            let title = scrub(&structured.title);
            let goal = scrub(&structured.goal);
            let work_completed = structured
                .work_completed
                .iter()
                .map(|item| scrub(item))
                .collect();
            let decisions = structured
                .decisions
                .iter()
                .map(|item| scrub(item))
                .collect();
            let open_items = structured
                .open_items
                .iter()
                .map(|item| scrub(item))
                .collect();
            let session = StandupSession {
                source_hash: summary.source_hash.clone(),
                archived_at: summary.archived_at,
                branch,
                title,
                goal,
                work_completed,
                decisions,
                open_items,
            };
            bytes_read += summary.bytes_read;
            grouped.entry(repository).or_default().push(session);
        }

        let mut repositories: Vec<RepositoryGroup> = grouped
            .into_iter()
            .map(|(repository, mut sessions)| {
                sessions.sort_by(newest_first);
                RepositoryGroup {
                    repository,
                    sessions,
                }
            })
            .collect();
        repositories.sort_by(busiest_first);

        let sessions = repositories
            .iter()
            .map(|group| group.sessions.len())
            .sum::<usize>();
        let decisions = roll_up(&repositories, |session| &session.decisions);
        let open_items = roll_up(&repositories, |session| &session.open_items);

        Self {
            repositories,
            decisions,
            open_items,
            gaps: summarize(gaps, unplaceable),
            redaction,
            sessions,
            bytes_read,
        }
    }

    /// Repositories the window narrated at all.
    pub fn repositories_narrated(&self) -> usize {
        self.repositories.len()
    }
}

/// Newest first, by the clock the window itself was cut on.
///
/// The content hash breaks a tie rather than the input order doing it: two snapshots completing in
/// the same second is not far-fetched on a machine archiving a batch, and a document whose section
/// order depended on which worker thread finished first would not be reproducible. A session the
/// archive dated unreadably sorts last, because "when this happened is unknown" is not a claim
/// that it happened recently.
fn newest_first(left: &StandupSession, right: &StandupSession) -> std::cmp::Ordering {
    right
        .archived_at
        .cmp(&left.archived_at)
        .then_with(|| left.source_hash.cmp(&right.source_hash))
}

/// Busiest repository first, with the unattributed bucket always last.
///
/// A standup is read from the top, and the top is where the week actually went. The unattributed
/// bucket sits at the bottom however many sessions it holds: it is not a place work happened, it
/// is the absence of a place, and letting it head the document would put the least identifiable
/// sessions in front of the most.
fn busiest_first(left: &RepositoryGroup, right: &RepositoryGroup) -> std::cmp::Ordering {
    let unattributed = |group: &RepositoryGroup| group.repository == NO_REPOSITORY;
    unattributed(left)
        .cmp(&unattributed(right))
        .then_with(|| right.sessions.len().cmp(&left.sessions.len()))
        .then_with(|| left.repository.cmp(&right.repository))
}

/// Collects one list across the whole window, in reading order, dropping exact repeats.
///
/// The key is the *rendered* line — the text together with the repository it is attributed to —
/// because that is what a reader would see twice. A decision restated by three sessions in one
/// repository is one decision; the same sentence in two repositories is two facts about two
/// repositories, and merging them would attribute work to a repository it did not happen in.
fn roll_up(
    repositories: &[RepositoryGroup],
    field: impl Fn(&StandupSession) -> &Vec<String>,
) -> Vec<RolledUp> {
    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    for group in repositories {
        for session in &group.sessions {
            for text in field(session) {
                if seen.insert((group.repository.clone(), text.clone())) {
                    lines.push(RolledUp {
                        repository: group.repository.clone(),
                        text: text.clone(),
                    });
                }
            }
        }
    }
    lines
}

/// Groups gaps by reason so a systematic one reads as a single line.
///
/// The harness label is the archive's string and goes through [`format::identifier`] on the way
/// in, for exactly the reason [`crate::command`] clamps it in the other two lanes: a manifest
/// states whatever `source_agent` it likes, and this line is rendered verbatim.
fn summarize(gaps: &[Gap], unplaceable: usize) -> Vec<SkippedNote> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for gap in gaps {
        let reason = format!(
            "{}: {}",
            format::identifier(&gap.source_agent),
            gap.reason.sentence()
        );
        *counts.entry(reason).or_default() += 1;
    }
    let mut notes: Vec<_> = counts
        .into_iter()
        .map(|(reason, count)| SkippedNote { count, reason })
        .collect();
    if unplaceable > 0 {
        notes.push(SkippedNote {
            count: unplaceable,
            reason: "archived at a time this build could not place in the window".to_owned(),
        });
    }
    notes
}

/// Reads one session's cached `summary.md` and parses it, or says why it will not be narrated.
///
/// # Errors
///
/// Returns the [`GapReason`] this session appears in the document under: an unreadable cache, a
/// summary that is not valid UTF-8, a record the parser refuses, or munshi's placeholder.
pub fn read_summary(
    cache: &BlobCache,
    mirrored: &MirroredSession,
) -> Result<ReadSummary, GapReason> {
    // Read whole rather than streamed, unlike every transcript path in this crate. That is safe
    // *because* of the ceiling the download enforced: nothing reaches the cache under a summary's
    // digest that the archive did not declare as under a megabyte, so the largest string this can
    // produce is bounded by a constant rather than by how long somebody's session was.
    let mut markdown = Vec::new();
    cache
        .open_blob(&mirrored.source_hash)
        .and_then(|mut blob| blob.read_to_end(&mut markdown))
        .map_err(|error| GapReason::Unreadable(format!("cache read failed: {error}")))?;
    let markdown = String::from_utf8(markdown)
        .map_err(|_| GapReason::Unparseable("this session's `summary.md` is not valid UTF-8"))?;
    let archived = parse_archive_markdown(&markdown).map_err(unparseable)?;
    // Read from the parsed record rather than re-derived from the tags: `summary_placeholder` is
    // the explicit frontmatter flag, already widened by the parser to fall back to the tag, so it
    // is the broader of the two answers and the one munshi itself considers authoritative.
    if archived.summary_placeholder {
        return Err(GapReason::Placeholder);
    }
    Ok(ReadSummary {
        source_hash: mirrored.source_hash.clone(),
        archived_at: mirrored.archived_at,
        // The archive's declared original size, already verified against the transferred bytes, so
        // the footer needs no second pass over the file to count what it read.
        bytes_read: mirrored.size_bytes,
        archived,
    })
}

/// Why the parser refused a `summary.md`, in a sentence a Gaps line can carry.
///
/// [`RenderError`] is the one error type munshi's archive module has always returned in *both*
/// directions, so it still carries the two variants only its writer can produce. Parsing reaches
/// exactly `InvalidArchive` and `InvalidSummary`; the other two are matched to keep this total
/// without claiming they can occur, and deliberately share the vaguest sentence rather than being
/// given confident wording for a state that never happens.
fn unparseable(error: RenderError) -> GapReason {
    GapReason::Unparseable(match error {
        RenderError::InvalidArchive => {
            "this session's `summary.md` is not a munshi archive record this build can read"
        }
        RenderError::InvalidSummary => {
            "this session's `summary.md` carries a summary munshi's own validation refuses"
        }
        RenderError::InvalidPath | RenderError::Io(_) => {
            "this session's `summary.md` could not be parsed"
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn session(hash: &str, archived_at: Option<&str>) -> StandupSession {
        StandupSession {
            source_hash: hash.repeat(64),
            archived_at: archived_at.map(at),
            branch: None,
            title: "t".to_owned(),
            goal: "g".to_owned(),
            work_completed: Vec::new(),
            decisions: Vec::new(),
            open_items: Vec::new(),
        }
    }

    #[test]
    fn sessions_order_newest_first_and_break_ties_reproducibly() {
        let mut sessions = [
            session("b", Some("2026-08-20T00:00:00Z")),
            session("c", None),
            session("a", Some("2026-08-20T00:00:00Z")),
            session("d", Some("2026-08-22T00:00:00Z")),
        ];
        sessions.sort_by(newest_first);
        let order: Vec<_> = sessions
            .iter()
            .map(|session| session.source_hash.chars().next().unwrap())
            .collect();
        assert_eq!(order, ['d', 'a', 'b', 'c']);
    }

    /// The unattributed bucket is last however busy it is: it is the absence of a place, not a
    /// place with a lot going on.
    #[test]
    fn repositories_order_busiest_first_with_the_unattributed_bucket_last() {
        let group = |repository: &str, count: usize| RepositoryGroup {
            repository: repository.to_owned(),
            sessions: (0..count).map(|_| session("a", None)).collect(),
        };
        let mut groups = [
            group(NO_REPOSITORY, 9),
            group("surdy/munshi", 1),
            group("surdy/akit", 1),
            group("surdy/qanungo", 4),
        ];
        groups.sort_by(busiest_first);
        let order: Vec<_> = groups
            .iter()
            .map(|group| group.repository.as_str())
            .collect();
        assert_eq!(
            order,
            ["surdy/qanungo", "surdy/akit", "surdy/munshi", NO_REPOSITORY],
        );
    }

    /// The mirror can only ever tell this lane two things, and one of them is that no snapshot
    /// carried a summary.
    #[test]
    fn a_missing_summary_reads_as_a_missing_summary() {
        assert_eq!(
            GapReason::from_skip(&SkipReason::MissingArtifact(Artifact::Summary)),
            GapReason::MissingSummary,
        );
        assert_eq!(
            GapReason::from_skip(&SkipReason::Unreadable("cache read failed".to_owned())),
            GapReason::Unreadable("cache read failed".to_owned()),
        );
    }

    /// A hostile harness label reaches a Gaps line, exactly as it does in the other two lanes, so
    /// it is clamped wholesale rather than truncated.
    #[test]
    fn a_hostile_harness_label_cannot_break_out_of_a_gaps_line() {
        let notes = summarize(
            &[
                Gap {
                    source_agent: "claude-code | evil".to_owned(),
                    reason: GapReason::MissingSummary,
                },
                Gap {
                    source_agent: "back`tick".to_owned(),
                    reason: GapReason::Placeholder,
                },
            ],
            0,
        );
        let rendered = notes
            .iter()
            .map(|note| note.reason.clone())
            .collect::<Vec<_>>()
            .join("\n");
        for hostile in ["claude-code | evil", "back`tick"] {
            assert!(!rendered.contains(hostile), "{hostile:?} survived");
        }
        assert_eq!(rendered.matches(format::INVALID_IDENTIFIER).count(), 2);
    }

    #[test]
    fn unplaceable_sessions_become_their_own_gap_line() {
        let notes = summarize(&[], 3);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].count, 3);
        assert!(notes[0].reason.contains("could not place in the window"));
    }
}
