//! The flows fold: requests this archive shows you making again, **wherever** you made them — and
//! the multi-step sequences those requests fall into.
//!
//! This is qanungo #13's read-only half. The interpretation half is deliberately somewhere else: the
//! `skill-finder` contrib skill runs in the harness, under the user's own permission prompts, and it
//! is the thing that drafts a `SKILL.md` or an agent definition. This module computes; the harness
//! writes.
//!
//! # Why the lens is the whole archive and not one repository
//!
//! [`crate::doctor`] groups per repository because an instruction file *belongs* to a repository: a
//! finding that pooled two of them could not be acted on in either. This lane's question is the
//! opposite shape. A workflow worth turning into a skill is worth it **wherever it recurs**, and a
//! request you have made in six repositories is a stronger candidate than one you made six times in
//! one — so repositories are where a finding *lists* here, never how it groups. Concretely:
//!
//! - the index is built over every session in the reach at once, so a message in one repository is
//!   compared against messages in all of them;
//! - a session the archive attributes to **no repository joins the clustering** rather than being
//!   set aside. `doctor`'s carve-out exists because such a session has no instruction file for a
//!   finding to be about; a repeated request needs no repository to be a repeated request, so the
//!   carve-out does not apply and no session is held back from the comparison.
//!
//! Everything else — normalization, the authored filter, shingling, containment, the union-find,
//! the conversation merge — is [`crate::repetition`]'s, shared with `doctor` rather than forked.
//!
//! # What a "flow" is here, exactly
//!
//! Once the pool is clustered, every clustered message carries a **cluster id**. Within one session,
//! reading its clustered messages in file order gives a sequence of ids, and this lane mines the
//! recurring 2- and 3-step contiguous runs of that sequence — A→B and A→B→C.
//!
//! Three properties of that sequence are worth stating, because each of them is a choice:
//!
//! - **Adjacency is among *clustered* messages, not among messages.** Anything that did not cluster
//!   — an acknowledgement, a one-off question, a paste — sits between two steps without breaking
//!   them apart. That is not a tolerance grudgingly allowed, it is what the archive looks like: a
//!   real three-step workflow has a conversation in between its steps every single time.
//! - **A run of the same cluster id collapses to one step.** Restating one request twice in a row is
//!   insistence, not a two-step flow, and the first message of the run is the one cited.
//! - **A flow must recur in at least [`MIN_FLOW_SESSIONS`] distinct conversations**, on the
//!   conversation merge's reckoning rather than on session ids — a resumed session replays what came
//!   before it, and without the merge a single long sitting captured twice would be "a flow that
//!   recurred".
//!
//! A trigram's own bigrams are mined too and are reported beside it when they clear the floor; that
//! is deliberate, because A→B→C recurring three times and A→B recurring nine says something the
//! trigram alone does not.
//!
//! # What this cannot see, and the noise class it will not hide
//!
//! It reads **requests, never outcomes.** Nothing here knows whether a flow worked, how long it
//! took, what it produced, or whether the second step happened *because* of the first — the
//! sequence is an ordering in a transcript and no reading of one supports a causal claim. And
//! whether a flow is worth a skill is a judgement about the user's work, which this fold has no
//! access to: the document shows the repetition and stops.
//!
//! The pooled lens also has a known, structural noise class, and the honest thing is to name it
//! rather than to filter it by guesswork. Harness-injected prose whose opening
//! [`crate::repetition::authored`] cannot certify clusters *perfectly*, and pooled across the
//! archive it clusters with itself in every repository at once — which makes it, arithmetically,
//! among the most repeated text in the corpus. The preamble says so out loud, the remedy is a
//! certified opening added by looking at the real archive, and the triage is the skill's.
//!
//! # Redaction
//!
//! Third CLI verbatim surface, after `ask --verbatim` and `doctor`, and it holds the same line:
//! clustering runs on the **unscrubbed** transcript text (a secret must not decide what clusters),
//! and every excerpt is scrubbed on the way into a [`RequestCluster`] or a [`Flow`] through
//! [`crate::ask::snippet`]'s pipeline — scrub, then collapse, then clip, in that order, because
//! clipping first could cut a credential in half and render the surviving head. Only the excerpts
//! that are actually rendered are scrubbed, so the footer's counts describe the document rather than
//! the corpus behind it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use crate::ask::snippet;
use crate::format;
use crate::redaction::{RedactionReport, Redactor};
use crate::repetition::{
    Citation, Clustering, MAX_CITATIONS_PER_CLUSTER, MIN_CLUSTER_SESSIONS, Occurrence, Repetition,
    SessionRecord,
};
use crate::report::SkippedNote;
use crate::standup::NO_REPOSITORY;

/// Distinct conversations a multi-step flow has to recur in before it is reported.
///
/// **A tunable, not a decision — and arbitrary until measured**, exactly like
/// [`MIN_CLUSTER_SESSIONS`], which it is deliberately set equal to. Two is the floor the claim rests
/// on: a sequence that happened once is a conversation, not a flow, and no amount of looking at one
/// session can make it otherwise. It is left at the floor rather than raised because raising it
/// trades noise for silence about genuine pairs, and this lane's reader is a triage step that can
/// discard noise but cannot recover what was never printed.
pub const MIN_FLOW_SESSIONS: usize = MIN_CLUSTER_SESSIONS;

/// Steps in the shortest flow this lane mines. Two: one step is a cluster, which the section above
/// it already reports.
pub const MIN_FLOW_STEPS: usize = 2;

/// Steps in the longest flow this lane mines.
///
/// **A tunable, not a decision.** Three is where the issue's own sentence sits ("you've run this
/// 3-step flow N times"), and it is also where the arithmetic turns: every extra step multiplies the
/// candidate keys while making each one rarer, so a 4-step flow that clears
/// [`MIN_FLOW_SESSIONS`] is nearly always a 3-step flow with one more session-specific message
/// stapled on. Raising it is a one-line change and the mining loop is written for any width; it is
/// held at three until a real archive shows a 4-step recurrence the trigram misses.
pub const MAX_FLOW_STEPS: usize = 3;

/// Clusters the "Repeated requests" section renders before the list is cut short, when nobody says
/// otherwise.
///
/// **A default, not a ceiling** — `--clusters` raises it, on qanungo #16's finding and with its
/// semantics: the cut is on the *rendering* alone, every count is taken before it, and the document
/// states the number in force whenever it is not this one.
///
/// Twenty rather than `doctor`'s ten because the two cut different things: `doctor`'s ten is *per
/// repository* across as many sections as the archive has repositories, and this is one list for the
/// whole archive. A page of twenty is the same amount of reading.
pub const DEFAULT_CLUSTERS: usize = 20;

