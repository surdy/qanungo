//! Pipeline health: which devices are reporting, what is landing, what is missing, and how much
//! is in the archive.
//!
//! The one question no surface in the suite answered. `munshi status` speaks for one machine,
//! Patwari's `/readyz` says it is alive rather than what it holds, and qanungo's provenance footer
//! describes the last fold. "Are both my machines still reporting, and is anything getting stuck"
//! was answered until now by hand-run censuses. This module folds that answer once per refresh,
//! beside the four lanes, from what those folds already read plus two integer routes on the
//! archive.
//!
//! # The redaction line, and why this section cannot cross it
//!
//! Everything here is **counts, timestamps, byte sums, device labels and harness names**. There is
//! no transcript text, no summary text, no repository name, and no path — not because this module
//! filters them out but because it is not handed any: it reads [`SessionMetrics`]'s already-folded
//! per-session facts, [`SyncStats`]'s integers, [`CacheUsage`]'s two integers, and an archive
//! inventory whose whole contract is integers and instants. The three strings that *are* archive-
//! written — a device label, a harness label, a client's hostname — go through the same clamp-then-
//! scrub every other rendered identifier on this payload goes through, and a device with no label
//! renders as the device scope's own [`NO_DEVICE`] sentence rather than as anything that could be a
//! path. `tests/dashboard.rs` walks the built section against the planted-canary fixtures.
//!
//! # It computes nothing the page could not check
//!
//! The device rows are [`crate::scopes::by_device`] — the very same grouping the device scope
//! control is built from, so the panel's rows and the control's options cannot disagree about what
//! a device is or how many sessions it holds. The landing calendar is the same UTC archive day
//! [`crate::timeline`] places a session on, so its per-day counts sum to the same window total the
//! timeline's do. Nothing is averaged, rated, or scored.
//!
//! # Two clocks, and only one of them is here
//!
//! Landing is on **archive time** — the UTC calendar day Patwari finished the session's snapshot —
//! which is the clock decision 11 blessed for the timeline and the clock the window itself is cut
//! on. It is deliberately not the transcript's own local instant: that is the heatmap's clock, and
//! a health panel asking "did anything land yesterday" wants the archive's answer, not the
//! operator's.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};

use crate::cache::CacheUsage;
use crate::command::Folded;
use crate::evidence;
use crate::format;
use crate::metrics::SessionMetrics;
use crate::patwari::{ArchiveClient, ArchiveStats};
use crate::redaction::Redactor;
use crate::report::stamp;
use crate::scopes::{self, NO_DEVICE};
use crate::sync::SyncStats;

/// How long a device may go without a capture before the panel says so, in whole days.
///
/// Two days rather than one because a person takes a weekend, moves machines, or simply does not
/// open a terminal on a Tuesday, and a panel that cried about every quiet day would be ignored by
/// the end of the first week. It is a *flag*, not a threshold anything scores: the row carries the
/// exact last-capture time beside it, and a reader who knows they were on holiday can read past it.
pub const SILENT_AFTER_DAYS: i64 = 2;

/// What only the process running the refresh can know: the archive's own inventory, and what this
/// mirror is holding on this disk.
///
/// A struct handed to the payload rather than fetched inside it, on the same reasoning every other
/// section of that payload is built from somebody else's output: the fold is not the place to open
/// a socket, and a section that fetched during serialization could fail a refresh that had already
/// succeeded.
#[derive(Debug, Clone)]
pub struct Fleet {
    /// The archive's answer, or why there is not one. See [`Inventory`].
    pub archive: Inventory,
    /// This mirror's own footprint.
    pub mirror: Mirror,
}

