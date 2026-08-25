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
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sessions = Arc::new(sessions);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sessions = Arc::clone(&sessions);
            std::thread::spawn(move || serve_archive(stream, &sessions));
        }
    });
    base
}

fn serve_archive(mut stream: TcpStream, sessions: &[ArchivedSession]) {
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
    let Command::Dashboard(args) = Cli::parse_from([
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
    ])
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
/// every free-text transcript field this crate touches, and none of them may reach the wire.
///
/// It is stricter than the report's own check on one point: a coaching report may render tool names,
/// because they are schema metadata. This payload does not carry them either — its evidence is a
/// count and a hash — so a field that began naming tools would fail here.
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
    assert_eq!(payload["provenance"]["renders_verbatim"], false);

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
        "tool_use",
        "Bash",
    ] {
        assert!(!body.contains(forbidden), "`{forbidden}` reached the wire");
    }

    // What it does carry: counts, and hashes to go and read the rest for yourself.
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
    // And no route back into the archive, whose blobs are served unredacted.
    assert!(!body.contains("/content"), "{body}");
    assert!(!body.contains("/api/v1/artifacts"), "{body}");
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

/// Three routes, read-only, and nothing else — including nothing that looks like a path into the
/// archive or the filesystem.
#[test]
fn nothing_but_the_three_routes_answers() {
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
    // A query string changes nothing: there is no per-request knob on this surface at all.
    let (head, body) = request(address, "/api/data?redact=off");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(!body.contains("CANARY"), "a query string is not a switch");
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
    assert_eq!(payload["provenance"]["renders_verbatim"], false);
}
