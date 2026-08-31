//! The dashboard's HTTP surface: a blocking HTTP/1.1 server over `std::net::TcpListener`, one
//! thread per connection, no async runtime and no framework.
//!
//! It is the server counterpart of this crate's minimal HTTP client (munshi ADR 0006) and mirrors
//! munshi-dashboard's shape deliberately: one request per connection, `Connection: close`, an
//! explicit `Content-Length`, no keep-alive bookkeeping, an embedded single-file page, and no state
//! of its own on disk. Five routes exist — the page, the JSON snapshot, the event stream, one
//! evidence excerpt, and one ranked search — and everything else is 404.
//!
//! # The excerpt route, and the three things that bound it
//!
//! `GET /api/evidence/<source_hash>/<locator>` answers with the single event a rule counted,
//! scrubbed. Three rules hold it in place, and each one is a refusal rather than a check:
//!
//! 1. **Only what the payload named.** The current payload's [`EvidenceIndex`] is the entire
//!    servable set. A perfectly well-formed locator against a perfectly well-cached transcript is a
//!    404 unless a finding on the page offered exactly that anchor. Without this the route is a
//!    transcript-browsing API — walk the locators, read the session — which is the disclosure the
//!    2026-08-24 grilling refused when it took the Patwari deep-links off this page.
//! 2. **Never a fetch.** The blob must already be in the local cache, which the fold put there. A
//!    cache miss is a 404 saying so. A browser — any peer on the tailnet — must not be able to make
//!    this process talk to the archive; if it could, an unauthenticated surface would be a remote
//!    control for somebody else's bandwidth and for what lands on this disk.
//! 3. **The scrub is the process's, not the request's.** The redactor is built once from the
//!    command line and every reader gets it. There is no query parameter, and the router discards
//!    query strings before it decides anything.
//!
//! # The ask route, and the one rule it does not bend
//!
//! `GET /api/ask?q=<terms>&limit=<n>` ranks the refresh's own corpus of `summary.md` records and
//! answers with the hits (qanungo #10's dashboard ask-box). It is the **first route on this surface
//! whose query string is its argument**, which is worth stating plainly against the sentence beside
//! it in [`route`]: a query string still decides nothing about *what this process will say about
//! itself*. It cannot select a redaction posture, a window, a scope, or a session — the only two
//! parameters are words to rank by and how many rows to show, and every other parameter on this
//! target is ignored rather than interpreted.
//!
//! The excerpt route's three refusals hold here unchanged, and the first two are the load-bearing
//! ones:
//!
//! 1. **Never a fetch.** The corpus is [`command::AskCorpus`], mirrored and parsed on the refresh
//!    timer like the three document lanes. A request scores an in-memory `Vec`. There is no path
//!    from this route to Patwari — not a guarded one, none — so no browser on the tailnet can spend
//!    somebody else's bandwidth or decide what lands on this disk.
//! 2. **The scrub is the process's, not the request's.** Every string in an answer was scrubbed by
//!    the launch-time redactor on the way into the ranking ([`crate::ask::Ask::fold`]), and the
//!    answer states which posture stands behind it.
//! 3. **Bounded before it is work.** The raw `q` is capped at [`MAX_QUERY_BYTES`] and refused with a
//!    `400` *before* it is decoded or parsed, and `limit` is clamped into
//!    `1..=`[`MAX_ASK_LIMIT`] rather than trusted. A query of nothing but stop words is answered
//!    with the "no searchable terms" shape and no ranking at all.
//!
//! **No verbatim here.** `qanungo ask --verbatim` escalates into the shown hits' transcripts; that
//! is a CLI affordance and stays one (decision 11). This route cannot be made to serve a transcript
//! line, because the corpus it reads holds no transcript.
//!
//! # Where it differs from munshi-dashboard, and why
//!
//! - **It folds its own numbers instead of shelling out.** munshi-dashboard invokes `munshi
//!   ... --json` per panel. There is no `qanungo report --json`, and inventing one so a dashboard
//!   could parse it back would be two serializations and a subprocess where a function call does.
//!   It calls [`command::fold_coaching`], [`command::fold_cost`], and [`command::fold_standup`]
//!   directly — the same three calls `qanungo report`, `qanungo cost`, and `qanungo standup` make.
//! - **It refreshes in the background.** The three lanes together are **45 s** against the
//!   production archive, warm (measured 2026-08-25), and very nearly the sum of the three CLI runs:
//!   the shared blob cache spares the bytes and not the requests, because [`crate::sync`] asks the
//!   archive for one snapshot document per listed session before it consults the cache. On the
//!   request path that is not a dashboard, it is a wait — so the folds happen on a timer, the
//!   payload is serialized once per refresh, and a request is a memcpy. This is the "in-memory
//!   service" half of the 2026-08-24 grilling: process memory is the disposable materialization,
//!   and the persistent event store stays deferred.
//! - **It pushes.** `/api/events` is Server-Sent Events, so an open page learns about a refresh
//!   instead of polling for one. A poll would either be slower than the refresh interval or busier
//!   than it, and neither is what a page that changes every five minutes wants.
//! - **It binds beyond loopback on request.** munshi-dashboard refuses a routable address outright.
//!   This one is *for* the tailnet (qanungo #5: laptop, phone, TV), so it allows it and says out
//!   loud what that means — see [`posture_line`].
//!
//! # The security model, stated plainly
//!
//! **Nothing here authenticates a caller.** On loopback that is the machine's own boundary; on a
//! tailnet address the tailnet is the boundary and there is no second one. What limits the blast
//! radius is not access control but *what this process will say*: lane scores, rule ids, counts,
//! content hashes, and — only for the anchors a finding on the page named — one scrubbed event
//! apiece, with no link into Patwari, which serves unredacted blobs. See [`crate::dashboard`] and
//! [`crate::evidence`] for how each half of that line is held.
//!
//! # Blocking until the first fold
//!
//! The listener is bound **before** the first fold and accepted on **after** it. Binding first
//! means an address that is already in use fails in milliseconds rather than after a minute of
//! folding; accepting after means the port answering is a promise that the numbers behind it are
//! real. A "first refresh in progress" state would be a second payload shape, a second empty state
//! in the page, and a second set of tests, all for a window that happens once per process and that
//! the operator is watching the stderr of anyway.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::json;
use thiserror::Error;

use crate::ask::Query;
use crate::cache::{self, BlobCache};
use crate::cli::{ArchiveArgs, DashboardArgs, Refresh};
use crate::command::{self, CommandError, Folded};
use crate::dashboard::{self, Payload, Refreshed, Windows};
use crate::evidence::{self, EvidenceIndex};
use crate::format;
use crate::metrics;
use crate::redaction::Redactor;
use crate::report::stamp;

/// The single-file page, embedded so the binary is the whole deployment. No asset route exists,
/// and none can be added without also inventing a filesystem the server reads from.
const PAGE: &str = include_str!("../assets/dashboard.html");

/// Ceiling on a request head. The server reads a request line and discards the headers, so a peer
/// that never sends the terminator cannot grow this buffer beyond one small allocation.
const MAX_REQUEST_BYTES: usize = 8192;

/// How long a connection may take to send its head. Generous for a browser on a phone waking up on
/// the tailnet, short enough that a stalled peer frees its thread rather than holding it.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// How long one write may block. This is what eventually reclaims an event-stream thread whose
/// reader closed the laptop lid rather than the tab.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often an idle event stream writes a comment. Its job is to notice a dead peer — a write to
/// a closed socket fails and the thread exits — and to keep any intermediary from timing out a
/// connection that is legitimately silent for a whole refresh interval.
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);

/// What a disconnected page waits before reconnecting, in milliseconds. Under the heartbeat, so a
/// restarted server is picked up on the next beat rather than on the next refresh.
const SSE_RETRY_MILLIS: u64 = 3_000;

/// Ceiling on the raw `q` of an ask request, in bytes of the target as it arrived.
///
/// A tunable, and a refusal rather than a truncation: a query this long is not a search anybody
/// typed, and answering a clipped version of it would be answering a question nobody asked. It is
/// measured on the **undecoded** value, before any percent-escape is expanded, because decoding can
/// only shrink a value — so capping the raw bounds both, and does it before this process has spent
/// anything on the request. Well under [`MAX_REQUEST_BYTES`], which bounds the whole head anyway.
const MAX_QUERY_BYTES: usize = 1024;

/// How many ranked hits an ask request may ask for, and how many it gets when it asks for nothing.
///
/// Clamped rather than refused, unlike the CLI's `--limit` ([`crate::cli::parse_limit`]): a mistyped
/// flag is a person who wants to know, and a mistyped query string is a browser that should still
/// get an answer. The effective limit is echoed in the answer, so a truncated ranking says what
/// truncated it.
const DEFAULT_ASK_LIMIT: usize = 10;
const MAX_ASK_LIMIT: usize = 50;

/// Concurrent event streams the server will hold open.
///
/// Each one is a parked thread. A personal dashboard on a tailnet is a handful of tabs, and this is
/// far past that — but it is a bound rather than none, so a peer that opens streams in a loop is
/// refused with a status instead of being allowed to spawn threads until the process dies.
const MAX_EVENT_STREAMS: usize = 64;

