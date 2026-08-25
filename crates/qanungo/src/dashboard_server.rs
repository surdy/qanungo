//! The dashboard's HTTP surface: a blocking HTTP/1.1 server over `std::net::TcpListener`, one
//! thread per connection, no async runtime and no framework.
//!
//! It is the server counterpart of this crate's minimal HTTP client (munshi ADR 0006) and mirrors
//! munshi-dashboard's shape deliberately: one request per connection, `Connection: close`, an
//! explicit `Content-Length`, no keep-alive bookkeeping, an embedded single-file page, and no state
//! of its own on disk. Three routes exist — the page, the JSON snapshot, and the event stream —
//! and everything else is 404.
//!
//! # Where it differs from munshi-dashboard, and why
//!
//! - **It folds its own numbers instead of shelling out.** munshi-dashboard invokes `munshi
//!   ... --json` per panel. There is no `qanungo report --json`, and inventing one so a dashboard
//!   could parse it back would be two serializations and a subprocess where a function call does.
//!   It calls [`command::fold_coaching`] directly — the same call `qanungo report` makes.
//! - **It refreshes in the background.** A fold of thirty days is 17 s of sync plus 5 s of fold
//!   against the production archive; on the request path that is not a dashboard, it is a wait. So
//!   the fold happens on a timer, the payload is serialized once per refresh, and a request is a
//!   memcpy. This is the "in-memory service" half of the 2026-08-24 grilling: process memory is the
//!   disposable materialization, and the persistent event store stays deferred.
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
//! radius is not access control but *what the payload contains*: lane scores, rule ids, counts,
//! and content hashes, with no transcript text and no link into Patwari, which serves unredacted
//! blobs. See [`crate::dashboard`] for how that line is held.
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
use thiserror::Error;

