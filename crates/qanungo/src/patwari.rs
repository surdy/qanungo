//! The Patwari read client: the three archive surfaces a coaching report needs.
//!
//! A report is a *read* of somebody else's archive server. Patwari is LAN-only, serves roughly
//! eight concurrent requests, and times out at 30s, so this client is deliberately polite: one
//! listing traversal that follows only the server's own `next_cursor`, one detail request per
//! session, one download per transcript that is not already cached, and no retries. A read-side
//! client that retries into a busy archive turns a slow server into an unavailable one.
//!
//! # The three surfaces
//!
//! 1. `GET /api/v1/sessions?activity_from=…` — the report window. Patwari projects each
//!    session's *latest* completed snapshot onto the session row, which is exactly what a fold
//!    wants: one transcript per session, already the newest capture of it.
//! 2. `GET /api/v1/snapshots/{id}` — that snapshot's canonical manifest and its complete
//!    artifact list in one unpaginated response. The manifest is where capture provenance is
//!    stated (`session.source_agent`, `capture.artifact_set_version`), and those two decide
//!    which `munshi-transcript` interpreter may read the transcript at all.
//! 3. `GET /api/v1/artifacts/{id}/content` — the stored bytes, with the artifact's declared
//!    sizes, digests, and compression in `x-patwari-*` headers.
//!
//! There is a fourth, requested only for the sessions that need it:
//! `GET /api/v1/sessions/{id}/snapshots` — that session's snapshots, newest first. A projected
//! `latest_snapshot` can be a degenerate capture that shadows a complete sibling (munshi #78),
//! and this is how the mirror finds the sibling.
//!
//! # The verified download
//!
//! [`ReadClient::download_transcript`] streams. The stored bytes are read off the socket a buffer
//! at a time, hashed and counted as they arrive, decoded per the declared compression, and the
//! recovered original bytes are hashed, counted, and written straight through to the caller's
//! sink.
//!
//! Peak memory is a fixed set of buffers, whatever the transcript weighs — a 231 MB session and a
//! 4 KB one cost the same RAM. Per download in flight that is the transfer buffer
//! ([`TRANSFER_BUFFER_BYTES`], 256 KiB), the socket reader (`http::STREAM_BUFFER_BYTES`, 64 KiB),
//! the decoder's output buffer (libzstd's `ZSTD_DStreamOutSize`, ~128 KiB), the decompression
//! window ([`MAX_DECOMPRESSION_WINDOW_LOG`], 8 MiB), and the cache's write buffer
//! (`cache::STAGE_BUFFER_BYTES`, 64 KiB) — call it 8.5 MiB, times the worker count.
//!
//! The window is the only one of those that is not a constant we chose outright, and it is the
//! only one a hostile artifact could otherwise inflate, so it is pinned explicitly below rather
//! than left at libzstd's default.
//!
//! Nothing is *accepted* until every declaration has checked out: the transferred stored bytes
//! must match the declared stored size and digest, and the recovered original must match the
//! declared original size/digest and the digest the listing already promised. Because the bytes
//! are streamed to a sink rather than returned, the sink is responsible for the last step — the
//! blob cache stages a write and renames it into place only after this returns `Ok`, so bytes
//! that fail verification are unlinked rather than cached.
//!
//! # Bounding a transfer without capping a transcript
//!
//! There is no ceiling on how large a transcript may be. There are two bounds that are not that:
//!
//! - **The declared sizes bound the transfer.** The moment actual stored bytes exceed the
//!   declared stored size, or actual decompressed bytes exceed the declared original size, the
//!   transfer aborts — mid-stream, before the excess is written anywhere. A zstd bomb and a
//!   server that lies about a length both stop at the first byte past the promise, so neither can
//!   spend memory or disk that was not declared up front.
//! - **[`MAX_DECLARED_TRANSCRIPT_BYTES`] bounds the declarations.** A listing that claims an
//!   absurd size is refused before a byte moves, purely so a lying archive cannot fill the disk.
//!
//! The declarations are worth trusting as bounds because the archive verified them at ingest and
//! re-derives them from the canonical manifest on every read.

use std::io::{self, Read, Write};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::http::{self, Endpoint, HttpError, StreamingResponse};

