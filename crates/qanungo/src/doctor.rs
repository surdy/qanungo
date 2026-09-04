//! The doctor fold: instructions this archive shows you giving more than once.
//!
//! `report` counts what tools did, `cost` counts what messages cost, `standup` narrates what munshi
//! wrote, and `ask` ranks summaries against a question. This lane reads the one surface none of them
//! reason about — the text a *person* typed — and answers a single question over it: **which
//! instructions have you had to repeat across sessions of the same repository?**
//!
//! That is qanungo #11's read-only half. The interpretation half is deliberately somewhere else: the
//! `instructions-editor` contrib skill runs in the harness, inside the repository, under the user's
//! own permission prompts, and it is the thing that opens `CLAUDE.md` and proposes a diff. This
//! module computes; the harness edits.
//!
//! # What this can see, and what it cannot
//!
//! It reads transcripts. It has **no idea what your instruction files say** — qanungo never opens a
//! checkout, and the archive does not record one. So the finding here is *repetition*, and the whole
//! discipline of the lane is refusing to dress that up as *causation*. A cluster below says "you
//! typed nearly this, in these sessions of this repository". It does not say a missing instruction
//! caused anything, because nothing this fold reads could support that claim. Whether the repeated
//! text belongs in an instruction file is a judgement, and a judgement is not a fold.
//!
//! The within-session [`Friction`] counts sit under the clusters for the same reason they are only
//! *corroboration*: a message that arrives right after a tool failed is a proxy for friction, and
//! friction has many causes besides an instruction nobody wrote down. They are aggregates, never
//! excerpts, and they are never attributed to a cluster.
//!
//! # The detection scheme, and why this one
//!
//! Per repository, over every [`Event::User`] the transcripts of that repository carried:
//!
//! 0. **Keep only what a person typed** ([`authored`]) — `Event::User` is the *user surface*, not
//!    the user: every harness writes its own machinery into it, from pasted-image placeholders to
//!    slash commands to whole skill bodies, and all of it is byte-identical between sessions. It is
//!    excluded against a certified list of openings, and counted rather than dropped in silence.
//! 1. **Normalize** ([`normalize`]) — lower-case, drop punctuation, join the words with single
//!    spaces. "Always run `cargo fmt` first!" and "always run cargo fmt first" are the same
//!    instruction typed twice, and a comparison that disagreed about that would find nothing.
//! 2. **Shingle** ([`SHINGLE_WORDS`]-word phrases, [`shingles`]) — the standard near-duplicate
//!    primitive. Word *sets* would call two messages about the same nouns identical; whole-message
//!    equality would miss the same instruction retyped with one word changed. Phrases sit between
//!    the two, and a phrase is also what an instruction reads as.
//! 3. **Compare through an inverted index** — [`cluster`] maps each distinct phrase to the
//!    messages carrying it, so a message is only ever compared against messages it shares a phrase
//!    with. The all-pairs alternative is quadratic in the messages of a repository, which the
//!    archive is already large enough to make untenable.
//! 4. **Cluster** — single-linkage over the pairs that cross [`SIMILARITY_THRESHOLD_PERCENT`],
//!    joined with a union-find whose root is always the smaller index, so the grouping cannot depend
//!    on the order pairs were discovered in.
//! 5. **Join the sessions that are one conversation** ([`conversations`]) — a resumed or re-captured
//!    session replays the earlier one, so two session ids can hold the same conversation. Counting
//!    those as two sessions would report one long conversation as a page of repetitions, so they are
//!    merged before a cluster's session span is counted.
//!
//! Steps 0 and 5 are not decoration: both were forced by the first run against the production
//! archive, where without them injected boilerplate and one replayed conversation were most of what
//! the document said. The measurements are in each constant's own rustdoc.
//!
//! ## The one place this is bounded rather than exact
//!
//! A phrase carried by *hundreds* of a repository's messages ("i want you to") discriminates
//! nothing, and expanding it into pairs is what makes an inverted index degenerate back into the
//! quadratic scan it exists to avoid. So a phrase held by more than [`MAX_SHINGLE_POSTINGS`]
//! messages is skipped when candidates are gathered.
//!
//! The direction of that error is the point: a skipped phrase can only ever *lower* a pair's
//! measured overlap, so the bound can hide a repetition and can never invent one. Everything this
//! document reports is a pair that cleared the threshold on phrases the index actually counted —
//! there is no approximate similarity anywhere, and no hashing that could collide two different
//! phrases into one. What is bounded is which pairs are looked at, not what is true of the pairs
//! that are.
//!
//! # Clusters cross sessions; repetition inside one session does not count
//!
//! A cluster must span at least [`MIN_CLUSTER_SESSIONS`] distinct sessions of one repository.
//! Restating something three times inside a single session is a conversation, not an instruction
//! the machine keeps forgetting between sittings — that is what the friction counts are for.
//!
//! And a repository is a hard boundary: the index is built per repository, so a message in one
//! repository can never be compared against a message in another. An instruction missing from
//! repository A's `CLAUDE.md` is repository A's business, and merging the two would produce a
//! finding nobody can act on in either place.
//!
//! # The scrub happens here
//!
//! This is qanungo #8's fourth consumer and the CLI's second verbatim surface, after
//! `ask --verbatim`. A cluster renders an excerpt of the repeated instruction, which is transcript
//! text somebody typed, so:
//!
//! - **Clustering runs on the unscrubbed text.** A secret must not change what clusters, exactly as
//!   it must not change what matches in [`crate::verbatim`]: replacing a token before the comparison
//!   would let a credential quietly split a cluster in two, and the count beside the excerpt would
//!   then be wrong in a way no reader could see.
//! - **The excerpt is scrubbed on the way into the [`Cluster`]**, through
//!   [`crate::ask::snippet`]'s own pipeline — scrub, then collapse, then clip. Clipping first could
//!   cut a credential in half and render the surviving head.
//! - Only the excerpts that are actually *rendered* are scrubbed, so the counts in the footer
//!   describe the document rather than the corpus behind it — the rule [`crate::ask`] already holds.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

use chrono::{DateTime, Utc};
use munshi_transcript::{Classification, Event, Source, TranscriptStream, UnsupportedVersion};

use crate::ask::snippet;
use crate::format;
use crate::metrics::outcome;
use crate::redaction::{RedactionReport, Redactor};
use crate::report::SkippedNote;
use crate::standup::NO_REPOSITORY;

/// Words in one shingle — the phrase length two messages are compared on.
///
/// **A tunable, not a decision.** Four is long enough that an ordinary pair of English messages does
/// not share phrases by accident and short enough that an instruction retyped with a word moved
/// still shares most of its phrases with itself. Raising it makes the lane insist on longer verbatim
/// agreement; lowering it starts clustering messages that merely share vocabulary.
pub const SHINGLE_WORDS: usize = 4;

/// Shortest message, in words, this lane will compare at all.
///
/// **A tunable, not a decision**, and the one that keeps the clusters meaningful. "yes", "do it",
/// "continue", "thanks" and "now the tests" are the most repeated things anybody types at a coding
/// agent, and every one of them would cluster perfectly and mean nothing. An instruction worth
/// putting in a `CLAUDE.md` is a sentence; the floor is set where a sentence starts.
///
/// A message under the floor is *counted* ([`SessionMessages::messages`] against
/// [`SessionMessages::clusterable`]) rather than passed over in silence, so a reader can see how
/// much of the conversation this lane declined to look at.
pub const MIN_CLUSTERABLE_WORDS: usize = 8;

/// How much of the shorter message's phrases the two have to share, in percent, to be called the
/// same instruction.
///
/// **A tunable, not a decision.** Stated in percent and compared with integer arithmetic on purpose:
/// a threshold on a float is a threshold whose behaviour at the boundary depends on rounding, and
/// this lane's output has to be reproducible run to run.
///
/// The denominator is the *shorter* message's phrase count — containment rather than Jaccard —
/// because the shape this lane is looking for is an instruction restated inside a longer message. A
/// two-line rule repeated verbatim at the top of a long request is exactly the repetition worth
/// showing, and Jaccard would score it near zero for the crime of being surrounded by new context.
pub const SIMILARITY_THRESHOLD_PERCENT: usize = 60;

/// Distinct sessions a cluster has to span before it is reported.
///
/// **A tunable, not a decision** — but only in one direction. Two is the floor the lane's whole
/// claim rests on: repetition *within* one session is a conversation and belongs to the friction
/// counts, so a "cluster" confined to one session is not a finding at all. Raising it would trade
/// noise for silence about genuine pairs; it is left at the floor so nothing real is hidden.
pub const MIN_CLUSTER_SESSIONS: usize = 2;

