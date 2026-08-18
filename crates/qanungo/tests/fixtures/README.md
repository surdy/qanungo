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

Synthesized here, in the Claude Code 2.1.205 envelope, one per rule — Munshi's fixtures are
short, healthy sessions and none of them crosses a coaching threshold, which is exactly what a
rule test cannot use. Each file is the smallest transcript that trips one rule and no other.

`high-tool-error-rate.jsonl` additionally carries `CANARY_*` tokens in every free-text field
(user text, assistant text, tool command input, tool error output). The redaction test folds it,
renders a full report, and asserts that not one of those tokens survives into the Markdown.
