//! Fixture-backed proof that the standup lane groups, orders, rolls up, and scrubs — and that a
//! summary carrying planted credentials cannot put one of them on the screen.
//!
//! Every fixture goes through the *real* path: it is written into a real [`BlobCache`] under its
//! own content hash and read back with [`read_summary`], so the cache read, the UTF-8 check, the
//! `munshi-transcript` parse, and the placeholder verdict are exercised rather than stepped over.
//! Only the network is absent, and the network is what `tests/mirror.rs` covers.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::Parser;
use qanungo::cache::BlobCache;
use qanungo::cli::{Cli, Command, Window};
use qanungo::patwari::sha256_hex;
use qanungo::redaction::{PatternId, Redactor};
use qanungo::standup::{Gap, GapReason, NO_REPOSITORY, ReadSummary, Standup, read_summary};
use qanungo::standup_report::{StandupInstrumentation, StandupReport};
use qanungo::sync::{Artifact, MirroredSession, Skip, SkipReason};

/// The credentials planted in `qanungo-cost.md`. Not one of them may appear in a rendered
/// document while the secrets pass is on — not truncated, not partially, not once.
const PLANTED: [&str; 3] = [
    "ghp_CANARYCANARYCANARYCANARYCANARYCANARY",
    "sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99",
    "AKIACANARY0EXAMPLE99",
];

/// A decision recorded verbatim by two sessions in the same repository. The rollup must show it
/// once.
const SHARED_DECISION: &str = "Scores are recomputed on every run rather than persisted.";

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/standup")
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

    /// Stores a fixture and returns the mirrored session that points at it.
    fn store(&self, name: &str, archived_at: &str) -> MirroredSession {
        let bytes = fixture(name);
        let source_hash = sha256_hex(&bytes);
        self.cache
            .store(&source_hash, &bytes)
            .expect("the cache accepts a blob");
        MirroredSession {
            session_id: "1".repeat(32),
            snapshot_id: "2".repeat(32),
            source_hash,
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            size_bytes: bytes.len() as u64,
            archived_at: Some(at(archived_at)),
            repository: Some("whatever-the-projection-said".to_owned()),
            hostname: None,
            utc_offset: None,
        }
    }

    fn read(&self, name: &str, archived_at: &str) -> ReadSummary {
        let mirrored = self.store(name, archived_at);
        read_summary(&self.cache, &mirrored).expect("the fixture is a readable summary")
    }

    fn gap(&self, name: &str, archived_at: &str) -> GapReason {
        let mirrored = self.store(name, archived_at);
        read_summary(&self.cache, &mirrored).expect_err("the fixture is not narratable")
    }
}

/// The whole window, in the order the archive listed it — which is deliberately *not* the order
/// the document renders, so the fold's own ordering is what the assertions see.
fn window_summaries(archive: &Archive) -> Vec<ReadSummary> {
    vec![
        archive.read("munshi-tombstone.md", "2026-08-21T10:00:30Z"),
        archive.read("qanungo-scoring.md", "2026-08-20T11:30:30Z"),
        archive.read("no-repository.md", "2026-08-19T09:20:30Z"),
        archive.read("qanungo-cost.md", "2026-08-22T18:00:30Z"),
    ]
}

fn fold(archive: &Archive, redactor: &Redactor) -> Standup {
    Standup::fold(&window_summaries(archive), &[], 0, redactor)
}

// ---------------------------------------------------------------------------
// Grouping and ordering
// ---------------------------------------------------------------------------

/// The group key is the repository the *summary itself* names, not the one Patwari projected onto
/// the session row — the fixtures' mirrored sessions all claim a projection nobody should read.
#[test]
fn sessions_group_under_the_repository_their_own_summary_names() {
    let archive = Archive::new();
    let standup = fold(&archive, &Redactor::new());
    let groups: Vec<_> = standup
        .repositories
        .iter()
        .map(|group| (group.repository.as_str(), group.sessions.len()))
        .collect();
    assert_eq!(
        groups,
        [
            ("surdy/qanungo", 2),
            ("surdy/munshi", 1),
            (NO_REPOSITORY, 1)
        ],
        "busiest first, the unattributed bucket last",
    );
    assert_eq!(standup.sessions, 4);
    assert_eq!(standup.repositories_narrated(), 3);
    assert!(standup.bytes_read > 0, "the footer counts what it read");

    let rendered = render(&standup, Redactor::new());
    assert!(!rendered.contains("whatever-the-projection-said"));
}

