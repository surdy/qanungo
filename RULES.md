# What qanungo looks for

Every rule this build fires, every threshold it fires at, the lanes those rules score into, the prices it bills at, and the patterns it scrubs.

**This file is generated.** It is rendered from the constants and tables the runtime itself reads, so it cannot describe a rule the code does not run. Do not edit it by hand — run `qanungo rules > RULES.md` and commit the result; a test fails if the two disagree.

| Stamp | Value | What it pins |
| --- | --- | --- |
| Rule pack | `4137a6c61b13` | Every rule id, threshold, scoring constant, and lane mapping below. Two reports are comparable **iff this matches**. |
| Formula | `equal-weight-mean-of-linear-penalties/1` | How the readings combine into a score. Bumped when the arithmetic changes, so a re-weighting cannot hide behind unchanged numbers. |
| Redaction patterns | `2026-09-04` | The pattern set every rendered excerpt was scrubbed with. |
| Price table | `2026-09-05` | The dated rates the cost lane bills at. |

The full rule-pack digest is `4137a6c61b13e31aa51c9ef181670d23902de391d63220ee5667f76cb50b1f5f`; the short form above is its first 12 characters and is what the footer of every report prints.

## Rules

8 rules, evaluated in this order, which is also report order and the order the rule-pack digest hashes them in. They are **not** mutually exclusive: one session can trip several, and should then appear under each, because the findings ask for different things.

Every threshold is **arbitrary until measured** — a first guess at where a pattern stops being ordinary work and starts being a habit worth naming. Where a number has had a measurement run over it, the measurement is stated beside it; where the *Measured against* column is empty, the number is still a guess and should be read as one. A rule that fires constantly is evidence its threshold is wrong, not evidence the habit is everywhere.

### 1. High tool error rate — `high-tool-error-rate`

**Fires when.** A session's overall tool failure rate is over 20%, or any single tool's rate within it is over 30%. Either rate is read only once enough calls reported an outcome; a session under both minimums is not looked at rather than passed.

| Constant | Value | Measured against |
| --- | --- | --- |
| `SESSION_TOOL_ERROR_RATE` | 20% | — |
| `MIN_SESSION_TOOL_ATTEMPTS` | 10 | — |
| `TOOL_ERROR_RATE` | 30% | — |
| `MIN_TOOL_ATTEMPTS` | 5 | — |

- **Eligible sessions** — sessions that reported enough tool outcomes. A session outside that denominator is not looked at, which is not the same as looking and finding nothing.
- **Evidence** — event (it counted concrete events, so it can point at them — bounded locators into the transcript, whose excerpts are scrubbed on the way out). No lane reads this rule's fire rate.
- **Problem** — the report prefixes this with *n* of *m* folded sessions: “ran tool failure rates over threshold (20% session-wide, 30% for a single tool).”
- **Action** — “Failing calls are re-work: the agent spends a turn discovering what the environment already knew. Pin the failing tool's preconditions where the agent reads them — a CLAUDE.md note, a skill, or a wrapper that fails loudly — rather than correcting the same call again next session.”

### 2. Retry loop — `retry-loop`

**Fires when.** One *exact* command value ran 6+ times inside a single session. The busiest command's run count is the only trigger; the repeat share rides along as evidence and decides nothing. A session whose harness records no command field is skipped, not scored.

| Constant | Value | Measured against |
| --- | --- | --- |
| `RETRY_LOOP_REPEATS` | 6 | archive p95 of the busiest command's run count, measured 2026-08-18 against a proxy and confirmed the same day on the promoted `command` field: 20 of 408 measurable sessions (4.9%) |

- **Eligible sessions** — sessions that recorded a command. A session outside that denominator is not looked at, which is not the same as looking and finding nothing.
- **Evidence** — event (it counted concrete events, so it can point at them — bounded locators into the transcript, whose excerpts are scrubbed on the way out). Scored into **Tool Mastery**.
- **Problem** — the report prefixes this with *n* of *m* folded sessions: “ran one identical command 6+ times.”
- **Action** — “Repetition is the cheapest signal that a loop is not closing: the same command, the same disagreement, another turn. Fix what the command keeps arguing with — the stale config, the missing dependency, the wrong working directory — and say so where the agent reads it, rather than letting it rediscover the answer by running the command again. Where the repeat is legitimate re-checking after each edit, it is a watch mode or a single script waiting to be written.”