/// Sessions a repository needs in the reach before this lane looks at it for clusters.
///
/// **Derived, not arbitrary.** A cluster must span [`MIN_CLUSTER_SESSIONS`] sessions, so a
/// repository with fewer than that cannot produce one however hard it is looked at. The constant is
/// named rather than implied so the document can say *which* repositories fell under it — a
/// repository nothing was reported for because nothing could be reported for it is listed as
/// unexamined ([`Doctor::unexamined`]) rather than quietly absent.
pub const MIN_REPOSITORY_SESSIONS: usize = MIN_CLUSTER_SESSIONS;

/// How much of the smaller session's instructions two sessions have to share before they are read
/// as **one conversation captured twice** rather than as two.
///
/// **A tunable, not a decision — and the constant a first run against the production archive
/// forced.** The archive holds pairs of sessions, seconds apart, whose clusterable messages are the
/// same list at the same event ordinals: a conversation resumed, or re-captured, so the second
/// transcript replays the first. Counting those as two sessions turns every message of one long
/// conversation into a "repeated instruction", which is the largest false finding this lane can
/// make — the whole of one repository's section was that on the first run.
///
/// The separation measured on the real archive is not close: two genuinely distinct sessions that
/// repeat one instruction share a few percent of their instructions, and a replayed conversation
/// shares nearly all of them. Fifty sits in the wide gap between the two.
pub const SAME_CONVERSATION_PERCENT: usize = 50;

/// Instructions two sessions have to share before [`SAME_CONVERSATION_PERCENT`] is even consulted.
///
/// **A tunable, not a decision**, and the floor that keeps the conversation rule from eating the
/// finding it exists to protect. A percentage over a tiny denominator says nothing: two short
/// sessions that share the *one* instruction this lane is looking for share 100% of the smaller
/// one, and merging them would delete exactly the finding. A replayed transcript, by contrast,
/// brings the whole earlier conversation with it — the pair the production archive turned up shared
/// thirty-nine. Three is set well below that and well above one.
pub const MIN_SHARED_INSTRUCTIONS: usize = 3;

/// Messages a phrase may appear in before it is skipped for candidate gathering.
///
/// **A tunable, not a decision.** See the module docs for the honesty argument: skipping a phrase
/// can only lower a measured overlap, so this bounds the *work*, never the truth of what is
/// reported. Two hundred is generous — a phrase that common in one repository is boilerplate, and
/// expanding it into twenty thousand pairs buys nothing but time.
pub const MAX_SHINGLE_POSTINGS: usize = 200;

/// Occurrences one cluster cites before the list is cut short.
///
/// A tunable, on [`crate::verbatim::MAX_MATCHES_PER_SESSION`]'s reasoning: enough citations to go
/// and look, few enough that a cluster stays a paragraph. The total travels beside them
/// ([`Cluster::occurrences`]) so a cut list is never mistaken for the whole of what was found.
pub const MAX_CITATIONS_PER_CLUSTER: usize = 8;

/// Clusters one repository renders before the list is cut short, when nobody says otherwise.
///
/// A tunable, for the same reason and with the same discipline: [`RepositoryClusters::found`] counts
/// them all, so the document can say how many it is not showing.
///
/// **A default, not a ceiling** (qanungo #16). The reader of this document is often the
/// `instructions-editor` skill, which acts on clusters in the weight class this cut lands in — the
/// first production run hid two 2-occurrence clusters behind the "not shown" line while a
/// 2-occurrence cluster above it produced a shipped instruction-file edit. So `--clusters-per-repo`
/// raises it, [`Doctor::fold`] takes the effective value as an argument, and the document states it
/// whenever it is not this number.
pub const DEFAULT_CLUSTERS_PER_REPOSITORY: usize = 10;

/// Openings this build has certified, against the production archive, as text the **harness** put
/// on the user surface rather than text a person typed.
///
/// # Why this list exists at all
///
/// `Event::User` is not "what somebody typed". Every harness in the archive writes its own
/// machinery into the user role: a pasted image becomes `[Image: original 2400x1080, …]`, a slash
/// command becomes `<command-name>/model</command-name>…`, shell mode echoes `<bash-input>` and
/// `<bash-stdout>`, a finished background task arrives as `<task-notification>`, a skill is loaded
/// by prefixing its whole body, a compaction opens the next turn with a summary of the last one,
/// and munshi's own summarizer sends the archive its JSON instruction. Every one of those is
/// *byte-identical across sessions*, so every one of them clusters perfectly — and none of them is
/// an instruction anybody had to give twice.
///
/// The first run against the production archive is what settled this: without the list, injected
/// boilerplate was the representative of most of the clusters found, and the genuine findings sat
/// underneath it. Excluding it is the honest fix, and it is a better one than raising the
/// similarity threshold would have been — the boilerplate is a *perfect* match, so no threshold
/// short of impossible excludes it, and raising one would only have hidden real repetition too.
///
/// # It is incomplete by construction, and says so
///
/// A harness can inject anything, at any version, and this build learns of a new shape only by
/// somebody looking. So this is a floor on the noise rather than a proof of its absence: text a
/// harness injects in a shape not listed here still reaches the clustering, and a reader who sees a
/// cluster that is obviously machinery has found the next entry rather than a contradiction. What
/// the list removes is *counted* ([`SessionMessages::harness_generated`]) and stated in the
/// document, so its size is never a silent subtraction.
///
/// Matching is on the message's **opening**, after trimming leading whitespace, because that is
/// where every observed shape announces itself and because a substring test would exclude a real
/// instruction that happened to quote one of these markers back.
const HARNESS_PREFIXES: &[&str] = &[
    // Claude Code: shell mode, local slash commands, skills, background tasks, pasted images.
    "<bash-input>",
    "<bash-stdout>",
    "<bash-stderr>",
    "<command-name>",
    "<command-message>",
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<task-notification>",
    "<system-reminder>",
    "[Image:",
    "Caveat: The messages below were generated by the user while running local commands",
    "Base directory for this skill:",
    "This session is being continued from a previous conversation",
    "## Context Usage",
    "Your claude.ai usage limit has reset",
    // Copilot: skill preambles and the rename request it opens a session with.
    "<skill-context",
    "<session_rename_request>",
    // munshi's own summarizer, talking to the harness through the user surface.
    "{\"instruction\":",
];

/// Whether a user message is text a person typed, rather than something the harness injected.
///
/// See [`HARNESS_PREFIXES`] for the argument and for the honest bound on it.
pub fn authored(text: &str) -> bool {
    let opening = text.trim_start();
    !HARNESS_PREFIXES
        .iter()
        .any(|prefix| opening.starts_with(prefix))
}

/// One user message, as the fold holds it before anything is scrubbed.
///
/// Only messages that cleared [`MIN_CLUSTERABLE_WORDS`] are built at all: the rest are counted and
/// dropped, which is what keeps the memory this lane holds proportional to the instructions in the
/// archive rather than to every "yes" in it.
#[derive(Debug, Clone)]
pub struct Instruction {
    /// 1-based ordinal among the transcript's **events**, in file order.
    ///
    /// Counted exactly as [`crate::verbatim`] counts it — every typed event, user, assistant, and
    /// tool alike — so a locator printed here is the same coordinate that lane prints for the same
    /// event of the same transcript. A reader who takes one to their own copy of the file finds the
    /// same line either way, and `tests` pins the two against one fixture rather than leaving the
    /// agreement to a comment.
    pub locator: u64,
    /// The message as the transcript holds it. **Unscrubbed** — this is what the clustering compares
    /// and it is never rendered; only a representative's scrubbed excerpt is.
    pub text: String,
    /// [`normalize`]d form: lower-case words joined by single spaces. What [`shingles`] cuts.
    pub normalized: String,
}

/// What one transcript contributed.
#[derive(Debug, Clone, Default)]
pub struct SessionMessages {
    /// The messages long enough to be compared, in file order.
    pub clusterable: Vec<Instruction>,
    /// Every [`Event::User`] the transcript carried, clusterable or not. The honest denominator for
    /// everything below it.
    pub messages: usize,
    /// Messages the harness wrote onto the user surface rather than a person — an image
    /// placeholder, a slash command, a skill body, a task notification. Excluded from the
    /// comparison and counted here rather than dropped in silence. See [`HARNESS_PREFIXES`].
    pub harness_generated: usize,
    /// Messages that arrived while the last outcome a tool reported was a failure — the
    /// within-session friction proxy. See [`Friction`] for what it does and does not mean.
    pub after_error: usize,
    /// Records `munshi-transcript` could not read at all. They carry no typed text and so were not
    /// read; counted rather than passed over in silence.
    pub unreadable_records: u64,
}

