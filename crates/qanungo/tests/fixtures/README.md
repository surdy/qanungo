# Transcript fixtures

## `munshi/`

Copied verbatim from the Munshi suite's own fixtures (`munshi/fixtures/…`), which are the
pinned-envelope examples `munshi-transcript` itself is validated against. They exercise the fold
against transcripts nobody wrote for qanungo — they are ordinary short sessions, so they
deliberately fire **no** rule.

| File | Origin |
| --- | --- |
| `claude-code-2.1.44-normal.jsonl` | `fixtures/claude-code-2.1.44/normal/0c1a0de0-…-000000000001.jsonl` |
| `copilot-1.0.70-envelope.jsonl` | `fixtures/copilot-1.0.70/transcript/synthetic-envelope.jsonl` |
| `copilot-1.0.76-tool-activity.jsonl` | `fixtures/copilot-tool-activity/aaaaaaaa-…/events.jsonl` |

## `rules/`

Synthesized here, in the Claude Code 2.1.205 envelope unless stated otherwise, one per rule —
Munshi's fixtures are short, healthy sessions and none of them crosses a coaching threshold, which
is exactly what a rule test cannot use. Each file is the smallest transcript that trips one rule
and no other.

Three of them exist to pin the gap-aware duration metric (qanungo #14), where what matters is not
how many records a transcript holds but *how far apart their timestamps are*:

| File | Shape | Fires |
| --- | --- | --- |
| `marathon-session.jsonl` | 27 records 5m apart — one unbroken 2h10m sitting | Marathon session |
| `resumed-session.jsonl` | 6 sittings of 15m, a day apart — 1h30m of work in a 120h span | Heavily resumed session |
| `idle-gap-boundary.jsonl` | 9 records *exactly* `IDLE_GAP` apart — one sitting of exactly `MARATHON_SITTING_ACTIVE` | nothing |

The last is the boundary pin for both comparisons at once: a gap of exactly the idle threshold
stays inside its sitting (`≤`), and a sitting of exactly the marathon threshold has not crossed it
(`>`). Moving either constant without revisiting that fixture will be noticed.

`retry-loop.jsonl` is the exception to the envelope note: it is a **Codex rollout**, because
Codex's `local_shell_call` is the only record in the pinned interpreter that puts a `command`
field on a tool event, and repeated-command churn is folded from that field alone. Eight
`local_shell_call` records run three distinct commands, one of them six times — enough to cross
`RETRY_LOOP_REPEATS` — with the one-offs interleaved so that grouping by value, rather than
counting events, is what makes the test pass.

`high-tool-error-rate.jsonl` and `retry-loop.jsonl` additionally carry `CANARY_*` tokens in every
free-text field (user text, assistant text, tool command input, tool error output, and in the
retry fixture the repeated command itself). Two redaction tests fold them, render a full report,
and assert that not one of those tokens survives into the Markdown — the second exists because the
churn fold is the first metric that compares transcript *content*, and the report must still be
able to say a command ran six times without saying which command it was.
