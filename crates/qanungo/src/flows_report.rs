//! Markdown rendering for the flows lane.
//!
//! Like the standup, ask and doctor documents, this one renders prose that came out of the archive —
//! the excerpt of a repeated request — so it holds its redaction line the same way: **every string
//! below was scrubbed on the way into [`Flows`] by [`crate::flows`]**, before it reached this
//! module, and there is no unscrubbed copy in scope to render by mistake. The footer sentence is the
//! standup lane's, shared rather than reworded, because all four documents make the same promise
//! about the same scrub.
//!
//! # The three sentences this document must never write
//!
//! - **"This flow worked."** Nothing here reads an outcome. A step is a message somebody typed; what
//!   happened after it is not in this fold at any resolution.
//! - **"The second step happened because of the first."** A sequence in a transcript is an ordering
//!   and nothing more. The document says "these requests came in this order, this often" and stops.
//! - **"You should turn this into a skill."** Whether a repetition is worth tooling is a judgement
//!   about the reader's own work — how much of it was the same each time, whether the variation was
//!   the interesting part, whether they intend to do it again. The `skill-finder` skill makes that
//!   call *with* the user, in the harness, under their own permission prompts. This document shows
//!   the repetition.
//!
//! # It shows its own thresholds, and its own worst noise class
//!
//! A finding is the output of a handful of constants, and a reader who cannot see them cannot tell a
//! real repetition from a threshold artefact — so the scope block states the phrase length, the
//! overlap, the floor under a comparable message, and the conversations a cluster and a flow each
//! have to span. The two rendering cuts are stated the same way, and only when they are not the
//! defaults (qanungo #16's rule: the number a reader needs is the one the run actually used, and a
//! document naming a constant while a flag had moved it would be describing a different document).
//!
//! And the preamble names the noise class the pooled lens makes worse: harness-injected prose the
//! authored filter cannot certify clusters perfectly, and pooled across the archive it clusters with
//! itself in every repository at once. That is honestly the most repeated text in the corpus and it
//! is useless as a finding, so the document warns the reader where the top of the list comes from
//! rather than quietly dropping anything it could not prove was machinery.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::cli::Window;
use crate::flows::{
    DEFAULT_CLUSTERS, DEFAULT_FLOWS, Flow, Flows, MAX_FLOW_STEPS, MIN_FLOW_SESSIONS,
    MIN_FLOW_STEPS, RepositoryCount, RepositoryRead, RequestCluster,
};
use crate::format;
use crate::redaction::{PATTERN_REVISION, Redactor};
use crate::repetition::{
    MIN_CLUSTER_SESSIONS, MIN_CLUSTERABLE_WORDS, MIN_SHARED_INSTRUCTIONS,
    SAME_CONVERSATION_PERCENT, SHINGLE_WORDS, SIMILARITY_THRESHOLD_PERCENT,
};
use crate::report::{SkippedNote, stamp};
use crate::standup_report::{redaction_counts, redaction_line};
use crate::sync::SyncStats;

/// What a flows run cost, folded into the footer.
#[derive(Debug, Clone)]
pub struct FlowsInstrumentation {
    pub sync: SyncStats,
    /// Wall-time of reading the transcripts, clustering their messages and mining the flows, network
    /// excluded.
    pub fold_elapsed: Duration,
    /// The redactor the flags asked for, so the footer can say which passes ran.
    pub redactor: Redactor,
    pub patwari_url: String,
    pub cache_root: PathBuf,
}

/// Everything one flows document is rendered from.
pub struct FlowsReport<'a> {
    /// The window that narrowed the reach, or `None` for all of history.
    pub window: Option<&'a Window>,
    /// The cluster cut the fold was run with — the effective value of `--clusters`, not
    /// [`DEFAULT_CLUSTERS`], which is merely what it defaults to.
    pub clusters: usize,
    /// The flow cut the fold was run with — the effective value of `--flows`.
    pub flows: usize,
    pub generated_at: DateTime<Utc>,
    pub found: &'a Flows,
    pub instrumentation: &'a FlowsInstrumentation,
}