impl SessionMessages {
    /// Messages a person actually typed — every user message less the machinery. The denominator
    /// the friction rate is stated against, because a rate over injected boilerplate would measure
    /// how often the harness talks rather than how often the work went sideways.
    pub fn authored(&self) -> usize {
        self.messages - self.harness_generated
    }
}

/// One session's messages together with the archive identity a citation is made of.
#[derive(Debug, Clone)]
pub struct DoctorSession {
    /// The transcript's content hash — the citation a reader redeems, as in every other lane.
    pub source_hash: String,
    /// When the archive finished the snapshot this session was listed by. Archive time, the clock
    /// the window (if any) was cut on.
    pub archived_at: Option<DateTime<Utc>>,
    /// The repository the *listing* recorded for this session, unscrubbed and unclamped. `None` is a
    /// real state — a session captured outside a checkout — and is not a repository.
    pub repository: Option<String>,
    /// Decompressed transcript bytes the fold read.
    pub bytes_folded: u64,
    pub messages: SessionMessages,
}

/// One occurrence of a repeated instruction, as the document cites it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub archived_at: Option<DateTime<Utc>>,
    pub source_hash: String,
    /// The event ordinal within that transcript. See [`Instruction::locator`].
    pub locator: u64,
}

/// One instruction this repository's sessions gave more than once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    /// Messages in the cluster, across every session it spans.
    pub occurrences: usize,
    /// Distinct *conversations* it spans — sessions that replay one another count once. At least
    /// [`MIN_CLUSTER_SESSIONS`], by construction. See [`SAME_CONVERSATION_PERCENT`].
    pub sessions: usize,
    /// A scrubbed excerpt of the fullest occurrence — see [`most_representative`] for which one and
    /// why.
    pub excerpt: String,
    /// Where the occurrences were, newest first, at most [`MAX_CITATIONS_PER_CLUSTER`] of them.
    pub citations: Vec<Citation>,
}

/// The clusters of one repository, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryClusters {
    /// The repository the archive listed these sessions under, clamped and then scrubbed.
    pub repository: String,
    /// The clusters this document renders, at most the cap [`Doctor::fold`] was given —
    /// [`DEFAULT_CLUSTERS_PER_REPOSITORY`] unless `--clusters-per-repo` said otherwise.
    pub clusters: Vec<Cluster>,
    /// How many were found before that cut, so a truncated section can say so.
    pub found: usize,
    /// Occurrences across every cluster found — what orders the repositories.
    pub occurrences: usize,
}

/// One repository's within-session friction, in aggregate.
///
/// **Corroboration, not a finding.** A message arriving while the last thing a tool reported was a
/// failure is the cheapest honest proxy for "that did not go as expected, let me say more", and it
/// is nothing better than that: a failing test the session was written to chase produces the same
/// shape as an instruction nobody wrote down. It carries counts and never an excerpt, and it is
/// never attributed to a cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Friction {
    /// The repository, clamped and then scrubbed, or [`NO_REPOSITORY`].
    pub repository: String,
    pub sessions: usize,
    /// Every user message these sessions carried.
    pub messages: usize,
    /// How many of them followed a failing tool event.
    pub after_error: usize,
}

/// Why a repository contributed no clusters — stated, never omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unexamined {
    /// The repository, clamped and then scrubbed, or [`NO_REPOSITORY`].
    pub repository: String,
    pub sessions: usize,
    /// This build's own sentence, never archive text.
    pub reason: &'static str,
}

/// The sentence a repository under [`MIN_REPOSITORY_SESSIONS`] is listed with.
const TOO_FEW_SESSIONS: &str = "fewer sessions in this reach than a cross-session cluster needs";

/// The sentence the unattributed bucket is listed with.
const NOT_A_REPOSITORY: &str = "the archive attributes these sessions to no repository, and an instruction file belongs to one";

/// Everything one doctor document is rendered from.
#[derive(Debug, Clone, Default)]
pub struct Doctor {
    /// Repositories with at least one reported cluster, busiest first.
    pub repositories: Vec<RepositoryClusters>,
    /// Every repository the fold saw, including the ones that cluster nothing.
    pub friction: Vec<Friction>,
    /// Repositories examined for friction but not for clusters, with the reason.
    pub unexamined: Vec<Unexamined>,
    /// Sessions that contributed nothing, grouped by reason. Built by the caller, which is the only
    /// thing that knows what the mirror skipped.
    pub gaps: Vec<SkippedNote>,
    /// What the scrub fired across every string above. Counts only.
    pub redaction: RedactionReport,
    /// Sessions whose transcript was read.
    pub sessions: usize,
    /// Repositories whose messages were actually compared.
    pub repositories_examined: usize,
    /// Every user message read, across every session.
    pub messages: usize,
    /// How many of them the harness wrote rather than a person, and this build recognized as such.
    pub harness_generated: usize,
    /// How many of them were long enough, and authored, to compare.
    pub clusterable: usize,
    /// Distinct conversations those sessions turned out to be, across every repository examined —
    /// two sessions that replay the same conversation count once. See [`SAME_CONVERSATION_PERCENT`].
    pub conversations: usize,
    /// Sessions that carried no user message at all — nothing for this lane to look at, counted
    /// rather than silently contributing an empty result.
    pub sessions_without_messages: usize,
    /// Clusters found across every repository, before any per-repository cut.
    pub clusters: usize,
    /// Decompressed transcript bytes read.
    pub bytes_folded: u64,
    /// Records `munshi-transcript` could not read, across every session.
    pub unreadable_records: u64,
}

impl Doctor {
    /// Clusters one reach's user messages per repository, and counts the friction beside them.
    ///
    /// `gaps` and `gap_redaction` come from the caller: summarizing what the *mirror* skipped needs
    /// the mirror's own vocabulary, which lives with the commands, and the scrub that summary ran on
    /// each harness label has to reach this document's footer — a marker in the Gaps section under
    /// "redaction none" would be the document contradicting itself.
    ///
    /// `clusters_per_repo` is the rendering cut, [`DEFAULT_CLUSTERS_PER_REPOSITORY`] unless
    /// `--clusters-per-repo` raised it. It bounds what each section *shows* and nothing else: every
    /// count on this struct — [`Doctor::clusters`], [`RepositoryClusters::found`],
    /// [`RepositoryClusters::occurrences`] — is taken before the cut, so raising it reveals clusters
    /// without moving a single number.
    pub fn fold(
        sessions: &[DoctorSession],
        gaps: Vec<SkippedNote>,
        gap_redaction: &RedactionReport,
        redactor: &Redactor,
        clusters_per_repo: usize,
    ) -> Self {
        let mut redaction = RedactionReport::default();
        redaction.absorb(gap_redaction);
        let mut counted = Self {
            gaps,
            sessions: sessions.len(),
            ..Self::default()
        };

        // Grouped on the listing's own repository string, unscrubbed: the group key decides what is
        // compared against what, and two different repositories that happened to scrub to the same
        // marker must not be merged into one finding. The label is transformed once, below, where it
        // is about to be rendered.
        let mut grouped: BTreeMap<Option<&str>, Vec<&DoctorSession>> = BTreeMap::new();
        for session in sessions {
            counted.messages += session.messages.messages;
            counted.clusterable += session.messages.clusterable.len();
            counted.harness_generated += session.messages.harness_generated;
            counted.bytes_folded += session.bytes_folded;
            counted.unreadable_records += session.messages.unreadable_records;
            if session.messages.messages == 0 {
                counted.sessions_without_messages += 1;
            }
            grouped
                .entry(session.repository.as_deref())
                .or_default()
                .push(session);
        }

        for (repository, held) in grouped {
            // Clamp, then scrub. The repository here is lifted off a *listing row*, which is the
            // case [`crate::evidence::identifier_field`] argues the order for: the clamp has to
            // judge the archive's own bytes, or an over-length token would launder itself into a
            // renderable marker.
            let named = repository.map(|value| {
                let scrubbed = redactor.scrub(&format::identifier(value));
                redaction.absorb(&scrubbed.report);
                scrubbed.text
            });
            let label = named.clone().unwrap_or_else(|| NO_REPOSITORY.to_owned());
            counted.friction.push(Friction {
                repository: label.clone(),
                sessions: held.len(),
                messages: held.iter().map(|held| held.messages.authored()).sum(),
                after_error: held.iter().map(|held| held.messages.after_error).sum(),
            });
            let unexamined = match &named {
                None => Some(NOT_A_REPOSITORY),
                Some(_) if held.len() < MIN_REPOSITORY_SESSIONS => Some(TOO_FEW_SESSIONS),
                Some(_) => None,
            };
            if let Some(reason) = unexamined {
                counted.unexamined.push(Unexamined {
                    repository: label,
                    sessions: held.len(),
                    reason,
                });
                continue;
            }
            counted.repositories_examined += 1;
            let (found, conversations) = cluster(&held);
            counted.clusters += found.len();
            counted.conversations += conversations;
            if found.is_empty() {
                continue;
            }
            counted.repositories.push(build_section(
                label,
                found,
                redactor,
                &mut redaction,
                clusters_per_repo,
            ));
        }

        counted.repositories.sort_by(most_repeated_first);
        counted.friction.sort_by(most_friction_first);
        counted.unexamined.sort_by(|left, right| {
            right
                .sessions
                .cmp(&left.sessions)
                .then_with(|| left.repository.cmp(&right.repository))
        });
        counted.redaction = redaction;
        counted
    }

