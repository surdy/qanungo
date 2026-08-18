//! The mirror/sync path, end to end against a stand-in archive.
//!
//! The stand-in speaks the shapes the real `patwari-server` returns — the session listing with
//! its `latest_snapshot` projection and `next_cursor`, the snapshot document with its canonical
//! manifest and artifact list, and the artifact-content route with its `x-patwari-*` metadata
//! headers — so these tests exercise the real client without needing a server, a network, or a
//! populated archive. The live server is verified separately by running the binary against it.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use qanungo::cache::BlobCache;
use qanungo::patwari::{ReadClient, sha256_hex};
use qanungo::sync::{self, SkipReason};

// ---------------------------------------------------------------------------
// The stand-in archive
// ---------------------------------------------------------------------------

/// A way for the archive to serve something other than what it promised. Each variant defeats a
/// different stage of the client's three-stage verified download, so the stage that catches it is
/// exercised rather than assumed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Corruption {
    None,
    /// Same length, different bytes: the declared stored size still matches, so only the stored
    /// digest can catch it.
    StoredBytes,
    /// Fewer bytes than the declared stored size — a body cut short in transit.
    TruncatedBody,
    /// A compression this build does not implement.
    UnknownCompression,
    /// Content and headers agree with each other, but the *listing* promised a different
    /// `original_sha256`. This is the one that matters most: the listing's digest is the cache
    /// key and the hash a finding cites, so accepting these bytes would file evidence under a
    /// hash that does not describe them.
    ListingDigest,
}

/// The digest the [`Corruption::ListingDigest`] case advertises instead of the truth.
const WRONG_DIGEST: &str = "beefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef";

#[derive(Clone)]
struct ArchivedSession {
    session_id: String,
    snapshot_id: String,
    artifact_id: String,
    source_agent: String,
    artifact_set_version: u16,
    transcript: Vec<u8>,
    /// `identity` or `zstd`, matching Patwari's own compression vocabulary.
    compression: &'static str,
    /// A summary-only capture has no `transcript.jsonl` artifact at all.
    has_transcript: bool,
    corruption: Corruption,
}

impl ArchivedSession {
    fn claude(index: u8, transcript: &str, compression: &'static str) -> Self {
        Self {
            session_id: format!("{index:02x}").repeat(16),
            snapshot_id: format!("{:02x}", index + 100).repeat(16),
            artifact_id: format!("{:02x}", index + 200).repeat(16),
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            transcript: transcript.as_bytes().to_vec(),
            compression,
            has_transcript: true,
            corruption: Corruption::None,
        }
    }

    fn corrupt(index: u8, corruption: Corruption) -> Self {
        Self {
            corruption,
            ..Self::claude(index, TRANSCRIPT, "identity")
        }
    }

    /// The bytes the archive honestly holds, before any corruption is applied to the wire.
    fn stored(&self) -> Vec<u8> {
        match self.compression {
            "zstd" => zstd::encode_all(self.transcript.as_slice(), 3).unwrap(),
            _ => self.transcript.clone(),
        }
    }

    /// The `original_sha256` the *listing* advertises.
    fn listed_original_sha256(&self) -> String {
        match self.corruption {
            Corruption::ListingDigest => WRONG_DIGEST.to_owned(),
            _ => sha256_hex(&self.transcript),
        }
    }
}

#[derive(Default)]
struct Requests {
    targets: Vec<String>,
}

impl Requests {
    fn content_requests(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| target.contains("/content"))
            .count()
    }
}

/// Serves `sessions` until the test process exits, and returns its base URL.
fn spawn_archive(sessions: Vec<ArchivedSession>) -> (String, Arc<Mutex<Requests>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Requests::default()));
    let recorded = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sessions = sessions.clone();
            let recorded = Arc::clone(&recorded);
            std::thread::spawn(move || serve(stream, &sessions, &recorded));
        }
    });
    (base, requests)
}

