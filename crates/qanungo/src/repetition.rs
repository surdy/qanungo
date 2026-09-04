//! The repetition machinery two lanes look through: near-duplicate detection over the text a
//! *person* typed.
//!
//! `report` counts what tools did, `cost` counts what messages cost, `standup` narrates what munshi
//! wrote, and `ask` ranks summaries against a question. This module reads the one surface none of
//! them reason about — user messages — and answers exactly one question about it: **which of these
//! messages are the same request typed more than once?** It answers nothing else. What a repetition
//! *means* is the caller's business, and the two callers mean two different things by it:
//!
//! - [`crate::doctor`] groups per **repository** and calls the result an instruction you have had to
//!   repeat, because an instruction file belongs to a repository and a finding that spanned two of
//!   them could not be acted on in either.
//! - [`crate::flows`] groups across **the whole archive** and calls the result a request that
//!   recurs, because a workflow worth a skill is worth it wherever it recurs — the repositories are
//!   where such a finding *lists*, never how it groups.
//!
//! # The one place the two lenses disagree, and it is deliberate
//!
//! A session the archive attributes to **no repository** is not a repository, so `doctor` lists it
//! as unexamined rather than clustering it: there is no instruction file for a finding about it to
//! be about. `flows` has no such carve-out — a repeated request does not need a repository to be a
//! repeated request — so those sessions join its clustering like any other. That difference lives in
//! the two callers, and neither of them is this module's business: everything below treats the
//! sessions it is handed as one pool and never looks at a repository at all.
//!
//! # The detection scheme, and why this one
//!
//! Over every [`Event::User`] the transcripts of the pool carried:
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
//! 3. **Compare through an inverted index** — [`Clustering::of`] maps each distinct phrase to the
//!    messages carrying it, so a message is only ever compared against messages it shares a phrase
//!    with. The all-pairs alternative is quadratic in the messages of the pool, which the archive is
//!    already large enough to make untenable.
//! 4. **Cluster** — single-linkage over the pairs that cross [`SIMILARITY_THRESHOLD_PERCENT`],
//!    joined with a union-find whose root is always the smaller index, so the grouping cannot depend
//!    on the order pairs were discovered in.
//! 5. **Join the sessions that are one conversation** ([`Clustering::of`]'s second union-find) — a
//!    resumed or re-captured session replays the earlier one, so two session ids can hold the same
//!    conversation. Counting those as two sessions would report one long conversation as a page of
//!    repetitions, so they are merged before a cluster's session span is counted.
//!
//! Steps 0 and 5 are not decoration: both were forced by the first run against the production
//! archive, where without them injected boilerplate and one replayed conversation were most of what
//! the doctor document said. The measurements are in each constant's own rustdoc.
//!
//! ## The one place this is bounded rather than exact
//!
//! A phrase carried by *hundreds* of the pool's messages ("i want you to") discriminates nothing,
//! and expanding it into pairs is what makes an inverted index degenerate back into the quadratic
//! scan it exists to avoid. So a phrase held by more than [`MAX_SHINGLE_POSTINGS`] messages is
//! skipped when candidates are gathered.
//!
//! The direction of that error is the point: a skipped phrase can only ever *lower* a pair's
//! measured overlap, so the bound can hide a repetition and can never invent one. Everything either
//! document reports is a pair that cleared the threshold on phrases the index actually counted —
//! there is no approximate similarity anywhere, and no hashing that could collide two different
//! phrases into one. What is bounded is which pairs are looked at, not what is true of the pairs
//! that are.
//!
//! # Clusters cross sessions; repetition inside one session does not count
//!
//! A cluster must span at least [`MIN_CLUSTER_SESSIONS`] distinct *conversations* of the pool.
//! Restating something three times inside a single session is a conversation, not a request that
//! keeps coming back between sittings.
//!
//! # What this reads, and the enrichment it does not wait for
//!
//! User messages, and nothing else. qanungo #13 names munshi#77's prompt/tool signals as a
//! dependency, and for the first build of either lane it is not one: what a person *asked for* is
//! carried by what they typed, and a request restated across sessions is visible without knowing
//! which tools ran between the restatements. **Tool-sequence enrichment is a named deferral**, on
//! decision 6's interleave rule — the field gets pulled when a consumer names what it would decide
//! differently with it, and neither lane's V1 does. The most likely first consumer is a flow whose
//! steps are indistinguishable by prose but distinguishable by what ran after them; nothing in the
//! real archive has demanded that yet.
//!
//! # Nothing here is scrubbed
//!
//! Clustering runs on the **unscrubbed** text, in both lanes, for [`crate::verbatim`]'s reason:
//! replacing a token before the comparison would let a credential quietly split a cluster in two,
//! and the count beside the excerpt would then be wrong in a way no reader could see. Every string
//! this module hands back — [`Repetition::representative`] above all — is transcript text as the
//! archive holds it, and it is the **caller's** job to scrub it on the way into anything rendered.
//! Both callers do, through [`crate::ask::snippet`]'s pipeline: scrub, then collapse, then clip.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

