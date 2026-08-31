//! Markdown rendering for the doctor lane.
//!
//! Like the standup and ask documents, this one renders prose that came out of the archive — the
//! excerpt of a repeated instruction — so it holds its redaction line the same way: **every string
//! below was scrubbed on the way into [`Doctor`] by [`crate::doctor`]**, before it reached this
//! module, and there is no unscrubbed copy in scope to render by mistake. The footer sentence is the
//! standup lane's, shared rather than reworded, because all three documents make the same promise
//! about the same scrub.
//!
//! # The sentence this document must never write
//!
//! There is no causal claim anywhere in here, and that is a property of the text as much as of the
//! fold. This lane can see that the same instruction was typed in several sessions of one
//! repository. It cannot see the repository — qanungo never opens a checkout — so it does not know
//! whether `CLAUDE.md` already says the thing, said it badly, or does not exist. "A missing
//! instruction caused this rework" is a sentence no reading of a transcript supports, and the
//! preamble says so out loud rather than leaving a reader to infer the bound from what is absent.
//!
//! The document therefore does two things and stops: it shows the repetition, and it cites where the
//! repetition was. What to do about it is the `instructions-editor` skill's half, in the harness,
//! inside the repository, under the user's own permission prompts.
//!
//! # It shows its own thresholds
//!
//! A cluster is the output of four constants, and a reader who cannot see them cannot tell a real
//! finding from a threshold artefact. So the scope block states the phrase length, the overlap it
//! insisted on, the floor under a clusterable message, and the sessions a cluster has to span —
//! the same discipline the ask lane's rubric line holds, for the same reason.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::cli::Window;
use crate::doctor::{
    Cluster, Doctor, Friction, MIN_CLUSTER_SESSIONS, MIN_CLUSTERABLE_WORDS,
    MIN_SHARED_INSTRUCTIONS, RepositoryClusters, SAME_CONVERSATION_PERCENT, SHINGLE_WORDS,
    SIMILARITY_THRESHOLD_PERCENT,
};
use crate::format;
use crate::redaction::{PATTERN_REVISION, Redactor};
use crate::report::{SkippedNote, stamp};
use crate::standup_report::{redaction_counts, redaction_line};
use crate::sync::SyncStats;

/// What a doctor run cost, folded into the footer.
#[derive(Debug, Clone)]
pub struct DoctorInstrumentation {
    pub sync: SyncStats,
    /// Wall-time of reading the transcripts and clustering their messages, network excluded.
    pub fold_elapsed: Duration,
    /// The redactor the flags asked for, so the footer can say which passes ran.
    pub redactor: Redactor,
    pub patwari_url: String,
    pub cache_root: PathBuf,
}

/// Everything one doctor document is rendered from.
pub struct DoctorReport<'a> {
    /// The window that narrowed the reach, or `None` for all of history.
    pub window: Option<&'a Window>,
    pub generated_at: DateTime<Utc>,
    pub doctor: &'a Doctor,
    pub instrumentation: &'a DoctorInstrumentation,
}