/// The API base path every Patwari route is nested under.
const API_BASE: &str = "/api/v1";
/// Socket timeout, matching the server's own request timeout.
///
/// This bounds `connect` and each individual socket read, not the request as a whole (see
/// [`crate::http`]). On the JSON surfaces that amounts to the same thing, because a listing
/// arrives in one read. On a streamed download it deliberately does not: the transfer may run
/// well past 30s as long as bytes keep arriving, and it is the *archive's* whole-body deadline —
/// `PATWARI_REQUEST_TIMEOUT`, armed when it constructs the download response — that decides how
/// long a large transcript actually has. A download the server cuts short comes up short of its
/// declared stored size and is refused as a verification failure.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Page size requested from a listing — Patwari's maximum, so a window costs the fewest pages.
const LISTING_PAGE_SIZE: usize = 100;
/// Guards the pagination loop against a peer that never stops returning cursors.
const MAX_LISTING_PAGES: usize = 10_000;
/// Sanity ceiling on the sizes an artifact *declares*, in either its stored or its original form.
///
/// This is a disk-space guard, not a memory bound. The download streams, so a transcript costs
/// the same fixed set of buffers however large it is (see the module docs); what this stops is a
/// lying listing talking the mirror into filling the cache filesystem before a single digest can
/// disagree with it. It is deliberately far above any plausible transcript — a coaching report
/// should never see this fire, and if it does, the archive is wrong rather than the session long.
pub const MAX_DECLARED_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Stored bytes read from the socket per pass. One of the fixed buffers a download's peak memory
/// is made of; see the module docs for the rest.
const TRANSFER_BUFFER_BYTES: usize = 256 * 1024;
/// Base-2 log of the largest zstd back-reference window this client will decode with: 2^23, or
/// 8 MiB.
///
/// libzstd defaults `windowLogMax` to 2^27 — 128 MiB of window per decoder, which a hostile frame
/// can demand simply by declaring it, and which four mirror workers would turn into half a
/// gigabyte. Nothing the archive actually holds needs anywhere near that: Munshi compresses
/// transcripts at zstd level 3, whose window is 2^21 (2 MiB), and even level 19 stays at 2^23. So
/// this is set to 2^23 — comfortable headroom over anything an honest capture produces, and a
/// hard ceiling on what a dishonest one can cost. A frame declaring a larger window fails to
/// decode and is refused as a [`PatwariError::Decompression`].
const MAX_DECOMPRESSION_WINDOW_LOG: u32 = 23;
/// Longest `error.code` this client will render. Comfortably over Patwari's own longest code and
/// far under anything that could turn a Gaps line into a paragraph.
const MAX_ERROR_CODE_CHARS: usize = 64;
/// Stands in for an `error.code` that is not shaped like one.
const INVALID_ERROR_CODE: &str = "invalid-error-code";

/// The reserved logical path of the raw transcript inside a Munshi artifact set (Patwari ADR
/// 0005: artifact roles are conveyed by logical path alone).
pub const TRANSCRIPT_LOGICAL_PATH: &str = "transcript.jsonl";

#[derive(Debug, Error)]
pub enum PatwariError {
    #[error("patwari request failed: {0}")]
    Http(#[from] HttpError),
    #[error("patwari answered {status}{}", .code.as_ref().map(|code| format!(" ({code})")).unwrap_or_default())]
    Status { status: u16, code: Option<String> },
    #[error("patwari response was unintelligible: {0}")]
    Protocol(String),
    #[error("archived content failed verification: {0}")]
    Verification(String),
    #[error("stored bytes could not be decompressed: {0}")]
    Decompression(String),
    #[error("verified bytes could not be written locally: {0}")]
    Sink(String),
    #[error("artifact declares {size_bytes} bytes, over the {ceiling}-byte declared-size ceiling")]
    DeclaredSizeRefused { size_bytes: u64, ceiling: u64 },
}

/// Page size requested from a session's own snapshot listing. One page is enough: the listing is
/// newest-first, and the sibling that carries a transcript is by construction close to the top.
const SNAPSHOT_LISTING_PAGE_SIZE: usize = 50;

/// One session in the report window, with the context Patwari projects from its latest
/// completed snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedSession {
    /// Patwari's own session identity — not the harness session ID.
    pub session_id: String,
    /// The harness that produced the transcript, as the manifest states it (`claude-code`,
    /// `copilot-cli`, `codex-cli`).
    pub source_agent: String,
    pub snapshot_id: String,
    /// Server-side completion time of the latest snapshot. This is *archive* time, not
    /// transcript time: it selects the window, while the metrics use record timestamps.
    pub completed_at: String,
}