/// Flows the "Multi-step flows" section renders before the list is cut short. `--flows` raises it,
/// with [`DEFAULT_CLUSTERS`]'s semantics and for its reason.
pub const DEFAULT_FLOWS: usize = 20;

/// Repositories one row names before the rest are summarized as a remainder.
///
/// A tunable. The point of the per-repository tally is the *shape* of a finding — one repository
/// nine times, or nine repositories once each — and six entries settle that question while keeping a
/// cluster to a paragraph. The total travels beside them
/// ([`RequestCluster::repositories_found`]) so a cut list is never mistaken for the whole.
pub const MAX_REPOSITORIES_PER_ROW: usize = 6;

/// Instances one flow cites before the list is cut short. [`MAX_CITATIONS_PER_CLUSTER`]'s reasoning,
/// applied to a citation that carries every step's ordinal at once.
pub const MAX_INSTANCES_PER_FLOW: usize = MAX_CITATIONS_PER_CLUSTER;

/// One session, as this lane takes it in — the record [`crate::doctor`] takes too.
pub type FlowsSession = SessionRecord;

/// How often one repository carried a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryCount {
    /// The repository, clamped and then scrubbed, or [`NO_REPOSITORY`].
    pub repository: String,
    pub occurrences: usize,
}

/// One request the archive shows being made more than once, anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCluster {
    /// Messages in the cluster, across every session it spans.
    pub occurrences: usize,
    /// Distinct *conversations* it spans. At least [`MIN_CLUSTER_SESSIONS`], by construction.
    pub sessions: usize,
    /// A scrubbed excerpt of the fullest occurrence.
    pub excerpt: String,
    /// Which repositories carried it, most first, at most [`MAX_REPOSITORIES_PER_ROW`] of them.
    pub repositories: Vec<RepositoryCount>,
    /// How many carried it before that cut.
    pub repositories_found: usize,
    /// Where the occurrences were, newest first, at most [`MAX_CITATIONS_PER_CLUSTER`] of them.
    pub citations: Vec<Citation>,
}

/// One occurrence of a flow: one session, and the event ordinal of each of its steps in that
/// session's transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowInstance {
    pub archived_at: Option<DateTime<Utc>>,
    pub source_hash: String,
    /// One ordinal per step, in the flow's own order. Where a step is a run of the same request
    /// restated, this is the ordinal of the **first** message of that run.
    pub locators: Vec<u64>,
}

/// One multi-step sequence of repeated requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    /// One scrubbed excerpt per step, in order.
    pub steps: Vec<String>,
    /// How many times the sequence occurred, across every session.
    pub occurrences: usize,
    /// Distinct conversations it occurred in. At least [`MIN_FLOW_SESSIONS`], by construction.
    pub sessions: usize,
    /// Which repositories carried it, most first, at most [`MAX_REPOSITORIES_PER_ROW`] of them.
    pub repositories: Vec<RepositoryCount>,
    /// How many carried it before that cut.
    pub repositories_found: usize,
    /// Where the occurrences were, newest first, at most [`MAX_INSTANCES_PER_FLOW`] of them.
    pub instances: Vec<FlowInstance>,
}

/// One repository the reach covered — read and compared, because this lane excludes none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRead {
    /// The repository, clamped and then scrubbed, or [`NO_REPOSITORY`].
    pub repository: String,
    pub sessions: usize,
    /// Messages of those sessions that were long enough, and authored, to compare.
    pub clusterable: usize,
}

/// Everything one flows document is rendered from.
#[derive(Debug, Clone, Default)]
pub struct Flows {
    /// The clusters this document renders, best first, at most the cap [`Flows::fold`] was given.
    pub clusters: Vec<RequestCluster>,
    /// How many cleared the thresholds before that cut.
    pub clusters_found: usize,
    /// The flows this document renders, best first, at most the cap [`Flows::fold`] was given.
    pub flows: Vec<Flow>,
    /// How many cleared the thresholds before that cut.
    pub flows_found: usize,
    /// Every repository the reach covered, busiest first, with the unattributed bucket last.
    pub repositories: Vec<RepositoryRead>,
    /// Sessions that contributed nothing, grouped by reason. Built by the caller, which is the only
    /// thing that knows what the mirror skipped.
    pub gaps: Vec<SkippedNote>,
    /// What the scrub fired across every string above. Counts only.
    pub redaction: RedactionReport,
    /// Sessions whose transcript was read.
    pub sessions: usize,
    /// Every user message read, across every session.
    pub messages: usize,
    /// How many of them the harness wrote rather than a person, and this build recognized as such.
    pub harness_generated: usize,
    /// How many of them were long enough, and authored, to compare.
    pub clusterable: usize,
    /// Distinct conversations those sessions turned out to be — two sessions that replay the same
    /// conversation count once.
    pub conversations: usize,
    /// Sessions that carried no user message at all, counted rather than silently contributing an
    /// empty result.
    pub sessions_without_messages: usize,
    /// Decompressed transcript bytes read.
    pub bytes_folded: u64,
    /// Records `munshi-transcript` could not read, across every session.
    pub unreadable_records: u64,
}

