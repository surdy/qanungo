<p align="center">
  <img src="brand/header.svg" alt="qanungo — the auditor who reads the records" width="720">
</p>

qanungo is the **read-side application client** over your AI-coding session archive: it mirrors what
munshi captured and patwari stored, derives insight at read time, and turns it into standups,
coaching, cost analysis, instruction suggestions, and answers to plain-language questions about your
past work — across every machine and every harness, from any browser. No VS Code.

## Where this fits

qanungo is the **audit** side of a three-tool suite: it reads the archive the other two fill, and
never captures or stores anything itself. *Munshi writes the record; Patwari keeps the archive;
Qanungo audits it.*

**Start at [daftar](https://github.com/surdy/daftar)** — the suite's front door, with the pipeline
diagram, the install order, and a fifteen-minute path from nothing to a first report.

qanungo is the read-side analysis client designed and deferred in munshi
[ADR 0012](https://github.com/surdy/munshi/blob/main/docs/adr/0012-defer-the-analysis-client-until-a-first-consumer-exists.md).
Building it is the "first consumer" event that ADR waits for. It exposes a web dashboard — just
"the qanungo dashboard," a UI surface in this repo, not a separately-named product.

## Trust and authentication

**There is no authentication anywhere in this suite, and that is a design decision rather than an oversight.** qanungo talks to patwari with no credential because patwari has none to ask for: this was built as one person's private tooling for their own LAN and tailnet, where every client that can reach the archive is already trusted. The dashboard is unauthenticated for the same reason and refuses nobody — anyone who can open the port sees everything it serves — so bind it to loopback (the default, `127.0.0.1:8878`) or to a private network only, never to the open internet; on a non-loopback bind it prints a startup line saying out loud that nothing authenticates a caller and the network is therefore the only boundary. Redaction is on by default on every surface that renders archived text, and turning it off is a launch-time flag that announces itself. Anyone is welcome to take this suite and adapt it — if you need real multi-user access control, that is yours to add, and nothing here pretends to have it.

## Install

qanungo builds with a Rust 1.85+ toolchain (edition 2024) and reads a patwari archive over HTTP. There is **no default archive**: `--patwari-url` is required on every command, and it also reads the `PATWARI_URL` environment variable, so set it once in your shell.

```sh
cargo build --release
export PATWARI_URL=https://patwari.example.net   # your own archive's base URL
cargo run -- report --last 30d
```

How that archive gets served is yours to decide — a host on your own network, a container, a reverse proxy in front of it; qanungo needs only the base URL and unauthenticated read access to it. Exporting `PATWARI_URL` is also what makes the skills in `contrib/skills/` work, since they call the commands with no URL flag of their own.

## Quick start

Every line below assumes `PATWARI_URL` is already exported (see [Install](#install)); without it, or
without `--patwari-url`, a command stops and says so rather than guessing at an archive.

```sh
cargo run -- report --last 30d                       # your archive, from PATWARI_URL
cargo run -- report --last 7d --patwari-url http://127.0.0.1:8080
cargo run -- cost --last 12w                         # a quarter, in the units the grammar has
cargo run -- standup --last 7d                       # a week you can read to the end of
cargo run -- standup --last 30d --no-redact          # secrets unscrubbed, and the footer says so
cargo run -- ask "why did we pick zstd" --limit 5    # ranked over the archive's own summaries
cargo run -- ask "payments API" --last 12w --verbatim  # then dig into those same hits' transcripts
cargo run -- dashboard --last 30d                    # http://127.0.0.1:8878, refolded every 5m
cargo run -- dashboard --cost-last 4w --standup-last 3d   # narrow the bill and the narrative
cargo run -- doctor                                  # all of history, because that is the question
cargo run -- doctor --last 4w                        # or narrow it, on the same grammar
cargo run -- doctor --clusters-per-repo 50           # and read the clusters the default cut hides
cargo run -- flows                                   # what do I keep asking for, anywhere
cargo run -- flows --clusters 50 --flows 40          # each section's cut is a default, not a ceiling
cargo run -- dashboard --bind 192.0.2.10:8878        # a routable address on your private network, unauthenticated, and it says so
cargo run -- report --last 30d --json | jq '.data.lanes'   # the same fold, for a program instead of a person
```

The window grammar takes `h`, `d`, and `w` and deliberately not `m`, which would read as either
minutes or months; `12w` is the honest spelling of a quarter, and is the cost lane's default. `ask`
is the one lane with **no** default window — a query with no `--last` searches the whole archive,
because "have I ever done this" is a question about all of it. All three of the dashboard's windows
share that grammar — `--last` (default `30d`), `--cost-last` (default `12w`), `--standup-last`
(default `7d`), each defaulting to what its own command defaults to — and they move independently,
because narrowing the coaching window says nothing about what the bill should cover. The dashboard's
`--refresh` is a *disjoint* grammar — `s`, `m`, `h` — so neither parser accepts the other's units,
and an interval faster than a minute is refused rather than clamped: a warm three-lane refresh
measures about **13 s** against the production archive (the snapshot index cut it from ~45 s), and
polling near that is load on a LAN archive rather than a fresher page.

`--json` is on every document lane — `report`, `cost`, `standup`, `ask`, `doctor`, `flows` — and
**Markdown stays the default**, because the documents are what this tool is for. It is the same
fold, never a second one: the coaching, cost and standup sections are the very ones the dashboard
serves at `/api/data`, so `report --last 30d --json | jq '.data.lanes[0].fleet.score'` and the
score on the page are the same number by construction. Every document wears one envelope —
`schema_version`, `command`, `window`, `rule_pack` (the full digest, not the footer's short stamp),
`generated_at`, `provenance`, and `data` — with the Markdown footer's own figures in `provenance`
(fold and sync time, sessions listed and folded, bytes, cache hits and misses) rather than dropped.
The scrub does not change with the medium: `report` and `cost` carry no transcript content in either
form, and the four lanes that render archived text render it scrubbed in both, under the same
`--no-redact` and `--filter-profanity`. Errors still go to stderr, so a failed run gives `jq`
nothing rather than a document with an error sentence in it.

`--patwari-url` is **required** — there is no built-in archive to fall back on — and it also reads
`PATWARI_URL`, so set that once in your shell and no lane needs the flag again; `--cache-dir`
overrides the cache root — the blob cache plus the snapshot index beside it — which otherwise lives
in `$XDG_CACHE_HOME/qanungo` (falling back to `~/.cache/qanungo`) at `0o700` / `0o600`. Both flags
work on every lane. The coaching report renders **aggregates, tool names, and content hashes only**,
and the cost report adds exactly the model, billing-modifier, and repository identifiers the archive
itself recorded, each clamped on the way out — never transcript content, in either. The four
documents that do render archived text — the standup's summary prose, `ask`'s matched snippets and
`--verbatim` excerpts, `doctor`'s repeated instructions, and `flows`' repeated requests and flow
steps — are the four that take `--no-redact` and `--filter-profanity`; there is no `--redact`,
because losing the scrub should be something a reader of the command line, and of the document's own
footer, can see happened. Rule thresholds are named constants in `crates/qanungo/src/rules.rs`, and
the scoring constants in `crates/qanungo/src/scoring.rs` are the same kind of knob — all explicitly
arbitrary until the footer's fold-cost and rule-firing data say otherwise. The prices in
`crates/qanungo/src/pricing.rs` are the opposite kind of number: not a knob at all, but a sourced
figure with a date, changed only by adding a row.

## How it works

```
coding session → munshi (capture, normalize, summarize) → patwari (permanent, verified archive)
                                                                  │
                                                        qanungo (this repo)
                                                        incremental mirror → derive → application commands → dashboard
```

- **munshi** captures and normalizes sessions; **patwari** is the central, permanent, content-addressed archive of every machine. qanungo consumes them; it never captures.
- qanungo keeps a **disposable** incremental mirror of the archive — a content-addressed blob cache with a snapshot index beside it — that it folds in memory at read time; a persistent event store stays deferred. Rebuild = delete and resync. **Patwari is the only stable interface**; the derived store is a private implementation detail.
- Interpretation is **read-time** (munshi [ADR 0011](https://github.com/surdy/munshi/blob/main/docs/adr/0011-interpret-transcripts-at-read-time-through-a-shared-streaming-crate.md)): qanungo folds metrics over `munshi-transcript`'s typed event stream, consumed as a git dependency pinned to a revision. Nothing derived is ever written back into an immutable snapshot — improve a rule, resync, re-derive.

## Application commands (the read-side suite)

ADR 0012 anticipated qanungo carrying "application commands such as a prompt-corpus exporter or a session chronicle." The suite:

- **coach** — deterministic anti-pattern detection + five practice scores (Prompt Quality, Session Hygiene, Code Review, Tool Mastery, Context Management), with WoW/MoM trends. Rules are hardcoded in Rust — the `scan → match → aggregate → check` shape is learned from Microsoft's AI-Engineering-Coach, but a rule DSL was considered and declined (hardcoded rules until a second rule-author exists). Every finding carries the Patwari `source_hash` as evidence, not a snippet.
- **chronicle / standup** — a time-boxed narrative of what you shipped, aggregated from munshi's per-session summaries across machines and repos. (GitHub Copilot CLI's `/chronicle standup`, generalized cross-harness and grounded in curated summaries.)
- **ask** — plain-language questions over your history ("have I touched the payments API?"): a deterministic ranked search over the session summaries qanungo has mirrored into its own cache (Patwari-only, no third service), each hit citing a `source_hash`. `--verbatim` escalates into the transcripts of those same hits — the funnel is bounded to what the summary search already surfaced, never an archive-wide grep.
- **instructions-doctor** — mines sessions for instructions you have had to give more than once in one repository, quotes each repetition and cites the transcript moments it happened at, and leaves the `CLAUDE.md` / `AGENTS.md` edit to a harness skill. It reports repetition; it never claims a missing instruction *caused* anything, because nothing it reads could support that.
- **cost** — token/cost breakdown by model and repo (by-machine deferred — the cost fold has no by-device slice yet), and premium-waste flags.
- **skill & agent finder** — detects repeated requests and the multi-step flows they fall into, pooled across the whole archive rather than per repository, and leaves drafting the reusable skill or custom subagent to a harness skill. It reports requests, never outcomes: an ordering in a transcript is not a cause, and whether a repetition is worth tooling is yours to decide.

## The dashboard

A plain web app on the tailnet (laptop, phone, TV; no editor, no extension), mirroring
munshi-dashboard's read-only, contract-consuming posture. The page reads, in order: the **scope
control**, the **ask box**, the **five lanes**, the **timeline**, the **habits heatmap**, the
**findings**, the **cost** breakdown, and the **standup** narrative, over a provenance footer.
Verbatim evidence is **redacted on the way to the browser** (toggleable, default on) and served by
qanungo itself.

> The original plan had every finding, chart point, and session row deep-link into `session-recall` → the verified transcript in Patwari. The 2026-08-24 grilling on [#5](https://github.com/surdy/qanungo/issues/5) **retracted that**: Patwari serves raw blobs and never redacts, so such a link does not hand a browser a redacted transcript — it hands any tailnet device the whole unredacted one. Raw Patwari URLs therefore never appear in dashboard HTML; anything verbatim is cut from the local cache, redacted, and served by qanungo. The recall funnel stays a CLI/harness affordance, where your own shell already has raw access.

## Documentation

| Document | What it covers |
| --- | --- |
| [ADR 0001 — Recompute all history with the current rule pack](docs/adr/0001-recompute-all-history-with-the-current-rule-pack.md) | Why no score is ever frozen per session, and what the rule-pack digest in every footer is for |
| [Pricing sources (2026-08-23)](docs/pricing-sources-2026-08-23.md) | The sourced provenance of every row in the cost lane's date-versioned price table, and every figure it refuses to invent |
| [Redaction patterns (2026-08-24)](docs/redaction-patterns-2026-08-24.md) | The original pattern research: every prefix, length class, and charset the scrub anchors on, and every deliberate gap |
| [Redaction patterns — amendment 2026-08-31](docs/redaction-patterns-2026-08-31.md) | `prose-credential` and `paired-username` ([#15](https://github.com/surdy/qanungo/issues/15)) — patterns whose evidence is adjacency, not a separator |
| [Redaction patterns — amendment 2026-09-04](docs/redaction-patterns-2026-09-04.md) | `prose-paired-username` ([#17](https://github.com/surdy/qanungo/issues/17)); this is the **current** `PATTERN_REVISION` |
| [Transcript fixtures](crates/qanungo/tests/fixtures/README.md) | Where the test corpus came from and what each directory exercises — `munshi/` verbatim from the suite's own fixtures, then `rules/`, `cost/`, `standup/`, `doctor/` |
| [`contrib/skills/`](contrib/skills/) | The six coding-agent skills that wrap these commands (see [Skills](#skills)) |

## Skills

Thin read-only clients that call qanungo's commands, or the recall funnel, ship in
[`contrib/skills/`](contrib/skills/) (as munshi ships `session-recall`). Drop one into your agent's
skills directory and it becomes the friendliest way in: you ask in plain language instead of
remembering a command. One interpretation half per shipped lane — qanungo stays deterministic and
LLM-free, and the reading-with-you happens in the harness.

**All six require an archive URL.** Each calls qanungo with no URL flag of its own, so
`PATWARI_URL` must be exported in the shell (see [Install](#install)); a command that exits 2 with
the missing-archive message means that install is unfinished.

| Skill | Triggers on | Wraps | Writes |
| --- | --- | --- | --- |
| [`ask`](contrib/skills/ask/) | "when did I…", "have I ever…", "which session did X", "did we already decide…", "find that session where…" | `qanungo ask` | **Nothing** — read-only |
| [`coach`](contrib/skills/coach/) | "how am I doing", "coach me", "review my AI-coding habits", "why is my X score low", "what should I improve" | `qanungo report` | **Nothing** — read-only |
| [`standup`](contrib/skills/standup/) | "what did I ship", "standup", "what did I work on this week/month" | `qanungo standup` | **Nothing** — read-only |
| [`cost-review`](contrib/skills/cost-review/) | "what does my AI coding cost", "where did the tokens go", "is caching paying off", "how is spend trending" | `qanungo cost` | **Nothing** — read-only |
| [`instructions-editor`](contrib/skills/instructions-editor/) | "what am I repeating myself about", "what should be in my CLAUDE.md", "why do I keep re-explaining this", or a handed-over doctor cluster | `qanungo doctor` | **Your own `CLAUDE.md` / `AGENTS.md`**, as a reviewed diff under normal permission prompts — never the archive |
| [`skill-finder`](contrib/skills/skill-finder/) | "what do I keep doing", "what should be a skill", "what am I retyping every week", or a handed-over flows cluster | `qanungo flows` | **New skill / subagent files**, under normal permission prompts — never the archive |

None of them writes derived data back into the archive: they render or propose; you decide.

## Design rules (inherited, non-negotiable)

1. **Read-time, re-derivable.** No derived data frozen at capture (ADR 0011).
2. **Patwari never interprets.** No metrics/scores stored in the archive (ADR 0012).
3. **Deterministic scoring, optional narration.** Scores are facts you can recompute; the LLM only narrates. Curated prose output goes to Notesmith.
4. **Evidence is a hash.** Findings carry the Patwari `source_hash`, not a truncated snippet.
5. **Grow the typed surface per consumer.** New `munshi-transcript` signals land only when a qanungo command consumes them.

## Status

*Last reviewed 2026-09-04.*

**Shipped — the P0 vertical slice** ([#7](https://github.com/surdy/qanungo/issues/7)): a `qanungo`
CLI that syncs a minimal content-addressed mirror of the recent archive, folds five metrics over
`munshi-transcript`'s typed events, evaluates eight hardcoded rules, and emits a Markdown coaching
report — findings as Problem / Action / `source_hash` evidence, with an instrumentation footer on
every run.

**Shipped — scores and trends** ([#4](https://github.com/surdy/qanungo/issues/4)) sit on top of it:
the five practice lanes, scored 0–100 **per harness** from the window's own readings, with
window-over-window arrows against the equal-length window immediately before it. All five lanes are
now fed by signals the fold types — Prompt Quality, Session Hygiene, and Tool Mastery from the
start, and, since munshi#77 typed the compaction markers and the review signals, Context Management
(off a session compacting its window over and over) and Code Review (off shipping without a review
step). A lane a harness gives no signal for still renders as *not scored* rather than taking a
default — no signal, no claim. Every run recomputes all of it with the current rule pack
([ADR 0001](docs/adr/0001-recompute-all-history-with-the-current-rule-pack.md)) and stamps the pack's
digest into the footer, so two reports compare only when that stamp matches. Every timestamp in the
report is UTC: munshi now types the capture machine's local offset (munshi#77), but the report
presents UTC rather than re-basing its cadence — that offset is consumed only by the dashboard's
heatmap.

**Shipped — the cost lane** ([#12](https://github.com/surdy/qanungo/issues/12)) is the second lane
over the same slice: `qanungo cost` folds the per-message model and token usage `munshi-transcript`
types (munshi#77) into a token/cost breakdown by model and by repository. Three things make it
honest. It **deduplicates by `message_id` before summing** — one API message reaches a transcript as
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
because a transcript cannot say which of Copilot's two billing regimes the account was on. A
**premium-waste flag** reads the top tier straight off that same price table and marks the small
sessions that paid it. **Deferred:** the by-machine cut the issue asks for, rather than faked — the
cost lane folds by repository, and a by-device slice waits on a `by_device` fold it does not yet
have; the session projection does now carry a hostname (the dashboard's device scope uses it), but
the cost fold does not group on it.

**Shipped — the redaction layer** ([#8](https://github.com/surdy/qanungo/issues/8)) shipped ahead of
the first surface that needed it: a scrub with two independently switched passes — secrets **on** by
default, profanity off — anchored on structure (a vendor prefix, a length class, a charset, a key
name) rather than on entropy, with every pattern's provenance dated and committed beside it
([`docs/redaction-patterns-2026-08-24.md`](docs/redaction-patterns-2026-08-24.md), amended by
[`docs/redaction-patterns-2026-08-31.md`](docs/redaction-patterns-2026-08-31.md) for the two
patterns [#15](https://github.com/surdy/qanungo/issues/15) added, whose evidence is a credential noun
standing in prose and a `username=` beside a live `password=` rather than a separator, and by
[`docs/redaction-patterns-2026-09-04.md`](docs/redaction-patterns-2026-09-04.md) for the one
[#17](https://github.com/surdy/qanungo/issues/17) added, which carries that same adjacency evidence
over a **sentence** instead of over a URL, so that `username : … and password …` loses both halves).
Its report carries counts per pattern id and *nothing about what it matched* — no offset, no
excerpt, not even in `Debug` — because the thing that ends up in a footer or a panic message must
not be the leak. `report` and `cost` are deliberately not wired to it: a filter over a document that
carries no transcript content can only be decoration, and decoration in a security control invites
the reader to trust it.

**Shipped — the standup lane** ([#9](https://github.com/surdy/qanungo/issues/9)) is the third lane
and the first that renders prose. `qanungo standup` reads the `summary.md` every snapshot already
carries — munshi's own curated record of the session, written when it was captured (munshi ADR
0009/0010) — rather than the transcript beside it, and emits sessions grouped by repository (busiest
first, sessions newest first), then the window's decisions and open items rolled up across every
repository with exact repeats dropped. **No model reconstructs anything**: qanungo selects, orders,
groups, and deduplicates the archive's own words, which is what makes it cheaper and
better-grounded than an LLM chronicle over raw sessions. It is #8's first consumer, and the wiring
is structural — the scrub happens in the fold, so the renderer holds no unscrubbed copy of a field
to leak by accident, and the footer states which passes ran, what they fired as counts per pattern
id, and the pattern revision they fired from. A session with no summary anywhere, one this build
cannot parse, and munshi's own placeholder each land in **Gaps** with the reason, never in the
narrative and never silently dropped.

**Shipped — the ask lane** ([#10](https://github.com/surdy/qanungo/issues/10)) is the
ask-your-history half, and it is deliberately a *search*, not a model. `qanungo ask "<query>"` ranks
the same `summary.md` records the standup lane reads, by a fixed, total rubric a reader can predict,
and renders each hit as a scrubbed snippet cited by the content hash of the summary it came from —
Patwari-only, no third service, no index to keep warm. It is the one lane with **no default
window**: "have I ever done this" is a question about all of history, so `--last` is optional and
its absence means the whole archive; `--limit` (default 10, and zero is refused rather than
clamped) widens or narrows the ranking. Two rules keep it honest. **Scoring reads the unscrubbed
text and only the displayed snippet is scrubbed**, so a secret-shaped token can never change a
ranking by being replaced first; and a session the cache could not read, parse, or that holds only
munshi's placeholder is counted as *not searchable* rather than quietly dropped — a silent-drop
count bug found in review is why that reconciliation is now pinned. `--verbatim` is the funnel's
next stage rather than a second search: for the hits `ask` is already going to show, and only
those, it opens those transcripts and searches `munshi-transcript`'s **typed** records — user and
assistant text, and a tool event's command, error, and output — never the raw JSONL bytes and never
the tool payload blobs, because a term inside one says a session *read* a file, not that it was
about it. That bound is the design: an unbounded escalation would be an archive-wide grep, which is
exactly the cost this lane refused. The same corpus is served in the dashboard's **ask box**, which
asks this server's own `/api/ask` route against the corpus the refresh already parsed, so a browser
can never make the process talk to the archive. The `/ask` contrib skill is the synthesis half.

**Shipped — the dashboard** ([#5](https://github.com/surdy/qanungo/issues/5)) is the fourth lane and
the first that is not a document. `qanungo dashboard` serves the other lanes' own numbers — five
score cards, the findings under them, the quarter's bill, the week's narrative, a provenance footer
— as one embedded single-file page on a hand-rolled `std::net` server, theme-aware and legible from
a phone to a TV. It is a *presentation*, not a second computation: it calls the same folds `report`,
`cost`, and `standup` call, and serializes the results instead of rendering them as Markdown, so the
page and the terminal cannot come to disagree about a score or a dollar. Each section keeps its own
lane's window — `--last 30d`, `--cost-last 12w`, `--standup-last 7d` — because a score wants a
month, a bill wants a quarter, and a standup wants a week you can read to the end of. One refresh
folds all of it and publishes **one** payload, so a reader can never see a bill from one refresh
beside a standup from another. A long-lived process re-folds every `--refresh`, swaps the served
payload atomically, and pushes an SSE event so open pages re-fetch; a request is then a memcpy.
**Deferred**, per the issue's 2026-08-24 grilling: the persistent event store — a request is already
a memcpy against the last-published payload, so the interactive guideline holds without one, and
process memory is the disposable materialization. (What used to dominate the refresh was sync, not
the fold; a snapshot index — not a store — cut it, and the refresh is now fold-bound.)

**What the served surface renders is three different claims, and it says which is which.** The
coaching and cost sections carry aggregates and hashes: scores, rule ids, counts, dollars, token
counts, the model and repository identifiers the archive itself recorded — each clamped on the way
out — and `sha256` content hashes. No transcript text reaches either, by construction, exactly as
their Markdown documents hold that line. The standup section is deliberately different: it renders
**verbatim-class archived prose**, the same `summary.md` text `qanungo standup` prints, and it
renders it **scrubbed** — the fold runs the [#8](https://github.com/surdy/qanungo/issues/8) redactor
on the way into its own types, so no unscrubbed string ever reaches the payload, and the served
strings are pinned equal to the fold's. The ask box's answers are the same class, scrubbed the same
way and by the same redactor. A finding will also expand into the individual events its rule
counted, one scrubbed event each, from this server's own route. Redaction is **on by default**, and
`--no-redact` is **launch-time only**: it belongs to the process, never to a request, because a
scrub a browser could flip with a query string is a bypass with a nicer name — and turning it off on
a non-loopback bind prints its own very loud line. There is deliberately **no link into Patwari**,
which serves unredacted blobs; a hash is what you take to your own shell. Loopback by default;
`--bind` on a tailnet address is how a phone reads it, and startup prints one line saying that
nothing authenticates a caller and the tailnet is therefore the only boundary.

The dashboard also carries a **scope control**: one select for the repository the archive listed a
session under, one for the device (its capture hostname, a second primary axis mutually exclusive
with the repository), and one for the harness, narrowing the scores, the findings, and — for a
repository — the bill and the narrative together (a device narrows the scores, findings, and
timeline, while the bill, the narrative, and the heatmap stay whole-window — none of those is cut by
hostname). Every scope is folded server-side and shipped with the payload
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

Beside it now sits a **heatmap** — a "Habits" grid of hour-of-day against day-of-week — which waited
on exactly what the timeline did not need: each session's own local offset, since "worked at 1 a.m."
and "worked on a Sunday" are the claims UTC misplaces and the only ones that view exists to make.
munshi#77 typed that offset per capture, so each session is placed at its transcript's first-activity
hour shifted into its own local time; a session with no recorded offset is counted but left unplaced
in the footer rather than mislocated.

**Shipped (V1) — the instructions doctor** ([#11](https://github.com/surdy/qanungo/issues/11)):
`qanungo doctor` reads the text a *person* typed across the archive and reports the instructions
given more than once in the sessions of one repository — each cluster quoted once, scrubbed, with a
citation per occurrence. Four things make it something other than a grep. It reports **repetition,
never causation**: it reads transcripts and never a checkout, so it cannot know what a `CLAUDE.md`
says, and the document refuses that sentence in its own preamble — deciding what belongs in an
instruction file is `contrib/skills/instructions-editor`'s half, in the repo, under permission
prompts. It compares **only what a person typed**, because `Event::User` is a surface the harness
also writes to (pasted-image placeholders, slash commands, whole skill bodies, task notifications),
and all of that is byte-identical between sessions and would otherwise be most of the finding. It
counts **conversations, not session ids** — a resumed session replays the one before it, and merging
those is what keeps one long conversation from reporting as a page of repetitions. And it is the
CLI's second verbatim surface after `ask --verbatim`: the clustering reads the transcript's own
bytes, so a credential cannot change what clusters, and the one excerpt each cluster renders is
scrubbed before it is cut. **Deferred:** the V2 measured-outcomes half, until edits and post-deploy
captures accrue.

**Shipped — the skill & agent finder** ([#13](https://github.com/surdy/qanungo/issues/13)) sits
beside it: `qanungo flows` runs that same detection machinery — the two share one module rather than
forking it — through the opposite lens. The doctor groups per repository because an instruction file
belongs to one; a workflow worth a skill is worth it **wherever** it recurs, so this lane pools every
session in the reach into one comparison, takes in the sessions the archive attributes to no
repository at all, and lists each finding by the repositories it turned up in. Over the clusters that
come out it mines the recurring two- and three-step runs of each session's clustered messages, in
order: adjacency is among *clustered* messages, so ordinary conversation between two steps does not
break them apart, a request restated back-to-back collapses to one step, and a flow has to recur in
two distinct conversations. It reads **requests and never outcomes** — nothing in it knows whether a
flow worked — and it makes the doctor's one known noise class *worse* rather than better, which the
document says out loud in its own preamble: harness-injected prose the authored filter cannot certify
clusters with itself in every repository at once, which can genuinely make it the most repeated text
in the corpus and still worth nothing. Triaging that, and drafting anything, is
`contrib/skills/skill-finder`'s half.

**Declined.** The rule DSL ([#3](https://github.com/surdy/qanungo/issues/3)) — not deferred:
hardcoded Rust rules stand until a second rule-author exists, and the design in that issue is kept
for the record only.

**Deferred, pulled by friction rather than scheduled.** Mirror hardening
([#1](https://github.com/surdy/qanungo/issues/1)) and later metrics
([#2](https://github.com/surdy/qanungo/issues/2)), each waiting on a real need. The narrator
([#6](https://github.com/surdy/qanungo/issues/6)) is likely mooted by the shipped `/coach` skill.
Design notes and the decision log are kept outside this repository. See issues for the phased plan.

## The name

A *qanungo* (Hindi क़ानूनगो, Urdu قانونگو, from Persian *qānūn-go*, "one who speaks the law") was the
revenue officer above the patwaris — the one who read the record-keepers' ledgers, checked them
against each other, and answered for what they said. He never wrote the entries and never held the
archive; he audited it. That is exactly what this tool is.

The companion projects are named in the same spirit: [munshi](https://github.com/surdy/munshi)
(मुंशी, منشی), the clerk who kept the written record, and
[patwari](https://github.com/surdy/patwari) (पटवारी, پٹواری), the village record-keeper who
maintained the permanent land ledger — three offices of one record room, which is what
[daftar](https://github.com/surdy/daftar) (दफ़्तर, دفتر) is named for.
**Munshi writes the record; Patwari keeps the archive; Qanungo audits it.**

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
