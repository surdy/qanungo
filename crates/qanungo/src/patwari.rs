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
//! # The verified download
//!
//! [`ReadClient::download_transcript`] never returns a byte it has not verified: the
//! transferred stored bytes must match the declared stored size and digest, the bytes are then
//! decoded per the declared compression, and the recovered original must match both the
//! declared original size/digest and the digest the listing already promised. The size gate
//! runs *before* the request, on both the stored and the original size, so a highly
//! compressible artifact cannot decompress into unbounded memory.

use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::http::{self, Endpoint, HttpError};

/// The API base path every Patwari route is nested under.
const API_BASE: &str = "/api/v1";
/// Network timeout for a single request, matching the server's own request timeout.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Page size requested from a listing — Patwari's maximum, so a window costs the fewest pages.
const LISTING_PAGE_SIZE: usize = 100;
/// Guards the pagination loop against a peer that never stops returning cursors.
const MAX_LISTING_PAGES: usize = 10_000;
/// Upper bound on one transcript, in both its stored and its original form. A transcript past
/// this is skipped and counted rather than folded; nothing in a coaching report is worth
/// materializing a quarter-gigabyte of JSONL for.
pub const MAX_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
/// Read-bound headroom over the declared stored size, covering the status line, the response
/// headers, and chunked-encoding framing so an artifact at the cap still transfers completely.
const RESPONSE_FRAMING_ALLOWANCE_BYTES: usize = 64 * 1024;

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
    #[error("artifact declares {size_bytes} bytes, over the {cap}-byte download cap")]
    TooLarge { size_bytes: u64, cap: u64 },
}

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

    /// Downloads one transcript artifact and returns its verified original bytes.
    ///
    /// # Errors
    ///
    /// Returns [`PatwariError::TooLarge`] before any transfer when either declared size is over
    /// [`MAX_TRANSCRIPT_BYTES`], and a verification error when the transferred or recovered
    /// bytes do not match what the archive declared.
    pub fn download_transcript(&self, artifact: &ListedArtifact) -> Result<Vec<u8>, PatwariError> {
        for size_bytes in [artifact.stored_size_bytes, artifact.original_size_bytes] {
            if size_bytes > MAX_TRANSCRIPT_BYTES {
                return Err(PatwariError::TooLarge {
                    size_bytes,
                    cap: MAX_TRANSCRIPT_BYTES,
                });
            }
        }
        let limit = usize::try_from(artifact.stored_size_bytes)
            .unwrap_or(usize::MAX)
            .saturating_add(RESPONSE_FRAMING_ALLOWANCE_BYTES);
        let response =
            http::get_with_limit(&self.endpoint, self.timeout, &artifact.content_url, limit)?;
        if response.status != 200 {
            return Err(PatwariError::Status {
                status: response.status,
                code: error_code(&response.body),
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

        // 1. The transferred stored bytes must be exactly what the archive says it stores.
        let stored_bytes = response.body;
        if stored_bytes.len() as u64 != stored_size {
            return Err(PatwariError::Verification(format!(
                "stored size mismatch: got {} bytes, expected {stored_size}",
                stored_bytes.len()
            )));
        }
        if sha256_hex(&stored_bytes) != stored_sha {
            return Err(PatwariError::Verification(
                "stored content hash does not match the archive's declared stored hash".to_owned(),
            ));
        }

        // 2. Decode per the declared compression. The pre-transfer gate already bounded what
        //    this can expand to.
        let original_bytes = match compression.as_str() {
            "identity" => stored_bytes,
            "zstd" => zstd::decode_all(stored_bytes.as_slice())
                .map_err(|error| PatwariError::Decompression(error.to_string()))?,
            other => {
                return Err(PatwariError::Protocol(format!(
                    "unknown compression `{other}`"
                )));
            }
        };

        // 3. The recovered original must match both the response headers and the digest the
        //    snapshot listing already promised — which is the cache key and the cited
        //    `source_hash`, so a mismatch would corrupt evidence, not just a download.
        if original_bytes.len() as u64 != original_size {
            return Err(PatwariError::Verification(format!(
                "original size mismatch: got {} bytes, expected {original_size}",
                original_bytes.len()
            )));
        }
        let digest = sha256_hex(&original_bytes);
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
        Ok(original_bytes)
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

/// Patwari's stable machine-readable `error.code`, when the body carries one.
fn error_code(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| nested_str(&value, &["error", "code"]))
}

fn header_digest(response: &http::Response, name: &str) -> Result<String, PatwariError> {
    let value = response
        .header(name)
        .ok_or_else(|| PatwariError::Protocol(format!("artifact content missing {name}")))?;
    Ok(strip_digest(value))
}

fn header_u64(response: &http::Response, name: &str) -> Result<u64, PatwariError> {
    response
        .header(name)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| PatwariError::Protocol(format!("artifact content missing {name}")))
}

/// Lowercase hexadecimal sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
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
