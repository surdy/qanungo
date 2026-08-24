---
schema_version: 2
id: "copilot:33333333-3333-4333-8333-333333333333"
agent: "copilot-cli"
session_id: "33333333-3333-4333-8333-333333333333"
project: "munshi"
project_identity: "github.com/surdy/munshi"
project_component: "munshi-3333"
repository: "surdy/munshi"
branch: "main"
started_at: "2026-08-21T09:00:00.000Z"
updated_at: "2026-08-21T10:00:00.000Z"
completion_reason: "complete"
summary_revision: 1
source_cursor: 60
normalizer_version: 3
source_cursor_records: 60
source_cursor_bytes: 4096
source_prefix_hash: "sha256:c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3"
source_bytes: 4096
source_hash: "sha256:c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3"
artifact_set_version: 2
transcript_sha256: "sha256:c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3"
extracted_outputs: []
tags:
  - "archive"
---

# Tombstone the degenerate snapshots the backfill left behind

## Goal

Re-elect the complete snapshots that a 2026-07-28 backfill run shadowed with summary-only captures.

## Work completed

- Tombstoned 56 summary-only snapshots so the complete siblings are projected again.

## Decisions

- The read side keeps its sibling fallback as defense in depth rather than trusting the fix.

## Files changed

- crates/munshi/src/archive.rs

## Commands and validation

- cargo test

## Open items

- Watch whether any further backfill writes the same shape.