use chrono::{DateTime, Utc};
use munshi_transcript::{Classification, Event, Source, TranscriptStream, UnsupportedVersion};

use crate::metrics::outcome;

/// Words in one shingle — the phrase length two messages are compared on.
///
/// **A tunable, not a decision.** Four is long enough that an ordinary pair of English messages does
/// not share phrases by accident and short enough that an instruction retyped with a word moved
/// still shares most of its phrases with itself. Raising it makes the comparison insist on longer
/// verbatim agreement; lowering it starts clustering messages that merely share vocabulary.
pub const SHINGLE_WORDS: usize = 4;

/// Shortest message, in words, this machinery will compare at all.
///
/// **A tunable, not a decision**, and the one that keeps the clusters meaningful. "yes", "do it",
/// "continue", "thanks" and "now the tests" are the most repeated things anybody types at a coding
/// agent, and every one of them would cluster perfectly and mean nothing. A request worth naming is
/// a sentence; the floor is set where a sentence starts.
///
/// A message under the floor is *counted* ([`SessionMessages::messages`] against
/// [`SessionMessages::clusterable`]) rather than passed over in silence, so a reader can see how
/// much of the conversation was declined.
pub const MIN_CLUSTERABLE_WORDS: usize = 8;

/// How much of the shorter message's phrases the two have to share, in percent, to be called the
/// same request.
///
/// **A tunable, not a decision.** Stated in percent and compared with integer arithmetic on purpose:
/// a threshold on a float is a threshold whose behaviour at the boundary depends on rounding, and
/// both documents built on this have to be reproducible run to run.
///
/// The denominator is the *shorter* message's phrase count — containment rather than Jaccard —
/// because the shape being looked for is a request restated inside a longer message. A two-line rule
/// repeated verbatim at the top of a long request is exactly the repetition worth showing, and
/// Jaccard would score it near zero for the crime of being surrounded by new context.
pub const SIMILARITY_THRESHOLD_PERCENT: usize = 60;

/// Distinct conversations a cluster has to span before it is reported.
///
/// **A tunable, not a decision** — but only in one direction. Two is the floor the whole claim rests
/// on: repetition *within* one session is a conversation, so a "cluster" confined to one session is
/// not a finding at all. Raising it would trade noise for silence about genuine pairs; it is left at
/// the floor so nothing real is hidden.
pub const MIN_CLUSTER_SESSIONS: usize = 2;

/// How much of the smaller session's messages two sessions have to share before they are read as
/// **one conversation captured twice** rather than as two.
///
/// **A tunable, not a decision — and the constant a first run against the production archive
/// forced.** The archive holds pairs of sessions, seconds apart, whose clusterable messages are the
/// same list at the same event ordinals: a conversation resumed, or re-captured, so the second
/// transcript replays the first. Counting those as two sessions turns every message of one long
/// conversation into a "repeated request", which is the largest false finding either lane can
/// make — the whole of one repository's doctor section was that on the first run.
///
/// The separation measured on the real archive is not close: two genuinely distinct sessions that
/// repeat one instruction share a few percent of their messages, and a replayed conversation shares
/// nearly all of them. Fifty sits in the wide gap between the two.
pub const SAME_CONVERSATION_PERCENT: usize = 50;

