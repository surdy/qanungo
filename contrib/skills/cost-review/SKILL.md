---
name: cost-review
description: Review AI-coding spend from `qanungo cost` — dollars by model and repository, caching leverage, window-over-window drift. Use when the user asks what their AI coding costs, where tokens went, whether caching is paying off, or how spend is trending.
---

# cost-review

The interpretation half of qanungo's cost lane (qanungo#12). `qanungo cost` computes the *numbers* — priced from a date-versioned table with committed provenance; this skill reads them with the user: where the money went, what moved, what to do about it.

## Steps

1. **Run the command.** `qanungo cost --last 12w` (a quarter; window as the user asks — `h`/`d`/`w`, no `m` by design). Output: totals, by-model and by-repository breakdowns, caching savings, comparison-window delta, a flagged section, the top-tier section when there is one, and the footer with the price-table revision.
2. **Interpret within the lane's honesty rules:**
   - **Dollars are claude-code only**, at Anthropic API list prices — a *what-it-would-cost* reference, not a bill.
   - **Copilot never gets dollars.** Its rows are token volumes; the billing regime behind them is unknowable from a transcript. Never convert, estimate, or "roughly" price copilot usage.
   - **Caching savings are real leverage**: the "would-be" number is what the same traffic costs uncached — lead with it when the user asks whether caching matters.
   - The **comparison delta** is adjacent equal windows; call drift by what changed (sessions? model mix? repos?) using the breakdowns, not by the total alone.
3. **Read the flagged section before concluding anything.** Flags are the lane refusing to guess (unpriced model/date, zero-token oddities). If a model is flagged unpriced, the fix is a new dated entry in the price table (`docs/pricing-sources-*.md`, provenance required) — offer to draft it; that is a qanungo repo change under normal prompts, never silent.
4. **Read "Small sessions at the top price tier" as a reading, never an accusation.** When the section is present it lists individual sessions whose *whole* measured usage priced at the dearest rate the price table held on the day the archive took each one, and whose size fell under both floors (billed messages, output tokens) named in the section itself. Honesty rules, all of them load-bearing:
   - **It is a spend observation, not a practice score.** No lane scores model choice, and the section carries no verdict, rate, or trend. The archive records what a session cost and how much it wrote; it does not record what it was *worth*. Say what it shows — "this session cost $X and produced N tokens over M messages on `<model>`" — and let the user judge.
   - **"Top tier" is the table's answer, not a model you know.** It is derived from the date-versioned price table at each session's own archive date, so it moves when the catalogue moves and it may name a model the user does not think of as premium. Never substitute your own opinion of which models are expensive.
   - **The constants are arbitrary-until-measured.** Quote them from the section; don't defend them as thresholds and don't invent your own. If the user thinks a floor is wrong, that is a qanungo change (`crates/qanungo/src/cost.rs`), proposed as a diff.
   - **Never extrapolate past the listed sessions.** No "you probably do this a lot", no share-of-spend estimate for unlisted sessions, no projection. Sessions the build could not read whole — a cheaper model beside the dear one, an unpriced model, a token-bearing `<synthetic>` placeholder — are deliberately absent, so the list is a floor on the shape and never a census of it.
   - **The fix conversation, if the user wants one, is model choice for *that kind of session*** — the question "would a cheaper model have served a question-and-answer session as well?" — and it is theirs to answer, from sessions they can open by `source_hash`. Absent the section, say nothing about model choice at all.
5. **Recommendations stay traceable.** Tie every tip ("this repo's spend tripled", "long sessions dominate") to a line of the output.

## Boundaries

- Read-only against the archive. The only writes this skill may ever propose are in the qanungo repo and are shown as a diff first: a price-table entry (with provenance), or a change to a top-tier floor. Never edit either silently, and never quote a figure computed with a floor other than the one the run printed.
- Costs come from record-level usage deduped by `message_id` upstream — don't re-derive numbers from raw transcripts; the command is the ground truth.
- The footer's price-table revision belongs in any figure the user is going to quote elsewhere.