/// One entry in a session's snapshot listing: enough to ask for the snapshot itself, and the
/// completion time the archive ordered it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRef {
    pub snapshot_id: String,
    /// Server-side completion time. Archive time, not transcript time.
    pub completed_at: String,
}

/// One snapshot's provenance and artifact set, read in a single request.
#[derive(Debug, Clone)]
pub struct SnapshotDetail {
    pub source_agent: String,
    pub artifact_set_version: u16,
    pub artifacts: Vec<ListedArtifact>,
}

impl SnapshotDetail {
    /// The artifact holding the raw transcript, when the set contains one. A snapshot without
    /// one is a real archive state (a summary-only capture), not an error.
    pub fn transcript(&self) -> Option<&ListedArtifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.logical_path == TRANSCRIPT_LOGICAL_PATH)
    }
}

/// One artifact as the snapshot describes it: everything needed to bound, fetch, cross-check,
/// and content-address a download without asking the server again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedArtifact {
    pub artifact_id: String,
    pub logical_path: String,
    /// Bare lowercase hex, with Patwari's `sha256:` document prefix stripped. This is the
    /// content hash of the transcript itself, and therefore both the blob-cache key and the
    /// `source_hash` a finding cites.
    pub original_sha256: String,
    pub original_size_bytes: u64,
    pub stored_size_bytes: u64,
    pub content_url: String,
}

/// A synchronous Patwari read client bound to one archive server.
#[derive(Debug, Clone)]
pub struct ReadClient {
    endpoint: Endpoint,
    timeout: Duration,
}