### 3. Marathon session — `marathon-session`

**Fires when.** The session's longest continuous *sitting* — consecutive records separated by no gap longer than 15m — ran over 2h 00m. Never wall-clock span, and never total active time.

| Constant | Value | Measured against |
| --- | --- | --- |
| `MARATHON_SITTING_ACTIVE` | 2h 00m | the archive's p95 longest-sitting at this idle gap, measured 2026-08-18 over 564 transcripts: 25 sessions (4.4%) |
| `IDLE_GAP` | 15m | half of a two-part setting, measured 2026-08-18 over 564 transcripts and 606k records: the pooled gap distribution has no valley, so this is chosen behaviourally — above the harness's 180 s timeout artifact and below any break a person would call still being in the session |

- **Eligible sessions** — sessions with a measurable sitting. A session outside that denominator is not looked at, which is not the same as looking and finding nothing.
- **Evidence** — structural (it measured a shape rather than an utterance, so its evidence is timestamps and counts and it mints no excerpt). Scored into **Session Hygiene**.
- **Problem** — the report prefixes this with *n* of *m* folded sessions: “worked for more than 2h 00m without a break longer than 15m.”
- **Action** — “Split the work at the next natural boundary and start the follow-on in a fresh session. Write the handoff down first — what is done, what is next, which files matter — so the new context starts from a summary rather than from hours of accumulated conversation.”

### 4. Heavily resumed session — `resumed-session`

**Fires when.** The session's calendar span is at least 10.0x its active time *and* it was picked up in 5+ separate sittings. Both halves, so neither one long break nor a handful of brisk sittings fires on its own.

| Constant | Value | Measured against |
| --- | --- | --- |
| `RESUMED_SPAN_TO_ACTIVE` | 10.0 | roughly twice the archive's median dilution (4.9x), measured 2026-08-18; 59.6% of sessions are multi-sitting, so being resumed is the normal shape |
| `RESUMED_MIN_SITTINGS` | 5 | — |
| `IDLE_GAP` | 15m | half of a two-part setting, measured 2026-08-18 over 564 transcripts and 606k records: the pooled gap distribution has no valley, so this is chosen behaviourally — above the harness's 180 s timeout artifact and below any break a person would call still being in the session |

- **Eligible sessions** — sessions with a measurable span and active time. A session outside that denominator is not looked at, which is not the same as looking and finding nothing.
- **Evidence** — structural (it measured a shape rather than an utterance, so its evidence is timestamps and counts and it mints no excerpt). Scored into **Session Hygiene**.
- **Problem** — the report prefixes this with *n* of *m* folded sessions: “were worked in 5+ separate sittings, with a calendar footprint at least 10.0x their active time.”
- **Action** — “Start a fresh session per work item rather than returning to a standing one. The archive keeps the old transcript, so nothing is lost by leaving it closed — and a session that maps onto one piece of work is the unit every summary, metric, and coaching finding here is actually about.”

### 5. Babysitting pattern — `babysitting`

**Fires when.** The session carried 15+ user requests at under 2.0 tool activities per request.

| Constant | Value | Measured against |
| --- | --- | --- |
| `BABYSITTING_TOOLS_PER_REQUEST` | 2.0 | — |
| `BABYSITTING_MIN_USER_REQUESTS` | 15 | — |

- **Eligible sessions** — sessions carrying 15+ user requests. A session outside that denominator is not looked at, which is not the same as looking and finding nothing.
- **Evidence** — structural (it measured a shape rather than an utterance, so its evidence is timestamps and counts and it mints no excerpt). Scored into **Prompt Quality**.
- **Problem** — the report prefixes this with *n* of *m* folded sessions: “carried 15+ user requests at under 2.0 tool activities each — turn-by-turn steering rather than delegation.”
- **Action** — “Ask bigger. State the outcome and the constraints once and let the agent plan the steps. Where the same sequence of small asks keeps recurring, it is a skill waiting to be written — capture it once instead of retyping it every session.”