/// Messages two sessions have to share before [`SAME_CONVERSATION_PERCENT`] is even consulted.
///
/// **A tunable, not a decision**, and the floor that keeps the conversation rule from eating the
/// finding it exists to protect. A percentage over a tiny denominator says nothing: two short
/// sessions that share the *one* request being looked for share 100% of the smaller one, and merging
/// them would delete exactly the finding. A replayed transcript, by contrast, brings the whole
/// earlier conversation with it — the pair the production archive turned up shared thirty-nine.
/// Three is set well below that and well above one.
pub const MIN_SHARED_INSTRUCTIONS: usize = 3;

/// Messages a phrase may appear in before it is skipped for candidate gathering.
///
/// **A tunable, not a decision.** See the module docs for the honesty argument: skipping a phrase
/// can only lower a measured overlap, so this bounds the *work*, never the truth of what is
/// reported. Two hundred is generous — a phrase that common in one pool is boilerplate, and
/// expanding it into twenty thousand pairs buys nothing but time.
pub const MAX_SHINGLE_POSTINGS: usize = 200;

/// Occurrences one cluster cites before the list is cut short.
///
/// A tunable, on [`crate::verbatim::MAX_MATCHES_PER_SESSION`]'s reasoning: enough citations to go
/// and look, few enough that a cluster stays a paragraph. The total travels beside them
/// ([`Repetition::occurrences`]) so a cut list is never mistaken for the whole of what was found.
pub const MAX_CITATIONS_PER_CLUSTER: usize = 8;

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
/// a request anybody had to make twice.
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
/// the list removes is *counted* ([`SessionMessages::harness_generated`]) and stated in both
/// documents, so its size is never a silent subtraction.
///
/// **`flows` is where the gap shows worst**, and knowing that is not the same as being able to close
/// it. A skill body injected in an uncertified shape clusters per repository under `doctor`, where
/// it is one residual cluster among a repository's own; pooled across the whole archive it clusters
/// with *itself in every repository at once*, which is arithmetically the most repeated text in the
/// corpus. The remedy is a certified opening, added the way every entry here was — by looking at the
/// real archive and confirming the shape — never a similarity heuristic that guesses at prose.
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
    // Copilot: skill preambles, the rename request it opens a session with, and its own reminder
    // marker — spelled with an underscore where Claude Code spells it with a hyphen, which is why
    // the entry above does not cover it. Certified by the `flows` calibration run: 32 occurrences
    // across 27 sessions of the production archive, every one of them either the deferred-tool
    // notice or the contents of the repository's own `AGENTS.md` / `copilot-instructions.md`
    // injected into the user surface, and no message in the archive opens with it any other way.
    "<skill-context",
    "<session_rename_request>",
    "<system_reminder>",
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
/// dropped, which is what keeps the memory this holds proportional to the requests in the archive
/// rather than to every "yes" in it.
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
    /// within-session friction proxy. See [`crate::doctor::Friction`] for what it does and does not
    /// mean; `flows` does not read it.
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
pub struct SessionRecord {
    /// The transcript's content hash — the citation a reader redeems, as in every other lane.
    pub source_hash: String,
    /// When the archive finished the snapshot this session was listed by. Archive time, the clock
    /// the window (if any) was cut on.
    pub archived_at: Option<DateTime<Utc>>,
    /// The repository the *listing* recorded for this session, unscrubbed and unclamped. `None` is a
    /// real state — a session captured outside a checkout — and is not a repository. What the two
    /// lanes do with that is the difference the module docs name.
    pub repository: Option<String>,
    /// Decompressed transcript bytes the fold read.
    pub bytes_folded: u64,
    pub messages: SessionMessages,
}

/// One occurrence of a repeated request, as either document cites it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub archived_at: Option<DateTime<Utc>>,
    pub source_hash: String,
    /// The event ordinal within that transcript. See [`Instruction::locator`].
    pub locator: u64,
}

