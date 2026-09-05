# Date-versioned pricing research — retrieved 2026-08-23

Input for qanungo#12 (cost lane). Every figure sourced; unknowns explicitly listed. Do not invent prices.

> **Superseded as the current revision.** This file has been amended once:
> [`pricing-sources-2026-09-05.md`](pricing-sources-2026-09-05.md) adds the `claude-fable-5-1` row,
> whose cache read is priced at 0.025x base input rather than the 0.1x §1 below states for every
> other model. **`PRICE_TABLE_REVISION` is now `2026-09-05`** (`crates/qanungo/src/pricing.rs`), not
> the date in this file's title. No figure below changed — the five rows here were re-read on
> 2026-09-05 and all still stand — and this file remains the provenance of every one of them. Read
> both, newest last.

**This file is the provenance of the original five rows of `crates/qanungo/src/pricing.rs`.** Every
row of that table came from here or from a dated amendment beside it and from nowhere else, and each
one's doc comment names the file and section it came from. It is
committed as it was researched so that a number in the binary can always be traced to a source and a
retrieval date rather than to somebody's memory of a pricing page. Changing a price means adding a
row with a new `effective_from` **and** amending this file with the source that says so — never
editing a figure in place, because a report of last quarter's spend has to keep pricing last
quarter at last quarter's rates.

## 1. Anthropic API models (direct claude-code usage) — dollars per MTok

Primary source: https://platform.claude.com/docs/en/about-claude/pricing (retrieved 2026-08-23).
Cache prices are fixed multipliers of base input: 5m write = 1.25x, 1h write = 2x, cache read = 0.1x.
No documented change to these multipliers during the archive window (Oct 2025 → today). (The read
multiplier is no longer universal: Claude Fable 5.1 reads at 0.025x — see the 2026-09-05 amendment.
The write multipliers still hold for every row in the table.)

| Archive model id | Official model | Effective from | Input | Output | 5m cache write | 1h cache write | Cache read |
|---|---|---|---|---|---|---|---|
| claude-haiku-4-5-20251001 | Claude Haiku 4.5 | 2025-10-15 (launch) | $1.00 | $5.00 | $1.25 | $2.00 | $0.10 |
| claude-opus-4-8 | Claude Opus 4.8 | 2026-05-28 (launch) | $5.00 | $25.00 | $6.25 | $10.00 | $0.50 |
| claude-fable-5 | Claude Fable 5 | 2026-06-09 (launch) | $10.00 | $50.00 | $12.50 | $20.00 | $1.00 |
| claude-sonnet-5 | Claude Sonnet 5 | 2026-06-30 (launch) | $2.00 | $10.00 | $2.50 | $4.00 | $0.20 |
| claude-opus-5 | Claude Opus 5 | 2026-07-24 (launch) | $5.00 | $25.00 | $6.25 | $10.00 | $0.50 |
| `<synthetic>` | Not a model (claude-code placeholder for synthetic/error messages) | — | $0 | $0 | $0 | $0 | $0 |

Versioning notes (documented history only):
- **Sonnet 5**: launched 2026-06-30 at $2/$10 as introductory pricing through 2026-08-31 with a
  scheduled rise to $3/$15 on 2026-09-01; on 2026-08-10 Anthropic made the intro price permanent —
  the increase will not occur. Net: $2/$10 applies to all Sonnet 5 usage, past and future.
- **Fast mode** (Opus 4.8 and Opus 5): distinct tier at $10 in / $50 out, cache multipliers stack on
  that base. Usage records would need a `speed` flag to detect it (claude-code usage carries a
  `speed` field — NOT promoted in the current munshi pull; note as a limitation or a future pull).
- **Long-context**: no >200K surcharge for Claude 4.6+ models (full 1M at standard pricing);
  Haiku 4.5 is 200K-only. The old 2x >200K premium applied only to Sonnet 4/4.5 1M beta — absent
  from this archive.
- Other modifiers only if flagged in usage: inference_geo "us" = 1.1x (Claude 4.6+); Batch = 0.5x.
- **inference_geo, measured on the archive 2026-08-23**: of the same 61,184 claude usage records,
  61,122 read `not_available` and the remaining 62 record no readable value; `"us"` never occurs.
  `not_available` is the API stating that **no geo-routing premium applied** — it is the field's
  ordinary state and the base-rate case, NOT an unknown region. Reading it as one flagged 100% of
  the first production run (311 sessions, 35,001 messages, 8.2B tokens) as unpriced, which is how
  the misreading was caught. The 1.1x above therefore applies to a value this archive has never
  held; keep it for correctness on `"us"` and keep flagging genuinely unrecognized regions, so a
  new routing premium cannot be silently priced at base.