impl FlowsReport<'_> {
    /// Renders the repeated requests, the flows over them, and what was read, as Markdown.
    ///
    /// Deterministic in full: the same reach over the same archive with the same flags produces
    /// byte-identical output, because every ordering in [`Flows`] is total and nothing here reads a
    /// clock except the two timestamps it prints.
    pub fn render(&self) -> String {
        let mut out = String::new();
        match self.window {
            Some(window) => {
                let _ = writeln!(out, "# Repeated flows — last {window}\n");
            }
            None => out.push_str("# Repeated flows — all of history\n\n"),
        }
        self.render_scope(&mut out);
        self.render_clusters(&mut out);
        self.render_flows(&mut out);
        self.render_repositories(&mut out);
        self.render_gaps(&mut out);
        self.render_footer(&mut out);
        out
    }

    /// What was read, what the tool can and cannot see, and the thresholds that produced the rest.
    fn render_scope(&self, out: &mut String) {
        let found = self.found;
        let reach = match self.window {
            Some(window) => format!(
                "in the last {window} (archived since {} UTC)",
                stamp(window.opens_at(self.generated_at)),
            ),
            None => "across all of the archive's history".to_owned(),
        };
        let _ = writeln!(
            out,
            "Read {} {} {reach}, at {} (UTC). They carried {} user {}: {} of those the harness \
             wrote onto the user surface itself — a pasted image, a slash command, a skill body, a \
             finished background task — and {} of what a person actually typed was long enough to \
             compare.",
            found.sessions,
            plural(found.sessions, "session", "sessions"),
            stamp(self.generated_at),
            found.messages,
            plural(found.messages, "message", "messages"),
            found.harness_generated,
            found.clusterable,
        );
        out.push_str(
            "\nThis reads **requests, never outcomes**. It can show that you asked for nearly the \
             same thing several times, and in what order those asks arrived; it knows nothing about \
             whether any of it worked, what it produced, or how long it took, and an ordering in a \
             transcript is not a cause. Whether a repetition is worth turning into a skill or an \
             agent is a judgement about your own work that this document deliberately leaves to \
             you — it shows the repetition and stops.\n",
        );
        out.push_str(
            "\nUnlike `qanungo doctor`, which groups per repository because an instruction file \
             belongs to one, everything below is pooled across **the whole archive**: a workflow \
             worth a skill is worth it wherever it recurs, so repositories are what a finding is \
             listed *by* and never what it is grouped by. Sessions the archive attributes to no \
             repository are compared here like any other — a repeated request needs no repository \
             to be one.\n",
        );
        let _ = writeln!(
            out,
            "\nTwo messages are called the same request when they share at least \
             {SIMILARITY_THRESHOLD_PERCENT}% of the shorter one's {SHINGLE_WORDS}-word phrases. A \
             message under {MIN_CLUSTERABLE_WORDS} words is never compared — an acknowledgement is \
             not a request — and a cluster is reported only when it spans at least \
             {MIN_CLUSTER_SESSIONS} distinct conversations. A flow is a run of \
             {MIN_FLOW_STEPS} to {MAX_FLOW_STEPS} of those clustered requests, **adjacent among \
             the clustered messages of one session** — anything that did not cluster sits between \
             two steps without breaking them apart, which is what a real workflow looks like — and \
             it is reported only when it recurs in at least {MIN_FLOW_SESSIONS} conversations. Two \
             sessions that share at least {MIN_SHARED_INSTRUCTIONS} requests **and** \
             {SAME_CONVERSATION_PERCENT}% of the smaller one's are read as one conversation \
             captured twice, because a resumed session replays what came before it. Every one of \
             those numbers is an arbitrary-until-measured constant, named in \
             `crates/qanungo/src/repetition.rs` and `crates/qanungo/src/flows.rs`.",
        );
        out.push_str(
            "\n**Read the top of the list with suspicion.** The harness-written count above is a \
             floor on that noise rather than a proof of its absence: a harness can inject anything, \
             and this build recognizes only the shapes somebody has looked at. Injected text in an \
             unrecognized shape is byte-identical wherever it appears, so pooling the archive makes \
             it cluster with itself in every repository at once — which can genuinely make it the \
             most repeated text in the corpus and still worth nothing. A cluster that is plainly \
             machinery rather than something you typed is a gap in that list, and triaging it out \
             is the `skill-finder` skill's first step.\n",
        );
        self.render_cuts(out);
        if found.sessions_without_messages > 0 {
            let _ = writeln!(
                out,
                "\n{} of the {} {} read carried no user message this build could read, so there \
                 was nothing in {} to compare — counted here rather than passed over as an empty \
                 result.",
                found.sessions_without_messages,
                found.sessions,
                plural(found.sessions, "session", "sessions"),
                plural(found.sessions_without_messages, "it", "them"),
            );
        }
    }

    /// States a rendering cut, and only when it is not the default.
    ///
    /// At the default it stays unstated, because the "not shown" line under each section already
    /// says the cut bit and a sentence about a cut that hid nothing is noise (qanungo #16).
    fn render_cuts(&self, out: &mut String) {
        let moved = [
            ("`--clusters`", self.clusters, DEFAULT_CLUSTERS, "clusters"),
            ("`--flows`", self.flows, DEFAULT_FLOWS, "flows"),
        ];
        for (flag, effective, default, what) in moved {
            if effective == default {
                continue;
            }
            let _ = writeln!(
                out,
                "\nAt most {effective} {what} are rendered below, because {flag} asked for that \
                 rather than the usual {default}. The cut is on the rendering and never on the \
                 mining: every count of what was found is the number it would have been at the \
                 default, and only how much of it is shown has moved.",
            );
        }
    }

    fn render_clusters(&self, out: &mut String) {
        open_section(out, "## Repeated requests");
        let found = self.found;
        if found.clusters_found == 0 {
            out.push_str(
                "\nNo request cleared these thresholds anywhere in the archive. That is an answer \
                 about this archive at these settings — nothing was found and hidden.\n",
            );
            return;
        }
        let _ = writeln!(
            out,
            "\n**{} {}**, pooled across every session in the reach; those sessions were read as {} \
             distinct {}, because a resumed session replays the one before it. Each is listed with \
             the repositories it turned up in.",
            found.clusters_found,
            plural(found.clusters_found, "cluster", "clusters"),
            found.conversations,
            plural(found.conversations, "conversation", "conversations"),
        );
        out.push('\n');
        for cluster in &found.clusters {
            render_cluster(out, cluster);
        }
        render_held_back(
            out,
            found.clusters_found - found.clusters.len(),
            "cluster",
            "clusters",
        );
    }

    fn render_flows(&self, out: &mut String) {
        open_section(out, "## Multi-step flows");
        let found = self.found;
        if found.flows_found == 0 {
            let _ = writeln!(
                out,
                "\nNo sequence of {MIN_FLOW_STEPS} or more repeated requests recurred in \
                 {MIN_FLOW_SESSIONS} or more conversations. Repeated requests can be real without \
                 any of them falling into a repeated *order*, which is what this section would \
                 have found — so this is an answer about the archive, not a section that was cut.",
            );
            return;
        }
        let _ = writeln!(
            out,
            "\n**{} {}** — sequences of the clusters above, in the order they arrived within one \
             session. A step is a cluster, so the excerpt under it is that cluster's own \
             representative wording rather than a quotation of any single instance. A three-step \
             flow's own two-step runs are mined too and appear here beside it when they clear the \
             floor, because \"A→B→C three times\" and \"A→B nine times\" are different facts.",
            found.flows_found,
            plural(found.flows_found, "flow", "flows"),
        );
        out.push('\n');
        for flow in &found.flows {
            render_flow(out, flow);
        }
        render_held_back(out, found.flows_found - found.flows.len(), "flow", "flows");
    }

    /// Every repository the reach covered — rendered rather than omitted, because a repository
    /// absent from a report is indistinguishable from a repository with nothing to say.
    ///
    /// There is no "not examined" list under this lane, and that absence is the finding: this fold
    /// excludes nothing. A repository with one session in the reach still had its messages compared
    /// against every other repository's, and the unattributed bucket did too.
    fn render_repositories(&self, out: &mut String) {
        if self.found.repositories.is_empty() {
            return;
        }
        open_section(out, "## Repositories read");
        out.push('\n');
        out.push_str(
            "Every one of these was compared against every other — the pool is the archive, so no \
             repository is held back for being too small and no session is set aside for having no \
             repository at all.\n\n",
        );
        out.push_str("| Repository | Sessions | Comparable messages |\n");
        out.push_str("| --- | ---: | ---: |\n");
        for read in &self.found.repositories {
            render_repository_row(out, read);
        }
    }

    fn render_gaps(&self, out: &mut String) {
        let gaps: &[SkippedNote] = &self.found.gaps;
        if gaps.is_empty() {
            return;
        }
        open_section(out, "## Gaps");
        out.push('\n');
        out.push_str("These archived sessions put nothing in the reading above:\n\n");
        for note in gaps {
            let _ = writeln!(out, "- {} — {}", note.count, note.reason);
        }
    }

    fn render_footer(&self, out: &mut String) {
        let instrumentation = self.instrumentation;
        out.push_str("\n---\n\n");
        out.push_str(&redaction_line(
            instrumentation.redactor,
            &self.found.redaction,
        ));
        out.push_str(
            "\n\nEvery occurrence is cited by the content hash of the `transcript.jsonl` it was \
             read from, beside the event's own ordinal in that file — a flow's citation carries one \
             ordinal per step, in the flow's order. To read one in full, ask the archive for the \
             artifact and fetch the `content_url` that comes back (the filter takes the bare \
             digest, without the `sha256:` prefix):\n\n",
        );
        let _ = writeln!(
            out,
            "    GET {}/api/v1/artifacts?original_sha256=<hash>\n",
            instrumentation.patwari_url.trim_end_matches('/'),
        );
        let _ = writeln!(
            out,
            "_Instrumentation — sync {} · fold {} · {} sessions · {} user messages ({} \
             harness-written, {} comparable) · {} read · cache {} hits / {} misses ({} \
             transferred) · snapshots {} indexed / {} fetched · {} unreadable records · redaction \
             {} · patterns {PATTERN_REVISION} · archive {} · cache {}_",
            format::elapsed(instrumentation.sync.elapsed),
            format::elapsed(instrumentation.fold_elapsed),
            self.found.sessions,
            self.found.messages,
            self.found.harness_generated,
            self.found.clusterable,
            format::bytes(self.found.bytes_folded),
            instrumentation.sync.cache_hits,
            instrumentation.sync.cache_misses,
            format::bytes(instrumentation.sync.bytes_transferred),
            instrumentation.sync.snapshots_indexed,
            instrumentation.sync.snapshots_fetched,
            self.found.unreadable_records,
            redaction_counts(&self.found.redaction),
            instrumentation.patwari_url,
            display_path(&instrumentation.cache_root),
        );
    }
}