    /// Whether any repetition cleared the thresholds at all.
    pub fn is_empty(&self) -> bool {
        self.repositories.is_empty()
    }
}

/// Builds one repository's rendered section: order the clusters, cut the list, scrub what survives.
///
/// The scrub runs *after* the cut, on the excerpts that are actually going to be rendered — the rule
/// [`crate::ask`] holds for the same reason. A footer that counted replacements in text nobody sees
/// would describe the corpus rather than the document.
fn build_section(
    repository: String,
    mut found: Vec<Repetition>,
    redactor: &Redactor,
    redaction: &mut RedactionReport,
    clusters_per_repo: usize,
) -> RepositoryClusters {
    found.sort_by(most_occurrences_first);
    let occurrences = found.iter().map(|cluster| cluster.occurrences.len()).sum();
    RepositoryClusters {
        repository,
        found: found.len(),
        occurrences,
        clusters: found
            .into_iter()
            .take(clusters_per_repo)
            .map(|repetition| {
                // Scrub, then collapse, then clip — [`crate::ask::snippet`]'s pipeline, so every
                // quotation this crate prints is cut at one length by one rule and none of them is
                // cut before it is scrubbed.
                let scrubbed = redactor.scrub(&repetition.representative);
                redaction.absorb(&scrubbed.report);
                Cluster {
                    occurrences: repetition.occurrences.len(),
                    sessions: repetition.sessions,
                    excerpt: snippet(&scrubbed.text),
                    citations: repetition
                        .occurrences
                        .into_iter()
                        .take(MAX_CITATIONS_PER_CLUSTER)
                        .collect(),
                }
            })
            .collect(),
    }
}

/// One cluster before it is rendered: still carrying the representative's unscrubbed text.
struct Repetition {
    /// Every occurrence, newest first.
    occurrences: Vec<Citation>,
    sessions: usize,
    /// The fullest occurrence's own text, unscrubbed. Scrubbed by [`build_section`] on the way into
    /// the excerpt, and never stored anywhere else.
    representative: String,
}

/// Clusters one repository's clusterable messages.
///
/// Every message of every session in one flat list, an inverted index over their phrases, one pass
/// of candidate comparison per message, and a union-find over the pairs that cleared the threshold.
/// See the module docs for why each step is the step it is.
fn cluster(sessions: &[&DoctorSession]) -> (Vec<Repetition>, usize) {
    let occurrences: Vec<Occurrence<'_>> = sessions
        .iter()
        .enumerate()
        .flat_map(|(session, held)| {
            held.messages
                .clusterable
                .iter()
                .map(move |instruction| Occurrence {
                    session,
                    archived_at: held.archived_at,
                    source_hash: &held.source_hash,
                    instruction,
                })
        })
        .collect();
    if occurrences.len() < MIN_CLUSTER_SESSIONS {
        return (Vec::new(), sessions.len());
    }

    let phrases: Vec<BTreeSet<&str>> = occurrences
        .iter()
        .map(|occurrence| shingles(&occurrence.instruction.normalized))
        .collect();
    let mut index: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (position, held) in phrases.iter().enumerate() {
        for phrase in held {
            index.entry(phrase).or_default().push(position);
        }
    }

    let mut groups = Groups::new(occurrences.len());
    for (position, held) in phrases.iter().enumerate() {
        // Candidates are gathered against *earlier* messages only: every pair is then considered
        // exactly once, and the map is one message's worth of counts rather than the whole
        // repository's worth of pairs.
        let mut shared: BTreeMap<usize, usize> = BTreeMap::new();
        for phrase in held {
            let postings = &index[phrase];
            if postings.len() > MAX_SHINGLE_POSTINGS {
                continue;
            }
            for &candidate in postings {
                if candidate < position {
                    *shared.entry(candidate).or_default() += 1;
                }
            }
        }
        for (candidate, count) in shared {
            if alike(count, held.len(), phrases[candidate].len()) {
                groups.join(position, candidate);
            }
        }
    }

    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for position in 0..occurrences.len() {
        members
            .entry(groups.root(position))
            .or_default()
            .push(position);
    }
    let members: Vec<Vec<usize>> = members.into_values().collect();
    let mut conversations = conversations(sessions, &occurrences, &members);
    let distinct = (0..sessions.len())
        .map(|session| conversations.root(session))
        .collect::<BTreeSet<_>>()
        .len();
    let found = members
        .into_iter()
        .filter_map(|positions| repetition(&occurrences, &positions, &mut conversations))
        .collect();
    (found, distinct)
}

/// Joins the sessions that are one conversation captured twice.
///
/// # The finding this exists to refuse
///
/// A session resumed — or re-captured — carries the earlier conversation with it, so the archive
/// holds pairs of session ids whose user messages are the same list in the same order. Every one of
/// those messages then looks like an instruction given in two sessions, and a whole repository's
/// section becomes one long conversation reported back as forty separate repetitions. That is not a
/// threshold that needs raising: the messages really are identical, and no similarity rule can tell
/// a replay from a repetition by looking at *one pair of messages*. It has to be answered at the
/// level of the pair of **sessions**.
///
/// # How it is answered
///
/// Reusing the clustering that has already run: two sessions that appear together in a large
/// fraction of one of their instruction clusters are the same conversation. The denominator is the
/// smaller session's clusterable count, so a short session replayed inside a longer one is still
/// caught, and the threshold is [`SAME_CONVERSATION_PERCENT`].
///
/// The join is a union-find over *sessions*, with the smaller index as root, for the same
/// determinism reason the message grouping uses one.
fn conversations(
    sessions: &[&DoctorSession],
    occurrences: &[Occurrence<'_>],
    members: &[Vec<usize>],
) -> Groups {
    let mut shared: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for positions in members {
        let present: BTreeSet<usize> = positions
            .iter()
            .map(|&position| occurrences[position].session)
            .collect();
        let present: Vec<usize> = present.into_iter().collect();
        for (index, &left) in present.iter().enumerate() {
            for &right in &present[index + 1..] {
                *shared.entry((left, right)).or_default() += 1;
            }
        }
    }
    let mut joined = Groups::new(sessions.len());
    for ((left, right), count) in shared {
        let smaller = sessions[left]
            .messages
            .clusterable
            .len()
            .min(sessions[right].messages.clusterable.len());
        if count >= MIN_SHARED_INSTRUCTIONS
            && smaller > 0
            && count * 100 >= SAME_CONVERSATION_PERCENT * smaller
        {
            joined.join(left, right);
        }
    }
    joined
}

/// Whether a pair's shared phrase count clears the threshold, against the shorter message.
///
/// Integer arithmetic throughout: `shared / min >= threshold / 100` without the division, so the
/// boundary case is decided by the same comparison on every machine and every run.
fn alike(shared: usize, left: usize, right: usize) -> bool {
    let shorter = left.min(right);
    shorter > 0 && shared * 100 >= SIMILARITY_THRESHOLD_PERCENT * shorter
}

/// One message of one session, while the clustering is looking at it.
struct Occurrence<'a> {
    /// Index into the repository's session list — what "distinct sessions" counts.
    session: usize,
    archived_at: Option<DateTime<Utc>>,
    source_hash: &'a str,
    instruction: &'a Instruction,
}

