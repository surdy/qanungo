//! The dashboard lane, end to end against a stand-in archive.
//!
//! The stand-in speaks the same shapes `tests/mirror.rs` does — the session listing with its
//! `latest_snapshot` projection, a session's own snapshot listing, the snapshot document with its
//! artifact list, and the artifact-content route with its `x-patwari-*` metadata headers — cut down
//! to what this lane needs: one honest transcript per session, `identity`, `Content-Length`. The
//! verified-download machinery is exercised to destruction in `mirror.rs`; what is under test here
//! is the *served surface* over a fold that really happened.
//!
//! Two properties carry the weight. The payload a browser gets must **reconcile with the fold**
//! — the same scores, the same finding counts, the same rule-pack stamp `qanungo report` would
//! print — and it must carry **no verbatim transcript content**, proved against fixtures whose
//! every free-text field holds a canary.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use clap::Parser;
use qanungo::cli::{Cli, Command, DashboardArgs};
use qanungo::command;
use qanungo::dashboard_server::Dashboard;
use qanungo::patwari::sha256_hex;
use qanungo::scoring::{Lane, RulePack, Scorecard};

// ---------------------------------------------------------------------------
// The stand-in archive
// ---------------------------------------------------------------------------

struct ArchivedSession {
    session_id: String,
    snapshot_id: String,
    artifact_id: String,
    source_agent: String,
    transcript: Vec<u8>,
    original_sha256: String,
    completed_at: String,
    /// The `summary.md` this snapshot carries, when it carries one. `None` is a real archive state
    /// — the standup lane names it as a gap — and is what most of the transcript-only fixtures
    /// below stay in, so the coaching and cost sections can be exercised without also inventing a
    /// narrative for every one of them.
    summary: Option<Vec<u8>>,
    summary_artifact_id: String,
    summary_sha256: String,
    /// The repository the archive's own session projection records, which is what the cost lane
    /// cuts by. `None` is a session captured outside a checkout, and gets its own row.
    repository: Option<String>,
}

impl ArchivedSession {
    /// One archived session carrying `transcript`, completed `hours_ago` — relative to now rather
    /// than at a fixed date, so a window measured in days keeps selecting it however long this test
    /// lives.
    fn new(index: u8, source_agent: &str, transcript: &[u8], hours_ago: i64) -> Self {
        let completed = Utc::now() - TimeDelta::hours(hours_ago);
        Self {
            session_id: format!("{index:02x}").repeat(16),
            snapshot_id: format!("{:02x}", index.wrapping_add(100)).repeat(16),
            artifact_id: format!("{:02x}", index.wrapping_add(200)).repeat(16),
            source_agent: source_agent.to_owned(),
            original_sha256: sha256_hex(transcript),
            transcript: transcript.to_vec(),
            completed_at: completed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            summary: None,
            summary_artifact_id: format!("{:02x}", index.wrapping_add(50)).repeat(16),
            summary_sha256: String::new(),
            repository: None,
        }
    }

    /// Attaches a `summary.md` to this session's snapshot, so the standup lane has something to
    /// narrate for it.
    fn with_summary(mut self, summary: &[u8]) -> Self {
        self.summary_sha256 = sha256_hex(summary);
        self.summary = Some(summary.to_vec());
        self
    }

    /// Sets the repository the *listing's* projection reports — the string the cost lane's
    /// by-repository cut is keyed on. Deliberately not the one a `summary.md` names: the standup
    /// lane reads the summary's own, and the two are different facts about a session.
    fn in_repository(mut self, repository: &str) -> Self {
        self.repository = Some(repository.to_owned());
        self
    }

    /// The artifact this id names, with the digest the archive declares for it.
    fn artifact(&self, id: &str) -> Option<(&[u8], &str)> {
        if id == self.artifact_id {
            return Some((&self.transcript, &self.original_sha256));
        }
        match &self.summary {
            Some(summary) if id == self.summary_artifact_id => {
                Some((summary, &self.summary_sha256))
            }
            _ => None,
        }
    }
}

/// Serves `sessions` until the test process exits, and returns its base URL.
fn spawn_archive(sessions: Vec<ArchivedSession>) -> String {
    spawn_counted_archive(sessions).0
}

/// The same, plus a counter of **transcript-content** requests.
///
/// It is what pins the invariant the excerpt route rests on: a dashboard that answered a browser
/// by fetching from the archive would be a remote control for somebody else's bandwidth, so the
/// test asserts the counter does not move rather than asserting the response looked right.
fn spawn_counted_archive(sessions: Vec<ArchivedSession>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sessions = Arc::new(sessions);
    let fetches = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&fetches);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sessions = Arc::clone(&sessions);
            let fetches = Arc::clone(&counted);
            std::thread::spawn(move || serve_archive(stream, &sessions, &fetches));
        }
    });
    (base, fetches)
}

fn serve_archive(mut stream: TcpStream, sessions: &[ArchivedSession], fetches: &AtomicUsize) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let path = target.split_once('?').map_or(target.as_str(), |(p, _)| p);

    let response = if path == "/api/v1/sessions" {
        json_response(&session_page(sessions))
    } else if let Some(id) = path
        .strip_prefix("/api/v1/sessions/")
        .and_then(|rest| rest.strip_suffix("/snapshots"))
    {
        json_response(&session_snapshots(sessions, id))
    } else if let Some(id) = path.strip_prefix("/api/v1/snapshots/") {
        match sessions.iter().find(|session| session.snapshot_id == id) {
            Some(session) => json_response(&snapshot_document(session)),
            None => not_found(),
        }
    } else if let Some(id) = path
        .strip_prefix("/api/v1/artifacts/")
        .and_then(|rest| rest.strip_suffix("/content"))
    {
        fetches.fetch_add(1, Ordering::Relaxed);
        match sessions.iter().find_map(|session| session.artifact(id)) {
            Some((bytes, digest)) => content_response(bytes, digest),
            None => not_found(),
        }
    } else {
        not_found()
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
}

fn session_page(sessions: &[ArchivedSession]) -> String {
    let items: Vec<String> = sessions
        .iter()
        .map(|session| {
            format!(
                r#"{{"session_id":"{}","source_agent":"{}","source_session_id":"harness-{}",
                    "created_at":"{}","updated_at":"{}",
                    "latest_snapshot":{{"snapshot_id":"{}","completed_at":"{}",
                    "project":null,"repository":{},"branch":null,"source_agent_version":null,
                    "artifact_set_version":2,"snapshot_url":"/api/v1/snapshots/{}",
                    "manifest_url":"/api/v1/snapshots/{}/manifest"}},
                    "captures_url":"/api/v1/sessions/{}/captures",
                    "snapshots_url":"/api/v1/sessions/{}/snapshots"}}"#,
                session.session_id,
                session.source_agent,
                session.session_id,
                session.completed_at,
                session.completed_at,
                session.snapshot_id,
                session.completed_at,
                match &session.repository {
                    Some(name) => format!("\"{name}\""),
                    None => "null".to_owned(),
                },
                session.snapshot_id,
                session.snapshot_id,
                session.session_id,
                session.session_id,
            )
        })
        .collect();
    format!(
        r#"{{"items":[{}],"next_cursor":null,"high_watermark":null}}"#,
        items.join(",")
    )
}

fn session_snapshots(sessions: &[ArchivedSession], session_id: &str) -> String {
    let items: Vec<String> = sessions
        .iter()
        .filter(|session| session.session_id == session_id)
        .map(|session| {
            format!(
                r#"{{"snapshot_id":"{}","session_id":"{}","snapshot_fingerprint":"sha256:{}",
                    "manifest_id":"{}","manifest_sha256":"sha256:{}",
                    "completed_at":"{}","artifact_count":1,
                    "total_original_bytes":{},"total_stored_bytes":{},"capture_count":1,
                    "snapshot_url":"/api/v1/snapshots/{}",
                    "captures_url":"/api/v1/snapshots/{}/captures",
                    "manifest_url":"/api/v1/manifests/{}"}}"#,
                session.snapshot_id,
                session.session_id,
                "0".repeat(64),
                session.snapshot_id,
                "1".repeat(64),
                session.completed_at,
                session.transcript.len(),
                session.transcript.len(),
                session.snapshot_id,
                session.snapshot_id,
                session.snapshot_id,
            )
        })
        .collect();
    format!(
        r#"{{"items":[{}],"next_cursor":null,"high_watermark":null}}"#,
        items.join(",")
    )
}

fn snapshot_document(session: &ArchivedSession) -> String {
    format!(
        r#"{{"snapshot_id":"{}","session_id":"{}","snapshot_fingerprint":"sha256:{}",
            "manifest_id":"{}","manifest_sha256":"sha256:{}",
            "completed_at":"{}","artifact_count":1,
            "total_original_bytes":{},"total_stored_bytes":{},"capture_count":1,
            "captures_url":"/api/v1/snapshots/{}/captures","manifest_url":"/api/v1/manifests/{}",
            "manifest":{{"schema_version":1,
                "session":{{"source_agent":"{}","source_session_id":"harness"}},
                "capture":{{"captured_at":"{}","source_cursor":null,
                    "source_state_hash":null,"source_metadata":{{}},"project":null,
                    "repository":null,"branch":null,"source_agent_version":null,
                    "artifact_set_version":2,"munshi_version":null}},
                "artifacts":[]}},
            "artifacts":[{{"artifact_id":"{}","artifact_index":0,
                "logical_path":"transcript.jsonl","media_type":"application/jsonl",
                "original_size_bytes":{},"original_sha256":"sha256:{}",
                "stored_size_bytes":{},"stored_sha256":"sha256:{}","compression":"identity",
                "metadata_url":"/api/v1/artifacts/{}",
                "content_url":"/api/v1/artifacts/{}/content"}}{}]}}"#,
        session.snapshot_id,
        session.session_id,
        "0".repeat(64),
        session.snapshot_id,
        "1".repeat(64),
        session.completed_at,
        session.transcript.len(),
        session.transcript.len(),
        session.snapshot_id,
        session.snapshot_id,
        session.source_agent,
        session.completed_at,
        session.artifact_id,
        session.transcript.len(),
        session.original_sha256,
        session.transcript.len(),
        session.original_sha256,
        session.artifact_id,
        session.artifact_id,
        summary_artifact(session),
    )
}