#[derive(Debug, Error)]
pub enum DashboardError {
    #[error("could not bind {address}: {source}")]
    Bind {
        address: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error("could not open the transcript cache: {0}")]
    Cache(#[source] io::Error),
    #[error(transparent)]
    Fold(#[from] CommandError),
}

/// Runs `qanungo dashboard` in the foreground until interrupted.
///
/// Binds, states its posture, folds the window pair once, then serves that payload and refreshes it
/// on a timer. There is no daemon, no pidfile, and nothing to clean up: the process holds the whole
/// of the dashboard's state, and killing it is the shutdown procedure.
///
/// # Errors
///
/// Returns an error when the address cannot be bound or when the *first* fold fails — a dashboard
/// that came up with no numbers would be a page that lies about an archive it never reached. Every
/// later fold failure is a stale payload and a stderr line, never an exit.
pub fn run(args: &DashboardArgs) -> Result<(), DashboardError> {
    Dashboard::start(args)?.run();
    Ok(())
}

/// A bound listener with its first fold already taken, before anything has been accepted on it.
///
/// The two halves are separable so a test can drive the *real* routes over a real socket without
/// also starting a timer that would re-sync an archive mid-assertion: [`Dashboard::start`] does
/// everything that can fail, [`Dashboard::serve`] answers requests, and [`Dashboard::run`] is the
/// two of them with the refresh loop in between.
pub struct Dashboard {
    listener: TcpListener,
    service: Arc<Service>,
    archive: ArchiveArgs,
    windows: Windows,
    refresh: Refresh,
}

impl Dashboard {
    /// Binds the address and takes the first fold. Nothing is accepted yet.
    ///
    /// Bind first, fold second: an address already in use fails in milliseconds rather than after a
    /// minute of folding, and the port is reserved for the whole of the startup so a connection
    /// that arrives during it queues in the backlog instead of being refused.
    ///
    /// # Errors
    ///
    /// The address cannot be bound, or the first fold failed.
    pub fn start(args: &DashboardArgs) -> Result<Self, DashboardError> {
        let listener = TcpListener::bind(args.bind).map_err(|source| DashboardError::Bind {
            address: args.bind,
            source,
        })?;
        let address = listener.local_addr().unwrap_or(args.bind);
        let redactor = args.redaction.redactor();
        eprintln!("qanungo dashboard: listening on http://{address}");
        eprintln!("qanungo dashboard: {}", posture_line(address, &redactor));
        if let Some(line) = redaction_posture_line(address, &redactor) {
            eprintln!("qanungo dashboard: {line}");
        }

        // Opened once, at launch, and held for the life of the process: the excerpt route reads
        // the same blobs the fold already mirrored, and opening a cache per request would be a
        // directory creation on a path a peer's request drove.
        let cache = match &args.archive.cache_dir {
            Some(dir) => BlobCache::open(dir),
            None => BlobCache::open_default(),
        }
        .map_err(DashboardError::Cache)?;

        let windows = Windows {
            coaching: args.last.clone(),
            cost: args.cost_last.clone(),
            standup: args.standup_last.clone(),
        };
        let service = Arc::new(Service::new(
            fold_and_publish(
                &args.archive,
                &windows,
                &args.refresh,
                Refreshed {
                    generation: 1,
                    at: Utc::now(),
                    stale_since: None,
                },
                &redactor,
            )?,
            cache,
            redactor,
        ));
        Ok(Self {
            listener,
            service,
            archive: args.archive.clone(),
            windows,
            refresh: args.refresh.clone(),
        })
    }

    /// The address actually bound — the port the operating system chose, when the caller asked for
    /// zero.
    pub fn address(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)))
    }

    /// Answers requests until the listener fails, with no refresh loop behind it.
    pub fn serve(&self) {
        serve(&self.listener, &self.service);
    }

    /// Starts the background refresh loop, then serves until the process is interrupted.
    pub fn run(self) {
        let refreshing = Arc::clone(&self.service);
        let archive = self.archive.clone();
        let windows = self.windows.clone();
        let refresh = self.refresh.clone();
        let redactor = self.service.redactor;
        if let Err(error) = thread::Builder::new()
            .name("dashboard-refresh".to_owned())
            .spawn(move || refresh_loop(&refreshing, &archive, &windows, &refresh, &redactor))
        {
            // Serving the one payload already folded is strictly better than refusing to serve at
            // all, so this is reported and survived rather than propagated.
            eprintln!(
                "qanungo dashboard: could not start the refresh loop, serving one fold: {error}"
            );
        }
        self.serve();
    }
}

/// The one-line statement of what binding here means, printed at startup whichever answer it is.
///
/// A routable bind is allowed — reading the dashboard from a phone is the point of qanungo #5 — so
/// the honesty has to come from saying what it costs rather than from refusing it. The line names
/// the two facts a reader needs: that nothing authenticates, and that the tailnet is therefore the
/// entire boundary.
pub fn posture_line(address: SocketAddr, redactor: &Redactor) -> String {
    let excerpts = if redactor.redacts_secrets() {
        "redacted evidence excerpts"
    } else {
        "UNREDACTED evidence excerpts"
    };
    if address.ip().is_loopback() {
        format!(
            "{address} is loopback — only this machine can reach the page; nothing here \
             authenticates a caller, and it serves {excerpts} of the events a rule counted",
        )
    } else {
        format!(
            "{address} is NOT loopback — this page is UNAUTHENTICATED and the tailnet is the only \
             boundary in front of it; it serves scores, rule ids, counts, content hashes and \
             {excerpts} of the events a rule counted, never a whole transcript and never a link \
             into the archive",
        )
    }
}

/// The second posture line, printed only when `--no-redact` was typed.
///
/// Split from [`posture_line`] rather than folded into it because it is a different kind of
/// statement: the first says what binding *here* means and is always true of a run, while this one
/// says a person turned a default-on security control off and is true only when they did. Nothing
/// is refused — `--no-redact` on a trusted terminal is a legitimate choice the redaction lane
/// already blessed — but on a routable address it is that choice applied to every device on the
/// tailnet, so the sentence is as loud as the situation.
pub fn redaction_posture_line(address: SocketAddr, redactor: &Redactor) -> Option<String> {
    if redactor.redacts_secrets() {
        return None;
    }
    Some(if address.ip().is_loopback() {
        format!(
            "--no-redact: excerpts are served RAW, secrets included, to callers on {address} \
             (loopback)",
        )
    } else {
        format!(
            "!! --no-redact ON A NON-LOOPBACK BIND !! every device that can reach {address} can \
             read UNREDACTED transcript excerpts — live credentials included — from this \
             unauthenticated page. Drop the flag or bind loopback.",
        )
    })
}

/// The payload every request is answered from.
struct Served {
    /// Bumped on every swap. An event stream compares it to know a refresh from a reconnect.
    ///
    /// A **publication** counter, not a fold counter: a failed refresh republishes the previous
    /// fold with this bumped, because a page's numbers going stale is a change worth pushing. See
    /// [`Served::fold_generation`] for the other one, and why one field could not be both.
    generation: u64,
    /// Which fold the body, the anchors, and the corpus below came from.
    ///
    /// Equal to [`Served::generation`] on every successful refresh and deliberately **behind** it
    /// during a failing run: [`republish_as_stale`] swaps in a new publication of the *same* fold,
    /// so the document it re-stamps still says `provenance.generation: N` while the publication is
    /// N+1.
    ///
    /// Two fields rather than one because two different questions are asked of them, and answering
    /// both from `generation` is what put an answer and the page it sits beside a generation apart:
    /// the event stream asks *has the served payload been replaced* (publication), and an ask answer
    /// asks *which fold produced the corpus I ranked* (fold). The second has to agree with what the
    /// page reads out of the payload, or the page's own "these results are older than this document"
    /// check fires on a corpus that never moved.
    fold_generation: u64,
    refreshed_at: DateTime<Utc>,
    /// When the current run of failed refreshes began, if one is under way.
    ///
    /// Carried beside the body as well as patched into it: the JSON payload states it for the page,
    /// and the ask route needs the same fact for an answer that is not built from the payload. One
    /// field read by both is what keeps a search from claiming to be fresher than the document above
    /// it. See [`republish_as_stale`].
    stale_since: Option<DateTime<Utc>>,
    /// Serialized once per refresh rather than once per request, so open tabs cost nothing.
    body: Vec<u8>,
    /// The anchors this body names — the whole servable set while it is the current payload.
    ///
    /// It lives *inside* the served payload rather than beside it so the two can never disagree: a
    /// refresh swaps the document and the set of things it will expand in one move, and a reader
    /// holding an anchor from the previous refresh simply gets a 404 rather than an excerpt from a
    /// finding that is no longer on the page.
    evidence: EvidenceIndex,
    /// The summaries this generation can rank a search against — the whole searchable set while
    /// this payload is the current one.
    ///
    /// Inside the payload for the same reason the evidence index is: one refresh publishes the
    /// document, the anchors it will expand, and the corpus it will search in a single move, so an
    /// answer's stated generation is the generation that actually answered it — which is
    /// [`Served::fold_generation`], the one the body itself states. Behind an [`Arc`] because a
    /// stale re-stamp republishes the same corpus rather than re-reading it, and cloning a pointer
    /// is what makes that free.
    ask: Arc<command::AskCorpus>,
}

impl Served {
    /// What an event stream sends when this payload becomes current. Deliberately not the payload:
    /// the page re-fetches `/api/data`, so the stream stays a few dozen bytes however large the
    /// window is, and a reconnecting page takes the same path as a refreshing one.
    fn notice(&self) -> String {
        format!(
            r#"{{"generation":{},"refreshed_at":"{}"}}"#,
            self.generation,
            crate::report::stamp(self.refreshed_at),
        )
    }
}

/// The served payload, and the way an event stream waits for the next one.
struct Service {
    served: Mutex<Arc<Served>>,
    changed: Condvar,
    streams: AtomicUsize,
    /// The blobs the fold already mirrored. **Read-only on this path, and never a fetch**: an
    /// excerpt request that misses is answered, not filled.
    cache: BlobCache,
    /// The scrub every excerpt goes through, fixed at launch. `Copy`, so a request path never
    /// takes a lock to find out what the redaction posture is.
    redactor: Redactor,
}

impl Service {
    fn new(served: Served, cache: BlobCache, redactor: Redactor) -> Self {
        Self {
            served: Mutex::new(Arc::new(served)),
            changed: Condvar::new(),
            streams: AtomicUsize::new(0),
            cache,
            redactor,
        }
    }

