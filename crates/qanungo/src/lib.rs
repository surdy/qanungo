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
//! sync ([`sync`], [`cache`], [`patwari`]) → fold ([`metrics`]) → evaluate ([`rules`]) → emit
//! ([`report`]). Four metrics, six rules, Markdown on stdout, evidence cited by content hash,
//! and an instrumentation footer on every run.
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

pub mod cache;
pub mod cli;
pub mod command;
pub mod format;
pub mod http;
pub mod metrics;
pub mod patwari;
pub mod report;
pub mod rules;
pub mod sync;