/// Turns one group of positions into a reportable cluster, or `None` when it is not one.
///
/// The two refusals are the lane's own claim: a group of one is a message, not a repetition, and a
/// group confined to a single session is a conversation, not an instruction that had to be given
/// again. Both are dropped here rather than filtered at the rendering site, so nothing downstream
/// can render a "cluster" that never crossed a session boundary.
fn repetition(
    occurrences: &[Occurrence<'_>],
    positions: &[usize],
    conversations: &mut Groups,
) -> Option<Repetition> {
    // Distinct *conversations*, not distinct session ids: a session that replays another one is the
    // same conversation carried forward, and counting it twice is what would turn one long
    // conversation into a page of findings. See [`conversations`].
    let sessions: BTreeSet<usize> = positions
        .iter()
        .map(|&position| conversations.root(occurrences[position].session))
        .collect();
    if sessions.len() < MIN_CLUSTER_SESSIONS {
        return None;
    }
    let representative = positions
        .iter()
        .map(|&position| &occurrences[position])
        .min_by(most_representative)?;
    let mut cited: Vec<&Occurrence<'_>> = positions
        .iter()
        .map(|&position| &occurrences[position])
        .collect();
    cited.sort_by(newest_first);
    Some(Repetition {
        sessions: sessions.len(),
        representative: representative.instruction.text.clone(),
        occurrences: cited
            .into_iter()
            .map(|occurrence| Citation {
                archived_at: occurrence.archived_at,
                source_hash: occurrence.source_hash.to_owned(),
                locator: occurrence.instruction.locator,
            })
            .collect(),
    })
}

/// Which occurrence speaks for a cluster: the **longest** one, ties broken totally.
///
/// Longest because the members of a cluster are near-duplicates of one another, so the fullest one
/// is the one that states the instruction with the least left out — quoting the terse restatement
/// would show the reader less than the archive actually has. The ties are broken on the earliest
/// archive time, then the content hash, then the event ordinal, which is a total order over
/// occurrences: the same archive produces the same quotation every run.
///
/// A session the archive dated unreadably sorts after any dated one, because "when this happened is
/// unknown" is not a claim that it happened first.
fn most_representative(left: &&Occurrence<'_>, right: &&Occurrence<'_>) -> std::cmp::Ordering {
    fn length(occurrence: &Occurrence<'_>) -> usize {
        occurrence.instruction.text.chars().count()
    }
    fn dated<'a>(occurrence: &'a Occurrence<'_>) -> (bool, Option<DateTime<Utc>>, &'a str, u64) {
        (
            occurrence.archived_at.is_none(),
            occurrence.archived_at,
            occurrence.source_hash,
            occurrence.instruction.locator,
        )
    }
    length(right)
        .cmp(&length(left))
        .then_with(|| dated(left).cmp(&dated(right)))
}

/// Newest occurrence first, with the same total order behind it.
fn newest_first(left: &&Occurrence<'_>, right: &&Occurrence<'_>) -> std::cmp::Ordering {
    left.archived_at
        .is_none()
        .cmp(&right.archived_at.is_none())
        .then_with(|| right.archived_at.cmp(&left.archived_at))
        .then_with(|| left.source_hash.cmp(right.source_hash))
        .then_with(|| left.instruction.locator.cmp(&right.instruction.locator))
}

/// The most-repeated cluster first, within one repository.
fn most_occurrences_first(left: &Repetition, right: &Repetition) -> std::cmp::Ordering {
    right
        .occurrences
        .len()
        .cmp(&left.occurrences.len())
        .then_with(|| right.sessions.cmp(&left.sessions))
        .then_with(|| left.representative.cmp(&right.representative))
}

/// The repository with the most repetition first. Ties fall back to the label, so the section order
/// is total.
fn most_repeated_first(
    left: &RepositoryClusters,
    right: &RepositoryClusters,
) -> std::cmp::Ordering {
    right
        .occurrences
        .cmp(&left.occurrences)
        .then_with(|| right.found.cmp(&left.found))
        .then_with(|| left.repository.cmp(&right.repository))
}

/// The repository with the most error-following messages first, with the unattributed bucket last —
/// [`crate::standup`]'s rule, for its reason: the bucket is the absence of a place, not a busy one.
fn most_friction_first(left: &Friction, right: &Friction) -> std::cmp::Ordering {
    let unattributed = |friction: &Friction| friction.repository == NO_REPOSITORY;
    unattributed(left)
        .cmp(&unattributed(right))
        .then_with(|| right.after_error.cmp(&left.after_error))
        .then_with(|| right.messages.cmp(&left.messages))
        .then_with(|| left.repository.cmp(&right.repository))
}

/// Union-find over message positions, with the smaller index always the root.
///
/// The root rule is what makes the grouping independent of the order pairs were discovered in: two
/// runs that join the same pairs in different orders produce the same roots and therefore the same
/// clusters, which is the property a document claiming to be reproducible needs.
struct Groups {
    parent: Vec<usize>,
}

impl Groups {
    fn new(count: usize) -> Self {
        Self {
            parent: (0..count).collect(),
        }
    }

    fn root(&mut self, mut node: usize) -> usize {
        while self.parent[node] != node {
            // Halve the path on the way up: the classic compression, and it changes nothing about
            // which root a node ends at.
            self.parent[node] = self.parent[self.parent[node]];
            node = self.parent[node];
        }
        node
    }

    fn join(&mut self, left: usize, right: usize) {
        let (left, right) = (self.root(left), self.root(right));
        if left == right {
            return;
        }
        let (root, child) = if left < right {
            (left, right)
        } else {
            (right, left)
        };
        self.parent[child] = root;
    }
}

/// Reads one transcript's user messages and its friction count, streaming it rather than reading it
/// whole.
///
/// One pass, one record at a time; the memory this holds is the clusterable messages themselves,
/// which is what the clustering later needs and nothing more. The result is deterministic for a
/// given transcript: messages are kept in file order and every count is a straight tally.
///
/// # Errors
///
/// Returns an error when `artifact_set_version` names a contract this build cannot read — the same
/// refusal [`crate::metrics::fold_transcript`] and [`crate::verbatim::search`] make.
pub fn read_messages(
    source: Source,
    artifact_set_version: u16,
    reader: impl BufRead,
) -> Result<SessionMessages, UnsupportedVersion> {
    let stream = TranscriptStream::new(source, artifact_set_version, reader)?;
    let mut read = SessionMessages::default();
    let mut locator = 0_u64;
    // Whether the last tool event to report an outcome at all reported a failure. A tool event that
    // reports no outcome ([`crate::metrics::outcome`] returning `None`) says nothing either way and
    // therefore leaves this alone — the fold's own rule for what counts as a failure, so the two
    // cannot come to disagree about what "failed" means.
    let mut after_failure = false;
    for item in stream {
        let Ok(record) = item else {
            read.unreadable_records += 1;
            continue;
        };
        let Classification::Content { events } = &record.classification else {
            continue;
        };
        for event in events {
            locator += 1;
            match event {
                Event::User { text } => {
                    read.messages += 1;
                    // Before anything else: machinery on the user surface is not somebody replying
                    // to a failure any more than it is somebody giving an instruction, so it is
                    // counted and set aside without touching either tally.
                    if !authored(text) {
                        read.harness_generated += 1;
                        continue;
                    }
                    if after_failure {
                        read.after_error += 1;
                        // Attributed once. A person who sends three messages after one failure has
                        // had one thing go wrong, and counting all three would report the length of
                        // their reply rather than the number of failures they replied to.
                        after_failure = false;
                    }
                    let normalized = normalize(text);
                    if words(&normalized) >= MIN_CLUSTERABLE_WORDS {
                        read.clusterable.push(Instruction {
                            locator,
                            text: text.clone(),
                            normalized,
                        });
                    }
                }
                Event::Tool(tool) => {
                    if let Some(succeeded) = outcome(tool) {
                        after_failure = !succeeded;
                    }
                }
                Event::Assistant { .. } => {}
            }
        }
    }
    Ok(read)
}

/// Lower-cases, drops everything that is not a letter or a digit, and joins what is left with single
/// spaces.
///
/// The comparison this lane makes is about *what was asked for*, so the things that differ between
/// two typings of one instruction — capitalisation, punctuation, a wrapped line, a backticked path —
/// are removed before anything is compared. What survives is the sequence of words, which is what
/// [`shingles`] cuts into phrases and what a reader would call "the same instruction".
///
/// The result is a canonical single-space-joined string on purpose: a phrase is then a *contiguous
/// slice* of it, so the index below borrows rather than allocating a string per phrase.
pub fn normalize(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for word in text.split(|character: char| !character.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.extend(word.chars().flat_map(char::to_lowercase));
    }
    normalized
}

