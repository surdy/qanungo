# Transcript fixtures

## `munshi/`

Copied verbatim from the Munshi suite's own fixtures (`munshi/fixtures/…`), which are the
pinned-envelope examples `munshi-transcript` itself is validated against. They exercise the fold
against transcripts nobody wrote for qanungo — the first three are ordinary short sessions, so they
deliberately fire **no** rule.

| File | Origin |
| --- | --- |
| `claude-code-2.1.44-normal.jsonl` | `fixtures/claude-code-2.1.44/normal/0c1a0de0-…-000000000001.jsonl` |
| `copilot-1.0.70-envelope.jsonl` | `fixtures/copilot-1.0.70/transcript/synthetic-envelope.jsonl` |
| `copilot-1.0.76-tool-activity.jsonl` | `fixtures/copilot-tool-activity/aaaaaaaa-…/events.jsonl` |
| `claude-code-2.1.235-compaction.jsonl` | `fixtures/claude-code-compaction/transcript/0c1a0de0-…-000000077003.jsonl` |
| `copilot-1.0.76-compaction.jsonl` | `fixtures/copilot-compaction/dddddddd-…/events.jsonl` |
| `claude-code-2.1.235-invocation.jsonl` | `fixtures/claude-code-invocation/transcript/0c1a0de0-…-000000077004.jsonl` |
| `copilot-1.0.76-invocation.jsonl` | `fixtures/copilot-invocation/eeeeeeee-…/events.jsonl` |

The last four are the exception to that note, and they are borrowed rather than synthesized on
purpose: they are the fixtures munshi cut for issue #77's promotions, so the record shapes in them
are the ones the interpreter was actually certified against, traps included. The two compaction
files fire **Compaction churn** and nothing else.

They pin the two counting rules a consumer cannot get right by guessing. The Copilot file writes
five `session.compaction_start` records and five `session.compaction_complete` records — ten markers
for what the fold must report as **four** compactions, because one completion states `success:false`
and a start is not a compaction. The Claude Code file writes five `compact_boundary` records and
states no outcome on any of them, which is why the failure filter is spelled
`succeeded != Some(false)`: `== Some(true)` would read this whole harness as having compacted
nothing. Both carry the malformed metadata munshi's own survey found — a `preTokens` that is a
string, a `compactMetadata` that is a number, a marker with no metadata at all, a
`compaction_complete` whose `data` is unreadable — so the pre-compaction totals the finding carries
as context are stated on fewer compactions than there are, which is the shape a rendering path has
to handle rather than assume away.

The two **invocation** files are pull B's, and they are borrowed for the same reason plus one more:
the classification of which names are review passes is qanungo's to make, so the fixture that tests
it should be the one written by somebody who was not making it. Between them they carry every
decoy the classifier has to survive.

The Claude Code file invokes eight skills, including `code-review` and `security-review` (both
review passes), `simplify` (the quality pass this consumer deliberately **excludes** — it disclaims
bug-hunting and redirects to `code-review`), and `artifact-design`/`run` (not reviews). It also
carries the two traps that decide whether the classifier is right: a `SlashCommand` **tool**
invoking `/code-review`, and a typed `<command-name>/security-review</command-name>` slash command.
Neither counts — in this harness a review is invoked through the `Skill` tool, and the slash surface
exists in this lane only to make the harness *observable*. Its one shell command is `cargo test`, so
it ships nothing and is not eligible for the review rate at all. It is deliberately **not** in the
"munshi fixtures fire nothing" list: it carries 15+ user records to exercise the slash-command edge
cases, which is the Babysitting shape by construction.

The Copilot file is the observability case in one record: `/chronicle improve` sits in a
`user.message` as prose with no marker of any kind, beside a real `skill.invoked`. That is exactly
why Copilot is **couldn't-look** for this rule — its skill surface is typed and its slash surface is
not, and partial observability cannot support the sentence "Copilot ran no review".

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

`unreviewed-ship.jsonl` and `reviewed-ship.jsonl` are the Code Review lane's pair (qanungo #4,
munshi#77 pull B). They are the same short session twice, differing in one record: whether a
`code-review` skill ran before the commit. The unreviewed one fires **Shipped without review**
and nothing else; the reviewed one fires nothing at all.

`unreviewed-ship.jsonl` carries the ship parser's **positive** surface in four `Bash` calls — the
compound `git add -A && git status --porcelain && git commit -q -m …` shape the archive actually
writes, a `git -C /work/fixture commit --amend` with a global flag before the subcommand, a plain
`git commit`, and — the one that matters — `git log --oneline -5 | grep -i commit`, which contains
the words and runs no commit. A substring test passes three of those four and is wrong.

That is the surface the fixture covers, **not** the whole behaviour of the parser, and the
difference is worth writing down. `is_commit_command` is a token test over unquoted `&&` / `;` /
`|` / newline splits, not a shell parser. Its **known negatives**, probed against the mirror and
left unfixed on purpose:

