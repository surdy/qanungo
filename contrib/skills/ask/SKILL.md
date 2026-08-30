---
name: ask
description: Answer questions about the user's own past AI-coding sessions from `qanungo ask` — a ranked search over the archive's session summaries. Use when the user asks "when did I…", "have I ever…", "which session did X", "did we already decide…", "find that session where…" — across every machine, repo, and harness.
---

# ask

The interpretation half of qanungo's ask-your-history lane (qanungo#10). `qanungo ask "<query>"` computes the *retrieval* — a deterministic ranked list of matching session summaries, scrubbed, each cited by the content hash of the `summary.md` it was read from; this skill turns those hits into the *answer* the user asked for. The synthesis happens here, in the harness; qanungo stays deterministic and LLM-free.

## Steps

1. **Run the command.** `qanungo ask "payments api"` — the query is a positional argument. **There is no default window**: absent `--last`, the search covers all of the archive's history, which is what "have I ever" means. Add `--last 4w` only when the user deliberately narrows (`h`/`d`/`w` units — no `m` by design); never impose a window they didn't imply. `--limit` caps the ranking (default 10); raise it when the output says more matched than it showed.
2. **Read the document as retrieval, not as prose.** Output: a scope line (how many sessions were searched, over what reach, plus any that *couldn't* be searched), the terms it actually searched for after dropping fragments and stop words, then the ranked hits — each `### <rank>. <title>` with a metadata line (harness · repository · branch · archived date · score · `source_hash`), a blockquote snippet, and the fields it matched in.
3. **Answer only from the hits.** Every claim traces to a returned hit; the snippet is the evidence and the ranked list is the ground truth. The score is the stated rubric — title and repository weigh most, a summary carrying more of the query outranks one carrying less — not relevance magic. A thin top hit is a thin answer, and saying "the closest match is weak" is the correct answer. **No matches is an answer** ("nothing in the archive's summaries matches that"), never a licence to speculate. A query of only stop words or short fragments is refused before the archive is touched — reword it into a specific term.
4. **Carry the coverage.** The scope line's searched count and its "could not be searched" count are the honest bounds of the answer; if sessions were unsearchable, say so rather than narrating around them — no signal, no claim.
5. **Cite, and stop where the evidence stops.** Give the user the `source_hash` of any session you lean on; the footer prints the exact archive request that redeems it for the whole summary or the transcript behind it. If the snippet isn't enough, hand over the citation and say deeper transcript search is a planned follow-on (`--verbatim` does **not** exist) — the `session-recall` skill can take it further where a session identity is in hand. Never grep transcripts by hand, and never state a transcript-level fact from summary evidence.

## Redaction boundary (qanungo#8)

The command's output is **already redacted, default ON** — `[REDACTED:<pattern>]` markers are load-bearing. Never guess at, reconstruct, or ask the archive for what a marker hides; a query that lands on a secret-bearing line still cannot render the secret. Never pass `--no-redact` on your own initiative; if the user asks for it, note that off is a deliberate choice for their own local reading — the footer confesses it in bold by design. The footer's pattern revision says which set scrubbed the text.

## Boundaries

- Read-only end to end: the search mirrors and ranks the local cache; nothing writes to the archive.
- It searches the archive's **curated summaries**, not transcripts. A fact munshi never wrote into a `summary.md` is invisible to `ask` — that is a coverage statement about the corpus, not a defect, and it belongs in the answer when a search comes back empty.
- Every harness, machine, and repository in the archive is in scope; the only narrowing knobs are `--last` and `--limit`.
- The instrumentation footer is for the user's eyes when they ask about coverage or performance; drop it from a polished answer, but keep the citations.