impl ReadClient {
    /// Binds to `base_url` (`http://host:port` or `https://host`).
    ///
    /// # Errors
    ///
    /// Returns an error when the URL is not a usable `http(s)` endpoint.
    pub fn connect(base_url: &str) -> Result<Self, PatwariError> {
        Ok(Self {
            endpoint: http::parse_endpoint(base_url.trim_end_matches('/'))?,
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// Lists every session whose latest snapshot completed at or after `activity_from`,
    /// following only the cursors the server returns.
    ///
    /// # Errors
    ///
    /// Returns an error when a page cannot be fetched or does not carry the documented fields.
    pub fn list_sessions(&self, activity_from: &str) -> Result<Vec<ListedSession>, PatwariError> {
        let mut sessions = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LISTING_PAGES {
            let mut target = format!(
                "{API_BASE}/sessions?limit={LISTING_PAGE_SIZE}&activity_from={}",
                http::encode_value(activity_from)
            );
            if let Some(cursor) = &cursor {
                target.push_str("&cursor=");
                target.push_str(&http::encode_value(cursor));
            }
            let page = self.get_json(&target)?;
            let items = page.get("items").and_then(Value::as_array).ok_or_else(|| {
                PatwariError::Protocol("session listing missing items".to_owned())
            })?;
            for item in items {
                sessions.push(listed_session(item)?);
            }
            cursor = page
                .get("next_cursor")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if cursor.is_none() {
                return Ok(sessions);
            }
        }
        Err(PatwariError::Protocol(
            "session listing never stopped returning cursors".to_owned(),
        ))
    }

    /// Lists one session's own snapshots, newest first, as the archive orders them.
    ///
    /// Only the first page is read (at most [`SNAPSHOT_LISTING_PAGE_SIZE`] snapshots): this
    /// exists to find a complete sibling of a degenerate `latest_snapshot`, and a sibling further
    /// back than fifty captures is not one this client will chase across pages of a LAN archive
    /// it is trying to be polite to.
    ///
    /// # Errors
    ///
    /// Returns an error when the listing cannot be fetched or does not carry the documented
    /// fields.
    pub fn session_snapshots(&self, session_id: &str) -> Result<Vec<SnapshotRef>, PatwariError> {
        let target = format!(
            "{API_BASE}/sessions/{}/snapshots?limit={SNAPSHOT_LISTING_PAGE_SIZE}",
            http::encode_value(session_id)
        );
        let page = self.get_json(&target)?;
        page.get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| PatwariError::Protocol("snapshot listing missing items".to_owned()))?
            .iter()
            .map(|item| {
                Ok(SnapshotRef {
                    snapshot_id: required_str(item, "snapshot_id")?,
                    completed_at: required_str(item, "completed_at")?,
                })
            })
            .collect()
    }

    /// Reads one snapshot's provenance and complete artifact list.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be fetched or its manifest omits the capture
    /// provenance that decides how the transcript may be interpreted.
    pub fn snapshot(&self, snapshot_id: &str) -> Result<SnapshotDetail, PatwariError> {
        let target = format!("{API_BASE}/snapshots/{}", http::encode_value(snapshot_id));
        let value = self.get_json(&target)?;
        let source_agent = nested_str(&value, &["manifest", "session", "source_agent"])
            .ok_or_else(|| {
                PatwariError::Protocol("snapshot manifest missing session.source_agent".to_owned())
            })?;
        let artifact_set_version =
            nested_u64(&value, &["manifest", "capture", "artifact_set_version"])
                .and_then(|version| u16::try_from(version).ok())
                .ok_or_else(|| {
                    PatwariError::Protocol(
                        "snapshot manifest missing capture.artifact_set_version".to_owned(),
                    )
                })?;
        let artifacts = value
            .get("artifacts")
            .and_then(Value::as_array)
            .ok_or_else(|| PatwariError::Protocol("snapshot missing artifacts".to_owned()))?
            .iter()
            .map(listed_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SnapshotDetail {
            source_agent,
            artifact_set_version,
            artifacts,
        })
    }

    /// Streams one transcript artifact into `sink`, verifying every declaration the archive made
    /// about it, and reports what actually moved.
    ///
    /// `sink` receives the *original* (decompressed) bytes, and receives them before they have
    /// been fully verified — the digests are only complete once the last byte is through. A
    /// caller must therefore treat what it wrote as provisional until this returns `Ok`;
    /// [`BlobCache::stage`](crate::cache::BlobCache::stage) exists precisely to make that
    /// discipline the default.
    ///
    /// # Errors
    ///
    /// Returns [`PatwariError::DeclaredSizeRefused`] before any transfer when either declared
    /// size is over [`MAX_DECLARED_TRANSCRIPT_BYTES`], and a [`PatwariError::Verification`] when
    /// the transfer runs past a declared size or the transferred or recovered bytes do not hash
    /// to what the archive declared.
    pub fn download_transcript(
        &self,
        artifact: &ListedArtifact,
        sink: &mut impl Write,
    ) -> Result<DownloadReceipt, PatwariError> {
        for size_bytes in [artifact.stored_size_bytes, artifact.original_size_bytes] {
            if size_bytes > MAX_DECLARED_TRANSCRIPT_BYTES {
                return Err(PatwariError::DeclaredSizeRefused {
                    size_bytes,
                    ceiling: MAX_DECLARED_TRANSCRIPT_BYTES,
                });
            }
        }
        let mut response =
            http::get_streaming(&self.endpoint, self.timeout, &artifact.content_url)?;
        if response.status != 200 {
            return Err(PatwariError::Status {
                status: response.status,
                code: error_code(&response.error_body()),
            });
        }

        let compression = response
            .header("x-patwari-compression")
            .ok_or_else(|| {
                PatwariError::Protocol("artifact content missing compression header".to_owned())
            })?
            .to_owned();
        let stored_sha = header_digest(&response, "x-patwari-stored-sha256")?;
        let original_sha = header_digest(&response, "x-patwari-original-sha256")?;
        let stored_size = header_u64(&response, "x-patwari-stored-size-bytes")?;
        let original_size = header_u64(&response, "x-patwari-original-size-bytes")?;

        // The listing and the content headers are two renderings of the same manifest row, so
        // they have to agree before either is trusted as the bound the transfer aborts against.
        // Disagreement is not a size problem to resolve by picking the smaller one; it means the
        // archive is contradicting itself about the artifact under this digest.
        if stored_size != artifact.stored_size_bytes
            || original_size != artifact.original_size_bytes
        {
            return Err(PatwariError::Verification(format!(
                "declared sizes disagree: the listing says {}/{} stored/original, the content \
                 headers say {stored_size}/{original_size}",
                artifact.stored_size_bytes, artifact.original_size_bytes
            )));
        }
        if !matches!(compression.as_str(), "identity" | "zstd") {
            return Err(PatwariError::Protocol(format!(
                "unknown compression `{compression}`"
            )));
        }

        // The transfer: stored bytes metered on the way in, original bytes metered on the way
        // out, neither side ever holding more than a buffer.
        let mut stored = Meter::new(stored_size);
        let mut original = MeteredSink::new(sink, original_size);
        let transferred = {
            let mut decode = match compression.as_str() {
                "zstd" => {
                    let mut decoder = zstd::stream::write::Decoder::new(&mut original)
                        .map_err(|error| PatwariError::Decompression(error.to_string()))?;
                    decoder
                        .window_log_max(MAX_DECOMPRESSION_WINDOW_LOG)
                        .map_err(|error| PatwariError::Decompression(error.to_string()))?;
                    Decode::Zstd(Box::new(decoder))
                }
                _ => Decode::Identity(&mut original),
            };
            transfer(response.body(), &mut stored, &mut decode)
        };

        match transferred {
            Ok(()) => {}
            Err(TransferError::Transport(error)) => return Err(PatwariError::Http(error)),
            Err(TransferError::StoredOverflow) => {
                return Err(PatwariError::Verification(format!(
                    "stored bytes ran past the declared {stored_size}, aborted after {} bytes",
                    stored.count
                )));
            }
            Err(TransferError::Decode(error)) => {
                return Err(if original.overflowed {
                    PatwariError::Verification(format!(
                        "decompressed bytes ran past the declared {original_size}, aborted after \
                         {} bytes",
                        original.meter.count
                    ))
                } else if original.sink_failed {
                    PatwariError::Sink(error.to_string())
                } else {
                    PatwariError::Decompression(error.to_string())
                });
            }
        }

        // 1. The transferred stored bytes must be exactly what the archive says it stores. A
        //    short body — a dropped connection, or the server's own download deadline expiring
        //    mid-response — lands here rather than being cached as a prefix.
        if stored.count != stored_size {
            return Err(PatwariError::Verification(format!(
                "stored size mismatch: got {} bytes, expected {stored_size}",
                stored.count
            )));
        }
        if stored.digest() != stored_sha {
            return Err(PatwariError::Verification(
                "stored content hash does not match the archive's declared stored hash".to_owned(),
            ));
        }

        // 2. The recovered original must match both the response headers and the digest the
        //    snapshot listing already promised — which is the cache key and the cited
        //    `source_hash`, so a mismatch would corrupt evidence, not just a download.
        if original.meter.count != original_size {
            return Err(PatwariError::Verification(format!(
                "original size mismatch: got {} bytes, expected {original_size}",
                original.meter.count
            )));
        }
        let digest = original.meter.digest();
        if digest != original_sha {
            return Err(PatwariError::Verification(
                "decompressed content hash does not match the archive's declared original hash"
                    .to_owned(),
            ));
        }
        if digest != artifact.original_sha256 {
            return Err(PatwariError::Verification(format!(
                "decompressed content hash sha256:{digest} does not match the listing's declared \
                 sha256:{}",
                artifact.original_sha256
            )));
        }
        Ok(DownloadReceipt {
            stored_bytes: stored.count,
            original_bytes: original.meter.count,
        })
    }

    fn get_json(&self, target: &str) -> Result<Value, PatwariError> {
        let response = http::get(&self.endpoint, self.timeout, target)?;
        if response.status != 200 {
            return Err(PatwariError::Status {
                status: response.status,
                code: error_code(&response.body),
            });
        }
        serde_json::from_slice(&response.body)
            .map_err(|error| PatwariError::Protocol(error.to_string()))
    }
}

/// What one verified download actually moved. The two numbers are the footer's two numbers: the
/// stored form crossed the wire, the original form is what a fold will read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadReceipt {
    /// Stored (compressed) bytes transferred and verified.
    pub stored_bytes: u64,
    /// Original bytes recovered, verified, and written to the sink.
    pub original_bytes: u64,
}

/// Counts and hashes bytes as they pass, against a declared total.
struct Meter {
    hasher: Sha256,
    count: u64,
    declared: u64,
}

impl Meter {
    fn new(declared: u64) -> Self {
        Self {
            hasher: Sha256::new(),
            count: 0,
            declared,
        }
    }