impl DoctorReport<'_> {
    /// Renders the clusters, the friction, and what was not looked at, as Markdown.
    ///
    /// Deterministic in full: the same reach over the same archive with the same flags produces
    /// byte-identical output, because every ordering in [`Doctor`] is total and nothing here reads a
    /// clock except the two timestamps it prints.
    pub fn render(&self) -> String {
        let mut out = String::new();
        match self.window {
            Some(window) => {
                let _ = writeln!(out, "# Instructions doctor — last {window}\n");
            }
            None => out.push_str("# Instructions doctor — all of history\n\n"),
        }
        self.render_scope(&mut out);
        self.render_clusters(&mut out);
        self.render_friction(&mut out);
        self.render_unexamined(&mut out);
        self.render_gaps(&mut out);
        self.render_footer(&mut out);
        out
    }

    /// What was read, what the tool can and cannot see, and the thresholds that produced the rest.
    fn render_scope(&self, out: &mut String) {
        let doctor = self.doctor;
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
            doctor.sessions,
            plural(doctor.sessions, "session", "sessions"),
            stamp(self.generated_at),
            doctor.messages,
            plural(doctor.messages, "message", "messages"),
            doctor.harness_generated,
            doctor.clusterable,
        );
        out.push_str(
            "\nThis reads transcripts, not repositories. It can show that you typed nearly the same \
             instruction in several sessions of one repository; it has **no idea what your \
             `CLAUDE.md` or `AGENTS.md` says**, because qanungo never opens a checkout. So what is \
             below is *repetition*, not a diagnosis: no line here claims that a missing \
             instruction is the reason for anything, and whether repeated text belongs in an \
             instruction file is a judgement this document deliberately leaves to you.\n",
        );
        let _ = writeln!(
            out,
            "\nTwo messages of one repository are called the same instruction when they share at \
             least {SIMILARITY_THRESHOLD_PERCENT}% of the shorter one's {SHINGLE_WORDS}-word \
             phrases. A message under {MIN_CLUSTERABLE_WORDS} words is never compared — an \
             acknowledgement is not an instruction — and a cluster is reported only when it spans \
             at least {MIN_CLUSTER_SESSIONS} distinct sessions, because restating something inside \
             one session is a conversation. Two sessions that share at least \
             {MIN_SHARED_INSTRUCTIONS} instructions **and** {SAME_CONVERSATION_PERCENT}% of the \
             smaller one's are read as one conversation captured twice — a resumed session replays \
             what came before it, and counting that as repetition would turn one long conversation \
             into a page of findings. Every one of those numbers is an arbitrary-until-measured \
             constant, named in `crates/qanungo/src/doctor.rs`.",
        );
        out.push_str(
            "\nThe harness-written count above is a floor on that noise rather than a proof of its \
             absence: a harness can inject anything, and this build recognizes only the shapes \
             somebody has looked at. Injected text in an unrecognized shape still reaches the \
             comparison, so a cluster that is plainly machinery rather than an instruction is a gap \
             in that list — not a contradiction of the count above.\n",
        );
        if doctor.sessions_without_messages > 0 {
            let _ = writeln!(
                out,
                "\n{} of the {} {} read carried no user message this build could read, so there \
                 was nothing in {} to compare — counted here rather than passed over as an empty \
                 result.",
                doctor.sessions_without_messages,
                doctor.sessions,
                plural(doctor.sessions, "session", "sessions"),
                plural(doctor.sessions_without_messages, "it", "them"),
            );
        }
    }

    fn render_clusters(&self, out: &mut String) {
        out.push_str("\n## Repeated instructions\n");
        if self.doctor.is_empty() {
            let _ = writeln!(
                out,
                "\nNo instruction cleared these thresholds in any repository. That is an answer \
                 about this archive at these settings — nothing was found and hidden.",
            );
            return;
        }
        let _ = writeln!(
            out,
            "\n**{} {} in {} of the {} {} examined**, grouped by the repository the archive listed \
             each session under; the sessions of those examined repositories were read as {} \
             distinct {}, because a resumed session replays the one before it. A cluster never \
             spans two repositories: an instruction missing from one repository's files is that \
             repository's business.",
            self.doctor.clusters,
            plural(self.doctor.clusters, "cluster", "clusters"),
            self.doctor.repositories.len(),
            self.doctor.repositories_examined,
            plural(
                self.doctor.repositories_examined,
                "repository",
                "repositories"
            ),
            self.doctor.conversations,
            plural(self.doctor.conversations, "conversation", "conversations"),
        );
        for section in &self.doctor.repositories {
            render_repository(out, section);
        }
    }

    /// The corroborating stats: aggregates per repository, and the sentence that keeps them from
    /// being read as a second finding.
    fn render_friction(&self, out: &mut String) {
        out.push_str("\n## Friction\n\n");
        if self.doctor.friction.is_empty() {
            out.push_str("No session was read, so there is nothing to count.\n");
            return;
        }
        out.push_str(
            "User messages that arrived while the last outcome a tool reported was a failure. This \
             is a **proxy**, and a coarse one: a session written to chase a failing test produces \
             the same shape as an instruction nobody wrote down, so it corroborates the clusters \
             above at most and is never attributed to one. Counts only — no message is quoted \
             here.\n\n",
        );
        out.push_str("| Repository | Sessions | Typed messages | After a failure | Rate |\n");
        out.push_str("| --- | ---: | ---: | ---: | ---: |\n");
        for friction in &self.doctor.friction {
            render_friction_row(out, friction);
        }
    }

    /// Repositories the fold saw and did not cluster, named with the reason.
    ///
    /// Rendered rather than omitted for the reason every section of every document in this crate is:
    /// a repository absent from a report is indistinguishable from a repository with nothing to say,
    /// and only one of those is true here.
    fn render_unexamined(&self, out: &mut String) {
        if self.doctor.unexamined.is_empty() {
            return;
        }
        out.push_str("\n## Not examined for repetition\n\n");
        out.push_str(
            "These repositories were read and counted above, and their messages were not compared:\
             \n\n",
        );
        for unexamined in &self.doctor.unexamined {
            let _ = writeln!(
                out,
                "- `{}` — {} {}, {}",
                unexamined.repository,
                unexamined.sessions,
                plural(unexamined.sessions, "session", "sessions"),
                unexamined.reason,
            );
        }
    }

    fn render_gaps(&self, out: &mut String) {
        let gaps: &[SkippedNote] = &self.doctor.gaps;
        if gaps.is_empty() {
            return;
        }
        out.push_str("\n## Gaps\n\n");
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
            &self.doctor.redaction,
        ));
        out.push_str(
            "\n\nEvery occurrence is cited by the content hash of the `transcript.jsonl` it was \
             read from, beside the event's own ordinal in that file. To read one in full, ask the \
             archive for the artifact and fetch the `content_url` that comes back (the filter takes \
             the bare digest, without the `sha256:` prefix):\n\n",
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
            self.doctor.sessions,
            self.doctor.messages,
            self.doctor.harness_generated,
            self.doctor.clusterable,
            format::bytes(self.doctor.bytes_folded),
            instrumentation.sync.cache_hits,
            instrumentation.sync.cache_misses,
            format::bytes(instrumentation.sync.bytes_transferred),
            instrumentation.sync.snapshots_indexed,
            instrumentation.sync.snapshots_fetched,
            self.doctor.unreadable_records,
            redaction_counts(&self.doctor.redaction),
            instrumentation.patwari_url,
            display_path(&instrumentation.cache_root),
        );
    }
}