- **Cache-write tier, measured on the archive 2026-08-23**: of 389.8M cache-write tokens across all
  61,184 claude usage records, ephemeral_1h = 389,777,788 and ephemeral_5m = 0 — claude-code uses
  the 1h cache exclusively. Cache writes MUST be priced from the promoted per-tier buckets
  (cache_1h at 2x base, cache_5m at 1.25x base), never from the undifferentiated total at an
  assumed 5m rate (that would under-bill 1.6x). Buckets may exceed the total by a small residue
  (9,108 tokens archive-wide); prefer buckets when present.
- Each model's launch date is its first price point (did not exist before); no post-launch price
  change found for any of the five.
- Fable 5 availability gap 2026-06-12 → 2026-06-30 (export-control review) — not a price change.

## 2. GitHub Copilot billing — two regimes split at 2026-06-01

**Before 2026-06-01 (and legacy annual plans until renewal): premium requests x model multipliers.**
Per-token dollars are NOT defined in this regime. Current legacy multiplier table
(docs.github.com, retrieved 2026-08-23), archive-relevant models:
Claude Opus 4.6 / 4.7 / 4.8 = 27x; Claude Sonnet 4.6 = 9x; Claude Sonnet 4.5 = 6x;
Claude Haiku 4.5 = 0.33x; GPT-5.5 = 57x; GPT-5.4 = 6x; GPT-5.4 mini = 6x; GPT-5.3-Codex = 6x;
Gemini 3.1 Pro = 6x. Auto-model-selection: 10% discount. Multipliers INCREASED on 2026-06-01;
the pre-June table is not authoritatively archived (secondary sources conflict) — UNKNOWN, do not
guess. Claude Opus 5, Claude Sonnet 5, GPT-5.6 Sol/Terra are absent from the legacy table
(usage-based only).

**From 2026-06-01 (monthly/new plans): usage-based billing in GitHub AI Credits, 1 credit = $0.01.**
Published per-MTok rates (docs.github.com models-and-pricing, retrieved 2026-08-23):
Anthropic models mirror the Anthropic API rates above (incl. cache rates). GPT rates:
GPT-5.5 $5/$30 (long-context $10/$45); GPT-5.6 Sol $2/$10 + $2.50 cache-write (PROMOTIONAL 50%
off through 2026-09-03); GPT-5.6 Terra $2/$12; GPT-5.4 $2.50/$15; GPT-5.4 mini $0.75/$4.50;
GPT-5.3-Codex $1.75/$14; Gemini 3.1 Pro $2/$12 (long-context $4/$18).

**Design conclusion (verified/refined):** for Copilot sessions, dollarization is soft even
post-June (can't tell plan regime, included-allotment vs overage, long-context tier, or promo
applicability from a transcript). Report token volumes as primary for Copilot; any credit-equivalent
figure must be labeled an estimate. Hard dollar figures: Anthropic API (claude-code) usage only.
Note also: Copilot per-message signal is output tokens ONLY (no per-message input/cache), so even
token-volume claims for Copilot are output-side; full aggregates live in session.shutdown (not
promoted, ~2/3 of sessions).

## 3. Not found — no authoritative pricing (do not price)

- Copilot org fine-tunes (octodemo/...) — unknown.
- gemini-3.1-pro-preview — matched to "Gemini 3.1 Pro" by assumption; -preview has no separate rate.
- Pre-2026-06-01 Copilot multiplier table with effective dates — unknown (sources conflict).
- Anthropic cache-multiplier history — current 1.25x/2x/0.1x confirmed; no change announcement found,
  but absence of announcement is the only evidence.

## Sources

- https://platform.claude.com/docs/en/about-claude/pricing (official, current prices)
- https://platform.claude.com/docs/en/about-claude/models/overview (official, Fable 5 availability)
- https://docs.github.com/en/copilot/reference/copilot-billing/models-and-pricing (official, usage-based)
- https://docs.github.com/en/copilot/reference/copilot-billing/request-based-billing-legacy/model-multipliers-for-annual-plans (official, legacy)
- https://github.blog/news-insights/company-news/github-copilot-is-moving-to-usage-based-billing/
- https://www.anthropic.com/news/claude-opus-5 · /claude-sonnet-5 · /claude-haiku-4-5
- https://x.com/claudeai/status/2086891169217122586 (Sonnet 5 permanent pricing)
- Launch-date corroboration (secondary): tech-ish.com (Opus 5, Jul 24), coursiv.io (Opus 4.8, May 28),
  enterprisedna.co (Sonnet 5 freeze, Aug 10), llm-stats.com (Haiku 4.5, Oct 15 2025).
All retrieved 2026-08-23.