/// One cluster before it is rendered: still carrying the representative's **unscrubbed** text.
///
/// The caller scrubs on the way into whatever it renders. See the module docs' last section for why
/// the scrub cannot happen any earlier than that.
#[derive(Debug, Clone)]
pub struct Repetition {
    /// Every occurrence, newest first.
    pub occurrences: Vec<Citation>,
    /// Distinct *conversations* it spans — sessions that replay one another count once. At least
    /// [`MIN_CLUSTER_SESSIONS`], by construction. See [`SAME_CONVERSATION_PERCENT`].
    pub sessions: usize,
    /// The fullest occurrence's own text, unscrubbed. See [`most_representative`] for which one and
    /// why.
    pub representative: String,
    /// Positions into [`Clustering::occurrences`], ascending — which is session-then-file order.
    ///
    /// `doctor` ignores this; `flows` needs it, because a multi-step flow is a statement about the
    /// *order* clustered messages arrived in and that order is only recoverable from the flat list.
    pub positions: Vec<usize>,
}

/// One message of one session, while the clustering is looking at it.
pub struct Occurrence<'a> {
    /// Index into the pool's session list — what "distinct sessions" counts before the conversation
    /// merge, and what [`Clustering::conversation_of`] maps to a conversation.
    session: usize,
    archived_at: Option<DateTime<Utc>>,
    source_hash: &'a str,
    instruction: &'a Instruction,
}

impl Occurrence<'_> {
    /// Index into the pool's session list.
    pub fn session(&self) -> usize {
        self.session
    }

    /// The event ordinal this message sits at in its transcript.
    pub fn locator(&self) -> u64 {
        self.instruction.locator
    }
}

/// One pool of sessions, clustered.
///
/// Built once by [`Clustering::of`] and then read by the lane: [`Clustering::repetitions`] is the
/// clusters that cleared [`MIN_CLUSTER_SESSIONS`], and [`Clustering::occurrences`] plus
/// [`Repetition::positions`] are what a caller that cares about *order* reads instead.
pub struct Clustering<'a> {
    occurrences: Vec<Occurrence<'a>>,
    /// Groups of occurrence positions, keyed by union-find root ascending, every position in
    /// exactly one group.
    members: Vec<Vec<usize>>,
    /// Union-find over sessions: two sessions that replay one conversation share a root.
    conversations: Groups,
    /// Distinct conversations the pool turned out to be.
    distinct_conversations: usize,
}

