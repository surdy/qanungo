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
    /// The host the snapshot manifest's `capture.source_metadata` records the capture running on,
    /// which is what the per-device scope groups by. `None` is a capture from before the metadata
    /// existed — the manifest carries an empty `source_metadata` — and lands in the `NO_DEVICE`
    /// bucket.
    hostname: Option<String>,
    /// The capturing machine's UTC offset the snapshot manifest recorded (`-07:00`), when it
    /// recorded one. `None` is a capture from before the offset metadata existed — the state the
    /// heatmap places on no cell and counts. Written into the manifest's `capture.source_metadata`.
    utc_offset: Option<String>,
}

impl ArchivedSession {
    /// One archived session carrying `transcript`, completed `hours_ago` — relative to now rather
    /// than at a fixed date, so a window measured in days keeps selecting it however long this test
    /// lives.
    fn new(index: u8, source_agent: &str, transcript: &[u8], hours_ago: i64) -> Self {
        let completed = Utc::now() - TimeDelta::hours(hours_ago);
        Self {
            session_id: format!("{index:02x}").repeat(16),
            snapshot_id: snapshot_id(index),
            artifact_id: format!("{:02x}", index.wrapping_add(200)).repeat(16),
            source_agent: source_agent.to_owned(),
            original_sha256: sha256_hex(transcript),
            transcript: transcript.to_vec(),
            completed_at: completed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            summary: None,
            summary_artifact_id: format!("{:02x}", index.wrapping_add(50)).repeat(16),
            summary_sha256: String::new(),
            repository: None,
            hostname: None,
            utc_offset: None,
        }
    }

    /// Records the capturing machine's UTC offset on this session's snapshot manifest — the fact the
    /// heatmap shifts a transcript instant into a local hour by. Only the heatmap reads it.
    fn with_utc_offset(mut self, offset: &str) -> Self {
        self.utc_offset = Some(offset.to_owned());
        self
    }

    /// Attaches a `summary.md` to this session's snapshot, so the standup lane has something to
    /// narrate for it.
    fn with_summary(mut self, summary: &[u8]) -> Self {
        self.summary_sha256 = sha256_hex(summary);
        self.summary = Some(summary.to_vec());
        self
    }

    /// Moves this session's archive completion to an exact wall-clock time on a UTC day some
    /// number of days back — the only builder that can put two sessions either side of a midnight.
    ///
    /// [`ArchivedSession::new`] dates a session in hours-ago, so a window measured in days keeps
    /// selecting it however long this test file lives; that is right for every lane which only asks
    /// *whether* a session is in the window. The timeline asks *which day* it is on, and a day
    /// boundary is a thing you can only get wrong at midnight — so this pins the time of day while
    /// keeping the date relative, which is both properties at once.
    fn completed_on(mut self, days_ago: i64, hour: u32, minute: u32, second: u32) -> Self {
        let at = (Utc::now() - TimeDelta::days(days_ago))
            .date_naive()
            .and_hms_opt(hour, minute, second)
            .expect("a real time of day")
            .and_utc();
        self.completed_at = at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self
    }

    /// Sets the repository the *listing's* projection reports — the string the cost lane's
    /// by-repository cut is keyed on. Deliberately not the one a `summary.md` names: the standup
    /// lane reads the summary's own, and the two are different facts about a session.
    fn in_repository(mut self, repository: &str) -> Self {
        self.repository = Some(repository.to_owned());
        self
    }

    /// Sets the host the snapshot manifest records the capture running on — the fact the per-device
    /// scope groups by. A session left without one stays in the `NO_DEVICE` bucket, the state of a
    /// capture written before the metadata existed.
    fn on_device(mut self, hostname: &str) -> Self {
        self.hostname = Some(hostname.to_owned());
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

/// The same, plus a counter of **every request the archive is asked for** — listings, snapshot
/// documents, and artifact content alike.
///
/// It is what pins the invariant both request-path routes rest on: a dashboard that answered a
/// browser by reaching for the archive would be a remote control for somebody else's bandwidth and
/// for what lands on this disk, so the tests assert the counter does not move rather than asserting
/// the response looked right.
///
/// Counting *all* of them is deliberate, and is the second version of this helper. Counting only
/// artifact content measured less than the tests claimed: a request path that listed the window —
/// or walked a session's snapshots, or fetched one snapshot document — would have moved no counter
/// and passed. The invariant is that a request induces **no archive traffic of any kind**, so that
/// is what is counted.
fn spawn_counted_archive(sessions: Vec<ArchivedSession>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sessions = Arc::new(sessions);
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sessions = Arc::clone(&sessions);
            let requests = Arc::clone(&counted);
            std::thread::spawn(move || serve_archive(stream, &sessions, &requests));
        }
    });
    (base, requests)
}

fn serve_archive(mut stream: TcpStream, sessions: &[ArchivedSession], requests: &AtomicUsize) {
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

    // Counted here, once, before the route is even decided: what the tests assert is that a request
    // to the *dashboard* produces no traffic here at all, and a counter attached to one branch
    // would measure one kind of reaching-out rather than the absence of any.
    requests.fetch_add(1, Ordering::Relaxed);

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
                    "source_state_hash":null,"source_metadata":{source_metadata},"project":null,
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
        source_metadata = source_metadata(session),
    )
}

