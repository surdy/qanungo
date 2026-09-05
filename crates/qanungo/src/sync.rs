//! The mirror: re-list the window, fetch what is not already cached.
//!
//! A run lists the sessions whose latest snapshot landed inside the window, resolves each one's
//! artifact, and asks the blob cache whether it already holds that content hash. There is no
//! cursor protocol: Patwari's cursors are bound to one traversal (they carry the first page's
//! high-watermark so a walk is exact while newer rows land), not a token that resumes across
//! runs, and a window listing is a handful of hundred-row pages anyway. The re-list is the cheap
//! part.
//!
//! What was not cheap was resolving: one `GET /snapshots/{id}` per listed session, every run, to
//! learn a content hash this client had already been told — ~700 requests against the real
//! archive, and the whole of a warm sync (qanungo #1). A snapshot is immutable, so that document
//! is now kept in a **snapshot index** beside the blobs ([`BlobCache::snapshot_document`]) and a
//! session whose projected snapshot is indexed *and* whose artifact is already held costs no
//! request at all. The index is consulted only on that hit path: any session the cache cannot
//! serve outright is resolved from a freshly fetched document, so nothing read out of the index
//! ever decides a download or reaches the wire. A warm sync is therefore the listing pages, and
//! a cold one costs exactly what it did.
//!
//! The one place the budget stretches is a session whose projected latest snapshot cannot be
//! used: the mirror then asks for that session's own snapshot listing and takes the newest sibling
//! that can be. See [`usable_snapshot`].
//!
//! # One mirror, two artifacts
//!
//! Which artifact a run wants is an argument ([`Artifact`]), not a second copy of this file.
//! `report` and `cost` want each session's `transcript.jsonl`; `standup` (qanungo #9) wants its
//! `summary.md`. Everything else is identical and has to *stay* identical — the same listing, the
//! same window, the same sibling fallback, the same staged content-addressed write — because two
//! lanes that selected the window differently for the same `--last` would be two lanes describing
//! two different weeks.
//!
//! # Being a polite client
//!
//! Patwari is a LAN server that serves about eight concurrent requests behind a 30s timeout.
//! This mirror runs a small fixed worker pool over the session list and never retries: a failed
//! session is recorded as a [`Skip`] and the report says so, which is strictly better than
//! turning a struggling archive into an unavailable one. One session's failure never fails the
//! run; a failure to list the window does, because there is no report to write without it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::cache::BlobCache;
use crate::patwari::{
    ListedArtifact, ListedSession, MAX_DECLARED_SUMMARY_BYTES, MAX_DECLARED_TRANSCRIPT_BYTES,
    PatwariError, ReadClient, SUMMARY_LOGICAL_PATH, SnapshotDetail, TRANSCRIPT_LOGICAL_PATH,
};

/// Worker threads used against the archive. Comfortably under Patwari's concurrency limit, so a
/// report never crowds out the archive's other clients.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// Hard ceiling on worker threads, whatever the operator asks for. Patwari serves roughly eight
/// concurrent requests: a client that opens more does not go faster, it just occupies every slot
/// the archive has and starves its other readers. The politeness is a property of this client,
/// not a suggestion to the person running it.
pub const MAX_CONCURRENCY: usize = 8;

/// The default must sit inside the ceiling it is defaulted against — checked at compile time so
/// raising one of the two constants without the other cannot build.
const _: () = assert!(DEFAULT_CONCURRENCY >= 1 && DEFAULT_CONCURRENCY <= MAX_CONCURRENCY);

/// Which of a snapshot's artifacts a run mirrors.
///
/// The variants are the two reserved logical paths a Munshi snapshot carries (Patwari ADR 0005),
/// and each one answers the same three questions for the mirror: which artifact to take out of a
/// snapshot, whether a snapshot carrying it is usable at all, and how large a declaration to
/// believe about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Artifact {
    /// `transcript.jsonl` — the raw session, folded by `report` and `cost`.
    Transcript,
    /// `summary.md` — munshi's curated record of the session, rendered by `standup`.
    Summary,
}