    /// The current payload. Cloning the `Arc` rather than the bytes is what makes a request cheap.
    fn snapshot(&self) -> Arc<Served> {
        Arc::clone(&self.served.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replaces the payload wholesale and wakes every waiting stream.
    ///
    /// Atomic by construction: a reader either has the whole old payload or the whole new one, and
    /// there is no moment at which the page could fetch a document half-built from two folds.
    fn publish(&self, served: Served) {
        let mut current = self.served.lock().unwrap_or_else(PoisonError::into_inner);
        *current = Arc::new(served);
        drop(current);
        self.changed.notify_all();
    }

    /// Blocks until the payload's generation differs from `seen`, or until `timeout` elapses.
    fn wait_for_change(&self, seen: u64, timeout: Duration) -> Option<Arc<Served>> {
        let served = self.served.lock().unwrap_or_else(PoisonError::into_inner);
        if served.generation != seen {
            return Some(Arc::clone(&served));
        }
        let (served, _) = self
            .changed
            .wait_timeout(served, timeout)
            .unwrap_or_else(PoisonError::into_inner);
        (served.generation != seen).then(|| Arc::clone(&served))
    }
}

/// Folds all four lanes once and serializes the payload, narrating each to stderr.
///
/// **One call, one generation, one document.** The four folds happen back to back and what is
/// published is built from all of them at once, so the atomic swap below carries a whole page rather
/// than a section of one: a reader can never see a bill from this refresh beside a standup from the
/// last. A torn view across lanes is not made unlikely here, it is made unrepresentable — there is
/// no intermediate state in which a [`Served`] holds two lanes.
///
/// The fourth lane produces no section. [`command::fold_ask_corpus`] reads every `summary.md` in the
/// archive and keeps them parsed on the [`Served`], so `/api/ask` can rank a request against them
/// without a browser ever being able to make this process talk to the archive. It joins the same
/// generation for the same reason the other three share one: an answer taken over a corpus from a
/// different refresh than the numbers beside it is the torn view under another name.
///
/// # What three lanes actually cost, measured
///
/// Against the production archive on 2026-08-25, warm cache, before the snapshot index: **45.4 s**
/// for the whole refresh — coaching sync 16.10 s + fold 6.32 s, cost sync 15.95 s + fold 4.62 s,
/// standup sync 2.37 s + fold 20 ms. The second refresh in the same process took 45.3 s, so there
/// was no warm-up left to find.
///
/// The blob cache spares the **transfers** and not the requests, and that is the number worth
/// stating plainly rather than hoping for. Every one of the three mirrors reported `0 B
/// transferred` — the cost lane's 705 sessions were all cache hits over a window twice the
/// coaching lane's — and its sync still cost 15.95 s, because [`crate::sync`] asked the archive
/// for one snapshot document per listed session whether or not the artifact behind it was already
/// on disk. So the refresh was very nearly the *sum* of the three lanes' own runs, and an earlier
/// draft of this comment claiming otherwise was wrong.
///
/// That was the friction that pulled qanungo #1's snapshot index (same day): with the documents
/// indexed, the same refresh measured coaching sync 1.25 s + fold 5.54 s, cost sync 1.14 s + fold
/// 4.92 s, standup sync 89 ms + fold 19 ms — **~13 s**, of which the archive is ~2.5 s. The
/// refresh is now fold-bound, which is the term decision 11 said a persistent store would be for.
///
/// What that buys is still worth having. On a cold cache the sharing is real: the standup lane
/// alone measured 4.94 s cold against 2.37 s warm, and the transcript lanes' cold cost is the
/// archive's 3.1 GiB, paid once between the two of them instead of twice. And 45 s inside a
/// five-minute interval is 15% of the process's life spent talking to Patwari, against the 6% the
/// coaching lane alone spent — comfortably inside what [`crate::cli::MIN_DASHBOARD_REFRESH`] was
/// set to protect, and the reason that floor is a floor rather than a default.
///
/// Every line goes to stderr, including the access log below: this lane writes no document to
/// stdout, and keeping the whole narration on one stream means `qanungo dashboard >/dev/null`
/// cannot silently swallow the posture statement.
fn fold_and_publish(
    archive: &ArchiveArgs,
    windows: &Windows,
    refresh: &Refresh,
    refreshed: Refreshed,
    redactor: &Redactor,
) -> Result<Served, CommandError> {
    eprintln!(
        "qanungo dashboard: folding four lanes from {} — coaching {} (and the window before it), \
         cost {} (and the window before it), standup {}, ask all history",
        archive.patwari_url, windows.coaching, windows.cost, windows.standup,
    );
    let started = Instant::now();
    let coaching = command::fold_coaching(archive, &windows.coaching, redactor)?;
    eprintln!(
        "qanungo dashboard: coaching — {}",
        instrumentation_line(&coaching)
    );
    let cost = command::fold_cost(archive, &windows.cost, redactor)?;
    eprintln!(
        "qanungo dashboard: cost — sync {} · fold {} · {} sessions (+{} comparison) · {} records \
         · price table {}",
        format::elapsed(cost.instrumentation.sync.elapsed),
        format::elapsed(cost.instrumentation.fold_elapsed),
        cost.instrumentation.sessions_folded,
        cost.instrumentation.comparison_sessions_folded,
        cost.instrumentation.records_read,
        crate::pricing::PRICE_TABLE_REVISION,
    );
    // The redactor is handed to the fold rather than applied afterwards: `Standup::fold` scrubs on
    // the way into its own types, so what comes back has no unscrubbed string in it for the payload
    // to reach. See `crate::command::FoldedStandup`.
    let standup = command::fold_standup(archive, &windows.standup, *redactor)?;
    eprintln!(
        "qanungo dashboard: standup — sync {} · fold {} · {} sessions across {} repositories · {} \
         read · redaction fired {}",
        format::elapsed(standup.instrumentation.sync.elapsed),
        format::elapsed(standup.instrumentation.fold_elapsed),
        standup.standup.sessions,
        standup.standup.repositories_narrated(),
        format::bytes(standup.standup.bytes_read),
        standup.standup.redaction.total(),
    );
    // The fourth lane, and the only one that produces no section: it reads every `summary.md` in
    // the archive and keeps them parsed, so `/api/ask` can rank a request without reaching for the
    // archive. It joins the same generation as the other three — a failure here stales the whole
    // document, because a search over half a corpus would answer "no" for a session it never read.
    let ask = command::fold_ask_corpus(archive)?;
    eprintln!(
        "qanungo dashboard: ask — sync {} · fold {} · {} of {} listed sessions searchable · {} held",
        format::elapsed(ask.instrumentation.sync.elapsed),
        format::elapsed(ask.instrumentation.fold_elapsed),
        ask.searchable(),
        ask.listed(),
        format::bytes(ask.bytes_read),
    );
    let folds_elapsed = started.elapsed();

    let evidence = dashboard::evidence_index(&coaching);
    let body = serde_json::to_vec(
        &Payload {
            windows,
            refresh,
            coaching: &coaching,
            cost: &cost,
            standup: &standup,
            ask: &ask,
            folds_elapsed,
            refreshed,
            redactor,
        }
        .build(),
    )
    .unwrap_or_else(|_| b"{}".to_vec());
    eprintln!(
        "qanungo dashboard: refresh {} — four lanes in {} · payload {} · {} anchors over {} \
         sessions · {} searchable summaries · serialized at {}",
        refreshed.generation,
        format::elapsed(folds_elapsed),
        format::bytes(body.len() as u64),
        evidence.anchors(),
        evidence.sessions(),
        ask.searchable(),
        format::elapsed(started.elapsed()),
    );
    Ok(Served {
        generation: refreshed.generation,
        // The same number, because this *is* the fold that generation names. The two only come
        // apart when a later refresh fails and republishes this one.
        fold_generation: refreshed.generation,
        refreshed_at: refreshed.at,
        stale_since: refreshed.stale_since,
        body,
        evidence,
        ask: Arc::new(ask),
    })
}

/// The instrumentation footer's own quantities, in the footer's own renderings, on one stderr line.
///
/// Same numbers, same `format` calls, different punctuation: a person watching a long-lived
/// dashboard should be able to compare a refresh line against a `qanungo report` footer without
/// converting anything.
fn instrumentation_line(folded: &Folded) -> String {
    let instrumentation = &folded.instrumentation;
    format!(
        "sync {} · fold {} · {} sessions (+{} comparison) · {} folded · cache {} hits / {} misses \
         ({} transferred) · snapshots {} indexed / {} fetched · rule pack {}",
        format::elapsed(instrumentation.sync.elapsed),
        format::elapsed(instrumentation.fold_elapsed),
        instrumentation.sessions_folded,
        instrumentation.comparison_sessions_folded,
        format::bytes(instrumentation.bytes_folded),
        instrumentation.sync.cache_hits,
        instrumentation.sync.cache_misses,
        format::bytes(instrumentation.sync.bytes_transferred),
        instrumentation.sync.snapshots_indexed,
        instrumentation.sync.snapshots_fetched,
        instrumentation.rule_pack.stamp(),
    )
}

/// Re-syncs and re-folds on the interval, forever.
///
/// A failed refresh keeps the last good payload and republishes it with `stale_since` set, rather
/// than blanking the page or serving nothing: the numbers are still true of the window they were
/// taken over, and the honest correction is to date them, not to hide them. The republish bumps the
/// generation on purpose — a page's numbers becoming stale is a change worth pushing.
fn refresh_loop(
    service: &Service,
    archive: &ArchiveArgs,
    windows: &Windows,
    refresh: &Refresh,
    redactor: &Redactor,
) {
    let mut generation = 1;
    let mut stale_since: Option<DateTime<Utc>> = None;
    loop {
        thread::sleep(refresh.interval());
        generation += 1;
        let at = Utc::now();
        match fold_and_publish(
            archive,
            windows,
            refresh,
            Refreshed {
                generation,
                at,
                stale_since: None,
            },
            redactor,
        ) {
            Ok(served) => {
                stale_since = None;
                service.publish(served);
            }
            Err(error) => {
                // The *first* failure of a run dates the numbers: what a reader needs is the age of
                // the last success, and re-dating on every failure would make a dashboard that has
                // been broken all day look a minute old.
                let since = *stale_since.get_or_insert(at);
                eprintln!(
                    "qanungo dashboard: refresh {generation} failed, serving the fold from {} — \
                     {error}",
                    crate::report::stamp(since),
                );
                republish_as_stale(service, generation, since);
            }
        }
    }
}

/// Re-stamps the served payload as stale without re-folding it.
///
/// Patching the serialized document rather than rebuilding it keeps the failure path from needing
/// the [`Folded`] that produced it, which would mean holding a second window's worth of session
/// metrics alive for the whole life of the process against the chance the archive goes away.
///
/// **What a re-stamp costs now.** It bumps the generation on purpose — a page's numbers going stale
/// is a change worth pushing — and since the standup-and-cost slice the body every open tab then
/// re-fetches is ~744 KiB against production rather than the ~162 KiB it was, for a payload whose
/// only changed bytes are one timestamp. On a tailnet with a handful of tabs that is not worth a
/// mechanism, and inventing a patch protocol so a stale-stamp could be pushed without a re-fetch
/// would be a second way for a page and a payload to disagree about a generation. The honest fix is
/// upstream of this function: the standup section is 71% of that body, and bounding what a served
/// narrative renders is the follow-up where it gets addressed — see [`crate::dashboard`]'s
/// "One route, measured".
///
/// # Why the body's own generation is left alone
///
/// The publication number goes up and the *document* keeps saying `provenance.generation: N`,
/// because N is the fold it is still showing. Patching the number to N+1 was the other way to close
/// the gap this function once opened — an ask answer stamping N+1 while the payload beside it said
/// N — and it is the wrong way round: it would make the document claim a refresh that never
/// happened, which is exactly the lie `stale_since` exists to prevent. So [`Served::fold_generation`]
/// carries N through the re-stamp instead, and the ask route stamps that. One patched key, still.
fn republish_as_stale(service: &Service, generation: u64, since: DateTime<Utc>) {
    let current = service.snapshot();
    let stale = format!(r#""stale_since":"{}""#, crate::report::stamp(since));
    let body = match String::from_utf8(current.body.clone()) {
        Ok(document) => document
            .replace(r#""stale_since":null"#, &stale)
            .into_bytes(),
        Err(_) => current.body.clone(),
    };
    service.publish(Served {
        generation,
        // Kept, not bumped: the body below is still the fold it was, and it still says so. This is
        // what an ask answer stamps, so a search and the document it sits beside name one fold.
        fold_generation: current.fold_generation,
        refreshed_at: current.refreshed_at,
        stale_since: Some(since),
        body,
        // The same anchors: this is the same fold, re-stamped. A stale page that can still expand
        // its own findings is the point of keeping the numbers at all.
        evidence: current.evidence.clone(),
        // And the same corpus, for the same reason: a search that still answers over the summaries
        // of the last good refresh is better than one that refuses, provided the answer says how old
        // they are — which `stale_since` above is what lets it do.
        ask: Arc::clone(&current.ask),
    });
}

/// Accepts connections until the process is interrupted.
///
/// One thread per connection, unbounded: the read and write timeouts are what reclaim them, and a
/// personal dashboard on a tailnet has no traffic shape a pool would help with. A connection that
/// cannot get a thread is dropped with a line on stderr — one refused reader must not take the
/// dashboard down.
fn serve(listener: &TcpListener, service: &Arc<Service>) {
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let service = Arc::clone(service);
                if let Err(error) = thread::Builder::new()
                    .name("dashboard-connection".to_owned())
                    .spawn(move || handle(stream, &service))
                {
                    eprintln!("qanungo dashboard: could not spawn a connection thread: {error}");
                }
            }
            Err(error) => eprintln!("qanungo dashboard: accept failed: {error}"),
        }
    }
}

/// One connection: read the head, route it, answer it, close it.
fn handle(mut stream: TcpStream, service: &Service) {
    // Failures here are ignored deliberately: a socket that will not take a timeout is a socket
    // that is about to fail the read anyway, and refusing to serve it would be the harsher answer.
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let peer = stream
        .peer_addr()
        .map_or_else(|_| "unknown".to_owned(), |address| address.to_string());

    let Ok(head) = read_head(&mut stream) else {
        return;
    };
    let head = String::from_utf8_lossy(&head);
    let Some((method, target)) = parse_request_line(&head) else {
        let _ = write_response(
            &mut stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            b"bad request",
        );
        return;
    };

    // Routing reads the request line's real bytes; every log line below reads this instead. Named
    // once rather than clamped at five call sites, so a log line added later cannot be the one that
    // forgets.
    let request = logged_request(method, target);

    match route(method, target) {
        Route::Page => {
            eprintln!("qanungo dashboard: {peer} - {request} 200");
            let _ = write_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                PAGE.as_bytes(),
            );
        }
        Route::Data => {
            let served = service.snapshot();
            eprintln!(
                "qanungo dashboard: {peer} - {request} 200 (refresh {})",
                served.generation,
            );
            let _ = write_response(&mut stream, "200 OK", "application/json", &served.body);
        }
        Route::Events => {
            // The count is taken before the stream starts and released when it ends, so a refused
            // stream costs a status rather than a thread that parks forever.
            if service.streams.fetch_add(1, Ordering::Relaxed) >= MAX_EVENT_STREAMS {
                service.streams.fetch_sub(1, Ordering::Relaxed);
                eprintln!("qanungo dashboard: {peer} - {request} 503");
                let _ = write_response(
                    &mut stream,
                    "503 Service Unavailable",
                    "text/plain; charset=utf-8",
                    b"too many open event streams",
                );
                return;
            }
            eprintln!("qanungo dashboard: {peer} - {request} 200 (event stream opened)");
            let _ = stream_events(service, &mut stream);
            service.streams.fetch_sub(1, Ordering::Relaxed);
            eprintln!("qanungo dashboard: {peer} - event stream closed");
        }
        Route::Evidence {
            source_hash,
            locator,
        } => {
            let (status, body) = evidence_response(service, &source_hash, locator);
            eprintln!("qanungo dashboard: {peer} - {request} {status}");
            let _ = write_response(&mut stream, status, "application/json", &body);
        }
        Route::Ask(ask) => {
            let (status, body) = ask_response(service, &ask);
            eprintln!("qanungo dashboard: {peer} - {request} {status}");
            let _ = write_response(&mut stream, status, "application/json", &body);
        }
        Route::NotFound => {
            eprintln!("qanungo dashboard: {peer} - {request} 404");
            let _ = write_response(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"not found",
            );
        }
    }
}

