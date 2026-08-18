//! The minimal mirror: re-list the window, fetch what is not already cached.
//!
//! There is no cursor protocol and no local index (qanungo #7 pulls all of that out of P0). A run
//! lists the sessions whose latest snapshot landed inside the window, resolves each one's
//! transcript artifact, and asks the blob cache whether it already holds that content hash. A
//! naive re-sync is affordable precisely because the expensive part — transferring transcript
//! bytes — is skipped by content hash, and the cheap part is one small JSON request per session.
//!
//! # Being a polite client
//!
//! Patwari is a LAN server that serves about eight concurrent requests behind a 30s timeout.
//! This mirror runs a small fixed worker pool over the session list and never retries: a failed
//! session is recorded as a [`Skip`] and the report says so, which is strictly better than
//! turning a struggling archive into an unavailable one. One session's failure never fails the
//! run; a failure to list the window does, because there is no report to write without it.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::cache::BlobCache;
use crate::patwari::{ListedSession, PatwariError, ReadClient};

/// Worker threads used against the archive. Comfortably under Patwari's concurrency limit, so a
/// report never crowds out the archive's other clients.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// One session's transcript, present in the cache and ready to fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirroredSession {
    /// The transcript's content hash — cache key and cited evidence in one.
    pub source_hash: String,
    /// The harness that produced it, from the snapshot's canonical manifest.
    pub source_agent: String,
    /// The artifact contract the transcript was captured under; decides which interpreter reads
    /// it.
    pub artifact_set_version: u16,
    /// The transcript's size in bytes, as the archive declares it.
    pub size_bytes: u64,
}

/// Why one session contributed nothing to the fold. Recorded rather than swallowed: a report
/// that quietly dropped a third of the window would be worse than one that says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The snapshot has no `transcript.jsonl` artifact — a summary-only capture.
    NoTranscript,
    /// The manifest names a harness this build has no interpreter for.
    UnknownAgent(String),
    /// The archive could not be read for this session. Carries the classified failure's text.
    Unreadable(String),
}

/// One skipped session, named by what could be named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    pub source_agent: String,
    pub reason: SkipReason,
}

/// What the mirror cost, for the instrumentation footer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub sessions_listed: usize,
    /// Transcripts already held under their content hash: no transfer.
    pub cache_hits: u64,
    /// Transcripts fetched and verified this run.
    pub cache_misses: u64,
    /// Bytes of transcript transferred (after decompression) on the misses.
    pub bytes_fetched: u64,
    pub elapsed: Duration,
}

/// The mirror's result: what can be folded, and what could not be.
#[derive(Debug, Clone, Default)]
pub struct Mirror {
    pub sessions: Vec<MirroredSession>,
    pub skipped: Vec<Skip>,
    pub stats: SyncStats,
}

/// Lists the window and ensures every listed session's transcript is in the cache.
///
/// Sessions come back in the archive's own newest-first listing order, so the report is stable
/// across runs even though the workers finish out of order.
///
/// # Errors
///
/// Returns an error only when the window itself cannot be listed. Per-session failures become
/// [`Skip`]s.
pub fn sync(
    client: &ReadClient,
    cache: &BlobCache,
    activity_from: &str,
    concurrency: usize,
) -> Result<Mirror, PatwariError> {
    let started = Instant::now();
    let listed = client.list_sessions(activity_from)?;
    let mut mirror = Mirror {
        stats: SyncStats {
            sessions_listed: listed.len(),
            ..SyncStats::default()
        },
        ..Mirror::default()
    };

    let queue = Mutex::new(listed.into_iter().enumerate());
    let outcomes: Mutex<Vec<(usize, Outcome)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..concurrency.max(1) {
            scope.spawn(|| {
                loop {
                    let Some((index, session)) = lock(&queue).next() else {
                        break;
                    };
                    let outcome = mirror_session(client, cache, &session);
                    lock(&outcomes).push((index, outcome));
                }
            });
        }
    });

    let mut outcomes = outcomes
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    outcomes.sort_by_key(|(index, _)| *index);
    for (_, outcome) in outcomes {
        match outcome {
            Outcome::Mirrored { session, lookup } => {
                match lookup {
                    crate::cache::Lookup::Hit => mirror.stats.cache_hits += 1,
                    crate::cache::Lookup::Miss => {
                        mirror.stats.cache_misses += 1;
                        mirror.stats.bytes_fetched += session.size_bytes;
                    }
                }
                mirror.sessions.push(session);
            }
            Outcome::Skipped(skip) => mirror.skipped.push(skip),
        }
    }
    mirror.stats.elapsed = started.elapsed();
    Ok(mirror)
}

enum Outcome {
    Mirrored {
        session: MirroredSession,
        lookup: crate::cache::Lookup,
    },
    Skipped(Skip),
}

/// Resolves one session's transcript and makes sure the cache holds it.
fn mirror_session(client: &ReadClient, cache: &BlobCache, session: &ListedSession) -> Outcome {
    let skip = |source_agent: &str, reason: SkipReason| {
        Outcome::Skipped(Skip {
            source_agent: source_agent.to_owned(),
            reason,
        })
    };
    let snapshot = match client.snapshot(&session.snapshot_id) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return skip(
                &session.source_agent,
                SkipReason::Unreadable(error.to_string()),
            );
        }
    };
    if crate::metrics::source_for_agent(&snapshot.source_agent).is_none() {
        return skip(
            &snapshot.source_agent,
            SkipReason::UnknownAgent(snapshot.source_agent.clone()),
        );
    }
    let Some(transcript) = snapshot.transcript() else {
        return skip(&snapshot.source_agent, SkipReason::NoTranscript);
    };
    let mirrored = MirroredSession {
        source_hash: transcript.original_sha256.clone(),
        source_agent: snapshot.source_agent.clone(),
        artifact_set_version: snapshot.artifact_set_version,
        size_bytes: transcript.original_size_bytes,
    };

    if cache.contains(&mirrored.source_hash) {
        return Outcome::Mirrored {
            session: mirrored,
            lookup: crate::cache::Lookup::Hit,
        };
    }
    let bytes = match client.download_transcript(transcript) {
        Ok(bytes) => bytes,
        Err(error) => {
            return skip(
                &snapshot.source_agent,
                SkipReason::Unreadable(error.to_string()),
            );
        }
    };
    // The download already proved these bytes hash to `source_hash` against the archive's own
    // declaration, so the cache stores them without re-hashing.
    if let Err(error) = cache.store(&mirrored.source_hash, &bytes) {
        return skip(
            &snapshot.source_agent,
            SkipReason::Unreadable(format!("cache write failed: {error}")),
        );
    }
    Outcome::Mirrored {
        session: mirrored,
        lookup: crate::cache::Lookup::Miss,
    }
}

/// A poisoned worker mutex means another worker panicked; the remaining work is still valid, so
/// the lock is taken anyway rather than cascading the panic across the pool.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_name_the_agent_they_could_not_read() {
        let skip = Skip {
            source_agent: "future-harness".to_owned(),
            reason: SkipReason::UnknownAgent("future-harness".to_owned()),
        };
        assert_eq!(skip.source_agent, "future-harness");
    }

    #[test]
    fn stats_start_empty() {
        let stats = SyncStats::default();
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 0);
        assert_eq!(stats.bytes_fetched, 0);
    }
}