impl Artifact {
    /// The reserved logical path this artifact is conveyed by.
    pub const fn logical_path(self) -> &'static str {
        match self {
            Self::Transcript => TRANSCRIPT_LOGICAL_PATH,
            Self::Summary => SUMMARY_LOGICAL_PATH,
        }
    }

    /// The largest size the download path will believe of a declaration about this artifact.
    const fn declared_ceiling(self) -> u64 {
        match self {
            Self::Transcript => MAX_DECLARED_TRANSCRIPT_BYTES,
            Self::Summary => MAX_DECLARED_SUMMARY_BYTES,
        }
    }

    /// This artifact within one snapshot, when the snapshot carries it.
    fn of(self, snapshot: &SnapshotDetail) -> Option<&ListedArtifact> {
        match self {
            Self::Transcript => snapshot.transcript(),
            Self::Summary => snapshot.summary(),
        }
    }

    /// Whether a snapshot is usable for this artifact *on its own* — which is precisely the
    /// question of whether it is worth looking at the snapshot's siblings.
    ///
    /// The two answers differ, and the difference is not an oversight. A transcript is only usable
    /// if this build also has an interpreter for the harness that wrote it, because a transcript
    /// is a harness-shaped file. A `summary.md` is munshi's *own* format whatever harness produced
    /// the session — the parser reads the frontmatter's `agent` key itself — so the manifest's
    /// `source_agent` decides nothing about whether it can be read, and asking would only make the
    /// standup lane blind to a session whose manifest this build does not recognize.
    fn usable(self, snapshot: &SnapshotDetail) -> bool {
        self.of(snapshot).is_some()
            && match self {
                Self::Transcript => {
                    crate::metrics::source_for_agent(&snapshot.source_agent).is_some()
                }
                Self::Summary => true,
            }
    }
}

/// One session's artifact, present in the cache and ready to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirroredSession {
    /// The archive's own id for the session this artifact was read for, off the listing row.
    ///
    /// Carried so a lane that decides *after* the mirror ran that it also wants the session's
    /// **other** artifact can resolve one without re-listing the window — which is exactly what
    /// `ask --verbatim` does: it mirrors summaries, ranks them, and only then escalates into the
    /// transcripts of the handful of sessions it is going to show ([`fetch`]). Nothing in a fold
    /// reads it.
    pub session_id: String,
    /// The snapshot the archive *projected* for this session, off the same listing row — not
    /// necessarily the snapshot the artifact was read from, when the munshi #78 sibling walk had to
    /// look past it ([`usable_snapshot`]).
    ///
    /// The projection is deliberately what is kept: an escalation to a second artifact starts where
    /// this run started and makes the same walk for itself, because the sibling carrying a
    /// `summary.md` is not necessarily the one carrying the transcript.
    pub snapshot_id: String,
    /// The artifact's content hash — cache key and cited evidence in one.
    pub source_hash: String,
    /// The harness that produced it, from the snapshot's canonical manifest.
    pub source_agent: String,
    /// The artifact contract the snapshot was captured under; decides which interpreter reads a
    /// transcript.
    pub artifact_set_version: u16,
    /// The artifact's size in bytes once decompressed — what the fold reads, and what the footer
    /// counts as "folded". Distinct from the stored size that crosses the wire.
    pub size_bytes: u64,
    /// When the archive finished the snapshot this session was listed by — **archive time, not
    /// transcript time**.
    ///
    /// This is the clock `activity_from` selects on, so it is also the clock a report partitions
    /// its window and its comparison window on: the two decisions have to be made on the same
    /// timestamp or a session could satisfy the listing and belong to neither half. `None` when
    /// the archive stated a time this build cannot parse, which places the session nowhere and is
    /// reported as a gap rather than guessed into a window.
    pub archived_at: Option<DateTime<Utc>>,
    /// The repository the archive recorded for the snapshot it listed this session by, when it
    /// recorded one. Carried from the listing rather than re-derived, on the same reasoning as
    /// [`MirroredSession::archived_at`]: it describes the row the window selected. `None` is a
    /// real state — a session captured outside a checkout — and the cost lane gives it its own
    /// row rather than merging it into a named repository's.
    pub repository: Option<String>,
    /// The host the snapshot's manifest recorded the capture running on. Unlike
    /// [`MirroredSession::repository`], this comes from the *snapshot* the artifact was read from,
    /// not the listing row — it lives only on the manifest. `None` on a capture written before the
    /// metadata existed, carried for a presentation to scope by and read by nothing here.
    pub hostname: Option<String>,
    /// The capturing host's UTC offset (e.g. `-07:00`), from the same snapshot manifest and for
    /// the same presentation-only reason as [`MirroredSession::hostname`]. `None` on older
    /// captures.
    pub utc_offset: Option<String>,
}