/// The snapshot's second artifact, when it has one. A snapshot with no `summary.md` is what the
/// standup lane calls a gap, and most of the fixtures here stay in that state deliberately.
fn summary_artifact(session: &ArchivedSession) -> String {
    let Some(summary) = &session.summary else {
        return String::new();
    };
    format!(
        r#",{{"artifact_id":"{}","artifact_index":1,
            "logical_path":"summary.md","media_type":"text/markdown",
            "original_size_bytes":{},"original_sha256":"sha256:{}",
            "stored_size_bytes":{},"stored_sha256":"sha256:{}","compression":"identity",
            "metadata_url":"/api/v1/artifacts/{}",
            "content_url":"/api/v1/artifacts/{}/content"}}"#,
        session.summary_artifact_id,
        summary.len(),
        session.summary_sha256,
        summary.len(),
        session.summary_sha256,
        session.summary_artifact_id,
        session.summary_artifact_id,
    )
}

fn json_response(body: &str) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}

fn content_response(bytes: &[u8], digest: &str) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/octet-stream\r\n\
         Content-Length: {}\r\n\
         x-patwari-compression: identity\r\n\
         x-patwari-original-size-bytes: {}\r\n\
         x-patwari-original-sha256: sha256:{}\r\n\
         x-patwari-stored-size-bytes: {}\r\n\
         x-patwari-stored-sha256: sha256:{}\r\n\r\n",
        bytes.len(),
        bytes.len(),
        digest,
        bytes.len(),
        digest,
    )
    .into_bytes();
    response.extend_from_slice(bytes);
    response
}

fn not_found() -> Vec<u8> {
    let body = r#"{"error":{"code":"not_found","message":"not found"}}"#;
    let mut response = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}

// ---------------------------------------------------------------------------
// Driving the dashboard
// ---------------------------------------------------------------------------

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

/// The dashboard's arguments, pointed at a stand-in archive and a scratch cache, on a port the
/// operating system picks.
fn args(base: &str, cache: &std::path::Path) -> DashboardArgs {
    args_with(base, cache, &[])
}

/// The same, plus whatever flags a test is about — `--no-redact` above all, which is the one flag
/// on this lane that changes what a reader is handed.
fn args_with(base: &str, cache: &std::path::Path, extra: &[&str]) -> DashboardArgs {
    let Command::Dashboard(args) = Cli::parse_from(
        [
            "qanungo",
            "dashboard",
            "--last",
            "30d",
            "--bind",
            "127.0.0.1:0",
            "--patwari-url",
            base,
            "--cache-dir",
            cache.to_str().expect("a utf-8 scratch path"),
        ]
        .into_iter()
        .chain(extra.iter().copied()),
    )
    .command
    else {
        panic!("`dashboard` parses as the dashboard command");
    };
    args
}

/// Starts a dashboard over `sessions`, serving on a background thread, and hands back its address.
///
/// The scratch directory is leaked into the returned tuple rather than dropped, because the served
/// process keeps reading the blob cache underneath it for as long as the test does.
fn spawn_dashboard(sessions: Vec<ArchivedSession>) -> (SocketAddr, tempfile::TempDir) {
    let base = spawn_archive(sessions);
    let directory = tempfile::tempdir().expect("a scratch directory");
    let dashboard =
        Dashboard::start(&args(&base, &directory.path().join("qanungo"))).expect("the first fold");
    let address = dashboard.address();
    // `serve` rather than `run`: a refresh timer would re-sync the archive underneath an assertion,
    // and what is under test here is the served surface rather than the clock.
    std::thread::spawn(move || dashboard.serve());
    (address, directory)
}

/// One request, one response, read whole. The server closes every connection, so `read_to_end` is
/// the framing.
fn request(address: SocketAddr, target: &str) -> (String, String) {
    request_with(address, "GET", target)
}

fn request_with(address: SocketAddr, method: &str, target: &str) -> (String, String) {
    let mut stream = TcpStream::connect(address).expect("the dashboard is listening");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("a readable socket");
    stream
        .write_all(format!("{method} {target} HTTP/1.1\r\nHost: dashboard\r\n\r\n").as_bytes())
        .expect("the request goes out");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("a whole response");
    let response = String::from_utf8(response).expect("the dashboard serves utf-8");
    let (head, body) = response
        .split_once("\r\n\r\n")
        .expect("a head and a body, separated");
    (head.to_owned(), body.to_owned())
}

/// The transcript fixtures, as the archive would hold them.
fn transcript(relative: &str) -> Vec<u8> {
    std::fs::read(fixture(relative)).expect("fixture is readable")
}

/// The standup fixtures, as the archive would hold them. The same files `tests/standup.rs` folds
/// directly, so the grouping this lane produces over the wire can be checked against the grouping
/// that test already pins.
fn summary(name: &str) -> Vec<u8> {
    std::fs::read(fixture(&format!("standup/{name}"))).expect("fixture is readable")
}

/// A window with something to say in all three sections.
///
/// The repository each session is *listed* under is deliberately not the repository its own
/// `summary.md` names: the cost lane cuts by the archive's projection and the standup lane groups by
/// what the summary itself says, and those are two different facts about a session. A fixture where
/// they agreed could not tell a lane reading the wrong one from a lane reading the right one.
///
/// Two of the six put nothing in the narrative — one carries munshi's placeholder, one carries no
/// `summary.md` at all — because a window with no gaps in it cannot show that gaps are counted.
fn three_lane_archive() -> Vec<ArchivedSession> {
    vec![
        ArchivedSession::new(
            11,
            "claude-code",
            &transcript("cost/claude-billing.jsonl"),
            2,
        )
        .with_summary(&summary("qanungo-cost.md"))
        .in_repository("surdy/qanungo"),
        ArchivedSession::new(
            12,
            "claude-code",
            &transcript("rules/marathon-session.jsonl"),
            3,
        )
        .with_summary(&summary("qanungo-scoring.md"))
        .in_repository("surdy/qanungo"),
        ArchivedSession::new(
            13,
            "claude-code",
            &transcript("rules/high-tool-error-rate.jsonl"),
            4,
        )
        .with_summary(&summary("munshi-tombstone.md"))
        .in_repository("surdy/munshi"),
        ArchivedSession::new(14, "claude-code", &transcript("rules/retry-loop.jsonl"), 5)
            .with_summary(&summary("no-repository.md")),
        // munshi still owes a real summary here: a gap, never a narrative.
        ArchivedSession::new(15, "claude-code", &transcript("rules/babysitting.jsonl"), 6)
            .with_summary(&summary("placeholder.md")),
        // No `summary.md` on any snapshot: the other gap, and the window's only copilot session.
        ArchivedSession::new(
            16,
            "copilot-cli",
            &transcript("cost/copilot-billing.jsonl"),
            7,
        )
        .in_repository("surdy/munshi"),
    ]
}