fn serve(mut stream: TcpStream, sessions: &[ArchivedSession], recorded: &Mutex<Requests>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain the rest of the head so the client's write always completes.
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
    recorded.lock().unwrap().targets.push(target.clone());

    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let response = if path == "/api/v1/sessions" {
        json_response(&session_page(sessions, query))
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

/// Two pages when there is more than one session, so the client's cursor discipline is exercised
/// rather than assumed.
fn session_page(sessions: &[ArchivedSession], query: &str) -> String {
    let second_page = query.contains("cursor=page-two");
    let (page, next_cursor) = if sessions.len() > 1 && !second_page {
        (&sessions[..1], "\"page-two\"")
    } else if second_page {
        (&sessions[1..], "null")
    } else {
        (sessions, "null")
    };
    let items: Vec<String> = page
        .iter()
        .map(|session| {
            format!(
                r#"{{"session_id":"{}","source_agent":"{}","source_session_id":"harness-{}",
                    "created_at":"2026-08-10T09:00:00.000Z","updated_at":"2026-08-10T10:00:00.000Z",
                    "latest_snapshot":{{"snapshot_id":"{}","completed_at":"2026-08-10T10:00:00.000Z",
                    "project":null,"repository":null,"branch":null,"source_agent_version":null,
                    "artifact_set_version":{},"snapshot_url":"/api/v1/snapshots/{}",
                    "manifest_url":"/api/v1/snapshots/{}/manifest"}},
                    "captures_url":"/api/v1/sessions/{}/captures",
                    "snapshots_url":"/api/v1/sessions/{}/snapshots"}}"#,
                session.session_id,
                session.source_agent,
                session.session_id,
                session.snapshot_id,
                session.artifact_set_version,
                session.snapshot_id,
                session.snapshot_id,
                session.session_id,
                session.session_id,
            )
        })
        .collect();
    format!(
        r#"{{"items":[{}],"next_cursor":{next_cursor},"high_watermark":null}}"#,
        items.join(",")
    )
}

fn snapshot_document(session: &ArchivedSession) -> String {
    let stored = session.stored();
    let artifacts = if session.has_transcript {
        format!(
            r#"[{{"artifact_id":"{}","artifact_index":0,"logical_path":"transcript.jsonl",
                "media_type":"application/jsonl","original_size_bytes":{},
                "original_sha256":"sha256:{}","stored_size_bytes":{},
                "stored_sha256":"sha256:{}","compression":"{}",
                "metadata_url":"/api/v1/artifacts/{}",
                "content_url":"/api/v1/artifacts/{}/content"}}]"#,
            session.artifact_id,
            session.transcript.len(),
            session.listed_original_sha256(),
            stored.len(),
            sha256_hex(&stored),
            session.compression,
            session.artifact_id,
            session.artifact_id,
        )
    } else {
        "[]".to_owned()
    };
    format!(
        r#"{{"snapshot_id":"{}","session_id":"{}","snapshot_fingerprint":"sha256:{}",
            "manifest_id":"{}","manifest_sha256":"sha256:{}",
            "completed_at":"2026-08-10T10:00:00.000Z","artifact_count":1,
            "total_original_bytes":{},"total_stored_bytes":{},"capture_count":1,
            "captures_url":"/api/v1/snapshots/{}/captures","manifest_url":"/api/v1/manifests/{}",
            "manifest":{{"schema_version":1,
                "session":{{"source_agent":"{}","source_session_id":"harness"}},
                "capture":{{"captured_at":"2026-08-10T10:00:00.000Z","source_cursor":null,
                    "source_state_hash":null,"source_metadata":{{}},"project":null,
                    "repository":null,"branch":null,"source_agent_version":null,
                    "artifact_set_version":{},"munshi_version":null}},
                "artifacts":[]}},
            "artifacts":{artifacts}}}"#,
        session.snapshot_id,
        session.session_id,
        "0".repeat(64),
        session.snapshot_id,
        "1".repeat(64),
        session.transcript.len(),
        stored.len(),
        session.snapshot_id,
        session.snapshot_id,
        session.source_agent,
        session.artifact_set_version,
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
    // Everything the headers declare is computed from the honest bytes; the corruption is then
    // applied to what actually goes on the wire, exactly as a damaged blob or a lying peer would
    // present it.
    let honest = session.stored();
    let declared_stored_sha = sha256_hex(&honest);
    let declared_stored_len = honest.len();
    let mut compression = session.compression;
    let mut body = honest;
    match session.corruption {
        Corruption::StoredBytes => {
            // Same length, one flipped byte: the size check passes, the digest check must not.
            if let Some(last) = body.last_mut() {
                *last ^= 0xff;
            }
        }
        Corruption::TruncatedBody => {
            body.truncate(body.len() / 2);
        }
        Corruption::UnknownCompression => compression = "brotli",
        Corruption::None | Corruption::ListingDigest => {}
    }
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/jsonl\r\n\
         Content-Length: {declared_stored_len}\r\n\
         x-patwari-compression: {compression}\r\n\
         x-patwari-original-size-bytes: {}\r\n\
         x-patwari-original-sha256: sha256:{}\r\n\
         x-patwari-stored-size-bytes: {declared_stored_len}\r\n\
         x-patwari-stored-sha256: sha256:{declared_stored_sha}\r\n\r\n",
        session.transcript.len(),
        sha256_hex(&session.transcript),
    )
    .into_bytes();
    response.extend_from_slice(&body);
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
// Tests
// ---------------------------------------------------------------------------