    /// Records `bytes`, reporting whether the declared total has been overrun. The overrun is
    /// checked *before* the bytes are counted as accepted, so the transfer stops at the first
    /// byte past the promise rather than one buffer later.
    fn observe(&mut self, bytes: &[u8]) -> bool {
        if self.count + bytes.len() as u64 > self.declared {
            return false;
        }
        self.count += bytes.len() as u64;
        self.hasher.update(bytes);
        true
    }

    fn digest(&self) -> String {
        hex_digest(self.hasher.clone().finalize())
    }
}

/// The original-byte side of a transfer: meters what the decoder produces and passes it straight
/// through to the caller's sink, so decompressed bytes are never accumulated.
///
/// The two failure flags exist because a `Write` can only fail as an `io::Error`, and the three
/// ways this fails are three different findings: an artifact that decompresses past its declared
/// size is a *verification* failure, a sink that will not take bytes is a *local* failure, and
/// anything else came out of the decoder.
struct MeteredSink<'a, W: Write> {
    sink: &'a mut W,
    meter: Meter,
    overflowed: bool,
    sink_failed: bool,
}

impl<'a, W: Write> MeteredSink<'a, W> {
    fn new(sink: &'a mut W, declared: u64) -> Self {
        Self {
            sink,
            meter: Meter::new(declared),
            overflowed: false,
            sink_failed: false,
        }
    }
}