use crate::cli::{ArchiveArgs, DashboardArgs, Refresh, Window};
use crate::command::{self, CommandError, Folded};
use crate::dashboard::{self, Refreshed};
use crate::format;

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
    window: Window,
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
        eprintln!("qanungo dashboard: listening on http://{address}");
        eprintln!("qanungo dashboard: {}", posture_line(address));

        let service = Arc::new(Service::new(fold_and_publish(
            &args.archive,
            &args.last,
            &args.refresh,
            Refreshed {
                generation: 1,
                at: Utc::now(),
                stale_since: None,
            },
        )?));
        Ok(Self {
            listener,
            service,
            archive: args.archive.clone(),
            window: args.last.clone(),
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
        let window = self.window.clone();
        let refresh = self.refresh.clone();
        if let Err(error) = thread::Builder::new()
            .name("dashboard-refresh".to_owned())
            .spawn(move || refresh_loop(&refreshing, &archive, &window, &refresh))
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
pub fn posture_line(address: SocketAddr) -> String {
    if address.ip().is_loopback() {
        format!(
            "{address} is loopback — only this machine can reach the page; nothing here \
             authenticates a caller",
        )
    } else {
        format!(
            "{address} is NOT loopback — this page is UNAUTHENTICATED and the tailnet is the only \
             boundary in front of it; it serves scores, rule ids, counts and content hashes, never \
             transcript text and never a link into the archive",
        )
    }
}

/// The payload every request is answered from.
struct Served {
    /// Bumped on every swap. An event stream compares it to know a refresh from a reconnect.
    generation: u64,
    refreshed_at: DateTime<Utc>,
    /// Serialized once per refresh rather than once per request, so open tabs cost nothing.
    body: Vec<u8>,
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
}

impl Service {
    fn new(served: Served) -> Self {
        Self {
            served: Mutex::new(Arc::new(served)),
            changed: Condvar::new(),
            streams: AtomicUsize::new(0),
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

/// Folds the window pair once and serializes the payload, narrating both to stderr.
///
/// Every line goes to stderr, including the access log below: this lane writes no document to
/// stdout, and keeping the whole narration on one stream means `qanungo dashboard >/dev/null`
/// cannot silently swallow the posture statement.
fn fold_and_publish(
    archive: &ArchiveArgs,
    window: &Window,
    refresh: &Refresh,
    refreshed: Refreshed,
) -> Result<Served, CommandError> {
    eprintln!(
        "qanungo dashboard: folding the last {window} and the window before it from {} — a warm \
         run takes about 25 s, a cold one about a minute",
        archive.patwari_url,
    );
    let started = Instant::now();
    let folded = command::fold_coaching(archive, window)?;
    let body = serde_json::to_vec(&dashboard::payload(window, refresh, &folded, refreshed))
        .unwrap_or_else(|_| b"{}".to_vec());
    eprintln!(
        "qanungo dashboard: refresh {} — {} · payload {} · {}",
        refreshed.generation,
        instrumentation_line(&folded),
        format::bytes(body.len() as u64),
        format::elapsed(started.elapsed()),
    );
    Ok(Served {
        generation: refreshed.generation,
        refreshed_at: refreshed.at,
        body,
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
         ({} transferred) · rule pack {}",
        format::elapsed(instrumentation.sync.elapsed),
        format::elapsed(instrumentation.fold_elapsed),
        instrumentation.sessions_folded,
        instrumentation.comparison_sessions_folded,
        format::bytes(instrumentation.bytes_folded),
        instrumentation.sync.cache_hits,
        instrumentation.sync.cache_misses,
        format::bytes(instrumentation.sync.bytes_transferred),
        instrumentation.rule_pack.stamp(),
    )
}

/// Re-syncs and re-folds on the interval, forever.
///
/// A failed refresh keeps the last good payload and republishes it with `stale_since` set, rather
/// than blanking the page or serving nothing: the numbers are still true of the window they were
/// taken over, and the honest correction is to date them, not to hide them. The republish bumps the
/// generation on purpose — a page's numbers becoming stale is a change worth pushing.
fn refresh_loop(service: &Service, archive: &ArchiveArgs, window: &Window, refresh: &Refresh) {
    let mut generation = 1;
    let mut stale_since: Option<DateTime<Utc>> = None;
    loop {
        thread::sleep(refresh.interval());
        generation += 1;
        let at = Utc::now();
        match fold_and_publish(
            archive,
            window,
            refresh,
            Refreshed {
                generation,
                at,
                stale_since: None,
            },
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
        refreshed_at: current.refreshed_at,
        body,
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

/// Which of the three routes a request is for.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// The embedded page.
    Page,
    /// The JSON payload.
    Data,
    /// The refresh event stream.
    Events,
    NotFound,
}

/// Routes a request. `GET` only, case-sensitively, and the query string decides nothing.
///
/// There is no 405: a non-`GET` request to this surface is not a method mismatch worth negotiating,
/// it is a caller who has the wrong server. Nothing is read from the filesystem, so path traversal
/// has nothing to traverse to — an unmatched target is simply not a route.
fn route(method: &str, target: &str) -> Route {
    if method != "GET" {
        return Route::NotFound;
    }
    match target.split('?').next().unwrap_or(target) {
        "/" | "/index.html" => Route::Page,
        "/api/data" => Route::Data,
        "/api/events" => Route::Events,
        _ => Route::NotFound,
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

    #[test]
    fn the_three_routes_are_the_only_routes() {
        assert_eq!(route("GET", "/"), Route::Page);
        assert_eq!(route("GET", "/index.html"), Route::Page);
        assert_eq!(route("GET", "/api/data"), Route::Data);
        assert_eq!(route("GET", "/api/events"), Route::Events);
        for target in [
            "/api",
            "/api/data/",
            "/favicon.ico",
            "/assets/dashboard.html",
            "/../../etc/passwd",
            "/api/v1/artifacts/abc/content",
        ] {
            assert_eq!(route("GET", target), Route::NotFound, "{target}");
        }
    }

    /// A query string is a caller's business and never the server's: `/api/data?since=x` is the
    /// same route as `/api/data`, and no route reads a parameter at all — a per-request knob on a
    /// surface with a redaction posture is exactly what the grilling ruled out.
    #[test]
    fn query_strings_do_not_change_the_route() {
        assert_eq!(route("GET", "/api/data?cachebust=1"), Route::Data);
        assert_eq!(route("GET", "/?x=y"), Route::Page);
        assert_eq!(route("GET", "/api/events?last=99"), Route::Events);
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
            generation: 7,
            refreshed_at: DateTime::parse_from_rfc3339("2026-08-24T09:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
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
        for routable in ["0.0.0.0:8878", "100.64.0.7:8878", "[::]:8878"] {
            let line = posture_line(routable.parse().expect("a socket address"));
            assert!(line.contains("NOT loopback"), "{line}");
            assert!(line.contains("UNAUTHENTICATED"), "{line}");
            assert!(line.contains("tailnet"), "{line}");
            assert!(line.contains("never transcript text"), "{line}");
            assert!(!line.contains('\n'), "the posture is one line: {line}");
        }
        for local in ["127.0.0.1:8878", "127.0.0.53:1", "[::1]:8878"] {
            let line = posture_line(local.parse().expect("a socket address"));
            assert!(line.contains("is loopback"), "{line}");
            assert!(line.contains("only this machine"), "{line}");
            assert!(!line.contains('\n'), "the posture is one line: {line}");
        }
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
        let service = Arc::new(Service::new(Served {
            generation: 1,
            refreshed_at: Utc::now(),
            body: br#"{"generation":1,"stale_since":null}"#.to_vec(),
        }));
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
            refreshed_at: Utc::now(),
            body: br#"{"generation":2,"stale_since":null}"#.to_vec(),
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
        let service = Service::new(Served {
            generation: 4,
            refreshed_at: taken_at,
            body: br#"{"provenance":{"sessions_folded":703,"stale_since":null}}"#.to_vec(),
        });
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
            served.refreshed_at, taken_at,
            "the payload is still the fold it was, taken when it was",
        );
        let document: serde_json::Value = serde_json::from_slice(&served.body).unwrap();
        assert_eq!(
            document["provenance"]["stale_since"],
            "2026-08-24T09:05:00Z"
        );
        assert_eq!(
            document["provenance"]["sessions_folded"], 703,
            "the numbers survive the restamp",
        );
    }
}
