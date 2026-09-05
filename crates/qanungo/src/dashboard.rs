//! The dashboard's payload: three lanes' own numbers, as JSON.
//!
//! # The redaction line (hard), restated for a served surface
//!
//! The line this module holds is **not one sentence any more**, because the page is no longer one
//! lane. It is three, and saying so is the honest version:
//!
//! - The **coaching** section carries lane scores, rule ids, counts, rendered aggregates,
//!   archive-stated identifiers, tool names, evidence anchors, and `sha256` content hashes.
//! - The **cost** section carries token counts, message counts, dollars, the model, modifier, and
//!   repository identifiers the archive itself recorded, clamped, and — for the small top-tier
//!   sessions it lists one by one — the same `sha256` content hashes the coaching section cites.
//! - Neither of those two carries a byte of transcript text. Both hold that by *construction*, the
//!   way [`crate::report`] and [`crate::cost_report`] do: every field is read off a fold whose
//!   types have already reduced a transcript to counts, timestamps, locators, and digests, so
//!   there is no string in either to filter.
//! - The **standup** section carries prose somebody typed into a terminal — and carries it
//!   *scrubbed*, which is a different claim from carrying none. See below.
//!
//! ## The standup section, and why it is not an exception
//!
//! [`crate::standup`] scrubs on the way *into* its own types, so a
//! [`crate::command::FoldedStandup`] holds no pre-scrub string at all — the parsed
//! archive record is a local of [`fold_standup`](crate::command::fold_standup) and is dropped
//! before it returns. This module therefore has nothing unscrubbed in scope to serialize by
//! mistake, exactly as [`crate::standup_report`] has nothing unscrubbed to render by mistake. The
//! two surfaces are the same guarantee reached the same way, and the redaction markers travel with
//! the text so a reader can see the scrub fired.
//!
//! What this module must *not* do is re-scrub. A second redactor call here would be a second place
//! the posture could drift from the one the process was launched with, and a section whose text had
//! been through two passes would report counts that matched neither.
//!
//! A fixture archive stuffed with canary strings — including planted credentials in a `summary.md`
//! — is serialized through this module in `tests/dashboard.rs`, so a field that started carrying
//! unscrubbed text fails a test rather than reaching a browser.
//!
//! ## The copilot rule, carried onto the wire
//!
//! [`crate::cost`]'s honesty rule is that Copilot records output tokens and nothing else, and its
//! billing regime is not recoverable from a transcript — so it gets volumes and **no dollars, no
//! credit estimate, no premium-request count**. That rule is a property of the *payload* here and
//! not of the page: the copilot rows carry no money-shaped field at all, and there is no blended
//! total anywhere that would hide the split behind one number. A page cannot render a dollar figure
//! it was never handed, which is a stronger guarantee than a page that was handed one and chose
//! well. `tests/dashboard.rs` pins it over the wire.
//!
//! An **anchor is not content**: a locator, a record number, a line number, a timestamp, and a tool
//! name — schema metadata, and the one verbatim string decision 9 blessed for an aggregate surface.
//! The tool name is nevertheless *scrubbed* here as well as clamped, because a harness writes its
//! own tool names and this payload is the label on a control that expands into transcript text; see
//! [`anchor_value`]. What an anchor *resolves to* is content, and that resolution is a separate
//! on-demand route with the same redactor wired into it — see [`crate::evidence`] and
//! [`crate::dashboard_server`]. This is why `provenance.renders_verbatim` is now **`true`**: the
//! payload still carries none, but the surface it belongs to does, and a page that claimed
//! otherwise would be inviting a reader to trust a control it had stopped exercising.
//!
//! **No raw Patwari links, anywhere.** Patwari serves unredacted blobs and never redacts, so a
//! deep link from this page to an artifact would hand any tailnet device the whole transcript —
//! the correction the 2026-08-24 grilling made to qanungo #5. The archive's base URL appears once,
//! in the provenance block, as *text saying which archive these numbers came from*. The page
//! renders it as text and builds no link from it; the recall funnel stays a CLI affordance, where
//! the user's own shell already has raw access.
//!
//! # It computes nothing
//!
//! Every number here was computed by [`fold_coaching`](crate::command::fold_coaching),
//! [`fold_cost`](crate::command::fold_cost), and [`fold_standup`](crate::command::fold_standup) —
//! the same three calls `qanungo report`, `qanungo cost`, and `qanungo standup` make, on the same
//! windows. This module chooses a shape and a key name; it does not choose a value.
//!
//! Not one arithmetic here is this module's own. The arrow rules are [`Trend::between`] and
//! [`Blend::comparable`](crate::scoring::Blend::comparable), the same two functions the Markdown
//! table draws its `▲` from. The cost delta is the same subtraction [`crate::cost_report`] renders,
//! drawn under the same two refusals — no comparison window at all, and a comparison window that
//! priced nothing are different facts and both are stated. What caching saved is
//! [`PricedTokens::cache_saving`]. The row orderings are the CLI's orderings, most expensive first,
//! because the reason to read a cost table is to find where the money went and that reason does not
//! change with the medium. Every rendered figure comes out of [`crate::format`] beside its raw
//! value, so a second implementation of "how a dollar reads" cannot drift into the JavaScript.
//!
//! # Three lanes, three windows, one generation
//!
//! The sections do not share a window and must not be made to: a coaching score wants a month, a
//! bill wants a quarter, a standup wants a week. [`Windows`] carries all three, each labelled in
//! its own section and again in provenance, so no reader has to guess which span a number is a
//! statement about.
//!
//! They do share a **generation**. One refresh folds all three and publishes one document, so a
//! reader can never see a cost section from one fold beside a standup section from another — a
//! torn view across lanes is impossible by construction rather than unlikely by timing. That is the
//! whole reason this is one payload built in one call: see [`crate::dashboard_server`].
//!
//! # One route, measured
//!
//! `/api/data` stays the single payload. When the standup and cost sections landed it measured
//! **744 KiB** against production: standup 529 KiB (71%), findings-with-anchors 195 KiB (26%), cost
//! 15 KiB (2%), everything else under 6 KiB — 4.6x the V1 payload, with the standup section
//! essentially all of the growth (100 sessions of prose, the same 353 KiB `qanungo standup
//! --last 7d` prints).
//!
//! **Every figure in this section is a snapshot of a moving archive.** The archive gains tens of
//! sessions a day, and the same command measured 946.8 KiB days later with nothing about this code
//! changed. These numbers are here for the *shape* — which section dominates, and what each one
//! scales with — so each is stamped with the day it was taken and none is worth chasing. The same
//! caution applies to any session or rule-firing count quoted anywhere in this crate's docs.
//!
//! Splitting the standup onto its own route was the obvious alternative and is not worth what it
//! costs. The saving is one 744 KiB fetch per refresh interval becoming one 215 KiB fetch plus a
//! second one when a reader scrolls — about 2.5 kbit/s of sustained tailnet traffic either way, on
//! a page that renders every section on load anyway. The price is a second route, a second set of
//! atomicity tests, and — the real objection — a page that can hold two sections fetched at
//! different generations, which is the exact failure this slice is arranged against. Keeping the
//! generation honest across two routes is possible (both would read one [`Refreshed`] snapshot) but
//! it is a guarantee held by care where it is currently held by there being nothing to get wrong.
//!
//! The number to watch is the standup window: `--standup-last 30d` would put this near 2 MiB. If it
//! goes there, the answer is still not a split — it is that a served narrative of 400 sessions is
//! not a thing anybody reads, and the section should bound what it renders and say that it did.
//!
//! ## What the scope slice added, measured
//!
//! Re-measured against production on 2026-08-25 with the scopes in: **1,095,731 B (1070.0 KiB)**
//! against **969,498 B (946.8 KiB)** for the same window without them — **+123.3 KiB, +13.0%**,
//! of which the `scopes` section is 105.5 KiB (9.9% of the body) and the two evidence tags are
//! 17.8 KiB across 333 citations. The window was 705 sessions in **28 repositories** across two
//! harnesses.
//!
//! What that cost buys is worth stating plainly, because 10% is not nothing. It is 28 pre-folded
//! scopes served to every reader, of which one reader will look at one — but the alternative is a
//! per-request fold, which is the thing this design refuses for reasons that are not about bytes
//! (see below). It also **scales with repositories, not with sessions or with prose**: the section
//! is one cell per (repository × lane × harness-or-fleet) and each cell is a score, a trend, and
//! the component lines that explain it — about 650 B on production. Doubling the archive's history
//! does not move it; working in twice as many repositories does.
//!
//! The one trim available and not taken is the component `detail` sentences, which are
//! most of those bytes. They stay because a score with no reading beside it is a number a reader
//! cannot check, and the whole argument for scopes is that a reader can ask a narrower question and
//! still see what answered it.
//!
//! ## What the timeline slice added, measured
//!
//! Against production on 2026-08-25, same window and same run pair: **1,123,533 B (1097.2 KiB)**
//! with the timeline against **1,108,020 B (1082.1 KiB)** without — **+15,513 B, +15.1 KiB,
//! +1.4%**. The `timeline` sections together are 15,061 B (14.7 KiB, **1.3% of the body**): 1,898 B
//! at the top level and 13,163 B across the 28 repository scopes. That is 179 day rows at **84 B a
//! row**, and a row is a date and two integer arrays.
//!
//! An order of magnitude cheaper than the scopes it rides beside, and it is worth saying why,
//! because the obvious implementation is not. The section is **sparse over the calendar**: only days
//! a session actually landed on are served, so a scope's rows are bounded by that scope's session
//! count and the whole section by the window's. A dense day × harness grid per scope would instead
//! cost the window's *length* × the roster × the repository count — on this window, 30 days × 2
//! harnesses × 28 scopes of mostly zeroes, several times the size for strictly less information.
//! Keeping the harness axis positional rather than repeating a label per day per scope is the other
//! half: the labels alone would have been most of the section.
//!
//! The window measured 707 sessions over **26 UTC days**, and no comparison window at all — the
//! archive holds nothing in `[60d, 30d)`, so `comparison_days` was empty and the boundary rule was
//! exercised at `--last 15d` instead (531 + 176 = 707 across 16 + 10 days, each half reconciling
//! with its own count).
//!
//! # Scopes, and the rule they do not bend
//!
//! [`Payload::scopes_section`] adds every repository scope to the same document: one pre-folded
//! entry per repository, each carrying the five lanes and what fired inside it, and every **device**
//! scope beside them — the second primary axis, the same fold grouped by the host the manifest
//! recorded rather than the repository the listing projected. The numbers are [`crate::scopes`]'s
//! selection of this fold handed back to [`Scorecard::fold_refs`] — never a second formula, and
//! never a second fold of a transcript. The two primary axes do not cross: harness is folded and so
//! rides free inside either, but a repository *and* a device at once would name a cell no fold here
//! produced, so the page narrows by one or the other. See [`crate::scopes`].
//!
//! Two properties are worth naming here because they are what a reviewer should check. The
//! **all/all scope is the top-level section**, unchanged and unduplicated: a scope control that
//! moved the whole-window numbers would be a payload change wearing a feature. And **no scope is a
//! function of the request** — there is no query string on this route and there will not be one,
//! because a served document that varied by who asked could show two readers different numbers
//! under the same generation stamp, and because it would put a fold behind a request an
//! unauthenticated peer controls.
//!
//! # The timeline, and the clock it is honest about
//!
//! [`Payload::timeline_section`] adds the same fold laid on a calendar: sessions and active time per
//! **UTC calendar day of the session's archive completion**, split by harness, for the whole window
//! and again inside every repository scope. It is grouped by the same selection the scores are, so
//! a narrowed page's bars sum to that page's own session count.
//!
//! Three things about it are worth a reviewer's attention. It is **archive time**, not the
//! transcript's own clock and not the reader's local one — the clock the window was cut on, which is
//! the only one the counts can reconcile against, and the page says so in its own words rather than
//! only here. It carries **numbers and ISO dates and no string at all**, because the harness axis is
//! the payload's existing one and a day row is positional against it. And it is the clock that lets
//! the timeline reconcile where the heatmap below cannot: a per-day volume survives a missing offset
//! and "at 1 a.m." does not. See [`crate::timeline`].
//!
//! # The heatmap, and the clock it is honest about instead
//!
//! [`Payload::heatmap_section`] adds the *other* clock: the same fold laid on the operator's own
//! day. A cell is a **local hour of the weekday the work began** — the transcript's first-activity
//! instant shifted by the session's own `utc_offset` — because "at 1 a.m." and "on a Sunday" are
//! precisely the two claims UTC misplaces, and the heatmap is the view that exists to make them. It
//! is grouped by the same selection per scope, carries **grid indices and no string at all**, and a
//! session with no recorded offset is on no cell and counted, the same refusal the timeline makes
//! for a missing archive time. See [`crate::heatmap`].
//!
//! # The ask answer, which is not a section
//!
//! [`AskAnswer`] is the one document in this module a *request* decides, and it is deliberately not
//! part of the payload above. `GET /api/ask?q=…` ranks the refresh's own in-memory corpus of
//! `summary.md` records ([`crate::command::AskCorpus`]) with [`crate::ask`]'s rubric — the same
//! function, the same total order, the same scrub `qanungo ask` runs — and answers with the hits.
//!
//! Serving it per request rather than pre-folding it is not a hole in the "no fold behind a request"
//! rule; it is that rule's two objections not applying. There is no archive traffic on the path (the
//! corpus was mirrored on the refresh timer, so a browser cannot make this process talk to Patwari —
//! the evidence route's iron rule, applied rather than excepted), and a ranking is a selection of the
//! corpus rather than a claim about it, so two readers asking different questions at one generation
//! are not being told different things. Pre-folding was never the alternative: the space of queries
//! is not enumerable, and shipping the corpus to the browser would be shipping every summary in the
//! archive to every tab.
//!
//! The provenance block gains the lane's own numbers ([`ask_lane_value`]) and nothing else changes
//! above. **No verbatim on this surface**: `--verbatim` stays a CLI affordance (decision 11), and
//! the corpus this route reads holds no transcript to escalate into.
//!
//! # What this slice leaves out
//!
//! Both views this section used to defer are now here: the heatmap above, and the per-**device**
//! scope — exactly a repository scope over [`SessionMetrics::hostname`], a second primary axis on
//! the same payload rather than a change to it, made worth building by the archive accruing more
//! than one host since the capture side shipped on 2026-08-25. What stays out is two *combinations*,
//! not views: the bill is still whole-window under a device (cost groups by repository and carries
//! no host — a later `cost.rs` fold), and the heatmap is cut by repository, not device, so a
//! per-device habits grid is a free later combination over this same payload.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::ask::{Ask, Query};
use crate::cli::{Refresh, Window};
use crate::command::{AskCorpus, Folded, FoldedCost, FoldedStandup};
use crate::cost::{
    CopilotTokens, CostTotals, Flagged, PREMIUM_FLAG_MAX_MESSAGES, PREMIUM_FLAG_MAX_OUTPUT_TOKENS,
    PremiumFlag, PricedTokens, TokenTally,
};
use crate::cost_report::PREMIUM_SESSIONS_LISTED;
use crate::evidence::{self, EventAnchor, EvidenceIndex};
use crate::format;
use crate::heatmap::Heatmap;
use crate::metrics::{SessionMetrics, Totals};
use crate::pricing::PRICE_TABLE_REVISION;
use crate::redaction::{PATTERN_REVISION, RedactionReport, Redactor};
use crate::report::{self, SkippedNote, stamp};
use crate::rules::Finding;
use crate::scopes::{self, DeviceScope, NO_DEVICE, RepositoryScope};
use crate::scoring::{Lane, LaneScore, Scorecard, Trend};
use crate::standup::{NO_REPOSITORY, RolledUp, StandupSession};
use crate::timeline::Timeline;

/// What the refresh loop knows and the fold does not: which run this is, when it landed, and
/// whether the last attempt to take a new one failed.
#[derive(Debug, Clone, Copy)]
pub struct Refreshed {
    /// Bumped on every swap of the served payload. An SSE client uses it to tell a genuine refresh
    /// from a reconnection.
    pub generation: u64,
    /// When this payload was published.
    pub at: DateTime<Utc>,
    /// When the *first* of the current run of failed refreshes happened, if the loop is currently
    /// failing. `None` means the numbers below are as fresh as the interval allows.
    ///
    /// Why the first failure rather than the last: what a reader needs is how old the numbers are,
    /// and that is the age of the last *success*, which the first failure dates. Reporting the most
    /// recent failure would make a dashboard that has been broken for a day look a minute old.
    pub stale_since: Option<DateTime<Utc>>,
}

/// The set of anchors this payload names, and therefore the entire set of excerpts the process will
/// resolve while this payload is the current one.
///
/// Built from the findings rather than from the fold: a session can be anchored and not be *cited*
/// — the rule it was anchored for may not have fired on it — and only what a reader can see on the
/// page is something they may ask to expand. See [`crate::evidence::EvidenceIndex`] for why that
/// boundary is what keeps this from becoming a transcript-browsing API.
pub fn evidence_index(folded: &Folded) -> EvidenceIndex {
    let by_hash: BTreeMap<&str, &SessionMetrics> = folded
        .sessions
        .iter()
        .map(|session| (session.source_hash.as_str(), session))
        .collect();
    let mut index = EvidenceIndex::default();
    for finding in &folded.findings {
        for evidence in &finding.evidence {
            // A finding whose session is not in this window's fold cannot happen — the findings
            // were evaluated over exactly these sessions — and if it ever did, the honest answer is
            // to offer nothing rather than to guess at an interpreter.
            let Some(session) = by_hash.get(evidence.source_hash.as_str()) else {
                continue;
            };
            for anchor in &evidence.anchors {
                index.offer(
                    &evidence.source_hash,
                    &session.source_agent,
                    session.artifact_set_version,
                    anchor.locator,
                );
            }
        }
    }
    index
}

/// The three windows one refresh folds.
///
/// A type rather than three arguments because they travel together everywhere — the refresh loop
/// holds them, the payload labels each section with one, and provenance echoes all three — and
/// three bare [`Window`]s in a row is exactly the signature where two of them get swapped and every
/// test still passes.
#[derive(Debug, Clone)]
pub struct Windows {
    /// What the lane scores and the findings are taken over. `--last`, default `30d`.
    pub coaching: Window,
    /// What the bill covers. `--cost-last`, default `12w`.
    pub cost: Window,
    /// What the narrative covers. `--standup-last`, default `7d`.
    pub standup: Window,
}

/// Everything one served document is built from: three folds, the windows they were taken over,
/// and the three facts only a long-lived process has.
///
/// A struct with a [`Payload::build`] rather than a function of eight arguments, on the same
/// reasoning [`crate::report::Report`] and [`crate::cost_report::CostReport`] are structs with a
/// `render` — and for one reason more, which is that every field here is somebody else's output.
/// Naming them at the call site is what keeps the cost fold from being handed to the standup
/// section by a caller counting positions.
pub struct Payload<'a> {
    pub windows: &'a Windows,
    pub refresh: &'a Refresh,
    pub coaching: &'a Folded,
    pub cost: &'a FoldedCost,
    pub standup: &'a FoldedStandup,
    /// The searchable corpus this refresh read. **No section of this document is built from it** —
    /// it answers `/api/ask` on request ([`AskAnswer`]) and appears here only in the provenance
    /// block, so a reader of the footer can see what the fourth lane cost. It rides on the
    /// [`Payload`] rather than beside it because it is part of the same generation: a footer
    /// reporting a corpus from a different refresh than the numbers above it would be the torn view
    /// this whole design is arranged against.
    pub ask: &'a AskCorpus,
    /// Wall-time of the three folds together — the number that answers "what does a refresh of this
    /// page actually cost".
    ///
    /// Measured rather than added, even though against production it lands within a tenth of a
    /// second of the sum of the three lanes' own syncs and folds (45.4 s warm, 2026-08-25). It is
    /// measured because *whether* it equals that sum is a fact about the mirror that can change: the
    /// blob cache already spares the transfers, and a cursor protocol or a listing cache would start
    /// sparing the per-session requests that currently dominate. A footer that added the parts would
    /// keep reporting the old answer straight through the change worth noticing.
    pub folds_elapsed: Duration,
    pub refreshed: Refreshed,
    pub redactor: &'a Redactor,
}

