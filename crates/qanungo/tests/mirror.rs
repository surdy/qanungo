//! The mirror/sync path, end to end against a stand-in archive.
//!
//! The stand-in speaks the shapes the real `patwari-server` returns — the session listing with
//! its `latest_snapshot` projection and `next_cursor`, the snapshot document with its canonical
//! manifest and artifact list, and the artifact-content route with its `x-patwari-*` metadata
//! headers — so these tests exercise the real client without needing a server, a network, or a
//! populated archive. The live server is verified separately by running the binary against it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use qanungo::cache::BlobCache;
use qanungo::metrics;
use qanungo::patwari::{MAX_DECLARED_TRANSCRIPT_BYTES, ReadClient, sha256_hex};
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
    /// *More* stored bytes than declared, framed as chunked so no `Content-Length` bounds the
    /// read. Only the client's own declared-size bound can stop this one, and it has to stop it
    /// mid-stream rather than after the fact.
    ExtraStoredBytes,
    /// The content headers declare a different original size than the listing did. Both are the
    /// archive's own renderings of one manifest row, so disagreement means the archive is
    /// contradicting itself — and the client has to refuse rather than pick a winner, because
    /// whichever it picked would become the bound it enforces the transfer against.
    HeaderSizeDisagreement,
}

/// The digest the [`Corruption::ListingDigest`] case advertises instead of the truth.
const WRONG_DIGEST: &str = "beefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeefbeef";

struct ArchivedSession {
    session_id: String,
    snapshot_id: String,
    artifact_id: String,
    source_agent: String,
    artifact_set_version: u16,
    transcript: Vec<u8>,
    /// The stored form, computed once at construction. Fixtures reach tens of megabytes, and
    /// re-deriving this per request would make the stand-in, not the client, the thing under
    /// test.
    stored: Vec<u8>,
    /// The honest digest of `transcript`, hashed once for the same reason.
    original_sha256: String,
    /// `identity` or `zstd`, matching Patwari's own compression vocabulary.
    compression: &'static str,
    /// The `original_size_bytes` the archive advertises, in both the listing and the content
    /// headers, when that is deliberately not the truth. `None` means it is.
    declared_original_bytes: Option<u64>,
    /// Serve the body as `Transfer-Encoding: chunked` in chunks of this size. `None` is
    /// `Content-Length`, which is what the real server sends for a download.
    wire_chunks: Option<usize>,
    /// A summary-only capture has no `transcript.jsonl` artifact at all.
    has_transcript: bool,
    corruption: Corruption,
}

impl ArchivedSession {
    fn claude(index: u8, transcript: &str, compression: &'static str) -> Self {
        Self::from_bytes(index, transcript.as_bytes().to_vec(), compression)
    }

