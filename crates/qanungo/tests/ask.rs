//! Fixture-backed proof that the ask lane ranks, snippets, and — above all — scrubs, over the real
//! parse-and-fold path rather than a constructed one.
//!
//! Every fixture is written into a real [`BlobCache`] under its own content hash and read back with
//! [`read_summary`], so the cache read, the UTF-8 check, and the `munshi-transcript` parse all run;
//! only the network is absent, which `tests/mirror.rs` covers. The fixtures are the standup lane's,
//! reused unchanged — they are ordinary `summary.md` records, and one of them
//! (`qanungo-cost.md`) carries planted credentials whose whole purpose is to try to reach a
//! rendered surface. A ranked search is a new such surface, so it gets the same canary.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use qanungo::ask::{Ask, Query};
use qanungo::ask_report::{AskInstrumentation, AskReport};
use qanungo::cache::BlobCache;
use qanungo::patwari::sha256_hex;
use qanungo::redaction::Redactor;
use qanungo::standup::{ReadSummary, read_summary};
use qanungo::sync::{MirroredSession, SyncStats};

/// The credentials planted in `qanungo-cost.md`. Not one may appear in a rendered ranking while the
/// secrets pass is on — not truncated, not partially, not once.
const PLANTED: [&str; 3] = [
    "ghp_CANARYCANARYCANARYCANARYCANARYCANARY",
    "sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99",
    "AKIACANARY0EXAMPLE99",
];

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

    fn read(&self, name: &str, archived_at: &str) -> ReadSummary {
        let bytes = fixture(name);
        let source_hash = sha256_hex(&bytes);
        self.cache
            .store(&source_hash, &bytes)
            .expect("the cache accepts a blob");
        let mirrored = MirroredSession {
            source_hash,
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            size_bytes: bytes.len() as u64,
            archived_at: Some(at(archived_at)),
            repository: Some("whatever-the-projection-said".to_owned()),
            hostname: None,
            utc_offset: None,
        };
        read_summary(&self.cache, &mirrored).expect("the fixture is a readable summary")
    }
}

/// The window the archive listed, in an order that is deliberately not the ranked order, so the
/// fold's own ranking is what the assertions see.
fn corpus(archive: &Archive) -> Vec<ReadSummary> {
    vec![
        archive.read("munshi-tombstone.md", "2026-08-21T10:00:30Z"),
        archive.read("qanungo-scoring.md", "2026-08-20T11:30:30Z"),
        archive.read("no-repository.md", "2026-08-19T09:20:30Z"),
        archive.read("qanungo-cost.md", "2026-08-22T18:00:30Z"),
    ]
}

fn render(ask: &Ask, query: &Query, redactor: Redactor) -> String {
    let instrumentation = AskInstrumentation {
        sync: SyncStats::default(),
        fold_elapsed: Duration::from_millis(1),
        redactor,
        patwari_url: "https://patwari.example".to_owned(),
        cache_root: PathBuf::from("/cache"),
    };
    AskReport {
        raw_query: "reused in the test",
        query,
        window: None,
        limit: 10,
        generated_at: at("2026-08-27T00:00:00Z"),
        ask,
        instrumentation: &instrumentation,
    }
    .render()
}

/// The most on-topic summary for the query ranks first, over the real parse-and-fold path.
#[test]
fn the_best_match_ranks_first() {
    let archive = Archive::new();
    let query = Query::parse("price the window at list rates");
    let ask = Ask::fold(&query, &corpus(&archive), &Redactor::new(), 10, 0);
    assert!(!ask.hits.is_empty(), "the cost summary should match");
    assert!(
        ask.hits[0].title.contains("Price the window"),
        "the cost summary ranks first, got: {}",
        ask.hits[0].title,
    );
    // The scorecard fixture is off-topic for this query and must not outrank it.
    assert_eq!(ask.searched, 4);
}

/// The canary: a query that lands squarely on a credential-bearing line still cannot put the
/// credential on the screen. The word "pasted" appears only in the work-completed line that also
/// carries a GitHub token, so that line *is* the snippet this hit renders — and the scrub has to
/// have reached it.
#[test]
fn a_query_that_hits_a_credential_line_never_renders_the_credential() {
    let archive = Archive::new();
    let query = Query::parse("pasted");
    let ask = Ask::fold(&query, &corpus(&archive), &Redactor::new(), 10, 0);
    assert!(
        !ask.hits.is_empty(),
        "the credential-bearing summary should match"
    );
    assert!(
        ask.hits[0].snippet.contains("pasted into the run log"),
        "the credential line is the snippet, got: {}",
        ask.hits[0].snippet,
    );

    let document = render(&ask, &query, Redactor::new());
    for secret in PLANTED {
        assert!(
            !document.contains(secret),
            "the ranking leaked a planted credential: {secret}",
        );
    }
    // The token that was on that very line is gone, and the footer says a replacement was made.
    assert!(
        !ask.redaction.is_empty(),
        "the scrub should have caught the planted secret"
    );
    assert!(document.contains("replacements were made"));
}

/// With `--no-redact` the same secret is allowed through — the flag is real — and the footer
/// confesses it, so a reader of the output can see the scrub was off.
#[test]
fn no_redact_lets_the_secret_through_and_says_so() {
    let archive = Archive::new();
    let query = Query::parse("pasted");
    let bare = Redactor::new().with_secrets(false);
    let ask = Ask::fold(&query, &corpus(&archive), &bare, 10, 0);
    let document = render(&ask, &query, bare);
    assert!(document.contains("**not scrubbed for secrets** (`--no-redact`)"));
    // The point of the flag: at least one planted secret now appears, proving the default was doing
    // real work rather than decorating an already-clean corpus.
    assert!(
        PLANTED.iter().any(|secret| document.contains(secret)),
        "with the scrub off a planted secret should be visible",
    );
}

/// A query nothing matches is answered as the archive's own "no", not a truncated list — the
/// distinction a person asking "have I ever done this" depends on.
#[test]
fn a_query_nobody_matches_is_an_honest_no() {
    let archive = Archive::new();
    let query = Query::parse("kubernetes helm chart");
    let ask = Ask::fold(&query, &corpus(&archive), &Redactor::new(), 10, 0);
    assert_eq!(ask.total_matches, 0);
    let document = render(&ask, &query, Redactor::new());
    assert!(document.contains("No session's summary matched"));
    assert!(document.contains("not a truncation"));
    assert!(document.contains("Searched 4"));
}