/// A window whose sessions trip several rules and whose every free-text field carries a canary.
fn canary_archive() -> Vec<ArchivedSession> {
    vec![
        ArchivedSession::new(
            1,
            "claude-code",
            &transcript("rules/high-tool-error-rate.jsonl"),
            2,
        ),
        ArchivedSession::new(
            2,
            "claude-code",
            &transcript("rules/marathon-session.jsonl"),
            3,
        ),
        ArchivedSession::new(3, "codex-cli", &transcript("rules/retry-loop.jsonl"), 4),
        // The Code Review lane's shape, so the recursive no-verbatim/no-href walk below runs over
        // a payload in which unreviewed-ship actually fires. It is the one rule whose anchors point
        // at operator-written commit messages, and one of those messages carries a planted
        // credential — so a payload that leaked either the message or the secret fails here rather
        // than only in the excerpt-route test.
        ArchivedSession::new(
            4,
            "claude-code",
            &transcript("rules/unreviewed-ship.jsonl"),
            5,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn the_page_route_serves_the_embedded_page() {
    let (address, _directory) = spawn_dashboard(canary_archive());
    for target in ["/", "/index.html"] {
        let (head, body) = request(address, target);
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{target}: {head}");
        assert!(
            head.contains("Content-Type: text/html; charset=utf-8"),
            "{head}"
        );
        assert!(head.contains("Cache-Control: no-store"), "{head}");
        assert!(head.contains("Connection: close"), "{head}");
        assert!(body.starts_with("<!doctype html>"), "{target}");
        assert!(body.contains("Practice scores"), "{target}");
        // The page is the whole deployment: no asset route exists to load anything from.
        assert!(!body.contains("href"), "the page links to nothing");
    }
}

/// The property the whole lane rests on: what a browser is handed is the coaching report's own
/// numbers. The fold below is a second, independent run of exactly what `qanungo report` does.
#[test]
fn the_data_route_serves_json_that_reconciles_with_the_fold() {
    let base = spawn_archive(canary_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let args = args(&base, &directory.path().join("qanungo"));
    let dashboard = Dashboard::start(&args).expect("the first fold");
    let address = dashboard.address();
    std::thread::spawn(move || dashboard.serve());

    let (head, body) = request(address, "/api/data");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(head.contains("Content-Type: application/json"), "{head}");
    assert!(head.contains("Cache-Control: no-store"), "{head}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("the payload is JSON");

    // The same window, folded again from the same archive and the now-warm cache.
    let folded = command::fold_coaching(&args.archive, &args.last).expect("the window folds");
    let scorecard = Scorecard::fold(&folded.sessions);

    assert_eq!(payload["sessions"]["folded"], folded.sessions.len());
    assert_eq!(
        payload["provenance"]["rule_pack"],
        RulePack::current().stamp(),
    );
    assert_eq!(payload["window"]["last"], "30d");

    for lane in Lane::ALL {
        let served = payload["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == lane.key())
            .unwrap_or_else(|| panic!("{lane:?} is on the page"));
        match scorecard.fleet(lane) {
            Some(blend) => {
                assert_eq!(served["fleet"]["state"], "scored", "{lane:?}");
                assert_eq!(served["fleet"]["score"], blend.score, "{lane:?}");
            }
            None => assert_ne!(served["fleet"]["state"], "scored", "{lane:?}"),
        }
        for harness in &scorecard.harnesses {
            let column = served["harnesses"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["source_agent"] == harness.source_agent)
                .unwrap_or_else(|| panic!("{} has a column", harness.source_agent));
            assert_eq!(column["sessions"], harness.sessions);
            match harness.lane(lane).score() {
                Some(score) => assert_eq!(column["score"], score, "{lane:?}"),
                None => assert_eq!(column["score"], serde_json::Value::Null, "{lane:?}"),
            }
        }
    }

    let served_findings = payload["findings"].as_array().unwrap();
    assert_eq!(served_findings.len(), folded.findings.len());
    assert!(
        !served_findings.is_empty(),
        "the fixture window must trip rules, or this reconciles nothing",
    );
    for (served, finding) in served_findings.iter().zip(&folded.findings) {
        assert_eq!(served["rule"], finding.rule.key());
        assert_eq!(served["title"], finding.rule.title());
        assert_eq!(served["problem"], finding.problem);
        assert_eq!(served["action"], finding.action);
        assert_eq!(served["sessions_affected"], finding.evidence.len());
        let hashes: Vec<&str> = served["source_hashes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hash| hash.as_str().unwrap())
            .collect();
        let expected: Vec<&str> = finding
            .evidence
            .iter()
            .map(|evidence| evidence.source_hash.as_str())
            .collect();
        assert_eq!(hashes, expected);
    }
}

/// The redaction line, on the surface that publishes to a browser — and it is **not one line any
/// more**, because the page is not one lane. The test says which claim belongs to which section
/// rather than making a single sentence that is now false of a third of the document:
///
/// - The **coaching** and **cost** sections carry no verbatim transcript content at all. That is
///   what this test proves, against fixtures whose every free-text field holds a canary.
/// - **Anchors are not content**: the coaching section names tool names, locators, record numbers,
///   and timestamps, which are schema metadata and positions. What a human typed or a tool printed
///   reaches a reader only through the excerpt route, scrubbed, one counted event at a time.
/// - The **standup** section carries verbatim prose, *scrubbed by the fold*. It is empty in this
///   window — none of these snapshots carries a `summary.md` — and the two tests below pin its own
///   claim: its strings are the fold's own strings, and a planted credential never reaches the wire.
///
/// A test that kept asserting "no verbatim anywhere" over a payload that now serves prose would be
/// passing on a technicality of which fixture it chose, which is worse than not having it.
#[test]
fn the_coaching_and_cost_sections_contain_no_verbatim_transcript_content() {
    for relative in ["rules/high-tool-error-rate.jsonl", "rules/retry-loop.jsonl"] {
        let raw = std::fs::read_to_string(fixture(relative)).unwrap();
        assert!(raw.contains("CANARY_"), "{relative} must carry canaries");
    }

    let (address, _directory) = spawn_dashboard(canary_archive());
    let (_head, body) = request(address, "/api/data");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("the payload is JSON");
    assert!(
        !payload["findings"].as_array().unwrap().is_empty(),
        "the payload under test must have findings",
    );
    // The surface renders verbatim — through the excerpt route, and through the standup section
    // when there is one — and says so.
    assert_eq!(payload["provenance"]["renders_verbatim"], true);
    assert_eq!(payload["provenance"]["redaction"]["secrets"], true);

    // No snapshot here carries a summary, so the section that *would* hold prose holds none. The
    // whole-body assertion below is therefore a statement about the other two sections, and the
    // fixture is what makes that true rather than the assertion's wording.
    assert_eq!(payload["standup"]["sessions"], 0);
    assert!(
        payload["standup"]["repositories"]
            .as_array()
            .unwrap()
            .is_empty(),
    );

    // The cost section folded these same canary-stuffed transcripts and carries nothing from them:
    // its fold reads `assistant_meta` and never touches a record's classification at all.
    assert!(payload["cost"]["records_read"].as_u64().unwrap() > 0);
    let cost = serde_json::to_string(&payload["cost"]).unwrap();
    assert!(
        !cost.contains("CANARY"),
        "a canary reached the cost section"
    );

    assert!(!body.contains("CANARY"), "a canary token reached the wire");
    for forbidden in [
        "CANARY_USER_REQUEST_ONE",
        "CANARY_ASSISTANT_MESSAGE",
        "CANARY_COMMAND_0",
        "CANARY_ERROR_TEXT_0",
        "CANARY_RETRY_COMMAND",
        "CANARY_ONE_OFF_A",
        "rm -rf",
        "cargo test",
        "git status",
        "/work/fixture",
        "gitBranch",
    ] {
        assert!(!body.contains(forbidden), "`{forbidden}` reached the wire");
    }

    // What it does carry: counts, hashes, and the positions of the events a rule counted.
    let hashes: Vec<&str> = payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|finding| finding["source_hashes"].as_array().unwrap())
        .map(|hash| hash.as_str().unwrap())
        .collect();
    assert!(!hashes.is_empty());
    for hash in hashes {
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|character| character.is_ascii_hexdigit()));
    }
    // Tool names are the one verbatim string every surface in this crate may render (decision 9),
    // and an anchor without one would be an excerpt request nobody could read before making.
    let anchors = all_anchors(&payload);
    assert!(!anchors.is_empty(), "the fixture window anchors events");
    assert!(
        anchors.iter().any(|anchor| anchor["tool"] == "Bash"),
        "an anchor names the tool it counted: {anchors:?}",
    );
    for anchor in &anchors {
        assert!(anchor["locator"].as_u64().unwrap() >= 1);
        assert!(anchor["record"].as_u64().unwrap() >= 1);
        assert!(anchor["at"].as_str().unwrap().ends_with('Z'));
    }

    // And no route back into the archive, whose blobs are served unredacted.
    assert!(!body.contains("/content"), "{body}");
    assert!(!body.contains("/api/v1/artifacts"), "{body}");
}

/// Every anchor the payload names, across every finding.
fn all_anchors(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    payload["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .flat_map(|finding| finding["evidence"].as_array().unwrap())
        .flat_map(|evidence| evidence["anchors"].as_array().unwrap())
        .cloned()
        .collect()
}

/// The event stream announces the payload a page has just been handed, so a reconnecting page and
/// a refreshing one take the same path.
#[test]
fn the_event_stream_announces_the_current_refresh() {
    let (address, _directory) = spawn_dashboard(canary_archive());

    let mut stream = TcpStream::connect(address).expect("the dashboard is listening");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("a readable socket");
    stream
        .write_all(b"GET /api/events HTTP/1.1\r\nHost: dashboard\r\n\r\n")
        .expect("the request goes out");

    // Read only as far as the first event's data line: the stream never ends on its own, which is
    // the point of it.
    let mut reader = BufReader::new(&stream);
    let mut seen = String::new();
    let data = loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("the stream is readable");
        assert_ne!(read, 0, "the stream closed before its first event: {seen}");
        seen.push_str(&line);
        if let Some(data) = line.strip_prefix("data: ") {
            break data.trim_end().to_owned();
        }
    };

    assert!(seen.starts_with("HTTP/1.1 200 OK\r\n"), "{seen}");
    assert!(
        seen.contains("Content-Type: text/event-stream\r\n"),
        "{seen}"
    );
    assert!(seen.contains("Cache-Control: no-store\r\n"), "{seen}");
    assert!(
        !seen.contains("Content-Length"),
        "a stream that declared a length would end: {seen}",
    );
    assert!(seen.contains("retry: 3000\n\n"), "{seen}");
    assert!(seen.contains("event: refresh\n"), "{seen}");

    let notice: serde_json::Value = serde_json::from_str(&data).expect("the notice is JSON");
    assert_eq!(notice["generation"], 1, "the first fold is generation one");
    assert!(
        notice["refreshed_at"].as_str().unwrap().ends_with('Z'),
        "the notice dates itself in UTC: {notice}",
    );

    // The generation on the stream is the one the payload states, which is what lets a page tell a
    // refresh from a reconnection without re-fetching to find out.
    let (_head, body) = request(address, "/api/data");
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["provenance"]["generation"], notice["generation"]);
    assert_eq!(
        payload["provenance"]["refreshed_at"],
        notice["refreshed_at"]
    );
}

/// Four routes, read-only, and nothing else — including nothing that looks like a path into the
/// archive or the filesystem.
#[test]
fn nothing_but_the_four_routes_answers() {
    let (address, _directory) = spawn_dashboard(canary_archive());
    for target in [
        "/api",
        "/api/data/",
        "/favicon.ico",
        "/../../etc/passwd",
        "/api/v1/sessions",
    ] {
        let (head, _body) = request(address, target);
        assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{target}");
    }
    for method in ["POST", "PUT", "DELETE"] {
        let (head, _body) = request_with(address, method, "/api/data");
        assert!(
            head.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "{method} is not a verb this surface has",
        );
    }
    // A query string changes nothing: there is no per-request knob on this surface at all. That
    // matters most on the excerpt route, where the knob a caller might reach for is the redactor.
    let (head, body) = request(address, "/api/data?redact=off");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(!body.contains("CANARY"), "a query string is not a switch");

    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    let (source_hash, locator) = first_anchor(&payload);
    let (head, body) = request(
        address,
        &format!("/api/evidence/{source_hash}/{locator}?redact=off&no-redact=1"),
    );
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    let excerpt: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        excerpt["redaction"]["secrets"], true,
        "the scrub belongs to the process, not to the request",
    );
}

/// A malformed request line is answered and the connection closed, rather than being parsed
/// hopefully or left hanging.
#[test]
fn a_malformed_request_is_refused() {
    let (address, _directory) = spawn_dashboard(canary_archive());
    let mut stream = TcpStream::connect(address).expect("the dashboard is listening");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("a readable socket");
    stream.write_all(b"nonsense\r\n\r\n").expect("bytes go out");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("a whole response");
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request\r\n"),
        "{response}"
    );
}

/// An empty window is a served page too: the lanes keep their places, nothing is scored, and the
/// provenance block still says what was looked at.
#[test]
fn an_empty_archive_still_serves_a_page_and_a_payload() {
    let (address, _directory) = spawn_dashboard(Vec::new());
    let (head, body) = request(address, "/api/data");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("the payload is JSON");
    assert_eq!(payload["sessions"]["folded"], 0);
    assert_eq!(payload["lanes"].as_array().unwrap().len(), 5);
    assert!(payload["findings"].as_array().unwrap().is_empty());
    for lane in payload["lanes"].as_array().unwrap() {
        assert_ne!(
            lane["fleet"]["state"], "scored",
            "an empty window scores nothing: {lane}",
        );
    }
    assert_eq!(payload["provenance"]["sessions_listed"], 0);
    assert_eq!(payload["provenance"]["renders_verbatim"], true);
    // Nothing to expand, and the route says so rather than inventing an answer: an empty window
    // names no anchor, so every locator is unanchored.
    let (head, body) = request(address, &format!("/api/evidence/{}/1", "a".repeat(64)));
    assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{head}");
    let refusal: serde_json::Value = serde_json::from_str(&body).expect("a JSON refusal");
    assert_eq!(refusal["reason"], "not-anchored");
}

// ---------------------------------------------------------------------------
// The evidence route
// ---------------------------------------------------------------------------

/// The window the excerpt tests run against: one session whose failing tool results carry planted,
/// live-*shaped* credentials. Nothing here has ever been a real secret — each is a shape with
/// `CANARY` spelled through its body, exactly as the standup lane's fixture does it.
fn planted_secret_archive() -> Vec<ArchivedSession> {
    vec![ArchivedSession::new(
        7,
        "claude-code",
        &transcript("rules/error-with-planted-secret.jsonl"),
        2,
    )]
}

/// The first anchor of the first finding that has one, with its session's hash.
fn first_anchor(payload: &serde_json::Value) -> (String, u64) {
    for finding in payload["findings"].as_array().expect("findings") {
        for evidence in finding["evidence"].as_array().expect("evidence") {
            if let Some(anchor) = evidence["anchors"].as_array().expect("anchors").first() {
                return (
                    evidence["source_hash"].as_str().unwrap().to_owned(),
                    anchor["locator"].as_u64().unwrap(),
                );
            }
        }
    }
    panic!("the fixture window must anchor at least one event");
}

fn payload_of(address: SocketAddr) -> serde_json::Value {
    let (_head, body) = request(address, "/api/data");
    serde_json::from_str(&body).expect("the payload is JSON")
}

/// The whole point of the slice: an anchor the page named resolves to the one event the rule
/// counted — its tool, its outcome, its own text — and to nothing around it.
#[test]
fn an_anchor_resolves_to_the_counted_event_and_nothing_around_it() {
    let (address, _directory) = spawn_dashboard(canary_archive());
    let payload = payload_of(address);

    let errors = payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["rule"] == "high-tool-error-rate")
        .expect("the error-rate rule fires on this window");
    assert_eq!(errors["evidence_kind"], "event");
    let evidence = &errors["evidence"][0];
    let anchors = evidence["anchors"].as_array().unwrap();
    // Six failures in the fixture, under the cap, so all six are offered — and in file order.
    assert_eq!(anchors.len(), 6);
    let locators: Vec<u64> = anchors
        .iter()
        .map(|anchor| anchor["locator"].as_u64().unwrap())
        .collect();
    let mut ascending = locators.clone();
    ascending.sort_unstable();
    assert_eq!(locators, ascending, "anchors are offered in file order");
    assert_eq!(
        evidence["structural"],
        serde_json::Value::Null,
        "an event-shaped rule offers events, not a shape",
    );

    let source_hash = evidence["source_hash"].as_str().unwrap();
    let (head, body) = request(
        address,
        &format!("/api/evidence/{source_hash}/{}", locators[0]),
    );
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(head.contains("Content-Type: application/json"), "{head}");
    assert!(head.contains("Cache-Control: no-store"), "{head}");
    let excerpt: serde_json::Value = serde_json::from_str(&body).expect("the excerpt is JSON");

    assert_eq!(excerpt["source_hash"], source_hash);
    assert_eq!(excerpt["locator"], locators[0]);
    assert_eq!(excerpt["tool"], "Bash");
    assert_eq!(excerpt["event"], "tool_result");
    assert_eq!(excerpt["outcome"], false, "the counted event is a failure");
    assert!(excerpt["at"].as_str().unwrap().ends_with('Z'));
    // Claude Code puts the command on the invocation and the error on the result, so the excerpt
    // of a counted *error* carries the error and no command. Pairing the two means reading a
    // second event, which is surrounding context — deliberately out of this slice.
    assert_eq!(excerpt["command"], serde_json::Value::Null);
    assert!(
        excerpt["output"]
            .as_str()
            .expect("the failing result's own text")
            .contains("CANARY_ERROR_TEXT_0"),
        "{excerpt}",
    );
    // Nothing from any neighbouring event, and nothing from the raw tool payload.
    for forbidden in [
        "CANARY_USER_REQUEST_ONE",
        "CANARY_ASSISTANT_MESSAGE",
        "CANARY_COMMAND_0",
        "CANARY_OUTPUT_1",
        "CANARY_ERROR_TEXT_2",
        "/work/fixture",
        "gitBranch",
    ] {
        assert!(
            !body.contains(forbidden),
            "`{forbidden}` reached the excerpt"
        );
    }
    assert_eq!(excerpt["redaction"]["secrets"], true);
    assert_eq!(excerpt["truncated"], false);
}