const TRANSCRIPT: &str = concat!(
    r#"{"type":"user","uuid":"u1","timestamp":"2026-08-10T09:00:00.000Z","message":{"role":"user","content":"do the thing"}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-10T09:01:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}"#,
    "\n",
    r#"{"type":"user","uuid":"r1","timestamp":"2026-08-10T09:01:30.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"out","is_error":false}]}}"#,
    "\n",
);

const OTHER_TRANSCRIPT: &str = concat!(
    r#"{"type":"user","uuid":"u9","timestamp":"2026-08-11T09:00:00.000Z","message":{"role":"user","content":"second session"}}"#,
    "\n",
);

fn cache() -> (tempfile::TempDir, BlobCache) {
    let directory = tempfile::tempdir().unwrap();
    let cache = BlobCache::open(directory.path().join("qanungo")).unwrap();
    (directory, cache)
}

#[test]
fn an_empty_archive_mirrors_cleanly() {
    let (base, requests) = spawn_archive(Vec::new());
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 4).unwrap();
    assert!(mirror.sessions.is_empty());
    assert!(mirror.skipped.is_empty());
    assert_eq!(mirror.stats.sessions_listed, 0);
    assert_eq!(mirror.stats.cache_hits, 0);
    assert_eq!(mirror.stats.cache_misses, 0);
    assert_eq!(requests.lock().unwrap().content_requests(), 0);
}

#[test]
fn a_first_sync_fetches_every_transcript_and_a_second_serves_them_from_cache() {
    let sessions = vec![
        ArchivedSession::claude(1, TRANSCRIPT, "identity"),
        ArchivedSession::claude(2, OTHER_TRANSCRIPT, "zstd"),
    ];
    // What actually crosses the wire is the *stored* form: the zstd artifact transfers far fewer
    // bytes than it folds, which is exactly the distinction the footer must not blur.
    let wire_bytes: u64 = sessions
        .iter()
        .map(|session| session.stored().len() as u64)
        .sum();
    assert!(
        wire_bytes < (TRANSCRIPT.len() + OTHER_TRANSCRIPT.len()) as u64,
        "the zstd fixture must actually compress, or this asserts nothing"
    );
    let (base, requests) = spawn_archive(sessions);
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let first = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 4).unwrap();
    assert_eq!(first.stats.sessions_listed, 2);
    assert_eq!(first.sessions.len(), 2);
    assert_eq!(first.stats.cache_misses, 2);
    assert_eq!(first.stats.cache_hits, 0);
    assert_eq!(first.stats.bytes_transferred, wire_bytes);
    assert_eq!(requests.lock().unwrap().content_requests(), 2);

    // The cache is keyed by content hash, so the naive re-list costs listings only.
    let second = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 4).unwrap();
    assert_eq!(second.stats.cache_hits, 2);
    assert_eq!(second.stats.cache_misses, 0);
    assert_eq!(second.stats.bytes_transferred, 0);
    assert_eq!(
        requests.lock().unwrap().content_requests(),
        2,
        "a cache hit must not touch the content route"
    );
}

