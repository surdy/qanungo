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
        match sessions.iter().find(|session| session.artifact_id == id) {
            Some(session) => content_response(session),
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
                    "project":null,"repository":null,"branch":null,"source_agent_version":null,
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
                "content_url":"/api/v1/artifacts/{}/content"}}]}}"#,
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

fn content_response(session: &ArchivedSession) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/jsonl\r\n\
         Content-Length: {}\r\n\
         x-patwari-compression: identity\r\n\
         x-patwari-original-size-bytes: {}\r\n\
         x-patwari-original-sha256: sha256:{}\r\n\
         x-patwari-stored-size-bytes: {}\r\n\
         x-patwari-stored-sha256: sha256:{}\r\n\r\n",
        session.transcript.len(),
        session.transcript.len(),
        session.original_sha256,
        session.transcript.len(),
        session.original_sha256,
    )
    .into_bytes();
    response.extend_from_slice(&session.transcript);
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

/// The redaction line, on the surface that publishes to a browser. The fixtures put a canary in
/// every free-text transcript field this crate touches, and none of them may reach the payload.
///
/// The line moved with this slice and the test says exactly where it moved to. **Anchors are not
/// content**: the payload now names tool names, locators, record numbers, and timestamps, which are
/// schema metadata and positions. Everything a human typed or a tool printed is still absent — and
/// reaches a reader only through the excerpt route, scrubbed, one counted event at a time.
#[test]
fn the_served_payload_contains_no_verbatim_transcript_content() {
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
    // The surface renders verbatim now — through the route, never here — and says so.
    assert_eq!(payload["provenance"]["renders_verbatim"], true);
    assert_eq!(payload["provenance"]["redaction"]["secrets"], true);

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
