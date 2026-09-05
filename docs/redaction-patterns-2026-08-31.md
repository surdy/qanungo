# Redaction pattern research — amendment of 2026-08-31

> **Superseded in one part.** §5 defers "a prose *username* pattern"; qanungo#17 found the
> spaced-prose half of §1's own string still readable and reversed that deferral.
> [`redaction-patterns-2026-09-04.md`](redaction-patterns-2026-09-04.md) is the amendment that adds
> `prose-paired-username`, and `PATTERN_REVISION` is now `2026-09-04`. Everything else below stands.

Input for qanungo#15 (two patterns whose evidence is not a separator). This file is an
**amendment**, not a replacement: [`redaction-patterns-2026-08-24.md`](redaction-patterns-2026-08-24.md)
remains the provenance of every pattern it describes, of §0's standing trade, and of §4's profanity
list, none of which changed. Read that file first; this one records only what `PATTERN_REVISION`
moved for.

`PATTERN_REVISION` is now `2026-08-31`. It moved because the set **gained two ids** — documents
rendered before and after are no longer claiming the same scrub, which is the whole reason the
constant is stamped in a footer.

## 1. What was found

The `qanungo doctor` V1 review (2026-08-30) rendered a sighting of this shape — a repeated
cross-session instruction, which is what that lane looks for — as (every credential value in this
file is fabricated):

> `use http://… username : feedface00 and password c0ffeec0ffee for xtream. and
> http://…/get.php?username=feedface00&password=[REDACTED:secret-assignment]&type=m3u_plus`

The same credential pair is written twice in one string, and **only the query string's password was
scrubbed**. The doctor's excerpt path was proved correct in review — the whole string went through
the redactor once, before clipping, which is exactly why the fourth credential in it fired. The gap
was in the pattern set:

- `secret-assignment` gates on a `:` or `=` binding a key to its value (`redaction.rs`, "cheap
  structural gate before the key is normalized at all"). Prose `password c0ffeec0ffee` has no
  separator, so the pattern never reached its key test.
- `username` is deliberately absent from `SECRET_KEY_WORDS` — a username is not a secret. Here it
  was half of a live login whose other half had just been redacted, which is the one case where
  scrubbing the password alone leaves the reader holding something usable.

Filed as qanungo#15 with two recommendations, both implemented below.

## 2. `prose-credential`

A credential **noun**, one space, and a value shaped like a credential — with no separator at all.

| Element | Rule | Why it is safe |
|---|---|---|
| noun | `password` `passwords` `passwd` `passphrase` `pass` `pwd` `token` `secret` `apikey`, whole word, case-insensitive, optionally in a matching code span or quote pair | `key` is absent: it is an ordinary English word (*the key insight*). `username` is absent for the same reason it is absent from `SECRET_KEY_WORDS`. `api key` and `secret key` are not matched as phrases — a named recall cost, below. `pass` is the riskiest entry and was added on evidence; see below. |
| gap | **exactly one space**, optionally followed by one copula (`is`, `was`) and one more space | A sentence puts one space between two words. Several spaces or a tab is a *column*, and the first archive scan of this pattern found it reading one. |
| value | base62 plus `-` and `_`, optionally wrapped in `` ` ``, `"`, or `'`; **≥ 12 characters**; **at least one digit and at least one letter**; **not SHOUTING_SNAKE_CASE** | This is the entire guard, because the left side is only a noun and a space — the same grammar as *password manager*, *token stream*, *token count*, *the password is stored in the keychain*. |

**On `pass`, and on wrappers.** Both came out of the independent review's eyeball pass over the
first cut, which found the *same credential pair* written a second way and still readable:

```
- user `feedface00` · pass `c0ffeec0ffee`
```

`pass` was not a noun, and a backtick is neither a space nor a value byte, so the spelling was
invisible twice over. It was latent rather than live — it appeared on none of the four rendered
surfaces at the time — and it was one doctor-cluster membership away from rendering, which is not a
distinction worth keeping a known gap for.

`pass` is the riskiest word in the list (*a boarding pass*, *pass the build*, *pass-through*,
*the tests pass*), and it is carried on the same evidence as everything else here: over 933,292
records it fires only where a credential is, and its ordinary senses fire nothing. If a later scan
finds it eating prose, it goes, and the spelling moves to the recall costs.

**A wrapper is a boundary, never evidence.** The value inside a code span still has to pass every
test a bare one does — unlike `secret_assignment`'s quoted form, where a separator has already
vouched for the key and the quotes are structure on top of that. Here there is no separator, so the
wrapper buys nothing; it is accepted only so that the noun and the value can be adjacent through it.
A wrapped noun must close in the delimiter it opened with, and a wrapped value must close in the
value charset or **the match is refused whole** — the same never-a-partial-redaction invariant as
`quoted_value_end`.

**On SHOUTING_SNAKE_CASE.** A value carrying an `_` and no lowercase letter at all is a *name*: the
naming convention of an environment variable or a CI secret. The wrapped form found this on the
archive immediately —

```
6. Paste into GitHub secret `DEVELOPER_ID_CERTIFICATE_BASE64`
```

— and redacting it does not merely cost precision, it destroys the runbook step whose subject the
name is. Credentials are base62 or hex and carry lowercase; this is the argument `aws-access-key-id`
already makes when it refuses a longer `SHOUTING_IDENTIFIER` run.

**On the charset.** Narrower than a bare assignment's (`is_bare_value_byte`), which also admits
`. + / =`. Those are what turn a value into a file path, a version, a hostname, or the first word of
the next sentence, and in prose there is no separator standing behind them as evidence.

**On the floor.** Twelve is the length of the sighting (shown here as the fabricated
`c0ffeec0ffee`, twelve hex characters). It is also above every English word carrying a digit that
could plausibly follow one of these nouns: `sha256`, `base64url`, `argon2id`, `oauth2`, `utf8`,
`pbkdf2`, `md5`.

**On the digit-and-letter test.** All-letters is an English word however long it is; all-digits is a
measurement, and this archive writes `"input_tokens": 61184` tens of thousands of times — `token
61184` is the same number with its punctuation dropped.

**Two stand-downs, both about *partial* redaction rather than about prose.** Neither is optional:
partial redaction — a marker claiming a value was scrubbed with half of it still on the screen — is
the worst outcome this module has, and this is the least specific pattern in the set.

1. A value with a **dotted word tail** (`abc123def456.xyz`) is refused outright. The prose charset
   stops at the dot, so matching would take the first segment of somebody else's token.
2. A value that one of the **vendor matchers** recognizes (`github-token`, `anthropic-key`,
   `openai-key`, `aws-access-key-id`, `slack-token`, `gitlab-token`, `npm-token`, `google-api-key`,
   `jwt`) is left to that matcher, which knows its real length. `token eyJ….eyJ….SIG` reports as a
   `jwt` and goes whole; `token ghp_…` reports as a `github-token`.

**Recall costs, named:**

- An **all-letter prose passphrase** (`password correcthorsebatterystaple`) is not matched. The
  assignment form of it still is — a bare assignment has its separator to lean on, and §3 of the
  2026-08-24 file already bought that exception with the 20-character alphabetic floor. Prose has
  nothing to lean on.
- **Two-word nouns** (`api key c0ffeec0ffee`, `secret key …`) are not matched. A second word is a
  second place for prose to get in, and on this archive `secret` and `token` as single words fire
  zero times, which is not an argument for adding more surface.
- A value **under twelve characters**, or carrying no digit, is not matched however clearly the
  sentence means it as a credential.
- **Separators that are neither a space nor an assignment**: `password - c0ffeec0ffee`,
  `password, c0ffeec0ffee`, `password → c0ffeec0ffee`. A dash or a comma between the noun and the
  value is a *list*, and a list of nouns (`password, username, and host are all in the config`) is
  what that punctuation is overwhelmingly doing in a transcript. The review endorsed leaving these
  out; they are recorded here rather than fixed.
- **A non-breaking space** (U+00A0) or any other Unicode space between the noun and the value.
  `single_space` is ASCII `0x20` only, deliberately: the byte classes in this module are ASCII
  throughout, and a multi-byte space would be the first place a byte-offset mistake could reach a
  rendering path. Text pasted out of a word processor or a rendered web page is where this loses a
  match.
- A value **whose case is uniform and which carries an `_`** — see SHOUTING_SNAKE_CASE above. A real
  all-caps credential with an underscore in it is not matched.

## 3. `paired-username`

The `username=` of a URL whose `password=` fired. Its own id rather than `secret-assignment`,
because its evidence is different in kind: not a key that names a credential, but **adjacency to
one**. `username=[REDACTED:secret-assignment]` would tell the reader something untrue about why the
value went.

Every clause is structural, and all four must hold:

1. The name is `username` or `user`, and the byte before it is `?` or `&` — a **query parameter**,
   not a word in a sentence.
2. It is inside a URL: the run of URL bytes reaching back from it contains `://`. A bare
   `?username=x&password=y` in a shell line or a struct literal is not this. The run is bounded at
   2048 bytes **on both sides** — review found the forward half unbounded while this line claimed
   otherwise, and the code was fixed to match rather than the sentence, because a record with no
   whitespace in it would otherwise walk forward to the end of a two-megabyte line.
3. Somewhere in that same run — before or after, order is not evidence — there is a parameter whose
   normalized name **ends with** `password`, `passwd`, or `passphrase`.
4. **`secret_assignment` accepts that parameter**, by call rather than by re-implementation. This is
   what keeps the pair honest: `?username=x&password=` with an empty value, a bare number, or an
   expression scrubs nothing, so there is no live half, so the username stays.

Narrower than `SECRET_KEY_WORDS` on purpose: a `token=` or `session=` beside a `user=` is what half
the URLs in any archive look like, and a session id does not make a username sensitive. It is a
*password* that makes it the other half of a login.

**Prose usernames are not touched**, and that is the design rather than an omission. The sighting's
string keeps its `username : feedface00` in the sentence and loses only the query parameter. A
report that redacts the reader's own login name everywhere it appears has stopped being a report.

## 4. Measured on the archive

Same harness as 2026-08-24 — `cargo test --release --test redaction_scan -- --ignored --nocapture`,
counts only, the scan cannot report anything else. The mirror has grown since: **1,688 blobs,
933,292 records, 3.64 GiB**, so the columns below are not comparable with §3a of the 2026-08-24
file, only with each other.

Four rounds, two of them forced by what the scan showed. Rounds 1–2 are the first cut; rounds 3–4
are the review's `pass`-and-wrappers finding and the over-match it turned up in its turn.

| id | before (`2026-08-24` set) | 1: first cut | 2: gap tightened | 3: `pass` + wrappers | 4: names refused |
|---|---:|---:|---:|---:|---:|
| `secret-assignment` | 1,372 | 1,372 | 1,372 | 1,372 | 1,372 |
| `url-credentials` | 143 | 143 | 143 | 143 | 143 |
| `github-token` | 62 | 62 | 62 | 62 | 62 |
| `authorization-header` | 36 | 36 | 36 | 36 | 36 |
| `jwt` | 32 | 32 | 32 | 32 | 32 |
| `google-api-key` | 14 | 14 | 14 | 14 | 14 |
| `anthropic-key` | 10 | 10 | 10 | 10 | 10 |
| `aws-access-key-id` | 10 | 10 | 10 | 10 | 10 |
| `private-key-block` | 8 | 8 | 8 | 8 | 8 |
| `prose-credential` | — | 93 | 91 | 147 | **143** |
| `paired-username` | — | 48 | 48 | 48 | **48** |
| **total** | **1,687** | 1,828 | 1,826 | 1,882 | **1,878** |
| blobs firing anything | 89 / 1,688 | 96 / 1,688 | 95 / 1,688 | 96 / 1,688 | **96 / 1,688** |

**No existing pattern moved by a single hit, in any round.** That is the check that matters for a
pattern added to a longest-match scanner: `prose-credential` anchors on the noun, *before* the value
a vendor pattern would have anchored on, so an unguarded version of it would have quietly taken hits
away from `github-token` and `jwt`. The stand-down in §2 is why the first nine rows are identical
five times over.

### The two over-match rounds, and what they were

Inspecting the new fires (from the **scrubbed** text, so the values were already markers before
anything was read), each cut had exactly one fire class that was not a credential.

**Round 1 → 2, a deploy log's aligned table:**

```
ok       mosquitto-passwd     deadbeef0badf00d  -> /run/example-broker/mosquitto.conf.d/passwd
```

A resource name that happens to end in `passwd`, and a sixteen-hex **digest** in the next column. A
digest is the thing this module refuses hardest to redact — §0 of the 2026-08-24 file names it
first. The fix was structural rather than a length or charset tweak: a sentence puts **one space**
between two words, and a run of spaces or a tab is a column rule. 93 → 91, nothing else lost.

**Round 3 → 4, a CI secret's name:**

```
6. Paste into GitHub secret `DEVELOPER_ID_CERTIFICATE_BASE64`
```

Accepting wrapped values immediately reached the one thing a code span after the word *secret* most
often holds in a runbook: the secret's **name**. The fix is the SHOUTING_SNAKE_CASE test in §2 —
an `_` with no lowercase anywhere is a naming convention, not key material. 147 → 143, and the four
fires it removed were all this one sentence in four archived copies of the same runbook.

### What the surviving fires are

- **`prose-credential`, all 143**: two real classes. The IPTV credential pair the issue was filed
  from, written across dozens of sessions in every spelling a person reaches for — `… username
  feedface00 password …`, `--es password …`, `--user … --pass …`, `` user `…` · pass `…` ``,
  `` Username `…`, Password `…` ``, `Type password …` in an adb command — and this project's own
  working notes and issue text *quoting* that pair while describing the gap. Plus three fires on
  ``plaintext secret `secrettoken123` ``, an app's export round-trip check, where the value is
  exactly what the sentence says it is.
- **`paired-username`, all 48**: every one is an Xtream API URL,
  `…/player_api.php?username=…&password=…` or `…/get.php?…`, i.e. the same live pair in its query
  form.
- **`token`, `pwd`, `passphrase`, `apikey` fire zero times** across 933,292 records, and `secret`
  fires only on that one real value. The nouns most likely to appear in ordinary transcript prose
  contribute nothing, which is the value-shape gate doing the work it was built for. They are held
  to fixture canaries and to the noun × value × gap × wrapper grid in
  `the_prose_gate_is_the_noun_list_crossed_with_the_value_shape`.
- **`pass` earns its place and nothing more.** Every one of its fires is the credential pair;
  *boarding pass*, *pass the build*, *pass-through*, and *the tests pass* fire nothing, on the
  archive and in the fixtures both.

### What it did to the rendered lanes

Verified by running both binaries against the same cache:

- `report` and `cost` — **byte-identical** but for their own timestamps and timing instrumentation.
  Neither renders transcript content, so neither could have moved, and neither did.
- `standup` — body byte-identical; only the footer's revision stamp moved. Nothing matched in the
  window either way.
- `doctor` — exactly one line of one document changed, which is the line the issue was filed about,
  and the footer tally went from `secret-assignment×1` to `secret-assignment×1,
  prose-credential×1, paired-username×1`.

## 5. Still deferred

Everything in §5 of the 2026-08-24 file, unchanged — entropy scoring above all. Neither pattern here
looks at how random anything is; one is a noun and a value shape, the other is a URL and a
neighbour.

Added to that list:

- **A prose *username* pattern.** A username on its own is not a credential, and `username` is not
  going into any noun list. The pair is what is live, and §3 catches the pair in the only form where
  the two halves are provably in the same object.
- **Two-word credential nouns** (`api key`, `secret key`, `access token`). One row each when the
  archive shows one.
- **Separator forms between a noun and a bare value** — `password - c0ffeec0ffee`,
  `password, c0ffeec0ffee`, `password → …`. Punctuation between the two is far more often a list of
  field *names* than a field and its value, and the review endorsed leaving it. Named in §2's recall
  costs so that the next reader knows it was a decision.
- **Non-ASCII whitespace** (U+00A0 and its relatives) as the gap. This module's byte classes are
  ASCII end to end, and widening the gap test is the one change most likely to put a multi-byte
  offset into a rendering path. Also §2.

## Sources

- qanungo#15, and the `qanungo doctor` V1 review of 2026-08-30 that filed it — the string in §1
  (values fabricated), and both recommendations.
- The independent review of this change (2026-08-31, SHIP WITH FIXES) — the second spelling of the
  same credential (`pass`, and values in code spans) that rounds 3–4 were built from, and the
  `MAX_URL_SCAN_BYTES` doc/behaviour mismatch in §3 clause 2.
- `docs/redaction-patterns-2026-08-24.md` — everything this file does not restate.
- The local mirror, scanned 2026-08-31: 1,688 blobs, 933,292 records, 3.64 GiB, reproducible with
  `cargo test --release --test redaction_scan -- --ignored --nocapture`.