/// The manifest's `capture.source_metadata` object: the capturing machine's `hostname` (the fact the
/// per-device scope groups by) and its `utc_offset` (the fact the heatmap reads), each present only
/// when the session names it. The empty `{}` — a capture written before the metadata existed — is
/// what every other fixture here stays in.
fn source_metadata(session: &ArchivedSession) -> String {
    let mut fields = Vec::new();
    if let Some(hostname) = &session.hostname {
        fields.push(format!(r#""hostname":"{hostname}""#));
    }
    if let Some(offset) = &session.utc_offset {
        fields.push(format!(r#""utc_offset":"{offset}""#));
    }
    format!("{{{}}}", fields.join(","))
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

/// A window that ran on more than one machine, so the per-device scope has something to group.
///
/// Three real hosts and two states no host name renders: a session captured before the metadata
/// existed (`None`, the `NO_DEVICE` bucket) and one whose recorded hostname carries a pipe — a valid
/// JSON string the identifier clamp still refuses, so it reaches the wire as `INVALID_IDENTIFIER`
/// rather than as a raw option. `macbookpro` carries both harnesses, so the scope's per-harness
/// split is a claim with two terms to reconcile.
fn device_archive() -> Vec<ArchivedSession> {
    vec![
        ArchivedSession::new(
            21,
            "claude-code",
            &transcript("rules/marathon-session.jsonl"),
            2,
        )
        .on_device("macbookpro"),
        ArchivedSession::new(
            22,
            "claude-code",
            &transcript("rules/high-tool-error-rate.jsonl"),
            3,
        )
        .on_device("macbookpro"),
        ArchivedSession::new(
            23,
            "copilot-cli",
            &transcript("cost/copilot-billing.jsonl"),
            4,
        )
        .on_device("macbookpro"),
        ArchivedSession::new(24, "claude-code", &transcript("rules/retry-loop.jsonl"), 5)
            .on_device("j2vjcmqmyx"),
        // No hostname on the manifest: the capture predates the metadata, the `NO_DEVICE` bucket.
        ArchivedSession::new(
            25,
            "claude-code",
            &transcript("cost/claude-billing.jsonl"),
            6,
        ),
        // A hostile hostname the clamp refuses.
        ArchivedSession::new(26, "claude-code", &transcript("rules/babysitting.jsonl"), 7)
            .on_device("host|evil"),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A snapshot id shaped like the archive's: a lowercase UUID, which is the only shape the cache's
/// snapshot index will file. Sixteen copies of one byte, hyphenated 8-4-4-4-12.
fn snapshot_id(index: u8) -> String {
    let pair = format!("{:02x}", index.wrapping_add(100));
    let run = |count: usize| pair.repeat(count);
    format!("{}-{}-{}-{}-{}", run(4), run(2), run(2), run(2), run(6))
}

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
    let folded = command::fold_coaching(&args.archive, &args.last, &args.redaction.redactor())
        .expect("the window folds");
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
    let (base, requests) = spawn_counted_archive(canary_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let cache_root = directory.path().join("qanungo");
    let args = args(&base, &cache_root);
    let dashboard = Dashboard::start(&args).expect("the first fold");
    let address = dashboard.address();
    std::thread::spawn(move || dashboard.serve());

    let payload = payload_of(address);
    let (source_hash, locator) = first_anchor(&payload);
    let mirrored = requests.load(Ordering::Relaxed);
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
        requests.load(Ordering::Relaxed),
        mirrored,
        "the request must not have reached for the archive — for anything, not only for a blob",
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
    let coaching = command::fold_coaching(&args.archive, &args.last, &args.redaction.redactor())
        .expect("the window folds");
    assert_eq!(payload["sessions"]["folded"], coaching.sessions.len());
    assert_eq!(payload["window"]["last"], "30d");
    assert_eq!(
        payload["findings"].as_array().unwrap().len(),
        coaching.findings.len()
    );

    // Cost, over `--cost-last` — a different window, and the payload says which.
    let cost = command::fold_cost(&args.archive, &args.cost_last, &args.redaction.redactor())
        .expect("the window prices");
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

// ---------------------------------------------------------------------------
// Scope selection (qanungo #5): repository and harness
// ---------------------------------------------------------------------------

/// A window cut three ways, arranged so that every scope's score can be worked out by hand.
///
/// The Context Management lane is the instrument, because it is the pack's only **single-component**
/// lane: its score is `100 − 100 × clamp(fire_rate / 0.25, 0, 1)` and nothing else, so a fire rate
/// read off the fixture is a lane score with no second reading to reason about. Everything below is
/// either munshi's compaction fixture (four completions — the churn rule fires) or a claude
/// transcript with no compaction marker in it at all (observable and clean, so it counts in the
/// denominator rather than leaving it).
///
/// | Scope | Sessions | Thrashed | Rate | Lane |
/// | --- | --- | --- | --- | --- |
/// | `surdy/qanungo` · claude-code | 10 | 1 | 10% | 100 − 100×(0.10/0.25) = **60** |
/// | `surdy/qanungo` · copilot-cli | 5 | 5 | 100% | clamped → **0** |
/// | `surdy/qanungo` · fleet | — | — | — | mean(60, 0) = **30** |
/// | `surdy/munshi` · claude-code | 10 | 0 | 0% | **100** |
/// | unattributed · claude-code | 5 | 0 | 0% | **100** |
/// | whole window · claude-code | 25 | 1 | 4% | 100 − 100×(0.04/0.25) = **84** |
/// | whole window · fleet | — | — | — | mean(84, 0) = **42** |
///
/// The comparison window holds ten clean claude-code sessions in `surdy/qanungo` and nothing else,
/// which buys two facts at once: that scope's claude-code column moved 100 → 60, and its fleet
/// blend cannot carry an arrow at all, because copilot is in the roster on one side and not the
/// other.
fn scoped_archive() -> Vec<ArchivedSession> {
    let clean = transcript("rules/marathon-session.jsonl");
    let thrashing = transcript("munshi/claude-code-2.1.235-compaction.jsonl");
    let copilot = transcript("munshi/copilot-1.0.76-compaction.jsonl");
    let mut sessions = Vec::new();
    for index in 0..10u8 {
        let body = if index == 0 { &thrashing } else { &clean };
        sessions.push(
            ArchivedSession::new(index, "claude-code", body, 24).in_repository("surdy/qanungo"),
        );
    }
    for index in 10..15u8 {
        sessions.push(
            ArchivedSession::new(index, "copilot-cli", &copilot, 25).in_repository("surdy/qanungo"),
        );
    }
    for index in 15..25u8 {
        sessions.push(
            ArchivedSession::new(index, "claude-code", &clean, 26).in_repository("surdy/munshi"),
        );
    }
    // Captured outside a checkout: a real state, and its own bucket.
    for index in 25..30u8 {
        sessions.push(ArchivedSession::new(index, "claude-code", &clean, 27));
    }
    // The comparison window — 45 days back, inside `[60d, 30d)`.
    for index in 30..40u8 {
        sessions.push(
            ArchivedSession::new(index, "claude-code", &clean, 24 * 45)
                .in_repository("surdy/qanungo"),
        );
    }
    sessions
}

/// The lanes whose every component is a fire rate over *sessions*, so a scope with fewer than
/// [`qanungo::scoring::constants::MIN_SCORED_SESSIONS`] eligible sessions cannot read them.
///
/// Tool Mastery is deliberately not here: its pooled component is a rate over tool *calls*, gated
/// by a minimum number of calls rather than of sessions, so a handful of busy sessions is a real
/// reading and refusing it would be the honest-refusal discipline applied to the wrong denominator.
const FIRE_RATE_LANES: [&str; 4] = [
    "prompt-quality",
    "session-hygiene",
    "code-review",
    "context-management",
];

/// How many lanes the pack scores.
const LANES: u8 = 5;

/// One repository's pre-folded scope.
fn scope_of<'a>(payload: &'a serde_json::Value, repository: &str) -> &'a serde_json::Value {
    payload["scopes"]["repositories"]
        .as_array()
        .expect("the scopes section lists repositories")
        .iter()
        .find(|entry| entry["repository"] == repository)
        .unwrap_or_else(|| panic!("{repository} is not a scope"))
}

/// One lane inside a scope, or inside the whole-window section.
fn lane_in<'a>(scope: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    scope["lanes"]
        .as_array()
        .expect("five lanes")
        .iter()
        .find(|lane| lane["key"] == key)
        .unwrap_or_else(|| panic!("no {key} lane"))
}

/// One harness's column inside a lane.
fn column_in<'a>(lane: &'a serde_json::Value, source_agent: &str) -> &'a serde_json::Value {
    lane["harnesses"]
        .as_array()
        .expect("a column per harness")
        .iter()
        .find(|column| column["source_agent"] == source_agent)
        .unwrap_or_else(|| panic!("no {source_agent} column"))
}

/// The slice, end to end: every scope's score is the pack's own arithmetic over that scope's
/// sessions, and the all/all numbers are exactly the ones the page served before this slice.
///
/// The expectations are the table in [`scoped_archive`], computed by hand rather than by folding a
/// second time — a scope reconciling with a re-fold would prove only that two calls agree, which is
/// what the seam already guarantees. What is under test is that grouping the fold a second way
/// produces the *right* groups.
#[test]
fn every_repository_scope_scores_its_own_sessions_and_nothing_else() {
    let (address, _directory) = spawn_dashboard(scoped_archive());
    let payload = payload_of(address);

    // The whole window is unchanged by the slice: the top level is still the all/all scope.
    let whole = lane_in(&payload, "context-management");
    assert_eq!(column_in(whole, "claude-code")["score"], 84);
    assert_eq!(column_in(whole, "copilot-cli")["score"], 0);
    assert_eq!(whole["fleet"]["score"], 42);
    assert_eq!(payload["sessions"]["folded"], 30);

    // Scopes are listed busiest first, with the unattributed residue last.
    let labels: Vec<&str> = payload["scopes"]["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["repository"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        vec!["surdy/qanungo", "surdy/munshi", "no repository recorded"],
    );
    assert_eq!(payload["scopes"]["unattributed"], "no repository recorded");
    assert_eq!(
        payload["scopes"]["harnesses"],
        serde_json::json!(["claude-code", "copilot-cli"]),
    );

    let qanungo = scope_of(&payload, "surdy/qanungo");
    assert_eq!(qanungo["attributed"], true);
    assert_eq!(qanungo["sessions"]["folded"], 15);
    assert_eq!(qanungo["sessions"]["comparison_folded"], 10);
    assert_eq!(
        qanungo["sessions"]["by_harness"],
        serde_json::json!({ "claude-code": 10, "copilot-cli": 5 }),
    );
    let lane = lane_in(qanungo, "context-management");
    assert_eq!(column_in(lane, "claude-code")["score"], 60);
    assert_eq!(column_in(lane, "copilot-cli")["score"], 0);
    // The fleet inside a scope is the unweighted mean over the harnesses *present in that scope*,
    // which is the same rule the whole window blends by.
    assert_eq!(lane["fleet"]["state"], "scored");
    assert_eq!(lane["fleet"]["score"], 30);
    assert_eq!(
        lane["fleet"]["harnesses"],
        serde_json::json!(["claude-code", "copilot-cli"]),
    );

    let munshi = scope_of(&payload, "surdy/munshi");
    assert_eq!(munshi["sessions"]["folded"], 10);
    assert_eq!(munshi["sessions"]["comparison_folded"], 0);
    let lane = lane_in(munshi, "context-management");
    assert_eq!(column_in(lane, "claude-code")["score"], 100);
    assert_eq!(lane["fleet"]["score"], 100);
    // Only one harness worked in this repository, so only one is in its blend — and the column for
    // the other says "no sessions" rather than disappearing, because the columns are the window's
    // union and a harness that did not appear here is a fact rather than an absence.
    assert_eq!(
        lane["fleet"]["harnesses"],
        serde_json::json!(["claude-code"])
    );
    assert_eq!(column_in(lane, "copilot-cli")["state"], "no-sessions");
    assert_eq!(column_in(lane, "copilot-cli")["sessions"], 0);

    let unattributed = scope_of(&payload, "no repository recorded");
    assert_eq!(unattributed["attributed"], false);
    assert_eq!(unattributed["sessions"]["folded"], 5);
    assert_eq!(
        column_in(lane_in(unattributed, "context-management"), "claude-code")["score"],
        100,
    );

    // Every scope accounts for every folded session exactly once.
    let scoped: u64 = payload["scopes"]["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["sessions"]["folded"].as_u64().unwrap())
        .sum();
    assert_eq!(scoped, payload["sessions"]["folded"].as_u64().unwrap());
}

/// A trend inside a scope is the same statement as a trend over the window, taken over less: both
/// windows are cut by the same repository key, and the roster rule that suppresses a fleet arrow
/// applies inside a scope exactly as it does outside one.
#[test]
fn a_scopes_trend_is_taken_against_the_same_repository_in_the_earlier_window() {
    let (address, _directory) = spawn_dashboard(scoped_archive());
    let payload = payload_of(address);
    let lane = lane_in(scope_of(&payload, "surdy/qanungo"), "context-management");

    let trend = &column_in(lane, "claude-code")["trend"];
    assert_eq!(trend["direction"], "down");
    assert_eq!(trend["points"], 40, "100 in the earlier window, 60 in this");
    assert_eq!(trend["was"], 100);

    // Copilot worked in this repository only in the later window, so the blend is over a different
    // roster on each side and carries no arrow at all — a roster change moves an unweighted mean
    // with nobody's behaviour behind it.
    assert_eq!(lane["fleet"]["trend"], serde_json::Value::Null);
    // The copilot column itself has no earlier score to move against, which is a different fact
    // from a flat one.
    assert_eq!(
        column_in(lane, "copilot-cli")["trend"],
        serde_json::Value::Null
    );

    // A repository the earlier window never held has no arrow anywhere.
    let munshi = lane_in(scope_of(&payload, "surdy/munshi"), "context-management");
    assert_eq!(
        column_in(munshi, "claude-code")["trend"],
        serde_json::Value::Null
    );
    assert_eq!(munshi["fleet"]["trend"], serde_json::Value::Null);
}

/// A scope with nothing to read scores nothing. Never a zero, never a hundred, and never a number
/// carried over from the scope beside it.
#[test]
fn a_scope_too_small_to_read_renders_no_reading_rather_than_a_number() {
    // Four sessions in their own repository: one below `MIN_SCORED_SESSIONS`, so no fire rate in
    // this scope is a reading however the sessions went.
    let clean = transcript("rules/marathon-session.jsonl");
    let mut sessions: Vec<ArchivedSession> = (0..10u8)
        .map(|index| {
            ArchivedSession::new(index, "claude-code", &clean, 24).in_repository("surdy/qanungo")
        })
        .collect();
    sessions.extend((10..14u8).map(|index| {
        ArchivedSession::new(index, "claude-code", &clean, 25).in_repository("surdy/thin")
    }));
    let (address, _directory) = spawn_dashboard(sessions);
    let payload = payload_of(address);

    let thin = scope_of(&payload, "surdy/thin");
    assert_eq!(thin["sessions"]["folded"], 4);
    for key in FIRE_RATE_LANES {
        let lane = lane_in(thin, key);
        assert_eq!(
            lane["fleet"]["state"], "no-reading",
            "{key} invented a reading",
        );
        assert_eq!(lane["fleet"]["score"], serde_json::Value::Null);
        let column = column_in(lane, "claude-code");
        assert_eq!(column["state"], "no-reading", "{key}");
        assert_eq!(column["score"], serde_json::Value::Null);
        // And it says why, in the pack's own words, rather than going quiet.
        assert!(
            column["components"]
                .as_array()
                .unwrap()
                .iter()
                .all(|component| component["cost"].is_null()),
            "a silent component has no say: {column}",
        );
    }
    // The window it is part of does score, so this is the scope's own emptiness and not the fold's.
    assert_eq!(
        lane_in(scope_of(&payload, "surdy/qanungo"), "context-management")["fleet"]["score"],
        100,
    );
}

/// The tags and the counts are one statement.
///
/// Every cited session carries the repository and harness labels its scope is keyed on, and the
/// per-scope fire counts serialized beside them are counts of exactly those rows. That equality is
/// what lets the page narrow a finding list without evaluating a rule: the number under a heading
/// and the rows under it come from the same place.
#[test]
fn every_cited_session_is_tagged_with_the_scope_that_counts_it() {
    let (address, _directory) = spawn_dashboard(scoped_archive());
    let payload = payload_of(address);
    let scopes = payload["scopes"]["repositories"].as_array().unwrap();

    for finding in payload["findings"].as_array().unwrap() {
        let rule = finding.rule_key();
        let mut tagged = 0_usize;
        for evidence in finding["evidence"].as_array().unwrap() {
            let repository = evidence["repository"]
                .as_str()
                .unwrap_or_else(|| panic!("{rule}: an untagged citation"));
            let harness = evidence["harness"]
                .as_str()
                .unwrap_or_else(|| panic!("{rule}: an untagged citation"));
            assert!(
                scopes.iter().any(|scope| scope["repository"] == repository),
                "{rule}: cited a repository that is not a scope: {repository}",
            );
            assert!(
                payload["scopes"]["harnesses"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|label| label == harness),
                "{rule}: cited a harness that is not on the control: {harness}",
            );
            tagged += 1;
        }
        assert_eq!(
            tagged,
            finding["sessions_affected"].as_u64().unwrap() as usize,
            "{rule}: the count and the citations disagree",
        );

        // The per-scope counts partition the same citations: the tags a page filters by and the
        // counts a page could show cannot come apart.
        let mut summed = 0_usize;
        for scope in scopes {
            let row = scope["findings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["rule"] == rule.as_str())
                .unwrap_or_else(|| panic!("{rule} has no row in {}", scope["repository"]));
            let counted = row["sessions_affected"].as_u64().unwrap() as usize;
            let by_hand = finding["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|evidence| evidence["repository"] == scope["repository"])
                .count();
            assert_eq!(counted, by_hand, "{rule} in {}", scope["repository"]);
            let split: u64 = row["by_harness"]
                .as_object()
                .unwrap()
                .values()
                .map(|count| count.as_u64().unwrap())
                .sum();
            assert_eq!(
                split as usize, counted,
                "{rule}: the harness split is short"
            );
            summed += counted;
        }
        assert_eq!(summed, tagged, "{rule}: the scopes lost a citation");
    }

    // The window's one firing rule landed entirely in one repository, split across both harnesses —
    // checkable by hand against the fixture.
    let churn = scope_of(&payload, "surdy/qanungo")["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["rule"] == "compaction-churn")
        .expect("the churn rule has a row");
    assert_eq!(churn["sessions_affected"], 6);
    assert_eq!(
        churn["by_harness"],
        serde_json::json!({ "claude-code": 1, "copilot-cli": 5 }),
    );
    let elsewhere = scope_of(&payload, "surdy/munshi")["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["rule"] == "compaction-churn")
        .expect("a rule that fired nowhere here still has a row, carrying a zero");
    assert_eq!(elsewhere["sessions_affected"], 0);
}

trait RuleKey {
    fn rule_key(&self) -> String;
}

impl RuleKey for serde_json::Value {
    fn rule_key(&self) -> String {
        self["rule"]
            .as_str()
            .expect("a finding names its rule")
            .to_owned()
    }
}

/// A repository name is an archive-stated identifier on a surface that renders verbatim, so it is
/// clamped **and then** scrubbed — on **every** path that spells it.
///
/// The review of this slice found the path that did not. The cost lane rendered its repository
/// through the clamp alone, and the scope key went through clamp-then-scrub, so a repository named
/// like a credential came out as two different strings: the marker on the coaching side and the
/// raw token on the bill. That put the secret on the wire *and* split one repository into two
/// options, each narrowing half the page — which is precisely what a scope control must never do.
///
/// Every session here carries **priced usage**, because that is what puts a repository in
/// `cost.by_repository` at all. The earlier version of this test used a transcript with no billing
/// records in it, so the cost path was never taken and the test could not fail.
#[test]
fn a_hostile_repository_name_never_reaches_a_scope_control() {
    // The billing fixture once per repository, with its message ids rewritten each time:
    // `CostTotals::fold` deduplicates by message id across the whole window, so verbatim copies
    // would price once and only one repository would get a row.
    let billing = |tag: &str| {
        String::from_utf8(transcript("cost/claude-billing.jsonl"))
            .expect("the fixture is utf-8")
            .replace("msg_", &format!("msg_{tag}_"))
            .into_bytes()
    };
    let planted = "ghp_FAKEfake0123456789ABCDEFabcdef012345";
    // Not shaped like an identifier at all — a table pipe and a backtick — so the clamp replaces it
    // wholesale rather than truncating: a prefix of arbitrary text is still arbitrary text.
    let unrenderable = "surdy/evil|table`break";
    let sessions = vec![
        ArchivedSession::new(1, "claude-code", &billing("a"), 24).in_repository(planted),
        ArchivedSession::new(2, "claude-code", &billing("b"), 25).in_repository(unrenderable),
        ArchivedSession::new(3, "claude-code", &billing("c"), 26).in_repository("surdy/qanungo"),
    ];
    let (address, _directory) = spawn_dashboard(sessions);
    let (_head, body) = request(address, "/api/data");

    for raw in [planted, "surdy/evil|table", "table`break"] {
        assert!(
            !body.contains(raw),
            "a raw repository name reached the wire: {raw}",
        );
    }
    let payload: serde_json::Value = serde_json::from_str(&body).expect("json");
    let labels: Vec<&str> = payload["scopes"]["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["repository"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"surdy/qanungo"), "{labels:?}");
    assert!(
        labels.contains(&"invalid-identifier"),
        "the unrenderable name is replaced wholesale: {labels:?}",
    );
    let scrubbed = labels
        .iter()
        .find(|label| label.contains("REDACTED"))
        .unwrap_or_else(|| panic!("the credential-shaped name is not scrubbed: {labels:?}"));

    // The regression itself: three repositories, three scopes. A repository spelled one way by the
    // coaching fold and another by the bill would be **four**, two of them half-empty.
    assert_eq!(labels.len(), 3, "{labels:?}");
    let priced: Vec<&str> = payload["cost"]["by_repository"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row["repository"]
                .as_str()
                .unwrap_or_else(|| payload["scopes"]["unattributed"].as_str().unwrap())
        })
        .collect();
    assert_eq!(
        priced.len(),
        3,
        "every repository priced something: {priced:?}"
    );
    for label in &priced {
        assert!(
            labels.contains(label),
            "the bill spells a repository the control cannot select: {label}",
        );
    }
    // And the one that provoked the fix carries *both* halves under one label: the session the
    // coaching fold placed, and the money the cost fold priced.
    assert_eq!(scope_of(&payload, scrubbed)["sessions"]["folded"], 1);
    assert!(
        priced.contains(scrubbed),
        "the scrubbed label has no row on the bill: {priced:?}",
    );
}

/// The same discipline on the other label a scope control is built from — and the reason it turns
/// out to be belt-and-braces rather than a hole.
///
/// A harness label reaches the control only through a session the fold *folded*, and folding one
/// requires an interpreter: `metrics::source_for_agent` is an allowlist, so a harness the archive
/// invented is skipped and named as a gap rather than becoming a column. **An arbitrary harness
/// string can therefore never be an option in the scope select at all**, which is a stronger
/// property than scrubbing it would be.
///
/// It is scrubbed anyway ([`qanungo::evidence::identifier_field`], the ordering the anchor's tool
/// name settled), for the reason that ordering exists: the label is now option text and the key an
/// evidence tag is matched against, and it costs nothing. What this test pins is the equality that
/// follows — the control's vocabulary, the lane columns, the fleet roster, both `by_harness` maps,
/// the per-scope finding splits, and the evidence tags all spell a harness the same way. One
/// harness, one string, or a control and the rows it narrows come apart.
///
/// The gaps lines are deliberately **not** covered by this: `command::summarize` clamps without
/// scrubbing and is shared verbatim with the CLI's own Gaps section, so an archive-invented harness
/// label is rendered there as the archive wrote it. That is a pre-existing property of main, it is
/// prose rather than a control, and tightening it means giving the coaching and cost folds a
/// redactor and re-proving the CLI's bytes — its own change, not this one's.
#[test]
fn a_harness_is_spelled_the_same_way_everywhere_and_an_invented_one_never_becomes_an_option() {
    let clean = transcript("rules/marathon-session.jsonl");
    let invented = "ghp_FAKEfake0123456789ABCDEFabcdef012345";
    let mut sessions: Vec<ArchivedSession> = (0..6u8)
        .map(|index| {
            ArchivedSession::new(index, "claude-code", &clean, 24).in_repository("surdy/qanungo")
        })
        .collect();
    sessions.extend((6..9u8).map(|index| {
        ArchivedSession::new(index, invented, &clean, 25).in_repository("surdy/qanungo")
    }));
    let (address, _directory) = spawn_dashboard(sessions);
    let payload = payload_of(address);

    let vocabulary: Vec<String> = payload["scopes"]["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|label| label.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(vocabulary, vec!["claude-code".to_owned()]);
    assert!(
        payload["provenance"]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["reason"]
                .as_str()
                .unwrap()
                .contains("no interpreter for this harness")),
        "the sessions it could not read are named, not dropped",
    );

    // Every other place a harness is named uses the control's own spelling.
    let named = |value: &serde_json::Value| -> Vec<String> {
        value
            .as_object()
            .expect("a by_harness map")
            .keys()
            .map(ToOwned::to_owned)
            .collect()
    };
    let scope = scope_of(&payload, "surdy/qanungo");
    for (where_, labels) in [
        (
            "window by_harness",
            named(&payload["sessions"]["by_harness"]),
        ),
        ("scope by_harness", named(&scope["sessions"]["by_harness"])),
    ] {
        assert!(!labels.is_empty(), "{where_} is empty");
        for label in labels {
            assert!(vocabulary.contains(&label), "{where_}: {label}");
        }
    }
    let empty = Vec::new();
    for lanes in [&payload["lanes"], &scope["lanes"]] {
        for lane in lanes.as_array().unwrap() {
            for column in lane["harnesses"].as_array().unwrap() {
                let label = column["source_agent"].as_str().unwrap().to_owned();
                assert!(vocabulary.contains(&label), "lane column: {label}");
            }
            for blended in lane["fleet"]["harnesses"].as_array().unwrap_or(&empty) {
                let label = blended.as_str().unwrap().to_owned();
                assert!(vocabulary.contains(&label), "fleet roster: {label}");
            }
        }
    }
    for row in scope["findings"].as_array().unwrap() {
        for label in named(&row["by_harness"]) {
            assert!(vocabulary.contains(&label), "scope finding split: {label}");
        }
    }
    for finding in payload["findings"].as_array().unwrap() {
        for evidence in finding["evidence"].as_array().unwrap() {
            let label = evidence["harness"].as_str().unwrap().to_owned();
            assert!(vocabulary.contains(&label), "evidence tag: {label}");
        }
    }
}

/// The scopes section is bounded by the shape of the window rather than by its size.
///
/// It is one cell per (repository × lane × harness-or-fleet), so it grows with how many
/// repositories were worked in and not with how many sessions or how much prose the window holds.
/// The assertion is a per-cell ceiling rather than a share of the body, because the share depends
/// on how much narrative the standup section happens to carry — against production on 2026-08-25 a
/// cell was about 650 bytes and the whole section 105.5 KiB of a 1070 KiB payload, which is the
/// measurement in the module docs. The ceiling here is that number with room over it: it is meant
/// to catch a cell that started carrying something it should not, not to pin a byte count.
#[test]
fn the_scopes_section_is_bounded_by_the_number_of_scopes() {
    let (address, _directory) = spawn_dashboard(scoped_archive());
    let (_head, body) = request(address, "/api/data");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("json");
    let scopes = serde_json::to_vec(&payload["scopes"]).expect("the section reserializes");
    let count = payload["scopes"]["repositories"].as_array().unwrap().len();
    let harnesses = payload["scopes"]["harnesses"].as_array().unwrap().len();
    assert_eq!(count, 3);
    // The section is one cell per (repository × lane × harness-or-fleet), and what is bounded is
    // the cell: a window with more repositories in it costs proportionally more and nothing else.
    let cells = count * usize::from(LANES) * (harnesses + 1);
    let per_cell = scopes.len() / cells;
    assert!(
        per_cell < 900,
        "a scope cell costs {per_cell} bytes; the section is {} of a {} byte body",
        scopes.len(),
        body.len(),
    );
    // And it is a section, not the document: whatever else grows, the scopes never become most of
    // the payload on a window with a narrative in it.
    assert!(
        scopes.len() < body.len(),
        "{} of {}",
        scopes.len(),
        body.len(),
    );
}

/// The page's half of the slice: one control, wired to the payload's own scopes, computing nothing.
///
/// There is no JavaScript engine in this harness, so what is pinned here is the shape rather than
/// the behaviour: the control exists, it reads the payload's scope section, its narrowing is a
/// comparison of server-written labels, and every `localStorage` access is inside a `try`. The
/// behaviour itself was verified in Chrome against production — see the issue comment.
#[test]
fn the_page_carries_one_scope_control_and_scores_nothing_itself() {
    let (address, _directory) = spawn_dashboard(scoped_archive());
    let (head, body) = request(address, "/");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");

    for anchor in [
        r#"id="scope-repository""#,
        r#"id="scope-device""#,
        r#"id="scope-harness""#,
        r#"id="scope-note""#,
        "data.scopes.repositories",
        "data.scopes.devices",
        "data.scopes.harnesses",
        "citedInScope",
        "scopedCostSection",
        "scopedStandupSection",
    ] {
        assert!(body.contains(anchor), "the page has no {anchor}");
    }

    // Narrowing is a string comparison against labels the server wrote, never a rule re-evaluated
    // here — the payload is the single source of every number on the page. Both primary axes narrow
    // the findings this way.
    for narrowing in [
        "evidence.repository !== scope.repository",
        "evidence.device !== scope.device",
    ] {
        assert!(
            body.contains(narrowing),
            "findings are narrowed by the payload's own tags: {narrowing}",
        );
    }

    // Repository and device are exclusive primary axes: choosing one clears the other, so the
    // payload never has to carry a repository × device cell no fold produced.
    for exclusivity in ["scope.device = null;", "scope.repository = null;"] {
        assert!(
            body.contains(exclusivity),
            "the two primary axes clear each other: {exclusivity}",
        );
    }

    // Remembering the scope is a convenience, so every access to storage is guarded and a page with
    // nothing stored renders the whole window.
    assert_eq!(
        body.matches("localStorage").count(),
        2,
        "one read and one write, and nowhere else",
    );
    for guarded in [
        "window.localStorage.getItem(SCOPE_KEY)",
        "window.localStorage.setItem(SCOPE_KEY",
    ] {
        let at = body.find(guarded).expect("the access is there");
        let before = &body[..at];
        let opened = before.rfind("try {").expect("an access outside a try");
        let closed = before.rfind("} catch").map_or(0, |index| index);
        assert!(opened > closed, "{guarded} is not inside a try");
    }
    assert!(
        body.contains("const empty = { repository: null, device: null, harness: null };")
            && body.contains("if (!raw) return empty;"),
        "nothing stored renders the whole window",
    );

    // And every invariant the page already held, restated over the grown file.
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
    // Changing the scope re-renders what is already in hand: no query string, no second route.
    assert_eq!(
        body.matches("fetch(").count(),
        3,
        "the payload, an excerpt and a search, and nothing a scope control adds",
    );
    assert!(
        !body.contains("?repository=") && !body.contains("/api/data?"),
        "a scope is never a query string",
    );
    assert!(
        !body.contains("/api/ask?q=\" + encodeURIComponent(typed) + \"&repository"),
        "and a scope is never a search parameter either",
    );
}

/// The control narrows everything the page shows, which means the scope list has to be the
/// **union** of what all three sections labelled — and the three have to spell an ordinary
/// repository the same way, or an option would narrow one section and not the others.
///
/// They reach the label by three different routes: the coaching scope clamps then scrubs, the cost
/// lane clamps, and the standup lane scrubs then clamps — and they group by two different facts,
/// the archive's projection onto the listed snapshot versus the repository a session's own
/// `summary.md` names. What this pins is that an ordinary name survives all three identically, so
/// the disagreement that remains is about *which sessions* a repository holds and never about how
/// it is written.
#[test]
fn the_scope_list_is_the_union_of_what_all_three_sections_labelled() {
    let (address, _directory) = spawn_dashboard(three_lane_archive());
    let payload = payload_of(address);
    let labels: Vec<&str> = payload["scopes"]["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["repository"].as_str().unwrap())
        .collect();

    for row in payload["cost"]["by_repository"].as_array().unwrap() {
        let label = row["repository"]
            .as_str()
            .unwrap_or_else(|| payload["scopes"]["unattributed"].as_str().unwrap());
        assert!(
            labels.contains(&label),
            "the bill names {label}: {labels:?}"
        );
    }
    for group in payload["standup"]["repositories"].as_array().unwrap() {
        let label = group["repository"].as_str().unwrap();
        assert!(
            labels.contains(&label),
            "the narrative names {label}: {labels:?}",
        );
    }
    assert!(labels.contains(&"surdy/qanungo") && labels.contains(&"surdy/munshi"));
    // The unattributed bucket is one bucket across all three, spelled once by the payload.
    assert_eq!(
        labels
            .iter()
            .filter(|label| **label == "no repository recorded")
            .count(),
        1,
        "{labels:?}",
    );
}

/// A repository the *coaching* fold never saw is still a scope, because the narrative names it —
/// and a control that could not select a repository the page visibly renders would be a control
/// that lies about what it narrows. It scores nothing, and the payload says so rather than
/// inventing a reading.
///
/// This is the two-facts case in one fixture. Both sessions are **listed** under `surdy/qanungo`,
/// which is what the coaching scopes and the bill cut by; the second one's own `summary.md` names
/// `surdy/munshi`, which is what the standup groups by. Neither reading is wrong and the page must
/// not pretend they are the same, so the scope list holds both labels.
#[test]
fn a_repository_only_the_narrative_names_is_still_a_scope_with_nothing_to_score() {
    let clean = transcript("rules/marathon-session.jsonl");
    let sessions = vec![
        ArchivedSession::new(1, "claude-code", &clean, 24)
            .with_summary(&summary("qanungo-cost.md"))
            .in_repository("surdy/qanungo"),
        ArchivedSession::new(2, "claude-code", &clean, 25)
            .with_summary(&summary("munshi-tombstone.md"))
            .in_repository("surdy/qanungo"),
    ];
    let (address, _directory) = spawn_dashboard(sessions);
    let payload = payload_of(address);

    // The archive listed both sessions in one repository; the summaries name two.
    assert_eq!(scope_of(&payload, "surdy/qanungo")["sessions"]["folded"], 2);
    let narrated: Vec<&str> = payload["standup"]["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| group["repository"].as_str().unwrap())
        .collect();
    assert_eq!(narrated, vec!["surdy/munshi", "surdy/qanungo"]);

    let scope = scope_of(&payload, "surdy/munshi");
    assert_eq!(scope["sessions"]["folded"], 0);
    assert_eq!(scope["sessions"]["comparison_folded"], 0);
    for lane in scope["lanes"].as_array().unwrap() {
        assert_eq!(lane["fleet"]["state"], "no-reading", "{}", lane["key"]);
        for column in lane["harnesses"].as_array().unwrap() {
            assert_eq!(column["state"], "no-sessions", "{}", lane["key"]);
        }
    }
    // Every rule that fired in the window still has a row here, carrying a zero: a missing key and
    // a zero are different statements and only one of them is a reading.
    for row in scope["findings"].as_array().unwrap() {
        assert_eq!(row["sessions_affected"], 0, "{}", row["rule"]);
    }
    // Busiest first puts a repository with no coaching session behind every repository with one.
    let labels: Vec<&str> = payload["scopes"]["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["repository"].as_str().unwrap())
        .collect();
    assert_eq!(labels, vec!["surdy/qanungo", "surdy/munshi"]);
}

// ---------------------------------------------------------------------------
// The timeline (qanungo #5, the last code-gated slice)
// ---------------------------------------------------------------------------

/// A window whose every archive time is an exact instant on a known UTC day, so the day a session
/// lands on is a fact this test decides rather than one the clock decides for it.
///
/// The dates stay relative — `days_ago` — because the window is 30 days wide and this file has to
/// keep working next year; the *times of day* are pinned, because midnight is the only place a day
/// boundary can be got wrong. The two sessions a second apart across a midnight are the whole point:
/// under any other clock, or under a page that re-expressed the day in a reader's own zone, they
/// would share a bar.
///
/// | Day | Sessions | Repository | Harness |
/// | --- | --- | --- | --- |
/// | `D-5` | 2 | `surdy/munshi` | claude-code |
/// | `D-3` 23:59:59 | 1 | `surdy/qanungo` | claude-code |
/// | `D-2` 00:00:00 | 1 | `surdy/qanungo` | claude-code |
/// | `D-2` 12:00:00 | 1 | `surdy/qanungo` | copilot-cli |
///
/// Three days covered, five sessions. The comparison window — `[60d, 30d)` — holds two more on two
/// further days, one in each repository, so a scope's comparison half is a different shape from the
/// window's.
fn dated_archive() -> Vec<ArchivedSession> {
    let clean = transcript("rules/marathon-session.jsonl");
    let copilot = transcript("munshi/copilot-1.0.76-compaction.jsonl");
    vec![
        ArchivedSession::new(1, "claude-code", &clean, 0)
            .completed_on(5, 9, 0, 0)
            .in_repository("surdy/munshi"),
        ArchivedSession::new(2, "claude-code", &clean, 0)
            .completed_on(5, 17, 30, 0)
            .in_repository("surdy/munshi"),
        // One second before midnight, and one second's worth of the next day after it.
        ArchivedSession::new(3, "claude-code", &clean, 0)
            .completed_on(3, 23, 59, 59)
            .in_repository("surdy/qanungo"),
        ArchivedSession::new(4, "claude-code", &clean, 0)
            .completed_on(2, 0, 0, 0)
            .in_repository("surdy/qanungo"),
        ArchivedSession::new(5, "copilot-cli", &copilot, 0)
            .completed_on(2, 12, 0, 0)
            .in_repository("surdy/qanungo"),
        // The comparison window, on two days of its own.
        ArchivedSession::new(6, "claude-code", &clean, 0)
            .completed_on(41, 10, 0, 0)
            .in_repository("surdy/qanungo"),
        ArchivedSession::new(7, "claude-code", &clean, 0)
            .completed_on(44, 10, 0, 0)
            .in_repository("surdy/munshi"),
    ]
}

/// The date this fixture's `days_ago` names, spelled the way the payload spells it.
fn day_ago(days: i64) -> String {
    (Utc::now() - TimeDelta::days(days))
        .date_naive()
        .to_string()
}

/// One day row out of a timeline's list, by date.
fn day_in<'a>(days: &'a serde_json::Value, date: &str) -> &'a serde_json::Value {
    days.as_array()
        .expect("a day list")
        .iter()
        .find(|day| day["date"] == date)
        .unwrap_or_else(|| panic!("no {date} in {days}"))
}

/// Every session in one day row, across the harness columns.
fn sessions_on(day: &serde_json::Value) -> u64 {
    day["sessions"]
        .as_array()
        .expect("one count per harness column")
        .iter()
        .map(|count| count.as_u64().expect("a count"))
        .sum()
}

/// Every session in one day list.
fn sessions_over(days: &serde_json::Value) -> u64 {
    days.as_array()
        .expect("a day list")
        .iter()
        .map(sessions_on)
        .sum()
}

/// The slice's own claim: the window laid on UTC days of **archive completion**, split by harness,
/// with the two halves of the window pair kept apart.
///
/// The midnight pair is what this is really about. Two sessions one second apart are two bars,
/// because a UTC day ends at `23:59:59Z` and the next one starts at `00:00:00Z` — and because the
/// clock is the archive's completion time, which is the clock the window itself was cut on.
#[test]
fn the_timeline_lays_the_window_on_utc_days_of_archive_time() {
    let (address, _directory) = spawn_dashboard(dated_archive());
    let payload = payload_of(address);
    let timeline = &payload["timeline"];

    // Three days in the reported window, earliest first, and no day for a session that never
    // happened: a quiet day is a gap in the axis, not a row of zeroes on the wire.
    let dates: Vec<&str> = timeline["days"]
        .as_array()
        .expect("a day list")
        .iter()
        .map(|day| day["date"].as_str().expect("an ISO date"))
        .collect();
    assert_eq!(dates, vec![day_ago(5), day_ago(3), day_ago(2)]);
    assert_eq!(timeline["days_covered"], 3);
    assert_eq!(timeline["undated"], 0);

    // The midnight pair: one second apart, two days, one session each.
    assert_eq!(sessions_on(day_in(&timeline["days"], &day_ago(3))), 1);
    assert_eq!(sessions_on(day_in(&timeline["days"], &day_ago(2))), 2);
    assert_eq!(sessions_on(day_in(&timeline["days"], &day_ago(5))), 2);

    // Split by harness, positionally against the payload's one harness axis.
    let harnesses: Vec<&str> = payload["scopes"]["harnesses"]
        .as_array()
        .expect("the harness axis")
        .iter()
        .map(|label| label.as_str().unwrap())
        .collect();
    assert_eq!(harnesses, vec!["claude-code", "copilot-cli"]);
    let busiest = day_in(&timeline["days"], &day_ago(2));
    assert_eq!(busiest["sessions"], serde_json::json!([1, 1]));
    // A harness that worked nothing that day is a zero at its own column and never a missing one,
    // so the page can stack a column without asking which keys exist.
    assert_eq!(
        day_in(&timeline["days"], &day_ago(5))["sessions"],
        serde_json::json!([2, 0]),
    );

    // The comparison half is its own list on its own days.
    assert_eq!(timeline["comparison_days_covered"], 2);
    let comparison: Vec<&str> = timeline["comparison_days"]
        .as_array()
        .expect("a day list")
        .iter()
        .map(|day| day["date"].as_str().unwrap())
        .collect();
    assert_eq!(comparison, vec![day_ago(44), day_ago(41)]);
    assert_eq!(sessions_over(&timeline["comparison_days"]), 2);

    // And the footer quotes what the chart draws, rather than counting it a second time.
    let provenance = &payload["provenance"]["timeline"];
    assert_eq!(provenance["basis"], "archive-completion-utc");
    assert_eq!(provenance["days_covered"], 3);
    assert_eq!(provenance["comparison_days_covered"], 2);
    assert_eq!(provenance["undated"], 0);
}

/// The reconciliation the whole view rests on: **the bars add up to the number above them**.
///
/// In both halves of the window pair, in the whole window and inside every scope. That is only
/// possible because the day is taken on archive time — the clock `Placement` cut the windows on —
/// and it is the reason the timeline could ship while the heatmap could not.
#[test]
fn every_days_counts_sum_to_the_windows_own_session_count() {
    let (address, _directory) = spawn_dashboard(dated_archive());
    let payload = payload_of(address);

    let folded = payload["sessions"]["folded"].as_u64().expect("a count");
    assert_eq!(folded, 5);
    assert_eq!(
        sessions_over(&payload["timeline"]["days"])
            + payload["timeline"]["undated"].as_u64().unwrap(),
        folded,
    );
    assert_eq!(
        sessions_over(&payload["timeline"]["comparison_days"])
            + payload["timeline"]["comparison_undated"].as_u64().unwrap(),
        payload["sessions"]["comparison_folded"].as_u64().unwrap(),
    );

    // Every scope, against its own two counts — the numbers the page's own sentence quotes.
    let scopes = payload["scopes"]["repositories"]
        .as_array()
        .expect("the scopes section lists repositories");
    assert!(!scopes.is_empty());
    for scope in scopes {
        let timeline = &scope["timeline"];
        let label = &scope["repository"];
        assert_eq!(
            sessions_over(&timeline["days"]) + timeline["undated"].as_u64().unwrap(),
            scope["sessions"]["folded"].as_u64().unwrap(),
            "{label} draws a different number of sessions from the one it counts",
        );
        assert_eq!(
            sessions_over(&timeline["comparison_days"])
                + timeline["comparison_undated"].as_u64().unwrap(),
            scope["sessions"]["comparison_folded"].as_u64().unwrap(),
            "{label}'s comparison half does not reconcile",
        );
        assert_eq!(
            timeline["days_covered"],
            timeline["days"].as_array().unwrap().len()
        );
    }

    // And the scopes partition the window: every scope's day counts, added up per day, are the
    // whole window's. A scope control that showed more sessions than the window holds — or fewer —
    // would be narrowing to something other than a subset.
    let mut per_day: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for scope in scopes {
        for day in scope["timeline"]["days"].as_array().unwrap() {
            *per_day
                .entry(day["date"].as_str().unwrap().to_owned())
                .or_default() += sessions_on(day);
        }
    }
    let whole: std::collections::BTreeMap<String, u64> = payload["timeline"]["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|day| (day["date"].as_str().unwrap().to_owned(), sessions_on(day)))
        .collect();
    assert_eq!(per_day, whole);
}

/// Narrowing to a repository narrows the calendar with it, and the narrowed calendar is the same
/// grouping of the same sessions — never a second fold, and never a day the scope did not work.
#[test]
fn a_repository_scope_draws_only_its_own_days() {
    let (address, _directory) = spawn_dashboard(dated_archive());
    let payload = payload_of(address);

    let qanungo = scope_of(&payload, "surdy/qanungo")["timeline"].clone();
    let dates: Vec<&str> = qanungo["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|day| day["date"].as_str().unwrap())
        .collect();
    assert_eq!(dates, vec![day_ago(3), day_ago(2)]);
    assert_eq!(sessions_over(&qanungo["days"]), 3);
    // Both harnesses on the busy day; the columns are the payload's own axis, in every scope.
    assert_eq!(
        day_in(&qanungo["days"], &day_ago(2))["sessions"],
        serde_json::json!([1, 1]),
    );

    let munshi = scope_of(&payload, "surdy/munshi")["timeline"].clone();
    let dates: Vec<&str> = munshi["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|day| day["date"].as_str().unwrap())
        .collect();
    assert_eq!(dates, vec![day_ago(5)]);
    assert_eq!(munshi["days_covered"], 1);
    // A repository's comparison half is its own, and is a different day from the other's.
    assert_eq!(
        munshi["comparison_days"].as_array().unwrap()[0]["date"],
        day_ago(44),
    );
    assert_eq!(
        qanungo["comparison_days"].as_array().unwrap()[0]["date"],
        day_ago(41),
    );
}

/// Active time on the calendar is the fold's **own** gap-aware number, summed per day — the same
/// seconds the structural evidence block renders and the rules reason about, never a wall-clock
/// span and never a second measurement.
#[test]
fn active_time_per_day_is_the_folds_own_active_time() {
    let (address, args, _directory) = spawn_with(dated_archive(), &[]);
    let payload = payload_of(address);
    let folded = command::fold_coaching(&args.archive, &args.last, &args.redaction.redactor())
        .expect("the window folds");

    let mut expected: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for session in &folded.sessions {
        let day = session
            .archive_day()
            .expect("the fixture dates every session");
        *expected.entry(day.to_string()).or_default() += session
            .active_time()
            .map_or(0, |active| active.num_seconds());
    }
    let served: std::collections::BTreeMap<String, i64> = payload["timeline"]["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|day| {
            (
                day["date"].as_str().unwrap().to_owned(),
                day["active_seconds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|seconds| seconds.as_i64().unwrap())
                    .sum(),
            )
        })
        .collect();
    assert_eq!(served, expected);
    assert!(
        served.values().any(|seconds| *seconds > 0),
        "the fixture must have some activity to sum",
    );
}

/// The section's hard invariant: **numbers and ISO dates, and not one string besides**.
///
/// Not even a harness label. The per-day arrays are positional against `scopes.harnesses`, the
/// payload's one harness axis, so there is no place in this section for a byte the archive wrote to
/// hide — which is a stronger claim than "it is scrubbed", and the same one the structural evidence
/// block already makes. The walk runs over the top-level timeline and over every scope's, because a
/// section repeated per repository is a section that can go wrong per repository.
#[test]
fn the_timeline_section_is_numbers_and_dates_and_nothing_else() {
    let (address, _directory) = spawn_dashboard(dated_archive());
    let payload = payload_of(address);

    let mut leaves = 0;
    assert_timeline_leaves(&payload["timeline"], "timeline", &mut leaves);
    for scope in payload["scopes"]["repositories"].as_array().unwrap() {
        assert_timeline_leaves(&scope["timeline"], "scope timeline", &mut leaves);
    }
    assert!(leaves > 40, "the walk visited only {leaves} leaves");

    // And the width of every row is the harness axis, in every scope — which is what makes a
    // positional array readable at all.
    let harnesses = payload["scopes"]["harnesses"].as_array().unwrap().len();
    let mut rows = 0;
    for timeline in std::iter::once(&payload["timeline"]).chain(
        payload["scopes"]["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|scope| &scope["timeline"]),
    ) {
        for key in ["days", "comparison_days"] {
            for day in timeline[key].as_array().unwrap() {
                assert_eq!(day["sessions"].as_array().unwrap().len(), harnesses);
                assert_eq!(day["active_seconds"].as_array().unwrap().len(), harnesses);
                rows += 1;
            }
        }
    }
    assert!(rows > 0, "the fixture drew no day at all");
}

/// Asserts that every leaf under a timeline block is a non-negative number or an ISO `YYYY-MM-DD`
/// date. Recurses, because the interesting places for a string to hide are the day rows.
///
/// The date grammar is pinned rather than waved at: a day here is a calendar day and carries no
/// time and no zone, which is exactly what distinguishes it from the RFC 3339 instants the rest of
/// the payload is stamped with.
fn assert_timeline_leaves(value: &serde_json::Value, path: &str, leaves: &mut usize) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, field) in fields {
                assert_timeline_leaves(field, &format!("{path}.{key}"), leaves);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                assert_timeline_leaves(item, &format!("{path}[{index}]"), leaves);
            }
        }
        serde_json::Value::Number(number) => {
            *leaves += 1;
            assert!(
                number.as_i64().is_some_and(|value| value >= 0),
                "{path} is {number}, which is not a count",
            );
        }
        serde_json::Value::String(text) => {
            *leaves += 1;
            assert!(is_calendar_day(text), "{path} carries the string {text:?}");
        }
        other => panic!("{path} is {other}, which the timeline never serves"),
    }
}

/// `2026-08-11` — a calendar day, with no time of day and no zone on it.
fn is_calendar_day(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && text
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && text.parse::<chrono::NaiveDate>().is_ok()
}

/// The grammar the walk enforces, stated against values that must fail it — otherwise the walk is a
/// loop that visits leaves and asserts nothing.
#[test]
fn the_calendar_day_grammar_admits_a_date_and_nothing_else() {
    for good in ["2026-08-11", "2026-01-01", "2024-02-29"] {
        assert!(is_calendar_day(good), "{good} is a calendar day");
    }
    for bad in [
        // An instant, which is what every other timestamp on this page is — and is not a day.
        "2026-08-11T09:00:00Z",
        "2026-08-11 09:00",
        // A date that does not exist, and one that is not one at all.
        "2026-02-30",
        "2026-8-11",
        "surdy/qanungo",
        "claude-code",
        "",
    ] {
        assert!(!is_calendar_day(bad), "{bad:?} is not a calendar day");
    }
}

/// The section is bounded by **what happened**, not by how long the window is.
///
/// A day nothing was archived on is not served, so the cost of a scope's calendar is one row per
/// day that scope actually worked — which is bounded above by that scope's session count, and
/// therefore the whole section by the window's. A dense day × harness × scope grid would instead
/// cost the window's *length* times the roster times the repository count, for every quiet day in
/// it, which on a 28-repository production window is most of the section being zeroes.
#[test]
fn the_timeline_section_is_bounded_by_the_days_that_happened() {
    let (address, _directory) = spawn_dashboard(dated_archive());
    let (_head, body) = request(address, "/api/data");
    let payload: serde_json::Value = serde_json::from_str(&body).expect("json");

    let mut rows = 0;
    let mut bytes = serde_json::to_vec(&payload["timeline"])
        .expect("it reserializes")
        .len();
    for day in payload["timeline"]["days"]
        .as_array()
        .unwrap()
        .iter()
        .chain(payload["timeline"]["comparison_days"].as_array().unwrap())
    {
        let _ = day;
        rows += 1;
    }
    for scope in payload["scopes"]["repositories"].as_array().unwrap() {
        bytes += serde_json::to_vec(&scope["timeline"])
            .expect("it reserializes")
            .len();
        for key in ["days", "comparison_days"] {
            rows += scope["timeline"][key].as_array().unwrap().len();
        }
    }

    // No scope draws more days than it has sessions, in either half.
    for scope in payload["scopes"]["repositories"].as_array().unwrap() {
        let timeline = &scope["timeline"];
        assert!(
            timeline["days_covered"].as_u64().unwrap()
                <= scope["sessions"]["folded"].as_u64().unwrap(),
            "{} draws more days than it folded sessions",
            scope["repository"],
        );
    }

    // And a row is small, because it is two integer arrays and a date: the ceiling is that number
    // with room over it, meant to catch a row that started carrying something it should not.
    let per_row = bytes / rows;
    assert!(
        per_row < 220,
        "a timeline day costs {per_row} bytes over {rows} rows; the section is {bytes} of a {} byte body",
        body.len(),
    );
}

/// The page's half of the slice: one inline-SVG chart, drawn from the payload's own integers,
/// computing nothing and fetching nothing.
///
/// There is no JavaScript engine in this harness, so what is pinned here is the shape — the chart
/// exists, it reads the timeline section, the measure is a toggle rather than a second y-axis, the
/// legend and the table view are both there, and every page invariant survives the growth. The
/// drawing itself was checked in Chrome, light and dark, against production — see the issue comment.
#[test]
fn the_page_draws_one_inline_svg_chart_and_computes_nothing() {
    let (address, _directory) = spawn_dashboard(dated_archive());
    let (head, body) = request(address, "/");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");

    for anchor in [
        r#"id="timeline-heading""#,
        r#"id="timeline-chart""#,
        r#"id="timeline-measure""#,
        r#"id="timeline-legend""#,
        r#"id="timeline-note""#,
        r#"id="timeline-table""#,
        "<svg",
        "entry.timeline : data.timeline",
        "timeline.comparison_days",
        "paintTimeline(data, entry, column)",
    ] {
        assert!(body.contains(anchor), "the page has no {anchor}");
    }

    // The chart is built into the element the document already carries, so the SVG namespace is
    // read off a real node instead of being written here as a URL — which is what keeps the "loads
    // nothing from anywhere" check below a grep rather than a judgement call.
    assert!(
        body.contains("chart.namespaceURI"),
        "the namespace is taken from the document",
    );
    assert!(
        !body.contains("createElement(\"svg\")"),
        "an HTML element named svg is not an SVG element",
    );

    // One chart, one axis. The two measures are a toggle: two y-scales on one plot would invent a
    // correlation that is not in the data, and the page says so where a reader of it will look.
    assert!(
        body.contains(r#"<option value="active">Active hours per day</option>"#),
        "active time is a measure of the same chart",
    );
    assert!(
        body.contains("never two scales on one plot"),
        "the page states why the second measure is a toggle",
    );

    // The page says what a day is, on the page, rather than only in the crate's docs.
    assert!(
        body.contains("A day is the UTC calendar day the archive finished the session's snapshot"),
        "the chart names its own clock",
    );
    assert!(
        body.contains("no hour-of-day or day-of-week view here"),
        "the page says which view the missing offset still blocks",
    );

    // Colour follows the harness's place in the payload's own axis, never its rank among whatever
    // a filter left behind — and a series past the palette's third slot is de-emphasised rather
    // than given a hue nobody validated.
    assert!(
        body.contains(r#"index < 3 ? "s" + index : "sx""#),
        "a colour slot is the harness's index in the payload's axis",
    );

    // Every invariant the page already held, restated over the grown file.
    assert!(!body.contains("href"), "the page carries no links at all");
    assert!(!body.contains("<a "), "the page carries no anchors");
    assert!(
        !body.contains("innerHTML"),
        "every value is set as text, never parsed as markup",
    );
    assert!(
        !body.contains("http://") && !body.contains("https://") && !body.contains("//fonts."),
        "the page loads nothing from anywhere — the SVG namespace included",
    );
    assert_eq!(
        body.matches("fetch(").count(),
        3,
        "the payload, an excerpt and a search, and nothing the chart adds",
    );
}

// ---------------------------------------------------------------------------
// The harness label on a Gaps line
// ---------------------------------------------------------------------------

/// A GitHub token's shape — `ghp_` and exactly 36 base62 characters — and not a real one.
///
/// The point of the fixture is that it is *renderable*: no pipe, no backtick, no control
/// character, well under the identifier clamp's ceiling. `qanungo::format::identifier` hands it
/// straight back, which is why the clamp alone never protected this line.
const TOKEN_SHAPED_AGENT: &str = "ghp_FAKEfake0123456789ABCDEFabcdef012345";

/// A window whose second session's manifest names a credential-shaped harness.
///
/// That session is a gap in every lane at once, which is what makes one archive enough for all four
/// surfaces: no build has an interpreter for this "harness", so the transcript lanes skip it before
/// they fold, and no snapshot of it carries a `summary.md`, so the standup lane never narrates it.
/// Its label is the only thing about it that reaches a page — three times over, plus the payload.
fn hostile_label_archive() -> Vec<ArchivedSession> {
    vec![
        ArchivedSession::new(
            31,
            "claude-code",
            &transcript("cost/claude-billing.jsonl"),
            2,
        )
        .with_summary(&summary("qanungo-cost.md"))
        .in_repository("surdy/qanungo"),
        ArchivedSession::new(
            32,
            TOKEN_SHAPED_AGENT,
            &transcript("cost/copilot-billing.jsonl"),
            3,
        ),
    ]
}

/// Runs one document lane end to end against a stand-in archive and hands back its Markdown.
///
/// Through `Cli::parse_from` and the real command function, so the redactor each lane renders with
/// is the one it builds for itself — which for `report` and `cost` is the whole property under
/// test: neither flattens `RedactionArgs`, so there is no flag in the line below to have set.
fn document(lane: &str, base: &str, cache: &std::path::Path) -> String {
    let parsed = Cli::parse_from([
        "qanungo",
        lane,
        "--patwari-url",
        base,
        "--cache-dir",
        cache.to_str().expect("a utf-8 scratch path"),
    ])
    .command;
    let mut out = Vec::new();
    match &parsed {
        Command::Report(args) => command::report(args, &mut out).expect("the report renders"),
        Command::Cost(args) => command::cost(args, &mut out).expect("the cost report renders"),
        Command::Standup(args) => command::standup(args, &mut out).expect("the standup renders"),
        Command::Doctor(args) => command::doctor(args, &mut out).expect("the doctor renders"),
        // Ask is a document lane too, but this helper drives the three windowed lanes by name and
        // never supplies the query ask requires, so a run reaching here is a test wiring mistake.
        Command::Ask(_) => panic!("`{lane}` is not driven by this helper"),
        Command::Dashboard(_) => panic!("`{lane}` is not a document lane"),
    }
    String::from_utf8(out).expect("a document is UTF-8")
}

/// The gap this closes: a credential-shaped `source_agent` in a listing reached the Gaps section of
/// all three documents and the dashboard's `provenance.gaps` as itself, because the label was
/// clamped and never scrubbed.
///
/// All four surfaces are checked from one archive because all four render the same `SkippedNote`,
/// and the whole point of treating the label in the fold rather than at each rendering site is that
/// they cannot come apart. The negative half is the mutation guard: swap `evidence::identifier_field`
/// back to `format::identifier` and the token is present in every document below.
#[test]
fn a_credential_shaped_harness_label_is_scrubbed_in_every_gaps_section() {
    const MARKER: &str = "[REDACTED:github-token]";

    let base = spawn_archive(hostile_label_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let cache = directory.path().join("qanungo");

    for lane in ["report", "cost", "standup", "doctor"] {
        let rendered = document(lane, &base, &cache);
        assert!(
            !rendered.contains(TOKEN_SHAPED_AGENT),
            "`{lane}` printed the token: {rendered}",
        );
        assert!(
            rendered.contains(MARKER),
            "`{lane}` has no marker where the label belongs: {rendered}",
        );
    }

    // The served surface, over its own archive and its own fold.
    let (address, _args, _directory) = spawn_with(hostile_label_archive(), &[]);
    let payload = payload_of(address);
    let gaps = payload["provenance"]["gaps"].to_string();
    assert!(
        gaps.contains(MARKER) && !gaps.contains(TOKEN_SHAPED_AGENT),
        "provenance.gaps: {gaps}",
    );
    assert!(
        !payload.to_string().contains(TOKEN_SHAPED_AGENT),
        "the token reached the payload somewhere else",
    );
}

/// An ordinary harness label costs the common case nothing: the scrub is a no-op on it, the clamp
/// is a no-op on it, and every document names the harness exactly as the archive did.
///
/// The standup document is checked on its Gaps line rather than on the whole page, because that
/// lane's fixtures carry planted credentials in their *prose* and the markers those produce are the
/// scrub working as intended.
#[test]
fn an_ordinary_harness_label_is_unchanged_by_the_scrub() {
    let base = spawn_archive(three_lane_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let cache = directory.path().join("qanungo");

    for lane in ["report", "cost"] {
        let rendered = document(lane, &base, &cache);
        assert!(
            !rendered.contains("[REDACTED:"),
            "`{lane}` scrubbed something in a window of ordinary labels: {rendered}",
        );
        assert!(
            !rendered.contains(qanungo::format::INVALID_IDENTIFIER),
            "`{lane}` clamped something in a window of ordinary labels: {rendered}",
        );
        assert!(rendered.contains("claude-code"), "`{lane}`: {rendered}");
    }

    // The one lane with a gap to name here: the copilot session carries no `summary.md`.
    let standup = document("standup", &base, &cache);
    assert!(
        standup.contains("copilot-cli: no snapshot of this session carries a `summary.md`"),
        "the gap line names the harness the archive named: {standup}",
    );
}

/// The per-device scope: the same fold cut by the host the manifest recorded, mirroring the
/// repository scope. The list is busiest-first with the unrecorded bucket last, a hostile hostname
/// is clamped before it is ever an option, and each device scope's numbers reconcile to the sessions
/// the selection holds — a device scope is a *selection* of the fold, never a second one.
#[test]
fn the_device_scope_groups_the_window_by_the_host_the_manifest_recorded() {
    let base = spawn_archive(device_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let args = args(&base, &directory.path().join("qanungo"));
    let dashboard = Dashboard::start(&args).expect("the first fold");
    let address = dashboard.address();
    std::thread::spawn(move || dashboard.serve());

    let payload = payload_of(address);
    let devices = payload["scopes"]["devices"]
        .as_array()
        .expect("a device axis");

    // Busiest first, then the singles by label, then the unrecorded residue last. The hostile
    // hostname is on the wire as the clamp's marker, never as its raw bytes.
    let labels: Vec<&str> = devices
        .iter()
        .map(|scope| scope["device"].as_str().expect("a device label"))
        .collect();
    let unattributed = payload["scopes"]["unattributed_device"].as_str().unwrap();
    assert_eq!(
        labels,
        vec![
            "macbookpro",
            qanungo::format::INVALID_IDENTIFIER,
            "j2vjcmqmyx",
            unattributed,
        ],
    );
    assert_eq!(unattributed, qanungo::scopes::NO_DEVICE);

    // The residue is honestly unattributed, the busiest device is attributed, and the raw hostile
    // string appears nowhere in the served document.
    assert_eq!(devices[3]["attributed"], false);
    assert_eq!(devices[0]["attributed"], true);
    assert!(
        !payload.to_string().contains("host|evil"),
        "the raw hostname is off the wire",
    );

    // Each device's folded count, and they sum to the whole window: the scopes partition it.
    assert_eq!(devices[0]["sessions"]["folded"], 3);
    assert_eq!(devices[1]["sessions"]["folded"], 1);
    assert_eq!(devices[2]["sessions"]["folded"], 1);
    assert_eq!(devices[3]["sessions"]["folded"], 1);
    let summed: u64 = devices
        .iter()
        .map(|scope| scope["sessions"]["folded"].as_u64().unwrap())
        .sum();
    assert_eq!(summed, payload["sessions"]["folded"].as_u64().unwrap());

    // The busiest device carries both harnesses, and its per-harness split says so.
    assert_eq!(devices[0]["sessions"]["by_harness"]["claude-code"], 2);
    assert_eq!(devices[0]["sessions"]["by_harness"]["copilot-cli"], 1);

    // Reconciliation: the macbookpro scope's lanes are exactly `Scorecard::fold_refs` over the
    // folded sessions that ran on macbookpro — the same arithmetic as the whole window, over less.
    let folded = command::fold_coaching(&args.archive, &args.last, &args.redaction.redactor())
        .expect("the window folds");
    let macbook: Vec<_> = folded
        .sessions
        .iter()
        .filter(|session| session.hostname.as_deref() == Some("macbookpro"))
        .collect();
    assert_eq!(macbook.len(), 3);
    let card = Scorecard::fold_refs(&macbook);
    for lane in Lane::ALL {
        let served = devices[0]["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == lane.key())
            .unwrap_or_else(|| panic!("{lane:?} is in the device scope"));
        match card.fleet(lane) {
            Some(blend) => {
                assert_eq!(served["fleet"]["state"], "scored", "{lane:?}");
                assert_eq!(served["fleet"]["score"], blend.score, "{lane:?}");
            }
            None => assert_ne!(served["fleet"]["state"], "scored", "{lane:?}"),
        }
    }

    // Every finding's evidence rows carry a device tag drawn from the same label set, so selecting a
    // device narrows the findings the way selecting a repository does.
    let device_labels: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
    for finding in payload["findings"].as_array().unwrap() {
        for evidence in finding["evidence"].as_array().unwrap() {
            let tag = evidence["device"].as_str().expect("an evidence device tag");
            assert!(device_labels.contains(tag), "unknown device tag {tag}");
        }
    }
}

/// The payload contract the page's heatmap fallback rests on: the whole window carries a `heatmap`,
/// every repository scope carries its own, and a device scope carries **none** — the heatmap is cut
/// by repository, not device, so under a device the page must read `data.heatmap` rather than a key
/// that is not there. Pinned so a change cannot silently reintroduce the crash a device selection
/// once caused (the page has no test of its own; this guards the shape it depends on).
#[test]
fn a_device_scope_carries_no_heatmap_while_the_window_and_repository_scopes_do() {
    let base = spawn_archive(device_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let args = args(&base, &directory.path().join("qanungo"));
    let dashboard = Dashboard::start(&args).expect("the first fold");
    let address = dashboard.address();
    std::thread::spawn(move || dashboard.serve());

    let payload = payload_of(address);

    assert!(
        payload["heatmap"].is_object(),
        "the whole window carries a heatmap: {payload}",
    );
    for repository in payload["scopes"]["repositories"].as_array().unwrap() {
        assert!(
            repository["heatmap"].is_object(),
            "a repository scope carries its own heatmap: {repository}",
        );
    }
    let devices = payload["scopes"]["devices"].as_array().unwrap();
    assert!(
        !devices.is_empty(),
        "the device archive produced device scopes to check",
    );
    for device in devices {
        assert!(
            device.get("heatmap").map(|h| h.is_null()).unwrap_or(true),
            "a device scope carries no heatmap (the page falls back to the window's): {device}",
        );
    }
}

// ---------------------------------------------------------------------------
// The heatmap (qanungo #5, the habits view on local time)
// ---------------------------------------------------------------------------

/// A window whose sessions carry a known UTC offset, so the local hour each lands on is a fact this
/// test decides rather than one the archive's zone decides for it.
///
/// Every session shares the marathon fixture, whose own first record is `2026-08-10T09:00:00Z` — a
/// **Monday** — so the offset is the whole of what moves it into a local hour:
///
/// | # | Harness | Offset | Local start | Cell `(weekday, hour)` | Repository |
/// | --- | --- | --- | --- | --- | --- |
/// | 1 | claude-code | `-07:00` | Mon 02:00 | `(0, 2)` | `surdy/qanungo` |
/// | 2 | claude-code | `-07:00` | Mon 02:00 | `(0, 2)` | `surdy/qanungo` |
/// | 3 | copilot-cli | `+05:30` | (its own first record) | — | `surdy/qanungo` |
/// | 4 | claude-code | none | — | on no cell, `no_offset` | `surdy/munshi` |
///
/// Placement is on the transcript's own fixed instant, so a cell is stable year to year; the
/// sessions stay in the window because `archived_at` is relative (`hours_ago`). Session 3 uses a
/// copilot transcript whose first record this table does not pin — it exists to widen the harness
/// axis and to be a session on *some* cell, not to assert a particular one.
fn offset_archive() -> Vec<ArchivedSession> {
    let clean = transcript("rules/marathon-session.jsonl");
    let copilot = transcript("munshi/copilot-1.0.76-compaction.jsonl");
    vec![
        ArchivedSession::new(1, "claude-code", &clean, 1)
            .with_utc_offset("-07:00")
            .in_repository("surdy/qanungo"),
        ArchivedSession::new(2, "claude-code", &clean, 2)
            .with_utc_offset("-07:00")
            .in_repository("surdy/qanungo"),
        ArchivedSession::new(3, "copilot-cli", &copilot, 3)
            .with_utc_offset("+05:30")
            .in_repository("surdy/qanungo"),
        // No offset recorded — the state the heatmap places on no cell and counts.
        ArchivedSession::new(4, "claude-code", &clean, 4).in_repository("surdy/munshi"),
    ]
}

/// Every session placed across a heatmap's cells, over the harness columns.
fn placed_sessions(heatmap: &serde_json::Value) -> u64 {
    heatmap["cells"]
        .as_array()
        .expect("a cell list")
        .iter()
        .flat_map(|cell| cell["sessions"].as_array().expect("one count per harness"))
        .map(|count| count.as_u64().expect("a count"))
        .sum()
}

/// One cell out of a heatmap, by its local `(weekday, hour)`.
fn cell_at(heatmap: &serde_json::Value, weekday: u64, hour: u64) -> Option<&serde_json::Value> {
    heatmap["cells"]
        .as_array()
        .expect("a cell list")
        .iter()
        .find(|cell| {
            cell["weekday"].as_u64() == Some(weekday) && cell["hour"].as_u64() == Some(hour)
        })
}

/// The slice's own claim: the window laid on the operator's own clock — the local hour of the
/// weekday each session's work *began*, shifted from the transcript's instant by the session's own
/// offset — with the offset-less session counted on no cell.
#[test]
fn the_heatmap_lays_the_window_on_local_hours_of_the_week() {
    let (address, _directory) = spawn_dashboard(offset_archive());
    let payload = payload_of(address);
    let heatmap = &payload["heatmap"];

    // The two -07:00 claude sessions began Monday 09:00Z, which is Monday 02:00 local: cell (0, 2),
    // both on the claude column of the payload's one harness axis.
    let harnesses: Vec<&str> = payload["scopes"]["harnesses"]
        .as_array()
        .expect("the harness axis")
        .iter()
        .map(|label| label.as_str().unwrap())
        .collect();
    assert_eq!(harnesses, vec!["claude-code", "copilot-cli"]);
    let monday_two_am = cell_at(heatmap, 0, 2).expect("the -07:00 sessions' local cell");
    assert_eq!(monday_two_am["sessions"], serde_json::json!([2, 0]));
    assert_eq!(
        heatmap["cells_covered"],
        heatmap["cells"].as_array().unwrap().len(),
    );

    // The offset-less session is on no cell, counted — the same refusal the timeline makes for a
    // missing archive time, for the reason this whole view waited on.
    assert_eq!(heatmap["no_offset"], 1);
    assert_eq!(heatmap["undated"], 0);

    // Reconciliation: placed + unplaceable == the window's own folded count.
    let folded = payload["sessions"]["folded"].as_u64().expect("a count");
    assert_eq!(folded, 4);
    assert_eq!(
        placed_sessions(heatmap)
            + heatmap["no_offset"].as_u64().unwrap()
            + heatmap["undated"].as_u64().unwrap(),
        folded,
    );

    // Positional against the one harness axis, in the heatmap as in the lanes: every cell's two
    // arrays are exactly as wide as the roster.
    for cell in heatmap["cells"].as_array().unwrap() {
        assert_eq!(cell["sessions"].as_array().unwrap().len(), harnesses.len());
        assert_eq!(
            cell["active_seconds"].as_array().unwrap().len(),
            harnesses.len()
        );
    }

    // The footer names the clock in one word, and it is a *different* clock from the timeline's
    // archive-UTC — local, off the transcript's first activity.
    let provenance = &payload["provenance"]["heatmap"];
    assert_eq!(provenance["basis"], "first-activity-local");
    assert_eq!(provenance["cells_covered"], heatmap["cells_covered"]);
    assert_eq!(provenance["no_offset"], 1);
    assert_eq!(provenance["undated"], 0);
}

/// Narrowing to a repository narrows the grid with it, and each scope's cells reconcile to that
/// scope's own count — including a scope whose every session is unplaceable, which is all counts and
/// no cells rather than a phantom one.
#[test]
fn every_heatmap_scope_reconciles_to_its_own_count() {
    let (address, _directory) = spawn_dashboard(offset_archive());
    let payload = payload_of(address);

    // The whole window first.
    let whole = &payload["heatmap"];
    assert_eq!(
        placed_sessions(whole)
            + whole["no_offset"].as_u64().unwrap()
            + whole["undated"].as_u64().unwrap(),
        payload["sessions"]["folded"].as_u64().unwrap(),
    );

    // surdy/qanungo holds the three offset-bearing sessions: all placed, none counted off-grid.
    let qanungo = &scope_of(&payload, "surdy/qanungo")["heatmap"];
    assert_eq!(qanungo["no_offset"], 0);
    assert_eq!(placed_sessions(qanungo), 3);
    assert_eq!(
        cell_at(qanungo, 0, 2).expect("the -07:00 cell")["sessions"],
        serde_json::json!([2, 0])
    );

    // surdy/munshi holds only the offset-less session: no cell at all, one counted.
    let munshi = &scope_of(&payload, "surdy/munshi")["heatmap"];
    assert_eq!(munshi["cells_covered"], 0);
    assert_eq!(munshi["cells"], serde_json::json!([]));
    assert_eq!(munshi["no_offset"], 1);

    // Every scope reconciles against its own two counts — the numbers the page's own sentence
    // quotes.
    for scope in payload["scopes"]["repositories"].as_array().unwrap() {
        let heatmap = &scope["heatmap"];
        assert_eq!(
            placed_sessions(heatmap)
                + heatmap["no_offset"].as_u64().unwrap()
                + heatmap["undated"].as_u64().unwrap(),
            scope["sessions"]["folded"].as_u64().unwrap(),
            "{} draws a different number of sessions from the one it counts",
            scope["repository"],
        );
    }
}

/// The section's hard invariant: **numbers and nothing else** — not a weekday name, not a harness
/// label, not even an ISO date (the heatmap has no dates, only indices). Every leaf is a
/// non-negative integer, top level and in every scope, so there is nowhere for an archive-written
/// byte to hide.
#[test]
fn the_heatmap_section_is_integers_and_nothing_else() {
    let (address, _directory) = spawn_dashboard(offset_archive());
    let payload = payload_of(address);

    let mut leaves = 0;
    assert_heatmap_leaves(&payload["heatmap"], "heatmap", &mut leaves);
    for scope in payload["scopes"]["repositories"].as_array().unwrap() {
        assert_heatmap_leaves(&scope["heatmap"], "scope heatmap", &mut leaves);
    }
    assert!(leaves > 20, "the walk visited only {leaves} leaves");
}

/// Asserts that every leaf under a heatmap block is a non-negative integer. Recurses, because the
/// interesting places for a string to hide are the cell rows.
fn assert_heatmap_leaves(value: &serde_json::Value, path: &str, leaves: &mut usize) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, field) in fields {
                assert_heatmap_leaves(field, &format!("{path}.{key}"), leaves);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                assert_heatmap_leaves(item, &format!("{path}[{index}]"), leaves);
            }
        }
        serde_json::Value::Number(number) => {
            *leaves += 1;
            assert!(
                number.as_i64().is_some_and(|value| value >= 0),
                "{path} is {number}, which is not a count",
            );
        }
        other => panic!("{path} is {other}, which the heatmap never serves"),
    }
}

// ---------------------------------------------------------------------------
// The ask box (qanungo #10)
// ---------------------------------------------------------------------------

/// The credentials planted in `standup/qanungo-cost.md` — the same three `tests/ask.rs` guards the
/// CLI ranking against. Not one may appear in an answer while the secrets pass is on.
const PLANTED_IN_A_SUMMARY: [&str; 3] = [
    "ghp_CANARYCANARYCANARYCANARYCANARYCANARY",
    "sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99",
    "AKIACANARY0EXAMPLE99",
];

/// One search over the served surface.
fn ask(address: SocketAddr, query: &str) -> (String, serde_json::Value) {
    ask_target(address, &format!("/api/ask?q={query}"))
}

fn ask_target(address: SocketAddr, target: &str) -> (String, serde_json::Value) {
    let (head, body) = request(address, target);
    (
        head,
        serde_json::from_str(&body).expect("the answer is JSON"),
    )
}

/// The property this route rests on, and the same one the coaching section rests on: what a browser
/// is handed is `qanungo ask`'s own ranking.
///
/// The fold below is a second, independent run of exactly what the CLI does — `command::fold_ask`
/// with no window, which is the entry point `qanungo ask` takes when no `--last` is typed — over the
/// same archive and the now-warm cache. Every stable field of every hit has to agree, in order: a
/// served ranking that merely *resembled* the terminal's would be the "the web page and the CLI
/// disagree" bug this crate splits its folds to make impossible.
#[test]
fn the_ask_route_ranks_exactly_as_the_cli_does() {
    let (address, args, _directory) = spawn_with(three_lane_archive(), &[]);
    let (head, answer) = ask_target(address, "/api/ask?q=price+snapshot+archive&limit=3");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(head.contains("Content-Type: application/json"), "{head}");
    assert!(head.contains("Cache-Control: no-store"), "{head}");

    let query = qanungo::ask::Query::parse("price snapshot archive");
    let folded = command::fold_ask(
        &args.archive,
        None,
        &query,
        args.redaction.redactor(),
        3,
        false,
    )
    .expect("the archive searches");

    let hits = answer["hits"].as_array().expect("hits are an array");
    assert!(
        hits.len() >= 2,
        "the fixture matches more than once: {hits:?}"
    );
    assert_eq!(hits.len(), folded.ask.hits.len());
    assert_eq!(answer["total_matches"], folded.ask.total_matches);
    assert_eq!(answer["searched"], folded.ask.searched);
    assert_eq!(answer["unsearchable"], folded.ask.unsearchable);
    assert_eq!(answer["state"], "ranked");
    assert_eq!(
        answer["query"]["terms"],
        serde_json::json!(query.terms()),
        "the searched words, not the caller's bytes",
    );

    // Hit for hit, in order: the same sessions, the same scores, the same snippets, the same
    // reasons. The rank is the served list's own position, so it also pins the order itself.
    for (index, (served, cli)) in hits.iter().zip(&folded.ask.hits).enumerate() {
        assert_eq!(served["rank"], index + 1, "{index}");
        assert_eq!(served["source_hash"], cli.source_hash, "{index}");
        assert_eq!(served["score"], cli.score, "{index}");
        assert_eq!(served["title"], cli.title, "{index}");
        assert_eq!(served["snippet"], cli.snippet, "{index}");
        assert_eq!(served["harness"], cli.harness, "{index}");
        assert_eq!(served["matched"], serde_json::json!(cli.matched), "{index}");
        assert_eq!(
            served["repository"],
            serde_json::json!(cli.repository),
            "{index}",
        );
        assert_eq!(served["branch"], serde_json::json!(cli.branch), "{index}");
    }

    // The limit is the one the request asked for, and it truncated: the answer says so rather than
    // letting the third row read as the last match there was.
    assert_eq!(answer["limit"], 3);
    assert!(
        answer["total_matches"].as_u64().expect("a count") >= hits.len() as u64,
        "{answer}",
    );
}

/// The invariant this route inherits from the excerpt route, and the reason the corpus exists at
/// all: **a browser cannot make this process talk to the archive.**
///
/// The counter is **every request the archive is asked for** — the window listing, a session's
/// snapshots, a snapshot document, and artifact content alike. It moves while the refresh mirrors
/// the archive, and it must not move again however many searches are asked for, including one that
/// matches everything, one that matches nothing, and one refused for being too long.
///
/// Counting only artifact content would measure less than this test claims: a request path that
/// *listed* would move no such counter and pass. What is under test is the absence of archive
/// traffic of any kind, so that is what is counted — see [`spawn_counted_archive`].
#[test]
fn an_ask_request_never_reaches_the_archive() {
    let (base, requests) = spawn_counted_archive(three_lane_archive());
    let directory = tempfile::tempdir().expect("a scratch directory");
    let args = args(&base, &directory.path().join("qanungo"));
    let dashboard = Dashboard::start(&args).expect("the first fold");
    let address = dashboard.address();
    std::thread::spawn(move || dashboard.serve());

    let mirrored = requests.load(Ordering::Relaxed);
    assert!(mirrored > 0, "the refresh mirrored the archive");

    for target in [
        "/api/ask?q=price",
        "/api/ask?q=snapshot+archive+price+token&limit=50",
        "/api/ask?q=kubernetes+helm+chart",
        "/api/ask?q=the+a+of",
        "/api/ask",
        &format!("/api/ask?q={}", "a".repeat(4096)),
    ] {
        let (head, _body) = request(address, target);
        assert!(
            head.starts_with("HTTP/1.1 200 OK\r\n") || head.starts_with("HTTP/1.1 400 "),
            "{target}: {head}",
        );
        assert_eq!(
            requests.load(Ordering::Relaxed),
            mirrored,
            "{target} reached for the archive",
        );
    }
}

/// The done-bar's canary for this route: a `summary.md` carrying three live-*shaped* credentials is
/// ranked, quoted, and served over HTTP, and not one character of any of them survives.
///
/// The query is the one `tests/ask.rs` uses for the CLI: "pasted" appears only on the work-completed
/// line that also carries a GitHub token, so that line *is* the snippet this hit renders and the
/// scrub has to have reached it. The scrub is `Ask::fold`'s; what is pinned here is that the whole
/// path from the archive's bytes to the wire preserves it, and that the answer counts what fired so
/// a reader can see a marker was accounted for.
#[test]
fn a_planted_credential_in_a_summary_never_reaches_an_ask_answer() {
    let (address, _directory) = spawn_dashboard(three_lane_archive());
    let (head, answer) = ask(address, "pasted");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert_eq!(answer["state"], "ranked");

    let serialized = serde_json::to_string(&answer).expect("the answer serializes");
    for secret in PLANTED_IN_A_SUMMARY {
        assert!(!serialized.contains(secret), "the answer leaked {secret}");
    }
    // The line the query landed on is the one that carried the token, so the marker is what a
    // reader sees — proof the hit was rendered rather than the whole session being dropped.
    let snippet = answer["hits"][0]["snippet"]
        .as_str()
        .expect("the hit carries a snippet");
    assert!(snippet.contains("pasted into the run log"), "{snippet}");
    assert!(snippet.contains("[REDACTED:github-token]"), "{snippet}");
    assert!(
        answer["redaction"]["total"].as_u64().expect("a count") > 0,
        "the scrub fired and the answer says so",
    );
    assert_eq!(answer["redaction"]["secrets"], true);
    assert_eq!(
        answer["redaction"]["fired"][0]["pattern"], "github-token",
        "counts against pattern ids, never the value matched",
    );
}

/// `--no-redact` reaches this surface too, or it is a flag that lies about one of the surfaces it
/// governs — the negative half of the canary above.
#[test]
fn no_redact_serves_an_ask_answer_as_the_archive_holds_it() {
    let (address, _args, _directory) = spawn_with(three_lane_archive(), &["--no-redact"]);
    let (_head, answer) = ask(address, "pasted");
    let serialized = serde_json::to_string(&answer).expect("the answer serializes");
    assert_eq!(answer["redaction"]["secrets"], false);
    assert_eq!(answer["redaction"]["total"], 0);
    assert!(
        serialized.contains("ghp_CANARYCANARYCANARYCANARYCANARYCANARY"),
        "with the scrub off the summary's own line is what is quoted",
    );
}

/// The response's **shape**, pinned — because the page's JavaScript has no test of its own.
///
/// The lesson is the heatmap crash's: a device scope stopped carrying a key the page read and the
/// Rust suite could not see it, so the shape a page depends on is pinned in Rust. Two claims here.
/// Every field the ask box reads is present with the type it reads it as. And the route is
/// **scope-independent in V1** — there is no repository, device, harness, or window parameter, and a
/// request carrying one is byte-for-byte the same answer as one that does not, so a page that
/// narrows the sections above can never come to believe it narrowed this.
#[test]
fn the_ask_answer_shape_is_pinned_and_is_independent_of_every_scope() {
    let (address, _directory) = spawn_dashboard(three_lane_archive());
    let (_head, answer) = ask(address, "price");

    for (path, value) in [
        ("state", &answer["state"]),
        ("query.terms", &answer["query"]["terms"]),
        ("query.min_term_chars", &answer["query"]["min_term_chars"]),
        ("limit", &answer["limit"]),
        ("searched", &answer["searched"]),
        ("unsearchable", &answer["unsearchable"]),
        ("total_matches", &answer["total_matches"]),
        ("hits", &answer["hits"]),
        ("corpus.generation", &answer["corpus"]["generation"]),
        ("corpus.read_at", &answer["corpus"]["read_at"]),
        (
            "corpus.sessions_listed",
            &answer["corpus"]["sessions_listed"],
        ),
        ("corpus.bytes_read", &answer["corpus"]["bytes_read"]),
        ("corpus.scope", &answer["corpus"]["scope"]),
        ("redaction.secrets", &answer["redaction"]["secrets"]),
        ("redaction.profanity", &answer["redaction"]["profanity"]),
        (
            "redaction.pattern_revision",
            &answer["redaction"]["pattern_revision"],
        ),
        ("redaction.total", &answer["redaction"]["total"]),
        ("redaction.fired", &answer["redaction"]["fired"]),
    ] {
        assert!(!value.is_null(), "{path} is missing from the answer");
    }
    // `stale_since` is present and null on a healthy service — the page tells "fresh" from "the
    // refreshes are failing" by reading it, so an absent key would read as fresh.
    assert!(
        answer["corpus"].get("stale_since").is_some(),
        "the staleness key is served even when there is no staleness",
    );
    assert_eq!(answer["corpus"]["scope"], "all-history");
    assert!(answer["searched"].is_number());
    assert!(answer["hits"].is_array());

    let hit = &answer["hits"][0];
    for field in [
        "rank",
        "title",
        "harness",
        "repository",
        "branch",
        "archived_at",
        "score",
        "source_hash",
        "snippet",
        "matched",
    ] {
        assert!(hit.get(field).is_some(), "a hit carries no {field}: {hit}",);
    }
    assert!(hit["matched"].is_array());
    assert_eq!(
        hit["source_hash"].as_str().expect("a hash").len(),
        64,
        "the citation is a content hash and never a link",
    );

    // Scope-independent: every parameter the page's own controls are named after is ignored, and
    // the window is not a parameter at all.
    for target in [
        "/api/ask?q=price&repository=surdy/qanungo",
        "/api/ask?q=price&device=macbookpro",
        "/api/ask?q=price&harness=claude-code",
        "/api/ask?q=price&last=7d",
        "/api/ask?q=price&scope=all",
    ] {
        let (_head, scoped) = ask_target(address, target);
        assert_eq!(scoped, answer, "{target} changed the answer");
    }
}

/// The three answers over the wire: a list, the archive's own "no", and "you gave me no word to
/// search on" — plus the refusal that is not an answer at all.
#[test]
fn the_ask_route_states_which_kind_of_answer_it_is() {
    let (address, _directory) = spawn_dashboard(three_lane_archive());

    let (_head, ranked) = ask(address, "price");
    assert_eq!(ranked["state"], "ranked");
    assert!(!ranked["hits"].as_array().expect("hits").is_empty());

    let (_head, missed) = ask(address, "kubernetes+helm+chart");
    assert_eq!(missed["state"], "no-matches");
    assert_eq!(missed["total_matches"], 0);
    assert_eq!(missed["hits"], serde_json::json!([]));
    assert_eq!(
        missed["searched"], ranked["searched"],
        "it looked at the same corpus and the answer is no",
    );

    let (_head, empty) = ask(address, "the+a+of");
    assert_eq!(empty["state"], "no-searchable-terms");
    assert_eq!(empty["query"]["terms"], serde_json::json!([]));
    assert_eq!(empty["hits"], serde_json::json!([]));
    assert_eq!(
        empty["searched"], ranked["searched"],
        "the counts are the corpus's and cost nothing to state",
    );

    // Over the cap: a status, and none of the caller's bytes echoed back.
    let (head, refusal) = ask_target(address, &format!("/api/ask?q={}", "z".repeat(2048)));
    assert!(head.starts_with("HTTP/1.1 400 Bad Request\r\n"), "{head}");
    // `error` is the sentence the page puts in front of a reader and `reason` is what code matches
    // on, so the contract is both halves and not only the machine-readable one.
    assert_eq!(refusal["error"], "the query is too long to search");
    assert_eq!(refusal["reason"], "query-too-long");
    assert_eq!(refusal["bytes"], 2048);
    assert_eq!(refusal["max_bytes"], 1024);
    assert!(
        !serde_json::to_string(&refusal).unwrap().contains("zzzz"),
        "{refusal}",
    );
}

/// The corpus is every session the archive holds, and the answer's counts reconcile against the
/// footer's — one search and one provenance line describing the same read.
#[test]
fn the_ask_corpus_and_its_provenance_line_are_the_same_read() {
    let (address, _directory) = spawn_dashboard(three_lane_archive());
    let payload = payload_of(address);
    let (_head, answer) = ask(address, "price");

    let lane = &payload["provenance"]["lanes"]["ask"];
    assert_eq!(payload["provenance"]["ask_scope"], "all-history");
    assert_eq!(lane["scope"], "all-history");
    assert_eq!(lane["sessions_searchable"], answer["searched"]);
    assert_eq!(lane["sessions_unsearchable"], answer["unsearchable"]);
    assert_eq!(lane["sessions_listed"], answer["corpus"]["sessions_listed"]);
    assert_eq!(lane["bytes_read"], answer["corpus"]["bytes_read"]);
    assert_eq!(
        answer["corpus"]["generation"],
        payload["provenance"]["generation"]
    );

    // Counted, never dropped: four of the six fixtures carry a readable `summary.md`, one carries
    // munshi's placeholder and one carries none at all — so two sessions are unsearchable and every
    // one of the six is accounted for.
    assert_eq!(lane["sessions_searchable"], 4);
    assert_eq!(lane["sessions_unsearchable"], 2);
    assert_eq!(lane["sessions_listed"], 6);
}

/// The page's half of the slice: one box, wired to this server's own search route, computing
/// nothing and linking to nothing.
///
/// There is no JavaScript engine in this harness, so what is pinned is the shape — the box exists,
/// it submits to `/api/ask`, it renders the server's hits in the server's order, and every page
/// invariant survives the growth. The behaviour itself was checked in a browser against production.
#[test]
fn the_page_carries_one_ask_box_that_ranks_nothing_itself() {
    let (address, _directory) = spawn_dashboard(three_lane_archive());
    let (_head, body) = request(address, "/");

    assert!(body.contains("Ask your history"), "the section is there");
    assert!(body.contains("id=\"ask-query\""), "there is a query field");
    assert!(body.contains("id=\"ask-form\""), "Enter submits it");
    assert!(
        body.contains("/api/ask?q=\" + encodeURIComponent(typed)"),
        "the box asks this server's own route, with the query encoded",
    );
    assert!(
        body.contains("event.preventDefault()"),
        "the form navigates nowhere: this page has no links",
    );
    // The page reads the answer's own fields and writes them out. It does not rank: no sort, no
    // score arithmetic, no re-ordering of the hits the server sent.
    for read in [
        "askAnswer.state",
        "askAnswer.hits.map(askHitNode)",
        "hit.rank",
        "hit.score",
        "hit.source_hash",
        "hit.snippet",
        "hit.matched.join",
    ] {
        assert!(body.contains(read), "the page reads {read}");
    }
    assert!(
        !body.contains("askAnswer.hits.sort"),
        "the order is the server's total order, never re-derived here",
    );
    // The search is not narrowed by the scope controls, and the page says so rather than leaving a
    // reader to assume either way.
    assert!(
        body.contains("It is not narrowed by the scope controls"),
        "the page states the search's bounds",
    );

    // Every invariant the page already held, restated over the grown file.
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
    assert_eq!(
        body.matches("fetch(").count(),
        3,
        "the payload, an excerpt and a search, and nothing the ask box adds",
    );
}
