//! Fixture-backed proof that the doctor lane clusters, cites, and — above all — scrubs, over the
//! real cache-read-and-interpret path rather than a constructed one.
//!
//! Every fixture is written into a real [`BlobCache`] under its own content hash and read back with
//! [`read_messages`], so the cache read, the `munshi-transcript` interpretation, and the event walk
//! all run; only the network is absent, which `tests/mirror.rs` covers.
//!
//! The repository each session belongs to is supplied here rather than read out of the transcript,
//! because that is exactly where it comes from in production: the archive's own listing row
//! ([`MirroredSession::repository`]). That is what lets two of these tests fold the *same* two
//! transcripts under one repository and under two, and see a cluster in the first case and none in
//! the second.

use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use qanungo::ask::MAX_SNIPPET_CHARS;
use qanungo::cache::BlobCache;
use qanungo::doctor::{DEFAULT_CLUSTERS_PER_REPOSITORY, Doctor, DoctorSession};
use qanungo::doctor_report::{DoctorInstrumentation, DoctorReport};
use qanungo::metrics::source_for_agent;
use qanungo::patwari::sha256_hex;
use qanungo::redaction::{RedactionReport, Redactor};
use qanungo::sync::{MirroredSession, SyncStats};

/// The credentials planted in `repeated-rule.jsonl` and `repeated-rule-restated.jsonl`, inside the
/// very instruction the two sessions repeat. Neither has ever been real; each is a shape with
/// `CANARY` spelled through its body, as everywhere else in this tree.
///
/// There are two of them, at two positions, because they prove two different things. The Anthropic
/// key sits early enough to be replaced *and rendered whole*, so a reader can see the marker in the
/// excerpt. The GitHub token straddles the excerpt's own 200-character cut, which is the position
/// that makes the scrub-then-clip **order** observable — see the canary test.
const PLANTED_KEY: &str = "sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99";
const PLANTED_TOKEN: &str = "ghp_CANARYCANARYCANARYCANARYCANARYCANARY";

/// The words that survive around them, so a scrubbed excerpt can be shown to be an excerpt rather
/// than a hole.
const AROUND: &str = "always run cargo fmt and clippy before you tell me a change is done";

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/doctor")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

/// A cache holding one fixture per digest, exactly as a mirror run would have left it.
struct Archive {
    cache: BlobCache,
    _root: tempfile::TempDir,
}

impl Archive {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("a temporary cache root");
        let cache = BlobCache::open(root.path()).expect("an openable cache");
        Self { cache, _root: root }
    }

    /// Stores one transcript fixture and reads it back through the lane's own entry point.
    fn read(&self, name: &str, archived_at: &str, repository: Option<&str>) -> DoctorSession {
        let bytes = fixture(name);
        let source_hash = sha256_hex(&bytes);
        self.cache
            .store(&source_hash, &bytes)
            .expect("the cache accepts a blob");
        let mirrored = MirroredSession {
            session_id: "1".repeat(32),
            snapshot_id: "2".repeat(32),
            source_hash: source_hash.clone(),
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            size_bytes: bytes.len() as u64,
            archived_at: Some(at(archived_at)),
            repository: repository.map(str::to_owned),
            hostname: None,
            utc_offset: None,
        };
        let blob = self
            .cache
            .open_blob(&mirrored.source_hash)
            .expect("the blob is readable");
        let messages = qanungo::repetition::read_messages(
            source_for_agent(&mirrored.source_agent).expect("this build interprets claude-code"),
            mirrored.artifact_set_version,
            BufReader::new(blob),
        )
        .expect("v2 is a supported contract");
        DoctorSession {
            source_hash,
            archived_at: mirrored.archived_at,
            repository: mirrored.repository,
            bytes_folded: mirrored.size_bytes,
            messages,
        }
    }
}

/// The two sessions that repeat one instruction, both listed under one repository.
fn one_repository(archive: &Archive) -> Vec<DoctorSession> {
    vec![
        archive.read(
            "repeated-rule.jsonl",
            "2026-08-20T10:00:00Z",
            Some("surdy/qanungo"),
        ),
        archive.read(
            "repeated-rule-restated.jsonl",
            "2026-08-21T15:00:00Z",
            Some("surdy/qanungo"),
        ),
    ]
}

/// Folded at the default per-repository cut: these fixtures cluster far under it, and what the cut
/// itself does is `doctor.rs`'s own test.
fn fold(sessions: &[DoctorSession], redactor: &Redactor) -> Doctor {
    Doctor::fold(
        sessions,
        Vec::new(),
        &RedactionReport::default(),
        redactor,
        DEFAULT_CLUSTERS_PER_REPOSITORY,
    )
}