/// The done-bar's canary, at HTTP level: a planted credential in a counted event must come back
/// as a marker, and the rest of the sentence around it must survive untouched.
#[test]
fn a_planted_secret_comes_back_redacted_through_the_route() {
    let (address, _directory) = spawn_dashboard(planted_secret_archive());
    let payload = payload_of(address);
    let (source_hash, _) = first_anchor(&payload);

    let mut redacted = 0;
    let mut seen = 0;
    for locator in anchored_locators(&payload, &source_hash) {
        let (head, body) = request(address, &format!("/api/evidence/{source_hash}/{locator}"));
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        seen += 1;
        let excerpt: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            !body.contains("ghp_CANARY") && !body.contains("sk-ant-api03-CANARY"),
            "a planted credential reached the wire: {body}",
        );
        redacted += excerpt["redaction"]["total"].as_u64().unwrap();
        if let Some(output) = excerpt["output"].as_str() {
            if output.contains("[REDACTED:") {
                // The scrub replaces the credential and leaves the sentence around it alone —
                // an excerpt pockmarked past legibility would be a redactor nobody keeps on.
                assert!(output.contains("CANARY_ERROR_TEXT_"), "{output}");
            }
        }
    }
    assert!(seen >= 2, "the fixture anchors its failures");
    assert_eq!(
        redacted, 2,
        "both planted credentials fired, and nothing else did",
    );
}

