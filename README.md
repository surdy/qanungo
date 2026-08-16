# darpan

*दर्पण — the mirror.* Read-time analytics and coaching over your AI-coding session archive. darpan holds your coding habits up to a mirror: how you prompt, how you drive the agents, where sessions rot, when you overwork — scored, trended across every machine, and every finding linked to the verbatim transcript that proves it.

darpan is the read-side analysis client deferred in munshi [ADR 0012](https://github.com/surdy/munshi/blob/main/docs/adr/0012-defer-the-analysis-client-until-a-first-consumer-exists.md) (working name *Qanungo* — the officer who audited the patwaris' records). Building it is the "first consumer" event that ADR waits for.

## Where it sits

```
coding session → munshi (capture, normalize, summarize) → patwari (permanent, verified archive)
                                                                  │
                                                       darpan (this repo)
                                                       incremental mirror → derive → coach → dashboard
```

- **munshi** captures and normalizes sessions; **patwari** is the central, permanent, content-addressed archive of every machine. darpan consumes them; it never captures.
- darpan keeps a **disposable** incremental mirror of the archive and a rebuildable SQLite event store. Rebuild = delete and resync. **Patwari is the only stable interface**; the derived store is a private implementation detail.
- Interpretation is **read-time** (munshi [ADR 0011](https://github.com/surdy/munshi/blob/main/docs/adr/0011-interpret-transcripts-at-read-time-through-a-shared-streaming-crate.md)): darpan folds metrics over `munshi-transcript`'s typed event stream, consumed as a git dependency pinned to a tag. Nothing derived is ever written back into an immutable snapshot — improve a rule, resync, re-derive.

## What it does

- **Metric derivation** — folds coaching signals (tokens, tool confirmations, slash commands, file refs, code line counts, compaction, cancels, timing) over the event stream into a per-session metric record.
- **Rule engine** — anti-pattern detectors as Markdown files with a small `scan → match → aggregate → check` DSL, interpreted deterministically. Built-in pack is trusted; personal/project rules are prompted-and-validated before first use. Learned from Microsoft's AI-Engineering-Coach, ported to Rust.
- **Scoring** — five practice lanes (Prompt Quality, Session Hygiene, Code Review, Tool Mastery, Context Management), 0–100, with week-over-week / month-over-month trends. Deterministic and reproducible.
- **Web dashboard** — a plain web app on the tailnet. Laptop, phone, TV. No editor, no extension. Scope selector spans every device, not one window.
- **Recall bridge** — every finding, chart point, and session row deep-links into the `session-recall` funnel: "show me" opens the uncapped, verified transcript in Patwari.
- **Narrator (opt-in)** — an LLM pass turns findings + munshi's human session summaries into prose coaching. Strictly downstream of the deterministic core; never in the scoring path. Curated prose output goes to Notesmith.

## Design rules (inherited, non-negotiable)

1. **Read-time, re-derivable.** No derived data frozen at capture (ADR 0011).
2. **Patwari never interprets.** No metrics/scores stored in the archive (ADR 0012).
3. **Deterministic scoring, optional narration.** Scores are facts you can recompute; the LLM only narrates.
4. **Evidence is a hash.** Findings carry the Patwari `source_hash`, not a truncated snippet.
5. **Grow the typed surface per consumer.** New `munshi-transcript` signals land only when a darpan metric consumes them.

## Status

Bootstrapping. Full research + rationale: `~/repos/research/ai-coach/`. See issues for the phased plan.