impl Payload<'_> {
    /// Builds the served JSON document.
    ///
    /// One call per refresh, never per request: the body is serialized once and handed to every
    /// reader as bytes, so a hundred open tabs cost one fold and one serialization between them.
    ///
    /// The `redactor` does two things here, and neither of them is scrubbing the standup section —
    /// which arrives already scrubbed, by the fold, with the counts that say so. It is **stated** in
    /// the provenance block, because a page that renders verbatim has to say which scrub stands
    /// behind it, and that answer is fixed at launch and identical for every reader. And it scrubs
    /// the one archive-written string the coaching section puts onto its evidence controls, the tool
    /// name on an anchor (see `anchor_value` below).
    /// The coaching section keeps the **top level** it had when it was the whole payload —
    /// `window`, `sessions`, `lanes`, `findings` — rather than being moved under a `coaching` key
    /// beside its two new siblings. Symmetry is not worth the cost here: nesting it would break
    /// every reader of the V1 payload to buy a shape nobody reads twice, and the duplication that
    /// would keep both working is a second copy of the largest section in the document.
    pub fn build(&self) -> Value {
        // One pass over the fold's per-session facts, shared by the two sections that need them:
        // the findings, which tag each cited session so the page can narrow a list it already has,
        // and the scopes, which group the same sessions to score them. Built once because a tag and
        // a group key that were computed twice are two things that can disagree, and the whole
        // point of a scope control is that the tag on a row and the scope it belongs to are the
        // same statement.
        let tags = self.scope_tags();
        let mut document = self.coaching_section(&tags);
        let fields = document
            .as_object_mut()
            .expect("the coaching section is an object");
        let timeline = self.timeline_section();
        let heatmap = self.heatmap_section();
        fields.insert("cost".to_owned(), self.cost_section());
        fields.insert("standup".to_owned(), self.standup_section());
        fields.insert("scopes".to_owned(), self.scopes_section(&tags));
        fields.insert(
            "provenance".to_owned(),
            self.provenance(&timeline, &heatmap),
        );
        fields.insert("timeline".to_owned(), timeline);
        fields.insert("heatmap".to_owned(), heatmap);
        document
    }

    /// The coaching window on a calendar: how many sessions landed on each day, and how much work
    /// was in them.
    ///
    /// # Archive time, said out loud
    ///
    /// A day is the **UTC calendar day of the session's archive completion time**, and the page
    /// prints that sentence rather than leaving it in this comment. It is the clock the window
    /// itself was cut on, which is the whole reason it is the right one: the bars sum back to the
    /// session count in the subtitle above them. It is emphatically **not local time** — that is
    /// the heatmap's clock, off the transcript's own instant and the machine's offset. A per-day
    /// volume survives UTC and reconciles; "at 1 a.m." does not, which is why that claim lives on the
    /// heatmap and this view stays on archive time. See [`crate::timeline`].
    ///
    /// # Numbers and dates, nothing else
    ///
    /// The section carries **no string at all** — not even a harness label. Its per-day arrays are
    /// positional against `scopes.harnesses`, the payload's one harness axis, which the lanes are
    /// already keyed on and the page's control is already built from. Two things follow. It saves
    /// the obvious bytes: a label per day per harness per scope would be most of this section. And
    /// it buys the stronger property — a section made only of integers and ISO dates has nowhere
    /// for an archive-written byte to hide, which `tests/dashboard.rs` walks to prove rather than
    /// trusts. One harness, one string, in one place: the same rule the scope slice's review
    /// arrived at, applied before there was a second spelling to reconcile.
    ///
    /// # Two windows, two lists
    ///
    /// A window opens at an instant, not at midnight, so one calendar day can hold sessions from
    /// both halves of the pair. They are two lists rather than one list with a flag, so each sums
    /// to its own window's folded count and the page can draw the boundary where the window
    /// actually opens. A straddling day appears in both, holding its own half — which is the
    /// honest shape and, on a chart, a visible one.
    fn timeline_section(&self) -> Value {
        let folded = self.coaching;
        let now = Scorecard::fold(&folded.sessions);
        let before = folded.compared.then(|| Scorecard::fold(&folded.previous));
        let columns = report::harness_columns(&now, before.as_ref());
        // The comparison half is laid out only when there is a comparison window at all — the same
        // question the lanes ask before they draw an arrow, answered the same way, so the page
        // cannot draw a `before` the scores refused to compare against.
        let previous = if folded.compared {
            Timeline::fold(&folded.previous)
        } else {
            Timeline::default()
        };
        timeline_value(&Timeline::fold(&folded.sessions), &previous, &columns)
    }

    /// The coaching window on the operator's own clock: which local hour of which weekday the work
    /// began in, by harness.
    ///
    /// # Local time, said out loud
    ///
    /// A cell is a **local hour of the weekday the session's first activity fell on** — the
    /// transcript's own first-record instant shifted by the session's recorded `utc_offset`. It is
    /// the one view UTC breaks: "at 1 a.m." and "on a Sunday" are exactly the claims a UTC clock
    /// misplaces, which is why the timeline could ship on archive time and this could not until the
    /// offset accrued. The page prints that sentence rather than leaving it here. See
    /// [`crate::heatmap`].
    ///
    /// # No offset, no cell
    ///
    /// A session whose snapshot recorded no offset is on no cell and counted — surfaced the way the
    /// timeline surfaces its undated count, never guessed onto an hour by assuming a zone. This is a
    /// single-window section (no comparison half): a habits grid is a shape, and a shape reads
    /// against itself, not against a shifted copy of itself.
    ///
    /// # Numbers and indices, nothing else
    ///
    /// Like the timeline, the section carries **no string at all** — a cell is a weekday index, an
    /// hour, and two arrays positional against `scopes.harnesses`. The weekday and hour *labels* are
    /// the page's, and the page builds no date through `Date`.
    fn heatmap_section(&self) -> Value {
        let folded = self.coaching;
        let now = Scorecard::fold(&folded.sessions);
        let before = folded.compared.then(|| Scorecard::fold(&folded.previous));
        let columns = report::harness_columns(&now, before.as_ref());
        heatmap_value(&Heatmap::fold(&folded.sessions), &columns)
    }

    /// The coaching lane: the window pair, the five lanes, and the findings under them.
    fn coaching_section(&self, tags: &ScopeTags) -> Value {
        let window = &self.windows.coaching;
        let folded = self.coaching;
        // The same two questions the report asks in the same order: is there a comparison window at
        // all, and if so what did it score? A window too long to place an equal-length one before it
        // has no `before`, and therefore no arrow anywhere on the page.
        let comparison_opens_at = folded
            .compared
            .then(|| window.comparison_opens_at(folded.generated_at))
            .flatten();
        let now = Scorecard::fold(&folded.sessions);
        let before = comparison_opens_at.map(|_| Scorecard::fold(&folded.previous));
        let columns = report::harness_columns(&now, before.as_ref());
        let by_hash: BTreeMap<&str, &SessionMetrics> = folded
            .sessions
            .iter()
            .map(|session| (session.source_hash.as_str(), session))
            .collect();

        json!({
            "window": {
                "last": window.to_string(),
                "opens_at": stamp(window.opens_at(folded.generated_at)),
                "comparison_opens_at": comparison_opens_at.map(stamp),
                "generated_at": stamp(folded.generated_at),
                "compared": folded.compared,
            },
            "sessions": sessions_value(folded, self.redactor),
            "lanes": Lane::ALL
                .iter()
                .map(|lane| lane_value(*lane, &now, before.as_ref(), &columns, self.redactor))
                .collect::<Vec<_>>(),
            "findings": folded
                .findings
                .iter()
                .map(|finding| finding_value(finding, &by_hash, tags, self.redactor))
                .collect::<Vec<_>>(),
        })
    }

    /// The cost lane, over its own window: what was spent, on what, where, and what the fold
    /// refused to turn into money.
    fn cost_section(&self) -> Value {
        let window = &self.windows.cost;
        let folded = self.cost;
        let totals = &folded.totals;
        json!({
            "window": {
                "last": window.to_string(),
                "opens_at": stamp(window.opens_at(folded.generated_at)),
                "comparison_opens_at": folded
                    .previous
                    .as_ref()
                    .and_then(|_| window.comparison_opens_at(folded.generated_at))
                    .map(stamp),
                "generated_at": stamp(folded.generated_at),
            },
            "sessions": {
                "priced": totals.priceable_sessions,
                "token_only": totals.token_only_sessions,
                "no_signal": totals
                    .no_signal_sessions
                    .iter()
                    .map(|(agent, count)| json!({
                        "source_agent": format::identifier(agent),
                        "sessions": count,
                    }))
                    .collect::<Vec<_>>(),
            },
            "priced": priced_total_value(totals),
            "by_model": by_model_value(totals),
            "by_repository": by_repository_value(totals, self.redactor),
            "caching": caching_value(&totals.priced),
            "comparison": self.cost_comparison(),
            "copilot": copilot_value(totals),
            "flagged": flagged_value(&totals.flagged),
            "premium": premium_value(&totals.premium),
            "records_read": totals.records_read,
            "duplicate_records": totals.duplicate_records,
            "price_table_revision": PRICE_TABLE_REVISION,
            "gaps": gaps_value(&folded.skipped),
        })
    }

    /// The window-over-window move on the bill, under the CLI's own two refusals.
    ///
    /// Three states, because collapsing any two of them would be a page reporting the archive's
    /// shape as spending: no comparison window was asked for at all; one was, and priced nothing;
    /// or both windows priced something and there is a real delta to draw. `▲` is more money, which
    /// is a direction and not a verdict — the page says so beside it.
    fn cost_comparison(&self) -> Value {
        let folded = self.cost;
        let (Some(previous), Some(opens_at)) = (
            folded.previous.as_ref(),
            self.windows.cost.comparison_opens_at(folded.generated_at),
        ) else {
            return json!({ "state": "no-window" });
        };
        if !previous.priced_anything() {
            return json!({
                "state": "nothing-priced",
                "opens_at": stamp(opens_at),
            });
        }
        let now = folded.totals.priced.dollars;
        let was = previous.priced.dollars;
        json!({
            "state": "compared",
            "opens_at": stamp(opens_at),
            "was": was,
            "was_rendered": format::dollars(was),
            "was_sessions": previous.priceable_sessions,
            "delta": now - was,
            "delta_rendered": format::dollars((now - was).abs()),
            // The glyph the coaching lane's arrows use, chosen the same way, so one page does not
            // spell "more" two ways. Equality is exact on purpose: two windows that priced the same
            // float to the cent still moved by nothing.
            "direction": if now > was {
                "up"
            } else if now < was {
                "down"
            } else {
                "flat"
            },
            "glyph": if now > was {
                "▲"
            } else if now < was {
                "▼"
            } else {
                "="
            },
        })
    }

    /// The standup lane, over its own window: the archive's own words, as the fold scrubbed them.
    fn standup_section(&self) -> Value {
        let window = &self.windows.standup;
        let folded = self.standup;
        let standup = &folded.standup;
        json!({
            "window": {
                "last": window.to_string(),
                "opens_at": stamp(window.opens_at(folded.generated_at)),
                "generated_at": stamp(folded.generated_at),
            },
            "sessions": standup.sessions,
            "repositories_narrated": standup.repositories_narrated(),
            "repositories": standup
                .repositories
                .iter()
                .map(|group| json!({
                    "repository": group.repository,
                    "sessions": group
                        .sessions
                        .iter()
                        .map(standup_session_value)
                        .collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "decisions": rolled_up_value(&standup.decisions),
            "open_items": rolled_up_value(&standup.open_items),
            "gaps": gaps_value(&standup.gaps),
            "redaction": redaction_value(&standup.redaction),
        })
    }

    /// The scope control's whole vocabulary, and every number a scope selection can put on screen.
    ///
    /// # Why the payload carries every scope
    ///
    /// **Query strings decide nothing on this surface.** There is no `?repository=` here and there
    /// will not be one: a per-request knob would make the served document a function of who asked,
    /// which is how a page comes to show two readers different numbers under the same generation
    /// stamp, and it would put a fold behind a request a peer on an unauthenticated tailnet
    /// controls. The scopes are pre-folded per view, exactly as the grilling decided drill-down
    /// slices would be, and the cost of that is one small section on a payload that is already
    /// dominated by narrative prose. See the module docs for the measurement.
    ///
    /// # One dimension, not two
    ///
    /// A scope reads as a pair — repository × harness — and is serialized as one axis, because the
    /// other is already folded: each scope's `lanes` carry the per-harness split
    /// [`Scorecard::fold_refs`] produces, so the harness-filtered scope *is* a column that is
    /// already here and the all-harnesses one *is* the fleet blend beside it. Writing the cross
    /// product out would be writing the same numbers twice. See [`crate::scopes`].
    ///
    /// Every scope's `lanes` array is built over the **same** harness columns as the top-level one,
    /// so a harness keeps its position in every scope and the page's control is one index
    /// everywhere. A harness with no session in a scope renders `no-sessions` there — the state
    /// [`report::harness_columns`] takes the union to be able to state at all.
    fn scopes_section(&self, tags: &ScopeTags) -> Value {
        let folded = self.coaching;
        // The same two questions, and therefore the same answer, as the whole-window section: no
        // comparison window, no arrow anywhere — including inside a scope.
        let compared = folded.compared;
        let now_all = Scorecard::fold(&folded.sessions);
        let before_all = compared.then(|| Scorecard::fold(&folded.previous));
        let columns = report::harness_columns(&now_all, before_all.as_ref());

        let scopes = scopes::by_repository(folded, self.redactor, self.foreign_labels());
        // The second primary axis: the same fold grouped by the host the manifest recorded, not the
        // repository the listing projected. It carries no foreign labels because no other section
        // cuts by host, and it does not cross with the repository axis — see [`scopes`] and the
        // page's mutually-exclusive primary controls. Its per-harness split is folded in, exactly as
        // the repository axis's is, so the page's one harness control narrows a device scope too.
        let devices = scopes::by_device(folded, self.redactor, None);
        json!({
            // The bucket a session with no repository lands in, named once so the page can match
            // the cost lane's `null` row and the standup lane's own heading against it rather than
            // spelling the sentence a second time in JavaScript.
            "unattributed": NO_REPOSITORY,
            // The device axis's own residue sentence, named here for the same reason.
            "unattributed_device": NO_DEVICE,
            "harnesses": columns
                .iter()
                .map(|column| evidence::identifier_field(column, self.redactor))
                .collect::<Vec<_>>(),
            "repositories": scopes
                .iter()
                .map(|scope| {
                    scope_value(
                        scope,
                        compared,
                        &columns,
                        &folded.findings,
                        tags,
                        self.redactor,
                    )
                })
                .collect::<Vec<_>>(),
            "devices": devices
                .iter()
                .map(|scope| {
                    device_scope_value(
                        scope,
                        compared,
                        &columns,
                        &folded.findings,
                        tags,
                        self.redactor,
                    )
                })
                .collect::<Vec<_>>(),
        })
    }

    /// Repository labels the *other* two sections put on the page, each rendered exactly as the
    /// section that owns it renders it.
    ///
    /// They join the scope list so the control can narrow everything the page shows: the bill
    /// covers a quarter and the narrative a week, so both hold repositories a 30-day coaching
    /// window does not, and a control that could not select a repository the page visibly renders
    /// would be a control that lies about what it narrows.
    ///
    /// # One repository, one string
    ///
    /// The equality this rests on is not a coincidence to be documented — it is a property to be
    /// held, and the review of this slice found the place it was not. The cost section used to
    /// render its repository through [`format::identifier`] alone while the scope key went through
    /// [`scopes::repository_label`]'s clamp-then-scrub, so a repository whose name was shaped like
    /// a credential came out as **two different strings**: the marker on the coaching side and the
    /// raw token on the bill. That is worse than either failure on its own. The raw secret was on
    /// the wire and in a dropdown, and the same repository appeared as two options, each narrowing
    /// half the page — the exact cross-labelling this control exists not to do.
    ///
    /// So [`by_repository_value`] now renders through [`evidence::identifier_field`] too, and the
    /// three paths are one path: the archive's bytes are judged by the clamp, what survives is
    /// scrubbed, and the result is the group key, the cost cell, and the option text. The standup
    /// lane reaches the same place from the other direction — it scrubs into its own types and
    /// clamps the result — and its labels are the fold's own strings, which is why they are taken
    /// verbatim here rather than re-rendered.
    fn foreign_labels(&self) -> Vec<String> {
        let cost = self
            .cost
            .totals
            .by_repository
            .keys()
            .map(|repository| scopes::repository_label(repository.as_deref(), self.redactor));
        let standup = self
            .standup
            .standup
            .repositories
            .iter()
            .map(|group| group.repository.clone());
        cost.chain(standup).collect()
    }
}

/// A window pair on a calendar: two day lists, each summing to its own window's folded count.
///
/// The two are kept apart rather than merged under a per-day flag because a window opens at an
/// instant and not at midnight — see [`Payload::timeline_section`]. `days_covered` is served beside
/// each list rather than left to be counted from it, because it is the provenance figure the footer
/// quotes and a reader should not have to derive a stated number.
///
/// `undated` is a session the archive gave no readable completion time for. It is on no bar and it
/// is counted, so the page can say why the bars are one short instead of the reader discovering it.
/// It should always be zero: a session with no archive time could not have been placed into a
/// window to be folded in the first place. Serving it is the cheap half of that claim.
fn timeline_value(reported: &Timeline, previous: &Timeline, columns: &[String]) -> Value {
    json!({
        "days": days_value(reported, columns),
        "days_covered": reported.days_covered(),
        "undated": reported.undated,
        "comparison_days": days_value(previous, columns),
        "comparison_days_covered": previous.days_covered(),
        "comparison_undated": previous.undated,
    })
}

/// One window's days, earliest first, each a date and two arrays indexed by the payload's harness
/// axis.
///
/// The arrays are **dense over the columns and sparse over the calendar**: every day carries one
/// entry per harness — a zero where that harness worked nothing, so the page can stack a column
/// without checking which keys exist — while a day nothing happened on is simply absent, because a
/// window's length must not decide what a scope costs to serve.
fn days_value(timeline: &Timeline, columns: &[String]) -> Value {
    timeline
        .days
        .iter()
        .map(|day| {
            let cells: Vec<&crate::timeline::DayCell> = columns
                .iter()
                .map(|column| day.harnesses.get(column).unwrap_or(&EMPTY_DAY))
                .collect();
            json!({
                "date": day.date.to_string(),
                "sessions": cells.iter().map(|cell| cell.sessions).collect::<Vec<_>>(),
                "active_seconds": cells
                    .iter()
                    .map(|cell| cell.active_seconds)
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// The zero a harness that worked nothing on a day contributes. A borrow of one constant rather
/// than a value built per cell: it is the majority of the cells in any real window.
const EMPTY_DAY: crate::timeline::DayCell = crate::timeline::DayCell {
    sessions: 0,
    active_seconds: 0,
};

/// One window's local hour-of-week grid: the sparse list of cells a session started in, plus the
/// two counts of sessions that could not be placed on a local clock at all.
///
/// `cells_covered` is served beside the list rather than left to be counted from it, the same
/// provenance courtesy the timeline extends its `days_covered`. `no_offset` and `undated` are the
/// sessions on no cell — the first is the general real state this whole view waited on (a capture
/// from before the offset metadata existed), the second a session with an offset but no readable
/// first-activity instant — so the page can say why the cells are a few short instead of the reader
/// discovering it.
fn heatmap_value(heatmap: &Heatmap, columns: &[String]) -> Value {
    json!({
        "cells": cells_value(heatmap, columns),
        "cells_covered": heatmap.cells_covered(),
        "no_offset": heatmap.no_offset,
        "undated": heatmap.undated,
    })
}

/// One window's local hour-of-week cells, ordered `(weekday, hour)`, each a weekday index, an hour,
/// and two arrays indexed by the payload's harness axis.
///
/// **Dense over the columns and sparse over the grid**, exactly as the timeline is dense over
/// harnesses and sparse over the calendar: every cell carries one entry per harness — a zero where
/// that harness started nothing — while a slot nothing began in is simply absent, because a 7×24
/// grid must not decide what a scope costs to serve.
fn cells_value(heatmap: &Heatmap, columns: &[String]) -> Value {
    heatmap
        .cells
        .iter()
        .map(|cell| {
            let hours: Vec<&crate::heatmap::HourCell> = columns
                .iter()
                .map(|column| cell.harnesses.get(column).unwrap_or(&EMPTY_HOUR))
                .collect();
            json!({
                "weekday": cell.weekday,
                "hour": cell.hour,
                "sessions": hours.iter().map(|hour| hour.sessions).collect::<Vec<_>>(),
                "active_seconds": hours
                    .iter()
                    .map(|hour| hour.active_seconds)
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// The zero a harness that started nothing in a cell contributes — the heatmap's [`EMPTY_DAY`], and
/// the majority of the cells in any real grid.
const EMPTY_HOUR: crate::heatmap::HourCell = crate::heatmap::HourCell {
    sessions: 0,
    active_seconds: 0,
};

/// One repository scope: what it holds, what it scores, and what fired inside it.
fn scope_value(
    scope: &RepositoryScope<'_>,
    compared: bool,
    columns: &[String],
    findings: &[Finding],
    tags: &ScopeTags,
    redactor: &Redactor,
) -> Value {
    let now = Scorecard::fold_refs(&scope.sessions);
    let before = compared.then(|| Scorecard::fold_refs(&scope.previous));
    // The scope's own calendar, from the same selection its scores are taken over — so the bars
    // under a narrowed page sum to the number in that page's own sentence, exactly as the whole
    // window's do. Same fold, same grouping code, one axis: see `Payload::timeline_section`.
    let previous_days = if compared {
        Timeline::fold(scope.previous.iter().copied())
    } else {
        Timeline::default()
    };
    json!({
        "repository": scope.label,
        "attributed": scope.attributed,
        "sessions": {
            "folded": scope.sessions.len(),
            "comparison_folded": scope.previous.len(),
            "by_harness": scope.by_harness(redactor),
        },
        "lanes": Lane::ALL
            .iter()
            .map(|lane| lane_value(*lane, &now, before.as_ref(), columns, redactor))
            .collect::<Vec<_>>(),
        "timeline": timeline_value(
            &Timeline::fold(scope.sessions.iter().copied()),
            &previous_days,
            columns,
        ),
        // The scope's own habits grid, from the same selection its scores and calendar are taken
        // over — so a narrowed page's cells sum to that page's own session count. One axis, one
        // fold, no comparison half: see `Payload::heatmap_section`.
        "heatmap": heatmap_value(&Heatmap::fold(scope.sessions.iter().copied()), columns),
        "findings": scope_findings_value(scope, findings, tags),
    })
}

/// What each rule fired on inside one scope, per harness.
///
/// Every rule that fired **anywhere in the window** gets a row here, including the ones that fired
/// nowhere in this scope: a zero is a reading and a missing key is not, and a page that hid the
/// difference could not tell "this repository is clean of that habit" from "this build forgot to
/// count it". The counts are counts of *cited sessions* — the evidence the finding already carries
/// — which is the same set the page narrows, so the number under a heading and the rows under it
/// can be checked against each other by anybody with the payload open.
fn scope_findings_value(
    scope: &RepositoryScope<'_>,
    findings: &[Finding],
    tags: &ScopeTags,
) -> Value {
    findings
        .iter()
        .map(|finding| {
            let mut by_harness: BTreeMap<&str, usize> = BTreeMap::new();
            let mut total = 0_usize;
            for evidence in &finding.evidence {
                let Some(tag) = tags.get(evidence.source_hash.as_str()) else {
                    continue;
                };
                if tag.repository != scope.label {
                    continue;
                }
                total += 1;
                *by_harness.entry(tag.harness.as_str()).or_default() += 1;
            }
            json!({
                "rule": finding.rule.key(),
                "sessions_affected": total,
                "by_harness": by_harness,
            })
        })
        .collect()
}

/// One device scope: [`scope_value`] over the host axis.
///
/// Every field is the same and computed the same way — [`Scorecard::fold_refs`] over the scope's
/// selection, the per-harness lanes, the scope's own calendar — because a device scope is a
/// selection of the same fold, not a different one. Only two things differ, both of them the axis's
/// identity: the label serializes under `device` rather than `repository`, and the per-scope finding
/// counts are cut by the session's rendered device label rather than its repository. The page's
/// harness control reads a column out of the `lanes` here exactly as it does out of a repository
/// scope's; the harness split is folded in, so nothing is a device × harness cross product.
fn device_scope_value(
    scope: &DeviceScope<'_>,
    compared: bool,
    columns: &[String],
    findings: &[Finding],
    tags: &ScopeTags,
    redactor: &Redactor,
) -> Value {
    let now = Scorecard::fold_refs(&scope.sessions);
    let before = compared.then(|| Scorecard::fold_refs(&scope.previous));
    let previous_days = if compared {
        Timeline::fold(scope.previous.iter().copied())
    } else {
        Timeline::default()
    };
    json!({
        "device": scope.label,
        "attributed": scope.attributed,
        "sessions": {
            "folded": scope.sessions.len(),
            "comparison_folded": scope.previous.len(),
            "by_harness": scope.by_harness(redactor),
        },
        "lanes": Lane::ALL
            .iter()
            .map(|lane| lane_value(*lane, &now, before.as_ref(), columns, redactor))
            .collect::<Vec<_>>(),
        "timeline": timeline_value(
            &Timeline::fold(scope.sessions.iter().copied()),
            &previous_days,
            columns,
        ),
        "findings": device_scope_findings_value(scope, findings, tags),
    })
}

/// What each rule fired on inside one device scope, per harness — [`scope_findings_value`] cut by
/// the device tag instead of the repository tag. Every rule that fired anywhere in the window gets a
/// row, including a zero here, for the reason the repository version gives: a zero is a reading and a
/// missing key is not.
fn device_scope_findings_value(
    scope: &DeviceScope<'_>,
    findings: &[Finding],
    tags: &ScopeTags,
) -> Value {
    findings
        .iter()
        .map(|finding| {
            let mut by_harness: BTreeMap<&str, usize> = BTreeMap::new();
            let mut total = 0_usize;
            for evidence in &finding.evidence {
                let Some(tag) = tags.get(evidence.source_hash.as_str()) else {
                    continue;
                };
                if tag.device != scope.label {
                    continue;
                }
                total += 1;
                *by_harness.entry(tag.harness.as_str()).or_default() += 1;
            }
            json!({
                "rule": finding.rule.key(),
                "sessions_affected": total,
                "by_harness": by_harness,
            })
        })
        .collect()
}

/// The window's bill: the headline, and the sample behind it.
///
/// Dollars arrive raw *and* rendered, as every figure in this payload does — [`crate::format`] owns
/// how money reads (to the cent, grouped, and `<$0.01` rather than `$0.00` for real spend below
/// one), and a second implementation of that in JavaScript would drift from the cost report this
/// section claims to be a view of.
fn priced_total_value(totals: &CostTotals) -> Value {
    json!({
        "priced_anything": totals.priced_anything(),
        "dollars": totals.priced.dollars,
        "dollars_rendered": format::dollars(totals.priced.dollars),
        "sessions": totals.priceable_sessions,
        "messages": totals.priced.tokens.messages,
        "fast_messages": totals.priced.fast_messages,
        "tokens": tally_value(&totals.priced.tokens),
    })
}

/// One token tally, every category the fold counts, raw beside rendered.
///
/// `thinking` is carried and is deliberately *not* a category: it is a share of `output` and is
/// already inside it, so a reader adding the columns must not find it there twice. The key is named
/// `thinking_of_output` to say so on the wire rather than in a comment nobody serves.
fn tally_value(tally: &TokenTally) -> Value {
    let rendered = |count: u64| json!({ "tokens": count, "rendered": format::tokens(count) });
    json!({
        "total": rendered(tally.total()),
        "input": rendered(tally.input),
        "output": rendered(tally.output),
        "cache_write_5m": rendered(tally.cache_write_5m),
        "cache_write_1h": rendered(tally.cache_write_1h),
        "cache_write_untiered": rendered(tally.cache_write_untiered),
        "cache_read": rendered(tally.cache_read),
        "thinking_of_output": rendered(tally.thinking),
    })
}

/// Where the money went by model, most expensive first — the CLI's own ordering, because the reason
/// to read this table is the same reason in both media.
///
/// Model ids are the archive's strings and go through [`format::identifier`] exactly as they do on
/// the way into the Markdown table, for the reason that clamp exists: a manifest states whatever it
/// likes, and a served page is a rendering surface a peer does not get to choose characters on.
fn by_model_value(totals: &CostTotals) -> Value {
    let mut rows: Vec<_> = totals.by_model.iter().collect();
    rows.sort_by(|(left_model, left), (right_model, right)| {
        right
            .dollars
            .total_cmp(&left.dollars)
            .then_with(|| left_model.cmp(right_model))
    });
    rows.into_iter()
        .map(|(model, priced)| {
            let mut row = priced_row_value(priced);
            row["model"] = json!(format::identifier(model));
            row
        })
        .collect()
}

/// The same money, cut by the repository the archive recorded. A session captured outside a
/// checkout has no repository and is its own row: `repository` is `null` there rather than being
/// folded into somebody else's, exactly as the report gives it its own `(no repository)` line.
///
/// The name is clamped **and scrubbed** — [`evidence::identifier_field`] — rather than clamped
/// alone. Two reasons, and the second is the one that made this a defect rather than a preference.
///
/// A repository name is an archive-stated identifier, and decision 9 blessed those for an
/// aggregate surface; that blessing has already been withdrawn once, for the tool name on an
/// anchor, on the grounds that a *rendering control* is not an aggregate line. A repository name is
/// now the text of an option in the scope select, which is the same argument reaching the same
/// answer.
///
/// And this cell has to be **the same string** as the scope key built from the same archive value
/// ([`Payload::foreign_labels`]). Clamping here while clamping-then-scrubbing there gave a
/// credential-shaped repository two spellings — the marker on the coaching side, the raw token on
/// the bill — which put the secret on the wire *and* split one repository into two options that
/// each narrowed half the page. One rendering, one label, one option.
fn by_repository_value(totals: &CostTotals, redactor: &Redactor) -> Value {
    let mut rows: Vec<_> = totals.by_repository.iter().collect();
    rows.sort_by(|(left_name, left), (right_name, right)| {
        right
            .dollars
            .total_cmp(&left.dollars)
            .then_with(|| left_name.cmp(right_name))
    });
    rows.into_iter()
        .map(|(repository, priced)| {
            let mut row = priced_row_value(priced);
            row["repository"] = json!(
                repository
                    .as_deref()
                    .map(|repository| evidence::identifier_field(repository, redactor))
            );
            row
        })
        .collect()
}

/// The columns every priced row carries, whatever it is a row of.
fn priced_row_value(priced: &PricedTokens) -> Value {
    json!({
        "messages": priced.tokens.messages,
        "fast_messages": priced.fast_messages,
        "dollars": priced.dollars,
        "dollars_rendered": format::dollars(priced.dollars),
        "tokens": tally_value(&priced.tokens),
    })
}

/// What the prompt cache actually saved: the difference between reading tokens back and sending
/// them again.
///
/// `null` when nothing was read from the cache — an absent saving is a different statement from a
/// saving of zero, and a page showing `$0.00 saved` over a window with no cache reads would be
/// answering a question nobody asked.
fn caching_value(priced: &PricedTokens) -> Value {
    if priced.tokens.cache_read == 0 {
        return Value::Null;
    }
    json!({
        "read": json!({
            "tokens": priced.tokens.cache_read,
            "rendered": format::tokens(priced.tokens.cache_read),
        }),
        "read_dollars": priced.cache_read_dollars,
        "read_dollars_rendered": format::dollars(priced.cache_read_dollars),
        "at_input_rate": priced.cache_read_at_input_rate,
        "at_input_rate_rendered": format::dollars(priced.cache_read_at_input_rate),
        "saving": priced.cache_saving(),
        "saving_rendered": format::dollars(priced.cache_saving()),
        // The writes that filled it are already inside the total, so the saving above is the read
        // side alone and is not net of them. Carried so the page can say so rather than imply it.
        "write_5m": priced.tokens.cache_write_5m,
        "write_1h": priced.tokens.cache_write_1h,
    })
}

/// Copilot's rows: token volumes, and **nothing money-shaped**.
///
/// There is no `dollars` key here, no estimate, no credit equivalent, and no place a page could
/// find one — which is the whole of [`crate::cost::BillingSignal`]'s honesty rule, held on the wire
/// rather than in a renderer. Copilot records one `outputTokens` figure per assistant message and a
/// transcript does not say which of its two billing regimes the account was on, so `basis` says
/// `tokens-only` and the page labels the table with it.
fn copilot_value(totals: &CostTotals) -> Value {
    let mut rows: Vec<(&Option<String>, &CopilotTokens)> = totals.copilot.iter().collect();
    rows.sort_by(|(left_model, left), (right_model, right)| {
        right
            .output
            .cmp(&left.output)
            .then_with(|| left_model.cmp(right_model))
    });
    json!({
        "basis": "tokens-only",
        "rows": rows
            .into_iter()
            .map(|(model, volumes)| json!({
                "model": model.as_deref().map(format::identifier),
                "messages": volumes.messages,
                "output": volumes.output,
                "output_rendered": format::tokens(volumes.output),
            }))
            .collect::<Vec<_>>(),
    })
}

/// The window's small top-tier sessions, as the CLI's own section lists them.
///
/// The same three refusals the Markdown carries, carried onto the wire rather than restated on the
/// page: the floors are serialized beside the rows so a reader is told what "small" was measured
/// as, `sessions` is the denominator the flagged count is a share of, and `any` is false with a
/// null list when nothing cleared the floors — an absent flag, not a row of zeroes a page would
/// have to know to hide. Nothing here is a score, a rate, or a rank; the page has no more to say
/// about these sessions than the document does.
///
/// Money-shaped keys are correct here and forbidden two blocks away: these are claude-code sessions
/// priced at list, which is what [`crate::cost::PremiumFlag`] only ever admits. A Copilot session
/// has no rate and therefore no tier, so it cannot reach this list in any window, and the
/// copilot block's no-money rule is untouched by anything in it.
fn premium_value(premium: &PremiumFlag) -> Value {
    json!({
        "any": premium.any(),
        "sessions": premium.sessions,
        "floors": {
            "max_messages": PREMIUM_FLAG_MAX_MESSAGES,
            "max_output_tokens": PREMIUM_FLAG_MAX_OUTPUT_TOKENS,
            "max_output_tokens_rendered": format::tokens(PREMIUM_FLAG_MAX_OUTPUT_TOKENS),
        },
        "totals": premium.any().then(|| json!({
            "sessions": premium.flagged.len(),
            "dollars": premium.dollars(),
            "dollars_rendered": format::dollars(premium.dollars()),
            "output": premium.output(),
            "output_rendered": format::tokens(premium.output()),
            "messages": premium.messages(),
        })),
        // The same cut the Markdown section makes, at the same constant, so the page and the
        // document list the same sessions rather than two different handfuls — and so a window
        // that flagged hundreds cannot quietly grow the payload the whole page is one fetch of.
        // What was cut is stated rather than dropped, and the totals above count every one.
        "rows_hidden": premium.flagged.len().saturating_sub(PREMIUM_SESSIONS_LISTED),
        "rows": premium
            .flagged
            .iter()
            .take(PREMIUM_SESSIONS_LISTED)
            .map(|session| json!({
                "source_hash": format::identifier(&session.source_hash),
                "archived_at": session.archived_at.map(stamp),
                "models": session
                    .models
                    .iter()
                    .map(|model| format::identifier(model))
                    .collect::<Vec<_>>(),
                "messages": session.messages,
                "output": session.output,
                "output_rendered": format::tokens(session.output),
                "dollars": session.dollars,
                "dollars_rendered": format::dollars(session.dollars),
            }))
            .collect::<Vec<_>>(),
    })
}

/// Everything the fold counted and refused to turn into money, with the reason and the size — and
/// each reason kept apart from the others, because a section that merged them could not say whether
/// a gap was a placeholder, a missing price row, or a bug.
fn flagged_value(flagged: &Flagged) -> Value {
    json!({
        "any": flagged.any(),
        "synthetic": (flagged.synthetic.messages > 0).then(|| json!({
            "messages": flagged.synthetic.messages,
            "tokens": tally_value(&flagged.synthetic),
        })),
        "unpriced": flagged
            .unpriced
            .iter()
            .map(|(reason, tally)| json!({
                // Already clamped: `Unpriced::detail` takes the clamp rather than applying one
                // afterwards, so there is no version of this string with the archive's raw value in
                // it for this module to forget to clean up.
                "detail": reason.detail(format::identifier),
                "messages": tally.messages,
                "tokens": tally_value(tally),
            }))
            .collect::<Vec<_>>(),
        "untiered_cache_writes": (flagged.untiered_cache_writes > 0).then(|| json!({
            "tokens": flagged.untiered_cache_writes,
            "rendered": format::tokens(flagged.untiered_cache_writes),
            "messages": flagged.untiered_cache_write_messages,
        })),
        "undeduplicatable": flagged.undeduplicatable.any().then(|| json!({
            "records": flagged.undeduplicatable.records(),
            "without_a_message_id": flagged.undeduplicatable.without_a_message_id,
            "past_the_id_cap": flagged.undeduplicatable.past_the_id_cap,
            "tokens": flagged.undeduplicatable.tokens,
            "rendered": format::tokens(flagged.undeduplicatable.tokens),
        })),
    })
}

/// One session as the standup fold left it: **already scrubbed, and not scrubbed again here**.
///
/// Every string below came out of [`crate::standup::Standup::fold`], which ran the redactor into
/// [`StandupSession`]. A second pass here would be a second posture to keep in step with the first
/// and would double-count what fired; what this module owes instead is to serialize exactly what the
/// fold produced, markers and all, so what a reader sees on the page is what the CLI would print.
fn standup_session_value(session: &StandupSession) -> Value {
    json!({
        "source_hash": session.source_hash,
        "archived_at": session.archived_at.map(stamp),
        "branch": session.branch,
        "title": session.title,
        "goal": session.goal,
        "work_completed": session.work_completed,
        "decisions": session.decisions,
        "open_items": session.open_items,
    })
}

/// A rolled-up list across the whole window, each line attributed to the repository it came out of.
fn rolled_up_value(lines: &[RolledUp]) -> Value {
    lines
        .iter()
        .map(|line| json!({ "repository": line.repository, "text": line.text }))
        .collect()
}

/// Sessions that contributed nothing, grouped by reason. The same shape all three lanes' gaps take,
/// because they are the same [`SkippedNote`].
fn gaps_value(notes: &[SkippedNote]) -> Value {
    notes
        .iter()
        .map(|note| json!({ "count": note.count, "reason": note.reason }))
        .collect()
}

/// What a scrub fired, as counts against pattern ids and nothing else.
///
/// The type has nothing else to render: a [`RedactionReport`] cannot carry what it matched, which is
/// qanungo #8's counts-only invariant held by construction rather than by this function's restraint.
fn redaction_value(report: &RedactionReport) -> Value {
    json!({
        "total": report.total(),
        "fired": report
            .fired()
            .map(|(pattern, count)| json!({ "pattern": pattern.as_str(), "count": count }))
            .collect::<Vec<_>>(),
    })
}

/// How much of the window each harness contributed, so a reader can weigh a per-harness score
/// against the sample behind it.
///
/// Harness labels are the *archive's* strings, so they are clamped **and scrubbed** on the way out
/// — [`evidence::identifier_field`], the same treatment and the same ordering the anchor's tool
/// name gets. A manifest states whatever it likes, and this label is no longer only a table
/// heading: it is the text of an option in the scope control and the key the page matches an
/// evidence tag against, so it gets the treatment a rendering control gets. Every surface that
/// spells a harness in this payload spells it this way, because a control and the rows it narrows
/// disagreeing about a label is how one harness becomes two.
fn sessions_value(folded: &Folded, redactor: &Redactor) -> Value {
    let totals = Totals::fold(&folded.sessions);
    let by_harness: BTreeMap<String, usize> = totals
        .by_agent
        .iter()
        .map(|(agent, count)| (evidence::identifier_field(agent, redactor), *count))
        .collect();
    json!({
        "folded": folded.instrumentation.sessions_folded,
        "comparison_folded": folded.instrumentation.comparison_sessions_folded,
        "by_harness": by_harness,
    })
}

/// One practice lane: the fleet number, and the per-harness split behind it.
///
/// The three states a lane can be in are kept apart here exactly as the report's table keeps them
/// apart, because collapsing any two of them is how a score becomes a lie. `scored` is a reading;
/// `no-reading` is a fed lane whose signals were all silent this window; `not-scored` is a lane
/// nothing types a signal for at all, and it carries the sentence naming the pull that would light
/// it up. None of the three is ever a zero.
fn lane_value(
    lane: Lane,
    now: &Scorecard,
    before: Option<&Scorecard>,
    columns: &[String],
    redactor: &Redactor,
) -> Value {
    let fleet = match now.fleet(lane) {
        Some(blend) => {
            let comparable = blend.comparable(before.and_then(|card| card.fleet(lane)));
            json!({
                "state": "scored",
                "score": blend.score,
                "harnesses": blend
                    .harnesses
                    .iter()
                    .map(|agent| evidence::identifier_field(agent, redactor))
                    .collect::<Vec<_>>(),
                "trend": trend_value(Trend::between(blend.score, comparable)),
            })
        }
        None if lane.untyped().is_some() => json!({ "state": "not-scored" }),
        None => json!({ "state": "no-reading" }),
    };
    json!({
        "key": lane.key(),
        "title": lane.title(),
        "reason": lane.untyped(),
        "fleet": fleet,
        "harnesses": columns
            .iter()
            .map(|column| harness_value(lane, column, now, before, redactor))
            .collect::<Vec<_>>(),
    })
}

/// One harness's standing in one lane.
///
/// `no-sessions` is its own state rather than a missing entry: the columns are the union of both
/// windows' harnesses, so a harness that stopped appearing is a fact the page shows instead of one
/// it hides — the same reason [`report::harness_columns`] takes the union in the first place.
fn harness_value(
    lane: Lane,
    source_agent: &str,
    now: &Scorecard,
    before: Option<&Scorecard>,
    redactor: &Redactor,
) -> Value {
    let label = evidence::identifier_field(source_agent, redactor);
    let Some(harness) = now.harness(source_agent) else {
        return json!({
            "source_agent": label,
            "sessions": 0,
            "state": "no-sessions",
        });
    };
    let score = harness.lane(lane);
    let earlier = before
        .and_then(|card| card.harness(source_agent))
        .and_then(|harness| harness.lane(lane).score());
    let (state, value, trend) = match score {
        LaneScore::Scored { score, .. } => (
            "scored",
            Some(*score),
            trend_value(Trend::between(*score, earlier)),
        ),
        LaneScore::NoReading { .. } => ("no-reading", None, Value::Null),
        LaneScore::Untyped(_) => ("not-scored", None, Value::Null),
    };
    json!({
        "source_agent": label,
        "sessions": harness.sessions,
        "state": state,
        "score": value,
        "trend": trend,
        "components": score
            .components()
            .iter()
            .map(|component| json!({
                "label": component.label,
                "detail": component.detail,
                "cost": component.cost,
            }))
            .collect::<Vec<_>>(),
    })
}

/// A movement, or the explicit absence of one. `null` is the only honest answer when the
/// comparison window could not measure the lane — see [`Trend::between`].
fn trend_value(trend: Option<Trend>) -> Value {
    match trend {
        Some(trend) => json!({
            "direction": trend.direction().key(),
            "glyph": trend.direction().glyph(),
            "points": trend.magnitude(),
            "was": trend.was,
        }),
        None => Value::Null,
    }
}

/// One finding: the rule that fired, the report's own Problem and Action wording, how many sessions
/// it fired on, the hashes of those sessions, and — per session — the evidence its rule can
/// honestly offer.
///
/// The Problem and Action strings are lifted from [`crate::rules`] rather than re-worded for the
/// web, so the page and the CLI give the same advice in the same sentences. The per-session
/// evidence *detail* lines the Markdown carries are deliberately still not here: the page shows the
/// counted events themselves, which is a better answer than a sentence about them.
///
/// `evidence_kind` is the rule's own statement about what kind of evidence it has
/// ([`crate::rules::RuleId::evidence_kind`]), and the page renders it rather than deciding for
/// itself: anchors for a rule that counted events, timestamps and counts for one that measured a
/// shape, and both for fire-and-forget, which did each in a different component.
/// Which scope each folded session belongs to, by `source_hash`: the rendered repository label, the
/// rendered device label, and the rendered harness label, and nothing else.
///
/// The **same three strings** the scopes section groups by and the lane columns are keyed on, so the
/// page can narrow a finding list it already holds by comparing labels rather than by recomputing
/// anything. That equality is the contract: a tag is a claim about which scope's numbers a row is
/// counted in, and it is pinned by a test that reconciles the tags against the per-scope fire
/// counts serialized beside them.
type ScopeTags = BTreeMap<String, ScopeTag>;

/// One session's scope membership, as the wire spells it. The device label is the second primary
/// axis; the harness label is the free sub-axis both primary axes carry.
struct ScopeTag {
    repository: String,
    device: String,
    harness: String,
}

impl Payload<'_> {
    /// Tags every session of the reported coaching window.
    ///
    /// The comparison window is deliberately not in here. Nothing on the page cites a comparison
    /// session — it exists to produce an earlier score and nothing else — and a tag for a row that
    /// cannot be rendered would be an index into a list that does not exist.
    fn scope_tags(&self) -> ScopeTags {
        self.coaching
            .sessions
            .iter()
            .map(|session| {
                (
                    session.source_hash.clone(),
                    ScopeTag {
                        repository: scopes::repository_label(
                            session.repository.as_deref(),
                            self.redactor,
                        ),
                        device: scopes::device_label(session.hostname.as_deref(), self.redactor),
                        harness: evidence::identifier_field(&session.source_agent, self.redactor),
                    },
                )
            })
            .collect()
    }
}

fn finding_value(
    finding: &Finding,
    by_hash: &BTreeMap<&str, &SessionMetrics>,
    tags: &ScopeTags,
    redactor: &Redactor,
) -> Value {
    let kind = finding.rule.evidence_kind();
    json!({
        "rule": finding.rule.key(),
        "title": finding.rule.title(),
        "problem": finding.problem,
        "action": finding.action,
        "sessions_affected": finding.evidence.len(),
        "evidence_kind": kind.key(),
        "source_hashes": finding
            .evidence
            .iter()
            .map(|evidence| evidence.source_hash.clone())
            .collect::<Vec<_>>(),
        "evidence": finding
            .evidence
            .iter()
            .map(|evidence| json!({
                "source_hash": evidence.source_hash,
                // Which scope this cited session is in, so the page can narrow the list without a
                // second request and without any scoring of its own. Identifiers, not content:
                // the repository the archive projected onto the session's snapshot, the device its
                // manifest recorded, and the harness label, each rendered exactly as the scopes
                // section renders it — so selecting a device narrows this list the same way a
                // repository does.
                "repository": tags
                    .get(evidence.source_hash.as_str())
                    .map(|tag| tag.repository.clone()),
                "device": tags
                    .get(evidence.source_hash.as_str())
                    .map(|tag| tag.device.clone()),
                "harness": tags
                    .get(evidence.source_hash.as_str())
                    .map(|tag| tag.harness.clone()),
                "anchors": if kind.anchors() {
                    evidence
                        .anchors
                        .iter()
                        .map(|anchor| anchor_value(anchor, redactor))
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                },
                "structural": if kind.structural() {
                    by_hash
                        .get(evidence.source_hash.as_str())
                        .map(|session| structural_value(session))
                } else {
                    None
                },
            }))
            .collect::<Vec<_>>(),
    })
}

/// One anchor: where a counted event is, and nothing about what it said.
///
/// The tool name is clamped **and scrubbed** — [`evidence::identifier_field`], which is where the
/// clamp-then-scrub order is argued. Decision 9 blessed tool names as schema metadata a surface may
/// render verbatim, and that still holds for the aggregate lines this payload is otherwise made of.
/// It stops holding here: a harness writes its own tool names, an anchor is the label on the control
/// that expands into transcript text, and a tool name shaped like a credential is a credential on
/// the screen. Scrubbing a name nobody would mistake for a secret costs exactly nothing, so the
/// verbatim surfaces pay it.
fn anchor_value(anchor: &EventAnchor, redactor: &Redactor) -> Value {
    json!({
        "locator": anchor.locator,
        "record": anchor.record,
        "line": anchor.line,
        "at": anchor.at.map(stamp),
        "tool": anchor
            .tool
            .as_deref()
            .map(|tool| evidence::identifier_field(tool, redactor)),
    })
}

/// The structural evidence of a session-shaped finding: when the work happened and how much of it
/// there was. **Timestamps and numbers only** — there is no string in here at all, which is a
/// stronger statement than "it is scrubbed".
///
/// Durations arrive pre-rendered beside their raw seconds for the same reason the provenance block
/// does it: [`crate::format`] owns how a span reads, and a second implementation of that in
/// JavaScript would drift from the report the page claims to be a view of.
fn structural_value(session: &SessionMetrics) -> Value {
    let span = |delta: Option<chrono::TimeDelta>| {
        json!({
            "rendered": delta.map(format::span),
            "seconds": delta.map(|delta| delta.num_seconds().max(0)),
        })
    };
    json!({
        "active": span(session.active_time()),
        "span": span(session.span()),
        "longest_sitting": span(session.longest_sitting()),
        "sittings": session.sittings(),
        "first_record": session.summary.first_timestamp.map(stamp),
        "last_record": session.summary.last_timestamp.map(stamp),
        "user_requests": session.summary.user_requests,
        "assistant_messages": session.summary.assistant_messages,
        "tool_activities": session.summary.tool_activities,
        "boundaries": session
            .activity
            .sitting_boundaries()
            .iter()
            .map(|sitting| json!({
                "from": stamp(sitting.from),
                "to": stamp(sitting.to),
                "seconds": sitting.span().num_seconds().max(0),
                "rendered": format::span(sitting.span()),
            }))
            .collect::<Vec<_>>(),
        "boundaries_elided": session.activity.sittings_elided(),
    })
}

// ---------------------------------------------------------------------------
// The ask answer
// ---------------------------------------------------------------------------

/// One answer to one `GET /api/ask` — the only document on this surface a *request* decides.
///
/// # It is still not a fold behind a request
///
/// Everything above is served from a payload built once per refresh, and the reason is stated in
/// this module's own docs: a served document that varied by who asked could show two readers
/// different numbers under one generation stamp, and a fold behind a request an unauthenticated peer
/// controls is a remote control for somebody else's compute. Neither objection reaches this route,
/// and it is worth saying exactly why rather than treating the rule as bent.
///
/// - **It induces no archive traffic.** The corpus is [`crate::command::AskCorpus`], mirrored and
///   parsed on the service's own refresh timer. A request scores an in-memory `Vec`; there is no
///   path from here to Patwari, which is the evidence route's iron rule applied to a second route
///   rather than an exception carved out of it.
/// - **It states nothing about the archive that the page does not.** A ranking is a *selection* of
///   the summaries, not a number; two readers asking different questions of the same corpus at the
///   same generation are not being told different things about it. The answer carries the
///   generation it was taken at, so a reader can see which corpus answered.
/// - **It is bounded before it is work.** The raw query is capped and the limit clamped by the
///   router before anything is parsed or scored — see [`crate::dashboard_server`].
///
/// # No verbatim on this surface
///
/// `qanungo ask --verbatim` escalates a ranking into the shown hits' *transcripts*. That is a CLI
/// affordance and stays one (decision 11: the recall funnel is where the user's own shell already
/// has raw access). This route serves what the summaries say and the `source_hash` to redeem for the
/// rest, and it cannot be made to fetch a transcript — not because a check refuses it, but because
/// the corpus it reads holds no transcript at all.
///
/// # The scrub is [`Ask::fold`]'s
///
/// Every string below came out of the ranking already scrubbed by the launch-time redactor, exactly
/// as the CLI's document does: the snippet through the shared scrub-collapse-clip pipeline, the
/// title scrubbed, the repository and branch scrubbed and clamped. This module holds no pre-scrub
/// string to serialize by mistake, which is the same guarantee the standup section rests on.
///
/// The one string that is not the archive's is the query, and it is not echoed raw: what comes back
/// is [`Query::terms`] — the words the search actually used, which [`Query::parse`] has already
/// lower-cased and broken on everything that is not a letter or a digit. So a caller's own bytes
/// never make the round trip, and what does has no punctuation, no whitespace, and no control
/// character in it by construction rather than by filtering.
///
/// Length is the one dimension `parse` does not bound: inside the router's 1 KiB cap a single
/// alphanumeric run is one very long "word". So the echo passes through
/// [`format::identifier`] — the clamp every other label on this surface takes — which is a no-op on
/// every real term and replaces an absurd one wholesale rather than putting a kilobyte of a peer's
/// choosing on a rendering surface. The clamp is deliberately *not* pushed into `Query::parse`: that
/// would change what `qanungo ask` searches for, and this is a rendering bound, not a search rule.
pub struct AskAnswer<'a> {
    /// The parsed query. Its terms are what the answer echoes; the raw `q` is not on the wire.
    pub query: &'a Query,
    /// The limit actually applied, after the router clamped it — served so a truncated ranking says
    /// what truncated it.
    pub limit: usize,
    /// The ranking, or `None` when the query had no searchable word in it and nothing was scored.
    pub ask: Option<&'a Ask>,
    pub corpus: &'a AskCorpus,
    /// Which refresh published the corpus, and whether refreshing it is currently failing — the
    /// same two facts the page's own staleness banner is drawn from.
    pub refreshed: Refreshed,
    pub redactor: &'a Redactor,
}

impl AskAnswer<'_> {
    /// Builds the served JSON answer.
    pub fn build(&self) -> Value {
        let (searched, unsearchable) = match self.ask {
            // Equal to the corpus's own two counts by construction — `Ask::fold` is handed exactly
            // this corpus — and read off the ranking so that what the answer states is what the
            // ranking was taken over rather than a second count of the same thing.
            Some(ask) => (ask.searched, ask.unsearchable),
            None => (self.corpus.searchable(), self.corpus.unsearchable),
        };
        json!({
            "state": self.state(),
            "query": {
                "terms": self
                    .query
                    .terms()
                    .iter()
                    .map(|term| format::identifier(term))
                    .collect::<Vec<_>>(),
                // Why a word a reader typed may be missing above, in the answer rather than only in
                // the page's copy: the two rules that drop one are this crate's and a reader cannot
                // check a ranking against a rubric they were not told.
                "min_term_chars": crate::ask::MIN_TERM_CHARS,
            },
            "limit": self.limit,
            "searched": searched,
            "unsearchable": unsearchable,
            "total_matches": self.ask.map_or(0, |ask| ask.total_matches),
            "hits": self
                .ask
                .map(|ask| {
                    ask.hits
                        .iter()
                        .enumerate()
                        .map(|(rank, hit)| ask_hit_value(rank + 1, hit))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            "corpus": {
                "generation": self.refreshed.generation,
                "read_at": stamp(self.corpus.generated_at),
                "stale_since": self.refreshed.stale_since.map(stamp),
                "sessions_listed": self.corpus.listed(),
                "bytes_read": format::bytes(self.corpus.bytes_read),
                // What the search covers, said in the answer rather than assumed by the page. All
                // of history and no scope: `ask` asks a lifetime question (decision 12), and this
                // route is deliberately independent of the page's repository, device, and harness
                // controls — none of them is a parameter here and none narrows this list.
                "scope": ASK_SCOPE,
            },
            // The posture behind the snippets, on the same terms every other verbatim surface in
            // this crate states it: launch-time, identical for every reader, never a query string.
            "redaction": {
                "secrets": self.redactor.redacts_secrets(),
                "profanity": self.redactor.filters_profanity(),
                "pattern_revision": PATTERN_REVISION,
                "total": self.ask.map_or(0, |ask| ask.redaction.total()),
                "fired": self
                    .ask
                    .map(|ask| {
                        ask.redaction
                            .fired()
                            .map(|(pattern, count)| {
                                json!({ "pattern": pattern.as_str(), "count": count })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            },
        })
    }

    /// Which of the three answers this is.
    ///
    /// Three states rather than an empty list, because they are three different sentences and a
    /// page that collapsed them would answer a question nobody asked. `no-searchable-terms` is "you
    /// gave me no word to search on"; `no-matches` is the archive's own "no", which is the answer a
    /// person asking *have I ever done this* came for; `ranked` is a list. The CLI's document makes
    /// exactly the same three-way split in prose ([`crate::ask_report`]).
    fn state(&self) -> &'static str {
        match self.ask {
            None => "no-searchable-terms",
            Some(ask) if ask.total_matches == 0 => "no-matches",
            Some(_) => "ranked",
        }
    }
}

/// What this route searches, stated in one word wherever it is stated. All of history, and cut by no
/// scope: see [`AskAnswer`].
const ASK_SCOPE: &str = "all-history";

/// One ranked hit: where the session is, why it ranked, and the one line that shows it.
///
/// Everything here was scrubbed on the way into the hit by [`Ask::fold`] and is serialized exactly as
/// that fold produced it — the standup section's discipline, for the same reason: re-scrubbing would
/// be a second posture to keep in step with the first and would double-count what fired.
///
/// `source_hash` is a citation and **not a link**. The page renders it as selectable text; there is
/// no `href` anywhere on that page and there will not be one, because Patwari serves unredacted
/// blobs (decision 11).
fn ask_hit_value(rank: usize, hit: &crate::ask::AskHit) -> Value {
    json!({
        "rank": rank,
        "title": hit.title,
        // A closed enum this build owns, never archive free text — so it is the one label here that
        // needs no scrub. See `crate::ask::AskHit::harness`.
        "harness": hit.harness,
        "repository": hit.repository,
        "branch": hit.branch,
        "archived_at": hit.archived_at.map(stamp),
        "score": hit.score,
        "source_hash": hit.source_hash,
        "snippet": hit.snippet,
        "matched": hit.matched,
    })
}

/// What one lane's fold cost, in the footer's own quantities and the footer's own renderings.
///
/// Three lanes now, so the shape is a function rather than three copies of the same six keys —
/// which also means a lane cannot come to report its sync in seconds while another reports it in
/// milliseconds. Durations and byte counts arrive **pre-rendered** by [`crate::format`] alongside
/// their raw values, because those renderings are that module's job and a second implementation of
/// them in JavaScript would drift from the CLI footers this block mirrors.
fn lane_cost_value(
    window: &Window,
    sync: &crate::sync::SyncStats,
    fold_elapsed: Duration,
    sessions_folded: usize,
    bytes: u64,
) -> Value {
    json!({
        "window": window.to_string(),
        "sessions_listed": sync.sessions_listed,
        "sessions_folded": sessions_folded,
        "fold": format::elapsed(fold_elapsed),
        "fold_millis": u64::try_from(fold_elapsed.as_millis()).unwrap_or(u64::MAX),
        "sync": format::elapsed(sync.elapsed),
        "sync_millis": u64::try_from(sync.elapsed.as_millis()).unwrap_or(u64::MAX),
        "bytes_folded": format::bytes(bytes),
        "bytes_transferred": format::bytes(sync.bytes_transferred),
        "cache_hits": sync.cache_hits,
        "cache_misses": sync.cache_misses,
        "snapshots_indexed": sync.snapshots_indexed,
        "snapshots_fetched": sync.snapshots_fetched,
    })
}

/// What the ask corpus cost, in the same quantities and the same renderings as the other lanes —
/// with the two differences that are the lane's own identity rather than a divergence in style.
///
/// It names a **scope** rather than a window, because it has none: a lifetime question has no
/// `--last` (decision 12), and printing `all history` where a duration goes would invite a reader to
/// read it as one. And it reports what it holds *searchable* beside what the archive *listed*,
/// because those two are the whole of the corpus's honesty: every session listed either answers a
/// search or is counted as one this build could not read, and the answers themselves carry the same
/// pair so a reader can check a footer against a search.
fn ask_lane_value(corpus: &AskCorpus) -> Value {
    let sync = &corpus.instrumentation.sync;
    let fold_elapsed = corpus.instrumentation.fold_elapsed;
    json!({
        "scope": ASK_SCOPE,
        "sessions_listed": sync.sessions_listed,
        "sessions_searchable": corpus.searchable(),
        "sessions_unsearchable": corpus.unsearchable,
        "fold": format::elapsed(fold_elapsed),
        "fold_millis": u64::try_from(fold_elapsed.as_millis()).unwrap_or(u64::MAX),
        "sync": format::elapsed(sync.elapsed),
        "sync_millis": u64::try_from(sync.elapsed.as_millis()).unwrap_or(u64::MAX),
        "bytes_read": format::bytes(corpus.bytes_read),
        "bytes_transferred": format::bytes(sync.bytes_transferred),
        "cache_hits": sync.cache_hits,
        "cache_misses": sync.cache_misses,
        "snapshots_indexed": sync.snapshots_indexed,
        "snapshots_fetched": sync.snapshots_fetched,
    })
}

impl Payload<'_> {
    /// What the numbers cost and what they may be compared against.
    ///
    /// The instrumentation footer of every CLI run — now three of them, one per lane — plus the
    /// three facts only a long-lived process has: which refresh this is, when it landed, and
    /// whether the last attempt failed.
    ///
    /// The coaching lane's quantities stay at the top level of this block rather than moving under
    /// `lanes.coaching`, for the same reason its section did: a footer a reader already knows how to
    /// read should not be relocated to buy symmetry. The other two arrive beside it under
    /// [`lane_cost_value`], and every one of the three names the window it was taken over, because
    /// three folds over three spans in one document is exactly where an unlabelled number becomes a
    /// wrong one.
    /// `timeline` and `heatmap` are the sections [`Payload::timeline_section`] and
    /// [`Payload::heatmap_section`] already built, handed in rather than rebuilt: the footer quotes
    /// a few of their figures, and a footer that recomputed them could come to quote a different
    /// count from the one drawn above it.
    fn provenance(&self, timeline: &Value, heatmap: &Value) -> Value {
        let instrumentation = &self.coaching.instrumentation;
        let cost = &self.cost.instrumentation;
        let standup = &self.standup.instrumentation;
        // The cost lane's own two extras, added to the shared shape rather than given a second
        // near-identical literal: it folds a window *pair*, and it counts records where the others
        // count bytes.
        let mut cost_lane = lane_cost_value(
            &self.windows.cost,
            &cost.sync,
            cost.fold_elapsed,
            cost.sessions_folded,
            cost.bytes_folded,
        );
        cost_lane["comparison_sessions_folded"] = json!(cost.comparison_sessions_folded);
        cost_lane["records_read"] = json!(cost.records_read);
        json!({
            "window": self.windows.coaching.to_string(),
            "sessions_listed": instrumentation.sync.sessions_listed,
            "sessions_folded": instrumentation.sessions_folded,
            "comparison_sessions_folded": instrumentation.comparison_sessions_folded,
            "fold": format::elapsed(instrumentation.fold_elapsed),
            "fold_millis": u64::try_from(instrumentation.fold_elapsed.as_millis())
                .unwrap_or(u64::MAX),
            "sync": format::elapsed(instrumentation.sync.elapsed),
            "sync_millis": u64::try_from(instrumentation.sync.elapsed.as_millis())
                .unwrap_or(u64::MAX),
            "bytes_folded": format::bytes(instrumentation.bytes_folded),
            "bytes_transferred": format::bytes(instrumentation.sync.bytes_transferred),
            "cache_hits": instrumentation.sync.cache_hits,
            "cache_misses": instrumentation.sync.cache_misses,
            "snapshots_indexed": instrumentation.sync.snapshots_indexed,
            "snapshots_fetched": instrumentation.sync.snapshots_fetched,
            // The two windows the slice added, each with what folding it cost. The cost lane folds a
            // pair — its own window and the equal-length one before it — and the standup lane folds
            // one, so `comparison_sessions_folded` appears on the first and not on the second.
            "cost_window": self.windows.cost.to_string(),
            "standup_window": self.windows.standup.to_string(),
            // The fourth lane names a scope rather than a window: the ask corpus is all of history
            // by design, so there is no `--ask-last` to echo and a duration here would be a wrong
            // one. See `ask_lane_value`.
            "ask_scope": ASK_SCOPE,
            "lanes": {
                "cost": cost_lane,
                "standup": lane_cost_value(
                    &self.windows.standup,
                    &standup.sync,
                    standup.fold_elapsed,
                    self.standup.standup.sessions,
                    self.standup.standup.bytes_read,
                ),
                "ask": ask_lane_value(self.ask),
            },
            // What a refresh of the whole page costs, wall-clock, across all three folds. Measured
            // rather than added — though against production it lands within a tenth of a second of
            // the sum of the three lanes above (45.4 s warm), because the shared blob cache spares
            // the *bytes* and not the requests: `crate::sync` asks the archive for one snapshot
            // document per listed session before it ever consults the cache. It is measured because
            // whether it equals that sum is a fact about the mirror that can change.
            "refresh_elapsed": format::elapsed(self.folds_elapsed),
            "refresh_elapsed_millis": u64::try_from(self.folds_elapsed.as_millis())
                .unwrap_or(u64::MAX),
            "rule_pack": instrumentation.rule_pack.stamp(),
            "rule_pack_digest": instrumentation.rule_pack.digest(),
            // Beside the rule pack, and for the same reason it is beside it in the cost report's own
            // footer: two windows are comparable only when the table that priced them matches, and
            // a page that showed a delta without saying which revision drew it would be inviting a
            // comparison it cannot support.
            "price_table_revision": PRICE_TABLE_REVISION,
            "redaction_pattern_revision": PATTERN_REVISION,
            // Text, never a link. See the module docs: Patwari serves unredacted blobs, so a browser
            // deep-link into it is a transcript disclosure wearing a convenience.
            "patwari_url": instrumentation.patwari_url,
            "cache_root": instrumentation.cache_root.display().to_string(),
            "refresh_interval": self.refresh.to_string(),
            "refreshed_at": stamp(self.refreshed.at),
            "generation": self.refreshed.generation,
            "stale_since": self.refreshed.stale_since.map(stamp),
            "gaps": gaps_value(&self.coaching.skipped),
            // What the timeline is a statement about, stated where every other basis on this page
            // is stated. `days_covered` is days a session actually landed on, not the length of the
            // window: a fortnight with four working days in it covered four days, and a footer that
            // said fourteen would be quoting the calendar rather than the archive. `basis` names the
            // clock in one word so a reader of the raw payload does not have to infer it from a
            // module comment they cannot see — and names it as **archive time in UTC**, the clock
            // that reconciles, as against the heatmap's local one below.
            "timeline": {
                "basis": "archive-completion-utc",
                "days_covered": timeline["days_covered"],
                "comparison_days_covered": timeline["comparison_days_covered"],
                "undated": timeline["undated"],
            },
            // The heatmap's own basis, in the same spot and the same one-word register. It is
            // **local time off the transcript's own first-activity instant** — a different clock
            // from the timeline's archive-UTC, which is the whole reason both views exist. Its
            // provenance figures are the two ways a session leaves the grid: no recorded offset, or
            // an offset but no readable activity time. Both should be near-zero on today's archive
            // and both are served rather than assumed, so a reader can see why the cells are a few
            // short of the count above them.
            "heatmap": {
                "basis": "first-activity-local",
                "cells_covered": heatmap["cells_covered"],
                "no_offset": heatmap["no_offset"],
                "undated": heatmap["undated"],
            },
            // A machine-checkable statement of this *surface's* contract. It was already true of
            // the excerpt route; the standup section makes it true of the document itself, which is
            // a stronger claim and the one this flag now stands for. The block below says what
            // scrub stands behind it — the whole of qanungo #8's standing rule for a rendering
            // surface.
            "renders_verbatim": true,
            "redaction": {
                // Launch-time, identical for every reader, and never a query string: a redaction
                // control a browser could flip is a redaction bypass with a nicer name.
                "secrets": self.redactor.redacts_secrets(),
                "profanity": self.redactor.filters_profanity(),
                "pattern_revision": PATTERN_REVISION,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::TimeDelta;
    use clap::Parser;
    use munshi_transcript::SessionSummary;

    use super::*;
    use crate::cli::{Cli, Command};
    use crate::cost::SessionCost;
    use crate::cost_report::CostInstrumentation;
    use crate::evidence::SessionAnchors;
    use crate::metrics::{Activity, CommandChurn, Compactions, ReviewActivity, ToolOutcomes};
    use crate::report::Instrumentation;
    use crate::rules;
    use crate::scoring::RulePack;
    use crate::standup::{ReadSummary, RepositoryGroup, Standup};
    use crate::standup_report::StandupInstrumentation;
    use crate::sync::SyncStats;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn window(spelling: &str) -> Window {
        let Command::Dashboard(args) = Cli::parse_from([
            "qanungo",
            "dashboard",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--last",
            spelling,
        ])
        .command
        else {
            panic!("`dashboard` parses as the dashboard command");
        };
        args.last
    }

    fn refresh() -> Refresh {
        let Command::Dashboard(args) = Cli::parse_from([
            "qanungo",
            "dashboard",
            "--patwari-url",
            "http://127.0.0.1:8080",
        ])
        .command
        else {
            panic!("`dashboard` parses as the dashboard command");
        };
        args.refresh
    }

    /// The same `hygiene_window` shape the scoring and report tests use: `count` sessions, the
    /// first `marathons` of which work one long unbroken push.
    fn hygiene_window(source_agent: &str, count: usize, marathons: usize) -> Vec<SessionMetrics> {
        let first = at("2026-08-10T09:00:00Z");
        let step = crate::rules::thresholds::IDLE_GAP;
        let marathon = crate::rules::thresholds::MARATHON_SITTING_ACTIVE;
        (0..count)
            .map(|index| {
                let worked = if index < marathons {
                    marathon + step
                } else {
                    marathon / 4
                };
                let steps = worked.num_minutes() / step.num_minutes();
                let timestamps: Vec<_> = (0..=steps).map(|n| first + step * n as i32).collect();
                let last = *timestamps.last().expect("at least the first record");
                SessionMetrics {
                    source_hash: format!("{index:02x}").repeat(32),
                    source_agent: source_agent.to_owned(),
                    repository: None,
                    // Spread across five UTC days so the timeline section has a calendar to lay
                    // them on. Archive time, which is a different clock from the `first` above —
                    // every session here starts its transcript on the same instant, and the
                    // timeline still draws five bars.
                    archived_at: Some(
                        at("2026-08-11T09:00:00Z") + TimeDelta::days(index as i64 % 5),
                    ),
                    hostname: None,
                    utc_offset: None,
                    artifact_set_version: 2,
                    summary: SessionSummary {
                        user_requests: 4,
                        tool_activities: 20,
                        first_timestamp: Some(first),
                        last_timestamp: Some(last),
                        ..SessionSummary::default()
                    },
                    tools: ToolOutcomes::default(),
                    activity: Activity::over(timestamps),
                    commands: CommandChurn::default(),
                    compactions: Compactions {
                        observable: true,
                        ..Compactions::default()
                    },
                    reviews: ReviewActivity::default(),
                    anchors: SessionAnchors::default(),
                    bytes_folded: 1024,
                }
            })
            .collect()
    }

    fn folded(sessions: Vec<SessionMetrics>, previous: Vec<SessionMetrics>) -> Folded {
        let findings = rules::evaluate(&sessions);
        Folded {
            generated_at: at("2026-08-17T12:00:00Z"),
            instrumentation: Instrumentation {
                sync: SyncStats {
                    sessions_listed: sessions.len() + previous.len(),
                    cache_hits: 2,
                    cache_misses: 1,
                    snapshots_indexed: 2,
                    snapshots_fetched: 1,
                    bytes_transferred: 4096,
                    elapsed: Duration::from_millis(120),
                },
                fold_elapsed: Duration::from_millis(7),
                sessions_folded: sessions.len(),
                comparison_sessions_folded: previous.len(),
                bytes_folded: 8192,
                rule_pack: RulePack::current(),
                patwari_url: "http://127.0.0.1:8080".to_owned(),
                cache_root: PathBuf::from("/tmp/qanungo"),
            },
            compared: true,
            sessions,
            previous,
            findings,
            skipped: Vec::new(),
        }
    }

    fn refreshed() -> Refreshed {
        Refreshed {
            generation: 3,
            at: at("2026-08-17T12:00:00Z"),
            stale_since: None,
        }
    }

    fn windows() -> Windows {
        Windows {
            coaching: window("7d"),
            cost: cost_window("12w"),
            standup: standup_window("7d"),
        }
    }

    fn cost_window(spelling: &str) -> Window {
        let Command::Dashboard(args) = Cli::parse_from([
            "qanungo",
            "dashboard",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--cost-last",
            spelling,
        ])
        .command
        else {
            panic!("`dashboard` parses as the dashboard command");
        };
        args.cost_last
    }

    fn standup_window(spelling: &str) -> Window {
        let Command::Dashboard(args) = Cli::parse_from([
            "qanungo",
            "dashboard",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--standup-last",
            spelling,
        ])
        .command
        else {
            panic!("`dashboard` parses as the dashboard command");
        };
        args.standup_last
    }

    /// One claude-code session's worth of billing records, spelled as the 2.1.x envelope spells
    /// them — the same fixture shape `crate::cost`'s own tests use, so the numbers below can be read
    /// against the price table by eye.
    fn cost_session(
        model: &str,
        message_id: &str,
        usage: &str,
        repository: Option<&str>,
    ) -> SessionCost {
        let record = format!(
            r#"{{"type":"assistant","uuid":"{message_id}-r","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"assistant","id":"{message_id}","model":"{model}","content":[{{"type":"text","text":"x"}}],"usage":{usage}}}}}"#
        );
        SessionCost {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            repository: repository.map(ToOwned::to_owned),
            archived_at: Some(at("2026-08-10T00:00:00Z")),
            fold: crate::cost::fold_cost(
                munshi_transcript::Source::ClaudeCode,
                2,
                record.as_bytes(),
            )
            .expect("v2 is supported"),
            bytes_folded: 4096,
        }
    }

    fn cost_instrumentation() -> CostInstrumentation {
        CostInstrumentation {
            sync: SyncStats {
                sessions_listed: 3,
                cache_hits: 3,
                cache_misses: 0,
                snapshots_indexed: 3,
                snapshots_fetched: 0,
                bytes_transferred: 0,
                elapsed: Duration::from_millis(240),
            },
            fold_elapsed: Duration::from_millis(11),
            sessions_folded: 1,
            comparison_sessions_folded: 1,
            records_read: 12,
            bytes_folded: 4096,
            patwari_url: "http://127.0.0.1:8080".to_owned(),
            cache_root: PathBuf::from("/tmp/qanungo"),
        }
    }

    fn folded_cost(sessions: &[SessionCost], previous: Option<&[SessionCost]>) -> FoldedCost {
        FoldedCost {
            generated_at: at("2026-08-17T12:00:00Z"),
            totals: CostTotals::fold(sessions),
            previous: previous.map(CostTotals::fold),
            skipped: Vec::new(),
            instrumentation: cost_instrumentation(),
        }
    }

    /// A million output tokens of Opus 5 and a million cached reads, in one repository.
    fn priced_window() -> FoldedCost {
        folded_cost(
            &[cost_session(
                "claude-opus-5",
                "msg_1",
                r#"{"input_tokens":0,"output_tokens":1000000,"cache_read_input_tokens":1000000,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":100000}}"#,
                Some("surdy/qanungo"),
            )],
            None,
        )
    }

    fn standup_session(title: &str, goal: &str) -> StandupSession {
        StandupSession {
            source_hash: "a".repeat(64),
            archived_at: Some(at("2026-08-16T09:00:00Z")),
            branch: Some("main".to_owned()),
            title: title.to_owned(),
            goal: goal.to_owned(),
            work_completed: vec!["Split the fold from the document.".to_owned()],
            decisions: vec!["One payload, one generation.".to_owned()],
            open_items: vec!["Measure the refresh against production.".to_owned()],
        }
    }

    fn folded_standup(standup: Standup) -> FoldedStandup {
        FoldedStandup {
            generated_at: at("2026-08-17T12:00:00Z"),
            standup,
            instrumentation: StandupInstrumentation {
                sync: SyncStats {
                    sessions_listed: 2,
                    cache_hits: 1,
                    cache_misses: 1,
                    snapshots_indexed: 1,
                    snapshots_fetched: 1,
                    bytes_transferred: 2048,
                    elapsed: Duration::from_millis(45),
                },
                fold_elapsed: Duration::from_millis(2),
                redactor: Redactor::new(),
                patwari_url: "http://127.0.0.1:8080".to_owned(),
                cache_root: PathBuf::from("/tmp/qanungo"),
            },
        }
    }

    /// One narrated repository and one gap: enough shape for the section's own assertions, with the
    /// real grouping path covered end to end in `tests/dashboard.rs`.
    fn narrated_window() -> Standup {
        Standup {
            repositories: vec![RepositoryGroup {
                repository: "surdy/qanungo".to_owned(),
                sessions: vec![standup_session(
                    "Serve the standup and cost views",
                    "Present two folds that already ship.",
                )],
            }],
            decisions: vec![RolledUp {
                repository: "surdy/qanungo".to_owned(),
                text: "One payload, one generation.".to_owned(),
            }],
            open_items: vec![RolledUp {
                repository: "surdy/qanungo".to_owned(),
                text: "Measure the refresh against production.".to_owned(),
            }],
            gaps: vec![SkippedNote {
                count: 2,
                reason: "claude-code: munshi wrote a placeholder summary here".to_owned(),
            }],
            redaction: RedactionReport::default(),
            sessions: 1,
            bytes_read: 4096,
        }
    }

    /// The payload, from the four lanes a test chose — three folds and the search corpus, which
    /// feeds no section and appears only in the provenance block.
    fn build(coaching: Folded, cost: FoldedCost, standup: Standup, redactor: Redactor) -> Value {
        Payload {
            windows: &windows(),
            refresh: &refresh(),
            coaching: &coaching,
            cost: &cost,
            standup: &folded_standup(standup),
            ask: &corpus(Vec::new(), 0),
            folds_elapsed: Duration::from_millis(370),
            refreshed: refreshed(),
            redactor: &redactor,
        }
        .build()
    }

    /// The coaching lane's own assertions, over a payload whose other two sections are the smallest
    /// honest thing they can be.
    fn built(sessions: Vec<SessionMetrics>, previous: Vec<SessionMetrics>) -> Value {
        build(
            folded(sessions, previous),
            folded_cost(&[], None),
            Standup::default(),
            Redactor::new(),
        )
    }

    fn lane<'a>(payload: &'a Value, key: &str) -> &'a Value {
        payload["lanes"]
            .as_array()
            .expect("lanes is an array")
            .iter()
            .find(|lane| lane["key"] == key)
            .unwrap_or_else(|| panic!("{key} is a lane"))
    }

    /// Every lane keeps its place, in the order qanungo #4 names them, whether or not it scored.
    #[test]
    fn all_five_lanes_are_present_in_the_order_the_issue_names_them() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let keys: Vec<_> = payload["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|lane| lane["key"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            keys,
            vec![
                "prompt-quality",
                "session-hygiene",
                "code-review",
                "tool-mastery",
                "context-management",
            ]
        );
    }

    /// A scored lane carries the score, the per-harness split, and the readings that produced it —
    /// the same numbers the report's table and its "why the scores are what they are" section show.
    #[test]
    fn a_scored_lane_carries_its_score_and_the_readings_behind_it() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let hygiene = lane(&payload, "session-hygiene");
        assert_eq!(hygiene["fleet"]["state"], "scored");
        assert_eq!(hygiene["fleet"]["score"], 50);
        assert_eq!(hygiene["reason"], Value::Null);

        let harness = &hygiene["harnesses"][0];
        assert_eq!(harness["source_agent"], "claude-code");
        assert_eq!(harness["sessions"], 20);
        assert_eq!(harness["state"], "scored");
        assert_eq!(harness["score"], 50);
        assert_eq!(harness["components"][0]["label"], "Marathon session");
        assert_eq!(harness["components"][0]["cost"], 50.0);
        assert!(
            harness["components"][0]["detail"]
                .as_str()
                .unwrap()
                .starts_with("fired on 5 of 20"),
        );
    }

    /// The woken lane in the payload, both halves: the lane's own score, and the finding under it
    /// carrying `evidence_kind: "mixed"` with anchors on the commits.
    ///
    /// The `evidence_kind` assertion is the load-bearing one. The page renders anchors only where
    /// the rule says it has events to point at, and this rule half-does: the commits anchor, the
    /// missing review does not. A payload that claimed `structural` would hide real evidence, and
    /// one that claimed `event` would promise a locus for an absence.
    #[test]
    fn a_scored_code_review_lane_carries_its_score_and_a_mixed_evidence_kind() {
        let mut sessions = hygiene_window("claude-code", 20, 0);
        for (index, session) in sessions.iter_mut().enumerate() {
            session.reviews = ReviewActivity {
                observable: true,
                commits: 2,
                review_passes: u64::from(index < 4),
                skill_invocations: 2,
            };
            session.anchors.commits = vec![EventAnchor {
                locator: 3,
                record: 4,
                line: 4,
                at: None,
                tool: Some("Bash".to_owned()),
            }];
        }
        let payload = built(sessions, Vec::new());

        let review = lane(&payload, "code-review");
        assert_eq!(review["fleet"]["state"], "scored");
        assert_eq!(
            review["fleet"]["score"], 0,
            "16 of 20 is far past the floor"
        );
        assert_eq!(review["reason"], Value::Null);
        let harness = &review["harnesses"][0];
        assert_eq!(harness["components"][0]["label"], "Shipped without review");
        assert_eq!(harness["components"][0]["cost"], 100.0);
        assert!(
            harness["components"][0]["detail"]
                .as_str()
                .unwrap()
                .starts_with("fired on 16 of 20"),
        );

        let finding = payload["findings"]
            .as_array()
            .expect("findings are an array")
            .iter()
            .find(|finding| finding["rule"] == "unreviewed-ship")
            .expect("the rule fired");
        assert_eq!(finding["evidence_kind"], "mixed");
        assert_eq!(
            finding["evidence"][0]["anchors"]
                .as_array()
                .expect("the ship half anchors")
                .len(),
            1,
        );
    }

    /// The payload keeps the three lane states apart, and **`not-scored` is now unreachable**:
    /// every lane in the pack is typed, so a lane a harness cannot be read for serializes as
    /// `no-reading` with a null score and a null reason, like any other silent signal.
    ///
    /// This is the copilot shape of Code Review, asserted on the payload the page renders from — a
    /// reader of the JSON must be able to tell "nothing could look" from "nothing happened", and
    /// the component detail is where that distinction lives.
    #[test]
    fn a_lane_no_harness_could_look_at_serializes_as_no_reading() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let review = lane(&payload, "code-review");
        assert_eq!(review["fleet"]["state"], "no-reading");
        assert_eq!(review["fleet"]["score"], Value::Null);
        assert_eq!(review["harnesses"][0]["state"], "no-reading");
        assert_eq!(review["harnesses"][0]["score"], Value::Null);
        assert_eq!(
            review["reason"],
            Value::Null,
            "no lane is untyped any more, so none carries a waiting-for reason",
        );
        assert_eq!(
            review["harnesses"][0]["components"][0]["label"],
            "Shipped without review",
        );
        assert!(
            review["harnesses"][0]["components"][0]["detail"]
                .as_str()
                .expect("a silent component still says why")
                .contains("review surfaces are all typed"),
        );

        // Context Management was the other one until munshi#77 typed compaction. It now carries a
        // score and no reason at all, in the same payload the report's table is serialized from.
        let context = lane(&payload, "context-management");
        assert_eq!(context["fleet"]["state"], "scored");
        assert_eq!(context["fleet"]["score"], 100);
        assert_eq!(context["reason"], Value::Null);
        assert_eq!(
            context["harnesses"][0]["components"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            context["harnesses"][0]["components"][0]["label"],
            "Compaction churn",
        );
    }

    /// A fed lane whose signals were all silent is a third state, and must not read as either of
    /// the other two. Five sessions with no tool outcome and no command is exactly that.
    #[test]
    fn a_fed_but_silent_lane_reads_as_no_reading_rather_than_as_a_hundred() {
        let payload = built(hygiene_window("claude-code", 5, 0), Vec::new());
        let mastery = lane(&payload, "tool-mastery");
        assert_eq!(mastery["fleet"]["state"], "no-reading");
        assert_eq!(mastery["fleet"]["score"], Value::Null);
        assert_eq!(mastery["harnesses"][0]["state"], "no-reading");
        assert_eq!(
            mastery["reason"],
            Value::Null,
            "the lane is fed; it was silent"
        );
    }

    /// An arrow appears where both windows measured the lane, points the right way, and carries
    /// the size of the move and the score it moved from.
    #[test]
    fn a_lane_measured_in_both_windows_carries_a_trend() {
        let improved = built(
            hygiene_window("claude-code", 20, 2),
            hygiene_window("claude-code", 20, 5),
        );
        let trend = &lane(&improved, "session-hygiene")["harnesses"][0]["trend"];
        assert_eq!(trend["direction"], "up");
        assert_eq!(trend["glyph"], "▲");
        assert_eq!(trend["points"], 30);
        assert_eq!(trend["was"], 50);
        assert_eq!(
            lane(&improved, "session-hygiene")["fleet"]["trend"]["direction"],
            "up"
        );

        let worsened = built(
            hygiene_window("claude-code", 20, 5),
            hygiene_window("claude-code", 20, 2),
        );
        assert_eq!(
            lane(&worsened, "session-hygiene")["harnesses"][0]["trend"]["direction"],
            "down",
        );

        let flat = built(
            hygiene_window("claude-code", 20, 3),
            hygiene_window("claude-code", 20, 3),
        );
        let flat = &lane(&flat, "session-hygiene")["harnesses"][0]["trend"];
        assert_eq!(flat["direction"], "flat");
        assert_eq!(flat["points"], 0);
    }

    /// The rule the arrows live by: a lane the comparison window could not measure gets `null`,
    /// never an arrow drawn against nothing.
    #[test]
    fn a_lane_the_comparison_window_could_not_measure_carries_no_trend() {
        // Two eligible sessions is under the minimum a fire rate needs, so the earlier window has
        // no reading to compare against.
        let payload = built(
            hygiene_window("claude-code", 20, 5),
            hygiene_window("claude-code", 2, 2),
        );
        let hygiene = lane(&payload, "session-hygiene");
        assert_eq!(hygiene["harnesses"][0]["score"], 50);
        assert_eq!(hygiene["harnesses"][0]["trend"], Value::Null);
        assert_eq!(hygiene["fleet"]["trend"], Value::Null);

        // And with no comparison window folded at all, nothing on the page carries one.
        let alone = built(hygiene_window("claude-code", 20, 5), Vec::new());
        assert_eq!(
            lane(&alone, "session-hygiene")["fleet"]["trend"],
            Value::Null
        );
    }

    /// The fleet blend is the unweighted mean of the harness scores, and its trend appears only
    /// when the same harnesses blended it in both windows — a roster change moves the mean with
    /// nobody's behaviour behind it.
    #[test]
    fn a_fleet_trend_needs_the_same_roster_on_both_sides() {
        let mut both = hygiene_window("claude-code", 20, 10);
        both.extend(hygiene_window("copilot-cli", 20, 0));
        let mut earlier_both = hygiene_window("claude-code", 20, 10);
        earlier_both.extend(hygiene_window("copilot-cli", 20, 0));

        let same_roster = built(both.clone(), earlier_both);
        let fleet = &lane(&same_roster, "session-hygiene")["fleet"];
        assert_eq!(fleet["score"], 75, "the unweighted mean of 50 and 100");
        assert_eq!(fleet["harnesses"], json!(["claude-code", "copilot-cli"]));
        assert_eq!(fleet["trend"]["direction"], "flat");

        // The comparison window is one harness short: the blends are means over different rosters
        // and must not be compared at all.
        let changed_roster = built(both, hygiene_window("claude-code", 20, 10));
        assert_eq!(
            lane(&changed_roster, "session-hygiene")["fleet"]["trend"],
            Value::Null,
        );
    }

    /// A harness that scored last window and contributed nothing to this one keeps its column and
    /// says so, rather than vanishing and quietly changing what the fleet number is a mean of.
    #[test]
    fn a_harness_that_stopped_appearing_keeps_its_column() {
        let payload = built(hygiene_window("claude-code", 20, 5), {
            let mut previous = hygiene_window("claude-code", 20, 5);
            previous.extend(hygiene_window("copilot-cli", 20, 0));
            previous
        });
        let harnesses = lane(&payload, "session-hygiene")["harnesses"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(harnesses.len(), 2);
        assert_eq!(harnesses[1]["source_agent"], "copilot-cli");
        assert_eq!(harnesses[1]["state"], "no-sessions");
        assert_eq!(harnesses[1]["sessions"], 0);
    }

    /// A finding carries the rule's identity, the report's own wording, the count, and the hashes —
    /// and nothing that could only have come from a transcript.
    #[test]
    fn a_finding_carries_problem_action_counts_and_hashes() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let findings = payload["findings"].as_array().unwrap();
        let marathon = findings
            .iter()
            .find(|finding| finding["rule"] == "marathon-session")
            .expect("the marathon rule fires on this window");
        assert_eq!(marathon["title"], "Marathon session");
        assert_eq!(marathon["sessions_affected"], 5);
        assert_eq!(marathon["source_hashes"].as_array().unwrap().len(), 5);
        assert!(
            marathon["problem"]
                .as_str()
                .unwrap()
                .starts_with("5 of 20 folded sessions worked for more than"),
        );
        assert!(
            marathon["action"]
                .as_str()
                .unwrap()
                .starts_with("Split the work at the next natural boundary"),
        );
        for hash in marathon["source_hashes"].as_array().unwrap() {
            let hash = hash.as_str().unwrap();
            assert_eq!(hash.len(), 64);
            assert!(hash.chars().all(|character| character.is_ascii_hexdigit()));
        }
    }

    /// The provenance block is the CLI's instrumentation footer plus the three facts only a
    /// long-lived process has, and it states this module's own contract about itself.
    #[test]
    fn the_provenance_block_carries_the_footer_and_the_refresh() {
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            priced_window(),
            narrated_window(),
            Redactor::new(),
        );
        let provenance = &payload["provenance"];

        // The two lanes the slice added, each with the window it was folded over and what folding
        // it cost — beside the coaching lane's own quantities, which keep their place.
        assert_eq!(provenance["cost_window"], "12w");
        assert_eq!(provenance["standup_window"], "7d");
        let cost = &provenance["lanes"]["cost"];
        assert_eq!(cost["window"], "12w");
        assert_eq!(cost["sync"], "240 ms");
        assert_eq!(cost["fold"], "11 ms");
        assert_eq!(cost["fold_millis"], 11);
        assert_eq!(cost["sessions_folded"], 1);
        assert_eq!(cost["comparison_sessions_folded"], 1);
        assert_eq!(cost["records_read"], 12);
        assert_eq!(cost["cache_hits"], 3);
        let standup = &provenance["lanes"]["standup"];
        assert_eq!(standup["window"], "7d");
        assert_eq!(standup["sync"], "45 ms");
        assert_eq!(standup["fold"], "2 ms");
        assert_eq!(standup["sessions_folded"], 1);
        assert_eq!(standup["bytes_folded"], "4.0 KiB");
        assert_eq!(
            standup["comparison_sessions_folded"],
            Value::Null,
            "a narrative folds one window; there is no arrow to draw",
        );

        // What a whole refresh costs, wall-clock across the three folds — taken from the clock
        // rather than reconstructed from the footers, which is why this fixture's 370 ms is nothing
        // like the sum of the lane figures above it. Against production the two do very nearly
        // agree (the cache spares bytes, not requests); this asserts the field reports the
        // measurement it was handed and not an arithmetic over its neighbours.
        assert_eq!(provenance["refresh_elapsed"], "370 ms");
        assert_eq!(provenance["refresh_elapsed_millis"], 370);

        // The price table sits beside the rule pack, because a delta is only comparable when the
        // table that drew it matches — the same claim the rule-pack stamp makes about scores.
        assert_eq!(provenance["price_table_revision"], PRICE_TABLE_REVISION);
        assert_eq!(provenance["window"], "7d");
        assert_eq!(provenance["sessions_folded"], 20);
        assert_eq!(provenance["fold"], "7 ms");
        assert_eq!(provenance["sync"], "120 ms");
        assert_eq!(provenance["bytes_folded"], "8.0 KiB");
        assert_eq!(provenance["cache_hits"], 2);
        assert_eq!(provenance["cache_misses"], 1);
        assert_eq!(provenance["rule_pack"], RulePack::current().stamp());
        assert_eq!(provenance["redaction_pattern_revision"], PATTERN_REVISION);
        assert_eq!(provenance["refresh_interval"], "5m");
        assert_eq!(provenance["generation"], 3);
        assert_eq!(provenance["stale_since"], Value::Null);
        // True since the excerpt route: the payload still carries no transcript text, but the
        // surface it feeds renders some, and the block below says under which scrub.
        assert_eq!(provenance["renders_verbatim"], true);
        assert_eq!(provenance["redaction"]["secrets"], true);
        assert_eq!(provenance["redaction"]["profanity"], false);
        assert_eq!(
            provenance["redaction"]["pattern_revision"],
            PATTERN_REVISION
        );
    }

    // -----------------------------------------------------------------------
    // The cost section
    // -----------------------------------------------------------------------

    /// The headline, the models behind it, and the repositories the money went to — the same three
    /// answers `qanungo cost` prints, in the same order of most expensive first.
    #[test]
    fn the_cost_section_carries_the_total_the_models_and_the_repositories() {
        let totals = folded_cost(
            &[
                cost_session(
                    "claude-opus-5",
                    "msg_1",
                    r#"{"output_tokens":1000000,"cache_read_input_tokens":1000000}"#,
                    Some("surdy/qanungo"),
                ),
                cost_session(
                    "claude-sonnet-5",
                    "msg_2",
                    r#"{"output_tokens":1000000}"#,
                    None,
                ),
            ],
            None,
        );
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            totals,
            Standup::default(),
            Redactor::new(),
        );
        let cost = &payload["cost"];

        // 1M Opus output at $25 + 1M cache read at $0.50 + 1M Sonnet output at $10.
        assert_eq!(cost["priced"]["priced_anything"], true);
        assert_eq!(cost["priced"]["dollars_rendered"], "$35.50");
        assert_eq!(cost["priced"]["sessions"], 2);
        assert_eq!(cost["priced"]["messages"], 2);
        assert_eq!(
            cost["window"]["last"], "12w",
            "its own window, not `--last`"
        );

        // Most expensive first, and the tokens beside the money in both raw and rendered form.
        let models = cost["by_model"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["model"], "claude-opus-5");
        assert_eq!(models[0]["dollars_rendered"], "$25.50");
        assert_eq!(models[0]["tokens"]["output"]["tokens"], 1_000_000);
        assert_eq!(models[0]["tokens"]["output"]["rendered"], "1.0M");
        assert_eq!(models[1]["model"], "claude-sonnet-5");
        assert_eq!(models[1]["dollars_rendered"], "$10.00");

        // A session captured outside a checkout is its own row with a null name, never folded into
        // a named repository's.
        let repositories = cost["by_repository"].as_array().unwrap();
        assert_eq!(repositories.len(), 2);
        assert_eq!(repositories[0]["repository"], "surdy/qanungo");
        assert_eq!(repositories[0]["dollars_rendered"], "$25.50");
        assert_eq!(repositories[1]["repository"], Value::Null);
        assert_eq!(repositories[1]["dollars_rendered"], "$10.00");

        // What caching saved, priced against what sending the tokens again would have cost.
        assert_eq!(cost["caching"]["read"]["rendered"], "1.0M");
        assert_eq!(cost["caching"]["read_dollars_rendered"], "$0.50");
        assert_eq!(cost["caching"]["at_input_rate_rendered"], "$5.00");
        assert_eq!(cost["caching"]["saving_rendered"], "$4.50");

        assert_eq!(cost["price_table_revision"], PRICE_TABLE_REVISION);
    }

    /// The lane's honesty rule, held on the wire rather than in a renderer: Copilot records output
    /// tokens and nothing else, so its rows carry **no money-shaped field anywhere** and there is no
    /// blended total that would hide the split behind one number. A page cannot render a dollar
    /// figure it was never handed.
    #[test]
    fn copilot_rows_are_token_volumes_and_carry_no_money_anywhere() {
        let transcript = r#"{"type":"assistant.message","timestamp":"2026-08-01T10:00:00.000Z","data":{"content":"one","messageId":"m1","model":"claude-opus-4.8","outputTokens":128}}"#;
        let copilot = SessionCost {
            source_agent: "copilot-cli".to_owned(),
            fold: crate::cost::fold_cost(
                munshi_transcript::Source::Copilot,
                2,
                transcript.as_bytes(),
            )
            .expect("v2 is supported"),
            ..cost_session("unused", "unused", r#"{"output_tokens":0}"#, None)
        };
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            folded_cost(&[copilot], None),
            Standup::default(),
            Redactor::new(),
        );
        let cost = &payload["cost"];

        assert_eq!(cost["copilot"]["basis"], "tokens-only");
        let rows = cost["copilot"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["model"], "claude-opus-4.8");
        assert_eq!(rows[0]["messages"], 1);
        assert_eq!(rows[0]["output"], 128);
        assert_eq!(rows[0]["output_rendered"], "128");

        // Not "the dollars are zero" — there is no key here that could carry one, at any depth.
        assert_no_money(&cost["copilot"], "cost.copilot");

        // And the window's own total is untouched by a copilot session: it says nothing was priced,
        // rather than reporting a blended figure with copilot's tokens silently inside it.
        assert_eq!(cost["priced"]["priced_anything"], false);
        assert_eq!(cost["priced"]["dollars"], 0.0);
        assert_eq!(cost["sessions"]["priced"], 0);
        assert_eq!(cost["sessions"]["token_only"], 1);
        assert!(cost["by_model"].as_array().unwrap().is_empty());
        assert!(cost["by_repository"].as_array().unwrap().is_empty());
    }

    /// Asserts that no key under `value` is money-shaped and no string under it looks like a dollar
    /// figure. Recursive, because a page reads leaves and the interesting place to smuggle a number
    /// is inside a nested row rather than at the top of the block.
    fn assert_no_money(value: &Value, path: &str) {
        match value {
            Value::Object(fields) => {
                for (key, nested) in fields {
                    for forbidden in ["dollar", "cost", "price", "credit", "spend", "usd"] {
                        assert!(
                            !key.to_ascii_lowercase().contains(forbidden),
                            "{path}.{key} is money-shaped, and this block is tokens only",
                        );
                    }
                    assert_no_money(nested, &format!("{path}.{key}"));
                }
            }
            Value::Array(items) => {
                for (index, nested) in items.iter().enumerate() {
                    assert_no_money(nested, &format!("{path}[{index}]"));
                }
            }
            Value::String(text) => assert!(
                !text.contains('$'),
                "{path} renders a dollar figure: {text:?}",
            ),
            Value::Number(_) | Value::Bool(_) | Value::Null => {}
        }
    }

    /// The delta is drawn under the cost report's own two refusals, and both of them are states a
    /// reader can tell apart: no comparison window was asked for at all, or one was and it priced
    /// nothing. Neither is an arrow against zero.
    #[test]
    fn the_cost_delta_is_drawn_only_against_a_window_that_priced_something() {
        let earlier = [cost_session(
            "claude-sonnet-5",
            "msg_0",
            r#"{"output_tokens":1000000}"#,
            None,
        )];
        let compared = folded_cost(
            &[cost_session(
                "claude-opus-5",
                "msg_1",
                r#"{"output_tokens":1000000}"#,
                None,
            )],
            Some(&earlier),
        );
        let section = |cost: FoldedCost| {
            build(
                folded(hygiene_window("claude-code", 20, 5), Vec::new()),
                cost,
                Standup::default(),
                Redactor::new(),
            )["cost"]["comparison"]
                .clone()
        };

        // $25.00 this window against $10.00 before it.
        let moved = section(compared);
        assert_eq!(moved["state"], "compared");
        assert_eq!(moved["direction"], "up");
        assert_eq!(moved["glyph"], "▲");
        assert_eq!(moved["was_rendered"], "$10.00");
        assert_eq!(moved["delta_rendered"], "$15.00");
        assert_eq!(moved["was_sessions"], 1);
        assert!(moved["opens_at"].as_str().unwrap().ends_with('Z'));

        // Spending less points the other way, and the glyph says so without calling it better.
        let fell = section(folded_cost(
            &earlier,
            Some(&[cost_session(
                "claude-opus-5",
                "msg_1",
                r#"{"output_tokens":1000000}"#,
                None,
            )]),
        ));
        assert_eq!(fell["direction"], "down");
        assert_eq!(fell["glyph"], "▼");
        assert_eq!(fell["delta_rendered"], "$15.00");

        // A comparison window that priced nothing gets a state, never an arrow against zero.
        let nothing = section(folded_cost(&earlier, Some(&[])));
        assert_eq!(nothing["state"], "nothing-priced");
        assert_eq!(nothing["delta"], Value::Null);

        // And a window with no comparison window at all is a third state, not the second one.
        let absent = section(folded_cost(&earlier, None));
        assert_eq!(absent["state"], "no-window");
        assert_eq!(absent["opens_at"], Value::Null);
    }

    /// The small top-tier sessions reach the page as the document lists them: the floors that
    /// defined "small", the denominator the count is a share of, the rows with the hash to read
    /// each session by, and totals that are the CLI's own arithmetic rather than a second one.
    #[test]
    fn the_cost_section_carries_the_small_top_tier_sessions_with_the_floors_that_found_them() {
        let totals = folded_cost(
            &[
                cost_session("claude-fable-5", "msg_1", r#"{"output_tokens":1000}"#, None),
                // The denominator: same model, far too large to be listed.
                cost_session(
                    "claude-fable-5",
                    "msg_2",
                    r#"{"output_tokens":500000}"#,
                    None,
                ),
                // A cheaper model, which is not at the top tier however small its session is.
                cost_session("claude-opus-5", "msg_3", r#"{"output_tokens":10}"#, None),
            ],
            None,
        );
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            totals,
            Standup::default(),
            Redactor::new(),
        );
        let premium = &payload["cost"]["premium"];

        assert_eq!(premium["any"], true);
        assert_eq!(premium["sessions"], 2, "both fable sessions are eligible");
        assert_eq!(premium["floors"]["max_messages"], PREMIUM_FLAG_MAX_MESSAGES);
        assert_eq!(
            premium["floors"]["max_output_tokens"],
            PREMIUM_FLAG_MAX_OUTPUT_TOKENS
        );
        assert_eq!(premium["floors"]["max_output_tokens_rendered"], "3.0k");

        let rows = premium["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["models"], json!(["claude-fable-5"]));
        assert_eq!(rows[0]["messages"], 1);
        assert_eq!(rows[0]["output"], 1_000);
        assert_eq!(rows[0]["output_rendered"], "1.0k");
        assert_eq!(rows[0]["dollars_rendered"], "$0.05");
        assert_eq!(rows[0]["archived_at"], "2026-08-10T00:00:00Z");
        assert_eq!(rows[0]["source_hash"], "0".repeat(64));
        assert_eq!(premium["rows_hidden"], 0);

        // The totals are the CLI's, and the dollars in them are inside the window's own total
        // rather than beside it.
        assert_eq!(premium["totals"]["sessions"], 1);
        assert_eq!(premium["totals"]["dollars_rendered"], "$0.05");
        assert_eq!(premium["totals"]["output"], 1_000);
        assert_eq!(premium["totals"]["messages"], 1);
        assert!(
            payload["cost"]["priced"]["dollars"].as_f64().unwrap()
                > premium["totals"]["dollars"].as_f64().unwrap(),
        );

        // A window with nothing small at the top tier says so with an absent block rather than a
        // row of zeroes a page would have to know to hide — and still carries the floors, so the
        // page never has to guess what was measured.
        let clean = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            priced_window(),
            Standup::default(),
            Redactor::new(),
        );
        assert_eq!(clean["cost"]["premium"]["any"], false);
        assert_eq!(clean["cost"]["premium"]["totals"], Value::Null);
        assert!(
            clean["cost"]["premium"]["rows"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(clean["cost"]["premium"]["floors"]["max_messages"], 8);
    }

    /// The cap bites on the wire exactly as it does in the document: the page is handed
    /// [`PREMIUM_SESSIONS_LISTED`] rows and told how many it is not being handed, and the totals
    /// beside them still count every flagged session. Without this the payload would grow with the
    /// flag on a window that flagged hundreds, which is the one thing a single-fetch page cannot
    /// afford.
    #[test]
    fn the_top_tier_rows_are_capped_on_the_wire_and_say_how_many_were_cut() {
        let sessions: Vec<SessionCost> = (0..PREMIUM_SESSIONS_LISTED + 1)
            .map(|index| SessionCost {
                source_hash: format!("{index:064}"),
                ..cost_session("claude-fable-5", "msg_1", r#"{"output_tokens":1000}"#, None)
            })
            .collect();
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            folded_cost(&sessions, None),
            Standup::default(),
            Redactor::new(),
        );
        let premium = &payload["cost"]["premium"];

        assert_eq!(
            premium["rows"].as_array().unwrap().len(),
            PREMIUM_SESSIONS_LISTED,
        );
        assert_eq!(premium["rows_hidden"], 1);
        // The totals are over all 21, not over the 20 that were serialized: 21,000 output tokens
        // of Fable 5 at $50/MTok.
        assert_eq!(premium["totals"]["sessions"], PREMIUM_SESSIONS_LISTED + 1);
        assert_eq!(premium["totals"]["output"], 21_000);
        assert_eq!(premium["totals"]["messages"], 21);
        assert_eq!(premium["totals"]["dollars_rendered"], "$1.05");
        assert_eq!(premium["sessions"], PREMIUM_SESSIONS_LISTED + 1);
    }

    /// Copilot has no rate and therefore no tier, so nothing about it can reach a block full of
    /// money keys. The copilot block's own no-money rule is unchanged by the new one beside it,
    /// and the top-tier list stays empty in a window with no priced session at all.
    #[test]
    fn a_copilot_window_reaches_neither_the_top_tier_list_nor_a_dollar() {
        let transcript = r#"{"type":"assistant.message","timestamp":"2026-08-01T10:00:00.000Z","data":{"content":"one","messageId":"m1","model":"claude-fable-5","outputTokens":128}}"#;
        let copilot = SessionCost {
            source_agent: "copilot-cli".to_owned(),
            fold: crate::cost::fold_cost(
                munshi_transcript::Source::Copilot,
                2,
                transcript.as_bytes(),
            )
            .unwrap(),
            ..cost_session("unused", "unused", r#"{"output_tokens":0}"#, None)
        };
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            folded_cost(&[copilot], None),
            Standup::default(),
            Redactor::new(),
        );
        assert_eq!(payload["cost"]["premium"]["any"], false);
        assert_eq!(payload["cost"]["premium"]["sessions"], 0);
        assert_no_money(&payload["cost"]["copilot"], "cost.copilot");
    }

    /// Everything the fold counted and refused to price, each reason on its own line with its own
    /// tokens — a section that merged them could not say whether a gap was a placeholder, a missing
    /// price row, or a bug.
    #[test]
    fn the_cost_sections_flags_name_each_refusal_separately() {
        let totals = folded_cost(
            &[
                cost_session("<synthetic>", "msg_1", r#"{"output_tokens":500}"#, None),
                cost_session("claude-opus-9", "msg_2", r#"{"output_tokens":700}"#, None),
                cost_session(
                    "claude-opus-5",
                    "msg_3",
                    r#"{"input_tokens":10,"cache_creation_input_tokens":4096}"#,
                    None,
                ),
            ],
            None,
        );
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            totals,
            Standup::default(),
            Redactor::new(),
        );
        let flagged = &payload["cost"]["flagged"];

        assert_eq!(flagged["any"], true);
        assert_eq!(flagged["synthetic"]["messages"], 1);
        assert_eq!(flagged["synthetic"]["tokens"]["output"]["tokens"], 500);

        let unpriced = flagged["unpriced"].as_array().unwrap();
        assert_eq!(unpriced.len(), 1);
        assert_eq!(unpriced[0]["messages"], 1);
        assert!(
            unpriced[0]["detail"]
                .as_str()
                .unwrap()
                .contains("no price row for model `claude-opus-9`"),
            "{unpriced:?}",
        );

        // A write stated only as a total has no tier and therefore no rate: reported, never charged
        // at an assumed one.
        assert_eq!(flagged["untiered_cache_writes"]["tokens"], 4_096);
        assert_eq!(flagged["untiered_cache_writes"]["rendered"], "4.1k");
        assert_eq!(flagged["untiered_cache_writes"]["messages"], 1);

        // Nothing was undeduplicatable here, and an absent flag is null rather than a row of zeroes
        // a page would have to know to hide.
        assert_eq!(flagged["undeduplicatable"], Value::Null);

        // A clean window flags nothing at all, so the page can elide the whole block.
        let clean = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            priced_window(),
            Standup::default(),
            Redactor::new(),
        );
        assert_eq!(clean["cost"]["flagged"]["any"], false);
        assert_eq!(clean["cost"]["flagged"]["synthetic"], Value::Null);
    }

    /// A window that read nothing from the cache has no saving to report, and that is a different
    /// statement from a saving of zero — so the block is absent rather than zeroed.
    #[test]
    fn the_caching_block_is_absent_when_nothing_was_read_from_the_cache() {
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            folded_cost(
                &[cost_session(
                    "claude-opus-5",
                    "msg_1",
                    r#"{"output_tokens":1000}"#,
                    None,
                )],
                None,
            ),
            Standup::default(),
            Redactor::new(),
        );
        assert_eq!(payload["cost"]["caching"], Value::Null);
        assert_eq!(payload["cost"]["priced"]["priced_anything"], true);
    }

    /// A model id is the archive's string, and a served page is a rendering surface a peer does not
    /// get to choose characters on — the same clamp the Markdown table's cells pass through.
    #[test]
    fn a_hostile_model_id_is_clamped_before_it_reaches_the_cost_section() {
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            folded_cost(
                &[cost_session(
                    "evil | model",
                    "msg_1",
                    r#"{"output_tokens":10}"#,
                    Some("back`tick"),
                )],
                None,
            ),
            Standup::default(),
            Redactor::new(),
        );
        let serialized = serde_json::to_string(&payload["cost"]).unwrap();
        assert!(!serialized.contains("evil | model"), "{serialized}");
        assert!(!serialized.contains("back`tick"), "{serialized}");
        assert!(
            serialized.contains(format::INVALID_IDENTIFIER),
            "{serialized}"
        );
    }

    // -----------------------------------------------------------------------
    // The standup section
    // -----------------------------------------------------------------------

    /// The section is the fold's own strings, arranged and not re-worded: the grouping, the
    /// ordering, the rollups, and the gaps a reader would see in the Markdown.
    #[test]
    fn the_standup_section_is_the_folds_own_grouping_and_rollups() {
        let standup = narrated_window();
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            folded_cost(&[], None),
            standup.clone(),
            Redactor::new(),
        );
        let section = &payload["standup"];

        assert_eq!(section["window"]["last"], "7d", "its own window");
        assert_eq!(section["sessions"], 1);
        assert_eq!(section["repositories_narrated"], 1);

        let group = &section["repositories"][0];
        assert_eq!(group["repository"], "surdy/qanungo");
        let served = &group["sessions"][0];
        let session = &standup.repositories[0].sessions[0];
        // Equal to the fold's strings, character for character. That is the whole claim this
        // section makes: it does not re-scrub, re-word, or re-order what the fold produced.
        assert_eq!(served["title"], session.title);
        assert_eq!(served["goal"], session.goal);
        assert_eq!(served["branch"], "main");
        assert_eq!(served["source_hash"], session.source_hash);
        assert_eq!(served["archived_at"], "2026-08-16T09:00:00Z");
        assert_eq!(served["work_completed"], json!(session.work_completed));
        assert_eq!(served["decisions"], json!(session.decisions));
        assert_eq!(served["open_items"], json!(session.open_items));

        assert_eq!(section["decisions"][0]["repository"], "surdy/qanungo");
        assert_eq!(section["decisions"][0]["text"], standup.decisions[0].text);
        assert_eq!(section["open_items"][0]["text"], standup.open_items[0].text);

        // Gaps are the same shape all three lanes' gaps take, and are counted rather than dropped.
        assert_eq!(section["gaps"][0]["count"], 2);
        assert!(
            section["gaps"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("placeholder"),
        );

        // A clean scrub is `0` and an empty list, not an absent block: "nothing matched" and "the
        // scrub was off" are very different sentences, and provenance says which of the two this is.
        assert_eq!(section["redaction"]["total"], 0);
        assert!(section["redaction"]["fired"].as_array().unwrap().is_empty());
    }

    /// An empty window is a served section too: it says nothing was narrated rather than vanishing
    /// and leaving a reader to guess whether the lane broke.
    #[test]
    fn an_unnarrated_window_still_serves_a_standup_section() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let section = &payload["standup"];
        assert_eq!(section["sessions"], 0);
        assert_eq!(section["repositories_narrated"], 0);
        assert!(section["repositories"].as_array().unwrap().is_empty());
        assert!(section["decisions"].as_array().unwrap().is_empty());
        assert!(section["gaps"].as_array().unwrap().is_empty());
        assert_eq!(section["window"]["last"], "7d");
    }

    /// What a scrub fired travels with the text, as counts against pattern ids and nothing else —
    /// so a reader looking at a marker can see it was accounted for, and the type serialized here
    /// has no shape in which it could carry the value it matched.
    #[test]
    fn the_standup_section_reports_what_the_scrub_fired_as_counts_only() {
        let mut redaction = RedactionReport::default();
        for _ in 0..3 {
            redaction.absorb(
                &Redactor::new()
                    .scrub("ghp_0123456789012345678901234567890123456")
                    .report,
            );
        }
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            folded_cost(&[], None),
            Standup {
                redaction,
                ..narrated_window()
            },
            Redactor::new(),
        );
        let fired = &payload["standup"]["redaction"];
        assert_eq!(fired["total"], 3);
        assert_eq!(fired["fired"][0]["pattern"], "github-token");
        assert_eq!(fired["fired"][0]["count"], 3);
        assert!(
            !serde_json::to_string(fired).unwrap().contains("ghp_"),
            "a report cannot carry what it matched",
        );
    }

    // -----------------------------------------------------------------------
    // The ask answer
    // -----------------------------------------------------------------------

    /// One archived `summary.md` as the cache read it — the shape [`Ask::fold`] scores.
    ///
    /// Built directly rather than through a mirror and a parse, because what is under test in this
    /// module is the *answer over* a corpus; `tests/ask.rs` covers the reading of one, and
    /// `tests/dashboard.rs` covers both reached through HTTP.
    fn read_summary(hash: &str, title: &str, goal: &str, repository: Option<&str>) -> ReadSummary {
        let summary = munshi_transcript::StructuredSummary {
            title: title.to_owned(),
            goal: goal.to_owned(),
            work_completed: Vec::new(),
            decisions: Vec::new(),
            files_changed: Vec::new(),
            commands_and_validation: Vec::new(),
            open_items: Vec::new(),
            tags: Vec::new(),
        };
        ReadSummary {
            source_hash: hash.to_owned(),
            archived_at: Some(at("2026-08-16T09:00:00Z")),
            bytes_read: 2048,
            archived: munshi_transcript::ArchivedMarkdown {
                schema_version: 1,
                source: munshi_transcript::SourceKind::ClaudeCode,
                session_id: "session".to_owned(),
                project: munshi_transcript::ProjectIdentity {
                    identity: "id".to_owned(),
                    component: "component".to_owned(),
                    project: "project".to_owned(),
                    repository: repository.map(ToOwned::to_owned),
                    branch: Some("main".to_owned()),
                    origin: munshi_transcript::ProjectOrigin::Live,
                },
                summary_revision: 1,
                completion_reason: "complete".to_owned(),
                cursor_fallback_reason: None,
                cursor: None,
                source_cursor: 0,
                source_hash: hash.to_owned(),
                started_at: None,
                updated_at: None,
                summary_placeholder: false,
                artifact_set_version: Some(2),
                transcript_sha256: None,
                extracted_outputs: Vec::new(),
                summary,
            },
        }
    }

    fn corpus(read: Vec<ReadSummary>, unsearchable: usize) -> AskCorpus {
        AskCorpus::over(at("2026-08-17T12:00:00Z"), read, unsearchable)
    }

    /// Two summaries, one of which is squarely about payments.
    fn searchable() -> Vec<ReadSummary> {
        vec![
            read_summary(
                &"a".repeat(64),
                "Price the payments API",
                "Charge the payments API at list rates.",
                Some("surdy/qanungo"),
            ),
            read_summary(
                &"b".repeat(64),
                "Fold the timeline",
                "Lay the window on a calendar.",
                None,
            ),
        ]
    }

    /// One answer, built the way the route builds it.
    fn answer(query: &Query, limit: usize, corpus: &AskCorpus, redactor: &Redactor) -> Value {
        let ask = (!query.is_empty()).then(|| corpus.search(query, redactor, limit));
        AskAnswer {
            query,
            limit,
            ask: ask.as_ref(),
            corpus,
            refreshed: refreshed(),
            redactor,
        }
        .build()
    }

    /// A ranked answer carries the ranking, the counts it was taken over, and the provenance of the
    /// corpus that answered — which is the whole of what a page needs to state the search's bounds.
    #[test]
    fn an_ask_answer_carries_the_ranking_and_the_bounds_it_was_taken_over() {
        let corpus = corpus(searchable(), 3);
        let query = Query::parse("payments");
        let answer = answer(&query, 10, &corpus, &Redactor::new());

        assert_eq!(answer["state"], "ranked");
        assert_eq!(answer["query"]["terms"], json!(["payments"]));
        assert_eq!(
            answer["query"]["min_term_chars"],
            crate::ask::MIN_TERM_CHARS
        );
        assert_eq!(answer["limit"], 10);
        assert_eq!(answer["searched"], 2);
        assert_eq!(
            answer["unsearchable"], 3,
            "counted, never dropped — the corpus's own figure travels with every answer",
        );
        assert_eq!(answer["total_matches"], 1);

        let hits = answer["hits"].as_array().expect("hits are an array");
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit["rank"], 1);
        assert_eq!(hit["title"], "Price the payments API");
        assert_eq!(hit["harness"], "claude-code");
        assert_eq!(hit["repository"], "surdy/qanungo");
        assert_eq!(hit["branch"], "main");
        assert_eq!(hit["archived_at"], "2026-08-16T09:00:00Z");
        assert_eq!(hit["source_hash"], "a".repeat(64));
        assert_eq!(hit["matched"], json!(["title", "goal"]));
        assert!(hit["score"].as_u64().expect("a score") > 0);
        assert!(
            hit["snippet"]
                .as_str()
                .expect("a snippet")
                .contains("Price the payments API"),
        );

        // The corpus block: which refresh answered, when it was read, how much it holds, and what
        // it covers. A page that could not say these could not date its own answer.
        let block = &answer["corpus"];
        assert_eq!(block["generation"], 3);
        assert_eq!(block["read_at"], "2026-08-17T12:00:00Z");
        assert_eq!(block["stale_since"], Value::Null);
        assert_eq!(block["sessions_listed"], 5, "2 searchable + 3 not");
        assert_eq!(block["bytes_read"], "4.0 KiB");
        assert_eq!(
            block["scope"], "all-history",
            "no window and no scope: this route is independent of the page's controls",
        );

        // The scrub behind the snippets, stated as every verbatim surface here states it.
        assert_eq!(answer["redaction"]["secrets"], true);
        assert_eq!(answer["redaction"]["profanity"], false);
        assert_eq!(answer["redaction"]["pattern_revision"], PATTERN_REVISION);
        assert_eq!(answer["redaction"]["total"], 0);

        // The same query over the same corpus twice is the same answer, byte for byte.
        assert_eq!(answer, self::answer(&query, 10, &corpus, &Redactor::new()));
    }

    /// The three answers are three states, because they are three different sentences: no word to
    /// search on, the archive's own "no", and a list.
    #[test]
    fn the_three_answers_are_kept_apart_as_states() {
        let corpus = corpus(searchable(), 1);

        let empty = Query::parse("the a of");
        assert!(empty.is_empty());
        let answered = answer(&empty, 10, &corpus, &Redactor::new());
        assert_eq!(answered["state"], "no-searchable-terms");
        assert_eq!(answered["query"]["terms"], json!([]));
        assert_eq!(answered["total_matches"], 0);
        assert_eq!(answered["hits"], json!([]));
        // Even with nothing scored, the answer still says what it could and could not have looked
        // at — the counts are the corpus's and cost nothing.
        assert_eq!(answered["searched"], 2);
        assert_eq!(answered["unsearchable"], 1);

        let missed = Query::parse("kubernetes helm");
        let answered = answer(&missed, 10, &corpus, &Redactor::new());
        assert_eq!(answered["state"], "no-matches");
        assert_eq!(answered["hits"], json!([]));
        assert_eq!(answered["searched"], 2, "it looked, and the answer is no");
    }

    /// The limit truncates the list and the answer still counts what it hid, so a page can say
    /// "showing 1 of 2" rather than implying the last row was the last match.
    #[test]
    fn a_truncated_ranking_still_counts_what_it_hid() {
        let corpus = corpus(searchable(), 0);
        let query = Query::parse("the payments timeline window");
        let answer = answer(&query, 1, &corpus, &Redactor::new());
        assert_eq!(answer["limit"], 1);
        assert_eq!(answer["hits"].as_array().unwrap().len(), 1);
        assert_eq!(answer["total_matches"], 2);
    }

    /// The query is the one string here a *caller* wrote, and it does not make the round trip.
    ///
    /// What comes back is what the search actually used: lower-cased, broken on everything that is
    /// not a letter or a digit by `Query::parse`, and then clamped as an identifier — so an absurd
    /// run of letters inside the router's byte cap becomes a marker rather than a kilobyte of a
    /// peer's choosing on a rendering surface.
    #[test]
    fn an_answer_echoes_the_searched_terms_and_never_the_callers_bytes() {
        let corpus = corpus(searchable(), 0);
        let hostile = format!("PAYMENTS, <b>api</b>! {}", "z".repeat(200));
        let query = Query::parse(&hostile);
        let answer = answer(&query, 10, &corpus, &Redactor::new());

        let terms: Vec<&str> = answer["query"]["terms"]
            .as_array()
            .expect("terms are an array")
            .iter()
            .map(|term| term.as_str().expect("a term"))
            .collect();
        assert_eq!(terms, vec!["payments", "api", format::INVALID_IDENTIFIER]);
        let serialized = serde_json::to_string(&answer).expect("the answer serializes");
        assert!(!serialized.contains("<b>"), "{serialized}");
        assert!(!serialized.contains("zzzz"), "{serialized}");
    }

    /// The canary, at the answer's own level: a summary carrying a live-shaped credential on the
    /// very line a query lands on cannot render it, and the answer counts the replacement.
    ///
    /// The scrub is `Ask::fold`'s, not this module's — what is pinned here is that the answer
    /// serializes what the fold produced, markers and all, so a reader can see the scrub fired.
    #[test]
    fn a_planted_credential_cannot_render_through_an_answer() {
        let secret = "ghp_0123456789012345678901234567890123456";
        let leaky = read_summary(
            &"c".repeat(64),
            &format!("Rotate {secret} out of CI"),
            "Rotate the token.",
            Some("surdy/qanungo"),
        );
        let corpus = corpus(vec![leaky], 0);
        let query = Query::parse("rotate");
        let answer = answer(&query, 10, &corpus, &Redactor::new());

        let serialized = serde_json::to_string(&answer).expect("the answer serializes");
        assert!(!serialized.contains(secret), "{serialized}");
        assert!(
            serialized.contains("[REDACTED:github-token]"),
            "{serialized}"
        );
        // Twice, and honestly: the title is the highest-weighted field the query landed in, so it
        // is both the hit's title and its snippet, and the fold scrubbed each on its own way in.
        // The count is of replacements made, not of distinct secrets found.
        assert_eq!(answer["redaction"]["total"], 2);
        assert_eq!(answer["redaction"]["fired"][0]["pattern"], "github-token");
        assert_eq!(answer["redaction"]["fired"][0]["count"], 2);
    }

    /// A corpus whose refreshes are failing still answers, and the answer says how old it is —
    /// the same refusal to blank a page the payload's own `stale_since` makes.
    #[test]
    fn a_stale_corpus_dates_its_answer_rather_than_refusing_it() {
        let corpus = corpus(searchable(), 0);
        let query = Query::parse("payments");
        let ask = corpus.search(&query, &Redactor::new(), 10);
        let answer = AskAnswer {
            query: &query,
            limit: 10,
            ask: Some(&ask),
            corpus: &corpus,
            refreshed: Refreshed {
                generation: 9,
                at: at("2026-08-17T12:00:00Z"),
                stale_since: Some(at("2026-08-17T11:30:00Z")),
            },
            redactor: &Redactor::new(),
        }
        .build();
        assert_eq!(answer["state"], "ranked");
        assert_eq!(answer["corpus"]["generation"], 9);
        assert_eq!(answer["corpus"]["stale_since"], "2026-08-17T11:30:00Z");
        assert_eq!(
            answer["corpus"]["read_at"], "2026-08-17T12:00:00Z",
            "the corpus is still the one it was, read when it was",
        );
    }

    /// The fourth lane in the footer: a scope rather than a window, and the two counts that make
    /// the corpus's honesty checkable against any answer taken over it.
    #[test]
    fn the_provenance_block_carries_the_ask_lane() {
        let corpus = corpus(searchable(), 3);
        let coaching = folded(hygiene_window("claude-code", 20, 5), Vec::new());
        let cost = priced_window();
        let standup = folded_standup(narrated_window());
        let payload = Payload {
            windows: &windows(),
            refresh: &refresh(),
            coaching: &coaching,
            cost: &cost,
            standup: &standup,
            ask: &corpus,
            folds_elapsed: Duration::from_millis(370),
            refreshed: refreshed(),
            redactor: &Redactor::new(),
        }
        .build();

        assert_eq!(payload["provenance"]["ask_scope"], "all-history");
        let lane = &payload["provenance"]["lanes"]["ask"];
        assert_eq!(lane["scope"], "all-history");
        assert_eq!(lane["sessions_searchable"], 2);
        assert_eq!(lane["sessions_unsearchable"], 3);
        assert_eq!(lane["bytes_read"], "4.0 KiB");
        assert_eq!(lane["fold"], "0 ms");
        assert_eq!(
            lane["window"],
            Value::Null,
            "a lifetime question has no window to name",
        );
        // The corpus feeds no *section*: it is in the footer and nowhere else.
        for section in ["lanes", "cost", "standup", "timeline", "heatmap", "scopes"] {
            assert!(
                !serde_json::to_string(&payload[section])
                    .expect("the section serializes")
                    .contains("all-history"),
                "the ask corpus reached the {section} section",
            );
        }
    }

    // -----------------------------------------------------------------------
    // The whole document
    // -----------------------------------------------------------------------

    /// One refresh, one generation, one document. The three sections are built from three folds in
    /// a single call, so there is no shape of this payload in which two of them came from different
    /// refreshes — a torn view across lanes is unrepresentable rather than unlikely.
    #[test]
    fn the_three_sections_are_one_generation() {
        let payload = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            priced_window(),
            narrated_window(),
            Redactor::new(),
        );
        for section in ["lanes", "cost", "standup"] {
            assert!(!payload[section].is_null(), "{section} is served");
        }
        assert_eq!(payload["provenance"]["generation"], 3);
        // Each section names the window it is a statement about, and the three differ — which is
        // exactly why an unlabelled number here would be a wrong one.
        assert_eq!(payload["window"]["last"], "7d");
        assert_eq!(payload["cost"]["window"]["last"], "12w");
        assert_eq!(payload["standup"]["window"]["last"], "7d");
        assert_eq!(payload["provenance"]["window"], "7d");
        assert_eq!(payload["provenance"]["cost_window"], "12w");
        assert_eq!(payload["provenance"]["standup_window"], "7d");
    }

    /// The posture a reader sees is the posture the process was started with — there is no third
    /// state and no per-request one.
    #[test]
    fn the_payload_states_the_redactor_the_process_was_launched_with() {
        let raw = build(
            folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            folded_cost(&[], None),
            Standup::default(),
            Redactor::new().with_secrets(false).with_profanity(true),
        );
        assert_eq!(raw["provenance"]["redaction"]["secrets"], false);
        assert_eq!(raw["provenance"]["redaction"]["profanity"], true);
        assert_eq!(raw["provenance"]["renders_verbatim"], true);
    }

    /// A refresh that failed does not blank the page and does not pretend to be fresh: the last
    /// good numbers stay, dated by when they stopped being current.
    #[test]
    fn a_failing_refresh_dates_the_numbers_rather_than_hiding_them() {
        let coaching = folded(hygiene_window("claude-code", 20, 5), Vec::new());
        let cost = priced_window();
        let standup = folded_standup(narrated_window());
        let stale = Payload {
            windows: &windows(),
            refresh: &refresh(),
            coaching: &coaching,
            cost: &cost,
            standup: &standup,
            ask: &corpus(Vec::new(), 0),
            folds_elapsed: Duration::from_millis(370),
            refreshed: Refreshed {
                generation: 9,
                at: at("2026-08-17T12:00:00Z"),
                stale_since: Some(at("2026-08-17T11:30:00Z")),
            },
            redactor: &Redactor::new(),
        }
        .build();
        assert_eq!(stale["provenance"]["stale_since"], "2026-08-17T11:30:00Z");
        assert_eq!(stale["lanes"].as_array().unwrap().len(), 5);
        assert_eq!(stale["sessions"]["folded"], 20);
        // Every section keeps its numbers: a page whose refresh failed is out of date, not empty,
        // and blanking two thirds of it would be hiding facts that are still true of the windows
        // they were taken over.
        assert_eq!(stale["cost"]["priced"]["dollars_rendered"], "$26.50");
        assert_eq!(stale["standup"]["sessions"], 1);
    }

    /// A window too long to place an equal-length one before it has no comparison window, so the
    /// payload says so and carries no arrow anywhere.
    #[test]
    fn a_window_with_no_comparison_says_so_and_draws_nothing() {
        let mut folded = folded(hygiene_window("claude-code", 20, 5), Vec::new());
        folded.compared = false;
        let payload = build(
            folded,
            folded_cost(&[], None),
            Standup::default(),
            Redactor::new(),
        );
        assert_eq!(payload["window"]["compared"], false);
        assert_eq!(payload["window"]["comparison_opens_at"], Value::Null);
        assert_eq!(
            lane(&payload, "session-hygiene")["fleet"]["trend"],
            Value::Null
        );
    }

    /// A harness label is the archive's string, and a served page is a rendering surface a peer
    /// does not get to choose characters on. The clamp is [`format::identifier`]'s, the same one
    /// the report's Gaps lines pass through.
    #[test]
    fn a_hostile_harness_label_is_clamped_before_it_reaches_the_payload() {
        let hostile = "back`tick\nand a newline";
        let payload = built(hygiene_window(hostile, 20, 5), Vec::new());
        let serialized = serde_json::to_string(&payload).unwrap();
        assert!(!serialized.contains("back`tick"), "{serialized}");
        assert!(
            serialized.contains(format::INVALID_IDENTIFIER),
            "the label is replaced wholesale, not truncated: {serialized}",
        );
        assert_eq!(
            payload["sessions"]["by_harness"][format::INVALID_IDENTIFIER],
            20,
        );
    }

    /// The window pair the arrows are drawn across is labelled explicitly, in UTC, because UTC is
    /// the only clock the transcripts carry.
    #[test]
    fn both_windows_are_labelled_in_utc() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        assert_eq!(payload["window"]["last"], "7d");
        assert_eq!(payload["window"]["generated_at"], "2026-08-17T12:00:00Z");
        assert_eq!(payload["window"]["opens_at"], "2026-08-10T12:00:00Z");
        assert_eq!(
            payload["window"]["comparison_opens_at"],
            "2026-08-03T12:00:00Z",
        );
        assert_eq!(
            window("7d").delta(),
            TimeDelta::days(7),
            "the fixture window is the one the labels above were computed from",
        );
    }

    /// The timeline section's shape, over a fixture whose sessions are archived across five UTC
    /// days: a date and two arrays per day, the arrays positional against the payload's own harness
    /// axis, and the whole thing summing back to the window's session count.
    #[test]
    fn the_timeline_lays_the_window_on_days_and_sums_back_to_it() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let timeline = &payload["timeline"];
        let days = timeline["days"].as_array().expect("a day list");
        assert_eq!(days.len(), 5);
        assert_eq!(timeline["days_covered"], 5);
        assert_eq!(timeline["undated"], 0);
        assert_eq!(days[0]["date"], "2026-08-11");
        assert_eq!(days[4]["date"], "2026-08-15");

        // The window is spread four sessions to a day, and the counts add up to the fold.
        let counted: u64 = days
            .iter()
            .map(|day| day["sessions"][0].as_u64().expect("a count"))
            .sum();
        assert_eq!(counted, 20);
        assert_eq!(payload["sessions"]["folded"], 20);
        // One column per harness in the payload's one axis, in the timeline as in the lanes.
        let harnesses = payload["scopes"]["harnesses"].as_array().unwrap().len();
        assert_eq!(days[0]["sessions"].as_array().unwrap().len(), harnesses);
        assert_eq!(
            days[0]["active_seconds"].as_array().unwrap().len(),
            harnesses
        );
        // Active time is seconds, not a rendered span: a bar's height is a quantity.
        assert!(days[0]["active_seconds"][0].as_u64().expect("seconds") > 0);

        // The footer quotes the section rather than counting it a second time.
        assert_eq!(payload["provenance"]["timeline"]["days_covered"], 5);
        assert_eq!(
            payload["provenance"]["timeline"]["basis"],
            "archive-completion-utc",
        );
    }

    /// A window with no comparison window draws no earlier calendar — the same refusal the lanes
    /// make before they draw an arrow, made in the same place and answered from the same flag, so
    /// the page cannot show a `before` the scores declined to compare against.
    #[test]
    fn a_window_with_nothing_to_compare_against_draws_no_earlier_days() {
        let mut coaching = folded(
            hygiene_window("claude-code", 20, 5),
            hygiene_window("claude-code", 8, 0),
        );
        coaching.compared = false;
        let payload = build(
            coaching,
            folded_cost(&[], None),
            Standup::default(),
            Redactor::new(),
        );
        assert_eq!(payload["window"]["compared"], false);
        assert_eq!(payload["timeline"]["comparison_days"], json!([]));
        assert_eq!(payload["timeline"]["comparison_days_covered"], 0);
        assert_eq!(
            payload["provenance"]["timeline"]["comparison_days_covered"],
            0
        );
        for scope in payload["scopes"]["repositories"].as_array().unwrap() {
            assert_eq!(scope["timeline"]["comparison_days"], json!([]));
        }
        // The reported half is untouched by the refusal.
        assert_eq!(payload["timeline"]["days_covered"], 5);
    }

    /// A day nothing was archived on is a gap in the calendar, not a row of zeroes on the wire: the
    /// section costs one row per day that *happened*, so a long quiet window costs nothing extra.
    #[test]
    fn a_day_with_no_session_on_it_is_absent_rather_than_zero() {
        let mut sessions = hygiene_window("claude-code", 4, 0);
        for (index, session) in sessions.iter_mut().enumerate() {
            // Two sessions a fortnight apart: fourteen days between them, and two rows.
            session.archived_at =
                Some(at("2026-08-01T09:00:00Z") + TimeDelta::days(if index < 2 { 0 } else { 14 }));
        }
        let payload = built(sessions, Vec::new());
        let days = payload["timeline"]["days"].as_array().unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0]["date"], "2026-08-01");
        assert_eq!(days[1]["date"], "2026-08-15");
    }
}
