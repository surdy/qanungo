//! Scope selection: the same fold, cut a second way.
//!
//! qanungo #5's fourth slice. The dashboard already serves one window's numbers; this module is
//! what lets a reader ask for *part* of that window — one repository, one harness — and get an
//! answer computed the same way the whole-window answer was.
//!
//! # One fold, many scopes
//!
//! There is no second fold here, and there is deliberately no store to re-slice against (ADR
//! 0001). [`fold_coaching`](crate::command::fold_coaching) already produces per-session facts —
//! rule-readable metrics, a harness label, and now the repository the archive listed the session
//! under ([`SessionMetrics::repository`]) — and every score in the pack is a fold over those
//! per-session facts. So a scope is a **selection**, not a recomputation: pick the sessions, hand
//! them to [`Scorecard::fold_refs`], and the arithmetic that produced the all/all numbers produces
//! the scope's. The grilling's decision that drill-down slices are pre-folded per view is what this
//! is; the payload carries every scope because a query string decides nothing on this surface.
//!
//! # Two scope axes, and why only one of them is a payload dimension
//!
//! A scope reads as a pair — a repository filter and a harness filter — but the payload is not a
//! cross product, because **the harness axis is already folded**. [`Scorecard::fold_refs`] scores
//! per `source_agent` and blends the result, so inside one repository's scope the per-harness
//! entries *are* the harness-filtered scopes and the fleet blend *is* the all-harnesses one: a
//! blend over the single harness a filter selected is that harness's own score, by the definition
//! of an unweighted mean over one term. Serializing the cross product would be serializing the same
//! numbers a second time under different keys, and two copies of a number are two things that can
//! disagree.
//!
//! So the payload's scope dimension is the repository, and the page's harness control picks a
//! column out of a scope that is already there. That is also what keeps the promise that no scoring
//! logic lives in the JavaScript: selecting a harness reads a number, it does not compute one.
//!
//! # The label is the group
//!
//! Grouping is by the **rendered** label — [`repository_label`], which clamps
//! ([`format::identifier`]) and then scrubs ([`crate::redaction`]), the ordering
//! [`crate::evidence::identifier_field`] argues — rather than by the archive's raw string. Two raw
//! values that render to the same label are one row on the page, which is the honest shape for a
//! rendering surface: a reader cannot be offered a control whose options they cannot tell apart.
//! It also means a hostile repository name cannot smuggle a second, differently-spelled option into
//! a select element.
//!
//! A session the archive recorded no repository for is a real state — munshi records one only for a
//! session captured inside a checkout — and it gets its own bucket under
//! [`crate::standup::NO_REPOSITORY`], the same sentence the standup lane already uses so that one
//! page does not spell "we do not know" two ways. It sorts last, everywhere.
//!
//! # Two facts named "repository", and the page says which
//!
//! The label here is Patwari's **projection**: the repository on the snapshot the session was
//! listed by. It is the same fact the cost lane cuts by ([`crate::cost::SessionCost::repository`]),
//! so a repository scope narrows the coaching and cost sections consistently. It is *not* the fact
//! the standup lane groups by, which is the repository each session's own `summary.md` names — see
//! [`crate::standup::Standup::fold`]. The two agree on every ordinary session and are allowed to
//! disagree, so the served scope list is the **union** of what all three sections labelled, and a
//! scope with nothing to show in a section says so rather than rendering an empty one silently.

use std::collections::{BTreeMap, BTreeSet};

use crate::command::Folded;
use crate::evidence::identifier_field;
use crate::metrics::SessionMetrics;
use crate::redaction::Redactor;
use crate::standup::NO_REPOSITORY;

/// An archive-stated repository name, as every surface in this crate renders it: clamped, then
/// scrubbed.
///
/// `None` — a session captured outside a checkout — is [`NO_REPOSITORY`], which is spelled as a
/// sentence precisely so it cannot be confused with a repository actually called that.
///
/// The order is [`identifier_field`]'s and is argued there: the clamp has to judge the archive's
/// own bytes, or a 200-character token could launder itself into a renderable marker and be waved
/// through by a clamp that never saw what it really was.
pub fn repository_label(repository: Option<&str>, redactor: &Redactor) -> String {
    match repository {
        Some(repository) => identifier_field(repository, redactor),
        None => NO_REPOSITORY.to_owned(),
    }
}

/// One repository's slice of a folded window, in both halves of the window pair.
///
/// Borrows rather than clones: a scope is a *view* of the fold, and copying a few hundred
/// [`SessionMetrics`] once per repository to answer a question about grouping would be paying for
/// the event store this design decided not to build.
#[derive(Debug)]
pub struct RepositoryScope<'a> {
    /// The rendered label, and the group key. See the module docs.
    pub label: String,
    /// Whether the archive named a repository at all. `false` is the [`NO_REPOSITORY`] bucket.
    pub attributed: bool,
    /// The reported window's sessions in this repository, in the fold's own order.
    pub sessions: Vec<&'a SessionMetrics>,
    /// The comparison window's, grouped identically — which is what makes a scope's trend arrow
    /// the same statement as the whole window's, taken over less.
    pub previous: Vec<&'a SessionMetrics>,
}