| Shape | Reads as | Right? |
| --- | --- | --- |
| `git commit --dry-run`, `git commit --help` | ship | no — runs no commit |
| `echo "… && git commit"`, a heredoc body line starting `git commit` | ship | no — quoting and heredocs are not modelled |
| `(git commit …)`, `command git commit`, `bash -c '… git commit …'` | not a ship | no |
| `GIT_EDITOR=true git commit`, `timeout 60 git commit` | not a ship | no — only `sudo` is stepped over |
| `git --work-tree x commit` | not a ship | no — only `-C`/`-c` are stepped over |
| `git commit --amend` | ship | yes — amending is shipping |
| `gh …`, `jj commit`, `hg commit` | not a ship | yes — only `git` ships |

Total disagreement across the whole mirror is **one session**, and the errors run in both
directions at about the same size, so this bounds the reported rate rather than biasing it. The
unit test `the_commit_parser_has_documented_known_negatives` in `metrics.rs` pins every wrong row
above as *current behaviour*, so the disagreement is a recorded decision a future change has to
walk past deliberately rather than a surprise. Growing the parser into a shell tokenizer would need
its own test suite to be trustworthy and buys a single session, which is why it has not been done.

It is also the anchor slice's canary for this rule. The first commit message carries a planted,
live-*shaped* GitHub token, so the test that resolves each anchor through the excerpt route asserts
the credential comes back scrubbed while `CANARY_COMMIT_SUBJECT` around it survives. That pairing is
the evidence argument in executable form: a commit message is operator-written text, which is
exactly why anchoring it is worth doing *and* why it goes out through the redactor. As everywhere
else in this tree, the token has never been real and has `CANARY` spelled through its body.

`error-with-planted-secret.jsonl` is the evidence-excerpt slice's canary (qanungo #5). It is the
error-rate shape again — twelve `Bash` calls, the first six failing — with two of those failures
carrying a planted, live-*shaped* credential in the tool result's own text: a GitHub classic token
and an Anthropic key. Neither has ever been real; each is a shape with `CANARY` spelled through its
body, exactly as the standup lane's `qanungo-cost.md` does it. `tests/dashboard.rs` folds it, takes
the anchors the payload names, fetches every excerpt **over HTTP**, and asserts that both
credentials come back as `[REDACTED:…]` with the sentence around them intact — and that
`--no-redact` brings both back verbatim. A redactor that scrubbed regardless of the flag would pass
the first half and fail the second.

`tool-name-canary.jsonl` puts the planted credential in the one field a coaching surface is
otherwise allowed to render verbatim: the **tool name**. Twelve calls to a tool named
`ghp_CANARY…` — forty characters, inside the identifier clamp's ceiling and carrying none of its
forbidden characters, so the clamp passes it — half of them failing. It exists because decision 9's
"tool names are schema metadata" holds for aggregate lines and stops holding on a surface that
renders transcript text beside them: `tests/dashboard.rs` asserts the name comes back as a marker on
*both* verbatim paths, the anchor in the payload and the excerpt behind it, while an ordinary event
discriminator beside it is untouched.

`high-tool-error-rate.jsonl` and `retry-loop.jsonl` additionally carry `CANARY_*` tokens in every
free-text field (user text, assistant text, tool command input, tool error output, and in the
retry fixture the repeated command itself). Two redaction tests fold them, render a full report,
and assert that not one of those tokens survives into the Markdown — the second exists because the
churn fold is the first metric that compares transcript *content*, and the report must still be
able to say a command ran six times without saying which command it was.

## `cost/`