impl<W: Write> Write for MeteredSink<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.meter.observe(bytes) {
            self.overflowed = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "decompressed output ran past the declared original size",
            ));
        }
        self.sink.write_all(bytes).inspect_err(|_| {
            self.sink_failed = true;
        })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush().inspect_err(|_| {
            self.sink_failed = true;
        })
    }
}

/// Streaming decode of stored bytes into original bytes.
///
/// Push-shaped rather than pull-shaped on purpose: with the decoder as a `Write`, the transfer
/// loop keeps ownership of the socket and of the stored-side meter, so the stored bound is
/// enforced by the same loop that reads the socket rather than from inside a decoder's buffer.
enum Decode<'a, W: Write> {
    Identity(&'a mut W),
    Zstd(Box<zstd::stream::write::Decoder<'static, &'a mut W>>),
}

impl<W: Write> Decode<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Identity(sink) => sink.write_all(bytes),
            Self::Zstd(decoder) => decoder.write_all(bytes),
        }
    }

    /// Flushes whatever the decoder is still holding. Zstandard decompression produces its output
    /// as input arrives, so there is no frame to finalize here; a truncated frame simply yields
    /// fewer original bytes than declared, which the size and digest checks then refuse.
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Identity(sink) => sink.flush(),
            Self::Zstd(decoder) => decoder.flush(),
        }
    }
}

/// How a transfer stopped, kept separate from [`PatwariError`] so the classification happens once
/// the borrows on the meters have ended and both sides' counters can be read.
enum TransferError {
    Transport(HttpError),
    StoredOverflow,
    Decode(io::Error),
}

/// Reads the body to its end, metering the stored bytes and pushing them through the decoder.
fn transfer<W: Write>(
    body: &mut impl Read,
    stored: &mut Meter,
    decode: &mut Decode<'_, W>,
) -> Result<(), TransferError> {
    let mut buffer = vec![0_u8; TRANSFER_BUFFER_BYTES];
    loop {
        let read = body
            .read(&mut buffer)
            .map_err(|error| TransferError::Transport(HttpError::Transport(error.to_string())))?;
        if read == 0 {
            break;
        }
        if !stored.observe(&buffer[..read]) {
            return Err(TransferError::StoredOverflow);
        }
        decode
            .write_all(&buffer[..read])
            .map_err(TransferError::Decode)?;
    }
    decode.flush().map_err(TransferError::Decode)
}

fn listed_session(item: &Value) -> Result<ListedSession, PatwariError> {
    Ok(ListedSession {
        session_id: required_str(item, "session_id")?,
        source_agent: required_str(item, "source_agent")?,
        snapshot_id: nested_str(item, &["latest_snapshot", "snapshot_id"]).ok_or_else(|| {
            PatwariError::Protocol("session missing latest_snapshot.snapshot_id".to_owned())
        })?,
        completed_at: nested_str(item, &["latest_snapshot", "completed_at"]).ok_or_else(|| {
            PatwariError::Protocol("session missing latest_snapshot.completed_at".to_owned())
        })?,
    })
}