impl Flows {
    /// Clusters one reach's user messages across every repository at once, then mines the flows.
    ///
    /// `gaps` and `gap_redaction` come from the caller, for [`crate::doctor::Doctor::fold`]'s
    /// reason: summarizing what the *mirror* skipped needs the mirror's own vocabulary, and the
    /// scrub that summary ran has to reach this document's footer.
    ///
    /// `clusters` and `flows` are the two rendering cuts. They bound what each section *shows* and
    /// nothing else: [`Flows::clusters_found`] and [`Flows::flows_found`] are taken before either
    /// cut, so raising one reveals findings without moving a single count.
    pub fn fold(
        sessions: &[FlowsSession],
        gaps: Vec<SkippedNote>,
        gap_redaction: &RedactionReport,
        redactor: &Redactor,
        clusters: usize,
        flows: usize,
    ) -> Self {
        let mut redaction = RedactionReport::default();
        redaction.absorb(gap_redaction);
        let mut counted = Self {
            gaps,
            sessions: sessions.len(),
            ..Self::default()
        };

        // Labels are keyed on the listing's own repository string, unscrubbed, exactly as `doctor`
        // keys its grouping: two different repositories that happened to scrub to the same marker
        // must stay two rows. The transform runs once per distinct repository and every label it
        // produces is rendered, in the coverage section if nowhere else, so the footer's counts stay
        // a description of this document.
        let mut labels: BTreeMap<Option<&str>, String> = BTreeMap::new();
        let mut coverage: BTreeMap<Option<&str>, (usize, usize)> = BTreeMap::new();
        for session in sessions {
            counted.messages += session.messages.messages;
            counted.clusterable += session.messages.clusterable.len();
            counted.harness_generated += session.messages.harness_generated;
            counted.bytes_folded += session.bytes_folded;
            counted.unreadable_records += session.messages.unreadable_records;
            if session.messages.messages == 0 {
                counted.sessions_without_messages += 1;
            }
            let key = session.repository.as_deref();
            labels.entry(key).or_insert_with(|| match key {
                // Clamp, then scrub — [`crate::evidence::identifier_field`]'s order, because the
                // clamp has to judge the archive's own bytes or an over-length token would launder
                // itself into a renderable marker.
                Some(value) => {
                    let scrubbed = redactor.scrub(&format::identifier(value));
                    redaction.absorb(&scrubbed.report);
                    scrubbed.text
                }
                None => NO_REPOSITORY.to_owned(),
            });
            let seen = coverage.entry(key).or_insert((0, 0));
            seen.0 += 1;
            seen.1 += session.messages.clusterable.len();
        }
        counted.repositories = coverage
            .into_iter()
            .map(|(key, (sessions, clusterable))| RepositoryRead {
                repository: labels[&key].clone(),
                sessions,
                clusterable,
            })
            .collect();
        counted.repositories.sort_by(busiest_first);

        // One pool, every session in it — the whole difference from `doctor`, in one line.
        let held: Vec<&FlowsSession> = sessions.iter().collect();
        let mut clustering = Clustering::of(&held);
        let found = clustering.repetitions();
        counted.conversations = clustering.conversations();
        counted.clusters_found = found.len();

        // A cluster's id is its index here, which is the shared module's union-find root order: a
        // total order that depends on nothing but the pool's contents. Every ranking below is
        // computed over these ids and never over their positions in a rendered list, so a cut can
        // never change which flow is which.
        let sequences = sequences(&clustering, &found, sessions.len());
        let mined = mine(&sequences);
        // Every candidate's conversation span, resolved once and in key order before anything is
        // sorted or cut. Answering it needs the clustering *mutably* — the session union-find
        // compresses paths as it answers — and a comparator cannot hold a mutable borrow, so the
        // counts are taken here and read from the map everywhere below.
        let spans: BTreeMap<&Vec<usize>, usize> = mined
            .iter()
            .map(|(key, runs)| (key, conversations_of(&mut clustering, runs)))
            .collect();
        let mut flow_keys: Vec<(&Vec<usize>, &Vec<FlowRun>)> = mined
            .iter()
            .filter(|(key, _)| spans[key] >= MIN_FLOW_SESSIONS)
            .collect();
        counted.flows_found = flow_keys.len();
        flow_keys.sort_by(|left, right| {
            best_flow_first(
                left,
                right,
                &spans,
                &found,
                sessions,
                clustering.occurrences(),
            )
        });

        let mut ranked: Vec<usize> = (0..found.len()).collect();
        ranked.sort_by(|&left, &right| most_repeated_first(&found[left], &found[right]));
        ranked.truncate(clusters);
        flow_keys.truncate(flows);

        // Scrub exactly the excerpts this document is about to render, once each: a cluster that is
        // both in the list above and a step of a flow below is one excerpt, quoted twice.
        let mut needed: BTreeSet<usize> = ranked.iter().copied().collect();
        for (key, _) in &flow_keys {
            needed.extend(key.iter().copied());
        }
        let excerpts: BTreeMap<usize, String> = needed
            .into_iter()
            .map(|id| {
                // Scrub, then collapse, then clip — [`crate::ask::snippet`]'s pipeline, so every
                // quotation this crate prints is cut at one length by one rule and none of them is
                // cut before it is scrubbed.
                let scrubbed = redactor.scrub(&found[id].representative);
                redaction.absorb(&scrubbed.report);
                (id, snippet(&scrubbed.text))
            })
            .collect();

        counted.clusters = ranked
            .into_iter()
            .map(|id| {
                let repetition = &found[id];
                let (repositories, repositories_found) = tally(
                    repetition
                        .positions
                        .iter()
                        .map(|&position| clustering.occurrences()[position].session()),
                    sessions,
                    &labels,
                );
                RequestCluster {
                    occurrences: repetition.occurrences.len(),
                    sessions: repetition.sessions,
                    excerpt: excerpts[&id].clone(),
                    repositories,
                    repositories_found,
                    citations: repetition
                        .occurrences
                        .iter()
                        .take(MAX_CITATIONS_PER_CLUSTER)
                        .cloned()
                        .collect(),
                }
            })
            .collect();

        counted.flows = flow_keys
            .into_iter()
            .map(|(key, runs)| {
                let (repositories, repositories_found) =
                    tally(runs.iter().map(|run| run.session), sessions, &labels);
                let mut instances: Vec<FlowInstance> = runs
                    .iter()
                    .map(|run| FlowInstance {
                        archived_at: sessions[run.session].archived_at,
                        source_hash: sessions[run.session].source_hash.clone(),
                        locators: run
                            .positions
                            .iter()
                            .map(|&position| clustering.occurrences()[position].locator())
                            .collect(),
                    })
                    .collect();
                instances.sort_by(newest_instance_first);
                Flow {
                    steps: key.iter().map(|id| excerpts[id].clone()).collect(),
                    occurrences: runs.len(),
                    sessions: spans[&key],
                    repositories,
                    repositories_found,
                    instances: {
                        instances.truncate(MAX_INSTANCES_PER_FLOW);
                        instances
                    },
                }
            })
            .collect();

        counted.redaction = redaction;
        counted
    }

    /// Whether anything cleared the thresholds at all.
    pub fn is_empty(&self) -> bool {
        self.clusters_found == 0
    }
}

/// One occurrence of one flow: the session it happened in, and the occurrence position of each step.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FlowRun {
    session: usize,
    positions: Vec<usize>,
}

