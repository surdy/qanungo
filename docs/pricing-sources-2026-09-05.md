# Date-versioned pricing research — amendment of 2026-09-05

Input for the cost lane's `claude-fable-5-1` gap. This file is an **amendment**, not a replacement.
[`pricing-sources-2026-08-23.md`](pricing-sources-2026-08-23.md) remains the provenance of the
original five rows, of the cache-multiplier reasoning, of the Copilot §2 argument, and of the
"not found" list in its §3. Read it first; this one records only what `PRICE_TABLE_REVISION` moved
for.

`PRICE_TABLE_REVISION` is now `2026-09-05`. It moved because the table **gained one row**
(`claude-fable-5-1`) — usage a `2026-08-23` report printed as unpriced tokens a `2026-09-05` report
bills in dollars, so the two are not claiming the same spend, which is the whole reason the constant
is stamped in a footer.

The same standing rule applies here as in the base file: **do not invent prices.** A figure below is
either quoted from a retrieved page or it is absent.

## 0. Why this amendment exists

A real-archive `qanungo cost` run on 2026-09-05 printed `no price row for model claude-fable-5-1` in
its footer: 528 messages and 129.0M tokens — 126.9M of them cache reads — fell out of the priced
total. The model is in the archive and was not in the table.

## 1. Anthropic API models (direct claude-code usage) — dollars per MTok

Primary source: https://platform.claude.com/docs/en/about-claude/pricing (retrieved **2026-09-05**),
cross-checked against https://platform.claude.com/docs/en/models/fable-5-1/overview.md (retrieved
2026-09-05) for the per-model price panel and the release date, and against
https://platform.claude.com/docs/en/about-claude/models/overview.md (retrieved 2026-09-05) for the
lineup row. The column meanings are the base file's §1 columns, unchanged.

| Archive model id | Official model | Effective from | Input | Output | 5m cache write | 1h cache write | Cache read |
|---|---|---|---|---|---|---|---|
| claude-fable-5-1 | Claude Fable 5.1 | 2026-09-01 (launch) | $10.00 | $50.00 | $12.50 | $20.00 | $0.25 |

Every one of those five figures is quoted, not derived. The pricing page's model table gives the
row verbatim — "Claude Fable 5.1 | $10 / MTok | $12.50 / MTok | $20 / MTok | $0.25 / MTok | $50 /
MTok" — and the model page repeats it as a Pricing panel (Input $10, Output $50, 5m cache write
$12.50, 1h cache write $20, Cache read $0.25).

**The cache-read multiplier is the exception the base file did not have.** §1 of
`pricing-sources-2026-08-23.md` states the cache prices as fixed multipliers of base input: 5m write
1.25x, 1h write 2x, cache read 0.1x. The write multipliers still hold for this row ($12.50 = 1.25 ×
$10, $20 = 2 × $10). **The read multiplier does not**: the pricing page footnotes the table with
"Cache hits and refreshes on Claude Fable 5.1 and Claude Mythos 5.1 are priced at 0.025x the base
input price. All other models use the standard 0.1x multiplier", and its prompt-caching section
repeats it as "0.1x base input price (0.025x on Claude Fable 5.1 and Claude Mythos 5.1)". So
`claude-fable-5-1` reads at **$0.25/MTok**, a quarter of `claude-fable-5`'s $1.00/MTok at the same
$10 input. Deriving this row's read rate from the family multiplier would have over-billed a
cache-heavy session fourfold — and claude-code sessions are cache-heavy by construction (the run in
§0 was 98% cache reads).

Launch date: the model page's Availability panel says **"Released September 1, 2026"**, and the
deprecations page's tentative retirement of "Not sooner than September 1, 2027" is consistent with
it (retirement commitments on that page sit exactly one year after each model's launch, for all five
rows already in the table). 2026-09-01 is four days before this retrieval, so the row covers every
`claude-fable-5-1` session this archive can hold.

Notes carried forward and checked against this row:

- **Fast mode**: the pricing page's fast-mode table lists Claude Opus 5 and Claude Opus 4.8 only.
  Fable 5.1 has no published fast tier, so its row carries `fast: None`, and `speed = "fast"` on it
  is flagged rather than billed at the base rate. (Unchanged posture from the base file.)
- **inference_geo**: Fable 5.1 is post-4.6, so the base file's "Claude 4.6+" scoping of the 1.1x
  US-only premium covers it and the row carries the multiplier. Still a value this archive has never
  held.
- **Batch**: $5/$25 (50% off), the same schedule as Fable 5. Not priced here — this table holds no
  non-standard `service_tier` and flags them instead.
- **Long context**: full 1M at standard pricing, no >200K surcharge.
- **Mythos 5.1** (`claude-mythos-5-1`) is listed on the pricing page at the identical schedule
  (limited availability, Project Glasswing). It is **not** added as a row: it has never appeared in
  this archive, and a row for a model nobody here has used would be an untested claim. Add it when a
  session shows up on it.
- No post-launch price change for Fable 5.1 — it is four days old.
- The base file's other four "current" rows were re-read on the same retrieval and are unchanged:
  Haiku 4.5 $1/$5, Opus 4.8 $5/$25, Fable 5 $10/$50 (cache read still $1.00), Sonnet 5 $2/$10,
  Opus 5 $5/$25. Sonnet 5's cancelled 2026-09-01 rise is confirmed cancelled on the page
  ("the previously scheduled increase … will not occur").

## 2. Top tier: the table's first real tie

`is_top_tier_model` ranks on the published **output** rate and shares an exact tie. Fable 5.1 lists
at $50 of output, the same as Fable 5, so from 2026-09-01 **both are top tier** — the first genuine
tie the shipped table has held. This is the documented behaviour, not a new decision: a successor at
the same published rate joins the tier rather than evicting its predecessor, because the ranking
reads the pricing page and the pricing page still lists both at $50. The premium-waste flag
therefore treats a wholly-Fable-5.1 session exactly as it already treats a wholly-Fable-5 one.

The input column agrees with the output column for this row ($10, tied with Fable 5), so the
ranking's standing assumption — output order equals input order across every pair of rows — is
unbroken.

## 3. Not found — no authoritative figure (do not price)

- Nothing new. The base file's §3 stands unchanged.