Synthesized here for the cost lane (qanungo #12), because none of the fixtures above records a
single token figure — the munshi ones predate `assistant_meta` entirely, and the rules ones are
built around timestamps and tool outcomes.

| File | Shape | Pins |
| --- | --- | --- |
| `claude-billing.jsonl` | 9 records, 5 message ids: one API message split across **3** records repeating its `usage` verbatim, a second model, a fast-mode message, a `<synthetic>` placeholder, and a model no price table has heard of | deduplication, per-tier cache pricing, the fast tier, the unbilled line, the unpriced fallthrough |
| `copilot-billing.jsonl` | 3 `assistant.message` records across two models, each with `model` and `outputTokens` and nothing else | the token-only path: volumes by model, no dollars anywhere |

The billed records carry `"inference_geo":"not_available"`, which is what the archive actually
records on 61,122 of its 61,184 usage records — the API stating that no geo-routing premium
applied. It is the base-rate case, so the fixture's dollars are the same as if the field were
absent; the fixture spells it out anyway, because reading it as an unknown region is what priced
the entire first production run at zero.

The figures are round on purpose — 200k input, 100k output, 400k of 1-hour cache write, 1M of
cache read — so a rendered dollar can be checked against `docs/pricing-sources-2026-08-23.md` by
eye rather than by rerunning the arithmetic the code under test just did. The claude fixture's
message ids are also the whole point of two assertions at once: the fold must count **5** messages
across **9** records, and the report must never print one of those ids.

Both files carry `CANARY_*` tokens in every free-text field, including the `cwd` and `gitBranch`
envelope keys, and a redaction test renders a full cost report and asserts that not one of them
survives. That test is a stronger claim here than in `rules/`: the cost fold never reads a
record's classification at all, so the canaries have no path to the document even in principle,
and the test exists to keep it that way.

## `standup/`

Synthesized here for the standup lane (qanungo #9), because nothing above is a `summary.md` at
all — every other fixture in this tree is a transcript, and this lane never reads one.

Each file is a real munshi archive record: YAML frontmatter plus the headed Markdown body
`munshi_transcript::parse_archive_markdown` reads back, with the cursor fields consistent enough to
pass the parser's own cross-checks (`source_prefix_hash == source_hash`,
`source_cursor_records == source_cursor`, `source_cursor_bytes == source_bytes`). They are stored
into a real `BlobCache` under their own content hash by `tests/standup.rs`, so the cache read, the
UTF-8 check, the parse, and the placeholder verdict all run for real.

| File | Shape | Pins |
| --- | --- | --- |
| `qanungo-scoring.md` | `surdy/qanungo`, branch `main`, two decisions, one open item | grouping, the rollup's reading order |
| `qanungo-cost.md` | the same repository, archived later, on a different branch | newest-first ordering, the rollup's exact-duplicate drop (it repeats one of the scoring summary's decisions verbatim) |
| `munshi-tombstone.md` | `surdy/munshi`, one session, `copilot-cli` | the second repository group, and that a group's order is by session count |
| `no-repository.md` | names no `repository` and no `branch` | the labelled unattributed bucket, and that it sorts last |
| `placeholder.md` | carries `summary_placeholder: true` and the `munshi-placeholder-summary` tag | the placeholder gap — its stand-in prose must never reach the document |
| `not-an-archive.md` | plain Markdown, no frontmatter | the unparseable gap |

`qanungo-cost.md` additionally carries three planted, live-*shaped* credentials — a GitHub classic
token, an Anthropic key, and an AWS access key id — one in a work item, one in a decision, one in an
open item. They are the load-bearing fixture of the whole lane: a rendered standup must contain
none of them and three `[REDACTED:…]` markers instead, `--no-redact` must bring all three back, and
the sentences around them must be untouched. None of these strings has ever been a real credential;
each is a shape with `CANARY` spelled through its body.

`munshi-tombstone.md` is `copilot-cli` on purpose. A `summary.md` is munshi's own format whatever
harness produced the session, so the standup lane asks nothing about interpreters — and a fixture
under a second harness is what keeps that from silently becoming untrue.

## `doctor/`

Synthesized here for the instructions doctor (qanungo#11), because the lane compares a surface
nothing above exercises: the text a *person* typed, across two sessions of one repository. They are
Claude Code 2.1.205 transcripts and are stored into a real `BlobCache` under their own content hash
by `tests/doctor.rs`, so the cache read, the interpretation, and the event walk all run.

| File | Shape | Pins |
| --- | --- | --- |
| `repeated-rule.jsonl` | one long instruction, a bare `yes`, a failing `cargo clippy` and the reply to it | the cluster's first half, the minimum-length floor, the friction proxy |
| `repeated-rule-restated.jsonl` | the same instruction reworded in its last three words, plus one unrelated message | the near-duplicate match, and that the unrelated message clusters with nothing |
| `no-repetition.jsonl` | two long messages sharing no phrase with anything | the empty-result run |

The repository each session belongs to is **not** in these files. It comes from the archive's
listing row in production (`MirroredSession::repository`) and is supplied by the test, which is what
lets the same two transcripts be folded under one repository and under two — a cluster in the first
case and none in the second, from identical bytes.

The repeated instruction carries **two** planted credentials, at two positions, and the positions
are the whole point. An Anthropic key sits at character 20, early enough that its marker renders
whole inside the 200-character excerpt. A GitHub classic token starts at character **180**, so it
straddles the excerpt's own cut: 20 of its 40 characters fall inside the window, which is four short
of what the `github-token` pattern needs to recognize it. A clip-then-scrub order would therefore
render that fragment as itself, and `a_repeated_instruction_carrying_a_credential_renders_it_scrubbed`
asserts the wrong order *would* have leaked before asserting that the real one does not — so a change
to the clip ceiling that stopped the token straddling the edge fails the test rather than quietly
turning it into one that cannot fail. Neither credential has ever been real; each is a shape with
`CANARY` spelled through its body.
