//! The doctor fold: instructions this archive shows you giving more than once, **per repository**.
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
//! # The detection is not this module's
//!
//! Normalization, the authored filter, shingling, containment, the union-find and the conversation
//! merge all live in [`crate::repetition`], which [`crate::flows`] reads through the same door.
//! What is *here* is the doctor's own lens on the result, which is exactly three things: the
//! per-repository grouping, the friction counts beside it, and the rendering cut. See the shared
//! module for how a cluster is found and for the honest bound on the one place it is approximate.
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
//! # A repository is a hard boundary here, and only here
//!
//! A cluster must span at least [`crate::repetition::MIN_CLUSTER_SESSIONS`] distinct conversations
//! **of one repository**: the index is built per repository, so a message in one repository can
//! never be compared against a message in another. An instruction missing from repository A's
//! `CLAUDE.md` is repository A's business, and merging the two would produce a finding nobody can
//! act on in either place. A session the archive attributes to *no* repository has no instruction
//! file for a finding to be about, so it is listed as [`Unexamined`] rather than clustered.
//!
//! Both of those are this lane's judgements about instruction files, not facts about repetition.
//! [`crate::flows`] pools the whole archive and takes the no-repository sessions in, because the
//! thing it is looking for — a workflow worth a skill — is worth it wherever it recurs.
//!
//! # The scrub happens here
//!
//! This is qanungo #8's fourth consumer and the CLI's second verbatim surface, after
//! `ask --verbatim`. A cluster renders an excerpt of the repeated instruction, which is transcript
//! text somebody typed, so:
//!
//! - **Clustering runs on the unscrubbed text**, in [`crate::repetition`], for the reason its own
//!   docs give.
//! - **The excerpt is scrubbed on the way into the [`Cluster`]**, through
//!   [`crate::ask::snippet`]'s own pipeline — scrub, then collapse, then clip. Clipping first could
//!   cut a credential in half and render the surviving head.
//! - Only the excerpts that are actually *rendered* are scrubbed, so the counts in the footer
//!   describe the document rather than the corpus behind it — the rule [`crate::ask`] already holds.

use std::collections::BTreeMap;

use crate::ask::snippet;
use crate::format;
use crate::redaction::{RedactionReport, Redactor};
use crate::repetition::{
    Citation, Clustering, MAX_CITATIONS_PER_CLUSTER, MIN_CLUSTER_SESSIONS, Repetition,
    SessionRecord,
};
use crate::report::SkippedNote;
use crate::standup::NO_REPOSITORY;

/// One session, as this lane takes it in. The shared record, under the name the lane has always
/// used for it.
pub type DoctorSession = SessionRecord;

/// Sessions a repository needs in the reach before this lane looks at it for clusters.
///
/// **Derived, not arbitrary.** A cluster must span [`MIN_CLUSTER_SESSIONS`] conversations, so a
/// repository with fewer sessions than that cannot produce one however hard it is looked at. The
/// constant is named rather than implied so the document can say *which* repositories fell under
/// it — a repository nothing was reported for because nothing could be reported for it is listed as
/// unexamined ([`Doctor::unexamined`]) rather than quietly absent.
pub const MIN_REPOSITORY_SESSIONS: usize = MIN_CLUSTER_SESSIONS;

/// Clusters one repository renders before the list is cut short, when nobody says otherwise.
///
/// A tunable, with the discipline the rest of the crate holds: [`RepositoryClusters::found`] counts
/// them all, so the document can say how many it is not showing.
///
/// **A default, not a ceiling** (qanungo #16). The reader of this document is often the
/// `instructions-editor` skill, which acts on clusters in the weight class this cut lands in — the
/// first production run hid two 2-occurrence clusters behind the "not shown" line while a
/// 2-occurrence cluster above it produced a shipped instruction-file edit. So `--clusters-per-repo`
/// raises it, [`Doctor::fold`] takes the effective value as an argument, and the document states it
/// whenever it is not this number.
pub const DEFAULT_CLUSTERS_PER_REPOSITORY: usize = 10;

/// One instruction this repository's sessions gave more than once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    /// Messages in the cluster, across every session it spans.
    pub occurrences: usize,
    /// Distinct *conversations* it spans — sessions that replay one another count once. At least
    /// [`MIN_CLUSTER_SESSIONS`], by construction.
    pub sessions: usize,
    /// A scrubbed excerpt of the fullest occurrence.
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
    /// two sessions that replay the same conversation count once.
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
            let mut clustering = Clustering::of(&held);
            let found = clustering.repetitions();
            counted.clusters += found.len();
            counted.conversations += clustering.conversations();
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    use crate::repetition::tests::{RESTATED, RULE, UNRELATED};
    use crate::repetition::{Instruction, SessionMessages, normalize};

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
    ///
    /// This is the property [`crate::flows`] deliberately does **not** hold: the same pair below
    /// clusters there, because a workflow worth a skill is worth it wherever it recurs.
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
    /// falls in — far under the similarity threshold, which the test asserts by counting the
    /// clusters that form.
    pub(crate) fn distinct_instruction(index: usize) -> String {
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
        // sessions sharing twelve instructions is a resumed conversation by the conversation
        // merge's reckoning, and would be folded into one before any of this was cut. A pair
        // sharing one is what repetition actually looks like.
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
