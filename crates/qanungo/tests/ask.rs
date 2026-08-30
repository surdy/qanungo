//! Fixture-backed proof that the ask lane ranks, snippets, and — above all — scrubs, over the real
//! parse-and-fold path rather than a constructed one.
//!
//! Every fixture is written into a real [`BlobCache`] under its own content hash and read back with
//! [`read_summary`], so the cache read, the UTF-8 check, and the `munshi-transcript` parse all run;
//! only the network is absent, which `tests/mirror.rs` covers. The fixtures are the standup lane's,
//! reused unchanged — they are ordinary `summary.md` records, and one of them
//! (`qanungo-cost.md`) carries planted credentials whose whole purpose is to try to reach a
//! rendered surface. A ranked search is a new such surface, so it gets the same canary.

use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use qanungo::ask::{Ask, Escalation, Query};
use qanungo::ask_report::{AskInstrumentation, AskReport, VerbatimStats};
use qanungo::cache::BlobCache;
use qanungo::metrics::source_for_agent;
use qanungo::patwari::sha256_hex;
use qanungo::redaction::Redactor;
use qanungo::standup::{ReadSummary, read_summary};
use qanungo::sync::{MirroredSession, SyncStats};
use qanungo::verbatim::{self, SessionVerbatim};

/// The credentials planted in `qanungo-cost.md`. Not one may appear in a rendered ranking while the
/// secrets pass is on — not truncated, not partially, not once.
const PLANTED: [&str; 3] = [
    "ghp_CANARYCANARYCANARYCANARYCANARYCANARY",
    "sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99",
    "AKIACANARY0EXAMPLE99",
];

fn fixture(name: &str) -> Vec<u8> {
    read_fixture("tests/fixtures/standup", name)
}

/// The transcript fixture the escalation digs into. It is `tests/rules.rs`'s own planted-secret
/// session — a real Claude Code transcript whose error lines carry the very credentials `PLANTED`
/// lists — reused here for the same reason the summary fixtures are: a canary is only a canary if
/// the bytes it runs over are the shape the archive actually holds.
fn transcript_fixture(name: &str) -> Vec<u8> {
    read_fixture("tests/fixtures/rules", name)
}

fn read_fixture(directory: &str, name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(directory)
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
    render_with(ask, query, redactor, None)
}

fn render_with(
    ask: &Ask,
    query: &Query,
    redactor: Redactor,
    verbatim: Option<VerbatimStats>,
) -> String {
    let instrumentation = AskInstrumentation {
        sync: SyncStats::default(),
        fold_elapsed: Duration::from_millis(1),
        verbatim,
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

/// The escalation, over the real cache-read-and-parse path: a query that ranks a summary *and*
/// lands on the transcript's own lines, with one of those lines carrying a planted credential.
///
/// This is the `--verbatim` half of the canary above. The transcript surface is the most exposed
/// one this crate has — it is the session's raw text, not munshi's curated prose — so the property
/// has to hold there too: the line matches, the count is honest, and what reaches the document is
/// the marker rather than the token.
#[test]
fn a_verbatim_match_on_a_credential_line_never_renders_the_credential() {
    let archive = Archive::new();
    // "publish" ranks the cost summary (its open items mention the *published* rate card) and
    // lands on the transcript's opening request; "authentication" lands on the failure line that
    // carries the token. One query, both stages of the funnel — which is how a person would type
    // it.
    let query = Query::parse("publish authentication");
    let mut ask = Ask::fold(&query, &corpus(&archive), &Redactor::new(), 10, 0);
    assert!(!ask.hits.is_empty(), "the summary ranking still stands");

    let found = dig(&archive, &query, &Redactor::new());
    assert_eq!(
        found.total_matches, 2,
        "the request line and the failure line both match: {:?}",
        found.matches,
    );
    assert!(
        !found.redaction.is_empty(),
        "the scrub fired on the failure line"
    );

    // Attached the way `command::fold_ask` attaches it: onto the hit, with the escalation's own
    // replacement counts absorbed into the ranking's, so the one footer covers both.
    ask.redaction.absorb(&found.redaction);
    ask.hits[0].verbatim = Some(Escalation::Searched(found));
    let document = render_with(
        &ask,
        &query,
        Redactor::new(),
        Some(VerbatimStats {
            transcripts_searched: 1,
            matches: 2,
            shown: 2,
            ..VerbatimStats::default()
        }),
    );

    assert!(document.contains("_Verbatim — 2 matching lines in the transcript, showing 2:_"));
    assert!(
        document.contains("[REDACTED:github-token]"),
        "the marker is what a reader sees: {document}",
    );
    for secret in PLANTED {
        assert!(
            !document.contains(secret),
            "the escalation leaked a planted credential: {secret}",
        );
    }
    assert!(document.contains("replacements were made"));
}

/// The flag is real on this surface too: `--no-redact` hands back the transcript's own bytes, and
/// the footer says so. A flag that quietly kept scrubbing one surface would be worse than no flag.
#[test]
fn no_redact_reaches_the_verbatim_excerpts_as_well() {
    let archive = Archive::new();
    let query = Query::parse("publish authentication");
    let bare = Redactor::new().with_secrets(false);
    let mut ask = Ask::fold(&query, &corpus(&archive), &bare, 10, 0);
    let found = dig(&archive, &query, &bare);
    assert!(found.redaction.is_empty(), "nothing was replaced");
    ask.hits[0].verbatim = Some(Escalation::Searched(found));

    let document = render_with(&ask, &query, bare, Some(VerbatimStats::default()));
    assert!(document.contains("**not scrubbed for secrets** (`--no-redact`)"));
    assert!(
        document.contains("ghp_CANARYCANARYCANARYCANARYCANARYCANARY"),
        "with the scrub off the transcript's own line is what is quoted",
    );
}

/// Searches the planted-secret transcript out of the cache, exactly as the escalation does: stored
/// under its content hash, opened as a blob, streamed through the interpreter the manifest names.
fn dig(archive: &Archive, query: &Query, redactor: &Redactor) -> SessionVerbatim {
    let bytes = transcript_fixture("error-with-planted-secret.jsonl");
    let source_hash = sha256_hex(&bytes);
    archive
        .cache
        .store(&source_hash, &bytes)
        .expect("the cache accepts a blob");
    let blob = archive
        .cache
        .open_blob(&source_hash)
        .expect("the blob is readable");
    verbatim::search(
        source_for_agent("claude-code").expect("this build interprets claude-code"),
        2,
        BufReader::new(blob),
        query,
        redactor,
    )
    .expect("v2 is a supported contract")
}