/// Answers one excerpt request: the anchored event, scrubbed — or a 404 that says which of the
/// route's refusals stopped it.
///
/// The order matters and is the security argument, not an optimization. **What the payload named**
/// is checked before anything touches a disk, so an unanchored locator cannot even probe whether a
/// hash is cached. **The cache** is checked next and answered rather than filled, so no request can
/// make this process reach for the archive. Only then is a blob opened, re-parsed, and scrubbed.
///
/// Every refusal is a 404 with a reason rather than a 403 or a 400: the caller is unauthenticated
/// and the honest thing to tell them is that there is no such evidence here, while the *reason*
/// exists for the operator reading a log or a developer reading a response — and none of the four
/// reasons discloses anything the payload did not already say.
fn evidence_response(
    service: &Service,
    source_hash: &str,
    locator: u64,
) -> (&'static str, Vec<u8>) {
    let served = service.snapshot();
    let Some(session) = served.evidence.servable(source_hash, locator) else {
        return not_evidence(
            source_hash,
            locator,
            "not-anchored",
            "no finding in the current payload offers this anchor; this route serves the events \
             the page names and is not a way to read a transcript",
        );
    };
    if !service.cache.contains(source_hash) {
        return not_evidence(
            source_hash,
            locator,
            "cache-miss",
            "this transcript is not in the local cache; the dashboard never fetches from the \
             archive to answer a request, so this waits for the next refresh to mirror it",
        );
    }
    let Some(source) = metrics::source_for_agent(&session.source_agent) else {
        return not_evidence(
            source_hash,
            locator,
            "unknown-harness",
            "no interpreter for this harness in this build",
        );
    };
    let blob = match service.cache.open_blob(source_hash) {
        Ok(blob) => blob,
        Err(error) => {
            // The cache said it had it a moment ago, so this is a real local failure rather than a
            // caller's mistake, and it is the operator's to fix.
            eprintln!("qanungo dashboard: cached blob unreadable: {error}");
            return (
                "500 Internal Server Error",
                serialize(&json!({
                    "error": "the cached transcript could not be read",
                    "source_hash": source_hash,
                    "locator": locator,
                })),
            );
        }
    };
    let extracted = evidence::extract(
        source,
        session.artifact_set_version,
        io::BufReader::new(blob),
        locator,
    );
    match extracted {
        Ok(Some(raw)) => {
            let excerpt = raw.redacted(&service.redactor);
            (
                "200 OK",
                serialize(&excerpt_value(source_hash, &excerpt, &service.redactor)),
            )
        }
        Ok(None) => not_evidence(
            source_hash,
            locator,
            "no-such-event",
            "the cached transcript has no event at this locator",
        ),
        Err(error) => not_evidence(
            source_hash,
            locator,
            "unreadable-contract",
            &error.to_string(),
        ),
    }
}

/// Answers one search request: the ranking over the current generation's corpus — or the refusal
/// that stopped it before anything was parsed.
///
/// The order is the same argument the excerpt route's is, reaching the same place from the other
/// end. **The cap is checked before the parse**, so a kilobyte of a caller's choosing is never split
/// into terms, let alone scored against every summary in the archive. **The corpus is the served
/// one**, taken with the payload's own snapshot, so an answer and the document it sits beside are
/// the same generation and the answer says which. And **there is nothing here to fetch**: the corpus
/// was mirrored on the refresh timer, and a search that came up empty is an empty answer rather than
/// a reason to reach for the archive.
///
/// A query with no searchable word in it takes the short path [`command::ask`] takes: the answer
/// shape with `state: "no-searchable-terms"` and no ranking at all, which costs nothing over a
/// corpus of any size.
fn ask_response(service: &Service, request: &AskRequest) -> (&'static str, Vec<u8>) {
    let (query, limit) = match request {
        AskRequest::Search { query, limit } => (query, *limit),
        // A 400 rather than the evidence route's 404: that route refuses to say whether something
        // exists, and there is nothing to be coy about here — the caller's request is malformed by
        // a published grammar, and saying so with the bound is what lets them fix it. No byte of
        // the query is echoed back; the two numbers are this build's own.
        AskRequest::TooLong { bytes } => {
            return (
                "400 Bad Request",
                serialize(&json!({
                    "error": "the query is too long to search",
                    "reason": "query-too-long",
                    "bytes": bytes,
                    "max_bytes": MAX_QUERY_BYTES,
                })),
            );
        }
    };
    let served = service.snapshot();
    let parsed = Query::parse(query);
    // Parsed but never scored when there is nothing to score on — the same refusal `qanungo ask`
    // makes before it touches the archive, made here before it touches the corpus.
    let ask = (!parsed.is_empty()).then(|| served.ask.search(&parsed, &service.redactor, limit));
    (
        "200 OK",
        serialize(
            &dashboard::AskAnswer {
                query: &parsed,
                limit,
                ask: ask.as_ref(),
                corpus: &served.ask,
                refreshed: Refreshed {
                    // The **fold's** generation, not the publication's: it is the number the
                    // payload's own provenance block carries, and the page compares the two to
                    // decide whether an answer it is still showing predates the corpus in hand. A
                    // stale re-stamp bumps the publication and re-reads nothing, so stamping that
                    // here would tell a reader to search again for a corpus that never moved.
                    generation: served.fold_generation,
                    at: served.refreshed_at,
                    stale_since: served.stale_since,
                },
                redactor: &service.redactor,
            }
            .build(),
        ),
    )
}

