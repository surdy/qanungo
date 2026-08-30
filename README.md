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

A plain web app on the tailnet (laptop, phone, TV; no editor, no extension), mirroring munshi-dashboard's read-only, contract-consuming posture. Score cards + anti-pattern findings first, then the timeline, and then heatmaps/output/context-health/chronicle. Verbatim evidence is **redacted on the way to the browser** (toggleable, default on) and served by qanungo itself.

> The original plan had every finding, chart point, and session row deep-link into `session-recall` → the verified transcript in Patwari. The 2026-08-24 grilling on [#5](https://github.com/surdy/qanungo/issues/5) **retracted that**: Patwari serves raw blobs and never redacts, so such a link does not hand a browser a redacted transcript — it hands any tailnet device the whole unredacted one. Raw Patwari URLs therefore never appear in dashboard HTML; anything verbatim is cut from the local cache, redacted, and served by qanungo. The recall funnel stays a CLI/harness affordance, where your own shell already has raw access.

## Skills & agents

Thin read-only clients that call qanungo's API or the recall funnel ship in `contrib/` (as munshi ships `session-recall`): skills `/standup`, `/ask`, `/coach`, `/instructions-doctor`, `/skill-finder`, `/cost-review`; agents `coach`, `historian`, `instructions-editor`, `standup-writer`. None write derived data back into the archive — they render or propose; you decide.

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
arrows against the equal-length window immediately before it. Four lanes are fed by signals the fold
types today (Prompt Quality, Session Hygiene, Tool Mastery, and — since munshi#77 typed the
compaction markers — Context Management, off a session compacting its window over and over); Code
Review renders as *not scored* rather than taking a default — no signal, no claim. Every run recomputes all
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

**The redaction layer** ([#8](https://github.com/surdy/qanungo/issues/8)) shipped ahead of the first
surface that needed it: a scrub with two independently switched passes — secrets **on** by default,
profanity off — anchored on structure (a vendor prefix, a length class, a charset, a key name)
rather than on entropy, with every pattern's provenance dated and committed beside it
([`docs/redaction-patterns-2026-08-24.md`](docs/redaction-patterns-2026-08-24.md)). Its report
carries counts per pattern id and *nothing about what it matched* — no offset, no excerpt, not even
in `Debug` — because the thing that ends up in a footer or a panic message must not be the leak.
`report` and `cost` are deliberately not wired to it: a filter over a document that carries no
transcript content can only be decoration, and decoration in a security control invites the reader
to trust it.

**The standup lane** ([#9](https://github.com/surdy/qanungo/issues/9)) is the third lane and the
first that renders prose. `qanungo standup` reads the `summary.md` every snapshot already carries —
munshi's own curated record of the session, written when it was captured (munshi ADR 0009/0010) —
rather than the transcript beside it, and emits sessions grouped by repository (busiest first,
sessions newest first), then the window's decisions and open items rolled up across every repository
with exact repeats dropped. **No model reconstructs anything**: qanungo selects, orders, groups, and
deduplicates the archive's own words, which is what makes it cheaper and better-grounded than an LLM
chronicle over raw sessions. It is #8's first consumer, and the wiring is structural — the scrub
happens in the fold, so the renderer holds no unscrubbed copy of a field to leak by accident, and
the footer states which passes ran, what they fired as counts per pattern id, and the pattern
revision they fired from. A session with no summary anywhere, one this build cannot parse, and
munshi's own placeholder each land in **Gaps** with the reason, never in the narrative and never
silently dropped.

**The dashboard** ([#5](https://github.com/surdy/qanungo/issues/5)) is the fourth lane and the first
that is not a document. `qanungo dashboard` serves the other three lanes' own numbers — five score
cards, the findings under them, the quarter's bill, the week's narrative, a provenance footer — as
one embedded single-file page on a hand-rolled `std::net` server, theme-aware and legible from a
phone to a TV. It is a *presentation*, not a second computation: it calls the same three folds
`report`, `cost`, and `standup` call, and serializes the results instead of rendering them as
Markdown, so the page and the terminal cannot come to disagree about a score or a dollar. Each
section keeps its own lane's window — `--last 30d`, `--cost-last 12w`, `--standup-last 7d` — because
a score wants a month, a bill wants a quarter, and a standup wants a week you can read to the end of.
One refresh folds all three and publishes **one** payload, so a reader can never see a bill from one
refresh beside a standup from another. A long-lived process re-folds every `--refresh`, swaps the
served payload atomically, and pushes an SSE event so open pages re-fetch; a request is then a
memcpy. Per the issue's 2026-08-24 grilling the persistent event store stays **deferred** — sync
dominates the fold, and a store would fix the smaller half — so process memory is the disposable
materialization.

**What the served surface renders is three different claims, and it says which is which.** The
coaching and cost sections carry aggregates and hashes: scores, rule ids, counts, dollars, token
counts, the model and repository identifiers the archive itself recorded — each clamped on the way
out — and `sha256` content hashes. No transcript text reaches either, by construction, exactly as
their Markdown documents hold that line. The standup section is deliberately different: it renders
**verbatim-class archived prose**, the same `summary.md` text `qanungo standup` prints, and it
renders it **scrubbed** — the fold runs the [#8](https://github.com/surdy/qanungo/issues/8) redactor
on the way into its own types, so no unscrubbed string ever reaches the payload, and the served
strings are pinned equal to the fold's. A finding will also expand into the individual events its
rule counted, one scrubbed event each, from this server's own route. Redaction is **on by default**,
and `--no-redact` is **launch-time only**: it belongs to the process, never to a request, because a
scrub a browser could flip with a query string is a bypass with a nicer name — and turning it off on
a non-loopback bind prints its own very loud line. There is deliberately **no link into Patwari**,
which serves unredacted blobs; a hash is what you take to your own shell. Loopback by default;
`--bind` on a tailnet address is how a phone reads it, and startup prints one line saying that
nothing authenticates a caller and the tailnet is therefore the only boundary.

```sh
cargo run -- report --last 30d                       # the production archive on the LAN
cargo run -- report --last 7d --patwari-url http://127.0.0.1:8080
cargo run -- cost --last 12w                         # a quarter, in the units the grammar has
cargo run -- standup --last 7d                       # a week you can read to the end of
cargo run -- standup --last 30d --no-redact          # secrets unscrubbed, and the footer says so
cargo run -- dashboard --last 30d                    # http://127.0.0.1:8878, refolded every 5m
cargo run -- dashboard --cost-last 4w --standup-last 3d   # narrow the bill and the narrative
cargo run -- dashboard --bind 100.64.0.7:8878        # the tailnet, unauthenticated, and it says so
```

The window grammar takes `h`, `d`, and `w` and deliberately not `m`, which would read as either
minutes or months; `12w` is the honest spelling of a quarter, and is the cost lane's default. All
three of the dashboard's windows share that grammar — `--last` (default `30d`), `--cost-last`
(default `12w`), `--standup-last` (default `7d`), each defaulting to what its own command defaults
to — and they move independently, because narrowing the coaching window says nothing about what the
bill should cover. The dashboard's `--refresh` is a *disjoint* grammar — `s`, `m`, `h` — so neither
parser accepts the other's units, and an interval faster than a minute is refused rather than
clamped: a warm three-lane refresh measures about **45 s** against the production archive, and
polling near that is load on a LAN archive rather than a fresher page.

`--patwari-url` also reads `PATWARI_URL`; `--cache-dir` overrides the cache root — the blob cache
plus the snapshot index beside it — which otherwise lives in `$XDG_CACHE_HOME/qanungo` (falling
back to `~/.cache/qanungo`) at `0o700` / `0o600`. Both
flags work on every lane. The coaching report renders **aggregates, tool names, and content hashes
only**, and the cost report adds exactly the model, billing-modifier, and repository identifiers the
archive itself recorded, each clamped on the way out — never transcript content, in either. The
standup is the one document that does render archived prose, and it is the one that takes
`--no-redact` and `--filter-profanity`; there is no `--redact`, because losing the scrub should be
something a reader of the command line, and of the document's own footer, can see happened. Rule
thresholds are named constants in `crates/qanungo/src/rules.rs`, and the scoring
constants in `crates/qanungo/src/scoring.rs` are the same kind of knob — all explicitly arbitrary
until the footer's fold-cost and rule-firing data say otherwise. The prices in
`crates/qanungo/src/pricing.rs` are the opposite kind of number: not a knob at all, but a sourced
figure with a date, changed only by adding a row.

The dashboard also carries a **scope control**: one select for the repository the archive listed a
session under, one for the harness, narrowing the scores, the findings, and — for a repository —
the bill and the narrative together. Every scope is folded server-side and shipped with the payload
(there is no query string on that route, and a scope is never a per-request fold), so switching one
re-renders what the browser already holds. A scope's lanes are the same arithmetic over fewer
sessions: its fleet blend is the unweighted mean over the harnesses present *in that scope*, a
scope too small to read scores nothing rather than a phantom number, and the all/all view is the
whole-window payload unchanged.

And it carries a **timeline**: one inline-SVG chart of sessions per day, stacked by harness, with a
toggle to the same days measured in active hours instead — one chart with one axis, because two
y-scales on one plot invent a correlation the data does not have. A day is the **UTC calendar day
the archive finished the session's snapshot** — archive time, which is the clock the window itself
is cut on and therefore the only one on which the bars add up to the session count above them. It is
not the transcript's own clock and it is not local time. The chart narrows with the scope control,
carries a legend and the table of its own numbers, and the whole section on the wire is integers and
ISO dates with no string in it at all.

That is also why there is still **no hour-of-day or day-of-week heatmap**: a per-day volume survives
a missing local offset — a day is a day wherever you stand — while "worked at 1 a.m." and "worked on
a Sunday" are exactly the claims UTC misplaces, and they are the only claims that view exists to
make.

Everything else is still ahead: mirror hardening (#1), the rule DSL (#3), the narrator (#6), and the
dashboard's own remaining slices — per-*device* scope once a hostname has accrued in the archive,
and the heatmap once munshi#77's local offsets have accrued on real sessions. Full
research + rationale:
`~/repos/research/ai-coach/`. See
issues for the phased plan.