/// The same window with `--no-redact`: the flag has to actually mean raw, or it is a switch that
/// lies. This is the negative half of the canary — a redactor that scrubbed anyway would pass the
/// test above and fail here.
#[test]
fn no_redact_serves_the_event_as_the_transcript_holds_it() {
    let base = spawn_archive(planted_secret_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let args = args_with(&base, &directory.path().join("qanungo"), &["--no-redact"]);
    let dashboard = Dashboard::start(&args).expect("the first fold");
    let address = dashboard.address();
    std::thread::spawn(move || dashboard.serve());

    let payload = payload_of(address);
    assert_eq!(payload["provenance"]["redaction"]["secrets"], false);
    let (source_hash, _) = first_anchor(&payload);

    let mut raw_credentials = 0;
    for locator in anchored_locators(&payload, &source_hash) {
        let (_head, body) = request(address, &format!("/api/evidence/{source_hash}/{locator}"));
        let excerpt: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(excerpt["redaction"]["total"], 0, "nothing was scrubbed");
        if body.contains("ghp_CANARYCANARYCANARYCANARYCANARYCANARY")
            || body.contains("sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99")
        {
            raw_credentials += 1;
        }
        assert!(!body.contains("[REDACTED:"), "{body}");
    }
    assert_eq!(
        raw_credentials, 2,
        "--no-redact means raw, or it means nothing",
    );
}

/// Every locator the payload offers for one session.
fn anchored_locators(payload: &serde_json::Value, source_hash: &str) -> Vec<u64> {
    payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|finding| finding["evidence"].as_array().unwrap())
        .filter(|evidence| evidence["source_hash"] == source_hash)
        .flat_map(|evidence| evidence["anchors"].as_array().unwrap())
        .map(|anchor| anchor["locator"].as_u64().unwrap())
        .collect()
}

/// The route's grammar, and the boundary behind it. A target that is not *exactly* a 64-character
/// lowercase hash and a bare bounded positive integer is not a repairable request — it is not this
/// route — and a well-formed one the payload never named is a 404 too, which is the difference
/// between an evidence route and a transcript-browsing API.
#[test]
fn the_evidence_route_refuses_everything_the_payload_did_not_name() {
    let (address, _directory) = spawn_dashboard(canary_archive());
    let payload = payload_of(address);
    let (source_hash, locator) = first_anchor(&payload);

    let malformed = [
        format!("/api/evidence/{source_hash}"),
        format!("/api/evidence/{source_hash}/"),
        format!("/api/evidence/{source_hash}/{locator}/"),
        format!("/api/evidence/{source_hash}/{locator}/1"),
        format!("/api/evidence/{}/1", source_hash.to_uppercase()),
        format!("/api/evidence/{}/1", &source_hash[..63]),
        format!("/api/evidence/{source_hash}z/1"),
        format!("/api/evidence/{}/1", "g".repeat(64)),
        format!("/api/evidence/{source_hash}/0"),
        format!("/api/evidence/{source_hash}/01"),
        format!("/api/evidence/{source_hash}/-1"),
        format!("/api/evidence/{source_hash}/1.0"),
        format!("/api/evidence/{source_hash}/1e3"),
        format!("/api/evidence/{source_hash}/99999999999999999999"),
        format!("/api/evidence/{source_hash}/{}", "9".repeat(64)),
        "/api/evidence/".to_owned(),
        "/api/evidence".to_owned(),
    ];
    for target in &malformed {
        let (head, body) = request(address, target);
        assert!(
            head.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "{target} answered {head}",
        );
        assert!(
            !body.contains("locator"),
            "a malformed target is not a route, so it never became a lookup: {target}",
        );
    }

    // Well-formed, and refused for the reason that matters: nothing on the page named it.
    for target in [
        // A hash the payload does not carry at all.
        format!("/api/evidence/{}/{locator}", "a".repeat(64)),
        // The right session, a locator no finding offered. The transcript has an event there —
        // it is simply not one this page cited.
        format!("/api/evidence/{source_hash}/1"),
        format!("/api/evidence/{source_hash}/999999999"),
    ] {
        let (head, body) = request(address, &target);
        assert!(
            head.starts_with("HTTP/1.1 404 Not Found\r\n"),
            "{target} answered {head}",
        );
        let refusal: serde_json::Value = serde_json::from_str(&body).expect("a JSON refusal");
        assert_eq!(refusal["reason"], "not-anchored", "{target}");
        assert!(
            refusal["detail"]
                .as_str()
                .unwrap()
                .contains("not a way to read a transcript"),
        );
    }

    // A non-GET verb is not a method mismatch worth negotiating here either.
    for method in ["POST", "PUT", "DELETE", "HEAD"] {
        let (head, _body) = request_with(
            address,
            method,
            &format!("/api/evidence/{source_hash}/{locator}"),
        );
        assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{method}");
    }

    // And the anchored one still answers, so the refusals above are the route's judgement rather
    // than a route that never worked.
    let (head, _body) = request(address, &format!("/api/evidence/{source_hash}/{locator}"));
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
}

/// The invariant the whole route is built around: a cache miss is answered, never filled. A page
/// on an unauthenticated tailnet must not be able to make this process talk to the archive.
#[test]
fn a_cache_miss_is_a_404_with_provenance_and_never_a_fetch() {
    let (base, fetches) = spawn_counted_archive(canary_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let cache_root = directory.path().join("qanungo");
    let args = args(&base, &cache_root);
    let dashboard = Dashboard::start(&args).expect("the first fold");
    let address = dashboard.address();
    std::thread::spawn(move || dashboard.serve());

    let payload = payload_of(address);
    let (source_hash, locator) = first_anchor(&payload);
    let mirrored = fetches.load(Ordering::Relaxed);
    assert!(mirrored > 0, "the fold mirrored the window");

    // Take the blob out from under the served payload, which still names its anchors.
    let blob = cache_root
        .join("blobs")
        .join(&source_hash[..2])
        .join(&source_hash);
    std::fs::remove_file(&blob).expect("the blob was cached");

    let (head, body) = request(address, &format!("/api/evidence/{source_hash}/{locator}"));
    assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{head}");
    let refusal: serde_json::Value = serde_json::from_str(&body).expect("a JSON refusal");
    assert_eq!(refusal["reason"], "cache-miss");
    assert_eq!(refusal["source_hash"], source_hash);
    assert_eq!(refusal["locator"], locator);
    assert!(
        refusal["detail"]
            .as_str()
            .unwrap()
            .contains("never fetches from the archive to answer a request"),
    );
    assert_eq!(
        fetches.load(Ordering::Relaxed),
        mirrored,
        "the request must not have reached for the archive",
    );
}

/// A session-shaped rule shows what it measured. No excerpt, because it counted no event — and the
/// structural block it renders instead is timestamps and numbers with no string in it at all.
#[test]
fn a_session_shaped_finding_renders_structure_and_offers_no_anchor() {
    let (address, _directory) = spawn_dashboard(canary_archive());
    let payload = payload_of(address);

    let marathon = payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["rule"] == "marathon-session")
        .expect("the marathon rule fires on this window");
    assert_eq!(marathon["evidence_kind"], "structural");

    let evidence = &marathon["evidence"][0];
    assert!(
        evidence["anchors"].as_array().unwrap().is_empty(),
        "a duration has no event in it to anchor",
    );
    let structural = &evidence["structural"];
    assert_eq!(structural["sittings"], 1);
    assert_eq!(structural["active"]["rendered"], "2h 10m");
    assert_eq!(structural["longest_sitting"]["rendered"], "2h 10m");
    assert!(structural["active"]["seconds"].as_u64().unwrap() > 0);
    assert!(structural["user_requests"].as_u64().unwrap() > 0);
    assert_eq!(structural["boundaries_elided"], 0);

    let boundaries = structural["boundaries"].as_array().unwrap();
    assert_eq!(boundaries.len(), 1, "one unbroken sitting");
    assert!(boundaries[0]["from"].as_str().unwrap().ends_with('Z'));
    assert!(boundaries[0]["to"].as_str().unwrap().ends_with('Z'));

    // Every leaf of the structural block, at every depth, is a number or one of exactly two string
    // shapes. Walking only the top level would have skipped the nested duration objects and the
    // per-sitting renderings, which is where a smuggled string would actually fit.
    let serialized = serde_json::to_string(structural).unwrap();
    assert!(!serialized.contains("CANARY"), "{serialized}");
    let mut leaves = 0;
    assert_structural_leaves(structural, "structural", &mut leaves);
    // Six leaves in the three duration objects, six flat counts and timestamps, four in the one
    // sitting, and the elision count: a walk that stopped at the top level would see five.
    assert_eq!(leaves, 17, "the walk must reach the nested objects");

    // Asking for an excerpt anyway is refused like any other unanchored locator.
    let source_hash = evidence["source_hash"].as_str().unwrap();
    let (head, _body) = request(address, &format!("/api/evidence/{source_hash}/1"));
    assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{head}");
}

/// Asserts that every leaf under a structural evidence block is a number, a null, or one of the two
/// string shapes this surface is allowed to render: an RFC 3339 UTC timestamp, or a `format::span`
/// duration. Recurses through objects and arrays, because the interesting places for a transcript
/// string to hide — `active`/`span`/`longest_sitting` and each `boundaries[]` entry — are nested.
///
/// The duration grammar is pinned rather than waved at: `format::span` emits `<h>h <mm>m`, `<m>m`,
/// or `<s>s` and nothing else, so anything with a letter outside `hms` in it is not a duration this
/// crate rendered.
fn assert_structural_leaves(value: &serde_json::Value, path: &str, leaves: &mut usize) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, nested) in fields {
                assert_structural_leaves(nested, &format!("{path}.{key}"), leaves);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, nested) in items.iter().enumerate() {
                assert_structural_leaves(nested, &format!("{path}[{index}]"), leaves);
            }
        }
        serde_json::Value::String(text) => {
            *leaves += 1;
            assert!(
                is_utc_timestamp(text) || is_rendered_span(text),
                "{path} is a string that is neither a timestamp nor a duration: {text:?}",
            );
        }
        serde_json::Value::Number(_) | serde_json::Value::Null => *leaves += 1,
        serde_json::Value::Bool(_) => {
            panic!("{path} is a flag, and this block states measurements")
        }
    }
}

