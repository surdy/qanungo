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

Bootstrapping. Full research + rationale: `~/repos/research/ai-coach/`. See issues for the phased plan.