/// Snapshots of one session this client will look through when its `latest_snapshot` turns out
/// not to be foldable.
///
/// The failure mode this bounds is a session with hundreds of captures, none of them complete:
/// without a bound, one such session would cost hundreds of requests to conclude nothing. Eight
/// is far past the real shape of the problem (munshi #78: every affected session has exactly one
/// degenerate snapshot shadowing a complete one) and still cheap.
const MAX_FALLBACK_PROBES: usize = 8;

/// Why one session contributed nothing to the fold. Recorded rather than swallowed: a report
/// that quietly dropped a third of the window would be worse than one that says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// No snapshot of this session carries the artifact this run was mirroring, and there was no
    /// sibling to fall back to. A summary-only capture for the transcript lanes; a snapshot whose
    /// summary was never rendered for the standup lane.
    MissingArtifact(Artifact),
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
    /// Artifacts already held under their content hash: no transfer.
    pub cache_hits: u64,
    /// Artifacts fetched and verified this run.
    pub cache_misses: u64,
    /// Bytes that actually crossed the wire on the misses: the *stored* (compressed) size the
    /// archive serves, not the larger transcript it decompresses into. The two differ by a lot
    /// for zstd artifacts, and a footer that conflated them would misreport what a re-sync costs
    /// the network.
    pub bytes_transferred: u64,
    /// Sessions resolved from the snapshot index alone: no request of any kind.
    pub snapshots_indexed: u64,
    /// Snapshot documents actually requested from the archive this run — one per session the
    /// index could not settle, plus one per sibling probed by a fallback. This is the number
    /// qanungo #1 exists to drive to zero on a warm run.
    pub snapshots_fetched: u64,
    /// Sessions whose **projected** snapshot did not carry this run's artifact in a form this
    /// build can read — the munshi #78 class, counted rather than merely survived.
    ///
    /// Exactly the sessions [`usable_snapshot`] had to walk siblings for, whatever that walk then
    /// concluded. It is a *health* number and not a failure count: the mirror recovers most of
    /// them, and the field below says how many.
    pub projection_unusable: u64,
    /// How many of [`SyncStats::projection_unusable`] were resolved from a sibling snapshot.
    ///
    /// The difference between the two is the sessions this run could read nothing for — the ones
    /// the skip list already names a sentence each, here as one number a health panel can state
    /// without parsing sentences.
    pub recovered_from_sibling: u64,
    pub elapsed: Duration,
}

/// Per-run request counters shared by the workers.
#[derive(Default)]
struct Requests {
    snapshots_indexed: AtomicU64,
    snapshots_fetched: AtomicU64,
    /// Sessions whose projected snapshot sent [`usable_snapshot`] looking at siblings, and how
    /// many of those a sibling answered. Counted here rather than derived from the outcome
    /// afterwards, because a *recovery* leaves no skip behind at all and is therefore invisible to
    /// anything reading the skip list.
    projection_unusable: AtomicU64,
    recovered_from_sibling: AtomicU64,
}

/// The mirror's result: what can be read, and what could not be.
#[derive(Debug, Clone, Default)]
pub struct Mirror {
    pub sessions: Vec<MirroredSession>,
    pub skipped: Vec<Skip>,
    pub stats: SyncStats,
}

