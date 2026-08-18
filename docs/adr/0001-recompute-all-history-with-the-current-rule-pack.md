# Recompute all history with the current rule pack

Scores are never frozen per session. Every run recomputes every window it reports on with the rule
pack the running build carries — thresholds, rule set, lane mapping, scoring constants — and no
score is ever read back from a previous run. The alternative, stamping each session with the score
it had when it was first seen, was rejected: a month-over-month arrow drawn across a threshold
change would report *rule* drift as *behaviour* drift, and a coaching report whose arrows cannot be
trusted is worse than one with no arrows in it. Thresholds here are explicitly arbitrary until
measured and are expected to move as the archive teaches us where they belong, so the pack changing
is the normal case, not the exception the design gets to ignore.

This is affordable because derived data in qanungo is disposable by construction. Interpretation is
read-time through `munshi-transcript` (munshi
[ADR 0011](https://github.com/surdy/munshi/blob/main/docs/adr/0011-interpret-transcripts-at-read-time-through-a-shared-streaming-crate.md)),
Patwari never stores a metric or a score (munshi
[ADR 0012](https://github.com/surdy/munshi/blob/main/docs/adr/0012-defer-the-analysis-client-until-a-first-consumer-exists.md)),
and the local mirror is a content-addressed blob cache whose rebuild is "delete and resync". Nothing
has to be invalidated when the pack changes, because nothing derived is kept: re-deriving *is* the
invalidation. Today there is no derived store at all, so recompute-all is simply what already
happens; the decision is written down now because the moment a store lands it will be tempting to
keep last month's numbers in it, and it must not.

What the decision needs in order to be usable by a reader is a way to tell two reports apart, and
that is the **rule-pack stamp**: a sha256 over every rule id, every threshold value, every scoring
constant, the lane→signal mapping, and a version tag for the scoring formula itself, rendered short
in the instrumentation footer of every run. Floats are hashed by bit pattern rather than by a
printed decimal, and names are hashed alongside values, so renaming, reordering, or nudging any of
them moves the stamp. **Two reports are comparable if and only if their stamps match.** A trend
arrow inside one report is safe by construction — both of its windows were computed in the same
process by the same pack — and the stamp is what extends that guarantee to two reports taken weeks
apart.

The stamp is a comparability check, not a provenance record: it says the pack was the same, not
what was in it. When that stops being enough — a dashboard wanting to explain *which* threshold
moved — the pack's entry list is already the material for it, and printing more of it is a
rendering change rather than a redesign.