### 6. Fire-and-forget extreme — `fire-and-forget`

**Fires when.** The session carried exactly 1 user request, ran 40.0+ tool activities per request, *and* at least one call reported failure. All three, so an enormous clean run is not a finding.

| Constant | Value | Measured against |
| --- | --- | --- |
| `FIRE_AND_FORGET_TOOLS_PER_REQUEST` | 40.0 | — |
| `FIRE_AND_FORGET_USER_REQUESTS` | 1 | — |

- **Eligible sessions** — single-request sessions. A session outside that denominator is not looked at, which is not the same as looking and finding nothing.
- **Evidence** — mixed (one half counted events it can point at and the other is a shape or an absence, which can only be stated). Scored into **Prompt Quality**.
- **Problem** — the report prefixes this with *n* of *m* folded sessions: “ran 40.0+ tool activities on a single request and hit errors on the way.”
- **Action** — “Put a checkpoint in the middle. Ask for a plan before the work, or for a report at a named milestone, so a wrong turn surfaces while it is still one wrong turn rather than at the end of an hour of unattended tool calls.”

### 7. Compaction churn — `compaction-churn`

**Fires when.** The session completed 4+ context-window compactions. Compacting once is deliberately not a finding at any threshold.

| Constant | Value | Measured against |
| --- | --- | --- |
| `COMPACTION_CHURN_COMPLETIONS` | 4 | the p75 of the compacting distribution, measured 2026-08-24 over 734 transcripts: 27 of 734 eligible sessions (3.7%) |

- **Eligible sessions** — sessions whose harness records compactions. A session outside that denominator is not looked at, which is not the same as looking and finding nothing.
- **Evidence** — structural (it measured a shape rather than an utterance, so its evidence is timestamps and counts and it mints no excerpt). Scored into **Context Management**.
- **Problem** — the report prefixes this with *n* of *m* folded sessions: “compacted their context window 4+ times.”
- **Action** — “Repeated compaction is the window telling you the transcript has outgrown the work. Each round discards context the next round has to rediscover, and what survives is a summary of a summary. Land the current piece, write the handoff down — what is done, what is next, which files matter — and start the follow-on in a fresh session, so the new context begins from that handoff rather than from whatever the last compaction happened to keep.”

### 8. Shipped without review — `unreviewed-ship`

**Fires when.** The session committed code and nothing in it ran a review pass. **There is no threshold to tune**: the trigger is a count of none, so there is no constant to write down and none to defend.

*No threshold constants — this rule's trigger is a count of none.*

- **Eligible sessions** — sessions that shipped on a harness whose review surfaces are all typed. A session outside that denominator is not looked at, which is not the same as looking and finding nothing.
- **Evidence** — mixed (one half counted events it can point at and the other is a shape or an absence, which can only be stated). Scored into **Code Review**.
- **Problem** — the report prefixes this with *n* of *m* folded sessions: “committed code without running a review pass first.”
- **Action** — “A review pass is the cheapest place to catch what the writing missed, and it is cheapest before the commit rather than after it: once the work is committed the next reader is a diff nobody asked for. Run the review skill against the diff before committing — the same session, while the context that wrote the code is still loaded — rather than trusting the pass that wrote it to also be the pass that checks it.”

## Lanes

5 practice lanes. Each is a small set of **components**, each of which reads one rate off the window. A lane no signal feeds is never scored — it says which signal it is waiting for rather than defaulting to a zero or a hundred — and a lane whose components all came back empty for one harness reads *no reading*, which is also not a zero.