fn listed_artifact(item: &Value) -> Result<ListedArtifact, PatwariError> {
    Ok(ListedArtifact {
        artifact_id: required_str(item, "artifact_id")?,
        logical_path: required_str(item, "logical_path")?,
        original_sha256: strip_digest(&required_str(item, "original_sha256")?),
        original_size_bytes: required_u64(item, "original_size_bytes")?,
        stored_size_bytes: required_u64(item, "stored_size_bytes")?,
        content_url: required_str(item, "content_url")?,
    })
}

fn required_str(value: &Value, key: &str) -> Result<String, PatwariError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| PatwariError::Protocol(format!("response item missing {key}")))
}

fn required_u64(value: &Value, key: &str) -> Result<u64, PatwariError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| PatwariError::Protocol(format!("response item missing {key}")))
}

fn nested_str(value: &Value, path: &[&str]) -> Option<String> {
    nested(value, path)?.as_str().map(ToOwned::to_owned)
}

fn nested_u64(value: &Value, path: &[&str]) -> Option<u64> {
    nested(value, path)?.as_u64()
}

fn nested<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |value, key| value.get(*key))
}

/// Patwari renders digests as `sha256:<hex>` documents; everything downstream keys on the bare
/// hex, so the prefix is stripped once here.
pub fn strip_digest(value: &str) -> String {
    value.strip_prefix("sha256:").unwrap_or(value).to_owned()
}

/// Patwari's stable machine-readable `error.code`, when the body carries one — clamped to a shape
/// that is safe to render.
///
/// This is the only string this client lifts out of a response body it did not ask to parse, and
/// it ends up in the report's Gaps section, so it is the one place a peer gets to put characters
/// of its choosing into a document sworn to carry no upstream free text. The archive's own codes
/// are short snake-case tokens (`artifact_not_found`, `request_timeout`); anything that is not
/// shaped like one is not a code, whether the peer is confused, compromised, or not Patwari at
/// all. Such a value is replaced rather than truncated, because a prefix of arbitrary text is
/// still arbitrary text.
fn error_code(body: &[u8]) -> Option<String> {
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| nested_str(&value, &["error", "code"]))?;
    Some(
        if code.len() <= MAX_ERROR_CODE_CHARS && !code.is_empty() && code.bytes().all(is_code_byte)
        {
            code
        } else {
            INVALID_ERROR_CODE.to_owned()
        },
    )
}

/// Whether a byte may appear in a rendered `error.code`: lowercase alphanumerics, `_`, and `-`.
fn is_code_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
}

fn header_digest(response: &StreamingResponse, name: &str) -> Result<String, PatwariError> {
    let value = response
        .header(name)
        .ok_or_else(|| PatwariError::Protocol(format!("artifact content missing {name}")))?;
    Ok(strip_digest(value))
}

fn header_u64(response: &StreamingResponse, name: &str) -> Result<u64, PatwariError> {
    response
        .header(name)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| PatwariError::Protocol(format!("artifact content missing {name}")))
}

/// Lowercase hexadecimal sha256 of `bytes`. One-shot: the download path hashes incrementally
/// instead, because it never holds the bytes it is hashing.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