/// The archive's inventory, or the reason the panel is showing none.
///
/// Three states rather than an `Option`, because "this Patwari is older than the route" and "this
/// Patwari would not answer" are different facts about a fleet and a health panel that showed one
/// under the other's name would be misleading in exactly the place it is meant to be useful.
#[derive(Debug, Clone)]
pub enum Inventory {
    /// The archive answered both routes.
    Read {
        stats: ArchiveStats,
        clients: Vec<ArchiveClient>,
    },
    /// The archive has no `/api/v1/stats` — a Patwari older than the inventory routes. Everything
    /// else on the page is unaffected, which is the whole point of tolerating it.
    Unsupported,
    /// The archive was asked and something went wrong. The refresh still published: the four lanes
    /// had already succeeded, and blanking a page of good numbers over a failed inventory would be
    /// the panel taking the dashboard down.
    Failed(String),
}

/// What the mirror is costing this machine, and what the last refresh's coaching sync did.
#[derive(Debug, Clone)]
pub struct Mirror {
    pub cache_root: PathBuf,
    pub usage: CacheUsage,
}

/// One device seen in the coaching window.
#[derive(Debug, Clone)]
pub struct Device {
    /// The device scope's own label for this host — clamped, scrubbed, and [`NO_DEVICE`] when the
    /// archive named none.
    pub label: String,
    /// Whether the archive named a device at all.
    pub attributed: bool,
    pub sessions: usize,
    pub by_harness: BTreeMap<String, usize>,
    /// The newest archive completion time among this device's sessions in the window. `None` when
    /// no session of this device carried a time this build could parse — the timeline's `undated`
    /// state, surfaced rather than guessed at.
    pub last_capture: Option<DateTime<Utc>>,
}

impl Device {
    /// Whole days between this device's last capture and the fold — `None` when there is no last
    /// capture to count from, and never negative for a clock that ran backwards.
    fn silent_days(&self, generated_at: DateTime<Utc>) -> Option<i64> {
        let last = self.last_capture?;
        Some((generated_at - last).num_days().max(0))
    }