/// One refusal, as JSON. The hash and the locator are echoed back because both have already been
/// validated to their grammars — 64 lowercase hex and a bounded integer — so neither can carry a
/// byte the caller chose.
fn not_evidence(
    source_hash: &str,
    locator: u64,
    reason: &str,
    detail: &str,
) -> (&'static str, Vec<u8>) {
    (
        "404 Not Found",
        serialize(&json!({
            "error": "no such evidence",
            "reason": reason,
            "detail": detail,
            "source_hash": source_hash,
            "locator": locator,
        })),
    )
}

/// One excerpt, as JSON: the counted event and the account of what scrubbing it cost.
///
/// The redaction block is not decoration. A reader looking at an excerpt with no markers in it
/// needs to know whether that means "nothing matched" or "the scrub was off", and those are very
/// different sentences — so every excerpt carries the posture and the fired counts, and the counts
/// come from [`crate::redaction::RedactionReport`], which cannot carry what it matched.
fn excerpt_value(
    source_hash: &str,
    excerpt: &crate::evidence::Excerpt,
    redactor: &Redactor,
) -> serde_json::Value {
    json!({
        "source_hash": source_hash,
        "locator": excerpt.locator,
        "record": excerpt.record,
        "line": excerpt.line,
        "at": excerpt.at.map(stamp),
        "tool": excerpt.tool,
        "event": excerpt.event,
        "outcome": excerpt.outcome,
        "command": excerpt.command,
        "error": excerpt.error,
        "output": excerpt.output,
        "truncated": excerpt.truncated,
        "redaction": {
            "secrets": redactor.redacts_secrets(),
            "profanity": redactor.filters_profanity(),
            "pattern_revision": crate::redaction::PATTERN_REVISION,
            "total": excerpt.report.total(),
            "fired": excerpt
                .report
                .fired()
                .map(|(pattern, count)| json!({ "pattern": pattern.as_str(), "count": count }))
                .collect::<Vec<_>>(),
        },
    })
}

/// Serializes a response body, falling back to a bare error object rather than panicking: this is
/// on a request path, and every value above is built from numbers and validated strings.
fn serialize(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| br#"{"error":"unserializable"}"#.to_vec())
}

/// Holds one event stream open, writing a refresh notice on every swap and a comment between them.
///
/// The first notice is sent immediately: a page that has just connected needs to know which
/// generation it is looking at, and sending it here means a reconnecting page and a refreshing page
/// take exactly the same path.
fn stream_events(service: &Service, stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/event-stream\r\n\
          Cache-Control: no-store\r\n\
          Connection: close\r\n\
          X-Accel-Buffering: no\r\n\r\n",
    )?;
    let mut seen = {
        let served = service.snapshot();
        stream.write_all(format!("retry: {SSE_RETRY_MILLIS}\n\n").as_bytes())?;
        stream.write_all(sse_event("refresh", &served.notice()).as_bytes())?;
        stream.flush()?;
        served.generation
    };
    loop {
        match service.wait_for_change(seen, SSE_HEARTBEAT) {
            Some(served) => {
                seen = served.generation;
                stream.write_all(sse_event("refresh", &served.notice()).as_bytes())?;
            }
            None => stream.write_all(b": keep-alive\n\n")?,
        }
        stream.flush()?;
    }
}

/// One SSE event, framed.
///
/// A free function because the framing is the part worth pinning: an event whose data carried a
/// newline would silently become two events, and the only data this server sends is a one-line JSON
/// object it built itself. The debug assertion says so rather than leaving it to be true by luck.
fn sse_event(name: &str, data: &str) -> String {
    debug_assert!(
        !data.contains('\n'),
        "an SSE data line may not carry a newline: {data:?}",
    );
    format!("event: {name}\ndata: {data}\n\n")
}

/// How one request is named in the access log.
///
/// Both halves are bytes an unknown caller chose, and the access log's reader is a terminal — a
/// rendering surface with an interpreter behind it. It gets the same clamp every other rendering
/// surface in this crate gets ([`format::logged`]): nothing that is not printable ASCII survives,
/// so a request line cannot set a window title, ring a bell, hide the lines above it, or forge a
/// second log line. Escaped rather than replaced, because a log exists to say what was asked for
/// and the strange request is the one worth reading.
///
/// A free function for the same reason [`sse_event`] is one: the guarantee is worth pinning
/// directly, and the debug assertion states it rather than leaving it true by luck.
fn logged_request(method: &str, target: &str) -> String {
    let request = format!("{} {}", format::logged(method), format::logged(target));
    debug_assert!(
        !request.chars().any(char::is_control),
        "a log line may not carry a control character: {request:?}",
    );
    request
}

/// Which of the five routes a request is for.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// The embedded page.
    Page,
    /// The JSON payload.
    Data,
    /// The refresh event stream.
    Events,
    /// One anchored event, scrubbed. Parsed here rather than in the handler so that a target which
    /// is not *exactly* a hash and a locator never becomes a lookup at all.
    Evidence {
        source_hash: String,
        locator: u64,
    },
    /// One ranked search over the current generation's summary corpus, already bounded.
    Ask(AskRequest),
    NotFound,
}

/// One ask request as the router settled it, before anything has been parsed or scored.
///
/// Two arms rather than an `Option`, because an over-long query is **refused with a status** and a
/// missing one is answered: "you sent me a kilobyte" and "you sent me nothing to search on" are
/// different sentences, and the second is one of this lane's three honest answers rather than an
/// error. Both are decided in [`route`], so a request that will be refused never becomes a lookup
/// against what this process holds.
#[derive(Debug, PartialEq, Eq)]
enum AskRequest {
    /// A query inside the cap, percent-decoded, with the limit already clamped.
    Search { query: String, limit: usize },
    /// The raw `q` was over [`MAX_QUERY_BYTES`] and was refused before it was decoded.
    TooLong { bytes: usize },
}

/// Routes a request. `GET` only, case-sensitively.
///
/// There is no 405: a non-`GET` request to this surface is not a method mismatch worth negotiating,
/// it is a caller who has the wrong server. Nothing is read from the filesystem, so path traversal
/// has nothing to traverse to — an unmatched target is simply not a route.
///
/// The evidence target is **strictly validated before it is anything**: 64 lowercase hex characters
/// — the same [`cache::is_sha256_hex`] the blob cache checks a digest with, so the route and the
/// store cannot come to disagree about what a hash is — and a bare bounded positive integer. A
/// target with a trailing slash, an extra segment, an uppercase hex digit, or a locator with a sign
/// is not a repairable request; it is not this route.
///
/// # The one route whose query string is its argument
///
/// For four of the five, the query string decides nothing and is discarded before anything else
/// happens. `/api/ask` is the exception and is bounded in the same breath as it is read: `q` capped
/// and `limit` clamped by [`ask_route`], every other parameter ignored. What has not changed is the
/// thing that sentence was ever protecting — **no parameter on this surface selects a redaction
/// posture, a window, a scope, or a session**, so nothing a caller writes can make this process say
/// something about the archive it would not otherwise say.
fn route(method: &str, target: &str) -> Route {
    if method != "GET" {
        return Route::NotFound;
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };
    match path {
        "/" | "/index.html" => Route::Page,
        "/api/data" => Route::Data,
        "/api/events" => Route::Events,
        "/api/ask" => Route::Ask(ask_route(query)),
        _ => evidence_route(path),
    }
}

/// `?q=<terms>&limit=<n>`, bounded.
///
/// Both parameters are optional and neither is trusted. An absent, empty, or unparseable `limit`
/// takes [`DEFAULT_ASK_LIMIT`] and any value is clamped into `1..=`[`MAX_ASK_LIMIT`]; an absent `q`
/// is an empty query, which is answered rather than refused. The **first** occurrence of each key
/// wins — a fixed rule rather than last-wins, so `?q=a&q=b` has one meaning and not two.
///
/// The cap is measured on the raw value before [`decode_query_value`] touches it, which is the
/// point: percent-decoding can only shrink a value, so refusing on the encoded length bounds both
/// and does it before this process has spent anything on the request.
fn ask_route(query: &str) -> AskRequest {
    let raw = query_parameter(query, "q").unwrap_or("");
    if raw.len() > MAX_QUERY_BYTES {
        return AskRequest::TooLong { bytes: raw.len() };
    }
    let limit = query_parameter(query, "limit")
        .and_then(|value| decode_query_value(value).parse::<usize>().ok())
        .unwrap_or(DEFAULT_ASK_LIMIT)
        .clamp(1, MAX_ASK_LIMIT);
    AskRequest::Search {
        query: decode_query_value(raw),
        limit,
    }
}

/// The first value of `name` in a query string, undecoded, or `None` when the key is not there.
///
/// A bare key (`?q`) reads as absent rather than as an empty value: there is no difference worth
/// distinguishing on this route, and treating it as present would be inventing one.
fn query_parameter<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// Percent-decoding, for the one route that reads a parameter.
///
/// Hand-rolled rather than a dependency, on the same reasoning the rest of this lane is hand-rolled
/// (munshi ADR 0006): it is a dozen lines and the alternative is a crate on a path that has to be
/// auditable. `+` is a space, `%XX` is a byte, and anything else is itself — a lone `%` or a `%` with
/// a bad tail is passed through rather than refused, because this is a *search query* and the honest
/// answer to a malformed escape is to search for what was sent.
///
/// The decoded bytes are read back as UTF-8 **lossily**. A caller can send any byte here; what comes
/// out is a `String`, and [`crate::ask::Query::parse`] then keeps only its alphanumeric runs, so
/// nothing a caller chose reaches an answer or a log except through that filter and
/// [`format::logged`].
fn decode_query_value(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => match hex_byte(bytes[index + 1], bytes[index + 2]) {
                Some(byte) => {
                    decoded.push(byte);
                    index += 3;
                }
                None => {
                    decoded.push(b'%');
                    index += 1;
                }
            },
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Two hex digits as the byte they spell, in either case, or `None` when they are not two hex digits.
fn hex_byte(high: u8, low: u8) -> Option<u8> {
    let digit = |byte: u8| (byte as char).to_digit(16).map(|value| value as u8);
    Some(digit(high)? << 4 | digit(low)?)
}

/// `/api/evidence/<64 hex>/<locator>`, or nothing.
fn evidence_route(path: &str) -> Route {
    let Some(rest) = path.strip_prefix("/api/evidence/") else {
        return Route::NotFound;
    };
    // Exactly two segments: `split_once` plus a check that the tail carries no further slash, so
    // `/api/evidence/<hash>/1/2` and `/api/evidence/<hash>/1/` are both simply not this route.
    let Some((source_hash, locator)) = rest.split_once('/') else {
        return Route::NotFound;
    };
    if !cache::is_sha256_hex(source_hash) {
        return Route::NotFound;
    }
    match evidence::parse_locator(locator) {
        Some(locator) => Route::Evidence {
            source_hash: source_hash.to_owned(),
            locator,
        },
        None => Route::NotFound,
    }
}

/// The method and target of a request, or `None` when the first line is not one.
fn parse_request_line(head: &str) -> Option<(&str, &str)> {
    let mut tokens = head.split("\r\n").next()?.split(' ');
    let method = tokens.next().filter(|token| !token.is_empty())?;
    let target = tokens.next().filter(|token| !token.is_empty())?;
    Some((method, target))
}

/// Reads until the blank line that ends a request head, or until [`MAX_REQUEST_BYTES`].
///
/// Generic over the reader so the loop can be driven by a fake that fails if read past the head,
/// which is how a test proves it stops at the blank line rather than waiting for a peer that will
/// send nothing more. No body is ever read: every route is a `GET`.
fn read_head<R: Read>(stream: &mut R) -> io::Result<Vec<u8>> {
    let mut head = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&buffer[..read]);
        if head.len() >= MAX_REQUEST_BYTES || head.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    Ok(head)
}