/// Words in a [`normalize`]d string.
fn words(normalized: &str) -> usize {
    if normalized.is_empty() {
        return 0;
    }
    normalized.bytes().filter(|byte| *byte == b' ').count() + 1
}

/// The distinct [`SHINGLE_WORDS`]-word phrases of a [`normalize`]d message, as borrowed slices.
///
/// Distinct rather than counted: a message that says "run the tests" four times has said it once as
/// far as *what it is about* is concerned, and letting a repeated phrase count four times toward an
/// overlap would let one insistent sentence carry a whole comparison.
///
/// A message with fewer than [`SHINGLE_WORDS`] words has no phrase and therefore matches nothing —
/// unreachable through [`read_messages`], which will not build an [`Instruction`] anywhere near that
/// short, and handled rather than asserted so the function is total for any caller.
pub fn shingles(normalized: &str) -> BTreeSet<&str> {
    let bounds = word_bounds(normalized);
    if bounds.len() < SHINGLE_WORDS {
        return BTreeSet::new();
    }
    (0..=bounds.len() - SHINGLE_WORDS)
        .map(|first| &normalized[bounds[first].0..bounds[first + SHINGLE_WORDS - 1].1])
        .collect()
}

/// Byte ranges of each word of a [`normalize`]d string. The separator is ASCII space, so scanning
/// bytes cannot land inside a multi-byte character.
fn word_bounds(normalized: &str) -> Vec<(usize, usize)> {
    let mut bounds = Vec::new();
    let mut start = None;
    for (index, byte) in normalized.bytes().enumerate() {
        match (byte == b' ', start) {
            (false, None) => start = Some(index),
            (true, Some(from)) => {
                bounds.push((from, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        bounds.push((from, normalized.len()));
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instruction(locator: u64, text: &str) -> Instruction {
        Instruction {
            locator,
            normalized: normalize(text),
            text: text.to_owned(),
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn session(
        hash: char,
        archived_at: &str,
        repository: Option<&str>,
        texts: &[&str],
    ) -> DoctorSession {
        DoctorSession {
            source_hash: hash.to_string().repeat(64),
            archived_at: Some(at(archived_at)),
            repository: repository.map(str::to_owned),
            bytes_folded: 1_000,
            messages: SessionMessages {
                clusterable: texts
                    .iter()
                    .enumerate()
                    .map(|(index, text)| instruction(index as u64 + 1, text))
                    .collect(),
                messages: texts.len(),
                harness_generated: 0,
                after_error: 0,
                unreadable_records: 0,
            },
        }
    }

    /// The two instructions this test suite repeats, near-duplicates of one another: one word
    /// changed, the punctuation and casing different, and a third that shares only vocabulary.
    const RULE: &str = "Always run cargo fmt and cargo clippy before you tell me a change is done, and paste the \
         output.";
    const RESTATED: &str =
        "always run `cargo fmt` and `cargo clippy` before you tell me any change is done";
    const UNRELATED: &str =
        "The clippy lint about needless borrows is wrong here, leave that line alone please.";

    /// Normalization is what makes two typings of one instruction comparable: case, punctuation, and
    /// line wrapping all disappear, and the words survive in order.
    #[test]
    fn normalization_keeps_the_words_and_drops_everything_else() {
        assert_eq!(
            normalize("Always run `cargo fmt`,\n  then STOP."),
            "always run cargo fmt then stop",
        );
        assert_eq!(normalize("   "), "");
        assert_eq!(words(&normalize("   ")), 0);
        assert_eq!(words(&normalize("one two three")), 3);
        // A word that lower-cases to two characters does not break the word count.
        assert_eq!(words(&normalize("İstanbul repo")), 2);
    }

    /// A phrase is four consecutive words, distinct within a message, and a borrowed slice of the
    /// normalized text rather than a rebuilt string.
    #[test]
    fn shingles_are_distinct_four_word_phrases() {
        let normalized = normalize("run the tests then run the tests again");
        let phrases = shingles(&normalized);
        assert!(phrases.contains("run the tests then"));
        assert!(phrases.contains("the tests then run"));
        assert!(phrases.contains("run the tests again"));
        assert_eq!(phrases.len(), 5, "eight words, five phrases, all distinct");

        // Eight words is five phrases, but the first and the last are the same one, so the set
        // holds four: an insistent sentence cannot carry a comparison by repeating itself.
        let repeated = normalize("fix the failing test fix the failing test");
        assert_eq!(
            shingles(&repeated).len(),
            4,
            "the repeated phrase is counted once: {:?}",
            shingles(&repeated),
        );

        // Too short to have a phrase at all.
        assert!(shingles(&normalize("do it now")).is_empty());
        assert!(shingles("").is_empty());
    }

    /// The floor: an acknowledgement is never an instruction, however many times it is typed.
    #[test]
    fn short_messages_are_counted_and_never_clustered() {
        let transcript = |texts: &[&str]| {
            let mut lines = String::new();
            for (index, text) in texts.iter().enumerate() {
                lines.push_str(&format!(
                    r#"{{"type":"user","uuid":"u{index}","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"user","content":"{text}"}}}}"#,
                ));
                lines.push('\n');
            }
            read_messages(Source::ClaudeCode, 2, lines.as_bytes()).expect("v2 is supported")
        };
        let read = transcript(&["yes", "do it", "continue", RULE]);
        assert_eq!(read.messages, 4, "every user message is counted");
        assert_eq!(
            read.clusterable.len(),
            1,
            "only the sentence clears the floor",
        );
        assert_eq!(read.clusterable[0].text, RULE);
    }

    /// The core property: one instruction typed in two sessions of one repository is a cluster, an
    /// unrelated message is not in it, and the citations carry both sessions.
    #[test]
    fn one_instruction_repeated_across_sessions_clusters() {
        let sessions = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[RULE, UNRELATED],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/qanungo"),
                &[RESTATED],
            ),
        ];
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert_eq!(folded.repositories.len(), 1);
        let section = &folded.repositories[0];
        assert_eq!(section.repository, "surdy/qanungo");
        assert_eq!(section.clusters.len(), 1, "the unrelated message is alone");
        let cluster = &section.clusters[0];
        assert_eq!(cluster.occurrences, 2);
        assert_eq!(cluster.sessions, 2);
        assert!(
            cluster.excerpt.starts_with("Always run cargo fmt"),
            "the longest occurrence speaks for the cluster: {}",
            cluster.excerpt,
        );
        // Newest first, and the citation carries the transcript's own event ordinal.
        assert_eq!(cluster.citations.len(), 2);
        assert_eq!(cluster.citations[0].source_hash, "b".repeat(64));
        assert_eq!(cluster.citations[1].source_hash, "a".repeat(64));
        assert_eq!(cluster.citations[1].locator, 1);
    }

    /// Repetition inside one session is not a cluster: that is the friction section's business, and
    /// conflating the two would report a conversation as a forgotten instruction.
    #[test]
    fn repetition_inside_one_session_is_not_a_cluster() {
        let sessions = [session(
            'a',
            "2026-08-20T10:00:00Z",
            Some("surdy/qanungo"),
            &[RULE, RESTATED, RULE],
        )];
        // One session is also under the repository floor, so it is listed rather than examined.
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert!(folded.is_empty());
        assert_eq!(folded.unexamined.len(), 1);
        assert_eq!(folded.unexamined[0].reason, TOO_FEW_SESSIONS);

        // And with the floor cleared by a second session that shares nothing, the within-session
        // repeat still does not cluster on its own.
        let sessions = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[RULE, RESTATED],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/qanungo"),
                &[UNRELATED],
            ),
        ];
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert!(
            folded.is_empty(),
            "the pair is one session's own: {:?}",
            folded.repositories,
        );
        assert_eq!(folded.repositories_examined, 1);
    }

    /// The same instruction in two repositories is two facts about two repositories, and the index
    /// is built per repository so the two can never be compared at all.
    #[test]
    fn repositories_are_isolated_from_one_another() {
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", Some("surdy/qanungo"), &[RULE]),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/munshi"),
                &[RESTATED],
            ),
        ];
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert!(folded.is_empty(), "one session each, nothing to cluster");
        assert_eq!(folded.unexamined.len(), 2);

        // Even with each repository over the floor, the cross-repository pair does not cluster.
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", Some("surdy/qanungo"), &[RULE]),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/qanungo"),
                &[UNRELATED],
            ),
            session(
                'c',
                "2026-08-22T10:00:00Z",
                Some("surdy/munshi"),
                &[RESTATED],
            ),
            session(
                'd',
                "2026-08-23T10:00:00Z",
                Some("surdy/munshi"),
                &[UNRELATED],
            ),
        ];
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert!(
            folded.repositories.is_empty(),
            "no cluster spans the two repositories: {:?}",
            folded.repositories,
        );
        assert_eq!(folded.repositories_examined, 2);
    }

    /// A session the archive attributes to no repository is not a repository: it is listed as
    /// unexamined, and it still contributes its friction row.
    #[test]
    fn the_unattributed_bucket_is_listed_rather_than_clustered() {
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", None, &[RULE]),
            session('b', "2026-08-21T10:00:00Z", None, &[RESTATED]),
        ];
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert!(folded.is_empty());
        assert_eq!(folded.repositories_examined, 0);
        assert_eq!(folded.unexamined.len(), 1);
        assert_eq!(folded.unexamined[0].repository, NO_REPOSITORY);
        assert_eq!(folded.unexamined[0].reason, NOT_A_REPOSITORY);
        assert_eq!(folded.friction.len(), 1);
        assert_eq!(folded.friction[0].repository, NO_REPOSITORY);
    }

    /// Twelve mutually unrelated instructions, one per index.
    ///
    /// Built rather than hand-written: what is under test is the *cut*, and twelve realistic
    /// instructions that all had to stay under the similarity threshold would be twelve sentences
    /// chosen to prove something other than the thing being asserted. Each index appears three
    /// times in the sentence, so any two of these share only the phrases none of the three positions
    /// falls in — far under [`SIMILARITY_THRESHOLD_PERCENT`], which the test asserts by counting the
    /// clusters that form.
    fn distinct_instruction(index: usize) -> String {
        format!(
            "always regenerate the {index} bindings and rerun the {index} fixtures before you tell \
             me the {index} migration is finished"
        )
    }

    /// The per-repository cut hides clusters and says so; raising it reveals exactly those, and
    /// moves no count (qanungo #16).
    ///
    /// This is the finding the flag exists for: the hidden clusters are the *weakest* of what was
    /// found, which is the same weight class the `instructions-editor` skill acts on, so a reader
    /// that cannot raise the cut is reading a truncated input rather than a shorter one.
    #[test]
    fn the_per_repository_cut_is_a_default_the_caller_can_raise() {
        // Two sessions per instruction rather than two sessions carrying all twelve: a pair of
        // sessions sharing twelve instructions is a resumed conversation by
        // [`SAME_CONVERSATION_PERCENT`]'s reckoning, and would be folded into one before any of this
        // was cut. A pair sharing one is what repetition actually looks like.
        let texts: Vec<String> = (0..12).map(distinct_instruction).collect();
        let sessions: Vec<DoctorSession> = texts
            .iter()
            .enumerate()
            .flat_map(|(index, text)| {
                let hash = |offset: usize| char::from(b'a' + (index + offset) as u8);
                [
                    session(
                        hash(0),
                        "2026-08-20T10:00:00Z",
                        Some("surdy/qanungo"),
                        &[text],
                    ),
                    session(
                        hash(12),
                        "2026-08-21T10:00:00Z",
                        Some("surdy/qanungo"),
                        &[text],
                    ),
                ]
            })
            .collect();
        let fold = |cap: usize| {
            Doctor::fold(
                &sessions,
                Vec::new(),
                &RedactionReport::default(),
                &Redactor::new(),
                cap,
            )
        };

        let capped = fold(DEFAULT_CLUSTERS_PER_REPOSITORY);
        let section = &capped.repositories[0];
        assert_eq!(section.found, 12, "twelve instructions, twelve clusters");
        assert_eq!(section.clusters.len(), DEFAULT_CLUSTERS_PER_REPOSITORY);

        let raised = fold(50);
        let widened = &raised.repositories[0];
        assert_eq!(
            widened.clusters.len(),
            12,
            "a cap above what was found shows all of it and invents nothing",
        );
        assert_eq!(
            widened.clusters[..DEFAULT_CLUSTERS_PER_REPOSITORY],
            section.clusters[..],
            "the cut takes a prefix: raising it reveals, it never reorders",
        );

        // The cut is on the rendering alone, so nothing counted moves with it.
        assert_eq!(raised.clusters, capped.clusters);
        assert_eq!(widened.found, section.found);
        assert_eq!(widened.occurrences, section.occurrences);

        // And it cuts downwards on the same rule, for the operator who wanted a shorter read.
        assert_eq!(fold(3).repositories[0].clusters.len(), 3);
    }

    /// The same corpus folded twice is the same document, and the clustering does not depend on the
    /// order the sessions arrived in.
    #[test]
    fn the_fold_is_deterministic_and_order_independent() {
        let forwards = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[RULE, UNRELATED],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/qanungo"),
                &[RESTATED],
            ),
            session('c', "2026-08-22T10:00:00Z", Some("surdy/qanungo"), &[RULE]),
        ];
        let mut backwards = forwards.clone().to_vec();
        backwards.reverse();

        let fold = |sessions: &[DoctorSession]| {
            Doctor::fold(
                sessions,
                Vec::new(),
                &RedactionReport::default(),
                &Redactor::new(),
                DEFAULT_CLUSTERS_PER_REPOSITORY,
            )
        };
        let first = fold(&forwards);
        assert_eq!(first.repositories, fold(&forwards).repositories);
        assert_eq!(
            first.repositories,
            fold(&backwards).repositories,
            "the listing order is not part of the finding",
        );
        assert_eq!(first.repositories[0].clusters[0].occurrences, 3);
        assert_eq!(first.repositories[0].clusters[0].sessions, 3);
    }

    /// The canary: a cluster whose messages carry a credential renders its excerpt with the
    /// credential replaced — and the credential did not decide what clustered, because the
    /// comparison read the transcript's own bytes.
    #[test]
    fn a_cluster_carrying_a_credential_renders_it_scrubbed() {
        let secret = "ghp_CANARYCANARYCANARYCANARYCANARYCANARY";
        let first = format!("Never paste the token {secret} into the run log, redact it first.");
        let second = format!("never paste the token {secret} into the run log please redact it");
        let sessions = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[&first],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/qanungo"),
                &[&second],
            ),
        ];
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        let cluster = &folded.repositories[0].clusters[0];
        assert_eq!(cluster.occurrences, 2, "the secret did not split the pair");
        assert!(
            !cluster.excerpt.contains(secret),
            "the excerpt leaked: {}",
            cluster.excerpt,
        );
        assert!(cluster.excerpt.contains("[REDACTED:github-token]"));
        assert!(
            !folded.redaction.is_empty(),
            "and the replacement was counted",
        );

        // With the scrub off the flag is real, and the same pair still clusters.
        let bare = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new().with_secrets(false),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert!(bare.repositories[0].clusters[0].excerpt.contains(secret));
    }

    /// A repository label is clamped and then scrubbed on the way to the page, like every other
    /// archive-derived label this crate renders — and a hostile one cannot break the section
    /// heading it is written into.
    #[test]
    fn a_hostile_repository_label_is_clamped_and_a_credential_shaped_one_is_scrubbed() {
        const TOKEN_SHAPED: &str = "ghp_FAKEfake0123456789ABCDEFabcdef012345";
        assert_eq!(format::identifier(TOKEN_SHAPED), TOKEN_SHAPED);
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", Some("evil | repo"), &[RULE]),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("evil | repo"),
                &[RESTATED],
            ),
            session('c', "2026-08-22T10:00:00Z", Some(TOKEN_SHAPED), &[RULE]),
            session('d', "2026-08-23T10:00:00Z", Some(TOKEN_SHAPED), &[RESTATED]),
        ];
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        let labels: Vec<&str> = folded
            .repositories
            .iter()
            .map(|section| section.repository.as_str())
            .collect();
        assert!(labels.contains(&format::INVALID_IDENTIFIER));
        assert!(labels.contains(&"[REDACTED:github-token]"));
        assert!(!labels.iter().any(|label| label.contains(TOKEN_SHAPED)));
        assert!(!labels.iter().any(|label| label.contains('|')));
    }

    /// The friction proxy: a message that arrives while the last tool outcome was a failure is
    /// counted once, and a message after a success is not counted at all.
    #[test]
    fn friction_counts_the_message_after_a_failing_tool_once() {
        const AFTER_FAILURE: &str = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"user","content":"run the tests"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:05.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:09.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"two tests failed","is_error":true}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","timestamp":"2026-08-01T10:00:20.000Z","message":{"role":"user","content":"that is the fixture, fix the fixture and not the assertion"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u3","timestamp":"2026-08-01T10:00:30.000Z","message":{"role":"user","content":"and then run the whole suite again before you say anything"}}"#,
        );
        let read = read_messages(Source::ClaudeCode, 2, AFTER_FAILURE.as_bytes())
            .expect("v2 is supported");
        assert_eq!(read.messages, 3, "the tool result is not a user message");
        assert_eq!(
            read.after_error, 1,
            "one failure, one message attributed to it",
        );
        assert_eq!(read.clusterable.len(), 2);
        // The locator space is the event ordinal, so the first typed message is event 1 and the
        // reply to the failure is event 4.
        assert_eq!(read.clusterable[0].locator, 4);
        assert_eq!(read.clusterable[1].locator, 5);
    }

    /// This lane's locator is [`crate::verbatim`]'s locator: the same event of the same transcript
    /// gets the same coordinate from both, which is what lets a reader carry one between them.
    #[test]
    fn the_locator_space_is_the_verbatim_lanes() {
        const EXCHANGE: &str = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"user","content":"always run cargo fmt and clippy before you say a change is done"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:05.000Z","message":{"role":"assistant","content":[{"type":"text","text":"will do"}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","timestamp":"2026-08-01T10:00:20.000Z","message":{"role":"user","content":"always run cargo fmt and clippy before you say the change is done"}}"#,
        );
        let read =
            read_messages(Source::ClaudeCode, 2, EXCHANGE.as_bytes()).expect("v2 is supported");
        let found = crate::verbatim::search(
            Source::ClaudeCode,
            2,
            EXCHANGE.as_bytes(),
            &crate::ask::Query::parse("always"),
            &Redactor::new(),
        )
        .expect("v2 is supported");
        let mine: Vec<u64> = read
            .clusterable
            .iter()
            .map(|instruction| instruction.locator)
            .collect();
        let theirs: Vec<u64> = found.matches.iter().map(|hit| hit.locator).collect();
        assert_eq!(mine, theirs, "the two lanes count the same events");
        assert_eq!(mine, vec![1, 3]);
    }

    /// A record this build cannot read carries no typed text, so it is counted rather than read —
    /// the lane's own "counted, never dropped", inside one transcript.
    #[test]
    fn an_unreadable_record_is_counted_rather_than_read() {
        let transcript = concat!(
            "{not json at all\n",
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"user","content":"always run cargo fmt and clippy before you say a change is done"}}"#,
        );
        let read =
            read_messages(Source::ClaudeCode, 2, transcript.as_bytes()).expect("v2 is supported");
        assert_eq!(read.unreadable_records, 1);
        assert_eq!(read.messages, 1, "the readable record was still read");
    }

    /// A contract this build does not know is refused rather than half-read.
    #[test]
    fn an_unsupported_artifact_set_version_is_refused() {
        assert!(read_messages(Source::ClaudeCode, u16::MAX, "".as_bytes()).is_err());
    }

    /// The similarity test is containment against the shorter message, on integer arithmetic, so an
    /// instruction restated inside a longer request still clears the threshold.
    #[test]
    fn similarity_is_containment_against_the_shorter_message() {
        assert!(alike(6, 10, 6), "all six of the shorter one's phrases");
        assert!(alike(6, 10, 10), "60% of ten is six");
        assert!(!alike(5, 10, 10), "and five is under it");
        assert!(!alike(0, 0, 4), "a message with no phrase matches nothing");
    }

    /// The user *surface* is not the user: a pasted image, a slash command, a skill body and a task
    /// notification are all written by the harness, all byte-identical between sessions, and all
    /// excluded — counted, never silently dropped.
    #[test]
    fn harness_written_messages_are_counted_and_never_compared() {
        for injected in [
            "[Image: original 2400x1080, displayed at 2000x900. Multiply coordinates by 1.20.]",
            "<command-name>/model</command-name> <command-message>model</command-message>",
            "<bash-input>adb -s 192.168.1.2:5555 install -r build/app.apk</bash-input>",
            "<local-command-stdout>Set model to Opus 5 and saved as your default</local-command-stdout>",
            "<task-notification> <task-id>abc</task-id> <status>stopped</status> </task-notification>",
            "<skill-context name=\"grill-me\"> Base directory: /skills/grill-me </skill-context>",
            "Base directory for this skill: /skills/run **Running means launching the app**",
            "This session is being continued from a previous conversation that ran out of context.",
            "{\"instruction\":\"Summarize this coding session as exactly one JSON object\"}",
        ] {
            assert!(!authored(injected), "{injected}");
            // Leading whitespace does not smuggle one past the list.
            assert!(!authored(&format!("  \n{injected}")), "{injected}");
        }
        // A real instruction that happens to *mention* one of the markers is still authored: the
        // test is on the opening, not on the contents.
        assert!(authored(RULE));
        assert!(authored(
            "when you see <bash-input> in a transcript that is the harness talking, not me",
        ));

        let transcript = |texts: &[&str]| {
            let mut lines = String::new();
            for (index, text) in texts.iter().enumerate() {
                lines.push_str(&format!(
                    r#"{{"type":"user","uuid":"u{index}","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"user","content":{}}}}}"#,
                    serde_json::to_string(text).expect("a JSON string"),
                ));
                lines.push('\n');
            }
            read_messages(Source::ClaudeCode, 2, lines.as_bytes()).expect("v2 is supported")
        };
        let read = transcript(&[
            "[Image: original 2400x1080, displayed at 2000x900. Multiply coordinates by 1.20.]",
            RULE,
        ]);
        assert_eq!(read.messages, 2, "both are counted on the user surface");
        assert_eq!(read.harness_generated, 1);
        assert_eq!(read.authored(), 1);
        assert_eq!(read.clusterable.len(), 1);
        assert_eq!(read.clusterable[0].text, RULE);
    }

    /// Two sessions that replay the same conversation are one conversation, so the instructions
    /// they share are not repetitions — the largest false finding this lane can make, refused.
    #[test]
    fn two_sessions_replaying_one_conversation_do_not_repeat_themselves() {
        let conversation = [
            "Set the split DNS zone up so the home network resolves the services locally please",
            "Now write the caddy config for the same set of hostnames and show me the diff first",
            "Add the unifi controller to that list as well, with its own certificate please",
            "And finally write the runbook for all of it into the repository under docs please",
        ];
        let replayed = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/quadhost"),
                &conversation,
            ),
            session(
                'b',
                "2026-08-20T10:01:00Z",
                Some("surdy/quadhost"),
                &conversation,
            ),
        ];
        let folded = Doctor::fold(
            &replayed,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert!(
            folded.is_empty(),
            "a replayed transcript is not four repetitions: {:?}",
            folded.repositories,
        );
        assert_eq!(folded.sessions, 2, "both sessions were still read");
        assert_eq!(folded.conversations, 1, "and they are one conversation");

        // The control that keeps the rule from eating the finding: two sessions that share *one*
        // instruction out of four are two conversations, and the shared one is a cluster.
        let distinct = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/quadhost"),
                &conversation,
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/quadhost"),
                &[
                    conversation[0],
                    "Something else entirely about the podman socket and the quadlet unit files",
                    "And another unrelated request about the backup schedule for the volumes",
                    "One more about the tailscale funnel and whether it should stay switched off",
                ],
            ),
        ];
        let folded = Doctor::fold(
            &distinct,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert_eq!(folded.conversations, 2);
        assert_eq!(folded.repositories.len(), 1);
        assert_eq!(folded.repositories[0].clusters.len(), 1);
        assert_eq!(folded.repositories[0].clusters[0].sessions, 2);
    }

    /// A session with no user message at all is counted rather than quietly contributing an empty
    /// result: it is a session this lane could not look into, not a session with nothing in it.
    #[test]
    fn a_session_with_no_user_message_is_counted() {
        let sessions = [session(
            'a',
            "2026-08-20T10:00:00Z",
            Some("surdy/qanungo"),
            &[],
        )];
        let folded = Doctor::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert_eq!(folded.sessions, 1);
        assert_eq!(folded.sessions_without_messages, 1);
        assert_eq!(folded.messages, 0);
    }
}
