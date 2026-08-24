# qanungo

*क़ानूनगो — the officer who audited the patwaris' records.* qanungo is the **read-side application client** over your AI-coding session archive: it mirrors what munshi captured and patwari stored, derives insight at read time, and turns it into standups, coaching, cost analysis, instruction suggestions, and answers to plain-language questions about your past work — across every machine and every harness, from any browser. No VS Code.

qanungo is the read-side analysis client designed and deferred in munshi [ADR 0012](https://github.com/surdy/munshi/blob/main/docs/adr/0012-defer-the-analysis-client-until-a-first-consumer-exists.md). Building it is the "first consumer" event that ADR waits for. It exposes a web dashboard — just "the qanungo dashboard," a UI surface in this repo, not a separately-named product.

Lineage: **munshi** (the scribe) → **patwari** (the record-keeper) → **qanungo** (the auditor who reviews the records).

## Where it sits

```
coding session → munshi (capture, normalize, summarize) → patwari (permanent, verified archive)
                                                                  │
                                                        qanungo (this repo)
                                                        incremental mirror → derive → application commands → dashboard
```

- **munshi** captures and normalizes sessions; **patwari** is the central, permanent, content-addressed archive of every machine. qanungo consumes them; it never captures.
- qanungo keeps a **disposable** incremental mirror of the archive and a rebuildable SQLite event store. Rebuild = delete and resync. **Patwari is the only stable interface**; the derived store is a private implementation detail.
- Interpretation is **read-time** (munshi [ADR 0011](https://github.com/surdy/munshi/blob/main/docs/adr/0011-interpret-transcripts-at-read-time-through-a-shared-streaming-crate.md)): qanungo folds metrics over `munshi-transcript`'s typed event stream, consumed as a git dependency pinned to a tag. Nothing derived is ever written back into an immutable snapshot — improve a rule, resync, re-derive.

## Application commands (the read-side suite)

ADR 0012 anticipated qanungo carrying "application commands such as a prompt-corpus exporter or a session chronicle." The suite:

- **coach** — deterministic anti-pattern detection + five practice scores (Prompt Quality, Session Hygiene, Code Review, Tool Mastery, Context Management), with WoW/MoM trends. Rules are Markdown files with a small `scan → match → aggregate → check` DSL (learned from Microsoft's AI-Engineering-Coach, ported to Rust). Every finding carries the Patwari `source_hash` as evidence, not a snippet.
- **chronicle / standup** — a time-boxed narrative of what you shipped, aggregated from munshi's per-session summaries across machines and repos. (GitHub Copilot CLI's `/chronicle standup`, generalized cross-harness and grounded in curated summaries.)
- **ask** — plain-language questions over your history ("have I touched the payments API?"), backed by the `session-recall` funnel: Notesmith search → hash → verbatim grep in Patwari.
- **instructions-doctor** — mines sessions for repeated corrections and rework, then proposes concrete `CLAUDE.md` / `AGENTS.md` edits, pointing at the exact transcript moment a missing instruction caused the rework.
- **cost** — token/cost breakdown by model/repo/machine, and premium-waste flags.
- **skill & agent finder** — detects repeated multi-step prompt patterns and drafts a reusable skill or custom subagent from them.

## The dashboard

A plain web app on the tailnet (laptop, phone, TV; no editor, no extension), mirroring munshi-dashboard's read-only, contract-consuming posture. Score cards + anti-pattern findings first, then timeline/heatmaps/output/context-health/chronicle. Every finding, chart point, and session row deep-links into `session-recall` → the verified transcript in Patwari, **redacted** on the way to the browser (redaction is toggleable, default on).

## Skills & agents

Thin read-only clients that call qanungo's API or the recall funnel ship in `contrib/` (as munshi ships `session-recall`): skills `/standup`, `/coach`, `/instructions-doctor`, `/skill-finder`, `/cost-review`; agents `coach`, `historian`, `instructions-editor`, `standup-writer`. None write derived data back into the archive — they render or propose; you decide.

## Design rules (inherited, non-negotiable)

1. **Read-time, re-derivable.** No derived data frozen at capture (ADR 0011).
2. **Patwari never interprets.** No metrics/scores stored in the archive (ADR 0012).
3. **Deterministic scoring, optional narration.** Scores are facts you can recompute; the LLM only narrates. Curated prose output goes to Notesmith.
4. **Evidence is a hash.** Findings carry the Patwari `source_hash`, not a truncated snippet.
5. **Grow the typed surface per consumer.** New `munshi-transcript` signals land only when a qanungo command consumes them.

## Status

The **P0 vertical slice** ([#7](https://github.com/surdy/qanungo/issues/7)) exists: a `qanungo` CLI
that syncs a minimal content-addressed mirror of the recent archive, folds four metrics over
`munshi-transcript`'s typed events, evaluates six hardcoded rules, and emits a Markdown coaching
report — findings as Problem / Action / `source_hash` evidence, with an instrumentation footer on
every run.

**Scores and trends** ([#4](https://github.com/surdy/qanungo/issues/4)) sit on top of it: the five
practice lanes, scored 0–100 **per harness** from the window's own readings, with window-over-window
arrows against the equal-length window immediately before it. Three lanes are fed by signals the
fold types today (Prompt Quality, Session Hygiene, Tool Mastery); Code Review and Context Management
render as *not scored* rather than taking a default — no signal, no claim. Every run recomputes all
of it with the current rule pack
([ADR 0001](docs/adr/0001-recompute-all-history-with-the-current-rule-pack.md)) and stamps the pack's
digest into the footer, so two reports compare only when that stamp matches. Every timestamp is UTC:
nothing in munshi types the capture machine's local offset yet, so the report says UTC rather than
guessing one.

**The cost lane** ([#12](https://github.com/surdy/qanungo/issues/12)) is the second lane over the
same slice: `qanungo cost` folds the per-message model and token usage `munshi-transcript` types
(munshi#77) into a token/cost breakdown by model and by repository. Three things make it honest.
It **deduplicates by `message_id` before summing** — one API message reaches a transcript as
several records repeating its `usage` verbatim, and summing records over-counts output tokens
2.6-fold. It prices from a **static, date-versioned table** whose provenance is committed beside it
([`docs/pricing-sources-2026-08-23.md`](docs/pricing-sources-2026-08-23.md)), selecting the row
effective at each session's archive time, with cache writes priced from the per-tier buckets
rather than from the undifferentiated total (the two TTLs bill at different multiples, and
claude-code uses the 1-hour cache exclusively). And it **says what it cannot price**: a model with
no row, an unrecognized billing modifier, a cache write with no tier, and Claude Code's own
`<synthetic>` placeholder each get their own flagged line, tokens shown and no dollars invented.
Dollars are Anthropic API **list** prices for claude-code sessions only — the archive does not know
the account's billing plan — and Copilot sessions get output-token volumes and no money at all,
because a transcript cannot say which of Copilot's two billing regimes the account was on. The
by-machine cut the issue asks for is deferred rather than faked: Patwari's session projection
carries `repository` and no hostname.

```sh
cargo run -- report --last 30d                       # the production archive on the LAN
cargo run -- report --last 7d --patwari-url http://127.0.0.1:8080
cargo run -- cost --last 12w                         # a quarter, in the units the grammar has
```

The window grammar takes `h`, `d`, and `w` and deliberately not `m`, which would read as either
minutes or months; `12w` is the honest spelling of a quarter, and is the cost lane's default.

`--patwari-url` also reads `PATWARI_URL`; `--cache-dir` overrides the transcript cache, which
otherwise lives in `$XDG_CACHE_HOME/qanungo` (falling back to `~/.cache/qanungo`) at `0o700` /
`0o600`. Both flags work on either lane. The coaching report renders **aggregates, tool names, and
content hashes only**, and the cost report adds exactly the model, billing-modifier, and repository
identifiers the archive itself recorded, each clamped on the way out — never transcript content, in
either. Rule thresholds are named constants in `crates/qanungo/src/rules.rs`, and the scoring
constants in `crates/qanungo/src/scoring.rs` are the same kind of knob — all explicitly arbitrary
until the footer's fold-cost and rule-firing data say otherwise. The prices in
`crates/qanungo/src/pricing.rs` are the opposite kind of number: not a knob at all, but a sourced
figure with a date, changed only by adding a row.

Everything else is still ahead: mirror hardening (#1), the rule DSL (#3), the dashboard (#5), the
narrator (#6). Full research + rationale: `~/repos/research/ai-coach/`. See
issues for the phased plan.