#[test]
fn the_zstd_transcript_is_decoded_and_cached_under_its_original_hash() {
    let session = ArchivedSession::claude(3, TRANSCRIPT, "zstd");
    let (base, _requests) = spawn_archive(vec![session]);
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
    let mirrored = &mirror.sessions[0];
    assert_eq!(mirrored.source_hash, sha256_hex(TRANSCRIPT.as_bytes()));
    assert_eq!(mirrored.artifact_set_version, 2);
    assert!(cache.contains(&mirrored.source_hash));
}

#[test]
fn the_mirrored_window_keeps_the_archives_listing_order() {
    let sessions = vec![
        ArchivedSession::claude(4, TRANSCRIPT, "identity"),
        ArchivedSession::claude(5, OTHER_TRANSCRIPT, "identity"),
    ];
    let (base, _requests) = spawn_archive(sessions);
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    // Concurrency 4 over a two-page listing: workers finish out of order, the mirror does not.
    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 4).unwrap();
    let hashes: Vec<_> = mirror
        .sessions
        .iter()
        .map(|session| session.source_hash.clone())
        .collect();
    assert_eq!(
        hashes,
        vec![
            sha256_hex(TRANSCRIPT.as_bytes()),
            sha256_hex(OTHER_TRANSCRIPT.as_bytes()),
        ]
    );
}

#[test]
fn a_summary_only_snapshot_is_a_recorded_gap_not_a_failure() {
    let mut summary_only = ArchivedSession::claude(6, TRANSCRIPT, "identity");
    summary_only.has_transcript = false;
    let (base, _requests) = spawn_archive(vec![
        summary_only,
        ArchivedSession::claude(7, OTHER_TRANSCRIPT, "identity"),
    ]);
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
    assert_eq!(mirror.sessions.len(), 1);
    assert_eq!(mirror.skipped.len(), 1);
    assert_eq!(mirror.skipped[0].reason, SkipReason::NoTranscript);
}

#[test]
fn a_harness_without_an_interpreter_is_a_recorded_gap() {
    let mut future = ArchivedSession::claude(8, TRANSCRIPT, "identity");
    future.source_agent = "future-harness".to_owned();
    let (base, requests) = spawn_archive(vec![future]);
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
    assert!(mirror.sessions.is_empty());
    assert_eq!(
        mirror.skipped[0].reason,
        SkipReason::UnknownAgent("future-harness".to_owned())
    );
    assert_eq!(
        requests.lock().unwrap().content_requests(),
        0,
        "an uninterpretable transcript must not be transferred"
    );
}