/// Each session's clustered messages, in file order, as `(occurrence position, cluster id)` — with
/// runs of one id collapsed to their first message.
///
/// The collapse is the module docs' second property: a request restated twice in a row is one step.
/// Keeping the *first* position of a run rather than the last is what makes a citation point at
/// where the step began.
fn sequences(
    clustering: &Clustering<'_>,
    found: &[Repetition],
    sessions: usize,
) -> Vec<Vec<(usize, usize)>> {
    let mut assignment: Vec<Option<usize>> = vec![None; clustering.occurrences().len()];
    for (id, repetition) in found.iter().enumerate() {
        for &position in &repetition.positions {
            assignment[position] = Some(id);
        }
    }
    let mut sequences: Vec<Vec<(usize, usize)>> = vec![Vec::new(); sessions];
    // The occurrence list is session-then-file order by construction, so one forward pass yields
    // every session's messages already in the order they were typed.
    for (position, occurrence) in clustering.occurrences().iter().enumerate() {
        let Some(id) = assignment[position] else {
            continue;
        };
        let sequence = &mut sequences[occurrence.session()];
        if sequence.last().is_some_and(|(_, last)| *last == id) {
            continue;
        }
        sequence.push((position, id));
    }
    sequences
}

/// Mines the recurring contiguous runs of [`MIN_FLOW_STEPS`]..=[`MAX_FLOW_STEPS`] cluster ids.
///
/// Keyed on the id sequence itself, in a `BTreeMap`, so the mining is exact — no hashing that could
/// collide two flows — and the iteration order is the keys' own, which depends on nothing but the
/// pool.
fn mine(sequences: &[Vec<(usize, usize)>]) -> BTreeMap<Vec<usize>, Vec<FlowRun>> {
    let mut mined: BTreeMap<Vec<usize>, Vec<FlowRun>> = BTreeMap::new();
    for (session, sequence) in sequences.iter().enumerate() {
        for width in MIN_FLOW_STEPS..=MAX_FLOW_STEPS {
            if sequence.len() < width {
                break;
            }
            for window in sequence.windows(width) {
                mined
                    .entry(window.iter().map(|(_, id)| *id).collect())
                    .or_default()
                    .push(FlowRun {
                        session,
                        positions: window.iter().map(|(position, _)| *position).collect(),
                    });
            }
        }
    }
    mined
}

/// Distinct conversations a flow's occurrences fall in — sessions that replay one another count
/// once, for the same reason a cluster's session span does.
fn conversations_of(clustering: &mut Clustering<'_>, runs: &[FlowRun]) -> usize {
    runs.iter()
        .map(|run| clustering.conversation_of(run.session))
        .collect::<BTreeSet<_>>()
        .len()
}

/// Counts each repository's share of a finding, most first, cut with the total beside it.
///
/// The sessions are mapped through the label map built in [`Flows::fold`], so a repository's name is
/// clamped and scrubbed once for the whole document and two rows can never disagree about it.
fn tally(
    sessions_of: impl Iterator<Item = usize>,
    sessions: &[FlowsSession],
    labels: &BTreeMap<Option<&str>, String>,
) -> (Vec<RepositoryCount>, usize) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for session in sessions_of {
        *counts
            .entry(labels[&sessions[session].repository.as_deref()].as_str())
            .or_default() += 1;
    }
    let found = counts.len();
    let mut tallied: Vec<RepositoryCount> = counts
        .into_iter()
        .map(|(repository, occurrences)| RepositoryCount {
            repository: repository.to_owned(),
            occurrences,
        })
        .collect();
    tallied.sort_by(|left, right| {
        right
            .occurrences
            .cmp(&left.occurrences)
            .then_with(|| left.repository.cmp(&right.repository))
    });
    tallied.truncate(MAX_REPOSITORIES_PER_ROW);
    (tallied, found)
}

/// The most-repeated cluster first, with a total order behind it so the same pool ranks the same way
/// every run.
fn most_repeated_first(left: &Repetition, right: &Repetition) -> std::cmp::Ordering {
    right
        .occurrences
        .len()
        .cmp(&left.occurrences.len())
        .then_with(|| right.sessions.cmp(&left.sessions))
        .then_with(|| left.representative.cmp(&right.representative))
}

