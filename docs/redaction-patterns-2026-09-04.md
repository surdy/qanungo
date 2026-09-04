# Redaction pattern research — amendment of 2026-09-04

Input for qanungo#17 (the same pair, written as a sentence). This file is an **amendment**, not a
replacement. [`redaction-patterns-2026-08-24.md`](redaction-patterns-2026-08-24.md) remains the
provenance of the original twelve patterns, of §0's standing trade, and of the profanity list;
[`redaction-patterns-2026-08-31.md`](redaction-patterns-2026-08-31.md) remains the provenance of
`prose-credential` and `paired-username`. Read those first; this one records only what
`PATTERN_REVISION` moved for.

`PATTERN_REVISION` is now `2026-09-04`. It moved because the set **gained one id**
(`prose-paired-username`) — documents rendered before and after are no longer claiming the same
scrub, which is the whole reason the constant is stamped in a footer.

## 1. What was found

The `qanungo flows` review of 2026-09-04 (#13) read the archive's global top-five clusters and found
the string qanungo#15 was filed from *still half-readable*, one revision after it was fixed:

> `use http://line.…/ username : feedface00 and password [REDACTED:prose-credential] for xtream. and
> http://line.…/get.php?username=[REDACTED:paired-username]&password=[REDACTED:secret-assignment]&type=m3u_plus`

Three of the four credentials in that string go. The fourth — the **spaced-prose username** — escapes
both of #15's patterns, and by design in each case:

- `paired-username` requires a query-parameter context: the byte before the name must be `?` or `&`,
  and the run of URL bytes around it must contain `://`. `username : feedface00` in a sentence is a
  query parameter to nobody.
- `prose-credential` deliberately excludes `username` from `PROSE_SECRET_WORDS`, on the argument
  §2 of the 2026-08-31 file makes: a username is not a secret, and a report that redacts the
  reader's own login everywhere it appears has stopped being a report.

That argument is still right, and it is not what this string is. **An Xtream username is half of a
live login**, and the evidence that it is has been sitting on the same line the whole time: the
password half now fires `prose-credential` there. §3 of the 2026-08-31 file closed with "a prose
*username* pattern" on the deferred list, on the ground that "the pair is what is live, and §3
catches the pair in the only form where the two halves are provably in the same object". #13's flows
lane is what made that ground give way: pooled across the whole archive rather than per repository,
the excerpt is promoted into the **global top five**, where the skill-finder skill's reader sees it.

Pre-existing, not a #13 regression — `doctor` rendered the same excerpt identically before flows
existed. Filed as qanungo#17.

## 2. `prose-paired-username`

`paired_username`'s sibling, with the same evidence — **the password beside it** — measured over a
sentence instead of over a URL.

| Element | Rule | Why it is safe |
|---|---|---|
| noun | `username` or `user`, whole word, case-insensitive, optionally in a matching code span or quote pair | The same two names `PAIRED_USER_KEYS` carries, because it is the same pattern outside a URL. `usernames` and `user_id` are not matched: the whole-word test measures the alphabetic run and refuses a trailing word byte. |
| context | the byte before the noun is **not** `?` or `&` | The query form is `paired_username`'s, which has the tighter evidence of a URL run. See §2.1. |
| gap | either a **separator** (`:` or `=`, with any blanks around it, never `==`) or `prose-credential`'s **one space**, optionally with one copula (`is`, `was`) and one more space | The two say different things about the value, below. |
| value | base62 plus `-` and `_`, optionally wrapped in `` ` ``, `"`, or `'`; **≥ 6 characters**; **at least one letter**; **not SHOUTING_SNAKE_CASE**; **and a digit too, when the gap was not a separator** | See §2.2. |
| trigger | a password-class pattern **fires** within the same sentence span | The whole of why the value goes. See §2.3. |

### 2.1 The two patterns partition; they do not race

`prose_paired_username` stands down whenever the byte before the noun is `?` or `&`. That is not
politeness, it is what makes the pair of patterns a partition rather than a race inside
`best_match`, which resolves ties by matcher order and would otherwise decide which id a rendered
URL carries. `paired-username` is the id that tells the truth about a query parameter; this one is
the id that tells the truth about a sentence. The stand-down is why **`paired-username`'s archive
count cannot move by construction**, and §4 measures that it did not.

It also means a bare `?username=x&password=y` with no scheme — a shell line, a struct literal — is
still matched by neither. That was #15's decision and it is unchanged.

### 2.2 What the value has to prove depends on what bound it

- **With a separator**: the credential charset, ≥ 6, and at least one letter. A **plain word is
  allowed**, because the separator is structure the sentence had to supply: `user: postgres` beside a
  live password is a login, and *user alice logged in* is not written with a colon. The letter test
  is what keeps `user: 61184` from reading as a name.
- **Without one**: everything above **and a digit**, which is `prose-credential`'s own floor. Bare
  prose has nothing on the left but a noun and a space, and *the user is testing*, *user alice logged
  in*, and *the user was notified* are that exact grammar. An all-letter run after a bare `user` is a
  word, and this pattern does not eat words.

**On the floor.** Six, not `prose-credential`'s twelve, and the difference is what each pattern is
anchored on. `prose_credential` has nothing but the value's own shape, so the shape does all the
work. Here the evidence is the password beside it, and a login name is allowed to be short. Six is
where `admin`, `alice`, `guest`, and `me` stop.

**Wrappers, SHOUTING_SNAKE_CASE, the dotted tail, and the vendor stand-down** are all
`prose_credential`'s, for `prose_credential`'s reasons (§2 of the 2026-08-31 file): a wrapper is a
boundary and never evidence, a wrapped value must close in the value charset or the match is refused
whole, an `_` with no lowercase is a name rather than key material, and a value one of the vendor
matchers recognizes is left to the matcher that knows how far it runs.

**One guard is this pattern's own**, and the archive asked for it — see §4.

### 2.3 The trigger is a call, never a re-implementation

The value is scrubbed for one reason: a password-class pattern **accepts** within the span. Accepts,
not "the word is nearby". Three ways a password is written outside a query string, each asked of the
matcher that owns it:

1. **`prose_credential`, on a password-family noun.** `PROSE_SECRET_WORDS` narrowed to `password`,
   `passwords`, `passwd`, `passphrase`, `pass`, `pwd` — on exactly `PAIRED_PASSWORD_WORDS`' reasoning:
   a `token` or a `secret` in the same sentence as a `user` is a session and a user id, which is what
   half the lines in any transcript look like. It is the *password* that makes a login.
2. **`secret_assignment`, on a password-family key.** The key may be quoted, which is how a JSONL
   transcript writes most of them.
3. **`url_credentials`**, for the `scheme://user:pass@host` form written beside a prose repetition of
   the same name.

So a password those three refuse — an empty value, a bare number, an expression, a sentence — is no
pair, and the username stays. `user : fake64f1ab and the password is stored in the keychain` scrubs
nothing. Re-testing the value here instead of calling them is exactly the drift that would let the
two halves of a pair disagree about whether there is a pair, and §3 clause 4 of the 2026-08-31 file
made the same choice for the same reason.

### 2.4 The span is a sentence, and it is bounded twice

Two bounds, both of which hold:

- **A newline ends it**, on either side. That is the natural sentence boundary in a rendered
  transcript, and it is what keeps a `user:` line of a YAML block from pairing with a `password:`
  three lines below it.
- **`MAX_PAIR_SPAN_BYTES` = 256** either way, which is what keeps the search a constant rather than a
  walk over a two-megabyte record. It is generous against the production sighting — twenty-five bytes
  separate the username's value from the word `password` — and mean against a paragraph.

**The byte bound is the one that does the work on raw JSONL**, where a record is one line and the
newlines inside it are two-byte `\n` escapes rather than newlines. The archive scan therefore sees
*wider* spans than any rendered surface does. That direction is deliberate and it is the safe one:
the scan over-counts and can never under-count, so a fire class it shows is a class a reader can be
asked about. One pinhole in that claim, for honesty: JSON escaping also *inflates* raw byte
distance, so a rendered pair sitting just inside 256 bytes with many escaped characters between the
halves can fall outside 256 raw bytes and be missed by the scan while still firing at render — a
statistics-only divergence, never a leak in either direction.

### 2.5 Idempotence

**Both halves scrub in the same pass, in either order.** `scrub_secrets` matches against the original
bytes and writes into a separate buffer, so a password already replaced in the *output* is still live
in the text this pattern's span search reads. A second pass then decides nothing: the username is
itself a marker by then, a marker is not a value, and the scanner refuses any span containing one.

The one case that does change is text arriving **already scrubbed by an earlier revision**:
`user : feedface00 and password [REDACTED:prose-credential]` has no *firing* password in it, so the
username stays readable. That is `paired-username`'s answer to the same question, given for the same
reason — this pattern reports on evidence it can see, and inventing a pair out of a marker would be
guessing. It is a named recall cost, below.

## 3. `HARNESS_PREFIXES`: three shapes certified, two refused

A separate concern that travelled with the same issue, in `repetition.rs` rather than here. Recorded
in this file because it was measured on the same archive in the same pass; the list's own rules are
in its rustdoc.

Method, as for the `<system_reminder>` entry: enumerate every `Event::User` the mirror holds, tally
the openings that `authored()` does **not** already exclude, and for each candidate compute the
longest opening every occurrence agrees on. 11,512 user messages over 1,904 blobs, 3,835 of them
already excluded by the shipped list.

| Candidate | Occurrences / sessions | Distinct shapes | Verdict |
|---|---:|---:|---|
| `You are an assistant operating inside the Notesmith vault` | 55 / 55 | 3 (three vault names) | **certified** |
| `You are an assistant operating inside a Notesmith vault` | 26 / 26 | 3 (three trailing shapes) | **certified** |
| `Approach this as the design lead …` (short form, as filed) | — | — | **refused** |
| `Approach this as the design lead at a small studio known for their versatility, giving every client a visual identity pitched at the treatment the task actually calls for.` | 44 / 44 | 1 shape for 418 bytes | **certified** |
| `Draw as the engineer …` (short form, as filed) | — | — | **refused** |
| `Draw as the engineer who has to live with the decision, … assemble from prose` | 11 / 11 | 2 shapes agreeing for 194 bytes | **certified** |

**The Notesmith block** is the strongest of the three and the least arguable: it names a product and
lists that product's own MCP tools, and nothing but its MCP host writes it. It is spelled as **two
literals** rather than one shorter one, on `SLACK_PREFIXES`' reasoning — `You are an assistant
operating inside ` is a sentence a person could write, and a class is a widening nobody has to
notice. The archive's *human* mentions of the vault are all mid-sentence ("I checked your Notesmith
vault…", "There were some issues with Notesmith vault MCP…"), and an opening test cannot reach them,
which is why matching is on the opening and not on a substring.

**The two skill bodies are the first entries in that list that are ordinary English rather than a
marker**, and the length is the whole of what makes them certifiable. The short forms qanungo#17
proposed are refused: a person writing a design brief plausibly opens a message "Approach this as the
design lead…", and a person asking for a diagram plausibly opens "Draw as the engineer…". What is
certified instead is the *archive-computed* longest common opening, cut back to a sentence — roughly
five times longer, and carrying the clause that gives it away (`at a small studio known for their
versatility…`, `not as a decorator: a diagram earns its place when…`).

The diagramming entry is **the thinner of the two and is named as such**: eleven sightings, against
the thirty-two the `<system_reminder>` entry was certified on. It is also the one whose body has
already been seen to drift — its two archived shapes differ by a hyphen where the other has an em
dash, at byte 195 — which is why the certified literal is cut at 191. A skill body that is edited
past the cut stops matching and the noise returns to the clustering: the failure the list documents
about itself, and the safe direction for it to fail in.

Neither phrase opens any message in the archive that is not the injected body. Both appear elsewhere
only inside `<task-notification>`, which the list already excludes.

## 4. Measured on the archive

Same harness as before — `cargo test --release --test redaction_scan -- --ignored --nocapture`,
counts only, the scan cannot report anything else. The mirror has grown again: **1,904 blobs,
990,713 records, 3.75 GiB**, so the columns below are not comparable with §4 of the 2026-08-31 file,
only with each other. The `before` column is the shipped `2026-08-31` set re-run on today's mirror.

| id | before (`2026-08-31` set) | 1: first cut | 2: base64 padding refused |
|---|---:|---:|---:|
| `secret-assignment` | 2,381 | 2,381 | 2,381 |
| `prose-credential` | 242 | 242 | 242 |
| `url-credentials` | 144 | 144 | 144 |
| `github-token` | 62 | 62 | 62 |
| `paired-username` | 48 | 48 | 48 |
| `authorization-header` | 36 | 36 | 36 |
| `jwt` | 32 | 32 | 32 |
| `google-api-key` | 14 | 14 | 14 |
| `anthropic-key` | 10 | 10 | 10 |
| `aws-access-key-id` | 10 | 10 | 10 |
| `private-key-block` | 9 | 9 | 9 |
| `prose-paired-username` | — | 176 | **171** |
| **total** | **2,988** | 3,164 | **3,159** |
| blobs firing anything | 102 / 1,904 | 102 / 1,904 | **102 / 1,904** |

**No existing pattern moved by a single hit, in either round**, and `paired-username` least of all —
§2.1's stand-down makes that a construction property rather than a measurement, and the measurement
is here to prove the construction. `prose-paired-username` fires on no blob that was not already
firing something, which is what one expects of a pattern whose evidence is another pattern.

### The over-match round, and what it was

Inspecting the new fires from the **scrubbed** text — so every value in them was already a marker
before anything was read — round 1 had exactly one fire class that was not an IPTV credential: a
Kubernetes `Secret` manifest, five fires in two spellings — once as a file, once as a diff of the
same file.

```
type: Opaque
data:
  username: [REDACTED:prose-paired-username]=
  password: [REDACTED:secret-assignment]
```

Two things are wrong with it and only one is about precision. The base64 value ends in **padding**,
which the prose charset stops at, so the marker rendered with a stray `=` beside it — a *partial*
redaction in form, and whole-value-or-nothing is not a rule with an exception for harmless
leftovers. And the pair only existed because the scan reads raw JSONL, where those two lines are one
line (§2.4); at render time the newline between them would have ended the span.

The fix is structural and mirrors the dotted-tail stand-down already in the pattern: **a bare value
whose next byte is `=` did not end where this charset ran out** — it was written in the assignment's
charset, so stand down and leave it to whoever owns it. 176 → 171, and the five fires it removed were
that one manifest. Nothing else moved.

`prose_credential` has the same gap and is *not* changed here: fixing it would move a pre-existing
pattern's count in a revision whose whole claim is that none moved. Named in §5 instead.

### What the surviving fires are

All 171 are two classes, and they are the two classes `prose-credential` already had:

- **The IPTV credential pair**, in every spelling the archive writes it — `username : <value> and
  password …`, `username <value> password …`, `--es username <value> --es password …`,
  `--user <value> --pass …`, `` user `<value>` · pass `<value>` ``,
  `Username `<value>`, Password `<value>``, `user: <value>` in a dataset's `meta` beside a `password`
  in its users list, and `(user <value> / pass <value>)` in a verification verdict.
- **This project's own working notes and issue text** *quoting* that pair while describing the gap —
  including the #15 provenance file and the #13 review that filed #17.

There is no third class. `user`/`username` with no firing password on its line contributes nothing
across 990,713 records, which is the adjacency requirement doing the work it was built for.

### What it did to the rendered lanes

Verified by running both binaries against the same cache, parent `322a1e8` versus this build.

- `report`, `cost` — **byte-identical** but for their own timestamps and timing instrumentation.
  Neither renders transcript content and neither is wired to the redactor.
- `standup`, `ask` — **body byte-identical**; only the footer's revision stamp moved
  (`2026-08-31` → `2026-09-04`). Nothing matched in either window either way.
- `doctor` — the line the issue was filed about now renders
  `username : [REDACTED:prose-paired-username]`, and the footer tally went from
  `secret-assignment×1, prose-credential×1, paired-username×1` to that plus
  `prose-paired-username×1`. §3's certified prefixes moved the denominators (see below).
- `flows` — the same excerpt, the same footer change, in the cluster the issue quoted.

The `HARNESS_PREFIXES` half moved more, and moving it was the point:

| | parent | this build |
|---|---:|---:|
| harness-written user messages | 3,066 | **3,189** |
| comparable (typed, long enough) | 4,393 | **4,270** |
| `doctor` clusters / repositories | 36 in 8 | **29 in 6** |
| `doctor` conversations | 540 | **541** |
| `flows` clusters | 63 | **63** |
| `flows` conversations | 899 | **900** |
| `flows` multi-step flows | 6 | **6** |

123 of the 906 listed sessions' user messages are now excluded (the scan's 136 is over 1,904 cached
blobs, which include snapshots the current listing no longer carries). Nine `doctor` clusters
disappeared and every one was certified boilerplate; the total fell only to 29, so **two clusters
that did not exist before now do**. In `alice/notes` the render's "_2 further clusters in this
repository are not shown._" line is gone and three real clusters stand where the boilerplate did.
`alice/display` and `alice/tube` drop out of the document entirely — the design-skill body
was the *only* cluster either had.

`flows` is the interesting one. Its three boilerplate clusters — including the **#1 cluster of the
whole archive**, the Notesmith block at 90 occurrences across 81 sessions — are gone, and the total
is unchanged at 63, so **three clusters that did not exist before now do**. The mechanism is visible
in the row above it: dropping the injected bodies un-merged one pair of sessions that had been read
as a single conversation (899 → 900, and 540 → 541 in `doctor`), because a share of what they had in
common was the boilerplate. A cluster that spanned one conversation now spans two and clears
`MIN_CLUSTER_SESSIONS`. That is the certification paying for itself twice: noise out, and findings
that the noise was suppressing in.

Neither document renders any of the three certified shapes anywhere any more.

## 5. Still deferred

Everything in §5 of the 2026-08-24 and 2026-08-31 files, unchanged — entropy scoring above all. This
pattern looks at no randomness either; it is a noun, a gap, a value shape, and a neighbour.

Removed from that list: **a prose username pattern**, which is what this file is. The 2026-08-31
entry refusing it was right about its reason (a username alone is not a credential) and wrong about
its conclusion (that the query form is the only place the two halves are provably together — a
sentence is another).

Added to it:

- **`prose_credential`'s own base64-padding tail.** §4's fix is in `prose_paired_username` only.
  `password <base64>=` would still render a marker with a stray `=` beside it. It has never fired on
  this archive; when it does, the guard moves down into the shared code and a revision that is
  allowed to move `prose-credential`'s count pays for it.
- **A pair whose password half is already a marker.** §2.5: text scrubbed by an earlier revision and
  re-scrubbed by this one keeps its username. Treating a marker as evidence of a pair would mean
  redacting on the strength of a string any document can contain, and the honest answer is to report
  what this pass can see.
- **Nouns beyond `user` and `username`.** `login`, `account`, `email`, `id`. One row each when the
  archive shows one; `PAIRED_USER_KEYS` has carried exactly these two since #15 and neither has cost
  a miss.
- **Chat-log speaker labels.** A transcript line shaped `user: please reset the password <cred>`
  reads `user:` as a separator-form noun and eats the first ≥6-letter word after it ("please") —
  only ever beside a genuinely firing credential, and zero incidence across 993,777 archived
  records. Failure direction is one eaten word of prose, never a leak; a speaker-label grammar
  (`user:` at line start followed by sentence-shaped prose) is the fix if the archive ever grows
  the shape.
- **A password further away than a sentence.** A credentials block that puts the username and the
  password more than 256 bytes apart, or on separate lines, is not matched. Widening the span is the
  change most likely to manufacture a pair out of two unrelated lines, and the bound is where the
  archive says a real pair fits.
- **The `--user x --password y` form split across a line continuation.** A backslash-newline between
  the two halves ends the span. The archive writes this form on one line, so it costs nothing today.

## Sources

- qanungo#17, and the `qanungo flows` review of 2026-09-04 (#13) that filed it — the promoted excerpt
  in §1, both recommendations, and the `HARNESS_PREFIXES` candidates in §3.
- `docs/redaction-patterns-2026-08-31.md` — `prose-credential` and `paired-username`, whose design
  this pattern mirrors clause for clause, and whose §5 deferral this file reverses.
- `docs/redaction-patterns-2026-08-24.md` — everything neither file restates.
- The local mirror, scanned 2026-09-04: 1,904 blobs, 990,713 records, 3.75 GiB, reproducible with
  `cargo test --release --test redaction_scan -- --ignored --nocapture`.