/// `2026-08-10T09:00:00Z` — the one timestamp shape `report::stamp` writes.
fn is_utc_timestamp(text: &str) -> bool {
    text.len() == 20
        && text.ends_with('Z')
        && text
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                4 | 7 => character == '-',
                10 => character == 'T',
                13 | 16 => character == ':',
                19 => character == 'Z',
                _ => character.is_ascii_digit(),
            })
}

/// `2h 10m`, `47m`, `38s` — the whole of `format::span`'s output.
fn is_rendered_span(text: &str) -> bool {
    let shaped = |unit: char, text: &str| {
        text.strip_suffix(unit).is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    };
    match text.split_once(' ') {
        Some((hours, minutes)) => shaped('h', hours) && minutes.len() == 3 && shaped('m', minutes),
        None => shaped('m', text) || shaped('s', text),
    }
}

/// A harness writes its own tool names, and this surface renders them next to a control that
/// expands into transcript text. Decision 9 blessed tool names as schema metadata for the aggregate
/// lines; on the two verbatim paths — the anchor in the payload and the excerpt behind it — they are
/// clamped **and** scrubbed, so a name shaped like a credential is a marker on both.
#[test]
fn a_tool_name_shaped_like_a_credential_is_scrubbed_on_both_paths() {
    let raw = std::fs::read_to_string(fixture("rules/tool-name-canary.jsonl")).unwrap();
    let token = format!("ghp_{}", "CANARY".repeat(6));
    assert!(
        raw.contains(&token),
        "the fixture names its tool after a token"
    );

    let (address, _directory) = spawn_dashboard(vec![ArchivedSession::new(
        9,
        "claude-code",
        &transcript("rules/tool-name-canary.jsonl"),
        2,
    )]);

    // The payload path.
    let (_head, body) = request(address, "/api/data");
    assert!(
        !body.contains(&token),
        "a token-shaped tool name reached the payload"
    );
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    let anchors = all_anchors(&payload);
    assert!(!anchors.is_empty(), "the fixture fires the error rule");
    for anchor in &anchors {
        assert_eq!(anchor["tool"], "[REDACTED:github-token]", "{anchor}");
    }

    // The excerpt path, which is where a reader actually reads it.
    let (source_hash, locator) = first_anchor(&payload);
    let (head, body) = request(address, &format!("/api/evidence/{source_hash}/{locator}"));
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(
        !body.contains(&token),
        "a token-shaped tool name reached the excerpt"
    );
    let excerpt: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(excerpt["tool"], "[REDACTED:github-token]");
    assert_eq!(
        excerpt["event"], "tool_result",
        "an ordinary name is untouched"
    );
    assert!(
        excerpt["redaction"]["total"].as_u64().unwrap() >= 1,
        "the scrub is counted, so the marker explains itself: {excerpt}",
    );
}

/// The grammar the structural walk enforces, stated against values that must fail it — otherwise
/// the walk is a loop that visits leaves and asserts nothing.
#[test]
fn the_structural_grammar_admits_timestamps_and_durations_and_nothing_else() {
    for timestamp in ["2026-08-10T09:00:00Z", "2026-01-01T00:00:00Z"] {
        assert!(is_utc_timestamp(timestamp), "{timestamp}");
    }
    for span in ["2h 10m", "0h 00m", "47m", "38s", "365h 32m"] {
        assert!(is_rendered_span(span), "{span}");
    }
    for hostile in [
        "CANARY_COMMAND_0 rm -rf /tmp",
        "rm -rf",
        "/work/fixture",
        "2026-08-10T09:00:00Z extra",
        "2026-08-10 09:00:00",
        "",
        "h",
        "m",
        "2h",
        "2h 1m",
        "2 h 10 m",
        "12 sessions",
        "1d",
        "-5m",
    ] {
        assert!(
            !is_utc_timestamp(hostile) && !is_rendered_span(hostile),
            "{hostile:?} must not pass for a timestamp or a duration",
        );
    }
}

// ---------------------------------------------------------------------------
// The standup and cost sections
// ---------------------------------------------------------------------------

/// Starts a dashboard over `sessions` with whatever flags a test needs, and hands back the address
/// alongside the arguments — so a test can fold the *same* windows again independently and check
/// the served numbers against them.
fn spawn_with(
    sessions: Vec<ArchivedSession>,
    extra: &[&str],
) -> (SocketAddr, DashboardArgs, tempfile::TempDir) {
    let base = spawn_archive(sessions);
    let directory = tempfile::tempdir().expect("a scratch directory");
    let args = args_with(&base, &directory.path().join("qanungo"), extra);
    let dashboard = Dashboard::start(&args).expect("the first fold");
    let address = dashboard.address();
    std::thread::spawn(move || dashboard.serve());
    (address, args, directory)
}

/// The property the slice rests on, and the one the coaching section already had: what a browser is
/// handed is the three CLI lanes' own numbers. Each fold below is a second, independent run of
/// exactly what `qanungo report`, `qanungo cost`, and `qanungo standup` do.
#[test]
fn all_three_sections_reconcile_with_their_own_folds() {
    let (address, args, _directory) = spawn_with(three_lane_archive(), &[]);
    let payload = payload_of(address);

    // Coaching, over `--last`.
    let coaching = command::fold_coaching(&args.archive, &args.last).expect("the window folds");
    assert_eq!(payload["sessions"]["folded"], coaching.sessions.len());
    assert_eq!(payload["window"]["last"], "30d");
    assert_eq!(
        payload["findings"].as_array().unwrap().len(),
        coaching.findings.len()
    );

    // Cost, over `--cost-last` — a different window, and the payload says which.
    let cost = command::fold_cost(&args.archive, &args.cost_last).expect("the window prices");
    let served = &payload["cost"];
    assert_eq!(served["window"]["last"], "12w");
    assert_eq!(served["sessions"]["priced"], cost.totals.priceable_sessions);
    assert_eq!(
        served["sessions"]["token_only"],
        cost.totals.token_only_sessions
    );
    assert_eq!(
        served["priced"]["dollars_rendered"],
        qanungo::format::dollars(cost.totals.priced.dollars),
    );
    assert_eq!(
        served["priced"]["messages"],
        cost.totals.priced.tokens.messages
    );
    let models: Vec<&str> = served["by_model"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["model"].as_str().unwrap())
        .collect();
    assert!(!models.is_empty(), "the window must price something");
    for model in &models {
        assert!(
            cost.totals.by_model.contains_key(*model),
            "{model} is not a model the fold priced",
        );
    }
    // Most expensive first, which is the CLI's ordering and the reason to read the table.
    let dollars: Vec<f64> = served["by_model"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["dollars"].as_f64().unwrap())
        .collect();
    assert!(
        dollars.windows(2).all(|pair| pair[0] >= pair[1]),
        "{dollars:?} is not most-expensive-first",
    );

    // Standup, over `--standup-last` — a third window again.
    let standup =
        command::fold_standup(&args.archive, &args.standup_last, args.redaction.redactor())
            .expect("the window narrates");
    let served = &payload["standup"];
    assert_eq!(served["window"]["last"], "7d");
    assert_eq!(served["sessions"], standup.standup.sessions);
    assert_eq!(
        served["repositories_narrated"],
        standup.standup.repositories_narrated()
    );

    // One generation carries all three: there is no shape of this payload in which two sections
    // came from different refreshes.
    assert_eq!(payload["provenance"]["generation"], 1);
    assert_eq!(payload["provenance"]["window"], "30d");
    assert_eq!(payload["provenance"]["cost_window"], "12w");
    assert_eq!(payload["provenance"]["standup_window"], "7d");
}