/// Renders a digest as lowercase hexadecimal.
fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = digest.as_ref();
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_document_digest_prefix_idempotently() {
        assert_eq!(strip_digest("sha256:abc123"), "abc123");
        assert_eq!(strip_digest("abc123"), "abc123");
    }

    /// The bound has to admit an artifact that is exactly as large as it said it would be, and
    /// refuse the very next byte. Off by one in either direction is either a rejected transcript
    /// or an unbounded one.
    #[test]
    fn a_meter_admits_the_declared_size_exactly_and_refuses_the_next_byte() {
        let mut meter = Meter::new(4);
        assert!(meter.observe(b"ab"));
        assert!(meter.observe(b"cd"));
        assert_eq!(meter.count, 4);
        assert_eq!(meter.digest(), sha256_hex(b"abcd"));

        assert!(!meter.observe(b"e"), "one byte past the declaration");
        assert_eq!(meter.count, 4, "a refused write must not be counted");
        assert_eq!(
            meter.digest(),
            sha256_hex(b"abcd"),
            "nor may it reach the hash"
        );
    }

    #[test]
    fn a_meter_refuses_a_write_that_straddles_the_declared_size() {
        let mut meter = Meter::new(4);
        assert!(!meter.observe(b"abcde"));
        assert_eq!(meter.count, 0);
    }

    /// `error.code` is the only upstream string that reaches a rendered report, so the clamp on
    /// it is a redaction control, not tidiness.
    #[test]
    fn an_error_code_is_rendered_only_when_it_is_shaped_like_one() {
        let code = |body: &str| error_code(body.as_bytes());

        assert_eq!(
            code(r#"{"error":{"code":"artifact_not_found"}}"#).as_deref(),
            Some("artifact_not_found")
        );
        assert_eq!(
            code(r#"{"error":{"code":"request-timeout-2"}}"#).as_deref(),
            Some("request-timeout-2")
        );

        // A body with no code at all stays absent rather than becoming a placeholder.
        assert_eq!(code(r#"{"error":{"message":"nope"}}"#), None);
        assert_eq!(code("not json at all"), None);

        // Anything else is replaced wholesale: a prefix of arbitrary text is still arbitrary text.
        for hostile in [
            r#"{"error":{"code":"has spaces"}}"#,
            r#"{"error":{"code":"UPPERCASE"}}"#,
            r#"{"error":{"code":"newline\ninjected"}}"#,
            r#"{"error":{"code":"markup <b>|</b>"}}"#,
            r#"{"error":{"code":""}}"#,
        ] {
            assert_eq!(
                code(hostile).as_deref(),
                Some(INVALID_ERROR_CODE),
                "{hostile}"
            );
        }

        let overlong = "a".repeat(MAX_ERROR_CODE_CHARS + 1);
        assert_eq!(
            code(&format!(r#"{{"error":{{"code":"{overlong}"}}}}"#)).as_deref(),
            Some(INVALID_ERROR_CODE)
        );
        let at_the_limit = "a".repeat(MAX_ERROR_CODE_CHARS);
        assert_eq!(
            code(&format!(r#"{{"error":{{"code":"{at_the_limit}"}}}}"#)).as_deref(),
            Some(at_the_limit.as_str())
        );
    }

    #[test]
    fn hashes_bytes_as_lowercase_hex() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn reads_a_session_listing_item() {
        let item = serde_json::json!({
            "session_id": "11111111-1111-4111-8111-111111111111",
            "source_agent": "claude-code",
            "latest_snapshot": {
                "snapshot_id": "22222222-2222-4222-8222-222222222222",
                "completed_at": "2026-08-16T10:00:00.000Z",
            },
        });
        let session = listed_session(&item).unwrap();
        assert_eq!(session.source_agent, "claude-code");
        assert_eq!(session.snapshot_id, "22222222-2222-4222-8222-222222222222");
    }

    #[test]
    fn a_session_without_a_projected_snapshot_is_a_protocol_error() {
        let item = serde_json::json!({
            "session_id": "11111111-1111-4111-8111-111111111111",
            "source_agent": "claude-code",
        });
        assert!(matches!(
            listed_session(&item),
            Err(PatwariError::Protocol(_))
        ));
    }

    #[test]
    fn finds_the_transcript_by_its_reserved_logical_path() {
        let detail = SnapshotDetail {
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            artifacts: vec![
                ListedArtifact {
                    artifact_id: "a".to_owned(),
                    logical_path: "summary.md".to_owned(),
                    original_sha256: "aa".to_owned(),
                    original_size_bytes: 1,
                    stored_size_bytes: 1,
                    content_url: "/x".to_owned(),
                },
                ListedArtifact {
                    artifact_id: "b".to_owned(),
                    logical_path: TRANSCRIPT_LOGICAL_PATH.to_owned(),
                    original_sha256: "bb".to_owned(),
                    original_size_bytes: 2,
                    stored_size_bytes: 2,
                    content_url: "/y".to_owned(),
                },
            ],
        };
        assert_eq!(detail.transcript().unwrap().artifact_id, "b");

        let summary_only = SnapshotDetail {
            artifacts: detail.artifacts[..1].to_vec(),
            ..detail
        };
        assert!(summary_only.transcript().is_none());
    }
}