/// One cluster: how often, across how many conversations, where, the fullest wording of it, and a
/// citation per occurrence.
fn render_cluster(out: &mut String, cluster: &RequestCluster) {
    let _ = writeln!(
        out,
        "**{} {} across {} {} in {} {}** — {}\n",
        cluster.occurrences,
        plural(cluster.occurrences, "occurrence", "occurrences"),
        cluster.sessions,
        plural(cluster.sessions, "session", "sessions"),
        cluster.repositories_found,
        plural(cluster.repositories_found, "repository", "repositories"),
        repositories(&cluster.repositories, cluster.repositories_found),
    );
    let _ = writeln!(out, "> {}\n", cluster.excerpt);
    for citation in &cluster.citations {
        let _ = writeln!(
            out,
            "- archived {} (UTC) · `event {}` · `{}`",
            archived(citation.archived_at),
            citation.locator,
            citation.source_hash,
        );
    }
    if cluster.occurrences > cluster.citations.len() {
        let _ = writeln!(
            out,
            "- _and {} further {}, not listed_",
            cluster.occurrences - cluster.citations.len(),
            plural(
                cluster.occurrences - cluster.citations.len(),
                "occurrence",
                "occurrences",
            ),
        );
    }
    out.push('\n');
}

/// One flow: its steps in order, how often and where it recurred, and one citation per occurrence
/// carrying every step's ordinal.
fn render_flow(out: &mut String, flow: &Flow) {
    let _ = writeln!(
        out,
        "**{}-step flow · {} {} across {} {} in {} {}** — {}\n",
        flow.steps.len(),
        flow.occurrences,
        plural(flow.occurrences, "occurrence", "occurrences"),
        flow.sessions,
        plural(flow.sessions, "session", "sessions"),
        flow.repositories_found,
        plural(flow.repositories_found, "repository", "repositories"),
        repositories(&flow.repositories, flow.repositories_found),
    );
    for (index, step) in flow.steps.iter().enumerate() {
        let _ = writeln!(out, "{}. {step}", index + 1);
    }
    out.push('\n');
    for instance in &flow.instances {
        let events: Vec<String> = instance
            .locators
            .iter()
            .map(|locator| locator.to_string())
            .collect();
        let _ = writeln!(
            out,
            "- archived {} (UTC) · `events {}` · `{}`",
            archived(instance.archived_at),
            events.join(" → "),
            instance.source_hash,
        );
    }
    if flow.occurrences > flow.instances.len() {
        let _ = writeln!(
            out,
            "- _and {} further {}, not listed_",
            flow.occurrences - flow.instances.len(),
            plural(
                flow.occurrences - flow.instances.len(),
                "occurrence",
                "occurrences",
            ),
        );
    }
    out.push('\n');
}