| Lane | Key | Reads | Of |
| --- | --- | --- | --- |
| Prompt Quality | `prompt-quality` | Babysitting pattern (`fire-rate:babysitting`) | sessions carrying 15+ user requests |
|  |  | Fire-and-forget extreme (`fire-rate:fire-and-forget`) | single-request sessions |
| Session Hygiene | `session-hygiene` | Marathon session (`fire-rate:marathon-session`) | sessions with a measurable sitting |
|  |  | Heavily resumed session (`fire-rate:resumed-session`) | sessions with a measurable span and active time |
| Code Review | `code-review` | Shipped without review (`fire-rate:unreviewed-ship`) | sessions that shipped on a harness whose review surfaces are all typed |
| Tool Mastery | `tool-mastery` | Tool error rate (`pooled-tool-error-rate`) | calls that reported an outcome |
|  |  | Retry loop (`fire-rate:retry-loop`) | sessions that recorded a command |
| Context Management | `context-management` | Compaction churn (`fire-rate:compaction-churn`) | sessions whose harness records compactions |

### The formula

```text
penalty_i = clamp(reading_i / floor_i, 0, 1)
score     = round(100 × (1 − mean(penalty_i)))    // mean over the components that read
```

In words: each component divides its reading by its floor, clamped into 0…1; the lane scores **100 × (1 − mean(clamp(reading/floor, 0, 1)))** over the components that read. Every component in a lane weighs the same — nothing measured says one deserves more — and a component whose signal is absent has no say rather than a zero penalty.

| Constant | Value | What it does |
| --- | --- | --- |
| `FIRE_RATE_FLOOR` | 25% | The fire rate at which a fire-rate component spends its whole share. One floor for every rule, deliberately — and it **saturates**: every rate from here to 100% costs the same, so read the component's raw reading, not the lane number, where a rule fires above it. |
| `TOOL_ERROR_RATE_FLOOR` | 20% | The pooled tool failure rate at which that component spends its whole share. Anchored on `SESSION_TOOL_ERROR_RATE` rather than chosen freely. |
| `MIN_SCORED_SESSIONS` | 5 | Eligible sessions a fire-rate component needs before its rate is a reading at all. Under it the component reports no reading rather than a jumpy one. |
| `MIN_SCORED_TOOL_ATTEMPTS` | 10 | Calls that reported an outcome before the pooled error rate is a reading. |
| `CLEAN_SCORE` | 100 | The score of a window in which nothing this pack penalizes was observed. It means *nothing penalized was seen*, never *the practice is perfect*. |

Scores are computed **per `source_agent`**, because harnesses differ in what they can express and a blended number would move when the harness mix moved. The one fleet number is the **unweighted mean of the per-harness scores**, every harness counting once — stable under a mix shift, unstable under a roster shift, which is why a fleet trend arrow is drawn only when the same harnesses scored the lane on both sides. Scores are comparable across windows under the same rule pack, and **never across lanes**.

## Folds

What the fold derives from `munshi-transcript`'s typed events, before any rule looks at it. 6 of them; every rule and every lane above reads one of these and nothing else.

| Fold | Reads |
| --- | --- |
| Tool error rate | the outcome fields a tool event carries (`success`, `is_error`), per session and per tool name. Only events with an *explicit* outcome enter the denominator, so a harness that cannot express failure reports no rate rather than a flattering zero. |
| Tool-per-request ratio | tool activities over user requests — how much the agent does per thing it is asked for. |
| Cadence and duration | sessions per day, and each session's active time: the gaps between consecutive records, with anything longer than `IDLE_GAP` counted as the operator having walked away. Wall-clock span is derived as context, never as a trigger. |
| Repeated-command churn | the typed `command` field on a tool event: how much of a session's command-bearing activity was the same command run again. A session whose harness records no command field has no churn reading, not a zero one. |
| Context compaction | the compaction markers a record may carry — completed, not known to have failed. For the two harnesses that write markers, a transcript with none is a reading of *none*; for one that writes none, it is no reading at all. |
| Review activity | commits (`git commit` invocations) and skill invocations classified as a review pass, plus whether the harness types **every** surface a review could arrive on. A harness that does not is left out of the rate entirely. |

## Cost

Dollars are Anthropic API **list** prices, per million tokens, from the row effective at each session's archive time. A model with no row is unpriced rather than free, and a harness whose billing is not recoverable from a transcript gets token volumes and no money at all.