    fn from_bytes(index: u8, transcript: Vec<u8>, compression: &'static str) -> Self {
        let stored = match compression {
            "zstd" => zstd::encode_all(transcript.as_slice(), 3).unwrap(),
            _ => transcript.clone(),
        };
        Self {
            session_id: format!("{index:02x}").repeat(16),
            snapshot_id: format!("{:02x}", index.wrapping_add(100)).repeat(16),
            artifact_id: format!("{:02x}", index.wrapping_add(200)).repeat(16),
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            original_sha256: sha256_hex(&transcript),
            transcript,
            stored,
            compression,
            declared_original_bytes: None,
            wire_chunks: None,
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
    fn stored(&self) -> &[u8] {
        &self.stored
    }

    /// The `original_size_bytes` the *listing* advertises — the truth unless a fixture overrides
    /// it.
    fn declared_original_bytes(&self) -> u64 {
        self.declared_original_bytes
            .unwrap_or(self.transcript.len() as u64)
    }

    /// The `original_size_bytes` the *content headers* advertise. The same number, except when a
    /// fixture is deliberately making the archive contradict itself.
    fn header_original_bytes(&self) -> u64 {
        match self.corruption {
            Corruption::HeaderSizeDisagreement => self.declared_original_bytes() + 1,
            _ => self.declared_original_bytes(),
        }
    }

    /// The `original_sha256` the *listing* advertises.
    fn listed_original_sha256(&self) -> &str {
        match self.corruption {
            Corruption::ListingDigest => WRONG_DIGEST,
            _ => &self.original_sha256,
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
///
/// The fixtures are shared rather than cloned per connection: a multi-megabyte transcript is a
/// deliberate case here, and a stand-in that copied one per request would measure the copy.
fn spawn_archive(sessions: Vec<ArchivedSession>) -> (String, Arc<Mutex<Requests>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Requests::default()));
    let recorded = Arc::clone(&requests);
    let sessions = Arc::new(sessions);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sessions = Arc::clone(&sessions);
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
            session.declared_original_bytes(),
            session.listed_original_sha256(),
            stored.len(),
            sha256_hex(stored),
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
    let declared_stored_sha = sha256_hex(session.stored());
    let declared_stored_len = session.stored().len();
    let mut compression = session.compression;
    let mut body = session.stored().to_vec();
    let mut chunk_size = session.wire_chunks;
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
        Corruption::ExtraStoredBytes => {
            // Chunked, so the client's read is not bounded by `Content-Length` and the declared
            // stored size is the only thing left that can stop the overrun.
            body.extend(std::iter::repeat_n(b'x', declared_stored_len + 4096));
            chunk_size = Some(body.len().max(1));
        }
        Corruption::None | Corruption::ListingDigest | Corruption::HeaderSizeDisagreement => {}
    }
    let framing = if chunk_size.is_some() {
        "Transfer-Encoding: chunked".to_owned()
    } else {
        format!("Content-Length: {declared_stored_len}")
    };
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/jsonl\r\n\
         {framing}\r\n\
         x-patwari-compression: {compression}\r\n\
         x-patwari-original-size-bytes: {}\r\n\
         x-patwari-original-sha256: sha256:{}\r\n\
         x-patwari-stored-size-bytes: {declared_stored_len}\r\n\
         x-patwari-stored-sha256: sha256:{declared_stored_sha}\r\n\r\n",
        session.header_original_bytes(),
        session.original_sha256,
    )
    .into_bytes();
    match chunk_size {
        Some(size) => {
            for chunk in body.chunks(size.max(1)) {
                response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
                response.extend_from_slice(chunk);
                response.extend_from_slice(b"\r\n");
            }
            response.extend_from_slice(b"0\r\n\r\n");
        }
        None => response.extend_from_slice(&body),
    }
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
        (
            Corruption::ExtraStoredBytes,
            "stored bytes ran past the declared",
        ),
        (
            Corruption::HeaderSizeDisagreement,
            "declared sizes disagree",
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

/// A transcript comfortably past the 64 MiB ceiling the download path used to refuse outright.
///
/// Built from a few hundred large records rather than a million small ones, so the fold parses
/// realistic line lengths at a realistic cost, and deliberately highly compressible: the wire form
/// is a few kilobytes while the folded form is tens of megabytes, which is exactly the shape the
/// old cap got wrong. It applied to the *original* size, so it skipped the long tool-heavy
/// sessions the coaching rules care most about while their stored form would have transferred in
/// a blink.
fn oversized_transcript() -> Vec<u8> {
    const PADDING_BYTES: usize = 128 * 1024;
    const RECORDS: usize = 520;
    let padding = "a".repeat(PADDING_BYTES);
    let mut transcript = String::with_capacity(RECORDS * (PADDING_BYTES + 128) + TRANSCRIPT.len());
    for index in 0..RECORDS {
        transcript.push_str(&format!(
            r#"{{"type":"user","uuid":"pad{index}","timestamp":"2026-08-10T09:00:00.000Z","message":{{"role":"user","content":"{padding}"}}}}"#
        ));
        transcript.push('\n');
    }
    // The tool-bearing tail, so the fold has something to count besides padding.
    transcript.push_str(TRANSCRIPT);
    transcript.into_bytes()
}

/// The regression this whole change exists for: a transcript larger than the removed cap must
/// transfer, verify, cache, and fold — with the wire cost being the compressed form and the fold
/// reading the full original off disk.
#[test]
fn a_transcript_past_the_old_cap_streams_verifies_caches_and_folds() {
    const OLD_CAP_BYTES: u64 = 64 * 1024 * 1024;

    let transcript = oversized_transcript();
    assert!(
        transcript.len() as u64 > OLD_CAP_BYTES,
        "the fixture has to clear the cap it is here to prove is gone"
    );
    let digest = sha256_hex(&transcript);
    let session = ArchivedSession::from_bytes(30, transcript.clone(), "zstd");
    let wire_bytes = session.stored().len() as u64;
    assert!(
        wire_bytes < OLD_CAP_BYTES,
        "the stored form must be the small one, or this proves nothing about the wire"
    );

    let (base, _requests) = spawn_archive(vec![session]);
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
    assert_eq!(mirror.skipped.len(), 0, "{:?}", mirror.skipped);
    assert_eq!(mirror.sessions.len(), 1);
    let mirrored = &mirror.sessions[0];
    assert_eq!(mirrored.source_hash, digest);
    assert_eq!(mirrored.size_bytes, transcript.len() as u64);
    // Wire cost is the stored form; fold cost is the original. The footer keeps them apart.
    assert_eq!(mirror.stats.bytes_transferred, wire_bytes);
    assert_eq!(mirror.stats.cache_misses, 1);

    // The blob on disk is the whole decompressed transcript, byte for byte.
    let mut cached = Vec::new();
    cache
        .open_blob(&digest)
        .unwrap()
        .read_to_end(&mut cached)
        .unwrap();
    assert_eq!(cached.len(), transcript.len());
    assert_eq!(sha256_hex(&cached), digest);

    // And it folds off disk, streaming, with the tool tail counted.
    let source = metrics::source_for_agent(&mirrored.source_agent).unwrap();
    let fold = metrics::fold_transcript(
        source,
        mirrored.artifact_set_version,
        BufReader::new(cache.open_blob(&digest).unwrap()),
    )
    .unwrap();
    assert_eq!(fold.tools.by_tool["Bash"].attempts, 1);
    assert_eq!(fold.tools.by_tool["Bash"].errors, 0);
    assert_eq!(fold.summary.user_requests, 521);
}

/// `Content-Length` is what the real archive sends, so chunked framing is the path least likely
/// to be exercised by accident and most likely to be wrong. Many small chunks put a chunk boundary
/// inside almost every read, which is what makes the client carry the pending-terminator state
/// across reads rather than resolving it within one.
#[test]
fn a_body_arriving_in_many_small_chunks_is_reassembled_and_verified() {
    let mut session = ArchivedSession::claude(33, TRANSCRIPT, "zstd");
    session.wire_chunks = Some(7);
    let wire_bytes = session.stored().len() as u64;
    assert!(
        wire_bytes > 7 * 4,
        "the fixture must span several chunks, or this proves nothing"
    );

    let (base, _requests) = spawn_archive(vec![session]);
    let (_directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
    assert_eq!(mirror.skipped.len(), 0, "{:?}", mirror.skipped);
    assert_eq!(mirror.sessions.len(), 1);
    assert_eq!(
        mirror.sessions[0].source_hash,
        sha256_hex(TRANSCRIPT.as_bytes())
    );
    assert_eq!(mirror.stats.bytes_transferred, wire_bytes);
    assert!(cache.contains(&mirror.sessions[0].source_hash));
}

/// The decompression window is the one buffer in a download whose size the *artifact* gets to
/// choose, and libzstd's default ceiling for it is 128 MiB per decoder — half a gigabyte across
/// four mirror workers, demanded by nothing more than a frame header. This pins that the client
/// caps it instead.
///
/// The frame here is entirely honest about its content: correct bytes, correct stored digest,
/// correct sizes. The only thing wrong with it is how much memory it asks for.
#[test]
fn a_frame_demanding_an_oversized_window_is_refused() {
    use std::io::Write;

    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder
        .set_parameter(zstd::zstd_safe::CParameter::WindowLog(25))
        .unwrap();
    encoder
        .set_parameter(zstd::zstd_safe::CParameter::ContentSizeFlag(false))
        .unwrap();
    encoder.write_all(TRANSCRIPT.as_bytes()).unwrap();
    let wide_window = encoder.finish().unwrap();
    assert!(
        zstd::decode_all(wide_window.as_slice()).is_ok(),
        "the fixture must be a decodable frame, refused only for its window"
    );

    let mut session = ArchivedSession::claude(34, TRANSCRIPT, "zstd");
    session.stored = wide_window;

    let (base, requests) = spawn_archive(vec![session]);
    let (directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
    assert!(mirror.sessions.is_empty());
    let SkipReason::Unreadable(detail) = &mirror.skipped[0].reason else {
        panic!("expected an unreadable skip, got {:?}", mirror.skipped[0]);
    };
    assert!(
        detail.contains("could not be decompressed"),
        "an oversized window is a decompression refusal, not a verification failure: {detail}"
    );
    assert_eq!(requests.lock().unwrap().content_requests(), 1);
    assert_eq!(blob_count(directory.path()), 0);
    assert_eq!(mirror.stats.bytes_transferred, 0);
}

/// A zstd artifact that decompresses past the size the archive declared for it. The stored side
/// is entirely honest, so only the original-side bound can stop this, and it has to stop it while
/// the frame is still being decoded rather than after the disk has taken the whole bomb.
#[test]
fn more_decompressed_bytes_than_declared_aborts_the_transfer() {
    let bomb = vec![b'a'; 8 * 1024 * 1024];
    let mut session = ArchivedSession::from_bytes(31, bomb, "zstd");
    session.declared_original_bytes = Some(1024);

    let (base, requests) = spawn_archive(vec![session]);
    let (directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
    assert!(mirror.sessions.is_empty());
    let SkipReason::Unreadable(detail) = &mirror.skipped[0].reason else {
        panic!("expected an unreadable skip, got {:?}", mirror.skipped[0]);
    };
    assert!(
        detail.contains("decompressed bytes ran past the declared 1024"),
        "{detail}"
    );
    assert_eq!(requests.lock().unwrap().content_requests(), 1);
    assert_eq!(
        blob_count(directory.path()),
        0,
        "an aborted transfer must leave nothing behind, staged or otherwise"
    );
    assert_eq!(mirror.stats.bytes_transferred, 0);
}

/// The sanity ceiling is a disk-space guard on what the archive *claims*, so it has to fire
/// before a byte of content is requested rather than partway through one.
#[test]
fn a_declared_size_past_the_ceiling_is_refused_before_any_transfer() {
    let mut session = ArchivedSession::claude(32, TRANSCRIPT, "identity");
    session.declared_original_bytes = Some(MAX_DECLARED_TRANSCRIPT_BYTES + 1);

    let (base, requests) = spawn_archive(vec![session]);
    let (directory, cache) = cache();
    let client = ReadClient::connect(&base).unwrap();

    let mirror = sync::sync(&client, &cache, "2026-07-18T00:00:00.000Z", 2).unwrap();
    assert!(mirror.sessions.is_empty());
    let SkipReason::Unreadable(detail) = &mirror.skipped[0].reason else {
        panic!("expected an unreadable skip, got {:?}", mirror.skipped[0]);
    };
    assert!(detail.contains("declared-size ceiling"), "{detail}");
    assert_eq!(
        requests.lock().unwrap().content_requests(),
        0,
        "an absurd declaration must not be dignified with a transfer"
    );
    assert_eq!(blob_count(directory.path()), 0);
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
