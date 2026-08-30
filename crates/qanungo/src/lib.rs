//! Qanungo — the read-side coaching client over the Patwari archive.
//!
//! Munshi captures coding-agent sessions and Patwari keeps them. Qanungo only ever *reads* that
//! archive: it mirrors the transcripts it needs, folds them into a few honest metrics, applies a
//! small set of rules, and says something useful about how the work is going. It never writes to
//! the archive, and it never becomes a second source of truth about it.
//!
//! # The P0 vertical slice (qanungo #7)
//!
//! ```text
//! qanungo report --last 30d
//! ```
//!
//! sync ([`sync`], [`cache`], [`patwari`]) → fold ([`metrics`]) → evaluate ([`rules`]) → score
//! ([`scoring`]) → emit ([`report`]). Four metrics, six rules, five practice lanes of which three
//! are fed by anything typed today, Markdown on stdout, evidence cited by content hash, and an
//! instrumentation footer on every run.
//!
//! Three things about this slice are deliberate and load-bearing:
//!
//! - **The mirror is minimal.** A naive re-sync with a content-addressed blob cache, no cursor
//!   protocol, no eviction, no event store. Folding a window from cached blobs is cheap enough
//!   that persistence has to earn its way in — and the footer is what will decide that.
//! - **The rules are hardcoded and their thresholds are guesses.** Named constants marked
//!   arbitrary-until-measured, not a DSL and not a decision. The duration ones have since had
//!   that measurement run over them (qanungo #14) and are now pinned to the archive's own
//!   distribution, with the corpus and the fire rate written down beside them.
//! - **The report renders no transcript content.** Aggregates, tool names, and `source_hash`
//!   references only. See [`report`] for why that is a construction property rather than a
//!   filter.
//!
//! Scores and trends (qanungo #4) sit on top of the same slice: every run recomputes all of
//! history with the *current* rule pack rather than reading frozen scores back (qanungo ADR 0001),
//! and stamps the pack's digest into the footer so two reports can tell whether they are
//! comparable at all. A lane no typed signal feeds is never scored — see [`scoring`].
//!
//! # The cost lane (qanungo #12)
//!
//! ```text
//! qanungo cost --last 12w
//! ```
//!
//! is the second lane over the same spine: the same mirror, the same blob cache, the same window
//! pair, a different fold ([`cost`]), a static date-versioned price table ([`pricing`]), and its
//! own document ([`cost_report`]). Three of its properties are the whole of its honesty:
//!
//! - **Deduplicate before summing.** One API message reaches a transcript as several records
//!   repeating its usage verbatim; summing records over-counts output tokens 2.6-fold. The fold
//!   counts distinct message ids and reports how many records that dropped.
//! - **Price what is priceable, and say what is not.** Dollars are claimed for claude-code
//!   sessions only, at Anthropic API *list* prices, from rows effective at each session's archive
//!   time. A model with no row, an unrecognized billing modifier, a cache write with no tier, and
//!   claude-code's own `<synthetic>` placeholder each land in their own flagged line with tokens
//!   shown and no dollars invented. Copilot gets token volumes and no money at all, because its
//!   billing regime is not recoverable from a transcript.
//! - **Same redaction line.** Aggregates, plus the model, modifier, and repository identifiers the
//!   archive itself recorded — clamped on the way out. No transcript content, by construction: the
//!   cost fold never reads a record's classification at all.
//!
//! # The redaction layer (qanungo #8)
//!
//! Both lanes above hold that line by *construction*: neither document has a path from a
//! transcript's free text to its output, and both prove it with canary fixtures. The next lane —
//! `qanungo standup` (#9) — breaks that, because rendering munshi's summary prose means rendering
//! text somebody typed into a terminal. [`redaction`] ships first so that it does not have to be
//! retrofitted: a [`Redactor`](redaction::Redactor) with two independently switched passes,
//! secrets on by default and profanity off, patterns anchored on structure rather than entropy,
//! and a report that counts what fired and never carries what it matched.
//!
//! It is deliberately **not** wired into `report` or `cost`. A filter over a document that carries
//! no content can only be decoration, and decoration in a security control invites the reader to
//! trust it. The standing rule from qanungo #8 stands: any new surface that renders transcript
//! content says how it satisfies that issue, and from here that means naming the redactor it
//! called and stamping [`PATTERN_REVISION`](redaction::PATTERN_REVISION) in its footer.
//!
//! The issue's other half — `0o600`/`0o700` on the blob cache — was already true and already
//! tested before this lane; see [`cache`].
//!
//! # The standup lane (qanungo #9)
//!
//! ```text
//! qanungo standup --last 7d
//! ```
//!
//! is the third lane and the first that renders prose. It reads the `summary.md` every snapshot
//! carries (munshi ADR 0009/0010) rather than the transcript beside it, parses it with the
//! promoted `munshi-transcript` parser, and emits the sessions grouped by repository, newest first,
//! with the window's decisions and open items rolled up beneath them ([`standup`],
//! [`standup_report`]). Four properties are what make it honest:
//!
//! - **The archive's own words.** No model, no reconstruction; the summary was written by the
//!   harness that was in the session. Qanungo selects, orders, groups, and deduplicates.
//! - **The redactor, on the way in.** This is the standing rule's first consumer: every string the
//!   document renders is scrubbed inside the fold, so the renderer has no unscrubbed copy to leak
//!   by accident, and the footer stamps the pattern revision, the passes that ran, and what fired
//!   as counts per pattern id.
//! - **No signal, no claim.** A session with no summary anywhere, an unparseable one, and munshi's
//!   own placeholder each land in Gaps with the reason, and never in the narrative.
//! - **Its own bounds.** A `summary.md` is kilobytes where a transcript is megabytes, so the
//!   download that fetches one believes a ceiling three orders of magnitude lower
//!   ([`patwari::MAX_DECLARED_SUMMARY_BYTES`]) — and the *same* mirror, cache, and sibling-fallback
//!   discipline otherwise, because a lane that selected its window differently would be describing
//!   a different week.
//!
//! # The dashboard (qanungo #5)
//!
//! ```text
//! qanungo dashboard --last 30d
//! ```
//!
//! is the fourth lane and the first that is not a document. It is the other three lanes' own
//! numbers, served: five score cards, the findings under them, the bill, the week's narrative, a
//! provenance footer, and nothing else. Four properties are the whole of it:
//!
//! - **It is a presentation, not a computation.** [`command::fold_coaching`],
//!   [`command::fold_cost`], and [`command::fold_standup`] are the same three calls `report`,
//!   `cost`, and `standup` make, and [`dashboard`] serializes what they return instead of rendering
//!   it as Markdown. A dashboard with its own fold would drift from the CLI beside it, and "the page
//!   and the terminal disagree about my scores" is not a bug anybody can act on.
//! - **In memory, on a timer.** A long-lived process re-syncs and re-folds every `--refresh`,
//!   swaps the served payload atomically, and pushes an SSE event so open pages re-fetch; a request
//!   is a memcpy. Per the 2026-08-24 grilling, process memory is the "disposable materialization"
//!   and the persistent event store stays deferred — sync dominates the fold, and a store would fix
//!   the smaller half.
//! - **Aggregates in the payload, and no way back to the archive.** Scores, rule ids, counts,
//!   content hashes, and evidence anchors; no transcript text in the document itself, and **no
//!   Patwari links** — the archive serves unredacted blobs, so a deep link would be a disclosure
//!   wearing a convenience.
//! - **Unauthenticated, and it says so.** Loopback by default; `--bind` on a tailnet address is how
//!   a phone or a TV reads it, and startup prints one line naming what that costs
//!   ([`dashboard_server::posture_line`]).
//!
//! ## The evidence-excerpt slice
//!
//! The rules used to produce verdicts and counts with no *locations*, so a finding could say six
//! calls failed and never show one. The fold now records bounded [`evidence`] anchors for every
//! rule whose counted signal is an event, and `GET /api/evidence/<hash>/<locator>` resolves one
//! anchor into one scrubbed event. Four properties make it something other than a transcript API:
//!
//! - **Additive.** No verdict, score, fire rate, or rendered report changes; the CLI's Markdown is
//!   byte-for-byte what it was, proved against production and pinned by a control fold in
//!   `tests/rules.rs`.
//! - **Honest per component.** A rule that measured a *shape* — Marathon, Heavily-resumed,
//!   Babysitting — anchors nothing and renders structural evidence instead: active time, sitting
//!   boundaries, cadence counts. Fire-and-forget does each in a different component and says so.
//! - **The counted event only.** Tool name, that event's own command and error/output text through
//!   the #8 redactor, timestamp. No neighbours, no request/response context, no raw tool payload.
//! - **Bounded, cached, launch-time.** At most ten anchors per finding per session; the blob must
//!   already be in the local cache, so no browser can induce archive traffic; only anchors the
//!   current payload names resolve at all; and [`RedactionArgs`](cli::RedactionArgs) is read once
//!   at startup, never per request. `--no-redact` on a routable bind gets its own very loud line.
//!
//! ## The standup-and-cost slice
//!
//! The page was one lane's numbers; it is now three. `--cost-last` (default `12w`) and
//! `--standup-last` (default `7d`) join `--last` (default `30d`), each defaulting to what its own
//! command defaults to, and one refresh folds all three. Four properties:
//!
//! - **Zero new computation, zero new rules.** `cost` and `standup` got the seam `report` got for
//!   V1: [`command::fold_cost`] and [`command::fold_standup`] are the bodies those commands had,
//!   and each command is now that call plus its renderer. Both documents are byte-for-byte what
//!   they were.
//! - **One generation, three sections.** The three folds happen in one call and publish one
//!   [`dashboard::Payload`], so a reader can never see a bill from one refresh beside a standup from
//!   another. A torn view across lanes is unrepresentable rather than unlikely.
//! - **The redaction line is now three lines, and says so.** Coaching and cost carry no verbatim, by
//!   construction, as their documents do. Standup carries prose scrubbed *by the fold* —
//!   [`command::FoldedStandup`] holds no pre-scrub string for a surface to leak — and the served
//!   section's strings are pinned equal to the fold's, so the page and `qanungo standup` cannot
//!   disagree and a second scrub cannot creep in.
//! - **Copilot's honesty rule rides the wire.** Copilot rows are token volumes with no money-shaped
//!   field anywhere and no blended total: a page cannot render a dollar figure it was never handed.
//!
//! - **Scopes are the same fold, selected.** A repository scope is the sessions the coaching fold
//!   already produced, grouped by the archive's own projection and handed back to
//!   [`scoring::Scorecard::fold_refs`] — never re-folded, never re-scored by a second formula. The
//!   harness axis needs no payload dimension of its own, because the scorecard is already per
//!   harness; the page's harness control reads a number rather than computing one. See
//!   [`scopes`].
//!
//! - **The timeline is that fold on a calendar.** Sessions and active time per **UTC day of archive
//!   completion** — the clock the window itself was cut on, which is what makes the bars sum back to
//!   the session count in the subtitle above them. It is grouped per scope by the same selection the
//!   scores are, and it carries numbers and ISO dates and no string at all. See [`timeline`].
//!
//! - **The heatmap is that fold on the operator's own clock.** Sessions and active time per **local
//!   hour of the weekday the work began** — each session's transcript first-activity instant shifted
//!   by its own [`metrics::SessionMetrics::utc_offset`], because "at 1 a.m." and "on a Sunday" are
//!   exactly the claims UTC misplaces. A session with no recorded offset is on no cell and counted,
//!   the same refusal the timeline makes for a missing archive time. Same selection per scope, and
//!   numbers and grid indices and no string at all. See [`heatmap`].
//!
//! What remains: per-device scope, waiting on a hostname to accrue in the archive.

pub mod ask;
pub mod ask_report;
pub mod cache;
pub mod cli;
pub mod command;
pub mod cost;
pub mod cost_report;
pub mod dashboard;
pub mod dashboard_server;
pub mod evidence;
pub mod format;
pub mod heatmap;
pub mod http;
pub mod metrics;
pub mod patwari;
pub mod pricing;
pub mod redaction;
pub mod report;
pub mod rules;
pub mod scopes;
pub mod scoring;
pub mod standup;
pub mod standup_report;
pub mod sync;
pub mod timeline;
pub mod verbatim;