Price table revision `2026-09-05`. *Top tier* below is resolved as of 2026-09-01, the newest date in the table itself, so this document says the same thing whenever it is rendered.

| Model | From | Input | Output | Cache write 5m | Cache write 1h | Cache read | Fast tier | US premium | Top tier |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | ---: | --- |
| `claude-haiku-4-5-20251001` | 2025-10-15 | $1.00 | $5.00 | $1.25 | $2.00 | $0.10 | — | — | no |
| `claude-opus-4-8` | 2026-05-28 | $5.00 | $25.00 | $6.25 | $10.00 | $0.50 | $10.00 in / $50.00 out | ×1.1 | no |
| `claude-fable-5` | 2026-06-09 | $10.00 | $50.00 | $12.50 | $20.00 | $1.00 | — | ×1.1 | yes |
| `claude-sonnet-5` | 2026-06-30 | $2.00 | $10.00 | $2.50 | $4.00 | $0.20 | — | ×1.1 | no |
| `claude-opus-5` | 2026-07-24 | $5.00 | $25.00 | $6.25 | $10.00 | $0.50 | $10.00 in / $50.00 out | ×1.1 | no |
| `claude-fable-5-1` | 2026-09-01 | $10.00 | $50.00 | $12.50 | $20.00 | $0.25 | — | ×1.1 | yes |

A fast-mode session bills at the fast column where the model has one; where it does not, `speed = "fast"` is **unpriced** rather than billed at the base rate. A `US premium` of ×1.1 applies to US-only inference on the rows that document one; a row with none that meets US inference is unpriced, because pricing it either way would be a claim the table's sources do not support.

### Premium waste

One flag, not a judgement: sessions billed **wholly at the day's top tier** that were very small. Whether small was the wrong place for the dearest model is the reader's call.

| Constant | Value | What it does |
| --- | --- | --- |
| `PREMIUM_FLAG_MAX_OUTPUT_TOKENS` | 3000 tokens | The most output tokens a top-tier session may have produced and still be listed. Set in the gap below a floor cluster in the real distribution (2026-09-04: 4 of 61 wholly-top-tier sessions, 6.6%). |
| `PREMIUM_FLAG_MAX_MESSAGES` | 8 | The most billed messages such a session may have carried. A handful of exchanges rather than a working session; it bound nothing on the archive it was measured against and is carried because the two floors describe different shapes. |
| `PREMIUM_SESSIONS_LISTED` | 20 | How many flagged sessions the report lists before it stops and says how many more there were. |

## Ask

`qanungo ask` is a deterministic ranked search over the archive's own summaries — no model, no third service. A query is lower-cased and split on anything that is not a letter or a digit; each surviving term scores once per field it appears in, weighted:

| Field | Weight | Quotable |
| --- | ---: | --- |
| title | 5 | yes |
| repository | 4 | no |
| tags | 4 | no |
| goal | 3 | yes |
| decisions | 2 | yes |
| open items | 2 | yes |
| files changed | 2 | yes |
| work completed | 1 | yes |
| commands | 1 | yes |
| branch | 1 | no |

*Quotable* decides which field a hit's snippet is drawn from: a repository name ranks well and reads as one word out of context, so the snippet prefers a prose field that matched and falls back to a keyword only when none did.

| Constant | Value | What it does |
| --- | --- | --- |
| `MIN_TERM_CHARS` | 3 | Shortest query word kept. Shorter fragments match almost everything and rank nothing. |
| `MAX_SNIPPET_CHARS` | 200 | Longest snippet rendered. A snippet is a pointer into a summary, not the summary; the bound is on already-scrubbed text, so cutting one short can only hide detail. |
| `DEFAULT_ASK_LIMIT` | 10 | Ranked matches printed when `--limit` says otherwise. `--verbatim` escalates into at most this many transcripts — the funnel is bounded to what the summary search already surfaced, never an archive-wide grep. |
| stop words | 23 | Function words dropped before scoring. Deliberately short and English-only: a long list would quietly refuse to search for real words. |