impl<'a> Clustering<'a> {
    /// Clusters one pool's clusterable messages.
    ///
    /// Every message of every session in one flat list, an inverted index over their phrases, one
    /// pass of candidate comparison per message, and a union-find over the pairs that cleared the
    /// threshold. See the module docs for why each step is the step it is.
    pub fn of(sessions: &[&'a SessionRecord]) -> Self {
        let occurrences: Vec<Occurrence<'a>> = sessions
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
            return Self {
                occurrences,
                members: Vec::new(),
                conversations: Groups::new(sessions.len()),
                distinct_conversations: sessions.len(),
            };
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
            // pool's worth of pairs.
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
        let distinct_conversations = (0..sessions.len())
            .map(|session| conversations.root(session))
            .collect::<BTreeSet<_>>()
            .len();
        Self {
            occurrences,
            members,
            conversations,
            distinct_conversations,
        }
    }

    /// Distinct conversations the pool turned out to be — two sessions that replay one another count
    /// once.
    pub fn conversations(&self) -> usize {
        self.distinct_conversations
    }

    /// The conversation one session belongs to, as a session index that is stable for the pool.
    ///
    /// What a caller counting "how many sittings did this span" has to count, rather than session
    /// ids: a resumed session replays what came before it.
    pub fn conversation_of(&mut self, session: usize) -> usize {
        self.conversations.root(session)
    }

    /// Every message of the pool, in session-then-file order. Indexed by [`Repetition::positions`].
    pub fn occurrences(&self) -> &[Occurrence<'a>] {
        &self.occurrences
    }

    /// The clusters that cleared [`MIN_CLUSTER_SESSIONS`], in union-find root order.
    ///
    /// Root order rather than any interesting order on purpose: it is a total order that depends on
    /// nothing but the pool's own contents, and every caller sorts what it is going to render
    /// anyway. Both callers' sorts are stable, so the tie-break of last resort is this one.
    pub fn repetitions(&mut self) -> Vec<Repetition> {
        let mut found = Vec::new();
        for positions in &self.members {
            if let Some(repetition) =
                repetition(&self.occurrences, positions, &mut self.conversations)
            {
                found.push(repetition);
            }
        }
        found
    }
}

/// Joins the sessions that are one conversation captured twice.
///
/// # The finding this exists to refuse
///
/// A session resumed — or re-captured — carries the earlier conversation with it, so the archive
/// holds pairs of session ids whose user messages are the same list in the same order. Every one of
/// those messages then looks like a request made in two sessions, and a whole repository's doctor
/// section becomes one long conversation reported back as forty separate repetitions. That is not a
/// threshold that needs raising: the messages really are identical, and no similarity rule can tell
/// a replay from a repetition by looking at *one pair of messages*. It has to be answered at the
/// level of the pair of **sessions**.
///
/// # How it is answered
///
/// Reusing the clustering that has already run: two sessions that appear together in a large
/// fraction of one of their message clusters are the same conversation. The denominator is the
/// smaller session's clusterable count, so a short session replayed inside a longer one is still
/// caught, and the threshold is [`SAME_CONVERSATION_PERCENT`].
///
/// The join is a union-find over *sessions*, with the smaller index as root, for the same
/// determinism reason the message grouping uses one.
fn conversations(
    sessions: &[&SessionRecord],
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

/// Turns one group of positions into a reportable cluster, or `None` when it is not one.
///
/// The two refusals are the whole claim: a group of one is a message, not a repetition, and a group
/// confined to a single conversation is a conversation, not a request that had to be made again.
/// Both are dropped here rather than filtered at a rendering site, so nothing downstream can render
/// a "cluster" that never crossed a session boundary.
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
        positions: positions.to_vec(),
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
/// is the one that states the request with the least left out — quoting the terse restatement would
/// show the reader less than the archive actually has. The ties are broken on the earliest archive
/// time, then the content hash, then the event ordinal, which is a total order over occurrences: the
/// same archive produces the same quotation every run.
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
/// The comparison here is about *what was asked for*, so the things that differ between two typings
/// of one request — capitalisation, punctuation, a wrapped line, a backticked path — are removed
/// before anything is compared. What survives is the sequence of words, which is what [`shingles`]
/// cuts into phrases and what a reader would call "the same instruction".
///
/// The result is a canonical single-space-joined string on purpose: a phrase is then a *contiguous
/// slice* of it, so the index above borrows rather than allocating a string per phrase.
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
pub(crate) mod tests {
    use super::*;
    use crate::redaction::Redactor;

    /// The two instructions the two lanes' suites repeat, near-duplicates of one another: one word
    /// changed, the punctuation and casing different, and a third that shares only vocabulary.
    pub(crate) const RULE: &str = "Always run cargo fmt and cargo clippy before you tell me a change is done, and paste the \
         output.";
    pub(crate) const RESTATED: &str =
        "always run `cargo fmt` and `cargo clippy` before you tell me any change is done";
    pub(crate) const UNRELATED: &str =
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

    /// The floor: an acknowledgement is never a request worth reporting, however many times it is
    /// typed.
    #[test]
    fn short_messages_are_counted_and_never_clustered() {
        let read = transcript(&["yes", "do it", "continue", RULE]);
        assert_eq!(read.messages, 4, "every user message is counted");
        assert_eq!(
            read.clusterable.len(),
            1,
            "only the sentence clears the floor",
        );
        assert_eq!(read.clusterable[0].text, RULE);
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
            // Copilot's own reminder marker, underscored where Claude Code's is hyphenated. Both
            // spellings are in the archive, and only one of them was certified before the `flows`
            // calibration run went looking.
            "<system_reminder>\nIMPORTANT: The tools listed below are deferred",
            "<system_reminder> Custom instructions from madari/AGENTS.md. Apply these to any code",
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

    /// This module's locator is [`crate::verbatim`]'s locator: the same event of the same transcript
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
    /// "counted, never dropped", inside one transcript.
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

    /// Builds a `SessionMessages` out of a handful of user messages, through the real reader.
    pub(crate) fn transcript(texts: &[&str]) -> SessionMessages {
        let mut lines = String::new();
        for (index, text) in texts.iter().enumerate() {
            lines.push_str(&format!(
                r#"{{"type":"user","uuid":"u{index}","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"user","content":{}}}}}"#,
                serde_json::to_string(text).expect("a JSON string"),
            ));
            lines.push('\n');
        }
        read_messages(Source::ClaudeCode, 2, lines.as_bytes()).expect("v2 is supported")
    }
}