/// A session captured outside a checkout is a real state with its own bucket, not a session
/// quietly filed under somebody else's repository.
#[test]
fn a_summary_that_names_no_repository_gets_the_labelled_bucket() {
    let archive = Archive::new();
    let standup = fold(&archive, &Redactor::new());
    let unattributed = standup
        .repositories
        .last()
        .expect("the window narrated something");
    assert_eq!(unattributed.repository, NO_REPOSITORY);
    assert_eq!(
        unattributed.sessions[0].title,
        "Work out a shell one-liner outside any checkout",
    );
    assert_eq!(unattributed.sessions[0].branch, None);
}

/// Newest first *inside* a repository, on archive time — the clock the window itself was cut on.
#[test]
fn sessions_read_newest_first_inside_each_repository() {
    let archive = Archive::new();
    let standup = fold(&archive, &Redactor::new());
    let qanungo = &standup.repositories[0];
    let titles: Vec<_> = qanungo
        .sessions
        .iter()
        .map(|session| session.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Price the window at list rates and refuse to price the rest",
            "Ship the scoring lane behind a rule pack stamp",
        ],
    );
    assert_eq!(
        qanungo.sessions[0].archived_at,
        Some(at("2026-08-22T18:00:30Z")),
    );

    // And the rendered document is in the same order, so the heading order is a property of the
    // fold rather than of how the renderer happened to walk it.
    let rendered = render(&standup, Redactor::new());
    let first = rendered
        .find("Price the window")
        .expect("the newer session");
    let second = rendered
        .find("Ship the scoring")
        .expect("the older session");
    assert!(first < second, "{rendered}");
}

// ---------------------------------------------------------------------------
// The rollups
// ---------------------------------------------------------------------------

/// The same decision recorded by two sessions in one repository is one decision. Two summaries in
/// `surdy/qanungo` record `SHARED_DECISION` verbatim.
#[test]
fn the_decisions_rollup_drops_exact_repeats_within_a_repository() {
    let archive = Archive::new();
    let standup = fold(&archive, &Redactor::new());
    let shared: Vec<_> = standup
        .decisions
        .iter()
        .filter(|line| line.text == SHARED_DECISION)
        .collect();
    assert_eq!(shared.len(), 1, "recorded twice, rolled up once");
    assert_eq!(shared[0].repository, "surdy/qanungo");

    // Once in the whole document: a session's own section renders its title, goal, and completed
    // work, and decisions live only in the rollup — which is what makes the rollup worth reading
    // rather than a third repetition of what is already above it.
    let rendered = render(&standup, Redactor::new());
    assert_eq!(rendered.matches(SHARED_DECISION).count(), 1, "{rendered}");
}

/// Attribution survives the roll-up, and the rollup keeps reading order: the repositories come in
/// the document's own order and each session's list keeps the order the summary wrote it in.
#[test]
fn rolled_up_lines_keep_their_repository_and_their_order() {
    let archive = Archive::new();
    let standup = fold(&archive, &Redactor::new());
    let attributed: Vec<_> = standup
        .decisions
        .iter()
        .map(|line| line.repository.as_str())
        .collect();
    assert_eq!(
        attributed,
        [
            "surdy/qanungo",
            "surdy/qanungo",
            "surdy/qanungo",
            "surdy/qanungo",
            "surdy/munshi",
        ],
        "qanungo's four distinct decisions first, then munshi's one",
    );
    // The newer qanungo session's decisions come first because that session comes first, and the
    // shared line is attributed to the first session that recorded it.
    assert_eq!(standup.decisions[0].text, SHARED_DECISION);
    assert!(standup.decisions[3].text.contains("never scored"));
    assert!(standup.decisions[4].text.contains("defense in depth"));

    let open: Vec<_> = standup
        .open_items
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(open.len(), 4);
    assert!(open[0].starts_with("Confirm the price table"));
}

/// An empty rollup is a finding about the week, so the section is rendered saying so rather than
/// disappearing and leaving the reader to guess whether it was empty or forgotten.
#[test]
fn an_empty_rollup_section_still_appears_and_says_it_is_empty() {
    let archive = Archive::new();
    let standup = Standup::fold(
        &[archive.read("no-repository.md", "2026-08-19T09:20:30Z")],
        &[],
        0,
        &Redactor::new(),
    );
    assert!(standup.decisions.is_empty());
    let rendered = render(&standup, Redactor::new());
    assert!(rendered.contains("## Decisions"));
    assert!(rendered.contains("## Open items"));
    assert!(rendered.contains("No session in this window recorded any under this heading."));
}

// ---------------------------------------------------------------------------
// Gaps: one per reason
// ---------------------------------------------------------------------------