## Repetition

What `qanungo doctor` (one repository) and `qanungo flows` (the whole archive) compare messages with. Two messages are the same request when they share enough four-word phrases; everything below bounds that comparison. These report repetition and never cause: an ordering in a transcript is not a reason.

| Constant | Value | What it does |
| --- | --- | --- |
| `SHINGLE_WORDS` | 4 | Words in one shingle — the phrase length two messages are compared on. |
| `MIN_CLUSTERABLE_WORDS` | 8 | Shortest message compared at all. "yes", "continue" and "do it" are the most repeated things anybody types and mean nothing; the floor is set where a sentence starts. Shorter messages are counted, not silently passed over. |
| `SIMILARITY_THRESHOLD_PERCENT` | 60% | How much of the **shorter** message's phrases the two must share. Containment rather than Jaccard, so a short rule restated inside a long request still matches. |
| `MIN_CLUSTER_SESSIONS` | 2 | Distinct conversations a cluster must span before it is reported. Repetition inside one session is a conversation, not a finding. |
| `SAME_CONVERSATION_PERCENT` | 50% | Shared messages above which two sessions are read as **one conversation captured twice** and merged. Without it, a replayed transcript turns every message of one conversation into a repeated request. |
| `MIN_SHARED_INSTRUCTIONS` | 3 | Messages two sessions must share before the merge rule is consulted at all, so a percentage over a tiny denominator cannot delete the finding it is protecting. |
| `MAX_SHINGLE_POSTINGS` | 200 | Messages a phrase may appear in before it is skipped for candidate gathering. Bounds the work, never the truth: skipping a phrase can only lower a measured overlap. |
| `MAX_CITATIONS_PER_CLUSTER` | 8 | Occurrences one cluster cites before the list is cut short. The total travels beside them, so a cut list is never mistaken for the whole. |

## Redaction

Every surface that renders archived text scrubs it first, and stamps the pattern revision it scrubbed with. The ids below are what a marker and a footer report; **the patterns themselves are not printed here**, and that is deliberate — a catalogue of secret shapes is a different document. The research lives in `docs/redaction-patterns-*.md`, one file per revision.

Pattern revision `2026-09-04`: 18 ids, 17 of them secret patterns.

| Id | Kind | Added in |
| --- | --- | --- |
| `github-token` | secret | `2026-08-24` |
| `anthropic-key` | secret | `2026-08-24` |
| `openai-key` | secret | `2026-08-24` |
| `aws-access-key-id` | secret | `2026-08-24` |
| `aws-secret-key` | secret | `2026-08-24` |
| `slack-token` | secret | `2026-08-24` |
| `gitlab-token` | secret | `2026-08-24` |
| `npm-token` | secret | `2026-08-24` |
| `google-api-key` | secret | `2026-08-24` |
| `jwt` | secret | `2026-08-24` |
| `private-key-block` | secret | `2026-08-24` |
| `authorization-header` | secret | `2026-08-24` |
| `url-credentials` | secret | `2026-08-24` |
| `secret-assignment` | secret | `2026-08-24` |
| `prose-credential` | secret | `2026-08-31` |
| `paired-username` | secret | `2026-08-31` |
| `prose-paired-username` | secret | `2026-09-04` |
| `profanity` | profanity | `2026-08-24` |

Secrets are scrubbed **by default** and profanity is masked **only on request**. There is no `--redact`: the scrub is not something a person should have to remember to ask for, and the only way to lose it is `--no-redact`, which the document's own footer then confesses to.

---

Generated by `qanungo rules`, which reads no archive and needs no `--patwari-url`. Regenerate with `qanungo rules > RULES.md`; the test `rules_md_matches_the_rendered_catalogue` fails while the committed file and the build disagree. Every number above is rendered from the constant the runtime reads — `crates/qanungo/src/rules.rs`, `scoring.rs`, `metrics.rs`, `pricing.rs`, `cost.rs`, `ask.rs`, `repetition.rs`, `redaction.rs` — so a threshold cannot be changed without this document changing with it.
