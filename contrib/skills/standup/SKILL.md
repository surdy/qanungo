---
name: standup
description: Narrate what the user shipped across their AI-coding sessions, from `qanungo standup`. Use when the user asks "what did I ship", "standup", "what did I work on this week/month" — the command aggregates every machine, repo, and harness from the archive's own session summaries.
---

# standup

The narration half of qanungo's chronicle lane (qanungo#9). `qanungo standup` computes the *facts* — a deterministic Markdown digest of the window's session summaries; this skill turns them into the *telling*: a standup update, a weekly summary, a "what happened" answer. qanungo stays deterministic and LLM-free; the polish happens here, in the harness.

## Steps

**Requires an archive URL.** `PATWARI_URL` must be set in the shell, or `--patwari-url <URL>` passed on each command; there is no default archive. If a command exits 2 with the missing-archive message, stop and tell the user to finish the install as the README's Install section describes.

1. **Run the command.** `qanungo standup --last 7d` (window as the user asks: `h`/`d`/`w` units — there is deliberately no `m`). The output is grouped by repository, sessions newest-first (title · goal · work items), then rolled-up **Decisions** and **Open items**, then **Gaps**, then the instrumentation footer.
2. **Polish, don't embellish.** Condense per-repo sections into the narrative the user asked for (spoken standup, written update, changelog). Every claim must trace to a line of the command's output — the digest is the ground truth; add no work items it doesn't contain. Keep decisions and open items; they are the part a standup exists for.
3. **Carry the gaps.** If the Gaps section is non-empty, say so ("1 session has only a placeholder summary") rather than narrating around it — no signal, no claim.
4. **Respect the window.** If the user's question spans longer than the window you ran, re-run with a wider `--last` instead of extrapolating.

## Redaction boundary (qanungo#8)

The command's output is **already redacted, default ON** — `[REDACTED:<pattern>]` markers are load-bearing. Never guess at, reconstruct, or ask the archive for what a marker hides. Never pass `--no-redact` on your own initiative; if the user explicitly asks for it, note that off is a deliberate choice and proceed only for their own local reading. The footer's `patterns <revision>` line says which pattern set scrubbed the text.

## Boundaries

- Read-only end to end: the command mirrors and folds; nothing writes to the archive.
- Copilot/Claude/Codex sessions all appear — the summaries are munshi's own format regardless of harness.
- The instrumentation footer is for the user's eyes when they ask about performance or coverage; drop it from a polished narrative.
