//! `--json`: the same fold every document lane renders, under a shape a program can index.
//!
//! Markdown is what qanungo is *for* — a report a person reads, a standup pasted into a skill, a
//! doctor document the `instructions-editor` acts on — and it stays the default on every lane.
//! `--json` is for the other reader: `jq`, a script, a spreadsheet, a second tool that wants the
//! lane scores without parsing a table out of prose.
//!
//! # It computes nothing
//!
//! Not one number here is this module's own, exactly as [`crate::dashboard`] states of itself. Each
//! builder takes the very `Folded*` its Markdown renderer takes, and where the dashboard already
//! shapes that fold — the coaching, cost and standup sections, and the ranked ask hit — this module
//! calls **that** builder rather than writing a second one. So `qanungo report --json` and
//! `/api/data` cannot come to disagree about a lane score: they are the same function over the same
//! struct, and a change to either is a change to both.
//!
//! Two lanes had no served shape to borrow, `doctor` and `flows`, so their `data` is built here —
//! from the public fields of [`crate::doctor::Doctor`] and [`crate::flows::Flows`], which are the
//! same fields [`crate::doctor_report`] and [`crate::flows_report`] render and nothing besides.
//!
//! # Redaction: the same scrub, by construction rather than by a second pass
//!
//! The standing rule (qanungo #8) is that transcript content reaches a surface scrubbed. This
//! module holds it the way every other surface in the crate holds it — **there is nothing
//! unscrubbed in scope to serialize**:
//!
//! - `report` and `cost` carry no verbatim at all. Their folds have already reduced a transcript to
//!   counts, timestamps, locators and digests; the one archive-written string either document
//!   carries is a gap line's harness label, scrubbed by the fold. `tests/json_output.rs` plants
//!   credentials in the fixtures and walks both documents to prove it, the way `tests/dashboard.rs`
//!   does for the served payload.
//! - `standup`, `ask`, `doctor` and `flows` do carry prose, and it arrives **already scrubbed**:
//!   [`crate::standup::Standup::fold`], [`crate::ask::Ask::fold`], [`crate::doctor::Doctor::fold`]
//!   and [`crate::flows::Flows::fold`] each scrub on the way *into* their own types, so a
//!   `Folded*` holds no pre-scrub string for this module to reach.
//!
//! What this module must **not** do is re-scrub: a second redactor call would be a second posture
//! to keep in step with the first, and it would double-count what fired in the very footer that
//! reports it. So the redaction block below is a *statement* of the posture and its counts, and
//! never a filter.
//!
//! # The envelope
//!
//! Every command's document is the same six keys around a `data` that differs:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "command": "report",
//!   "generated_at": "2026-09-04T09:00:00Z",
//!   "window": { "last": "30d", "opens_at": "...", "comparison_opens_at": "..." },
//!   "rule_pack": "<64 hex characters>",
//!   "provenance": { "sessions_folded": 41, "fold": "1.2 s", "cache_hits": 41, ... },
//!   "data": { ... }
//! }
//! ```
//!
//! `schema_version` is the promise: an added key is not a version bump, a removed or re-meaning one
//! is. `rule_pack` is the **full** digest rather than the footer's short stamp, because a machine
//! comparing two runs wants the whole thing and a person reading one has the Markdown. It is
//! present on every command, including the four that evaluate no rule, because it names the pack
//! *this binary carries* — which is the fact that makes two `--json` documents comparable at all.
//!
//! `provenance` is the Markdown footer, key for key: what the fold cost, what the archive cost,
//! what the cache spared. It is in the envelope rather than in `data` because it is a statement
//! about the *run*, not about the lane's subject — and it is never omitted, because a number with
//! no cost beside it is a number nobody can decide whether to trust.
//!
//! # Errors still go to stderr
//!
//! `--json` changes what a lane writes to stdout and nothing else. A failure is still the binary's
//! error chain on stderr and a non-zero exit ([`crate::main`]'s job), so a script that pipes stdout
//! into `jq` gets either a document or nothing — never a document with an error sentence in it.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use crate::ask::{Ask, Escalation, Query};
use crate::cli::Window;
use crate::command::Folded;
use crate::command::{
    CommandError, FoldedAsk, FoldedCost, FoldedDoctor, FoldedFlows, FoldedStandup,
};
use crate::dashboard::{
    ASK_SCOPE, ask_hit_value, coaching_section, cost_section, gaps_value, redaction_value,
    scope_tags, standup_section,
};
use crate::doctor::Doctor;
use crate::flows::Flows;
use crate::format;
use crate::redaction::{PATTERN_REVISION, Redactor};
use crate::repetition::Citation;
use crate::report::stamp;
use crate::scoring::RulePack;
use crate::sync::SyncStats;