/// One repository's section: its clusters, best first, and what the cut hid.
fn render_repository(out: &mut String, section: &RepositoryClusters) {
    let _ = writeln!(out, "\n### {}\n", section.repository);
    for cluster in &section.clusters {
        render_cluster(out, cluster);
    }
    if section.found > section.clusters.len() {
        let _ = writeln!(
            out,
            "_{} further {} in this repository {} not shown._",
            section.found - section.clusters.len(),
            plural(
                section.found - section.clusters.len(),
                "cluster",
                "clusters"
            ),
            plural(section.found - section.clusters.len(), "is", "are"),
        );
    }
}

/// One cluster: how often, across how many sessions, the fullest wording of it, and where each
/// occurrence was.
fn render_cluster(out: &mut String, cluster: &Cluster) {
    let _ = writeln!(
        out,
        "**{} {} across {} {}**\n",
        cluster.occurrences,
        plural(cluster.occurrences, "occurrence", "occurrences"),
        cluster.sessions,
        plural(cluster.sessions, "session", "sessions"),
    );
    let _ = writeln!(out, "> {}\n", cluster.excerpt);
    for citation in &cluster.citations {
        let archived = match citation.archived_at {
            Some(at) => stamp(at),
            // Unreachable for a session inside a reach — placement selects on this very field — but
            // the type admits it, and inventing a date for a session whose own the archive stated
            // unreadably is exactly the guess the mirror refuses to make.
            None => "an unreadable time".to_owned(),
        };
        let _ = writeln!(
            out,
            "- archived {archived} (UTC) · `event {}` · `{}`",
            citation.locator, citation.source_hash,
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

/// One friction row. The rate is stated only when there is a denominator to state it against: a
/// repository whose sessions carried no user message has no rate, and printing `0%` for one would
/// be a reading where there is none.
fn render_friction_row(out: &mut String, friction: &Friction) {
    let rate = if friction.messages > 0 {
        format::percent(friction.after_error as f64 / friction.messages as f64)
    } else {
        "—".to_owned()
    };
    let _ = writeln!(
        out,
        "| `{}` | {} | {} | {} | {rate} |",
        friction.repository, friction.sessions, friction.messages, friction.after_error,
    );
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
    use crate::doctor::{Citation, Unexamined};
    use crate::redaction::RedactionReport;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn instrumentation() -> DoctorInstrumentation {
        DoctorInstrumentation {
            sync: SyncStats::default(),
            fold_elapsed: Duration::from_millis(5),
            redactor: Redactor::new(),
            patwari_url: "https://patwari.example".to_owned(),
            cache_root: PathBuf::from("/cache"),
        }
    }

    fn report<'a>(
        doctor: &'a Doctor,
        instrumentation: &'a DoctorInstrumentation,
    ) -> DoctorReport<'a> {
        DoctorReport {
            window: None,
            generated_at: at("2026-08-30T00:00:00Z"),
            doctor,
            instrumentation,
        }
    }

    fn cluster(occurrences: usize, citations: usize) -> Cluster {
        Cluster {
            occurrences,
            sessions: 2,
            excerpt: "always run cargo fmt and clippy before you say a change is done".to_owned(),
            citations: (0..citations)
                .map(|index| Citation {
                    archived_at: Some(at("2026-08-20T10:00:00Z")),
                    source_hash: format!("{index}").repeat(64),
                    locator: index as u64 + 1,
                })
                .collect(),
        }
    }

    fn found(clusters: Vec<Cluster>, found: usize) -> Doctor {
        Doctor {
            repositories: vec![RepositoryClusters {
                repository: "surdy/qanungo".to_owned(),
                occurrences: clusters.iter().map(|cluster| cluster.occurrences).sum(),
                found,
                clusters,
            }],
            clusters: found,
            conversations: 36,
            repositories_examined: 3,
            sessions: 40,
            messages: 900,
            harness_generated: 120,
            clusterable: 300,
            ..Doctor::default()
        }
    }

    /// The document never claims a cause, and says so in its own voice rather than leaving the
    /// bound to be inferred from what is missing.
    #[test]
    fn the_preamble_refuses_the_causal_claim_out_loud() {
        let doctor = found(vec![cluster(3, 3)], 1);
        let instrumentation = instrumentation();
        let rendered = report(&doctor, &instrumentation).render();
        assert!(rendered.contains("no idea what your"));
        assert!(rendered.contains("*repetition*, not a diagnosis"));
        assert!(rendered.contains("leaves to you"));
        // The words a causal reading would need are nowhere in the document.
        for forbidden in [
            "caused",
            "prevented",
            "led to",
            "would have prevented",
            "because there is no instruction",
        ] {
            assert!(!rendered.contains(forbidden), "{forbidden}: {rendered}");
        }
    }

    /// The thresholds that produced the finding are in the finding, so a reader can tell a real
    /// repetition from a threshold artefact.
    #[test]
    fn the_document_states_its_own_thresholds() {
        let doctor = found(vec![cluster(3, 3)], 1);
        let instrumentation = instrumentation();
        let rendered = report(&doctor, &instrumentation).render();
        assert!(rendered.contains("share at least 60% of the shorter one's 4-word phrases"));
        assert!(rendered.contains("under 8 words is never compared"));
        assert!(rendered.contains("at least 2 distinct sessions"));
        assert!(rendered.contains("arbitrary-until-measured"));
    }

    /// An empty result is an answer about the archive, not a blank section a reader has to guess
    /// at, and it is not phrased as a truncation.
    #[test]
    fn no_repetition_is_stated_as_an_answer() {
        let doctor = Doctor {
            sessions: 40,
            messages: 900,
            clusterable: 300,
            repositories_examined: 3,
            ..Doctor::default()
        };
        let instrumentation = instrumentation();
        let rendered = report(&doctor, &instrumentation).render();
        assert!(rendered.contains("No instruction cleared these thresholds"));
        assert!(rendered.contains("nothing was found and hidden"));
        assert!(rendered.contains("Read 40 sessions"));
        assert!(rendered.contains("They carried 900 user messages"));
    }

    /// A cut list says how much it cut, at both levels: occurrences inside a cluster and clusters
    /// inside a repository.
    #[test]
    fn a_truncated_list_says_it_was_truncated() {
        let doctor = found(vec![cluster(12, 8)], 5);
        let instrumentation = instrumentation();
        let rendered = report(&doctor, &instrumentation).render();
        // The header names the repositories the clusters are actually *in* as well as the number
        // examined: one is not the other, and reporting only the second reads as if every examined
        // repository carried a cluster.
        assert!(rendered.contains("**5 clusters in 1 of the 3 repositories examined**"));
        assert!(rendered.contains("**12 occurrences across 2 sessions**"));
        assert!(rendered.contains("- _and 4 further occurrences, not listed_"));
        assert!(rendered.contains("_4 further clusters in this repository are not shown._"));
        assert!(rendered.contains("- archived 2026-08-20T10:00:00Z (UTC) · `event 1` · `"));
    }

    /// The friction table is aggregates, and the sentence above it refuses to let them read as a
    /// second finding. A repository with no user message gets no rate rather than a flattering
    /// zero.
    #[test]
    fn friction_is_aggregates_with_its_own_bound_stated() {
        let doctor = Doctor {
            friction: vec![
                Friction {
                    repository: "surdy/qanungo".to_owned(),
                    sessions: 12,
                    messages: 400,
                    after_error: 40,
                },
                Friction {
                    repository: "surdy/silent".to_owned(),
                    sessions: 1,
                    messages: 0,
                    after_error: 0,
                },
            ],
            ..Doctor::default()
        };
        let instrumentation = instrumentation();
        let rendered = report(&doctor, &instrumentation).render();
        assert!(rendered.contains("This is a **proxy**, and a coarse one"));
        assert!(rendered.contains("never attributed to one"));
        assert!(rendered.contains("| `surdy/qanungo` | 12 | 400 | 40 | 10% |"));
        assert!(rendered.contains("| `surdy/silent` | 1 | 0 | 0 | — |"));
    }

    /// A repository nothing was reported for is listed with its reason rather than being absent,
    /// and a session with nothing to read is counted in the scope block.
    #[test]
    fn what_was_not_examined_is_named_rather_than_omitted() {
        let doctor = Doctor {
            sessions: 5,
            sessions_without_messages: 2,
            unexamined: vec![Unexamined {
                repository: "surdy/once".to_owned(),
                sessions: 1,
                reason: "fewer sessions in this reach than a cross-session cluster needs",
            }],
            gaps: vec![SkippedNote {
                count: 2,
                reason: "claude-code: snapshot has no `transcript.jsonl` artifact".to_owned(),
            }],
            ..Doctor::default()
        };
        let instrumentation = instrumentation();
        let rendered = report(&doctor, &instrumentation).render();
        assert!(rendered.contains("## Not examined for repetition"));
        assert!(rendered.contains("- `surdy/once` — 1 session, fewer sessions in this reach"));
        assert!(rendered.contains("2 of the 5 sessions read carried no user message"));
        assert!(rendered.contains("## Gaps"));
        assert!(rendered.contains("- 2 — claude-code: snapshot has no"));
    }

    /// The footer states the scrub that ran and what it fired, exactly as the other prose lanes do,
    /// and the same fold rendered twice is byte-identical.
    #[test]
    fn the_footer_confesses_the_scrub_and_the_render_is_stable() {
        let doctor = found(vec![cluster(3, 3)], 1);
        let instrumentation = instrumentation();
        let rendered = report(&doctor, &instrumentation).render();
        assert!(rendered.contains("scrubbed for secrets"));
        assert!(rendered.contains(PATTERN_REVISION));
        assert!(rendered.contains("_Instrumentation — sync"));
        assert!(rendered.contains("900 user messages (120 harness-written, 300 comparable)"));
        assert_eq!(rendered, report(&doctor, &instrumentation).render());

        let mut bare = instrumentation.clone();
        bare.redactor = Redactor::new().with_secrets(false);
        let confessed = report(&doctor, &bare).render();
        assert!(confessed.contains("**not scrubbed for secrets** (`--no-redact`)"));
    }

    /// A window narrows the heading and the scope line; its absence means all of history, and the
    /// document says which.
    #[test]
    fn the_reach_is_stated_either_way() {
        let doctor = found(vec![cluster(3, 3)], 1);
        let instrumentation = instrumentation();
        assert!(
            report(&doctor, &instrumentation)
                .render()
                .contains("across all of the archive's history")
        );

        let window = "4w".parse::<TestWindow>().unwrap().0;
        let narrowed = DoctorReport {
            window: Some(&window),
            generated_at: at("2026-08-30T00:00:00Z"),
            doctor: &doctor,
            instrumentation: &instrumentation,
        }
        .render();
        assert!(narrowed.contains("# Instructions doctor — last 4w"));
        assert!(narrowed.contains("in the last 4w (archived since 2026-08-02T00:00:00Z UTC)"));
    }

    /// A [`Window`] the tests can build without going through clap.
    struct TestWindow(Window);

    impl std::str::FromStr for TestWindow {
        type Err = String;

        fn from_str(value: &str) -> Result<Self, Self::Err> {
            let parsed = <crate::cli::Cli as clap::Parser>::try_parse_from([
                "qanungo", "doctor", "--last", value,
            ])
            .map_err(|error| error.to_string())?;
            let crate::cli::Command::Doctor(args) = parsed.command else {
                return Err("`doctor` parses as the doctor command".to_owned());
            };
            args.last
                .map(TestWindow)
                .ok_or_else(|| "no window".to_owned())
        }
    }

    /// A redaction report with something in it renders its counts in the footer, and the counts are
    /// all it renders — the invariant the layer is built on.
    #[test]
    fn the_footer_counts_what_fired_and_names_no_value() {
        let mut redaction = RedactionReport::default();
        redaction.absorb(
            &Redactor::new()
                .scrub("ghp_0123456789012345678901234567890123456")
                .report,
        );
        let doctor = Doctor {
            redaction,
            ..found(vec![cluster(3, 3)], 1)
        };
        let instrumentation = instrumentation();
        let rendered = report(&doctor, &instrumentation).render();
        assert!(rendered.contains("1 replacements were made"));
        assert!(!rendered.contains("ghp_0123"));
    }
}