/// The best flow first: most occurrences, then most conversations, then the longer flow, then the
/// steps' own wording.
///
/// The longer flow winning a tie is deliberate. A trigram and its own leading bigram often recur the
/// same number of times, and of the two the trigram is the one that says more.
///
/// # The last tie-break is wording, and it cannot be the cluster ids
///
/// A cluster id is its position in the union-find's root order, which is a position in the flat
/// occurrence list — so it depends on **the order the sessions were handed in**, which is the
/// archive's listing order rather than a fact about the archive. Breaking a tie on the id sequence
/// therefore produced a document that reordered itself when the same sessions arrived in a different
/// order, which a test caught. The steps' representative text is derived from the transcripts alone,
/// so it is stable under any listing order.
///
/// # Wording is not quite total on its own, so the occurrences settle it
///
/// Two *distinct* flows whose step wording is byte-identical would fall back to whatever the stable
/// sort had, which is listing order again — and the two would not be interchangeable, because their
/// citations differ. So the last word goes to [`instance_key`]: the transcript hash and step
/// ordinals of every occurrence, canonically ordered, which is archive-derived and total (see its
/// own docs for why two distinct flows cannot share one).
///
/// Whether the wording tie is reachable at all is a separate question, argued in [`instance_key`];
/// the compare is here so the ordering does not depend on that argument being right.
fn best_flow_first(
    left: &(&Vec<usize>, &Vec<FlowRun>),
    right: &(&Vec<usize>, &Vec<FlowRun>),
    spans: &BTreeMap<&Vec<usize>, usize>,
    found: &[Repetition],
    sessions: &[FlowsSession],
    occurrences: &[Occurrence<'_>],
) -> std::cmp::Ordering {
    let wording = |key: &Vec<usize>| -> Vec<&str> {
        key.iter()
            .map(|&id| found[id].representative.as_str())
            .collect()
    };
    right
        .1
        .len()
        .cmp(&left.1.len())
        .then_with(|| spans[&right.0].cmp(&spans[&left.0]))
        .then_with(|| right.0.len().cmp(&left.0.len()))
        .then_with(|| wording(left.0).cmp(&wording(right.0)))
        .then_with(|| {
            instance_key(left.1, sessions, occurrences).cmp(&instance_key(
                right.1,
                sessions,
                occurrences,
            ))
        })
}

/// One flow's occurrences as an archive-derived key: `(transcript hash, step ordinals)` per
/// occurrence, sorted into a canonical order that does not depend on how the sessions were listed.
///
/// # Why this is total over distinct flows
///
/// Each occurrence's ordinals are the events its steps sat at, and every clustered message belongs
/// to exactly one cluster — so a flow's *positions* determine its cluster-id sequence. Two flows with
/// the same key would therefore have the same positions and hence the same key sequence, which makes
/// them the same flow. Distinct flows have distinct keys.
///
/// # And why there is no test for the tie it settles
///
/// Reaching the compare needs two distinct clusters with byte-identical representatives, and that
/// looks unreachable rather than merely rare: two byte-identical messages share *all* their phrases,
/// so either enough of those phrases are under [`crate::repetition::MAX_SHINGLE_POSTINGS`] and the
/// pair joins into one cluster, or a majority are over it — in which case neither copy can join
/// anything at all, since the same phrases are skipped from whichever side gathers candidates, and
/// both are dropped as single-message groups. Single linkage keeps that symmetric: any mate close
/// enough to join one copy is exactly as close to the other, so it merges them rather than separating
/// them.
///
/// A fixture would therefore have to be built around an argument that says the fixture is impossible,
/// which is not a test. The compare is written anyway because that argument is subtle, and an
/// ordering should not rest on it.
fn instance_key<'a>(
    runs: &[FlowRun],
    sessions: &'a [FlowsSession],
    occurrences: &[Occurrence<'_>],
) -> Vec<(&'a str, Vec<u64>)> {
    let mut keyed: Vec<(&str, Vec<u64>)> = runs
        .iter()
        .map(|run| {
            (
                sessions[run.session].source_hash.as_str(),
                run.positions
                    .iter()
                    .map(|&position| occurrences[position].locator())
                    .collect(),
            )
        })
        .collect();
    keyed.sort();
    keyed
}

/// Newest instance first, on the total order every citation list in this crate is sorted by.
fn newest_instance_first(left: &FlowInstance, right: &FlowInstance) -> std::cmp::Ordering {
    left.archived_at
        .is_none()
        .cmp(&right.archived_at.is_none())
        .then_with(|| right.archived_at.cmp(&left.archived_at))
        .then_with(|| left.source_hash.cmp(&right.source_hash))
        .then_with(|| left.locators.cmp(&right.locators))
}

/// The repository with the most sessions first, with the unattributed bucket last —
/// [`crate::standup`]'s rule, for its reason: the bucket is the absence of a place, not a busy one.
fn busiest_first(left: &RepositoryRead, right: &RepositoryRead) -> std::cmp::Ordering {
    let unattributed = |read: &RepositoryRead| read.repository == NO_REPOSITORY;
    unattributed(left)
        .cmp(&unattributed(right))
        .then_with(|| right.sessions.cmp(&left.sessions))
        .then_with(|| right.clusterable.cmp(&left.clusterable))
        .then_with(|| left.repository.cmp(&right.repository))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::MAX_SNIPPET_CHARS;
    use crate::repetition::{Instruction, SessionMessages, normalize};

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
    ) -> FlowsSession {
        FlowsSession {
            source_hash: hash.to_string().repeat(64),
            archived_at: Some(at(archived_at)),
            repository: repository.map(str::to_owned),
            bytes_folded: 1_000,
            messages: SessionMessages {
                clusterable: texts
                    .iter()
                    .enumerate()
                    .map(|(index, text)| Instruction {
                        locator: index as u64 + 1,
                        normalized: normalize(text),
                        text: (*text).to_owned(),
                    })
                    .collect(),
                messages: texts.len(),
                harness_generated: 0,
                after_error: 0,
                unreadable_records: 0,
            },
        }
    }

    fn fold(sessions: &[FlowsSession]) -> Flows {
        capped(sessions, DEFAULT_CLUSTERS, DEFAULT_FLOWS)
    }

    fn capped(sessions: &[FlowsSession], clusters: usize, flows: usize) -> Flows {
        Flows::fold(
            sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new(),
            clusters,
            flows,
        )
    }

    /// A request, built rather than hand-written for [`crate::doctor`]'s reason: what is under test
    /// is the pooling and the mining, and a handful of realistic sentences chosen to stay under the
    /// similarity threshold would be sentences chosen to prove something else. The tag appears three
    /// times, so any two of these share only the phrases none of the three positions falls in.
    fn request(tag: &str) -> String {
        format!(
            "please regenerate the {tag} bindings and rerun the {tag} fixtures before you tell me \
             the {tag} migration is finished"
        )
    }

    /// The same request restated: most phrases in common, so it clusters, and different enough that
    /// a whole-message comparison would miss it.
    fn restated(tag: &str) -> String {
        format!(
            "please regenerate the {tag} bindings and rerun the {tag} fixtures before you tell me \
             any {tag} migration is done"
        )
    }

    /// A message long enough to be compared and unique enough to cluster with nothing — the
    /// conversation that sits *between* two steps of a real flow.
    ///
    /// The tag repeats three times for [`request`]'s reason, and here it is load-bearing rather than
    /// tidy: an earlier version varied one word, which made every two asides near-duplicates of each
    /// other. They then clustered, the sessions carrying them shared three clusters, and the
    /// conversation merge folded the fixture into a single sitting — a test that proved the opposite
    /// of what it claimed to.
    fn chatter(tag: &str) -> String {
        format!(
            "a one off {tag} aside about the {tag} console that nobody in this {tag} archive ever \
             repeats"
        )
    }

    /// The property [`crate::doctor`] deliberately refuses: the same request in two repositories is
    /// one finding here, listed by the repositories it turned up in.
    #[test]
    fn a_repeated_request_clusters_across_repositories() {
        let first = request("codegen");
        let second = restated("codegen");
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
                Some("surdy/munshi"),
                &[&second],
            ),
        ];
        let found = fold(&sessions);
        assert_eq!(found.clusters_found, 1, "one pool, one cluster");
        let cluster = &found.clusters[0];
        assert_eq!(cluster.occurrences, 2);
        assert_eq!(cluster.sessions, 2);
        assert_eq!(cluster.repositories_found, 2);
        assert_eq!(
            cluster
                .repositories
                .iter()
                .map(|count| (count.repository.as_str(), count.occurrences))
                .collect::<Vec<_>>(),
            vec![("surdy/munshi", 1), ("surdy/qanungo", 1)],
            "tied counts fall back to the label, so the order is total",
        );
        // The longest occurrence speaks for the cluster, as it does in every lane that quotes one.
        assert!(cluster.excerpt.starts_with("please regenerate the codegen"));
        assert_eq!(cluster.citations.len(), 2);
        assert_eq!(cluster.citations[0].source_hash, "b".repeat(64));
    }

    /// The carve-out `doctor` makes and this lane does not: a session the archive attributes to no
    /// repository is compared like any other, because a repeated request needs no repository to be
    /// one. It is listed under the bucket's own label, and the bucket sorts last.
    #[test]
    fn a_session_with_no_repository_joins_the_clustering() {
        let first = request("dns");
        let second = restated("dns");
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", None, &[&first]),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/quadhost"),
                &[&second],
            ),
        ];
        let found = fold(&sessions);
        assert_eq!(found.clusters_found, 1, "the unattributed session is in");
        assert_eq!(found.clusters[0].repositories_found, 2);
        assert!(
            found.clusters[0]
                .repositories
                .iter()
                .any(|count| count.repository == NO_REPOSITORY),
        );
        // And both are listed in the coverage section, with the bucket last.
        assert_eq!(found.repositories.len(), 2);
        assert_eq!(found.repositories[1].repository, NO_REPOSITORY);
        assert_eq!(found.repositories[0].repository, "surdy/quadhost");
        assert_eq!(found.repositories[0].clusterable, 1);
    }

    /// The core flow property: two requests in the same order in two sessions is a two-step flow,
    /// and its citation carries one event ordinal per step.
    #[test]
    fn a_two_step_flow_recurring_in_two_sessions_is_found() {
        let (first, second) = (request("schema"), request("deploy"));
        let steps: Vec<&str> = vec![&first, &second];
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", Some("surdy/qanungo"), &steps),
            session('b', "2026-08-21T10:00:00Z", Some("surdy/munshi"), &steps),
        ];
        let found = fold(&sessions);
        assert_eq!(found.clusters_found, 2);
        assert_eq!(found.flows_found, 1, "one bigram, no trigram to be had");
        let flow = &found.flows[0];
        assert_eq!(flow.steps.len(), 2);
        assert!(flow.steps[0].contains("schema"), "{:?}", flow.steps);
        assert!(flow.steps[1].contains("deploy"), "{:?}", flow.steps);
        assert_eq!(flow.occurrences, 2);
        assert_eq!(flow.sessions, 2);
        assert_eq!(flow.repositories_found, 2);
        assert_eq!(flow.instances.len(), 2);
        assert_eq!(
            flow.instances[0].source_hash,
            "b".repeat(64),
            "newest first"
        );
        assert_eq!(flow.instances[0].locators, vec![1, 2]);
    }

    /// Adjacency is among *clustered* messages: anything that did not cluster sits between two steps
    /// without breaking them apart, which is what every real workflow looks like. The ordinals in
    /// the citation are the steps' own, so the gap is visible in the numbers rather than hidden.
    #[test]
    fn unclustered_chatter_between_steps_does_not_break_a_flow() {
        let (first, second) = (request("schema"), request("deploy"));
        let (aside, other) = (chatter("monday"), chatter("tuesday"));
        let sessions = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[&first, &aside, &second],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/qanungo"),
                &[&first, &other, &second],
            ),
        ];
        let found = fold(&sessions);
        assert_eq!(
            found.clusters_found, 2,
            "the asides are comparable and cluster with nothing",
        );
        assert_eq!(found.flows_found, 1);
        assert_eq!(found.flows[0].occurrences, 2);
        assert_eq!(
            found.flows[0].instances[0].locators,
            vec![1, 3],
            "the ordinals are the steps' own, and the gap shows",
        );
    }

    /// A sequence that happened once is a conversation, not a flow — the floor the whole claim rests
    /// on, refused even when both of its steps are genuine clusters.
    #[test]
    fn a_sequence_confined_to_one_session_is_not_a_flow() {
        let (first, second) = (request("schema"), request("deploy"));
        let sessions = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[&first, &second],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/qanungo"),
                &[&first],
            ),
            session(
                'c',
                "2026-08-22T10:00:00Z",
                Some("surdy/munshi"),
                &[&second],
            ),
        ];
        let found = fold(&sessions);
        assert_eq!(found.clusters_found, 2, "both steps really are clusters");
        assert_eq!(
            found.flows_found, 0,
            "and the order they arrived in happened once: {:?}",
            found.flows,
        );
    }

    /// A request restated back-to-back is insistence, not a two-step flow: the run collapses to one
    /// step, and the ordinal cited is where the run began.
    #[test]
    fn a_restated_step_collapses_to_one_step() {
        let (first, again, second) = (request("schema"), restated("schema"), request("deploy"));
        let sessions = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[&first, &again, &second],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/qanungo"),
                &[&first, &second],
            ),
        ];
        let found = fold(&sessions);
        assert_eq!(found.clusters_found, 2);
        assert_eq!(
            found.flows_found, 1,
            "not a three-step flow: {:?}",
            found.flows
        );
        let flow = &found.flows[0];
        assert_eq!(flow.steps.len(), 2);
        assert_eq!(flow.occurrences, 2);
        assert_eq!(
            flow.instances[1].locators,
            vec![1, 3],
            "the run's first message is the step's ordinal",
        );
    }

    /// A three-step flow's own two-step runs are mined beside it, and at an equal weight the longer
    /// one is the one that says more, so it sorts first.
    ///
    /// The sessions carry four unique messages each on purpose: two sessions whose *whole*
    /// clusterable list is the three shared steps are a replayed conversation by the merge's
    /// reckoning, and would be folded into one before any of this could be found.
    #[test]
    fn a_trigram_is_reported_beside_its_own_bigrams() {
        let steps: Vec<String> = ["schema", "deploy", "verify"]
            .iter()
            .map(|tag| request(tag))
            .collect();
        let build = |session: char, at: &str, tag: &str| {
            let padding: Vec<String> = (0..4)
                .map(|index| chatter(&format!("{tag}{index}")))
                .collect();
            let texts: Vec<&str> = steps
                .iter()
                .map(String::as_str)
                .chain(padding.iter().map(String::as_str))
                .collect();
            (
                session,
                at.to_owned(),
                texts
                    .iter()
                    .map(|text| (*text).to_owned())
                    .collect::<Vec<_>>(),
            )
        };
        let (_, _, first) = build('a', "2026-08-20T10:00:00Z", "mon");
        let (_, _, second) = build('b', "2026-08-21T10:00:00Z", "tue");
        let first: Vec<&str> = first.iter().map(String::as_str).collect();
        let second: Vec<&str> = second.iter().map(String::as_str).collect();
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", Some("surdy/qanungo"), &first),
            session('b', "2026-08-21T10:00:00Z", Some("surdy/qanungo"), &second),
        ];
        let found = fold(&sessions);
        assert_eq!(
            found.conversations, 2,
            "the padding keeps them two sittings"
        );
        assert_eq!(found.clusters_found, 3);
        assert_eq!(
            found.flows_found,
            3,
            "A→B→C, A→B and B→C: {:?}",
            found
                .flows
                .iter()
                .map(|flow| flow.steps.len())
                .collect::<Vec<_>>(),
        );
        assert_eq!(found.flows[0].steps.len(), 3, "the longer flow leads");
        assert_eq!(found.flows[0].occurrences, 2);
        assert_eq!(found.flows[0].instances[0].locators.len(), 3);
        assert!(found.flows[1..].iter().all(|flow| flow.steps.len() == 2));

        // The property the last tie-break rests on, asserted where it is reachable: distinct flows
        // carry distinct occurrence lists, because a flow's positions determine its steps. See
        // [`instance_key`] for why the wording tie that would consult them looks unreachable.
        for (index, flow) in found.flows.iter().enumerate() {
            for other in &found.flows[index + 1..] {
                assert_ne!(
                    (&flow.steps, &flow.instances),
                    (&other.steps, &other.instances),
                    "two flows are indistinguishable in the document",
                );
            }
        }
    }

    /// The largest false finding this lane could make, refused: two sessions that replay one
    /// conversation are one conversation, so nothing in them recurs.
    #[test]
    fn a_replayed_conversation_manufactures_no_flow() {
        let conversation: Vec<String> = ["schema", "deploy", "verify", "ship"]
            .iter()
            .map(|tag| request(tag))
            .collect();
        let texts: Vec<&str> = conversation.iter().map(String::as_str).collect();
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", Some("surdy/qanungo"), &texts),
            session('b', "2026-08-20T10:01:00Z", Some("surdy/qanungo"), &texts),
        ];
        let found = fold(&sessions);
        assert_eq!(found.conversations, 1, "one conversation captured twice");
        assert!(
            found.clusters.is_empty(),
            "no request crossed a conversation boundary: {:?}",
            found.clusters,
        );
        assert_eq!(
            found.flows_found, 0,
            "and a replay is not a recurrence: {:?}",
            found.flows,
        );
        assert_eq!(found.sessions, 2, "both sessions were still read");
    }

    /// The same corpus folded twice is the same document, and neither the clustering nor the mining
    /// depends on the order the sessions arrived in.
    #[test]
    fn the_fold_is_deterministic_and_order_independent() {
        let (first, second, third) = (request("schema"), request("deploy"), request("verify"));
        let forwards = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[&first, &second],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/munshi"),
                &[&first, &second],
            ),
            session('c', "2026-08-22T10:00:00Z", None, &[&third, &first]),
            session(
                'd',
                "2026-08-23T10:00:00Z",
                Some("surdy/rz"),
                &[&third, &first],
            ),
        ];
        let mut backwards = forwards.clone().to_vec();
        backwards.reverse();

        let first_fold = fold(&forwards);
        assert_eq!(first_fold.clusters, fold(&forwards).clusters);
        assert_eq!(first_fold.flows, fold(&forwards).flows);
        let reversed = fold(&backwards);
        assert_eq!(
            first_fold.clusters, reversed.clusters,
            "the listing order is not part of the finding",
        );
        assert_eq!(first_fold.flows, reversed.flows);
        assert_eq!(first_fold.flows_found, 2, "A→B and C→A");
    }

    /// Both cuts are defaults the caller can raise (qanungo #16's semantics): raising one reveals a
    /// prefix of what was already found and moves no count.
    #[test]
    fn the_two_cuts_are_defaults_the_caller_can_raise() {
        // Two sessions per request, so each pair is a repetition rather than one long replayed
        // conversation.
        let texts: Vec<String> = (0..12)
            .map(|index| request(&format!("lane{index}")))
            .collect();
        let sessions: Vec<FlowsSession> = texts
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
                        Some("surdy/munshi"),
                        &[text],
                    ),
                ]
            })
            .collect();

        let cut = capped(&sessions, 5, DEFAULT_FLOWS);
        assert_eq!(cut.clusters_found, 12, "twelve requests, twelve clusters");
        assert_eq!(cut.clusters.len(), 5);

        let raised = capped(&sessions, 50, DEFAULT_FLOWS);
        assert_eq!(
            raised.clusters.len(),
            12,
            "a cap above what was found invents nothing"
        );
        assert_eq!(
            raised.clusters[..5],
            cut.clusters[..],
            "the cut takes a prefix: raising it reveals, it never reorders",
        );
        assert_eq!(raised.clusters_found, cut.clusters_found, "no count moved");
    }

    /// The flow cut behaves the same way, on its own list.
    #[test]
    fn the_flow_cut_is_a_default_the_caller_can_raise() {
        let steps: Vec<String> = ["schema", "deploy", "verify"]
            .iter()
            .map(|tag| request(tag))
            .collect();
        let pad = |tag: &str| -> Vec<String> {
            (0..4)
                .map(|index| chatter(&format!("{tag}{index}")))
                .collect()
        };
        let (left, right) = (pad("mon"), pad("tue"));
        let texts = |padding: &[String]| -> Vec<String> {
            steps.iter().chain(padding.iter()).cloned().collect()
        };
        let (first, second) = (texts(&left), texts(&right));
        let first: Vec<&str> = first.iter().map(String::as_str).collect();
        let second: Vec<&str> = second.iter().map(String::as_str).collect();
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", Some("surdy/qanungo"), &first),
            session('b', "2026-08-21T10:00:00Z", Some("surdy/qanungo"), &second),
        ];
        let cut = capped(&sessions, DEFAULT_CLUSTERS, 1);
        assert_eq!(cut.flows_found, 3);
        assert_eq!(cut.flows.len(), 1);
        let raised = capped(&sessions, DEFAULT_CLUSTERS, 50);
        assert_eq!(raised.flows.len(), 3);
        assert_eq!(raised.flows[..1], cut.flows[..]);
        assert_eq!(raised.flows_found, cut.flows_found);
    }

    /// The repository tally is by occurrence, cut with the total beside it so a shortened list is
    /// never mistaken for the whole of what was found.
    #[test]
    fn the_repository_tally_says_what_it_cut() {
        let text = request("shared");
        let sessions: Vec<FlowsSession> = (0..8)
            .map(|index| {
                session(
                    char::from(b'a' + index as u8),
                    "2026-08-20T10:00:00Z",
                    Some(&format!("surdy/repo{index}")),
                    &[&text],
                )
            })
            .collect();
        let found = fold(&sessions);
        let cluster = &found.clusters[0];
        assert_eq!(cluster.occurrences, 8);
        assert_eq!(cluster.repositories_found, 8);
        assert_eq!(cluster.repositories.len(), MAX_REPOSITORIES_PER_ROW);
        assert_eq!(
            found.repositories.len(),
            8,
            "coverage names every one of them"
        );
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
                Some("surdy/munshi"),
                &[&second],
            ),
        ];
        let found = fold(&sessions);
        let cluster = &found.clusters[0];
        assert_eq!(cluster.occurrences, 2, "the secret did not split the pair");
        assert!(
            !cluster.excerpt.contains(secret),
            "leaked: {}",
            cluster.excerpt
        );
        assert!(cluster.excerpt.contains("[REDACTED:github-token]"));
        assert!(
            !found.redaction.is_empty(),
            "and the replacement was counted"
        );

        // With the scrub off the flag is real, and the same pair still clusters.
        let bare = Flows::fold(
            &sessions,
            Vec::new(),
            &RedactionReport::default(),
            &Redactor::new().with_secrets(false),
            DEFAULT_CLUSTERS,
            DEFAULT_FLOWS,
        );
        assert!(bare.clusters[0].excerpt.contains(secret));
    }

    /// The canary the *ordering* actually rests on: a credential that straddles the excerpt's own
    /// clip. The test above proves a secret is replaced; it cannot prove the scrub ran *before* the
    /// clip, because its message is short enough to be quoted whole.
    ///
    /// The fixture checks its own premise first — it runs the wrong order deliberately and asserts
    /// that it *would* have leaked — so a change to [`MAX_SNIPPET_CHARS`] that stopped the token
    /// straddling the edge fails here rather than quietly turning this into a test that cannot fail.
    #[test]
    fn a_credential_straddling_the_clip_cannot_survive_it() {
        let token = format!("ghp_{}", "CANARY".repeat(6));
        // Laid out against the clip rather than hand-counted, so a change to MAX_SNIPPET_CHARS
        // cannot silently move the token off the edge and leave a test that proves nothing.
        let opening = "please regenerate the payments bindings and rerun every payments fixture, \
                       and the token is";
        let lead = format!(
            "{opening}{} and",
            " x".repeat((MAX_SNIPPET_CHARS - 20 - opening.chars().count()) / 2),
        );
        let message = format!("{lead} {token} which must never be printed anywhere at all");
        assert!(
            lead.chars().count() < MAX_SNIPPET_CHARS
                && lead.chars().count() + 1 + token.chars().count() > MAX_SNIPPET_CHARS,
            "the token has to straddle the clip for this test to mean anything: lead is {} chars",
            lead.chars().count(),
        );

        // The premise, stated as an assertion: clipping first and scrubbing after would leave a run
        // of the token on the screen. This is the mutation this test exists to catch.
        // Clipping first leaves a head of the token too short for the `github-token` pattern to
        // recognize, so the scrub that runs after it is a no-op and the head renders as itself.
        let clipped_first = Redactor::new().scrub(&snippet(&message)).text;
        assert!(
            clipped_first.contains("ghp_CANARY"),
            "the fixture proves nothing unless the wrong order leaks: {clipped_first}",
        );
        assert!(
            !clipped_first.contains("[REDACTED"),
            "and the leak is precisely that the truncated head is unrecognizable: {clipped_first}",
        );

        let restated = format!("{lead} {token} which must never be printed at all anywhere");
        let sessions = [
            session(
                'a',
                "2026-08-20T10:00:00Z",
                Some("surdy/qanungo"),
                &[&message],
            ),
            session(
                'b',
                "2026-08-21T10:00:00Z",
                Some("surdy/munshi"),
                &[&restated],
            ),
        ];
        let found = fold(&sessions);
        let excerpt = &found.clusters[0].excerpt;
        // The right order replaces the whole token before the clip runs, so what the clip lands
        // mid-way through is the *marker* — this build's own text, and harmless to cut. What can
        // never survive is any part of the token.
        assert!(excerpt.contains("[REDACTED:"), "{excerpt}");
        assert!(
            !excerpt.contains("ghp_"),
            "a head of the token survived: {excerpt}",
        );
        assert!(!excerpt.contains("CANARY"), "{excerpt}");
        assert!(
            !found.redaction.is_empty(),
            "and the replacement was counted"
        );
    }

    /// A hostile repository label is clamped and then scrubbed on the way to the page, like every
    /// other archive-derived label this crate renders — in the tally as well as in the coverage
    /// section, because the two read the same map.
    #[test]
    fn a_hostile_repository_label_is_clamped_and_a_credential_shaped_one_is_scrubbed() {
        const TOKEN_SHAPED: &str = "ghp_FAKEfake0123456789ABCDEFabcdef012345";
        let text = request("shared");
        let sessions = [
            session('a', "2026-08-20T10:00:00Z", Some("evil | repo"), &[&text]),
            session('b', "2026-08-21T10:00:00Z", Some(TOKEN_SHAPED), &[&text]),
        ];
        let found = fold(&sessions);
        let labels: Vec<&str> = found.clusters[0]
            .repositories
            .iter()
            .map(|count| count.repository.as_str())
            .collect();
        assert!(labels.contains(&format::INVALID_IDENTIFIER), "{labels:?}");
        assert!(labels.contains(&"[REDACTED:github-token]"), "{labels:?}");
        assert!(!labels.iter().any(|label| label.contains(TOKEN_SHAPED)));
        assert!(!labels.iter().any(|label| label.contains('|')));
        assert_eq!(found.repositories.len(), 2);
    }

    /// An archive with nothing repeated in it produces an empty finding rather than an error, and
    /// every count still describes what was read.
    #[test]
    fn nothing_repeated_is_an_empty_finding_with_its_coverage_intact() {
        let (first, second) = (request("schema"), request("deploy"));
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
                Some("surdy/munshi"),
                &[&second],
            ),
            session('c', "2026-08-22T10:00:00Z", Some("surdy/rz"), &[]),
        ];
        let found = fold(&sessions);
        assert!(found.is_empty());
        assert_eq!(found.clusters_found, 0);
        assert_eq!(found.flows_found, 0);
        assert_eq!(found.sessions, 3);
        assert_eq!(found.sessions_without_messages, 1);
        assert_eq!(
            found.repositories.len(),
            3,
            "every repository is still named"
        );
    }
}