/// Writes one framed response.
///
/// `Cache-Control: no-store` because every one of these is a snapshot of a moving fold, and a
/// browser that served yesterday's payload out of its own cache would be a coaching dashboard
/// quietly reporting last week.
fn write_response<W: Write>(
    stream: &mut W,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A corpus with nothing in it, for the tests that are about the payload plumbing rather than
    /// about the search. An archive whose every session is unreadable is exactly this state, so it
    /// is a real value and not only a stand-in.
    fn empty_corpus() -> Arc<command::AskCorpus> {
        Arc::new(command::AskCorpus::over(Utc::now(), Vec::new(), 0))
    }

    /// One search request, as the handler answers it.
    fn ask(service: &Service, target: &str) -> (&'static str, serde_json::Value) {
        let Route::Ask(request) = route("GET", target) else {
            panic!("{target} is the ask route");
        };
        let (status, body) = ask_response(service, &request);
        (
            status,
            serde_json::from_slice(&body).expect("the answer is JSON"),
        )
    }

    /// A service over an empty corpus, which is all the two refusal paths need.
    fn service_over_nothing() -> (Service, tempfile::TempDir) {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let service = Service::new(
            Served {
                generation: 2,
                fold_generation: 2,
                refreshed_at: Utc::now(),
                stale_since: None,
                body: b"{}".to_vec(),
                evidence: EvidenceIndex::default(),
                ask: empty_corpus(),
            },
            BlobCache::open(scratch.path()).expect("a scratch cache"),
            Redactor::new(),
        );
        (service, scratch)
    }

    #[test]
    fn the_five_routes_are_the_only_routes() {
        assert_eq!(route("GET", "/"), Route::Page);
        assert_eq!(route("GET", "/index.html"), Route::Page);
        assert_eq!(route("GET", "/api/data"), Route::Data);
        assert_eq!(route("GET", "/api/events"), Route::Events);
        assert_eq!(
            route("GET", &format!("/api/evidence/{}/12", "a".repeat(64))),
            Route::Evidence {
                source_hash: "a".repeat(64),
                locator: 12,
            },
        );
        assert_eq!(
            route("GET", "/api/ask?q=redaction&limit=3"),
            Route::Ask(AskRequest::Search {
                query: "redaction".to_owned(),
                limit: 3,
            }),
        );
        for target in [
            "/api",
            "/api/data/",
            "/favicon.ico",
            "/assets/dashboard.html",
            "/../../etc/passwd",
            "/api/v1/artifacts/abc/content",
            "/api/evidence",
            "/api/evidence/",
            // The ask route is one exact path, like the other four: a trailing slash or a segment
            // under it is not a narrower search, it is not this route.
            "/api/ask/",
            "/api/ask/redaction",
        ] {
            assert_eq!(route("GET", target), Route::NotFound, "{target}");
        }
        for method in ["POST", "PUT", "DELETE", "HEAD", "get"] {
            assert_eq!(route(method, "/api/ask?q=x"), Route::NotFound, "{method}");
        }
    }

    /// The evidence target is a grammar, not a suggestion. Everything here is refused *at the
    /// router*, before anything is looked up, so a malformed target never becomes a question about
    /// what this process has on disk.
    #[test]
    fn an_evidence_target_is_a_hash_and_a_bounded_integer_or_it_is_not_a_route() {
        let hash = "0123456789abcdef".repeat(4);
        assert_eq!(hash.len(), 64);
        assert_eq!(
            route("GET", &format!("/api/evidence/{hash}/1")),
            Route::Evidence {
                source_hash: hash.clone(),
                locator: 1,
            },
        );
        // A query string is a caller's business here as everywhere else on this surface.
        assert_eq!(
            route("GET", &format!("/api/evidence/{hash}/1?raw=1")),
            Route::Evidence {
                source_hash: hash.clone(),
                locator: 1,
            },
        );
        for target in [
            format!("/api/evidence/{hash}"),
            format!("/api/evidence/{hash}/"),
            format!("/api/evidence/{hash}/1/"),
            format!("/api/evidence/{hash}/1/2"),
            // Uppercase hex is not how a digest is spelled here, and a route that accepted both
            // spellings would be a second definition of "is a digest".
            format!("/api/evidence/{}/1", hash.to_uppercase()),
            format!("/api/evidence/{}/1", &hash[..63]),
            format!("/api/evidence/{}0/1", hash),
            format!("/api/evidence/{}/1", "g".repeat(64)),
            format!("/api/evidence/{hash}/0"),
            format!("/api/evidence/{hash}/007"),
            format!("/api/evidence/{hash}/-1"),
            format!("/api/evidence/{hash}/ 1"),
            format!("/api/evidence/{hash}/1234567890"),
            format!("/api/evidence/{hash}/../../etc/passwd"),
            format!("/api/evidence/../{hash}/1"),
        ] {
            assert_eq!(route("GET", &target), Route::NotFound, "{target}");
        }
        for method in ["POST", "PUT", "DELETE", "HEAD", "get"] {
            assert_eq!(
                route(method, &format!("/api/evidence/{hash}/1")),
                Route::NotFound,
                "{method}",
            );
        }
    }

    /// A query string is a caller's business and never the server's on the four routes that answer
    /// from the served payload: `/api/data?since=x` is the same route as `/api/data`, and none of
    /// them reads a parameter at all — a per-request knob on a surface with a redaction posture is
    /// exactly what the grilling ruled out.
    #[test]
    fn query_strings_do_not_change_the_four_payload_routes() {
        assert_eq!(route("GET", "/api/data?cachebust=1"), Route::Data);
        assert_eq!(route("GET", "/?x=y"), Route::Page);
        assert_eq!(route("GET", "/api/events?last=99"), Route::Events);
        assert_eq!(
            route("GET", &format!("/api/evidence/{}/1?raw=1", "a".repeat(64))),
            Route::Evidence {
                source_hash: "a".repeat(64),
                locator: 1,
            },
        );
    }

    /// The ask route is the one that reads its query string, and what it will read is two keys.
    ///
    /// The sentence the exception has to survive is the one above: no parameter on this surface
    /// selects a redaction posture, a window, a scope, or a session. So every other key here —
    /// including ones named after the page's own scope controls — is ignored rather than
    /// interpreted, and a request carrying them is the *same* request as one that does not.
    #[test]
    fn the_ask_route_reads_two_parameters_and_ignores_every_other() {
        let plain = route("GET", "/api/ask?q=redaction");
        assert_eq!(
            plain,
            Route::Ask(AskRequest::Search {
                query: "redaction".to_owned(),
                limit: DEFAULT_ASK_LIMIT,
            }),
        );
        for target in [
            "/api/ask?q=redaction&repository=surdy/qanungo",
            "/api/ask?q=redaction&device=macbookpro&harness=claude-code",
            "/api/ask?q=redaction&last=30d",
            "/api/ask?redact=off&q=redaction",
            "/api/ask?q=redaction&verbatim=1",
        ] {
            assert_eq!(route("GET", target), plain, "{target}");
        }
        // No `q` at all is an empty query — answered, not refused. See `ask_response`.
        assert_eq!(
            route("GET", "/api/ask"),
            Route::Ask(AskRequest::Search {
                query: String::new(),
                limit: DEFAULT_ASK_LIMIT,
            }),
        );
        // The first occurrence of a key wins, so a repeated one has one meaning and not two.
        assert_eq!(route("GET", "/api/ask?q=first&q=second"), {
            Route::Ask(AskRequest::Search {
                query: "first".to_owned(),
                limit: DEFAULT_ASK_LIMIT,
            })
        });
    }

    /// `limit` is clamped rather than trusted or refused: this is a browser's request, so it gets an
    /// answer, and the answer says which limit was used.
    #[test]
    fn the_ask_limit_is_clamped_into_its_range() {
        let limit_of = |target: &str| match route("GET", target) {
            Route::Ask(AskRequest::Search { limit, .. }) => limit,
            other => panic!("{target} is a search: {other:?}"),
        };
        assert_eq!(limit_of("/api/ask?q=x&limit=3"), 3);
        assert_eq!(limit_of("/api/ask?q=x&limit=0"), 1, "never nothing");
        assert_eq!(limit_of("/api/ask?q=x&limit=9999"), MAX_ASK_LIMIT);
        assert_eq!(
            limit_of("/api/ask?q=x&limit=-1"),
            DEFAULT_ASK_LIMIT,
            "unparseable is the default, not an error",
        );
        assert_eq!(limit_of("/api/ask?q=x&limit=lots"), DEFAULT_ASK_LIMIT);
        assert_eq!(limit_of("/api/ask?q=x&limit="), DEFAULT_ASK_LIMIT);
        assert_eq!(limit_of("/api/ask?q=x"), DEFAULT_ASK_LIMIT);
    }

    /// The cap is on the raw value and is checked at the router, before a single byte is decoded —
    /// so a kilobyte of somebody's choosing never becomes terms, let alone a scan of every summary.
    #[test]
    fn an_over_long_query_is_refused_before_it_is_decoded() {
        let inside = "a".repeat(MAX_QUERY_BYTES);
        assert_eq!(
            route("GET", &format!("/api/ask?q={inside}")),
            Route::Ask(AskRequest::Search {
                query: inside,
                limit: DEFAULT_ASK_LIMIT,
            }),
            "the cap is inclusive",
        );
        let over = "a".repeat(MAX_QUERY_BYTES + 1);
        assert_eq!(
            route("GET", &format!("/api/ask?q={over}")),
            Route::Ask(AskRequest::TooLong {
                bytes: MAX_QUERY_BYTES + 1
            }),
        );
        // Measured on the *encoded* value: escapes only shrink, so this is the bound on both.
        let escaped = "%41".repeat(MAX_QUERY_BYTES);
        assert!(matches!(
            route("GET", &format!("/api/ask?q={escaped}")),
            Route::Ask(AskRequest::TooLong { .. }),
        ));

        let (service, _scratch) = service_over_nothing();
        let (status, body) = ask(&service, &format!("/api/ask?q={}", "a".repeat(4096)));
        assert_eq!(status, "400 Bad Request");
        // The page renders `error` to the reader and matches on `reason`, so both are pinned.
        assert_eq!(body["error"], "the query is too long to search");
        assert_eq!(body["reason"], "query-too-long");
        assert_eq!(body["max_bytes"], MAX_QUERY_BYTES);
        assert_eq!(body["bytes"], 4096);
        // Not one byte of what was sent comes back: the refusal is made of this build's own words
        // and two numbers.
        assert!(
            !serde_json::to_string(&body).unwrap().contains("aaaa"),
            "{body}",
        );
    }

    /// Percent-decoding, for the one route that reads a parameter. A malformed escape is searched
    /// for rather than refused — this is a query box, and the honest answer to `50%` is to look for
    /// what was typed.
    #[test]
    fn a_query_value_is_percent_decoded_and_a_bad_escape_is_kept() {
        assert_eq!(decode_query_value("payments+api"), "payments api");
        assert_eq!(decode_query_value("payments%20api"), "payments api");
        assert_eq!(decode_query_value("price%2Dtable"), "price-table");
        assert_eq!(decode_query_value("%E2%9C%93"), "✓");
        assert_eq!(decode_query_value("100%"), "100%");
        assert_eq!(decode_query_value("50%zz"), "50%zz");
        assert_eq!(decode_query_value("%2"), "%2");
        assert_eq!(decode_query_value(""), "");
        assert_eq!(hex_byte(b'4', b'1'), Some(b'A'));
        assert_eq!(hex_byte(b'e', b'2'), Some(0xe2));
        assert_eq!(hex_byte(b'E', b'2'), Some(0xe2));
        assert_eq!(hex_byte(b'z', b'1'), None);
    }

    /// A query with no word to search on is answered rather than refused, and answered without
    /// ranking anything — the same short path `qanungo ask` takes before it touches the archive.
    #[test]
    fn a_query_with_nothing_to_search_on_is_an_answer() {
        let (service, _scratch) = service_over_nothing();
        let (status, body) = ask(&service, "/api/ask?q=the+a+of");
        assert_eq!(status, "200 OK");
        assert_eq!(body["state"], "no-searchable-terms");
        assert_eq!(body["query"]["terms"], serde_json::json!([]));
        assert_eq!(body["hits"], serde_json::json!([]));
        assert_eq!(body["corpus"]["generation"], 2);
        // And so is no `q` at all.
        assert_eq!(ask(&service, "/api/ask").1["state"], "no-searchable-terms");
    }

    /// Read-only by construction: there is no verb here but `GET`, and the match is case-sensitive
    /// because HTTP methods are.
    #[test]
    fn nothing_but_a_capital_get_is_routed() {
        for method in ["get", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", ""] {
            assert_eq!(route(method, "/api/data"), Route::NotFound, "{method}");
        }
    }

    #[test]
    fn a_request_line_yields_its_method_and_target() {
        assert_eq!(
            parse_request_line("GET /api/data HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some(("GET", "/api/data")),
        );
        for malformed in ["", "GET", "GET\r\n", " /api/data HTTP/1.1\r\n"] {
            assert_eq!(parse_request_line(malformed), None, "{malformed:?}");
        }
    }

    /// Hands out one chunk per `read` and fails afterwards, so a test can prove the head read stops
    /// at the blank line instead of waiting for a peer that will send nothing more.
    struct ChunkedReader {
        chunks: Vec<&'static [u8]>,
        next: usize,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let chunk = self.chunks.get(self.next).ok_or_else(|| {
                io::Error::other("the head read went past the blank line that ends it")
            })?;
            self.next += 1;
            buffer[..chunk.len()].copy_from_slice(chunk);
            Ok(chunk.len())
        }
    }

    #[test]
    fn the_head_read_stops_at_the_blank_line() {
        let mut reader = ChunkedReader {
            chunks: vec![b"GET /api/data HTTP/1.1\r\nHost: x\r\n\r\n"],
            next: 0,
        };
        let head = read_head(&mut reader).expect("the head ends inside the first chunk");
        assert!(head.ends_with(b"\r\n\r\n"));
    }

    /// The terminator may straddle a chunk boundary, which is why the search runs over the whole
    /// accumulated head rather than over the chunk just read.
    #[test]
    fn a_terminator_split_across_reads_is_still_found() {
        let mut reader = ChunkedReader {
            chunks: vec![b"GET /ap", b"i/events HTTP/1.1\r\nHost: x\r\n", b"\r\n"],
            next: 0,
        };
        let head = read_head(&mut reader).expect("the head is reassembled across chunks");
        let head = String::from_utf8(head).unwrap();
        assert_eq!(parse_request_line(&head), Some(("GET", "/api/events")));
    }

    /// A peer that never sends a blank line is bounded rather than allowed to allocate.
    #[test]
    fn a_head_that_never_ends_is_bounded() {
        let flood = vec![b'a'; 4 * MAX_REQUEST_BYTES];
        let head = read_head(&mut &flood[..]).expect("the read is bounded, not failed");
        assert!(head.len() >= MAX_REQUEST_BYTES);
        assert!(head.len() < MAX_REQUEST_BYTES + 1024);
    }

    #[test]
    fn responses_are_framed_closed_and_uncacheable() {
        let mut written = Vec::new();
        write_response(
            &mut written,
            "200 OK",
            "application/json",
            br#"{"ok":true}"#,
        )
        .expect("a vector always takes a write");
        let written = String::from_utf8(written).unwrap();
        assert!(written.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(written.contains("Content-Type: application/json\r\n"));
        assert!(written.contains("Content-Length: 11\r\n"));
        assert!(written.contains("Cache-Control: no-store\r\n"));
        assert!(written.contains("Connection: close\r\n"));
        assert!(written.ends_with("\r\n\r\n{\"ok\":true}"));
    }

    /// The framing an event stream lives by: a named event, one data line, a blank line to end it.
    #[test]
    fn an_event_is_framed_with_its_name_and_a_blank_line() {
        let framed = sse_event("refresh", r#"{"generation":3}"#);
        assert_eq!(framed, "event: refresh\ndata: {\"generation\":3}\n\n");
        assert!(
            framed.ends_with("\n\n"),
            "an unterminated event never arrives"
        );
        // Two fields and the blank line that dispatches them: an event carrying a third field, or
        // one whose data broke across two lines, would be a different message on the wire.
        assert_eq!(
            framed.split('\n').collect::<Vec<_>>(),
            vec!["event: refresh", r#"data: {"generation":3}"#, "", ""],
        );
    }

    /// The notice is the one thing the stream sends, so it has to be a single line of JSON: a
    /// newline in it would silently split one event into two.
    #[test]
    fn a_refresh_notice_is_one_line_of_parseable_json() {
        let served = Served {
            evidence: EvidenceIndex::default(),
            ask: empty_corpus(),
            generation: 7,
            fold_generation: 7,
            refreshed_at: DateTime::parse_from_rfc3339("2026-08-24T09:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            stale_since: None,
            body: Vec::new(),
        };
        let notice = served.notice();
        assert!(!notice.contains('\n'), "{notice}");
        let parsed: serde_json::Value = serde_json::from_str(&notice).expect("valid JSON");
        assert_eq!(parsed["generation"], 7);
        assert_eq!(parsed["refreshed_at"], "2026-08-24T09:30:00Z");
        assert_eq!(
            sse_event("refresh", &notice),
            format!("event: refresh\ndata: {notice}\n\n"),
            "one notice is one event",
        );
    }

    /// The access log is a rendering surface — the operator's terminal — and this lane exists to be
    /// `--bind`-exposed to an unauthenticated tailnet, so a peer must not choose bytes on it. A
    /// request line carrying an ESC, a BEL, and a newline produces a log line carrying none of the
    /// three, and stays one line.
    #[test]
    fn a_request_line_cannot_put_control_bytes_on_the_operators_terminal() {
        let hostile = "GET /\u{1b}]0;pwned\u{7}/fake\nSPOOFED-LOG-LINE HTTP/1.1\r\nHost: x\r\n\r\n";
        let (method, target) = parse_request_line(hostile).expect("a request line, of a sort");
        let request = logged_request(method, target);

        for byte in ['\u{1b}', '\u{7}', '\n', '\r'] {
            assert!(!request.contains(byte), "{byte:?} survived: {request:?}");
        }
        assert!(
            !request.chars().any(char::is_control),
            "no control character at all: {request:?}",
        );
        assert_eq!(
            request.lines().count(),
            1,
            "one request, one line: {request:?}"
        );
        // The bytes are escaped rather than dropped: an access log that hid the strange request
        // would be useless at the only moment anybody reads it.
        assert!(
            request.starts_with("GET /\\u{1b}]0;pwned\\u{7}/fake\\n"),
            "{request:?}"
        );
        assert!(request.contains("SPOOFED-LOG-LINE"), "{request:?}");

        // Routing still reads the real bytes, so the clamp changes what is *printed* and nothing
        // about what is served.
        assert_eq!(route(method, target), Route::NotFound);

        // And an ordinary request is logged exactly as it arrived.
        let (method, target) =
            parse_request_line("GET /api/data?x=1 HTTP/1.1\r\n\r\n").expect("a request line");
        assert_eq!(logged_request(method, target), "GET /api/data?x=1");
    }

    /// A routable bind is allowed and *named*: the tailnet case is the point of the lane, so the
    /// honesty is in the sentence rather than in a refusal.
    #[test]
    fn a_non_loopback_bind_states_its_posture() {
        let redactor = Redactor::new();
        for routable in ["0.0.0.0:8878", "100.64.0.7:8878", "[::]:8878"] {
            let line = posture_line(routable.parse().expect("a socket address"), &redactor);
            assert!(line.contains("NOT loopback"), "{line}");
            assert!(line.contains("UNAUTHENTICATED"), "{line}");
            assert!(line.contains("tailnet"), "{line}");
            // The claim changed with the slice and the sentence had to change with it: this page
            // now serves excerpts, so "never transcript text" would be false. What is still true —
            // and is what a reader needs — is that they are redacted, bounded to the events a rule
            // counted, and that there is no route into the archive.
            assert!(line.contains("redacted evidence excerpts"), "{line}");
            assert!(line.contains("never a whole transcript"), "{line}");
            assert!(line.contains("never a link into the archive"), "{line}");
            assert!(!line.contains('\n'), "the posture is one line: {line}");
        }
        for local in ["127.0.0.1:8878", "127.0.0.53:1", "[::1]:8878"] {
            let line = posture_line(local.parse().expect("a socket address"), &redactor);
            assert!(line.contains("is loopback"), "{line}");
            assert!(line.contains("only this machine"), "{line}");
            assert!(!line.contains('\n'), "the posture is one line: {line}");
        }
    }

    /// `--no-redact` is allowed — it is a documented choice the redaction lane already blessed —
    /// and on a routable address it is that choice made on behalf of every device on the tailnet,
    /// so it gets a second line and that line shouts.
    #[test]
    fn turning_redaction_off_says_so_and_says_it_loudest_where_it_costs_most() {
        let raw = Redactor::new().with_secrets(false);
        let routable: SocketAddr = "100.64.0.7:8878".parse().expect("a socket address");
        let local: SocketAddr = "127.0.0.1:8878".parse().expect("a socket address");

        // The default posture prints one line and nothing else.
        assert_eq!(redaction_posture_line(routable, &Redactor::new()), None);
        assert_eq!(redaction_posture_line(local, &Redactor::new()), None);

        // With the scrub off, the first line stops calling the excerpts redacted...
        let posture = posture_line(routable, &raw);
        assert!(
            posture.contains("UNREDACTED evidence excerpts"),
            "{posture}"
        );

        // ...and the second one names the cost in the loudest terms the lane has.
        let loud = redaction_posture_line(routable, &raw).expect("a second line");
        assert!(loud.contains("NON-LOOPBACK"), "{loud}");
        assert!(loud.contains("UNREDACTED"), "{loud}");
        assert!(loud.contains("credentials"), "{loud}");
        assert!(!loud.contains('\n'), "the posture is one line: {loud}");

        let quiet = redaction_posture_line(local, &raw).expect("a second line");
        assert!(quiet.contains("RAW"), "{quiet}");
        assert!(quiet.contains("loopback"), "{quiet}");
        assert!(!quiet.contains("NON-LOOPBACK"), "{quiet}");
    }

    /// A guard that the embedded asset is the page this server thinks it is serving, and that the
    /// page has no way to reach anything but this server's own three routes.
    #[test]
    fn the_embedded_page_talks_only_to_this_servers_routes() {
        assert!(PAGE.contains("/api/data"), "the page fetches the payload");
        assert!(
            PAGE.contains("/api/events"),
            "the page subscribes to refreshes"
        );
        assert!(PAGE.contains("/api/ask?q="), "the page can search");
        // The three sections the payload feeds, each fed from its own key rather than from a
        // second fetch: one document, one generation, and no route added to serve a section.
        for section in ["data.cost", "data.standup", "data.lanes", "data.findings"] {
            assert!(PAGE.contains(section), "the page renders {section}");
        }
        assert_eq!(
            PAGE.matches("fetch(").count(),
            3,
            "the payload, one excerpt at a time, and one search — and nothing else",
        );
        assert!(
            !PAGE.contains("href"),
            "the page carries no links at all — least of all into Patwari, which serves \
             unredacted blobs",
        );
        assert!(!PAGE.contains("<a "), "the page carries no anchors");
        assert!(
            !PAGE.contains("innerHTML"),
            "every value from the payload is set as text, never parsed as markup",
        );
        assert!(
            !PAGE.contains("//fonts.") && !PAGE.contains("http://") && !PAGE.contains("https://"),
            "the page loads nothing from anywhere",
        );
    }

    /// A published payload replaces the previous one whole, and a stream waiting on the old
    /// generation is woken with the new one.
    #[test]
    fn publishing_swaps_the_payload_and_wakes_the_waiters() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let service = Arc::new(Service::new(
            Served {
                generation: 1,
                fold_generation: 1,
                refreshed_at: Utc::now(),
                stale_since: None,
                body: br#"{"generation":1,"stale_since":null}"#.to_vec(),
                evidence: EvidenceIndex::default(),
                ask: empty_corpus(),
            },
            BlobCache::open(scratch.path()).expect("a scratch cache"),
            Redactor::new(),
        ));
        assert_eq!(service.snapshot().generation, 1);
        // Already past the generation being waited on: no wait at all.
        assert_eq!(
            service
                .wait_for_change(0, Duration::from_secs(30))
                .map(|served| served.generation),
            Some(1),
        );
        // Nothing published: the wait times out rather than reporting a change that did not happen.
        assert!(
            service
                .wait_for_change(1, Duration::from_millis(20))
                .is_none()
        );

        let waiting = Arc::clone(&service);
        let waiter = thread::spawn(move || {
            waiting
                .wait_for_change(1, Duration::from_secs(30))
                .map(|served| served.generation)
        });
        // Give the waiter a moment to park before the swap, so the wake path is what is exercised.
        thread::sleep(Duration::from_millis(50));
        service.publish(Served {
            generation: 2,
            fold_generation: 2,
            refreshed_at: Utc::now(),
            stale_since: None,
            body: br#"{"generation":2,"stale_since":null}"#.to_vec(),
            evidence: EvidenceIndex::default(),
            ask: empty_corpus(),
        });
        assert_eq!(waiter.join().expect("the waiter did not panic"), Some(2));
        assert_eq!(service.snapshot().generation, 2);
    }

    /// A failed refresh keeps the numbers and dates them. The page is never blanked and never
    /// pretends the fold it is showing is current.
    #[test]
    fn a_failed_refresh_restamps_the_payload_rather_than_emptying_it() {
        let taken_at = DateTime::parse_from_rfc3339("2026-08-24T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let service = Service::new(
            Served {
                generation: 4,
                fold_generation: 4,
                refreshed_at: taken_at,
                stale_since: None,
                body:
                    br#"{"provenance":{"generation":4,"sessions_folded":703,"stale_since":null}}"#
                        .to_vec(),
                evidence: EvidenceIndex::default(),
                ask: empty_corpus(),
            },
            BlobCache::open(scratch.path()).expect("a scratch cache"),
            Redactor::new(),
        );
        let since = DateTime::parse_from_rfc3339("2026-08-24T09:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        republish_as_stale(&service, 5, since);

        let served = service.snapshot();
        assert_eq!(
            served.generation, 5,
            "a page's numbers going stale is a change"
        );
        assert_eq!(
            served.fold_generation, 4,
            "the fold behind it did not move, and nothing that reads it may say otherwise",
        );
        assert_eq!(
            served.refreshed_at, taken_at,
            "the payload is still the fold it was, taken when it was",
        );
        let document: serde_json::Value = serde_json::from_slice(&served.body).unwrap();
        assert_eq!(
            document["provenance"]["stale_since"],
            "2026-08-24T09:05:00Z"
        );
        assert_eq!(
            document["provenance"]["generation"], 4,
            "the document still names the fold it is showing, never a refresh that never happened",
        );
        assert_eq!(
            document["provenance"]["sessions_folded"], 703,
            "the numbers survive the restamp",
        );
    }

    /// The property the two generation fields exist for: **a search and the document beside it name
    /// one fold**, through a failing run.
    ///
    /// The page decides whether an answer it is still showing predates the corpus in hand by
    /// comparing the answer's `corpus.generation` against the payload's `provenance.generation`. A
    /// stale re-stamp bumps the *publication* and re-reads nothing, so an answer stamped with the
    /// publication would sit a generation ahead of the payload and the page would tell a reader to
    /// search again for a corpus that had not moved — a wrong instruction, and inverted.
    ///
    /// This is what [`a_stale_corpus_dates_its_answer_rather_than_refusing_it`] in
    /// [`crate::dashboard`] could not see: it builds one answer from one [`Refreshed`] and never
    /// meets the re-stamp that makes the two numbers differ.
    #[test]
    fn a_stale_republish_keeps_an_answer_and_the_payload_on_one_generation() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let service = Service::new(
            Served {
                generation: 6,
                fold_generation: 6,
                refreshed_at: DateTime::parse_from_rfc3339("2026-08-24T09:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                stale_since: None,
                body: br#"{"provenance":{"generation":6,"stale_since":null}}"#.to_vec(),
                evidence: EvidenceIndex::default(),
                ask: empty_corpus(),
            },
            BlobCache::open(scratch.path()).expect("a scratch cache"),
            Redactor::new(),
        );
        // Healthy: the publication, the document, and an answer all agree.
        let stated = |service: &Service| {
            let document: serde_json::Value =
                serde_json::from_slice(&service.snapshot().body).expect("the body is JSON");
            let answered = ask(service, "/api/ask?q=redaction").1;
            (
                document["provenance"]["generation"].clone(),
                answered["corpus"]["generation"].clone(),
            )
        };
        let (document, answer) = stated(&service);
        assert_eq!(document, 6);
        assert_eq!(answer, document);

        // Two failed refreshes later the publication is 8 and the fold is still 6 — and the answer
        // has to say 6, because 6 is the fold it ranked.
        let since = DateTime::parse_from_rfc3339("2026-08-24T09:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        republish_as_stale(&service, 7, since);
        republish_as_stale(&service, 8, since);
        assert_eq!(service.snapshot().generation, 8, "the swap is pushed");

        let (document, answer) = stated(&service);
        assert_eq!(document, 6, "the document still names its own fold");
        assert_eq!(
            answer, document,
            "the answer and the payload must never sit a generation apart",
        );
        // And the answer says how old the corpus is, so the staleness is stated rather than hidden
        // behind two numbers that happen to match.
        assert_eq!(
            ask(&service, "/api/ask?q=redaction").1["corpus"]["stale_since"],
            "2026-08-24T09:05:00Z",
        );
    }
}