    /// The harness mix as one line: `claude-code 12 · copilot-cli 3`, busiest first.
    ///
    /// Rendered here rather than in the page because it is a *label made of the payload's own
    /// numbers*, and the page's contract is that it computes nothing. A reader can check every part
    /// of it against `by_harness` beside it.
    fn harness_mix(&self) -> String {
        let mut mix: Vec<(&String, &usize)> = self.by_harness.iter().collect();
        mix.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
        mix.iter()
            .map(|(harness, count)| format!("{harness} {count}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// One UTC archive day of the window: how many sessions landed, and from which devices.
#[derive(Debug, Clone)]
pub struct LandingDay {
    pub date: NaiveDate,
    pub sessions: usize,
    /// Positional against the device list this landing was folded with — the same axis, in the same
    /// order, so the page stacks a column by index rather than by looking a label up.
    pub by_device: Vec<usize>,
}

/// The window's sessions on the archive's calendar, split by device.
#[derive(Debug, Clone, Default)]
pub struct Landing {
    /// Days a session actually landed on, earliest first. Sparse over the calendar: a day nothing
    /// landed on is absent, because a window's length must not decide what a section costs to
    /// serve.
    pub days: Vec<LandingDay>,
    /// Sessions the archive stated a time for that this build could not parse. They are on no day
    /// and are counted, never guessed onto one.
    pub undated: usize,
}

impl Landing {
    /// Sessions placed on a day. With [`Landing::undated`] this is the window's own folded count,
    /// which is what makes the chart checkable against the sentence above it.
    pub fn sessions(&self) -> usize {
        self.days.iter().map(|day| day.sessions).sum()
    }
}

/// Every device the coaching window holds, in the device scope's own order.
///
/// [`crate::scopes::by_device`] rather than a second grouping: attributed hosts before the residue,
/// then busiest, then the label, with the unattributed bucket last. The panel's rows are therefore
/// the scope control's options in the scope control's order, and "highlight the row I scoped to" is
/// a label match rather than an alignment anybody has to maintain.
pub fn devices(folded: &Folded, redactor: &Redactor) -> Vec<Device> {
    scopes::by_device(folded, redactor, [])
        .into_iter()
        .map(|scope| Device {
            attributed: scope.label != NO_DEVICE,
            sessions: scope.sessions.len(),
            by_harness: scope.by_harness(redactor),
            last_capture: scope
                .sessions
                .iter()
                .filter_map(|session| session.archived_at)
                .max(),
            label: scope.label,
        })
        .collect()
}

/// The window laid on the archive's own calendar, split by the same device axis.
///
/// A session's day is [`SessionMetrics::archive_day`] — the UTC calendar day Patwari finished its
/// snapshot, the same call [`crate::timeline`] makes — so this view and the timeline above it place
/// every session on the same date, and neither can drift into the other's clock.
pub fn landing(sessions: &[SessionMetrics], devices: &[Device], redactor: &Redactor) -> Landing {
    let slot: BTreeMap<&str, usize> = devices
        .iter()
        .enumerate()
        .map(|(index, device)| (device.label.as_str(), index))
        .collect();
    let mut days: BTreeMap<NaiveDate, Vec<usize>> = BTreeMap::new();
    let mut undated = 0;
    for session in sessions {
        let Some(date) = session.archive_day() else {
            undated += 1;
            continue;
        };
        let label = scopes::device_label(session.hostname.as_deref(), redactor);
        let counts = days.entry(date).or_insert_with(|| vec![0; devices.len()]);
        // A label with no slot cannot happen — the devices were folded from these same sessions by
        // these same two calls — and if it ever did, the honest answer is to count the session on
        // its day and not to invent a column for it.
        if let Some(index) = slot.get(label.as_str()) {
            counts[*index] += 1;
        }
    }
    Landing {
        days: days
            .into_iter()
            .map(|(date, by_device)| LandingDay {
                date,
                sessions: by_device.iter().sum(),
                by_device,
            })
            .collect(),
        undated,
    }
}

/// The whole section, as the page reads it.
///
/// `sync` is the **coaching** lane's mirror statistics, because the transcript is the artifact the
/// munshi #78 class is about and the coaching window is the one this panel's other two views are
/// taken over. Reporting a different lane's sync beside these devices would be three numbers about
/// three windows under one heading.
pub fn section(folded: &Folded, fleet: &Fleet, redactor: &Redactor) -> Value {
    let devices = devices(folded, redactor);
    let landing = landing(&folded.sessions, &devices, redactor);
    let generated_at = folded.generated_at;
    let sync = &folded.instrumentation.sync;
    json!({
        "silent_after_days": SILENT_AFTER_DAYS,
        "devices": devices
            .iter()
            .map(|device| device_value(device, generated_at))
            .collect::<Vec<_>>(),
        "landing": landing_value(&landing),
        "gaps": gaps_value(sync, folded),
        "archive": archive_value(&fleet.archive, redactor),
        "mirror": mirror_value(&fleet.mirror, folded),
    })
}

/// One device row: what it did in the window, when it was last heard from, and whether that is long
/// enough ago to say so.
///
/// `silent` is served as its own boolean rather than left to the page to derive from the day count
/// and the constant, so the rule that decides it lives in exactly one place — here, beside the
/// constant it reads.
fn device_value(device: &Device, generated_at: DateTime<Utc>) -> Value {
    let silent_days = device.silent_days(generated_at);
    json!({
        "device": device.label,
        "attributed": device.attributed,
        "sessions": device.sessions,
        "by_harness": device.by_harness,
        "harness_mix": device.harness_mix(),
        "last_capture_at": device.last_capture.map(stamp),
        "silent_days": silent_days,
        "silent": silent_days.is_some_and(|days| days >= SILENT_AFTER_DAYS),
    })
}

/// The landing calendar: dates and integers, and no string at all.
///
/// The per-day arrays are positional against the `devices` list above them, on exactly the rule the
/// timeline's are positional against `scopes.harnesses`: one label, in one place, so a chart and a
/// table cannot spell a device two ways.
fn landing_value(landing: &Landing) -> Value {
    json!({
        // Named on the wire, as every other basis on this payload is, so a reader of the raw
        // document does not have to infer the clock from a module comment they cannot see.
        "basis": "archive-completion-utc",
        "days": landing
            .days
            .iter()
            .map(|day| json!({
                "date": day.date.to_string(),
                "sessions": day.sessions,
                "by_device": day.by_device,
            }))
            .collect::<Vec<_>>(),
        "days_covered": landing.days.len(),
        "sessions": landing.sessions(),
        "undated": landing.undated,
    })
}

/// What did not arrive whole, and what the mirror got back anyway.
///
/// The munshi #78 class in three numbers: how many sessions' newest snapshot could not answer for a
/// transcript, how many of those a sibling snapshot did answer for, and the remainder that stayed
/// unread. The remainder is served rather than left to be subtracted, because it is the number a
/// reader is actually looking for and the one they would get wrong.
///
/// `notes` is the report's own Gaps section, the same list the provenance footer carries. It is
/// repeated here rather than referenced: the panel is where somebody goes to find out what is
/// stuck, and a health section that made them read the footer for the reason would not be one.
fn gaps_value(sync: &SyncStats, folded: &Folded) -> Value {
    json!({
        "projection_unusable": sync.projection_unusable,
        "recovered_from_sibling": sync.recovered_from_sibling,
        "unrecovered": sync
            .projection_unusable
            .saturating_sub(sync.recovered_from_sibling),
        "sessions_listed": sync.sessions_listed,
        "sessions_folded": folded.instrumentation.sessions_folded,
        "notes": crate::dashboard::gaps_value(&folded.skipped),
    })
}

/// The archive's own totals, or a sentence saying why there are none.
///
/// `available` is a boolean the page switches on and `reason` is the sentence it prints, so a
/// browser never has to recognize a shape to know which of the two it was handed.
fn archive_value(inventory: &Inventory, redactor: &Redactor) -> Value {
    match inventory {
        Inventory::Unsupported => json!({
            "available": false,
            "reason": "archive totals: not available on this patwari",
        }),
        Inventory::Failed(error) => json!({
            "available": false,
            // The error text is this crate's own rendering of a status or a transport failure, but
            // it can quote a code the archive wrote, so it is clamped and scrubbed like every other
            // archive-stated string that reaches a browser.
            "reason": format!(
                "archive totals: {}",
                evidence::identifier_field(error, redactor)
            ),
        }),
        Inventory::Read { stats, clients } => json!({
            "available": true,
            "sessions": stats.sessions,
            "snapshots": stats.snapshots,
            "captures": stats.captures,
            "artifacts": stats.artifacts,
            "blobs": stats.blobs,
            "stored_bytes": stats.stored_bytes,
            "stored": format::bytes(stats.stored_bytes),
            "original_bytes": stats.original_bytes,
            "original": format::bytes(stats.original_bytes),
            "tombstones": stats.tombstones,
            "client_count": stats.clients,
            // Instants as the archive stated them, clamped and scrubbed on the way out like every
            // other string it wrote. This client never parses them: it prints them under a heading
            // that says whose clock they are.
            "last_ingest_at": archive_field(stats.last_ingest_at.as_deref(), redactor),
            "oldest_activity_at": archive_field(stats.oldest_activity_at.as_deref(), redactor),
            "newest_activity_at": archive_field(stats.newest_activity_at.as_deref(), redactor),
            "generated_at": evidence::identifier_field(&stats.generated_at, redactor),
            "schema_version": stats.schema_version,
            "instance": evidence::identifier_field(&stats.archive_instance_id, redactor),
            "clients": clients
                .iter()
                .map(|client| json!({
                    "client_id": evidence::identifier_field(&client.client_id, redactor),
                    // The device scope's own label function, so a machine is spelled the same way
                    // in this table as it is in the rows above it and in the scope control — and a
                    // client that registered no hostname reads as the same sentence, never as a
                    // path.
                    "device": scopes::device_label(client.hostname.as_deref(), redactor),
                    "captures": client.capture_count,
                    "first_seen_at": evidence::identifier_field(&client.first_seen_at, redactor),
                    "last_seen_at": archive_field(client.last_seen_at.as_deref(), redactor),
                }))
                .collect::<Vec<_>>(),
        }),
    }
}

/// An optional archive-stated string, clamped and scrubbed, or JSON null.
fn archive_field(value: Option<&str>, redactor: &Redactor) -> Value {
    match value {
        Some(value) => json!(evidence::identifier_field(value, redactor)),
        None => Value::Null,
    }
}

/// This mirror: where it keeps what it has read, how much that is, and what the last sync of the
/// coaching lane cost.
///
/// Every figure here is already in the provenance footer. It is repeated because the footer answers
/// "what did this page cost to build" and this answers "is my copy healthy", and a reader asking the
/// second question should not have to assemble it out of the first.
fn mirror_value(mirror: &Mirror, folded: &Folded) -> Value {
    let instrumentation = &folded.instrumentation;
    json!({
        "cache_root": mirror.cache_root.display().to_string(),
        "files": mirror.usage.files,
        "bytes": mirror.usage.bytes,
        "size": format::bytes(mirror.usage.bytes),
        "sync": format::elapsed(instrumentation.sync.elapsed),
        "sync_millis": millis(instrumentation.sync.elapsed),
        "fold": format::elapsed(instrumentation.fold_elapsed),
        "fold_millis": millis(instrumentation.fold_elapsed),
        "sessions_listed": instrumentation.sync.sessions_listed,
        "sessions_folded": instrumentation.sessions_folded,
        "cache_hits": instrumentation.sync.cache_hits,
        "cache_misses": instrumentation.sync.cache_misses,
        "snapshots_indexed": instrumentation.sync.snapshots_indexed,
        "snapshots_fetched": instrumentation.sync.snapshots_fetched,
    })
}

/// A duration in whole milliseconds, saturating rather than wrapping — the same rendering the
/// provenance block's `*_millis` fields carry, beside the same `format::elapsed` prose.
fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::TimeDelta;

    use crate::metrics::SessionMetrics;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a fixture timestamp")
            .with_timezone(&Utc)
    }

    fn device(sessions: usize, mix: &[(&str, usize)], last: Option<&str>) -> Device {
        Device {
            label: "macbookpro".to_owned(),
            attributed: true,
            sessions,
            by_harness: mix
                .iter()
                .map(|(name, count)| ((*name).to_owned(), *count))
                .collect(),
            last_capture: last.map(at),
        }
    }

    /// Busiest harness first, and the label as the tie-break, so the line is stable across runs
    /// rather than following whatever order a map happened to iterate in.
    #[test]
    fn a_harness_mix_reads_busiest_first_and_breaks_ties_on_the_label() {
        let mixed = device(15, &[("copilot-cli", 3), ("claude-code", 12)], None);
        assert_eq!(mixed.harness_mix(), "claude-code 12 · copilot-cli 3");

        let tied = device(4, &[("zed-agent", 2), ("claude-code", 2)], None);
        assert_eq!(tied.harness_mix(), "claude-code 2 · zed-agent 2");
    }

    /// The silence flag is whole days and never negative: an archive whose clock is ahead of this
    /// machine's reports "0 days ago", not a device that will report tomorrow.
    #[test]
    fn silence_is_whole_days_from_the_fold_and_never_negative() {
        let now = at("2026-09-04T12:00:00Z");
        assert_eq!(
            device(1, &[], Some("2026-09-04T09:00:00Z")).silent_days(now),
            Some(0),
        );
        assert_eq!(
            device(1, &[], Some("2026-09-01T09:00:00Z")).silent_days(now),
            Some(3),
        );
        assert_eq!(
            device(1, &[], Some("2026-09-05T09:00:00Z")).silent_days(now),
            Some(0),
        );
        assert_eq!(device(1, &[], None).silent_days(now), None);
    }

    /// The flag fires at the constant and not a day before it, so the panel's own threshold is
    /// pinned rather than left to whichever comparison operator was typed.
    #[test]
    fn a_device_is_silent_exactly_at_the_constant() {
        let now = at("2026-09-04T12:00:00Z");
        let silent = |days: i64| {
            let last = (now - TimeDelta::days(days)).to_rfc3339();
            device_value(&device(1, &[], Some(&last)), now)["silent"]
                .as_bool()
                .expect("a boolean")
        };
        assert!(!silent(SILENT_AFTER_DAYS - 1));
        assert!(silent(SILENT_AFTER_DAYS));
        assert!(silent(SILENT_AFTER_DAYS + 5));
    }

    /// The smallest honest session for a view that reads three of its fields: the host it ran on,
    /// when the archive finished it, and the harness that wrote it. Everything a fold would fill in
    /// from a transcript is left at its own zero, because nothing here reads any of it.
    fn dated(hostname: Option<&str>, archived_at: Option<&str>) -> SessionMetrics {
        SessionMetrics {
            source_hash: "00".repeat(32),
            source_agent: "claude-code".to_owned(),
            repository: None,
            archived_at: archived_at.map(at),
            hostname: hostname.map(ToOwned::to_owned),
            utc_offset: None,
            artifact_set_version: 2,
            summary: munshi_transcript::SessionSummary::default(),
            tools: crate::metrics::ToolOutcomes::default(),
            activity: crate::metrics::Activity::default(),
            commands: crate::metrics::CommandChurn::default(),
            compactions: crate::metrics::Compactions::default(),
            reviews: crate::metrics::ReviewActivity::default(),
            anchors: crate::evidence::SessionAnchors::default(),
            bytes_folded: 0,
        }
    }

    /// The calendar is sparse over days and dense over devices, and an unplaceable session is
    /// counted rather than dropped or guessed onto a date.
    #[test]
    fn landing_places_each_session_on_its_archive_day_and_counts_the_rest() {
        let redactor = Redactor::new();
        let devices = vec![
            Device {
                label: "one".to_owned(),
                attributed: true,
                sessions: 2,
                by_harness: BTreeMap::new(),
                last_capture: None,
            },
            Device {
                label: "two".to_owned(),
                attributed: true,
                sessions: 1,
                by_harness: BTreeMap::new(),
                last_capture: None,
            },
        ];
        let sessions = vec![
            dated(Some("one"), Some("2026-09-01T23:59:00Z")),
            dated(Some("one"), Some("2026-09-02T00:01:00Z")),
            dated(Some("two"), Some("2026-09-02T10:00:00Z")),
            dated(Some("one"), None),
        ];
        let landing = landing(&sessions, &devices, &redactor);

        assert_eq!(landing.days.len(), 2);
        assert_eq!(landing.days[0].date.to_string(), "2026-09-01");
        assert_eq!(landing.days[0].by_device, vec![1, 0]);
        assert_eq!(landing.days[1].date.to_string(), "2026-09-02");
        assert_eq!(landing.days[1].by_device, vec![1, 1]);
        assert_eq!(landing.undated, 1);
        // The whole point of the pair: what is drawn plus what could not be drawn is the window.
        assert_eq!(landing.sessions() + landing.undated, sessions.len());
    }

    /// A patwari without the inventory routes is a state the panel renders, never a refresh that
    /// failed — and the two ways it can have no totals do not read the same.
    #[test]
    fn an_archive_with_no_inventory_route_says_so_and_is_not_a_failure() {
        let redactor = Redactor::new();
        let unsupported = archive_value(&Inventory::Unsupported, &redactor);
        assert_eq!(unsupported["available"], false);
        assert_eq!(
            unsupported["reason"],
            "archive totals: not available on this patwari"
        );

        let failed = archive_value(
            &Inventory::Failed("patwari answered 503".to_owned()),
            &redactor,
        );
        assert_eq!(failed["available"], false);
        assert_eq!(failed["reason"], "archive totals: patwari answered 503");
    }
}