/// The repository tally beside a finding: "repo-a ×4, repo-b ×2", with a remainder when the list was
/// cut.
fn repositories(counted: &[RepositoryCount], found: usize) -> String {
    let mut named: Vec<String> = counted
        .iter()
        .map(|count| format!("`{}` ×{}", count.repository, count.occurrences))
        .collect();
    if found > counted.len() {
        named.push(format!("and {} more", found - counted.len()));
    }
    named.join(", ")
}

/// The line a cut section carries, drawn from what the section is actually missing rather than from
/// the cap, so a raised cut that still truncates still says so.
fn render_held_back(out: &mut String, held: usize, one: &'static str, many: &'static str) {
    if held == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "_{held} further {} {} not shown._",
        plural(held, one, many),
        plural(held, "is", "are"),
    );
}

fn render_repository_row(out: &mut String, read: &RepositoryRead) {
    let _ = writeln!(
        out,
        "| `{}` | {} | {} |",
        read.repository, read.sessions, read.clusterable,
    );
}

/// A citation's archive time, or this build's own sentence for a session the archive dated
/// unreadably — inventing a date for one is exactly the guess the mirror refuses to make.
fn archived(at: Option<DateTime<Utc>>) -> String {
    match at {
        Some(at) => stamp(at),
        None => "an unreadable time".to_owned(),
    }
}