/// The shape of the envelope below, and the only thing a consumer has to pin.
///
/// One, and it stays one until a key changes meaning or leaves. Adding a key is not a break — a
/// reader that indexes what it needs is unaffected — so this number is deliberately not a changelog
/// of every field this crate has ever grown.
pub const SCHEMA_VERSION: u64 = 1;

/// What `--last` covers when a lane has no `--last` and does not want one.
///
/// The three lifetime lanes — `ask`, `doctor`, `flows` — say this rather than a duration, for the
/// reason [`crate::cli::AskArgs`] argues: printing a span where there is none would invite a reader
/// to read one. It is the dashboard's own word for the same fact, imported rather than respelled.
const ALL_HISTORY: &str = ASK_SCOPE;

/// Writes one envelope to `out`, pretty-printed and newline-terminated.
///
/// Pretty rather than compact because the first reader of a `--json` run is nearly always a person
/// checking what the keys are called, and every consumer after that is a parser that does not care.
/// The trailing newline is so a document composes with a shell the way the Markdown does.
///
/// # Errors
///
/// Returns [`CommandError::Output`] when stdout cannot be written — a closed pipe, most often,
/// which is the same failure the Markdown path reports.
pub fn write(out: &mut impl Write, document: &Value) -> Result<(), CommandError> {
    let mut rendered = serde_json::to_vec_pretty(document)
        .expect("an envelope built from `json!` values always serializes");
    rendered.push(b'\n');
    out.write_all(&rendered).map_err(CommandError::Output)
}

/// The envelope every lane's document wears.
///
/// A struct with a [`Envelope::build`] rather than a function of seven arguments, on the reasoning
/// [`crate::dashboard::Payload`] states: every field is somebody else's output, and naming them at
/// the call site is what stops a window being handed the provenance of a different lane.
struct Envelope<'a> {
    command: &'a str,
    generated_at: DateTime<Utc>,
    window: Value,
    /// The full digest of the pack this binary carries. See the module docs.
    rule_pack: &'a str,
    provenance: Value,
    data: Value,
}

impl Envelope<'_> {
    fn build(self) -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "command": self.command,
            "generated_at": stamp(self.generated_at),
            "window": self.window,
            "rule_pack": self.rule_pack,
            "provenance": self.provenance,
            "data": self.data,
        })
    }
}

/// A window the lane was given, laid out the way every other surface in this crate lays one out.
///
/// `comparison_opens_at` is present only for the two lanes that fold a pair, and `null` there when
/// the window is too long to place an equal-length one before it — the same three-way honesty the
/// documents keep.
fn window_value(window: &Window, generated_at: DateTime<Utc>, comparison: bool) -> Value {
    let mut value = json!({
        "last": window.to_string(),
        "opens_at": stamp(window.opens_at(generated_at)),
    });
    if comparison {
        value["comparison_opens_at"] = match window.comparison_opens_at(generated_at) {
            Some(at) => json!(stamp(at)),
            None => Value::Null,
        };
    }
    value
}

/// A lifetime lane's window: the one it was narrowed to, or the whole archive.
///
/// `scope` is what a reader indexes when `last` is `null`, and it is a word rather than a duration
/// on purpose — see [`ALL_HISTORY`].
fn optional_window_value(window: Option<&Window>, generated_at: DateTime<Utc>) -> Value {
    match window {
        Some(window) => {
            let mut value = window_value(window, generated_at, false);
            value["scope"] = json!("window");
            value
        }
        None => json!({ "last": Value::Null, "scope": ALL_HISTORY }),
    }
}