fn render(doctor: &Doctor, redactor: Redactor) -> String {
    let instrumentation = DoctorInstrumentation {
        sync: SyncStats::default(),
        fold_elapsed: Duration::from_millis(1),
        redactor,
        patwari_url: "https://patwari.example".to_owned(),
        cache_root: PathBuf::from("/cache"),
    };
    DoctorReport {
        window: None,
        clusters_per_repo: DEFAULT_CLUSTERS_PER_REPOSITORY,
        generated_at: at("2026-08-30T00:00:00Z"),
        doctor,
        instrumentation: &instrumentation,
    }
    .render()
}

/// The finding, over the real interpret-and-cluster path: one instruction typed in two sessions of
/// one repository, quoted once and cited twice, with the unrelated messages beside it left alone.
#[test]
fn an_instruction_repeated_across_two_sessions_is_the_finding() {
    let archive = Archive::new();
    let sessions = one_repository(&archive);
    let doctor = fold(&sessions, &Redactor::new());

    assert_eq!(doctor.sessions, 2);
    assert_eq!(doctor.repositories_examined, 1);
    assert_eq!(
        doctor.messages, 5,
        "four typed messages in the first session and two in the second, less the tool result",
    );
    assert_eq!(
        doctor.clusterable, 4,
        "the bare `yes` is counted and never compared",
    );

    assert_eq!(doctor.repositories.len(), 1);
    let section = &doctor.repositories[0];
    assert_eq!(section.repository, "surdy/qanungo");
    assert_eq!(section.found, 1, "one repetition, not one per message");
    let cluster = &section.clusters[0];
    assert_eq!(cluster.occurrences, 2);
    assert_eq!(cluster.sessions, 2);
    assert!(
        cluster.excerpt.contains(AROUND),
        "the fullest wording speaks for the cluster: {}",
        cluster.excerpt,
    );

    // Newest first, and each citation carries the transcript hash and the event's own ordinal.
    assert_eq!(cluster.citations.len(), 2);
    assert_eq!(cluster.citations[0].source_hash, sessions[1].source_hash);
    assert_eq!(cluster.citations[1].source_hash, sessions[0].source_hash);
    assert_eq!(cluster.citations[1].locator, 1);

    let document = render(&doctor, Redactor::new());
    assert!(document.contains("**2 occurrences across 2 sessions**"));
    assert!(document.contains("### surdy/qanungo"));
    assert!(document.contains(&format!("`{}`", sessions[0].source_hash)));
}

/// The canary, and the ordering it actually rests on.
///
/// The repeated instruction carries a credential that **straddles the excerpt's own cut**: the
/// token starts at character 180 of a message longer than [`MAX_SNIPPET_CHARS`], so a
/// clip-then-scrub order would hand back its first twenty characters — four short of what the
/// `github-token` pattern needs to recognize it — and those characters would render as themselves.
///
/// The test checks its own premise first, by running the wrong order deliberately and asserting
/// that it *would* have leaked. A change to the clip ceiling or to the fixture that stopped the
/// token straddling the edge fails here rather than quietly turning this into a test that cannot
/// fail.
#[test]
fn a_repeated_instruction_carrying_a_credential_renders_it_scrubbed() {
    let archive = Archive::new();
    let sessions = one_repository(&archive);

    // The premise, stated as an assertion: cutting first and scrubbing after leaves a long run of
    // the token on the screen.
    let raw = &sessions[0].messages.clusterable[0].text;
    let cut: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_SNIPPET_CHARS)
        .collect();
    let wrong_order = Redactor::new().scrub(&cut).text;
    assert!(
        wrong_order.contains(&PLANTED_TOKEN[..20]),
        "the token must straddle the cut for this test to mean anything: {wrong_order}",
    );

    let doctor = fold(&sessions, &Redactor::new());
    let cluster = &doctor.repositories[0].clusters[0];
    assert_eq!(
        cluster.occurrences, 2,
        "the credential did not decide what clustered",
    );
    // Both credentials are gone and both markers are whole: scrubbing the *line* before cutting it
    // is what leaves a 23-character marker where a 40-character token was.
    assert!(
        cluster.excerpt.contains("[REDACTED:anthropic-key]"),
        "{}",
        cluster.excerpt
    );
    assert!(
        cluster.excerpt.contains("[REDACTED:github-token]"),
        "{}",
        cluster.excerpt
    );
    assert!(cluster.excerpt.contains(AROUND), "{}", cluster.excerpt);
    // Not a fragment of either, anywhere: no eight consecutive characters of a planted credential
    // survive into the rendered document.
    let document = render(&doctor, Redactor::new());
    for planted in [PLANTED_KEY, PLANTED_TOKEN] {
        let characters: Vec<char> = planted.chars().collect();
        for window in characters.windows(8) {
            let fragment: String = window.iter().collect();
            assert!(
                !document.contains(&fragment),
                "a credential fragment survived: {fragment}",
            );
        }
    }
    assert!(document.contains("replacements were made"));
    assert!(document.contains("scrubbed for secrets"));
}

