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

pub mod cache;
pub mod cli;
pub mod command;
pub mod cost;
pub mod cost_report;
pub mod format;
pub mod http;
pub mod metrics;
pub mod patwari;
pub mod pricing;
pub mod report;
pub mod rules;
pub mod scoring;
pub mod sync;
