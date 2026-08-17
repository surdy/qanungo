---
name: skill-finder
description: Turn a qanungo "Skill & Agent Finder" finding into a drafted reusable skill file or subagent definition. Use when qanungo (or the user) surfaces a repeated multi-step prompt flow worth promoting into tooling. Writes a new skill/agent file under normal permission prompts — never the archive.
---

# skill-finder

The write-posture half of qanungo's **Skill & Agent Finder** (qanungo#13). qanungo detects the repeated flow and emits a *finding*; this skill drafts the *skill or agent* from it. qanungo stays read-only; the authoring happens here, under the user's permission prompts.

> **Status: scaffold.** The `qanungo` CLI does not exist yet (see qanungo#13, #7). Until it does, drive this skill from a finding the user pastes in, or from `session-recall` output. Wire the `qanungo skill-finder` command in once it lands.

## Inputs
A finding of the shape: *this N-step prompt flow recurred M times across repos `<...>`*, with evidence — Patwari `source_hash`es for representative instances. (Once the CLI exists: `qanungo skill-finder --json`.)

## Steps
1. **Read the instances.** For each `source_hash`, open the redacted transcript via `session-recall` and extract the actual recurring steps — the real prompts and tool calls, not a paraphrase.
2. **Decide skill vs agent.** A repeatable *procedure* the user invokes → a **skill** (`SKILL.md`). A delegated, context-isolating *role* → a **subagent** (`.claude/agents/*.md`). If it's neither (a one-off), say so and stop.
3. **Draft the file.** Write a `SKILL.md` (frontmatter `name` + `description`, then steps) or an agent definition, generalized from the instances — parameterize what varied across them.
4. **Write on approval only.** Show the drafted file first. On approval, write it to the repo's `.claude/skills/<name>/SKILL.md` or `.claude/agents/<name>.md`. Writes only the new tooling file — never the archive.
5. **Note provenance.** In the drafted file's description or a comment, reference that it was promoted from a qanungo finding, so its origin is traceable.

## Boundaries
- Read-only to the archive; write-only to new skill/agent files.
- Redaction applies to any transcript span you surface (qanungo#8).
- This is the engine that promotes the other `contrib/` skills (`/standup`, `/coach`, `/cost-review`, `instructions-editor`) — but each still ships hand-reviewed, not auto-written.