/// The flag is real on this surface too: `--no-redact` hands back the transcript's own words, and
/// the footer confesses it. A flag that quietly kept scrubbing would be worse than no flag.
#[test]
fn no_redact_lets_the_credential_through_and_says_so() {
    let archive = Archive::new();
    let sessions = one_repository(&archive);
    let bare = Redactor::new().with_secrets(false);
    let doctor = fold(&sessions, &bare);
    let document = render(&doctor, bare);

    assert!(document.contains("**not scrubbed for secrets** (`--no-redact`)"));
    assert!(
        document.contains(PLANTED_KEY),
        "with the scrub off the instruction's own text is what is quoted",
    );
    // The token is past the excerpt's cut once nothing shortens the line, so what this proves is
    // that the scrub is the only thing that was removing it.
    assert!(!document.contains("[REDACTED:"));
}

/// The same instruction, the same two transcripts, listed under two different repositories: no
/// cluster, because an instruction missing from one repository's files is that repository's
/// business and the index is built per repository.
#[test]
fn the_same_instruction_in_two_repositories_never_clusters() {
    let archive = Archive::new();
    let split = vec![
        archive.read(
            "repeated-rule.jsonl",
            "2026-08-20T10:00:00Z",
            Some("surdy/qanungo"),
        ),
        archive.read(
            "repeated-rule-restated.jsonl",
            "2026-08-21T15:00:00Z",
            Some("surdy/munshi"),
        ),
    ];
    let doctor = fold(&split, &Redactor::new());
    assert!(doctor.is_empty(), "{:?}", doctor.repositories);

    // And both repositories are named as unexamined rather than being absent from the document.
    let document = render(&doctor, Redactor::new());
    assert!(document.contains("## Not examined for repetition"));
    assert!(document.contains("- `surdy/qanungo` — 1 session"));
    assert!(document.contains("- `surdy/munshi` — 1 session"));
    assert!(document.contains("No instruction cleared these thresholds"));
}

/// A corpus with nothing repeated in it is an answer about the archive, not a blank page — the
/// distinction somebody asking "have I been repeating myself" depends on.
#[test]
fn a_corpus_with_no_repetition_is_an_honest_no() {
    let archive = Archive::new();
    let sessions = vec![
        archive.read(
            "no-repetition.jsonl",
            "2026-08-22T12:00:00Z",
            Some("surdy/qanungo"),
        ),
        archive.read(
            "repeated-rule-restated.jsonl",
            "2026-08-21T15:00:00Z",
            Some("surdy/qanungo"),
        ),
    ];
    let doctor = fold(&sessions, &Redactor::new());
    assert_eq!(
        doctor.repositories_examined, 1,
        "the repository was looked at"
    );
    assert_eq!(doctor.clusters, 0);
    assert!(doctor.is_empty());

    let document = render(&doctor, Redactor::new());
    assert!(document.contains("No instruction cleared these thresholds in any repository"));
    assert!(document.contains("nothing was found and hidden"));
    // Not phrased as a truncation, and the reach and the thresholds are still stated.
    assert!(document.contains("Read 2 sessions"));
    assert!(document.contains("arbitrary-until-measured"));
}

/// The friction proxy, over a real transcript: one failing `cargo clippy`, one message replying to
/// it, and the table saying so in aggregate with nothing quoted.
#[test]
fn friction_is_counted_per_repository_and_quotes_nothing() {
    let archive = Archive::new();
    let sessions = one_repository(&archive);
    let doctor = fold(&sessions, &Redactor::new());

    assert_eq!(doctor.friction.len(), 1);
    let friction = &doctor.friction[0];
    assert_eq!(friction.repository, "surdy/qanungo");
    assert_eq!(friction.sessions, 2);
    assert_eq!(friction.messages, 5);
    assert_eq!(
        friction.after_error, 1,
        "one failing tool, one message attributed to it",
    );

    let document = render(&doctor, Redactor::new());
    assert!(document.contains("| `surdy/qanungo` | 2 | 5 | 1 | 20% |"));
    assert!(document.contains("This is a **proxy**, and a coarse one"));
    // The section carries no transcript text: the clippy error the session actually saw is nowhere
    // in the friction table, and nor is the message that replied to it.
    let table = document
        .split("## Friction")
        .nth(1)
        .expect("the friction section")
        .split("\n---\n")
        .next()
        .expect("the section body");
    assert!(!table.contains("immediately dereferenced"), "{table}");
    assert!(!table.contains("silencing it with an allow"), "{table}");
}

/// The document is deterministic in full: the same corpus rendered twice, and folded in either
/// order, produces the same bytes.
#[test]
fn the_document_is_deterministic() {
    let archive = Archive::new();
    let sessions = one_repository(&archive);
    let forwards = render(&fold(&sessions, &Redactor::new()), Redactor::new());
    assert_eq!(
        forwards,
        render(&fold(&sessions, &Redactor::new()), Redactor::new()),
    );

    let mut backwards = sessions.clone();
    backwards.reverse();
    assert_eq!(
        forwards,
        render(&fold(&backwards, &Redactor::new()), Redactor::new()),
        "the archive's listing order is not part of the finding",
    );
}