/// The three-stage verified download exists so that nothing unverified ever reaches the cache —
/// and therefore so that a cited `source_hash` always describes the bytes filed under it. Each
/// corruption defeats a different stage; every one of them must end as a recorded gap with an
/// empty cache, never as a folded session.
#[test]
fn corrupt_content_is_refused_at_every_stage_and_never_cached() {
    for (corruption, expected) in [
        (
            Corruption::StoredBytes,
            "stored content hash does not match",
        ),
        (Corruption::TruncatedBody, "stored size mismatch"),
        (
            Corruption::UnknownCompression,
            "unknown compression `brotli`",
        ),
        (
            Corruption::ListingDigest,
            "does not match the listing's declared",
        ),
    ] {
        let (base, requests) = spawn_archive(vec![ArchivedSession::corrupt(20, corruption)]);
        let (directory, cache) = cache();
        let client = ReadClient::connect(&base).unwrap();

        let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
        assert!(
            mirror.sessions.is_empty(),
            "corrupt content must not be folded"
        );
        assert_eq!(mirror.skipped.len(), 1);
        let SkipReason::Unreadable(detail) = &mirror.skipped[0].reason else {
            panic!("expected an unreadable skip, got {:?}", mirror.skipped[0]);
        };
        assert!(
            detail.contains(expected),
            "expected `{expected}` in: {detail}"
        );
        assert_eq!(
            requests.lock().unwrap().content_requests(),
            1,
            "the download is attempted exactly once — no retry storm"
        );
        assert_eq!(
            blob_count(directory.path()),
            0,
            "unverified bytes must never reach the cache"
        );
        // A refused download is not a transfer the footer should claim credit for.
        assert_eq!(mirror.stats.cache_misses, 0);
        assert_eq!(mirror.stats.bytes_transferred, 0);
    }
}

/// Files anywhere under a cache root, temporary or not.
fn blob_count(root: &std::path::Path) -> usize {
    fn walk(path: &std::path::Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            if entry.path().is_dir() {
                walk(&entry.path(), count);
            } else {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(root, &mut count);
    count
}

#[test]
fn an_unreachable_archive_fails_the_run_rather_than_reporting_on_nothing() {
    // Bound and immediately dropped: the port is closed, so connecting fails.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&format!("http://127.0.0.1:{port}")).unwrap();
    assert!(sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).is_err());
}

#[test]
fn the_report_command_runs_end_to_end_against_the_archive() {
    let (base, _requests) = spawn_archive(vec![ArchivedSession::claude(9, TRANSCRIPT, "zstd")]);
    let directory = tempfile::tempdir().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_qanungo"))
        .args(["report", "--last", "30d", "--patwari-url", &base])
        .arg("--cache-dir")
        .arg(directory.path().join("qanungo"))
        .env_remove("PATWARI_URL")
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let markdown = String::from_utf8(output.stdout).unwrap();
    assert!(markdown.starts_with("# Coaching report — last 30d"));
    assert!(markdown.contains("**1 sessions** — claude-code (1)"));
    assert!(markdown.contains("## Cadence"));
    assert!(markdown.contains("| Bash | 1 | 0 | 0% |"));
    assert!(markdown.contains("_Instrumentation —"));
    assert!(markdown.contains("cache 0 hits / 1 misses"));
    // Nothing in a three-record session crosses a threshold.
    assert!(markdown.contains("Nothing crossed a rule threshold"));
}

#[test]
fn the_report_command_reports_an_empty_archive_cleanly() {
    let (base, _requests) = spawn_archive(Vec::new());
    let directory = tempfile::tempdir().unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_qanungo"))
        .args(["report", "--last", "7d", "--patwari-url", &base])
        .arg("--cache-dir")
        .arg(directory.path().join("qanungo"))
        .env_remove("PATWARI_URL")
        .output()
        .expect("the binary runs");
    assert!(output.status.success());
    let markdown = String::from_utf8(output.stdout).unwrap();
    assert!(markdown.contains("No archived sessions fell in this window"));
    assert!(markdown.contains("_Instrumentation —"));
}

/// Not a fixture assertion: a live-archive smoke check kept next to the stand-in it stands in
/// for. Ignored by default so `cargo test` never needs the network.
#[test]
#[ignore = "requires a reachable Patwari server; run with PATWARI_URL set"]
fn against_a_live_archive() {
    let base = std::env::var("PATWARI_URL").expect("PATWARI_URL");
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();
    let mirror = sync::sync(&client, &cache, "2026-01-01T00:00:00.000Z", 4).unwrap();
    println!(
        "listed {} sessions, {} hits, {} misses",
        mirror.stats.sessions_listed, mirror.stats.cache_hits, mirror.stats.cache_misses
    );
}
