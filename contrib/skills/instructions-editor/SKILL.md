---
name: instructions-editor
description: Turn a qanungo "Instructions Doctor" finding into a concrete edit of this repo's CLAUDE.md / AGENTS.md. Use when qanungo (or the user) surfaces that repeated rework in a repo correlates with a missing or weak instruction, and you want to propose the fix. Writes only the user's instruction files, under normal permission prompts — never the archive.
---

# instructions-editor

The write-posture half of qanungo's **Instructions Doctor** (qanungo#11). qanungo computes the *finding*; this skill proposes the *edit*. qanungo stays read-only; the edit happens here, in the harness, under the user's permission prompts.

> **Status: scaffold.** The `qanungo` CLI does not exist yet (see qanungo#11, #7). Until it does, drive this skill from a finding the user pastes in, or from `session-recall` output. Wire the `qanungo instructions-doctor` command in once it lands.

## Inputs
A finding of the shape: *repeated rework / re-explanation in `<repo>` correlates with missing or weak instruction `<topic>`*, plus evidence — Patwari `source_hash`es pointing at the exact transcript moments. (Once the CLI exists: `qanungo instructions-doctor --repo <path> --json`.)

## Steps
1. **Read the evidence.** For each `source_hash`, use the `session-recall` funnel to open the redacted transcript span and confirm the rework pattern is real, not noise. Do not trust the finding blind.
2. **Locate the target.** Find the repo's `CLAUDE.md` (or `AGENTS.md`, or the relevant nested instruction file). If none exists, propose creating one.
3. **Propose a minimal edit.** Draft the smallest instruction that would have prevented the observed rework. Show it as a diff. Explain which finding / evidence it addresses.
4. **Apply on approval only.** Never write without showing the diff first. This skill writes instruction files and nothing else — it does not touch the archive, munshi, or Patwari.
5. **Close the loop.** Note that qanungo can later *measure* whether rework dropped in subsequent sessions (the measured-outcomes differentiator in qanungo#11) — tell the user to re-check after a week of use.

## Boundaries
- Read-only to the archive; write-only to the user's own instruction files.
- Redaction applies to any transcript span you surface (qanungo#8).
- One finding → one focused edit. Don't bundle unrelated instruction changes.