/// A placeholder is munshi saying it still owes a summary. Narrating its stand-in text would be
/// this lane inventing work nobody did.
#[test]
fn a_placeholder_summary_is_a_gap_and_never_a_narrative() {
    let archive = Archive::new();
    assert_eq!(
        archive.gap("placeholder.md", "2026-08-23T09:05:30Z"),
        GapReason::Placeholder,
    );
    let standup = Standup::fold(
        &[],
        &[Gap {
            source_agent: "claude-code".to_owned(),
            reason: GapReason::Placeholder,
        }],
        0,
        &Redactor::new(),
    );
    let rendered = render(&standup, Redactor::new());
    assert!(rendered.contains("still owes a real one"), "{rendered}");
    assert!(
        !rendered.contains("summary pending"),
        "the placeholder's own text must not reach the document: {rendered}",
    );
}

#[test]
fn an_unparseable_summary_is_a_gap_that_says_which_way_it_failed() {
    let archive = Archive::new();
    let GapReason::Unparseable(detail) = archive.gap("not-an-archive.md", "2026-08-20T00:00:00Z")
    else {
        panic!("a file with no frontmatter is not a munshi archive record");
    };
    assert!(detail.contains("not a munshi archive record"));
}

/// The mirror's own verdict — no snapshot of this session carried a `summary.md` at all — reaches
/// the same section in this lane's vocabulary.
#[test]
fn a_session_with_no_summary_anywhere_is_a_gap() {
    let skip = Skip {
        source_agent: "claude-code".to_owned(),
        reason: SkipReason::MissingArtifact(Artifact::Summary),
    };
    let standup = Standup::fold(&[], &[Gap::from_skip(&skip)], 0, &Redactor::new());
    let rendered = render(&standup, Redactor::new());
    assert!(rendered.contains("no snapshot of this session carries a `summary.md`"));
}

#[test]
fn an_unreadable_session_and_an_unplaceable_one_get_their_own_lines() {
    let unreadable = Skip {
        source_agent: "claude-code".to_owned(),
        reason: SkipReason::Unreadable("patwari answered 503".to_owned()),
    };
    let standup = Standup::fold(&[], &[Gap::from_skip(&unreadable)], 2, &Redactor::new());
    let rendered = render(&standup, Redactor::new());
    assert!(rendered.contains("claude-code: patwari answered 503"));
    assert!(rendered.contains("2 — archived at a time this build could not place in the window"));
}

/// Every gap is counted and none is silently dropped, however many reasons a window has.
#[test]
fn gaps_group_by_reason_and_count_every_session() {
    let placeholder = || Gap {
        source_agent: "claude-code".to_owned(),
        reason: GapReason::Placeholder,
    };
    let standup = Standup::fold(
        &[],
        &[
            placeholder(),
            placeholder(),
            Gap::from_skip(&Skip {
                source_agent: "copilot-cli".to_owned(),
                reason: SkipReason::MissingArtifact(Artifact::Summary),
            }),
        ],
        0,
        &Redactor::new(),
    );
    assert_eq!(standup.gaps.len(), 2);
    assert_eq!(standup.gaps.iter().map(|note| note.count).sum::<usize>(), 3);
    assert_eq!(standup.sessions, 0);
    let rendered = render(&standup, Redactor::new());
    assert!(rendered.contains("- 2 — claude-code: munshi wrote a placeholder"));
    assert!(rendered.contains("No archived session in this window carried a summary"));
}

// ---------------------------------------------------------------------------
// Redaction — the whole reason this lane waited for qanungo #8
// ---------------------------------------------------------------------------

/// The load-bearing test. A summary carrying three live-shaped credentials is rendered in full,
/// and not one character of any of them survives — while the sentences around them do.
#[test]
fn planted_credentials_never_reach_the_rendered_document() {
    let archive = Archive::new();
    let standup = fold(&archive, &Redactor::new());
    let rendered = render(&standup, Redactor::new());

    for secret in PLANTED {
        assert!(
            !rendered.contains(secret),
            "{secret} survived the scrub: {rendered}",
        );
    }
    for marker in [
        &format!("[REDACTED:{}]", PatternId::GithubToken),
        &format!("[REDACTED:{}]", PatternId::AnthropicKey),
        &format!("[REDACTED:{}]", PatternId::AwsAccessKeyId),
    ] {
        assert!(rendered.contains(marker.as_str()), "{marker}: {rendered}");
    }

    // The prose around each credential is untouched, which is the point of anchoring on structure
    // rather than on entropy: a document pockmarked where the text said something ordinary is a
    // document nobody reads twice.
    assert!(rendered.contains("and then rotated it."));
    assert!(rendered.contains("was revoked before this landed."));
    assert!(rendered.contains("should be rotated too."));

    // The footer accounts for exactly what fired, as counts against pattern ids.
    assert_eq!(standup.redaction.count(PatternId::GithubToken), 1);
    assert_eq!(standup.redaction.count(PatternId::AnthropicKey), 1);
    assert_eq!(standup.redaction.count(PatternId::AwsAccessKeyId), 1);
    assert_eq!(standup.redaction.total(), 3);
    assert!(rendered.contains("3 replacements were made"));
    assert!(rendered.contains(qanungo::redaction::PATTERN_REVISION));
}