/// The standup section groups, orders, rolls up, and names its gaps exactly as `tests/standup.rs`
/// pins the fold doing it — reached here through the mirror, the cache, the parse, and HTTP.
#[test]
fn the_standup_section_groups_and_gaps_as_the_standup_lane_does() {
    let (address, _args, _directory) = spawn_with(three_lane_archive(), &[]);
    let standup = payload_of(address)["standup"].clone();

    // Busiest repository first, the unattributed bucket last — grouped by what each *summary*
    // names, not by the repository the listing projected onto the session row.
    let groups: Vec<(&str, usize)> = standup["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| {
            (
                group["repository"].as_str().unwrap(),
                group["sessions"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        groups,
        [
            ("surdy/qanungo", 2),
            ("surdy/munshi", 1),
            (qanungo::standup::NO_REPOSITORY, 1),
        ],
    );
    assert_eq!(standup["sessions"], 4);
    assert_eq!(standup["repositories_narrated"], 3);

    // Newest first inside a repository, on archive time.
    let qanungo = &standup["repositories"][0]["sessions"];
    assert_eq!(
        qanungo[0]["title"],
        "Price the window at list rates and refuse to price the rest",
    );
    assert_eq!(
        qanungo[1]["title"],
        "Ship the scoring lane behind a rule pack stamp"
    );
    assert!(
        qanungo[0]["archived_at"].as_str().unwrap() > qanungo[1]["archived_at"].as_str().unwrap(),
    );
    assert_eq!(qanungo[0]["source_hash"].as_str().unwrap().len(), 64);
    assert!(!qanungo[0]["goal"].as_str().unwrap().is_empty());
    assert!(!qanungo[0]["work_completed"].as_array().unwrap().is_empty());

    // A session captured outside a checkout is its own bucket, with no branch.
    let unattributed = standup["repositories"][2]["sessions"][0].clone();
    assert_eq!(
        unattributed["title"],
        "Work out a shell one-liner outside any checkout",
    );
    assert_eq!(unattributed["branch"], serde_json::Value::Null);

    // The rollups: a decision two sessions in one repository recorded verbatim appears once, and
    // every line keeps the repository it came out of.
    let decisions: Vec<(&str, &str)> = standup["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| {
            (
                line["repository"].as_str().unwrap(),
                line["text"].as_str().unwrap(),
            )
        })
        .collect();
    let shared = "Scores are recomputed on every run rather than persisted.";
    assert_eq!(
        decisions.iter().filter(|(_, text)| *text == shared).count(),
        1,
        "recorded twice in one repository, rolled up once: {decisions:?}",
    );
    assert_eq!(decisions[0], ("surdy/qanungo", shared));
    assert_eq!(
        decisions
            .iter()
            .map(|(repository, _)| *repository)
            .collect::<Vec<_>>(),
        [
            "surdy/qanungo",
            "surdy/qanungo",
            "surdy/qanungo",
            "surdy/qanungo",
            "surdy/munshi",
        ],
    );
    assert!(!standup["open_items"].as_array().unwrap().is_empty());

    // Both gaps, each named by what could be named — and neither silently dropped.
    let gaps: Vec<String> = standup["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|gap| format!("{} — {}", gap["count"], gap["reason"].as_str().unwrap()))
        .collect();
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert!(
        gaps.iter().any(|gap| gap.contains("still owes a real one")),
        "the placeholder is a gap: {gaps:?}",
    );
    assert!(
        gaps.iter()
            .any(|gap| gap.contains("no snapshot of this session carries a `summary.md`")),
        "a session with no summary anywhere is a gap: {gaps:?}",
    );
}

/// The done-bar's canary for this slice: a `summary.md` carrying three live-*shaped* credentials is
/// narrated in full over HTTP, and not one character of any of them survives — while the sentences
/// around them do.
///
/// The scrub is the *fold's*, not this surface's. What the payload owes is to serialize what the
/// fold produced, so this test also pins that the markers arrive as markers rather than being tidied
/// away: a reader has to be able to see the scrub fired.
#[test]
fn a_planted_credential_in_a_summary_never_reaches_the_payload() {
    let planted = [
        "ghp_CANARYCANARYCANARYCANARYCANARYCANARY",
        "sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99",
        "AKIACANARY0EXAMPLE99",
    ];
    let raw = String::from_utf8(summary("qanungo-cost.md")).expect("the fixture is text");
    for secret in planted {
        assert!(raw.contains(secret), "the fixture must plant {secret}");
    }

    let (address, _args, _directory) = spawn_with(three_lane_archive(), &[]);
    let (_head, body) = request(address, "/api/data");

    // Nowhere in the document, at any depth, in any section.
    for secret in planted {
        assert!(!body.contains(secret), "{secret} reached the wire");
    }
    // Not a prefix of one either: a credential cut in half is still a credential leaking.
    for prefix in ["ghp_CANARY", "sk-ant-api03-CANARY", "AKIACANARY"] {
        assert!(!body.contains(prefix), "{prefix} reached the wire");
    }

    let payload: serde_json::Value = serde_json::from_str(&body).expect("the payload is JSON");
    let standup = &payload["standup"];
    let serialized = serde_json::to_string(standup).unwrap();
    for marker in [
        "[REDACTED:github-token]",
        "[REDACTED:anthropic-key]",
        "[REDACTED:aws-access-key-id]",
    ] {
        assert!(
            serialized.contains(marker),
            "the marker is served as the fold left it: {marker}",
        );
    }
    // The prose around each credential is untouched — a section pockmarked past legibility would be
    // a redactor nobody keeps on.
    for surviving in [
        "and then rotated it.",
        "was revoked before this landed.",
        "should be rotated too.",
    ] {
        assert!(
            serialized.contains(surviving),
            "{surviving} was scrubbed too"
        );
    }

    // And the section accounts for exactly what fired, as counts against pattern ids, carrying no
    // matched value anywhere.
    assert_eq!(standup["redaction"]["total"], 3);
    let fired: Vec<(&str, u64)> = standup["redaction"]["fired"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["pattern"].as_str().unwrap(),
                entry["count"].as_u64().unwrap(),
            )
        })
        .collect();
    // In pattern order, which is the order `RedactionReport::fired` walks — stable across runs, so
    // a page rendering the list is reproducible rather than dependent on which fixture matched
    // first.
    assert_eq!(
        fired,
        [
            ("github-token", 1),
            ("anthropic-key", 1),
            ("aws-access-key-id", 1),
        ],
    );
    assert_eq!(payload["provenance"]["renders_verbatim"], true);
    assert_eq!(payload["provenance"]["redaction"]["secrets"], true);
}

