---
name: ask
description: Answer questions about the user's own past AI-coding sessions from `qanungo ask` — a ranked search over the archive's session summaries. Use when the user asks "when did I…", "have I ever…", "which session did X", "did we already decide…", "find that session where…" — across every machine, repo, and harness.
---

# ask

The interpretation half of qanungo's ask-your-history lane (qanungo#10). `qanungo ask "<query>"` computes the *retrieval* — a deterministic ranked list of matching session summaries, scrubbed, each cited by the content hash of the `summary.md` it was read from; this skill turns those hits into the *answer* the user asked for. The synthesis happens here, in the harness; qanungo stays deterministic and LLM-free.

## Steps

**Requires an archive URL.** `PATWARI_URL` must be set in the shell, or `--patwari-url <URL>` passed on each command; there is no default archive. If a command exits 2 with the missing-archive message, stop and tell the user to finish the install as the README's Install section describes.

1. **Run the command.** `qanungo ask "payments api"` — the query is a positional argument. **There is no default window**: absent `--last`, the search covers all of the archive's history, which is what "have I ever" means. Add `--last 4w` only when the user deliberately narrows (`h`/`d`/`w` units — no `m` by design); never impose a window they didn't imply. `--limit` caps the ranking (default 10); raise it when the output says more matched than it showed.
2. **Read the document as retrieval, not as prose.** Output: a scope line (how many sessions were searched, over what reach, plus any that *couldn't* be searched), the terms it actually searched for after dropping fragments and stop words, then the ranked hits — each `### <rank>. <title>` with a metadata line (harness · repository · branch · archived date · score · `source_hash`), a blockquote snippet, and the fields it matched in.
3. **Answer only from the hits.** Every claim traces to a returned hit; the snippet is the evidence and the ranked list is the ground truth. The score is the stated rubric — title weighs most, then repository and tags, and a summary carrying more of the query outranks one carrying less — not relevance magic. A thin top hit is a thin answer, and saying "the closest match is weak" is the correct answer. **No matches is an answer** ("nothing in the archive's summaries matches that"), never a licence to speculate. A query of only stop words or short fragments is refused before the archive is touched — reword it into a specific term.
4. **Carry the coverage.** The scope line's searched count and its "could not be searched" count are the honest bounds of the answer; if sessions were unsearchable, say so rather than narrating around them — no signal, no claim.
5. **Escalate with `--verbatim` when the snippets aren't enough.** `qanungo ask "<query>" --verbatim` re-runs the same ranking and then searches the transcripts *of the hits it shows* for the same terms, quoting up to five matching lines per session — each `` `event <n>` `` · surface (`user`/`assistant`/`command`/`error`/`output`) · timestamp · excerpt — under a count line saying how many it found against how many it shows. It fetches at most `--limit` transcripts, so narrow the limit before reaching for it. A hit whose transcript couldn't be read says so instead of showing an empty block: report that, don't read it as "nothing there".
6. **Cite, and stop where the evidence stops.** Give the user the `source_hash` of any session you lean on; the footer prints the exact archive request that redeems it for the whole summary or the transcript behind it. Quote only what the output quotes — a `--verbatim` excerpt is the transcript-level evidence, and without one you have summary evidence only. The `session-recall` skill can take it further where a session identity is in hand (ask emits `source_hash` only, never a session id). Never grep transcripts by hand.

## Redaction boundary (qanungo#8)

The command's output is **already redacted, default ON** — `[REDACTED:<pattern>]` markers are load-bearing. Never guess at, reconstruct, or ask the archive for what a marker hides; a query that lands on a secret-bearing line still cannot render the secret, in a snippet or in a `--verbatim` excerpt. Never pass `--no-redact` on your own initiative; if the user asks for it, note that off is a deliberate choice for their own local reading — the footer confesses it in bold by design. The footer's pattern revision says which set scrubbed the text.

## Boundaries

- Read-only end to end: the search mirrors and ranks the local cache; nothing writes to the archive.
- The ranking searches the archive's **curated summaries**, not transcripts. A fact munshi never wrote into a `summary.md` is invisible to `ask` — that is a coverage statement about the corpus, not a defect, and it belongs in the answer when a search comes back empty.
- `--verbatim` **inherits that boundary rather than lifting it**: it is an escalation into the transcripts of the hits already shown, not an archive-wide full-text search. A session no summary matched is never opened, so "no verbatim match" means "not in these sessions", never "not in the archive". Say which.
- Every harness, machine, and repository in the archive is in scope; the only narrowing knobs are `--last` and `--limit` (`--limit` also bounds how many transcripts `--verbatim` fetches).
- The instrumentation footer is for the user's eyes when they ask about coverage or performance; drop it from a polished answer, but keep the citations.