/// `--no-redact` is the only way to lose the scrub, and a document that lost it says so in its own
/// footer — which is why the flag is spelled as a negation and there is no `--redact`.
#[test]
fn no_redact_restores_the_text_and_confesses_in_the_footer() {
    let archive = Archive::new();
    let unredacted = Redactor::new().with_secrets(false);
    let standup = fold(&archive, &unredacted);
    let rendered = render(&standup, unredacted);

    for secret in PLANTED {
        assert!(rendered.contains(secret), "{secret} should be back");
    }
    assert!(standup.redaction.is_empty());
    assert!(!rendered.contains("[REDACTED:"));
    assert!(rendered.contains("**not scrubbed for secrets** (`--no-redact`)"));
    assert!(rendered.contains("redaction none"));
}

/// The scrub happens in the fold, so the typed session a renderer receives is already clean. A
/// future surface reading these types inherits the property without asking for it.
#[test]
fn the_folded_session_carries_no_secret_even_before_rendering() {
    let archive = Archive::new();
    let standup = fold(&archive, &Redactor::new());
    let session = &standup.repositories[0].sessions[0];
    let everything = [
        vec![session.title.clone(), session.goal.clone()],
        session.work_completed.clone(),
        session.decisions.clone(),
        session.open_items.clone(),
    ]
    .concat()
    .join("\n");
    for secret in PLANTED {
        assert!(!everything.contains(secret), "{secret} survived the fold");
    }
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// The section skeleton, pinned: a reader and the `/standup` skill both parse this shape.
#[test]
fn the_document_has_the_sections_the_lane_promises() {
    let archive = Archive::new();
    let standup = Standup::fold(
        &window_summaries(&archive),
        &[Gap {
            source_agent: "claude-code".to_owned(),
            reason: GapReason::Placeholder,
        }],
        0,
        &Redactor::new(),
    );
    let rendered = render(&standup, Redactor::new());
    let headings: Vec<_> = rendered
        .lines()
        .filter(|line| line.starts_with('#'))
        .collect();
    assert_eq!(
        headings,
        [
            "# Standup — last 7d",
            "## surdy/qanungo",
            "### Price the window at list rates and refuse to price the rest",
            "### Ship the scoring lane behind a rule pack stamp",
            "## surdy/munshi",
            "### Tombstone the degenerate snapshots the backfill left behind",
            &format!("## {NO_REPOSITORY}"),
            "### Work out a shell one-liner outside any checkout",
            "## Decisions",
            "## Open items",
            "## Gaps",
        ],
    );
}

/// Two runs over the same window produce the same bytes. Nothing here reads a clock but the two
/// timestamps the document prints, and every ordering in the fold is total.
#[test]
fn the_document_is_deterministic() {
    let archive = Archive::new();
    let once = render(&fold(&archive, &Redactor::new()), Redactor::new());
    let twice = render(&fold(&archive, &Redactor::new()), Redactor::new());
    assert_eq!(once, twice);
}

/// The footer carries the instrumentation the house pattern asks of every lane, plus this lane's
/// own two: what the scrub fired, and which pattern set it fired from.
#[test]
fn the_footer_reports_the_run_and_the_scrub() {
    let archive = Archive::new();
    let standup = fold(&archive, &Redactor::new());
    let rendered = render(&standup, Redactor::new());
    let footer = rendered
        .lines()
        .find(|line| line.starts_with("_Instrumentation"))
        .expect("every run stamps a footer");
    for expected in [
        "sync ",
        "fold ",
        "4 sessions",
        " read ",
        "cache 0 hits / 0 misses",
        "redaction ",
        "patterns ",
        "archive http://127.0.0.1:9",
    ] {
        assert!(footer.contains(expected), "{expected}: {footer}");
    }
}

fn window() -> Window {
    let Command::Standup(args) = Cli::parse_from(["qanungo", "standup"]).command else {
        panic!("`standup` parses as the standup command");
    };
    args.last
}

fn render(standup: &Standup, redactor: Redactor) -> String {
    let instrumentation = StandupInstrumentation {
        sync: qanungo::sync::SyncStats::default(),
        fold_elapsed: Duration::from_millis(3),
        redactor,
        patwari_url: "http://127.0.0.1:9".to_owned(),
        cache_root: PathBuf::from("/tmp/qanungo"),
    };
    StandupReport {
        window: &window(),
        generated_at: at("2026-08-24T12:00:00Z"),
        standup,
        instrumentation: &instrumentation,
    }
    .render()
}