/// Lists the window and ensures every listed session's `artifact` is in the cache.
///
/// Sessions come back in the archive's own newest-first listing order, so the report is stable
/// across runs even though the workers finish out of order.
///
/// `concurrency` is clamped to `1..=`[`MAX_CONCURRENCY`] here as well as at the command line, so
/// no caller of this function can crowd the archive by passing a large number.
///
/// # Errors
///
/// Returns an error only when the window itself cannot be listed. Per-session failures become
/// [`Skip`]s.
pub fn sync(
    client: &ReadClient,
    cache: &BlobCache,
    artifact: Artifact,
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
    let requests = Requests::default();
    std::thread::scope(|scope| {
        for _ in 0..concurrency.clamp(1, MAX_CONCURRENCY) {
            scope.spawn(|| {
                loop {
                    let Some((index, session)) = lock(&queue).next() else {
                        break;
                    };
                    let outcome = mirror_session(client, cache, artifact, &session, &requests);
                    lock(&outcomes).push((index, outcome));
                }
            });
        }
    });
    mirror.stats.snapshots_indexed = requests.snapshots_indexed.load(Ordering::Relaxed);
    mirror.stats.snapshots_fetched = requests.snapshots_fetched.load(Ordering::Relaxed);
    mirror.stats.projection_unusable = requests.projection_unusable.load(Ordering::Relaxed);
    mirror.stats.recovered_from_sibling = requests.recovered_from_sibling.load(Ordering::Relaxed);

    let mut outcomes = outcomes
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    outcomes.sort_by_key(|(index, _)| *index);
    for (_, outcome) in outcomes {
        match outcome {
            Outcome::Mirrored {
                session,
                lookup,
                transferred_bytes,
            } => {
                match lookup {
                    crate::cache::Lookup::Hit => mirror.stats.cache_hits += 1,
                    crate::cache::Lookup::Miss => mirror.stats.cache_misses += 1,
                }
                mirror.stats.bytes_transferred += transferred_bytes;
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
        /// Stored bytes actually pulled over the wire: zero on a cache hit.
        transferred_bytes: u64,
    },
    Skipped(Skip),
}

/// Resolves one session's `artifact` and makes sure the cache holds it.
fn mirror_session(
    client: &ReadClient,
    cache: &BlobCache,
    artifact: Artifact,
    session: &ListedSession,
    requests: &Requests,
) -> Outcome {
    match resolve(
        client,
        cache,
        artifact,
        &session.session_id,
        &session.snapshot_id,
        &session.source_agent,
        requests,
    ) {
        Ok(resolved) => Outcome::Mirrored {
            session: mirrored(session, &resolved.snapshot, &resolved.wanted),
            lookup: resolved.lookup,
            transferred_bytes: resolved.transferred_bytes,
        },
        Err(skip) => Outcome::Skipped(skip),
    }
}

/// One session's artifact, resolved from the archive and present in the cache: which snapshot
/// carries it and which artifact that is, together with what getting there cost.
struct Resolved {
    snapshot: SnapshotDetail,
    wanted: ListedArtifact,
    lookup: crate::cache::Lookup,
    /// Stored bytes actually pulled over the wire: zero on a cache hit.
    transferred_bytes: u64,
}

/// The archive-side half of mirroring: resolve `artifact` for one session and make sure the cache
/// holds its bytes.
///
/// Keyed on the two ids and a fallback label rather than on a listing row, because the escalation
/// path ([`fetch`]) holds a [`MirroredSession`] rather than a [`ListedSession`] and has to make
/// *exactly* this walk — the same index confirmation, the same sibling fallback, the same
/// verified-then-committed download. Two resolutions that could disagree about which snapshot
/// carries a session's artifact would be two clients wearing one name.
///
/// `listed_agent` names the harness only for the failure before any snapshot has been read; every
/// later skip names the agent whichever snapshot was actually examined stated.
fn resolve(
    client: &ReadClient,
    cache: &BlobCache,
    artifact: Artifact,
    session_id: &str,
    snapshot_id: &str,
    listed_agent: &str,
    requests: &Requests,
) -> Result<Resolved, Skip> {
    let skip = |source_agent: &str, reason: SkipReason| Skip {
        source_agent: source_agent.to_owned(),
        reason,
    };
    if let Some((snapshot, wanted)) = held_via_index(cache, artifact, snapshot_id) {
        requests.snapshots_indexed.fetch_add(1, Ordering::Relaxed);
        return Ok(Resolved {
            snapshot,
            wanted,
            lookup: crate::cache::Lookup::Hit,
            transferred_bytes: 0,
        });
    }
    let snapshot = fetch_snapshot(client, cache, snapshot_id, requests)
        .map_err(|error| skip(listed_agent, SkipReason::Unreadable(error.to_string())))?;
    let source_agent = snapshot.source_agent.clone();
    let snapshot = match usable_snapshot(
        client,
        cache,
        artifact,
        session_id,
        snapshot_id,
        snapshot,
        requests,
    ) {
        Ok(Resolution::Usable(snapshot)) => snapshot,
        Ok(Resolution::Unusable(reason)) => return Err(skip(&source_agent, reason)),
        // The listing itself failed, so what this session holds is unknown. Saying "no such
        // artifact" here would be reporting a fact this run never learned.
        Err(error) => {
            return Err(skip(
                &source_agent,
                SkipReason::Unreadable(error.to_string()),
            ));
        }
    };
    let Some(wanted) = artifact.of(&snapshot).cloned() else {
        return Err(skip(&source_agent, SkipReason::MissingArtifact(artifact)));
    };

    if cache.contains(&wanted.original_sha256) {
        return Ok(Resolved {
            snapshot,
            wanted,
            lookup: crate::cache::Lookup::Hit,
            transferred_bytes: 0,
        });
    }
    // The artifact is streamed straight into a staged cache write: it is never held in memory,
    // and it becomes a blob only once the download has verified every digest and size the archive
    // declared. Dropping the staged write on any failure below unlinks the partial file, so an
    // aborted transfer leaves the cache exactly as it found it. A summary is kilobytes and could
    // be held in a `String` instead — but then the two lanes would cache by two different routes,
    // and only one of them would be the one that refuses unverified bytes.
    let mut staged = cache.stage(&wanted.original_sha256).map_err(|error| {
        skip(
            &snapshot.source_agent,
            SkipReason::Unreadable(format!("cache write failed: {error}")),
        )
    })?;
    let receipt = client
        .download_artifact(&wanted, artifact.declared_ceiling(), &mut staged)
        .map_err(|error| {
            skip(
                &snapshot.source_agent,
                SkipReason::Unreadable(error.to_string()),
            )
        })?;
    staged.commit().map_err(|error| {
        skip(
            &snapshot.source_agent,
            SkipReason::Unreadable(format!("cache write failed: {error}")),
        )
    })?;
    Ok(Resolved {
        snapshot,
        wanted,
        lookup: crate::cache::Lookup::Miss,
        // What the receipt counted crossing the wire, not what the listing promised would: the
        // two agree by the time a download succeeds, and counting the measurement keeps the
        // footer a record of the transfer rather than a restatement of the plan.
        transferred_bytes: receipt.stored_bytes,
    })
}

/// One artifact fetched for a *single* already-mirrored session, outside the window mirror.
///
/// What an escalation gets back: enough to open the blob and interpret it, plus what asking for it
/// cost, so a lane that fetches a few transcripts can instrument the escalation the way [`sync`]
/// instruments a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The artifact's content hash — the cache key its bytes are under.
    pub source_hash: String,
    /// The harness stated by the snapshot this artifact was actually read from, which is not
    /// necessarily the one that carried the session's other artifact.
    pub source_agent: String,
    pub artifact_set_version: u16,
    /// Decompressed size, as the archive declared it and the download verified it.
    pub size_bytes: u64,
    pub lookup: crate::cache::Lookup,
    /// Stored bytes that crossed the wire: zero on a cache hit.
    pub transferred_bytes: u64,
    /// Whether the snapshot index settled this session outright — no request of any kind.
    pub snapshot_indexed: bool,
    /// Snapshot documents requested from the archive for this one session.
    pub snapshots_fetched: u64,
}

/// Fetches one *other* artifact of a session the mirror already listed (qanungo #10's
/// `--verbatim`).
///
/// The window mirror moves one artifact for every session it listed, which is the right shape for
/// a lane that folds all of them and the wrong shape for one that reads a second artifact of a
/// handful. This is that second shape: one session, one artifact, the same resolution walk, and no
/// listing at all. Archive traffic is therefore whatever the caller's own bound is — for the ask
/// lane, the hits it is going to show.
///
/// # Errors
///
/// Returns the [`Skip`] this session would have been recorded as by a mirror run for `artifact`:
/// no snapshot carries it, no interpreter reads it, or the archive could not be read. A caller
/// states that rather than dropping the session, exactly as the mirror's own skip list is stated.
pub fn fetch(
    client: &ReadClient,
    cache: &BlobCache,
    artifact: Artifact,
    session: &MirroredSession,
) -> Result<Fetched, Skip> {
    let requests = Requests::default();
    let resolved = resolve(
        client,
        cache,
        artifact,
        &session.session_id,
        &session.snapshot_id,
        &session.source_agent,
        &requests,
    )?;
    Ok(Fetched {
        source_hash: resolved.wanted.original_sha256,
        source_agent: resolved.snapshot.source_agent,
        artifact_set_version: resolved.snapshot.artifact_set_version,
        size_bytes: resolved.wanted.original_size_bytes,
        lookup: resolved.lookup,
        transferred_bytes: resolved.transferred_bytes,
        snapshot_indexed: requests.snapshots_indexed.load(Ordering::Relaxed) > 0,
        snapshots_fetched: requests.snapshots_fetched.load(Ordering::Relaxed),
    })
}

/// The mirror's record of a session read from `snapshot`, whichever snapshot that is.
fn mirrored(
    session: &ListedSession,
    snapshot: &SnapshotDetail,
    wanted: &ListedArtifact,
) -> MirroredSession {
    MirroredSession {
        // Both ids come off the *listing row*: the session the window selected and the snapshot it
        // projected. An escalation to this session's other artifact re-walks from the projection,
        // so it must be the projection that is remembered rather than whichever sibling this run
        // ended up reading.
        session_id: session.session_id.clone(),
        snapshot_id: session.snapshot_id.clone(),
        source_hash: wanted.original_sha256.clone(),
        // Both taken from whichever snapshot actually carries the artifact: a fallback sibling
        // states its own provenance, and a degenerate snapshot's is not evidence about it.
        source_agent: snapshot.source_agent.clone(),
        artifact_set_version: snapshot.artifact_set_version,
        size_bytes: wanted.original_size_bytes,
        // Taken from the *listing*, which is the row `activity_from` filtered, so the window a
        // session is placed in is decided by the same timestamp that put it in the listing at all.
        // A fallback sibling's own completion time would be a different clock reading.
        archived_at: parse_archive_time(&session.completed_at),
        repository: session.repository.clone(),
        // These two, unlike `repository`, come from the snapshot actually read rather than the
        // listing row: they live only on the manifest, so a fallback sibling states its own.
        hostname: snapshot.hostname.clone(),
        utc_offset: snapshot.utc_offset.clone(),
    }
}

/// The session as the cache alone can state it: its projected snapshot is indexed, usable for
/// `artifact` on its own, and the artifact's blob is held. Anything short of that — no entry, an
/// entry this build cannot read, a projection that needs the sibling walk, a blob that is not
/// there — is `None`, and the session is resolved from the archive exactly as if there were no
/// index. That is the whole safety argument: the index can only ever *confirm* a hit, never
/// choose a download.
fn held_via_index(
    cache: &BlobCache,
    artifact: Artifact,
    snapshot_id: &str,
) -> Option<(SnapshotDetail, ListedArtifact)> {
    let document = cache.snapshot_document(snapshot_id)?;
    let document: Value = serde_json::from_slice(&document).ok()?;
    let snapshot = SnapshotDetail::from_document(&document).ok()?;
    if !artifact.usable(&snapshot) {
        return None;
    }
    let wanted = artifact.of(&snapshot)?.clone();
    cache
        .contains(&wanted.original_sha256)
        .then_some((snapshot, wanted))
}

/// Fetches a snapshot's document from the archive, indexes it, and parses it.
///
/// Indexing is best-effort and precedes parsing: a document is the archive's immutable statement
/// whether or not this build can read it, and a cache that cannot be written is still a cache.
fn fetch_snapshot(
    client: &ReadClient,
    cache: &BlobCache,
    snapshot_id: &str,
    requests: &Requests,
) -> Result<SnapshotDetail, PatwariError> {
    requests.snapshots_fetched.fetch_add(1, Ordering::Relaxed);
    let document = client.snapshot_document(snapshot_id)?;
    if let Ok(bytes) = serde_json::to_vec(&document) {
        let _ = cache.index_snapshot(snapshot_id, &bytes);
    }
    SnapshotDetail::from_document(&document)
}

/// What looking through a session's snapshots concluded.
enum Resolution {
    /// A snapshot carrying the wanted artifact in a form this build can read.
    Usable(SnapshotDetail),
    /// None does, and this is why — stated from what was actually seen.
    Unusable(SkipReason),
}

/// Picks the snapshot of this session to read: the archive's projected one when it is usable,
/// otherwise the newest sibling that is.
///
/// # Why this exists (munshi #78)
///
/// `latest_snapshot` is strictly newest-by-completion, and a *degenerate* capture can be the
/// newest one: 56 sessions in the real archive carry a summary-only snapshot, all written by a
/// single 2026-07-28 backfill run, each shadowing a complete sibling whose transcript was already
/// archived. Keying on the projection alone made ~10% of the archive invisible to every metric
/// here while the bytes sat in it the whole time.
///
/// The fix belongs upstream — tombstoning those snapshots re-elects the complete ones — and this
/// is defense in depth, not a substitute for it: a read-side client that can see an artifact
/// should read it.
///
/// The standup lane inherits the discipline unchanged rather than assuming its own artifact is
/// immune. The 2026-07-28 shape happens to favour it — a summary-only snapshot is exactly what
/// `standup` wants — but "the newest snapshot is the one worth reading" was never a safe belief
/// about *either* artifact, and a capture whose summary was never rendered is the mirror image of
/// the one that broke the transcript lanes.
///
/// # Why an unusable snapshot is not a short-circuit
///
/// It is tempting to skip a snapshot whose `source_agent` has no interpreter before spending the
/// listing request, and an earlier draft of this did. That re-creates the very blindness the
/// function exists to remove: the *degenerate* snapshot is the one carrying the odd manifest, and
/// believing its provenance over a complete sibling's is exactly the mistake of trusting a
/// projection. A snapshot is therefore looked past whenever [`Artifact::usable`] says so, and the
/// cost is bounded the same way in every case — one listing plus at most [`MAX_FALLBACK_PROBES`]
/// snapshot reads, and nothing at all for a session whose projection is already usable.
///
/// The reason returned when nothing is usable is taken from what was seen rather than from which
/// check ran last: a transcript that exists but cannot be interpreted is
/// [`SkipReason::UnknownAgent`], and only a session where no snapshot carries the artifact at all
/// is [`SkipReason::MissingArtifact`]. For [`Artifact::Summary`] the first of those cannot arise,
/// because a summary needs no interpreter to be read.
fn usable_snapshot(
    client: &ReadClient,
    cache: &BlobCache,
    artifact: Artifact,
    session_id: &str,
    snapshot_id: &str,
    projected: SnapshotDetail,
    requests: &Requests,
) -> Result<Resolution, PatwariError> {
    if artifact.usable(&projected) {
        return Ok(Resolution::Usable(projected));
    }
    // Past this line is the munshi #78 class: this session's newest snapshot cannot answer for the
    // artifact. Counted here — the one place that knows — because a walk that *succeeds* leaves
    // nothing behind for a later reader to count, and "how often is the projection wrong" is a
    // different question from "what did this run fail to read".
    requests.projection_unusable.fetch_add(1, Ordering::Relaxed);
    // Remembered across the probes so the eventual gap describes the whole session rather than
    // whichever snapshot happened to be examined last.
    let mut uninterpretable = artifact
        .of(&projected)
        .is_some()
        .then(|| projected.source_agent.clone());

    let siblings = client.session_snapshots(session_id)?;
    for sibling in siblings
        .iter()
        // The projected one is where we started; re-reading it would cost a request to learn
        // what this run already knows.
        .filter(|sibling| sibling.snapshot_id != snapshot_id)
        .take(MAX_FALLBACK_PROBES)
    {
        let detail = fetch_snapshot(client, cache, &sibling.snapshot_id, requests)?;
        if artifact.usable(&detail) {
            requests
                .recovered_from_sibling
                .fetch_add(1, Ordering::Relaxed);
            return Ok(Resolution::Usable(detail));
        }
        if artifact.of(&detail).is_some() {
            uninterpretable.get_or_insert(detail.source_agent.clone());
        }
    }
    Ok(Resolution::Unusable(match uninterpretable {
        Some(agent) => SkipReason::UnknownAgent(agent),
        None => SkipReason::MissingArtifact(artifact),
    }))
}

/// Parses an archive completion time, or reports that it could not be parsed.
///
/// Patwari states these as RFC 3339 in UTC. A value that is not one is not repaired, defaulted, or
/// assumed to be "now": the session simply cannot be placed in a window, and the report says so.
fn parse_archive_time(completed_at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(completed_at)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// A poisoned worker mutex means another worker panicked; the remaining work is still valid, so
/// the lock is taken anyway rather than cascading the panic across the pool.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use crate::patwari::ListedArtifact;

    use super::*;

    fn artifact(logical_path: &str) -> ListedArtifact {
        ListedArtifact {
            artifact_id: "a".to_owned(),
            logical_path: logical_path.to_owned(),
            original_sha256: "0".repeat(64),
            original_size_bytes: 1,
            stored_size_bytes: 1,
            content_url: "/x".to_owned(),
        }
    }

    fn snapshot(source_agent: &str, logical_paths: &[&str]) -> SnapshotDetail {
        SnapshotDetail {
            source_agent: source_agent.to_owned(),
            artifact_set_version: 2,
            artifacts: logical_paths.iter().copied().map(artifact).collect(),
            hostname: None,
            utc_offset: None,
        }
    }

    /// The one place the two lanes deliberately disagree: a transcript is a harness-shaped file
    /// and needs an interpreter, a `summary.md` is munshi's own format and does not. A harness
    /// this build has never heard of therefore hides a transcript and not a summary.
    #[test]
    fn a_summary_needs_no_interpreter_and_a_transcript_does() {
        let known = snapshot(
            "claude-code",
            &[TRANSCRIPT_LOGICAL_PATH, SUMMARY_LOGICAL_PATH],
        );
        assert!(Artifact::Transcript.usable(&known));
        assert!(Artifact::Summary.usable(&known));

        let unknown = snapshot(
            "future-harness",
            &[TRANSCRIPT_LOGICAL_PATH, SUMMARY_LOGICAL_PATH],
        );
        assert!(!Artifact::Transcript.usable(&unknown));
        assert!(
            Artifact::Summary.usable(&unknown),
            "the standup lane must still see a session whose manifest this build cannot place",
        );
    }

    /// The manifest's capture metadata rides onto the mirror record from the snapshot actually
    /// read — not the listing row, which never carries it — so a presentation downstream can scope
    /// by host or place on a clock without a second fold.
    #[test]
    fn mirrored_carries_the_snapshots_capture_metadata() {
        let session = ListedSession {
            session_id: "1".repeat(32),
            source_agent: "claude-code".to_owned(),
            snapshot_id: "2".repeat(32),
            completed_at: "2026-08-16T10:00:00.000Z".to_owned(),
            repository: Some("surdy/qanungo".to_owned()),
        };
        let mut snap = snapshot("claude-code", &[TRANSCRIPT_LOGICAL_PATH]);
        snap.hostname = Some("macbookpro".to_owned());
        snap.utc_offset = Some("-07:00".to_owned());
        let wanted = artifact(TRANSCRIPT_LOGICAL_PATH);

        let record = mirrored(&session, &snap, &wanted);
        assert_eq!(record.hostname.as_deref(), Some("macbookpro"));
        assert_eq!(record.utc_offset.as_deref(), Some("-07:00"));

        // A snapshot whose manifest predates the metadata leaves both `None` on the record.
        let older = snapshot("claude-code", &[TRANSCRIPT_LOGICAL_PATH]);
        let record = mirrored(&session, &older, &wanted);
        assert_eq!(record.hostname, None);
        assert_eq!(record.utc_offset, None);
    }

    /// Each lane is blind to exactly the snapshot that lacks *its* artifact, which is what makes
    /// the sibling fallback worth spending a listing request on in both directions.
    #[test]
    fn each_lane_looks_past_the_snapshot_missing_its_own_artifact() {
        let summary_only = snapshot("claude-code", &[SUMMARY_LOGICAL_PATH]);
        assert!(!Artifact::Transcript.usable(&summary_only));
        assert!(Artifact::Summary.usable(&summary_only));

        let transcript_only = snapshot("claude-code", &[TRANSCRIPT_LOGICAL_PATH]);
        assert!(Artifact::Transcript.usable(&transcript_only));
        assert!(!Artifact::Summary.usable(&transcript_only));
    }

    /// A summary is kilobytes and a transcript is megabytes, so one ceiling cannot bound both:
    /// the transcript's is no bound at all on a summary.
    #[test]
    fn the_declared_ceiling_is_a_property_of_the_artifact() {
        assert_eq!(
            Artifact::Transcript.declared_ceiling(),
            MAX_DECLARED_TRANSCRIPT_BYTES
        );
        assert_eq!(
            Artifact::Summary.declared_ceiling(),
            MAX_DECLARED_SUMMARY_BYTES
        );
        assert!(Artifact::Summary.declared_ceiling() < Artifact::Transcript.declared_ceiling());
        assert_eq!(Artifact::Summary.logical_path(), SUMMARY_LOGICAL_PATH);
        assert_eq!(Artifact::Transcript.logical_path(), TRANSCRIPT_LOGICAL_PATH);
    }

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
        assert_eq!(stats.bytes_transferred, 0);
    }

    /// An archive time this build cannot read places the session nowhere rather than somewhere
    /// convenient — a trend arrow computed over a guessed window would be a lie about behaviour.
    #[test]
    fn an_unparseable_archive_time_places_a_session_nowhere() {
        assert_eq!(
            parse_archive_time("2026-08-10T10:00:00.000Z"),
            Some(
                DateTime::parse_from_rfc3339("2026-08-10T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            ),
        );
        for unreadable in ["", "yesterday", "2026-08-10", "1786000000"] {
            assert_eq!(parse_archive_time(unreadable), None, "`{unreadable}`");
        }
    }

    #[test]
    fn concurrency_is_clamped_to_the_archives_capacity() {
        assert_eq!(0_usize.clamp(1, MAX_CONCURRENCY), 1);
        assert_eq!(64_usize.clamp(1, MAX_CONCURRENCY), MAX_CONCURRENCY);
        assert_eq!(
            DEFAULT_CONCURRENCY.clamp(1, MAX_CONCURRENCY),
            DEFAULT_CONCURRENCY
        );
    }
}
