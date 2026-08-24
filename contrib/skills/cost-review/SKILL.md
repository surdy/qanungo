---
name: cost-review
description: Review AI-coding spend from `qanungo cost` — dollars by model and repository, caching leverage, window-over-window drift. Use when the user asks what their AI coding costs, where tokens went, whether caching is paying off, or how spend is trending.
---

# cost-review

The interpretation half of qanungo's cost lane (qanungo#12). `qanungo cost` computes the *numbers* — priced from a date-versioned table with committed provenance; this skill reads them with the user: where the money went, what moved, what to do about it.

## Steps

1. **Run the command.** `qanungo cost --last 12w` (a quarter; window as the user asks — `h`/`d`/`w`, no `m` by design). Output: totals, by-model and by-repository breakdowns, caching savings, comparison-window delta, a flagged section, and the footer with the price-table revision.
2. **Interpret within the lane's honesty rules:**
   - **Dollars are claude-code only**, at Anthropic API list prices — a *what-it-would-cost* reference, not a bill.
   - **Copilot never gets dollars.** Its rows are token volumes; the billing regime behind them is unknowable from a transcript. Never convert, estimate, or "roughly" price copilot usage.
   - **Caching savings are real leverage**: the "would-be" number is what the same traffic costs uncached — lead with it when the user asks whether caching matters.
   - The **comparison delta** is adjacent equal windows; call drift by what changed (sessions? model mix? repos?) using the breakdowns, not by the total alone.
3. **Read the flagged section before concluding anything.** Flags are the lane refusing to guess (unpriced model/date, zero-token oddities). If a model is flagged unpriced, the fix is a new dated entry in the price table (`docs/pricing-sources-*.md`, provenance required) — offer to draft it; that is a qanungo repo change under normal prompts, never silent.
4. **Recommendations stay traceable.** Tie every tip ("this repo's spend tripled", "long sessions dominate") to a line of the output. The premium-waste rule (qanungo#3/#12) is future work — don't improvise it.

## Boundaries

- Read-only against the archive; the only write this skill may ever propose is a price-table entry in the qanungo repo, shown as a diff first.
- Costs come from record-level usage deduped by `message_id` upstream — don't re-derive numbers from raw transcripts; the command is the ground truth.
- The footer's price-table revision belongs in any figure the user is going to quote elsewhere.