/// The half of the footer every lane reports in the same quantities and the same renderings.
///
/// Durations and byte counts arrive **pre-rendered** by [`crate::format`] beside their raw values,
/// exactly as the served provenance block does it and for the same reason: how a duration reads is
/// that module's job, and a second implementation of it downstream would drift from the footers
/// this block mirrors.
fn sync_provenance(sync: &SyncStats, fold_elapsed: Duration) -> Map<String, Value> {
    let Value::Object(fields) = json!({
        "sessions_listed": sync.sessions_listed,
        "fold": format::elapsed(fold_elapsed),
        "fold_millis": millis(fold_elapsed),
        "sync": format::elapsed(sync.elapsed),
        "sync_millis": millis(sync.elapsed),
        "bytes_transferred": format::bytes(sync.bytes_transferred),
        "bytes_transferred_raw": sync.bytes_transferred,
        "cache_hits": sync.cache_hits,
        "cache_misses": sync.cache_misses,
        "snapshots_indexed": sync.snapshots_indexed,
        "snapshots_fetched": sync.snapshots_fetched,
    }) else {
        unreachable!("a `json!` object literal is an object");
    };
    fields
}

/// Where the numbers came from and where they were cached, on every lane's footer.
///
/// The archive's base URL is **text**, never a link, for the reason [`crate::dashboard`] states at
/// length: Patwari serves unredacted blobs. On this surface that matters less than it does in a
/// browser — a terminal reader's own shell already has raw access — but the same key means the same
/// thing in both places, and that is worth more than the exception would buy.
fn locate(fields: &mut Map<String, Value>, patwari_url: &str, cache_root: &Path) {
    fields.insert("patwari_url".to_owned(), json!(patwari_url));
    fields.insert("cache_root".to_owned(), json!(format::path(cache_root)));
}