/// Opens a section with exactly one blank line above its heading, whatever the section above
/// happened to leave behind.
///
/// The sections do not agree on their own trailing whitespace and cannot be made to: a flow ends
/// with a blank line after its citations, while the "not shown" line that sometimes follows it does
/// not. Prepending a newline unconditionally therefore produced one blank line in the truncated case
/// and two in the complete one — a document whose spacing depended on whether anything had been cut.
/// Normalizing at the heading fixes it in one place rather than in every section's tail.
fn open_section(out: &mut String, heading: &str) {
    while out.ends_with("\n\n") {
        out.pop();
    }
    out.push('\n');
    out.push_str(heading);
    out.push('\n');
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
    use crate::flows::{FlowInstance, MAX_REPOSITORIES_PER_ROW};
    use crate::redaction::RedactionReport;
    use crate::repetition::Citation;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn instrumentation() -> FlowsInstrumentation {
        FlowsInstrumentation {
            sync: SyncStats::default(),
            fold_elapsed: Duration::from_millis(5),
            redactor: Redactor::new(),
            patwari_url: "https://patwari.example".to_owned(),
            cache_root: PathBuf::from("/cache"),
        }
    }

    fn report<'a>(found: &'a Flows, instrumentation: &'a FlowsInstrumentation) -> FlowsReport<'a> {
        capped(found, instrumentation, DEFAULT_CLUSTERS, DEFAULT_FLOWS)
    }

    /// The same document, rendered as a run that moved one or both cuts would render it.
    fn capped<'a>(
        found: &'a Flows,
        instrumentation: &'a FlowsInstrumentation,
        clusters: usize,
        flows: usize,
    ) -> FlowsReport<'a> {
        FlowsReport {
            window: None,
            clusters,
            flows,
            generated_at: at("2026-09-04T00:00:00Z"),
            found,
            instrumentation,
        }
    }

    fn repositories(counts: &[(&str, usize)]) -> Vec<RepositoryCount> {
        counts
            .iter()
            .map(|(repository, occurrences)| RepositoryCount {
                repository: (*repository).to_owned(),
                occurrences: *occurrences,
            })
            .collect()
    }

    fn cluster(occurrences: usize, citations: usize) -> RequestCluster {
        RequestCluster {
            occurrences,
            sessions: 2,
            excerpt: "regenerate the bindings and rerun the fixtures before you say it is done"
                .to_owned(),
            repositories: repositories(&[("surdy/qanungo", 4), ("surdy/munshi", 2)]),
            repositories_found: 2,
            citations: (0..citations)
                .map(|index| Citation {
                    archived_at: Some(at("2026-08-20T10:00:00Z")),
                    source_hash: format!("{index}").repeat(64),
                    locator: index as u64 + 1,
                })
                .collect(),
        }
    }

    fn flow(steps: usize, occurrences: usize, instances: usize) -> Flow {
        Flow {
            steps: (0..steps)
                .map(|index| format!("step {index} wording"))
                .collect(),
            occurrences,
            sessions: 3,
            repositories: repositories(&[("surdy/qanungo", 2), ("no repository recorded", 1)]),
            repositories_found: 2,
            instances: (0..instances)
                .map(|index| FlowInstance {
                    archived_at: Some(at("2026-08-20T10:00:00Z")),
                    source_hash: format!("{index}").repeat(64),
                    locators: (0..steps as u64).map(|step| step * 10 + 4).collect(),
                })
                .collect(),
        }
    }

    fn found(clusters: Vec<RequestCluster>, clusters_found: usize, flows: Vec<Flow>) -> Flows {
        Flows {
            clusters_found,
            flows_found: flows.len(),
            clusters,
            flows,
            repositories: vec![
                RepositoryRead {
                    repository: "surdy/qanungo".to_owned(),
                    sessions: 30,
                    clusterable: 240,
                },
                RepositoryRead {
                    repository: "no repository recorded".to_owned(),
                    sessions: 10,
                    clusterable: 60,
                },
            ],
            conversations: 36,
            sessions: 40,
            messages: 900,
            harness_generated: 120,
            clusterable: 300,
            ..Flows::default()
        }
    }

    /// The document refuses the outcome claim and the causal claim in its own voice, rather than
    /// leaving either bound to be inferred from what is missing.
    #[test]
    fn the_preamble_refuses_the_outcome_and_causal_claims_out_loud() {
        let flows = found(vec![cluster(3, 3)], 1, vec![flow(2, 4, 4)]);
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("**requests, never outcomes**"));
        assert!(rendered.contains("an ordering in a transcript is not a cause"));
        assert!(rendered.contains("shows the repetition and stops"));
        assert!(rendered.contains("leaves to you"));
        // Affirmative constructions only: the preamble itself says "whether any of it worked",
        // which is the refusal rather than the claim, so a bare-word ban would fail on the very
        // sentence that makes the promise.
        for forbidden in [
            "this flow worked",
            "was caused by",
            "led to",
            "would have prevented",
            "you should turn this into",
            "we recommend",
        ] {
            assert!(!rendered.contains(forbidden), "{forbidden}: {rendered}");
        }
    }

    /// The lens is stated, because it is the whole difference from the doctor document a reader may
    /// have open beside this one.
    #[test]
    fn the_preamble_states_the_pooled_lens_and_the_no_repository_rule() {
        let flows = found(vec![cluster(3, 3)], 1, Vec::new());
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("pooled across **the whole archive**"));
        assert!(rendered.contains("worth it wherever it recurs"));
        assert!(
            rendered.contains("Sessions the archive attributes to no repository are compared here"),
            "{rendered}",
        );
    }

    /// The noise class the pooled lens makes worse is named out loud, because the top of the list is
    /// where a reader would otherwise take machinery for a finding.
    #[test]
    fn the_preamble_names_the_injected_prose_noise_class() {
        let flows = found(vec![cluster(3, 3)], 1, Vec::new());
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("**Read the top of the list with suspicion.**"));
        assert!(rendered.contains("only the shapes somebody has looked at"));
        assert!(rendered.contains("cluster with itself in every repository at once"));
        assert!(rendered.contains("`skill-finder` skill's first step"));
    }

    /// The thresholds that produced the findings are in the document, so a reader can tell a real
    /// repetition from a threshold artefact.
    #[test]
    fn the_document_states_its_own_thresholds() {
        let flows = found(vec![cluster(3, 3)], 1, vec![flow(3, 4, 4)]);
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("share at least 60% of the shorter one's 4-word phrases"));
        assert!(rendered.contains("under 8 words is never compared"));
        assert!(rendered.contains("at least 2 distinct conversations"));
        assert!(rendered.contains("a run of 2 to 3 of those clustered requests"));
        assert!(rendered.contains("**adjacent among the clustered messages of one session**"));
        assert!(rendered.contains("arbitrary-until-measured"));
    }

    /// Each moved cut is stated, with the **effective** number, and a cut left at its default is not
    /// mentioned at all (qanungo #16).
    #[test]
    fn a_moved_cut_is_stated_and_a_default_one_is_not() {
        let flows = found(vec![cluster(3, 3)], 1, vec![flow(2, 4, 4)]);
        let instrumentation = instrumentation();

        let default = report(&flows, &instrumentation).render();
        assert!(
            !default.contains("`--clusters`") && !default.contains("`--flows`"),
            "a cut that hid nothing and moved nothing is not news: {default}",
        );

        let raised = capped(&flows, &instrumentation, 50, DEFAULT_FLOWS).render();
        assert!(raised.contains("At most 50 clusters are rendered below"));
        assert!(raised.contains("rather than the usual 20"));
        assert!(raised.contains("never on the mining"));
        assert!(
            !raised.contains("At most 50 flows"),
            "one flag moved, one sentence"
        );

        let both = capped(&flows, &instrumentation, 50, 3).render();
        assert!(both.contains("At most 50 clusters are rendered below"));
        assert!(both.contains("At most 3 flows are rendered below"));
    }

    /// An empty result is an answer about the archive, not a blank section, and the two sections
    /// phrase their emptiness differently because they mean different things by it.
    #[test]
    fn an_empty_result_is_stated_as_an_answer_in_both_sections() {
        let flows = Flows {
            sessions: 40,
            messages: 900,
            clusterable: 300,
            ..Flows::default()
        };
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("No request cleared these thresholds anywhere in the archive"));
        assert!(rendered.contains("nothing was found and hidden"));
        assert!(rendered.contains("No sequence of 2 or more repeated requests recurred"));
        assert!(rendered.contains("not a section that was cut"));
        assert!(rendered.contains("Read 40 sessions"));
        assert!(rendered.contains("They carried 900 user messages"));
    }

    /// Every section heading gets exactly one blank line above it, whether or not the section above
    /// it ended with a held-back line.
    ///
    /// The bug this pins: a flow's citations end with a blank line and the "not shown" line does
    /// not, so a heading that prepended a newline unconditionally rendered one blank line when
    /// something had been cut and two when nothing had — spacing that leaked whether the document
    /// was complete.
    #[test]
    fn a_heading_gets_one_blank_line_above_it_whether_or_not_anything_was_cut() {
        let instrumentation = instrumentation();
        for (clusters_found, flows) in [(1, vec![flow(2, 2, 2)]), (5, vec![flow(2, 2, 2); 2])] {
            let found = found(vec![cluster(3, 3)], clusters_found, flows);
            for (cut_clusters, cut_flows) in [(DEFAULT_CLUSTERS, DEFAULT_FLOWS), (1, 1)] {
                let rendered = capped(&found, &instrumentation, cut_clusters, cut_flows).render();
                assert!(
                    !rendered.contains("\n\n\n"),
                    "two blank lines somewhere in:\n{rendered}",
                );
                assert!(rendered.contains("\n\n## Repositories read\n"));
                assert!(rendered.contains("\n\n## Multi-step flows\n"));
            }
        }
    }

    /// A cut list says how much it cut, at every level: occurrences inside a cluster, instances
    /// inside a flow, and each section's own list.
    #[test]
    fn a_truncated_list_says_it_was_truncated() {
        let flows = found(vec![cluster(12, 8)], 5, vec![flow(2, 9, 8)]);
        let instrumentation = instrumentation();
        let rendered = capped(&flows, &instrumentation, 1, 1).render();
        assert!(rendered.contains("**5 clusters**"));
        assert!(rendered.contains("**12 occurrences across 2 sessions in 2 repositories**"));
        assert!(rendered.contains("- _and 4 further occurrences, not listed_"));
        assert!(rendered.contains("_4 further clusters are not shown._"));
        assert!(rendered.contains("- _and 1 further occurrence, not listed_"));
        assert!(rendered.contains("- archived 2026-08-20T10:00:00Z (UTC) · `event 1` · `"));
    }

    /// A flow renders its steps in order and cites one instance per occurrence, carrying every
    /// step's ordinal so a reader can open the transcript at each one.
    #[test]
    fn a_flow_renders_its_steps_and_an_ordinal_per_step() {
        let flows = found(Vec::new(), 0, vec![flow(3, 4, 4)]);
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(
            rendered
                .contains("**3-step flow · 4 occurrences across 3 sessions in 2 repositories**"),
            "{rendered}",
        );
        assert!(rendered.contains("1. step 0 wording"));
        assert!(rendered.contains("2. step 1 wording"));
        assert!(rendered.contains("3. step 2 wording"));
        assert!(rendered.contains("`events 4 → 14 → 24`"));
        // And the tally beside it names the repositories, including the unattributed bucket.
        assert!(rendered.contains("`surdy/qanungo` ×2, `no repository recorded` ×1"));
    }

    /// A cut repository tally says how many it is not naming, so a shortened list is never mistaken
    /// for the whole of what was found.
    #[test]
    fn a_cut_repository_tally_states_its_remainder() {
        let mut only = cluster(20, 2);
        only.repositories = repositories(&[
            ("surdy/a", 5),
            ("surdy/b", 4),
            ("surdy/c", 3),
            ("surdy/d", 3),
            ("surdy/e", 2),
            ("surdy/f", 2),
        ]);
        only.repositories_found = 9;
        assert_eq!(only.repositories.len(), MAX_REPOSITORIES_PER_ROW);
        let flows = found(vec![only], 1, Vec::new());
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("in 9 repositories"));
        assert!(rendered.contains("`surdy/f` ×2, and 3 more"), "{rendered}");
    }

    /// The coverage section is the absence of the doctor's "not examined" list, said out loud: this
    /// lane holds nothing back, and a reader has to be able to see that rather than infer it.
    #[test]
    fn the_coverage_section_says_that_nothing_was_held_back() {
        let flows = found(vec![cluster(3, 3)], 1, Vec::new());
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("## Repositories read"));
        assert!(rendered.contains("no repository is held back for being too small"));
        assert!(rendered.contains("| `surdy/qanungo` | 30 | 240 |"));
        assert!(rendered.contains("| `no repository recorded` | 10 | 60 |"));
        assert!(
            !rendered.contains("Not examined"),
            "this lane has no such list: {rendered}",
        );
    }

    /// The footer states the scrub that ran and what it fired, exactly as the other prose lanes do,
    /// and the same fold rendered twice is byte-identical.
    #[test]
    fn the_footer_confesses_the_scrub_and_the_render_is_stable() {
        let flows = found(vec![cluster(3, 3)], 1, vec![flow(2, 2, 2)]);
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("scrubbed for secrets"));
        assert!(rendered.contains(PATTERN_REVISION));
        assert!(rendered.contains("_Instrumentation — sync"));
        assert!(rendered.contains("900 user messages (120 harness-written, 300 comparable)"));
        assert!(rendered.contains("one ordinal per step"));
        assert_eq!(rendered, report(&flows, &instrumentation).render());

        let mut bare = instrumentation.clone();
        bare.redactor = Redactor::new().with_secrets(false);
        let confessed = report(&flows, &bare).render();
        assert!(confessed.contains("**not scrubbed for secrets** (`--no-redact`)"));
    }

    /// The footer counts what fired and names no value — the invariant the redaction layer is built
    /// on, restated on this surface because it is the third one to quote transcript prose.
    #[test]
    fn the_footer_counts_what_fired_and_names_no_value() {
        let mut redaction = RedactionReport::default();
        redaction.absorb(
            &Redactor::new()
                .scrub("ghp_0123456789012345678901234567890123456")
                .report,
        );
        let flows = Flows {
            redaction,
            ..found(vec![cluster(3, 3)], 1, Vec::new())
        };
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("1 replacements were made"));
        assert!(!rendered.contains("ghp_0123"));
    }

    /// A window narrows the heading and the scope line; its absence means all of history, and the
    /// document says which.
    #[test]
    fn the_reach_is_stated_either_way() {
        let flows = found(vec![cluster(3, 3)], 1, Vec::new());
        let instrumentation = instrumentation();
        let all = report(&flows, &instrumentation).render();
        assert!(all.contains("# Repeated flows — all of history"));
        assert!(all.contains("across all of the archive's history"));

        let window = window("4w");
        let narrowed = FlowsReport {
            window: Some(&window),
            clusters: DEFAULT_CLUSTERS,
            flows: DEFAULT_FLOWS,
            generated_at: at("2026-09-04T00:00:00Z"),
            found: &flows,
            instrumentation: &instrumentation,
        }
        .render();
        assert!(narrowed.contains("# Repeated flows — last 4w"));
        assert!(narrowed.contains("in the last 4w (archived since 2026-08-07T00:00:00Z UTC)"));
    }

    /// A session with nothing in it is counted in the scope block, and a gap the mirror reported is
    /// carried into the document rather than left to be inferred from a smaller number.
    #[test]
    fn what_was_not_read_is_named_rather_than_omitted() {
        let flows = Flows {
            sessions: 5,
            sessions_without_messages: 2,
            gaps: vec![SkippedNote {
                count: 2,
                reason: "claude-code: snapshot has no `transcript.jsonl` artifact".to_owned(),
            }],
            ..Flows::default()
        };
        let instrumentation = instrumentation();
        let rendered = report(&flows, &instrumentation).render();
        assert!(rendered.contains("2 of the 5 sessions read carried no user message"));
        assert!(rendered.contains("## Gaps"));
        assert!(rendered.contains("- 2 — claude-code: snapshot has no"));
    }

    /// A [`Window`] the tests can build without going through clap by hand.
    fn window(value: &str) -> Window {
        let parsed = <crate::cli::Cli as clap::Parser>::try_parse_from([
            "qanungo",
            "flows",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--last",
            value,
        ])
        .expect("a window this lane accepts");
        let crate::cli::Command::Flows(args) = parsed.command else {
            panic!("`flows` parses as the flows command");
        };
        args.last.expect("the window was given")
    }
}