/// The standup section's own claim, stated as an equality rather than as an absence.
///
/// Every string it serves **is** the string the standup fold produced — the same scrub, at the same
/// pattern revision, in the same order — so the page and `qanungo standup --last 7d` cannot come to
/// disagree about what a session said. That is a stronger pin than "no secret got through": it also
/// rules out a section that re-scrubbed (double-counting what fired), re-worded, re-ordered, or
/// quietly truncated the prose on its way to a browser.
#[test]
fn the_standup_sections_strings_are_the_standup_folds_own_strings() {
    let (address, args, _directory) = spawn_with(three_lane_archive(), &[]);
    let served = payload_of(address)["standup"].clone();

    // A second, independent run of exactly what `qanungo standup` does, over the same window and
    // the now-warm cache.
    let folded =
        command::fold_standup(&args.archive, &args.standup_last, args.redaction.redactor())
            .expect("the window narrates");
    let standup = &folded.standup;

    let groups = served["repositories"].as_array().unwrap();
    assert_eq!(groups.len(), standup.repositories.len());
    assert!(!groups.is_empty(), "this must compare something");
    let mut strings = 0;
    for (served, group) in groups.iter().zip(&standup.repositories) {
        assert_eq!(served["repository"], group.repository);
        let sessions = served["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), group.sessions.len());
        for (served, session) in sessions.iter().zip(&group.sessions) {
            assert_eq!(served["source_hash"], session.source_hash);
            assert_eq!(served["title"], session.title);
            assert_eq!(served["goal"], session.goal);
            assert_eq!(
                served["branch"],
                serde_json::json!(session.branch),
                "including the absence of one",
            );
            assert_eq!(
                served["work_completed"],
                serde_json::json!(session.work_completed)
            );
            assert_eq!(served["decisions"], serde_json::json!(session.decisions));
            assert_eq!(served["open_items"], serde_json::json!(session.open_items));
            strings += 2
                + session.work_completed.len()
                + session.decisions.len()
                + session.open_items.len();
        }
    }
    assert!(
        strings >= 20,
        "only {strings} strings compared — the fixture window is too thin to prove anything",
    );

    // The rollups and the gaps, on the same rule.
    for (key, lines) in [
        ("decisions", &standup.decisions),
        ("open_items", &standup.open_items),
    ] {
        let served = served[key].as_array().unwrap();
        assert_eq!(served.len(), lines.len(), "{key}");
        for (served, line) in served.iter().zip(lines) {
            assert_eq!(served["text"], line.text, "{key}");
            assert_eq!(served["repository"], line.repository, "{key}");
        }
    }
    let gaps = served["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), standup.gaps.len());
    for (served, note) in gaps.iter().zip(&standup.gaps) {
        assert_eq!(served["count"], note.count);
        assert_eq!(served["reason"], note.reason);
    }

    // And what fired is the fold's count, not a second pass's: a section that re-scrubbed would
    // report double here even though every string above still matched.
    assert_eq!(served["redaction"]["total"], standup.redaction.total());
    assert_eq!(served["redaction"]["total"], 3);
}

/// `--no-redact` on this lane has to actually mean raw in the standup section too, or it is a switch
/// that lies about one of the two surfaces it governs. This is the negative half of the canary
/// above: a fold that scrubbed anyway would pass that test and fail this one.
#[test]
fn no_redact_serves_the_standup_section_as_the_archive_holds_it() {
    let (address, _args, _directory) = spawn_with(three_lane_archive(), &["--no-redact"]);
    let payload = payload_of(address);
    assert_eq!(payload["provenance"]["redaction"]["secrets"], false);

    let serialized = serde_json::to_string(&payload["standup"]).unwrap();
    for secret in [
        "ghp_CANARYCANARYCANARYCANARYCANARYCANARY",
        "sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99",
        "AKIACANARY0EXAMPLE99",
    ] {
        assert!(serialized.contains(secret), "{secret} should be back");
    }
    assert!(!serialized.contains("[REDACTED:"), "nothing was scrubbed");
    assert_eq!(payload["standup"]["redaction"]["total"], 0);
}

/// The cost section against the committed price table, to the cent. The fixture's figures are round
/// on purpose, so every number below is checkable by hand against
/// `docs/pricing-sources-2026-08-23.md`.
#[test]
fn the_cost_section_prices_the_billing_fixture_to_the_cent() {
    let archive = vec![
        ArchivedSession::new(
            21,
            "claude-code",
            &transcript("cost/claude-billing.jsonl"),
            2,
        )
        .in_repository("surdy/qanungo"),
        ArchivedSession::new(
            22,
            "copilot-cli",
            &transcript("cost/copilot-billing.jsonl"),
            3,
        )
        .in_repository("surdy/munshi"),
    ];
    let (address, _args, _directory) = spawn_with(archive, &[]);
    let cost = payload_of(address)["cost"].clone();

    // Opus 5 at $5/$25 with a $10 1-hour cache write and a $0.50 cache read, its fast tier at
    // $10/$50, and Sonnet 5 at $2/$10: $13.00 + $5.00.
    assert_eq!(cost["priced"]["dollars_rendered"], "$18.00");
    assert_eq!(cost["priced"]["sessions"], 1);
    assert_eq!(cost["priced"]["priced_anything"], true);
    assert_eq!(cost["priced"]["fast_messages"], 1);
    assert_eq!(cost["sessions"]["token_only"], 1);

    let models = cost["by_model"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["model"], "claude-opus-5");
    assert_eq!(models[0]["dollars_rendered"], "$13.00");
    assert_eq!(models[0]["tokens"]["input"]["rendered"], "200.0k");
    assert_eq!(models[0]["tokens"]["cache_write_1h"]["rendered"], "400.0k");
    assert_eq!(models[0]["tokens"]["cache_write_5m"]["tokens"], 0);
    assert_eq!(models[1]["model"], "claude-sonnet-5");
    assert_eq!(models[1]["dollars_rendered"], "$5.00");

    // The repository cut follows the archive's projection, and a copilot session carries no dollars
    // to attribute to one — so `surdy/munshi` is not a row here at all.
    let repositories = cost["by_repository"].as_array().unwrap();
    assert_eq!(repositories.len(), 1);
    assert_eq!(repositories[0]["repository"], "surdy/qanungo");
    assert_eq!(repositories[0]["dollars_rendered"], "$18.00");

    // A million tokens read back at $0.50 rather than sent again at $5.00.
    assert_eq!(cost["caching"]["read"]["rendered"], "1.0M");
    assert_eq!(cost["caching"]["read_dollars_rendered"], "$0.50");
    assert_eq!(cost["caching"]["at_input_rate_rendered"], "$5.00");
    assert_eq!(cost["caching"]["saving_rendered"], "$4.50");
    assert_eq!(cost["caching"]["write_1h"], 400_000);

    // Everything the table would not price, named separately and kept out of the total.
    assert_eq!(cost["flagged"]["any"], true);
    assert_eq!(cost["flagged"]["synthetic"]["messages"], 1);
    assert_eq!(
        cost["flagged"]["synthetic"]["tokens"]["output"]["tokens"],
        1_000
    );
    let unpriced = cost["flagged"]["unpriced"].as_array().unwrap();
    assert_eq!(unpriced.len(), 1);
    assert!(
        unpriced[0]["detail"]
            .as_str()
            .unwrap()
            .contains("claude-opus-9-imaginary"),
        "{unpriced:?}",
    );
    assert_eq!(unpriced[0]["tokens"]["output"]["tokens"], 700);

    // Deduplication did something, and the number that proves it is served rather than assumed.
    assert_eq!(cost["duplicate_records"], 2);
    assert_eq!(cost["price_table_revision"], "2026-08-23");
}

/// The lane's honesty rule, over the wire: copilot rows are token volumes and the payload carries
/// **no money-shaped field for them at all** — not a zero, not an estimate, not a blended total that
/// hides the split. A page cannot render a dollar figure it was never handed.
#[test]
fn the_copilot_rows_carry_no_dollars_over_the_wire() {
    // A window of copilot alone, so a blended total would have nowhere to hide.
    let (address, _args, _directory) = spawn_with(
        vec![
            ArchivedSession::new(
                23,
                "copilot-cli",
                &transcript("cost/copilot-billing.jsonl"),
                2,
            )
            .in_repository("surdy/munshi"),
        ],
        &[],
    );
    let cost = payload_of(address)["cost"].clone();

    assert_eq!(cost["copilot"]["basis"], "tokens-only");
    let rows = cost["copilot"]["rows"].as_array().unwrap();
    let volumes: Vec<(&str, u64)> = rows
        .iter()
        .map(|row| {
            (
                row["model"].as_str().unwrap(),
                row["output"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(volumes, [("claude-opus-4.8", 2_000), ("gpt-5.6-sol", 500)]);
    assert_eq!(rows[0]["output_rendered"], "2.0k");

    // No dollar sign, and no money-shaped key, anywhere under the copilot block.
    let serialized = serde_json::to_string(&cost["copilot"]).unwrap();
    assert!(!serialized.contains('$'), "{serialized}");
    for forbidden in ["dollar", "cost", "price", "credit", "spend"] {
        assert!(!serialized.contains(forbidden), "{forbidden}: {serialized}");
    }

    // And the window's total says nothing was priced rather than reporting a blended figure with
    // copilot's tokens quietly inside it.
    assert_eq!(cost["priced"]["priced_anything"], false);
    assert_eq!(cost["priced"]["dollars"], 0.0);
    assert_eq!(cost["priced"]["dollars_rendered"], "$0.00");
    assert_eq!(cost["sessions"]["priced"], 0);
    assert!(cost["by_model"].as_array().unwrap().is_empty());
    assert!(cost["by_repository"].as_array().unwrap().is_empty());
    assert_eq!(cost["caching"], serde_json::Value::Null);
}

/// The two new sections extend the invariants the page already held: nothing that could be a link,
/// no markup, no asset, and the same three routes.
#[test]
fn the_page_renders_the_two_new_sections_under_the_same_invariants() {
    let (address, _args, _directory) = spawn_with(three_lane_archive(), &[]);
    let (head, body) = request(address, "/");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");

    // The sections exist, are labelled, and are filled by the payload's own keys.
    for anchor in [
        r#"id="cost""#,
        r#"id="standup""#,
        r#"id="cost-heading""#,
        r#"id="standup-heading""#,
        "costSection(data.cost)",
        "standupSection(data.standup)",
    ] {
        assert!(body.contains(anchor), "the page has no {anchor}");
    }
    // The copilot table names itself as volumes, so a reader cannot mistake a missing column for a
    // missing figure.
    assert!(
        body.contains("Token volumes (copilot) — no dollars"),
        "the copilot table must say what it is",
    );
    assert!(
        body.contains("no credit estimate, no premium-request count"),
        "the page states why copilot has no money on it",
    );

    // Every invariant the V1 page was pinned by, restated over the grown file.
    assert!(!body.contains("href"), "the page carries no links at all");
    assert!(!body.contains("<a "), "the page carries no anchors");
    assert!(
        !body.contains("innerHTML"),
        "every value is set as text, never parsed as markup",
    );
    assert!(
        !body.contains("http://") && !body.contains("https://") && !body.contains("//fonts."),
        "the page loads nothing from anywhere",
    );
    assert!(
        !body.contains("/api/v1/artifacts"),
        "no route into the archive, which serves unredacted blobs",
    );
}

/// The Context Management lane, end to end over the served surface (qanungo #4, munshi#77 pull A).
///
/// Every session in this archive compacted past the threshold, so the lane renders a *score* rather
/// than the "not scored" sentence it carried for its whole life before this — and the churn finding
/// beside it is structural, with no anchor and no excerpt to ask for. Five copies of each fixture
/// because a fire rate under `MIN_SCORED_SESSIONS` is not a reading, which is the same discipline
/// every other lane holds.
#[test]
fn the_context_management_lane_scores_and_its_finding_offers_no_excerpt() {
    let claude = transcript("munshi/claude-code-2.1.235-compaction.jsonl");
    let copilot = transcript("munshi/copilot-1.0.76-compaction.jsonl");
    let archive: Vec<ArchivedSession> = (0..5)
        .flat_map(|index| {
            [
                ArchivedSession::new(index, "claude-code", &claude, 2),
                ArchivedSession::new(index + 100, "copilot-cli", &copilot, 3),
            ]
        })
        .collect();
    let (address, _directory) = spawn_dashboard(archive);
    let payload = payload_of(address);

    let lane = payload["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "context-management")
        .expect("the lane is on the page");
    assert_eq!(lane["reason"], serde_json::Value::Null, "no longer unfed");
    assert_eq!(lane["fleet"]["state"], "scored");
    // Every eligible session thrashed, which is four times the fire-rate floor and clamps to the
    // component's whole share — and the lane has exactly one component to spend.
    assert_eq!(lane["fleet"]["score"], 0);
    for harness in lane["harnesses"].as_array().unwrap() {
        assert_eq!(harness["score"], 0, "{}", harness["source_agent"]);
        assert_eq!(harness["components"].as_array().unwrap().len(), 1);
        assert_eq!(harness["components"][0]["label"], "Compaction churn");
    }

    let finding = payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["rule"] == "compaction-churn")
        .expect("the churn rule fires on this window");
    assert_eq!(finding["evidence_kind"], "structural");
    let evidence = &finding["evidence"][0];
    assert!(
        evidence["anchors"].as_array().unwrap().is_empty(),
        "a compaction marker carries no verbatim and has no tool-event ordinal to anchor",
    );
    // It renders the same structural block every session-shaped finding does, and that block is
    // still numbers and timestamps only.
    let mut leaves = 0;
    assert_structural_leaves(&evidence["structural"], "structural", &mut leaves);
    assert!(leaves > 0, "the finding shows the session's shape instead");

    // And an excerpt is refused, exactly as it is for a duration.
    let source_hash = evidence["source_hash"].as_str().unwrap();
    let (head, _body) = request(address, &format!("/api/evidence/{source_hash}/1"));
    assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{head}");
}
