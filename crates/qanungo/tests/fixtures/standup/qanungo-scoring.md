---
schema_version: 2
id: "claude-code:11111111-1111-4111-8111-111111111111"
agent: "claude-code"
session_id: "11111111-1111-4111-8111-111111111111"
project: "qanungo"
project_identity: "github.com/surdy/qanungo"
project_component: "qanungo-1111"
repository: "surdy/qanungo"
branch: "main"
started_at: "2026-08-20T09:00:00.000Z"
updated_at: "2026-08-20T11:30:00.000Z"
completion_reason: "complete"
summary_revision: 1
source_cursor: 120
normalizer_version: 3
source_cursor_records: 120
source_cursor_bytes: 4096
source_prefix_hash: "sha256:a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"
source_bytes: 4096
source_hash: "sha256:a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"
artifact_set_version: 2
transcript_sha256: "sha256:a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"
extracted_outputs: []
tags:
  - "rust"
  - "scoring"
---

# Ship the scoring lane behind a rule pack stamp

## Goal

Compute every score from the current rule pack on every run, and stamp the pack digest so that two reports can tell whether they compare at all.

## Work completed

- Recomputed all of history with the current pack rather than reading frozen scores back.
- Stamped the rule pack digest into the instrumentation footer.

## Decisions

- Scores are recomputed on every run rather than persisted.
- A lane that no typed signal feeds is never scored.

## Files changed

- crates/qanungo/src/scoring.rs

## Commands and validation

- cargo test

## Open items

- Decide whether the dashboard reads the stamp or the pack itself.