impl RepositoryScope<'_> {
    /// How many sessions each harness contributed to this scope's reported window, by rendered
    /// label — the *same* rendering the lane columns and the evidence tags use, clamped and then
    /// scrubbed. A count keyed on a label the control spells differently is a count nobody can
    /// line up with anything.
    pub fn by_harness(&self, redactor: &Redactor) -> BTreeMap<String, usize> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for session in &self.sessions {
            *counts
                .entry(identifier_field(&session.source_agent, redactor))
                .or_default() += 1;
        }
        counts
    }
}

/// Every repository scope a served document can offer, busiest first.
///
/// `also` carries labels the *other* sections put on the page — the cost lane's repository rows and
/// the standup lane's group headings, each already rendered by its own lane. They join the list
/// with no sessions of their own, because a control that could not select a repository the page
/// visibly renders would be a control that lies about what it narrows. Their lanes come out as
/// no-reading, which is what a scope with nothing to score honestly is.
///
/// Order is the standup lane's order, for the same reason it has it: a reader starts at the top,
/// and the top is where the window went. Ties break on the label so the list is stable across runs,
/// and the unattributed bucket is always last — it is a residue, not a busy repository.
pub fn by_repository<'a>(
    folded: &'a Folded,
    redactor: &Redactor,
    also: impl IntoIterator<Item = String>,
) -> Vec<RepositoryScope<'a>> {
    let mut reported: BTreeMap<String, Vec<&'a SessionMetrics>> = BTreeMap::new();
    let mut previous: BTreeMap<String, Vec<&'a SessionMetrics>> = BTreeMap::new();
    for (sessions, into) in [
        (&folded.sessions, &mut reported),
        (&folded.previous, &mut previous),
    ] {
        for session in sessions {
            let label = repository_label(session.repository.as_deref(), redactor);
            into.entry(label).or_default().push(session);
        }
    }

    let labels: BTreeSet<String> = reported
        .keys()
        .chain(previous.keys())
        .cloned()
        .chain(also)
        .collect();
    let mut scopes: Vec<RepositoryScope<'a>> = labels
        .into_iter()
        .map(|label| RepositoryScope {
            attributed: label != NO_REPOSITORY,
            sessions: reported.get(&label).cloned().unwrap_or_default(),
            previous: previous.get(&label).cloned().unwrap_or_default(),
            label,
        })
        .collect();
    scopes.sort_by(|left, right| {
        left.attributed
            .cmp(&right.attributed)
            .reverse()
            .then_with(|| right.sessions.len().cmp(&left.sessions.len()))
            .then_with(|| left.label.cmp(&right.label))
    });
    scopes
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use munshi_transcript::SessionSummary;

    use super::*;
    use crate::evidence::SessionAnchors;
    use crate::metrics::{Activity, CommandChurn, Compactions, ReviewActivity, ToolOutcomes};
    use crate::report::Instrumentation;
    use crate::scoring::{Lane, RulePack, Scorecard};
    use crate::sync::SyncStats;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn session(index: usize, source_agent: &str, repository: Option<&str>) -> SessionMetrics {
        let first = at("2026-08-10T09:00:00Z");
        SessionMetrics {
            source_hash: format!("{index:02x}").repeat(32),
            source_agent: source_agent.to_owned(),
            repository: repository.map(ToOwned::to_owned),
            artifact_set_version: 2,
            summary: SessionSummary {
                first_timestamp: Some(first),
                last_timestamp: Some(first),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes::default(),
            activity: Activity::over(vec![first]),
            commands: CommandChurn::default(),
            compactions: Compactions::default(),
            reviews: ReviewActivity::default(),
            anchors: SessionAnchors::default(),
            bytes_folded: 0,
        }
    }

    fn folded(sessions: Vec<SessionMetrics>, previous: Vec<SessionMetrics>) -> Folded {
        Folded {
            generated_at: at("2026-08-17T12:00:00Z"),
            instrumentation: Instrumentation {
                sync: SyncStats::default(),
                fold_elapsed: Duration::ZERO,
                sessions_folded: sessions.len(),
                comparison_sessions_folded: previous.len(),
                bytes_folded: 0,
                rule_pack: RulePack::current(),
                patwari_url: String::new(),
                cache_root: PathBuf::new(),
            },
            compared: true,
            sessions,
            previous,
            findings: Vec::new(),
            skipped: Vec::new(),
        }
    }

    /// The grouping, its ordering, and the bucket a session with no repository lands in. Both
    /// halves of the window pair are cut by the same key, which is what a scope's trend arrow
    /// rests on.
    #[test]
    fn scopes_group_both_windows_busiest_first_with_the_unattributed_bucket_last() {
        let folded = folded(
            vec![
                session(1, "claude-code", None),
                session(2, "claude-code", Some("surdy/munshi")),
                session(3, "claude-code", Some("surdy/qanungo")),
                session(4, "copilot-cli", Some("surdy/qanungo")),
                session(5, "claude-code", Some("surdy/qanungo")),
            ],
            vec![
                session(6, "claude-code", Some("surdy/munshi")),
                session(7, "claude-code", Some("surdy/patwari")),
            ],
        );
        let scopes = by_repository(&folded, &Redactor::new(), None);
        let labels: Vec<&str> = scopes.iter().map(|scope| scope.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                // Three sessions, then one each in label order, then the residue.
                "surdy/qanungo",
                "surdy/munshi",
                // Present only in the comparison window: a real scope with nothing to score.
                "surdy/patwari",
                NO_REPOSITORY,
            ],
        );
        assert_eq!(scopes[0].sessions.len(), 3);
        assert_eq!(scopes[0].previous.len(), 0);
        assert_eq!(scopes[1].sessions.len(), 1);
        assert_eq!(scopes[1].previous.len(), 1);
        assert_eq!(scopes[2].sessions.len(), 0);
        assert_eq!(scopes[2].previous.len(), 1);
        assert!(!scopes[3].attributed);
        assert_eq!(scopes[3].sessions.len(), 1);
        assert_eq!(
            scopes[0].by_harness(&Redactor::new()),
            BTreeMap::from([("claude-code".to_owned(), 2), ("copilot-cli".to_owned(), 1),]),
        );
    }

    /// A label the coaching fold never saw still becomes a scope, because the other two sections
    /// render it and a control that cannot select what the page shows is a control that lies.
    #[test]
    fn a_label_from_another_section_joins_the_list_with_nothing_to_score() {
        let folded = folded(
            vec![session(1, "claude-code", Some("surdy/qanungo"))],
            vec![],
        );
        let scopes = by_repository(
            &folded,
            &Redactor::new(),
            [
                "surdy/chitrakar".to_owned(),
                // Already present: the union must not double it.
                "surdy/qanungo".to_owned(),
            ],
        );
        let labels: Vec<&str> = scopes.iter().map(|scope| scope.label.as_str()).collect();
        assert_eq!(labels, vec!["surdy/qanungo", "surdy/chitrakar"]);
        assert!(scopes[1].sessions.is_empty());
        // And it scores nothing rather than a phantom number.
        let card = Scorecard::fold_refs(&scopes[1].sessions);
        assert!(card.harnesses.is_empty());
        for lane in Lane::ALL {
            assert_eq!(card.fleet(lane), None, "{lane:?} invented a blend");
        }
    }

    /// The label a select element is built from is the archive's string only when the archive's
    /// string is renderable. A repository name carrying a pipe, a control character, or a
    /// credential shape is clamped and scrubbed before it is ever a group key — and two hostile
    /// names that render alike are one option, not two.
    #[test]
    fn a_hostile_repository_name_is_clamped_and_scrubbed_before_it_is_a_group() {
        let hostile = "surdy/evil\nsecond-line";
        let folded = folded(
            vec![
                session(1, "claude-code", Some(hostile)),
                session(2, "claude-code", Some("also|bad")),
            ],
            vec![],
        );
        let scopes = by_repository(&folded, &Redactor::new(), None);
        assert_eq!(scopes.len(), 1, "both render alike, so both are one option");
        assert_eq!(scopes[0].label, crate::format::INVALID_IDENTIFIER);
        assert_eq!(scopes[0].sessions.len(), 2);
    }

    /// The clamp runs before the scrub, so a value too long to be an identifier cannot launder
    /// itself into a renderable marker — [`crate::evidence::identifier_field`]'s argument, restated
    /// on the surface that turns these strings into controls.
    #[test]
    fn a_token_shaped_repository_name_is_judged_by_the_clamp_first() {
        let redactor = Redactor::new();
        // Well inside the clamp: it reaches the scrub intact and leaves as a marker.
        let planted = "ghp_FAKEfake0123456789ABCDEFabcdef012345";
        let short = repository_label(Some(planted), &redactor);
        assert_ne!(short, crate::format::INVALID_IDENTIFIER);
        assert!(!short.contains(planted), "{short}");
        // Past the clamp's ceiling: replaced wholesale, never scrubbed down to something
        // renderable.
        let long = repository_label(Some(&"a".repeat(200)), &redactor);
        assert_eq!(long, crate::format::INVALID_IDENTIFIER);
        assert_eq!(repository_label(None, &redactor), NO_REPOSITORY);
    }
}