/// The scrub that stood behind whatever prose this run rendered, and what it fired.
///
/// A statement, never a filter — see the module docs. The two flags are the ones the operator typed
/// (or did not), and the counts are the fold's own [`crate::redaction::RedactionReport`], so a
/// reader can reconcile a document against the posture that produced it.
fn redaction_posture(redactor: &Redactor) -> Value {
    json!({
        "secrets": redactor.redacts_secrets(),
        "profanity": redactor.filters_profanity(),
        "pattern_revision": PATTERN_REVISION,
    })
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// The six lanes
// ---------------------------------------------------------------------------

/// `qanungo report --json`: the coaching section the dashboard serves, in the CLI's envelope.
///
/// `data` is [`coaching_section`] verbatim — `window`, `sessions`, `lanes`, `findings` — so
/// `.data.lanes[0].fleet.score` on this document and on `/api/data` are the same number by
/// construction rather than by agreement.
///
/// The redactor is [`Redactor::new`] because `report` has no `--no-redact`: the document renders
/// aggregates and hashes, and the one archive-stated string in it is scrubbed unconditionally. See
/// [`crate::command::report`].
pub fn report(window: &Window, folded: &Folded) -> Value {
    let redactor = Redactor::new();
    let instrumentation = &folded.instrumentation;
    let mut provenance = sync_provenance(&instrumentation.sync, instrumentation.fold_elapsed);
    provenance.insert(
        "sessions_folded".to_owned(),
        json!(instrumentation.sessions_folded),
    );
    provenance.insert(
        "comparison_sessions_folded".to_owned(),
        json!(instrumentation.comparison_sessions_folded),
    );
    provenance.insert(
        "bytes_folded".to_owned(),
        json!(format::bytes(instrumentation.bytes_folded)),
    );
    provenance.insert(
        "bytes_folded_raw".to_owned(),
        json!(instrumentation.bytes_folded),
    );
    provenance.insert(
        "rule_pack_stamp".to_owned(),
        json!(instrumentation.rule_pack.stamp()),
    );
    // The one lane that renders no verbatim *and* has no flag to turn one on. Stated rather than
    // omitted, so a consumer can tell "carries none" from "was not asked".
    provenance.insert("renders_verbatim".to_owned(), json!(false));
    locate(
        &mut provenance,
        &instrumentation.patwari_url,
        &instrumentation.cache_root,
    );

    Envelope {
        command: "report",
        generated_at: folded.generated_at,
        window: window_value(window, folded.generated_at, true),
        rule_pack: instrumentation.rule_pack.digest(),
        provenance: Value::Object(provenance),
        data: coaching_section(window, folded, &scope_tags(folded, &redactor), &redactor),
    }
    .build()
}

/// `qanungo cost --json`: the priced window the dashboard serves, in the CLI's envelope.
///
/// Its redactor is built the same way [`report`]'s is, and for the same reason: this lane has no
/// `--no-redact` either, and a gap line's harness label is scrubbed whatever was typed.
pub fn cost(window: &Window, folded: &FoldedCost) -> Value {
    let redactor = Redactor::new();
    let instrumentation = &folded.instrumentation;
    let mut provenance = sync_provenance(&instrumentation.sync, instrumentation.fold_elapsed);
    provenance.insert(
        "sessions_folded".to_owned(),
        json!(instrumentation.sessions_folded),
    );
    provenance.insert(
        "comparison_sessions_folded".to_owned(),
        json!(instrumentation.comparison_sessions_folded),
    );
    provenance.insert(
        "records_read".to_owned(),
        json!(instrumentation.records_read),
    );
    provenance.insert(
        "bytes_folded".to_owned(),
        json!(format::bytes(instrumentation.bytes_folded)),
    );
    provenance.insert(
        "bytes_folded_raw".to_owned(),
        json!(instrumentation.bytes_folded),
    );
    provenance.insert("renders_verbatim".to_owned(), json!(false));
    locate(
        &mut provenance,
        &instrumentation.patwari_url,
        &instrumentation.cache_root,
    );

    Envelope {
        command: "cost",
        generated_at: folded.generated_at,
        window: window_value(window, folded.generated_at, true),
        rule_pack: RulePack::current().digest(),
        provenance: Value::Object(provenance),
        data: cost_section(window, folded, &redactor),
    }
    .build()
}

/// `qanungo standup --json`: the narrated window the dashboard serves, in the CLI's envelope.
///
/// The first `--json` lane that carries prose. It carries it already scrubbed — see the module
/// docs — and states the posture and the counts in `provenance.redaction` and `data.redaction`
/// respectively, which are the two different questions "what was the scrub" and "what did it find".
pub fn standup(window: &Window, folded: &FoldedStandup) -> Value {
    let instrumentation = &folded.instrumentation;
    let mut provenance = sync_provenance(&instrumentation.sync, instrumentation.fold_elapsed);
    provenance.insert("sessions_folded".to_owned(), json!(folded.standup.sessions));
    provenance.insert(
        "bytes_read".to_owned(),
        json!(format::bytes(folded.standup.bytes_read)),
    );
    provenance.insert(
        "bytes_read_raw".to_owned(),
        json!(folded.standup.bytes_read),
    );
    provenance.insert("renders_verbatim".to_owned(), json!(true));
    provenance.insert(
        "redaction".to_owned(),
        redaction_posture(&instrumentation.redactor),
    );
    locate(
        &mut provenance,
        &instrumentation.patwari_url,
        &instrumentation.cache_root,
    );

    Envelope {
        command: "standup",
        generated_at: folded.generated_at,
        window: window_value(window, folded.generated_at, false),
        rule_pack: RulePack::current().digest(),
        provenance: Value::Object(provenance),
        data: standup_section(window, folded),
    }
    .build()
}

/// `qanungo ask --json`: the ranking, and — when `--verbatim` was asked for — what the transcripts
/// behind the shown hits said.
///
/// The hit shape is the dashboard's [`ask_hit_value`], with one key the served answer cannot have:
/// `verbatim`. The dashboard offers no escalation at all (a browser must never induce archive
/// traffic), so this is the CLI's own field rather than a divergence — the ranking itself is
/// identical.
pub fn ask(
    window: Option<&Window>,
    query: &Query,
    limit: usize,
    verbatim_requested: bool,
    folded: &FoldedAsk,
) -> Value {
    let instrumentation = &folded.instrumentation;
    let ask = &folded.ask;
    let mut provenance = sync_provenance(&instrumentation.sync, instrumentation.fold_elapsed);
    provenance.insert("sessions_searchable".to_owned(), json!(ask.searched));
    provenance.insert("sessions_unsearchable".to_owned(), json!(ask.unsearchable));
    provenance.insert(
        "bytes_read".to_owned(),
        json!(format::bytes(ask.bytes_read)),
    );
    provenance.insert("bytes_read_raw".to_owned(), json!(ask.bytes_read));
    provenance.insert("renders_verbatim".to_owned(), json!(true));
    provenance.insert(
        "redaction".to_owned(),
        redaction_posture(&instrumentation.redactor),
    );
    // The escalation is network work outside the fold timer, so it is reported as its own block
    // rather than folded into the figures above — the same split the Markdown footer makes.
    provenance.insert(
        "verbatim".to_owned(),
        match &instrumentation.verbatim {
            Some(stats) => json!({
                "requested": true,
                "transcripts_searched": stats.transcripts_searched,
                "transcripts_unavailable": stats.transcripts_unavailable,
                "transcripts_fetched": stats.transcripts_fetched,
                "snapshots_fetched": stats.snapshots_fetched,
                "matches": stats.matches,
                "shown": stats.shown,
                "unreadable_records": stats.unreadable_records,
                "bytes_searched": format::bytes(stats.bytes_searched),
                "bytes_transferred": format::bytes(stats.bytes_transferred),
                "elapsed": format::elapsed(stats.elapsed),
                "elapsed_millis": millis(stats.elapsed),
            }),
            None => json!({ "requested": false }),
        },
    );
    locate(
        &mut provenance,
        &instrumentation.patwari_url,
        &instrumentation.cache_root,
    );

    Envelope {
        command: "ask",
        generated_at: folded.generated_at,
        window: optional_window_value(window, folded.generated_at),
        rule_pack: RulePack::current().digest(),
        provenance: Value::Object(provenance),
        data: ask_data(query, limit, verbatim_requested, Some(ask)),
    }
    .build()
}

/// The answer to a query with no searchable word in it, before anything touched the archive.
///
/// The Markdown lane answers this on the spot rather than mirroring the whole archive to rank
/// nothing ([`crate::ask_report::no_searchable_terms`]); this is that same refusal, as a document.
/// Its provenance is empty of archive figures because there was no archive work to report, which is
/// the honest reading of a run that never listed a session.
pub fn ask_no_searchable_terms(
    window: Option<&Window>,
    query: &Query,
    limit: usize,
    redactor: &Redactor,
) -> Value {
    let generated_at = Utc::now();
    Envelope {
        command: "ask",
        generated_at,
        window: optional_window_value(window, generated_at),
        rule_pack: RulePack::current().digest(),
        provenance: {
            // The same keys every other run reports, all zero. Zeros rather than absent keys: a
            // consumer that indexes `provenance.cache_hits` should not have to special-case the one
            // answer that never opened the cache, and zero is the honest figure for a run that
            // listed nothing.
            let mut provenance = sync_provenance(&SyncStats::default(), Duration::ZERO);
            provenance.insert("sessions_searchable".to_owned(), json!(0));
            provenance.insert("sessions_unsearchable".to_owned(), json!(0));
            provenance.insert("renders_verbatim".to_owned(), json!(true));
            provenance.insert("redaction".to_owned(), redaction_posture(redactor));
            provenance.insert("verbatim".to_owned(), json!({ "requested": false }));
            Value::Object(provenance)
        },
        data: ask_data(query, limit, false, None),
    }
    .build()
}

/// The ranking, in the three states the Markdown makes the same three-way split into.
///
/// `no-searchable-terms` is "you gave me no word to search on"; `no-matches` is the archive's own
/// "no", which is the answer a person asking *have I ever done this* came for; `ranked` is a list.
/// The dashboard's [`crate::dashboard::AskAnswer`] states them in exactly these words.
fn ask_data(query: &Query, limit: usize, verbatim_requested: bool, ask: Option<&Ask>) -> Value {
    let state = match ask {
        None => "no-searchable-terms",
        Some(ask) if ask.total_matches == 0 => "no-matches",
        Some(_) => "ranked",
    };
    json!({
        "state": state,
        "query": {
            // The words the search actually used, clamped the way every other label on a rendering
            // surface is: a caller's own bytes never make the round trip. See `AskAnswer`.
            "terms": query
                .terms()
                .iter()
                .map(|term| format::identifier(term))
                .collect::<Vec<_>>(),
            "min_term_chars": crate::ask::MIN_TERM_CHARS,
        },
        "limit": limit,
        "verbatim_requested": verbatim_requested,
        "searched": ask.map_or(0, |ask| ask.searched),
        "unsearchable": ask.map_or(0, |ask| ask.unsearchable),
        "total_matches": ask.map_or(0, |ask| ask.total_matches),
        "hits": ask
            .map(|ask| {
                ask.hits
                    .iter()
                    .enumerate()
                    .map(|(rank, hit)| {
                        let mut value = ask_hit_value(rank + 1, hit);
                        value["verbatim"] = match &hit.verbatim {
                            Some(escalation) => escalation_value(escalation),
                            None => Value::Null,
                        };
                        value
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        "redaction": ask.map(|ask| redaction_value(&ask.redaction)),
    })
}

/// What the escalation found for one shown hit — or why there was nothing to look at.
///
/// Two states rather than an empty match list, for the reason [`Escalation`] is an enum rather than
/// an `Option`: a hit whose transcript could not be read is **not** a hit with no matches, and
/// serializing it as one would turn "I could not look" into "there is nothing there".
fn escalation_value(escalation: &Escalation) -> Value {
    match escalation {
        Escalation::Searched(found) => json!({
            "state": "searched",
            "total_matches": found.total_matches,
            "events_searched": found.events_searched,
            "unreadable_records": found.unreadable_records,
            "matches": found
                .matches
                .iter()
                .map(|matched| json!({
                    "locator": matched.locator,
                    "record": matched.record,
                    "line": matched.line,
                    "at": matched.at.map(stamp),
                    "surface": matched.surface,
                    "excerpt": matched.excerpt,
                }))
                .collect::<Vec<_>>(),
            "redaction": redaction_value(&found.redaction),
        }),
        // The mirror's own words, scrubbed where they were built — see `crate::command::escalate`.
        Escalation::Unavailable(reason) => json!({
            "state": "unavailable",
            "reason": reason,
        }),
    }
}

/// `qanungo doctor --json`: the repeated instructions, per repository, with their citations.
///
/// `clusters_per_repo` is echoed because it is the cut the *rendering* took: every count in here
/// was taken before it, so a consumer that wants the held-back clusters raises the flag rather than
/// inferring them from a difference it cannot see. `found` beside each repository's `clusters` is
/// how many there were; the array is how many this run rendered.
pub fn doctor(window: Option<&Window>, clusters_per_repo: usize, folded: &FoldedDoctor) -> Value {
    let instrumentation = &folded.instrumentation;
    let doctor = &folded.doctor;
    let mut provenance = sync_provenance(&instrumentation.sync, instrumentation.fold_elapsed);
    provenance.insert("sessions_folded".to_owned(), json!(doctor.sessions));
    provenance.insert(
        "bytes_folded".to_owned(),
        json!(format::bytes(doctor.bytes_folded)),
    );
    provenance.insert("bytes_folded_raw".to_owned(), json!(doctor.bytes_folded));
    provenance.insert(
        "unreadable_records".to_owned(),
        json!(doctor.unreadable_records),
    );
    provenance.insert("renders_verbatim".to_owned(), json!(true));
    provenance.insert(
        "redaction".to_owned(),
        redaction_posture(&instrumentation.redactor),
    );
    locate(
        &mut provenance,
        &instrumentation.patwari_url,
        &instrumentation.cache_root,
    );

    Envelope {
        command: "doctor",
        generated_at: folded.generated_at,
        window: optional_window_value(window, folded.generated_at),
        rule_pack: RulePack::current().digest(),
        provenance: Value::Object(provenance),
        data: doctor_data(clusters_per_repo, doctor),
    }
    .build()
}

fn doctor_data(clusters_per_repo: usize, doctor: &Doctor) -> Value {
    json!({
        "clusters_per_repo": clusters_per_repo,
        "repositories": doctor
            .repositories
            .iter()
            .map(|group| json!({
                "repository": group.repository,
                "found": group.found,
                "occurrences": group.occurrences,
                "clusters": group
                    .clusters
                    .iter()
                    .map(|cluster| json!({
                        "occurrences": cluster.occurrences,
                        "sessions": cluster.sessions,
                        "excerpt": cluster.excerpt,
                        "citations": citations_value(&cluster.citations),
                    }))
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
        // Counts and a repository label, never an excerpt — the type carries none. See
        // `crate::doctor::Friction`.
        "friction": doctor
            .friction
            .iter()
            .map(|friction| json!({
                "repository": friction.repository,
                "sessions": friction.sessions,
                "messages": friction.messages,
                "after_error": friction.after_error,
            }))
            .collect::<Vec<_>>(),
        "unexamined": doctor
            .unexamined
            .iter()
            .map(|note| json!({
                "repository": note.repository,
                "sessions": note.sessions,
                "reason": note.reason,
            }))
            .collect::<Vec<_>>(),
        "sessions": doctor.sessions,
        "repositories_examined": doctor.repositories_examined,
        "messages": doctor.messages,
        "harness_generated": doctor.harness_generated,
        "clusterable": doctor.clusterable,
        "conversations": doctor.conversations,
        "sessions_without_messages": doctor.sessions_without_messages,
        "clusters": doctor.clusters,
        "gaps": gaps_value(&doctor.gaps),
        "redaction": redaction_value(&doctor.redaction),
    })
}

/// `qanungo flows --json`: the repeated requests and the multi-step runs they fall into.
///
/// Both cuts are echoed for the reason `doctor`'s one is: `clusters_found` and `flows_found` are
/// the counts before the rendering cut, and the two arrays are what this run rendered.
pub fn flows(
    window: Option<&Window>,
    clusters: usize,
    flows: usize,
    folded: &FoldedFlows,
) -> Value {
    let instrumentation = &folded.instrumentation;
    let found = &folded.flows;
    let mut provenance = sync_provenance(&instrumentation.sync, instrumentation.fold_elapsed);
    provenance.insert("sessions_folded".to_owned(), json!(found.sessions));
    provenance.insert(
        "bytes_folded".to_owned(),
        json!(format::bytes(found.bytes_folded)),
    );
    provenance.insert("bytes_folded_raw".to_owned(), json!(found.bytes_folded));
    provenance.insert(
        "unreadable_records".to_owned(),
        json!(found.unreadable_records),
    );
    provenance.insert("renders_verbatim".to_owned(), json!(true));
    provenance.insert(
        "redaction".to_owned(),
        redaction_posture(&instrumentation.redactor),
    );
    locate(
        &mut provenance,
        &instrumentation.patwari_url,
        &instrumentation.cache_root,
    );

    Envelope {
        command: "flows",
        generated_at: folded.generated_at,
        window: optional_window_value(window, folded.generated_at),
        rule_pack: RulePack::current().digest(),
        provenance: Value::Object(provenance),
        data: flows_data(clusters, flows, found),
    }
    .build()
}

fn flows_data(clusters_cap: usize, flows_cap: usize, found: &Flows) -> Value {
    json!({
        "clusters_cap": clusters_cap,
        "flows_cap": flows_cap,
        "clusters": found
            .clusters
            .iter()
            .map(|cluster| json!({
                "occurrences": cluster.occurrences,
                "sessions": cluster.sessions,
                "excerpt": cluster.excerpt,
                "repositories": repository_counts_value(&cluster.repositories),
                "repositories_found": cluster.repositories_found,
                "citations": citations_value(&cluster.citations),
            }))
            .collect::<Vec<_>>(),
        "clusters_found": found.clusters_found,
        "flows": found
            .flows
            .iter()
            .map(|flow| json!({
                "steps": flow.steps,
                "occurrences": flow.occurrences,
                "sessions": flow.sessions,
                "repositories": repository_counts_value(&flow.repositories),
                "repositories_found": flow.repositories_found,
                "instances": flow
                    .instances
                    .iter()
                    .map(|instance| json!({
                        "archived_at": instance.archived_at.map(stamp),
                        "source_hash": instance.source_hash,
                        "locators": instance.locators,
                    }))
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
        "flows_found": found.flows_found,
        "repositories": found
            .repositories
            .iter()
            .map(|read| json!({
                "repository": read.repository,
                "sessions": read.sessions,
                "clusterable": read.clusterable,
            }))
            .collect::<Vec<_>>(),
        "sessions": found.sessions,
        "messages": found.messages,
        "harness_generated": found.harness_generated,
        "clusterable": found.clusterable,
        "conversations": found.conversations,
        "sessions_without_messages": found.sessions_without_messages,
        "gaps": gaps_value(&found.gaps),
        "redaction": redaction_value(&found.redaction),
    })
}

/// Where one repeated request was seen, and how often — the same pair both documents print.
fn repository_counts_value(counts: &[crate::flows::RepositoryCount]) -> Value {
    counts
        .iter()
        .map(|count| {
            json!({
                "repository": count.repository,
                "occurrences": count.occurrences,
            })
        })
        .collect()
}

/// One occurrence: which transcript, when the archive took it, and where in it to look.
///
/// A `source_hash` and a locator, which is a coordinate a reader takes to their own copy — never a
/// link into Patwari, on the rule [`crate::dashboard`] states.
fn citations_value(citations: &[Citation]) -> Value {
    citations
        .iter()
        .map(|citation| {
            json!({
                "archived_at": citation.archived_at.map(stamp),
                "source_hash": citation.source_hash,
                "locator": citation.locator,
            })
        })
        .collect()
}
