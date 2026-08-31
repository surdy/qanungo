# Redaction pattern research — retrieved 2026-08-24

Input for qanungo#8 (the redaction layer). Every token shape sourced; every deliberate gap listed.
Do not widen a pattern without a source.

> **Amended 2026-08-31** by [`redaction-patterns-2026-08-31.md`](redaction-patterns-2026-08-31.md),
> which adds `prose-credential` and `paired-username` for qanungo#15 and moves `PATTERN_REVISION` to
> that date. Nothing below changed — no pattern here matches differently — and this file remains the
> provenance of every id it describes. Read both.

**This file is the provenance of `crates/qanungo/src/redaction.rs`.** Every prefix, length, and
charset in that module came from here and from nowhere else, and each pattern's doc comment names
the row below it came from. It is committed as it was researched so that a `[REDACTED:…]` marker in
a report can be traced to a vendor's documented token format rather than to somebody's memory of
one. Adding, widening, or retiring a pattern means amending this file **and** moving
`PATTERN_REVISION`, which is stamped in the footer of any document that renders scrubbed content —
exactly as `PRICE_TABLE_REVISION` is for dollars.

## 0. The standing trade: precision over recall

A redactor's two failure modes are not symmetric here.

- A **false negative** leaves a credential on a screen. The credential is on a screen that is
  already showing a transcript from a private archive, over a private network, to its owner.
- A **false positive** replaces ordinary prose with `[REDACTED:…]`. It happens in a *coaching
  report* — a document whose whole value is that a person reads it and believes it. A report
  pockmarked with markers where the text said something ordinary is a report nobody reads twice,
  and a control nobody trusts is a control that gets turned off.

So: **every pattern anchors on structure — a vendor prefix, a length class, a charset, a
separator, a key name — and never on entropy.** Entropy scoring is explicitly deferred. It is the
mechanism that turns a sha256 in a commit message, a base64-encoded image, a UUID, and a Git object
id into redactions, and this archive is *full* of all four. If it is ever wanted, it belongs behind
its own flag with its own measurement, not folded into these patterns.

Where a pattern gives up recall to buy precision, the price is named in its row.

## 1. Vendor-prefixed tokens

Prefix, length class, and charset are all documented by the issuer, which is the point of a
prefixed token: GitHub introduced the `gh*_` prefixes precisely so that scanners could find them
without guessing. Shapes below are cross-checked against the public gitleaks ruleset, which is the
de-facto community reference for these regexes.

| id | Matches | Floor in code | Anchor, and why it is safe |
|---|---|---|---|
| `github-token` | `ghp_` `gho_` `ghu_` `ghs_` `ghr_` + body; `github_pat_` + body | 36 / 40 chars | gitleaks uses `ghp_[0-9a-zA-Z]{36}` and `github_pat_\w{82}`. The floors here are *at or below* the documented exact lengths, so a length change upstream degrades to still-matching rather than to silently-not. `ghp_short` in a sentence about prefixes does not match. |
| `anthropic-key` | `sk-ant-` + body | 16 chars | gitleaks: `sk-ant-api03-[a-zA-Z0-9_\-]{93}AA`. The floor is far below 93 on purpose — the `sk-ant-` prefix is unambiguous (no English word, no identifier convention produces it), so the floor only has to refuse a document *talking about* the prefix. |
| `openai-key` | `sk-` not followed by `ant-`, + body | 20 chars **and** at least one uppercase **and** at least one digit | `sk-` alone is a weak anchor: `sk-forward-compatibility-shim` is an ordinary kebab-case identifier of the right charset and length. The discriminator is **charset class, not entropy** — OpenAI keys are base62, so they carry both an uppercase letter and a digit; kebab-case prose carries neither. |
| `aws-access-key-id` | `AKIA` / `ASIA` + exactly 16 `[A-Z0-9]` | exactly 16 | gitleaks: `(A3T[A-Z0-9]|AKIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASIA)[A-Z0-9]{16}`. Only the two *credential* prefixes are taken: `AKIA` (long-lived key id) and `ASIA` (STS temporary). The rest identify users, groups, and roles — they are not secrets, and redacting an IAM role id out of a coaching report would be a false positive with no upside. **Exactly** 16, so a longer `SHOUTING_IDENTIFIER` starting with those four letters is not eaten. |
| `slack-token` | `xoxb-` `xoxa-` `xoxp-` `xoxr-` `xoxs-` `xoxe-` `xapp-` + body | 10 chars | gitleaks matches on `xox[baprs]-`, and carries `xoxe-` (refresh) and `xapp-` (app-level) as current formats alongside it. Lowercase only — the prefixes are issued lowercase. **Spelled as literals, not as `xox` plus a letter class.** The class was how an unsourced `o` got in, and `xoxo-mom-and-dad-love-you` was redacting as a bot token; a character class is a widening nobody has to notice, and §0's rule is that a pattern is never widened without a source. `xoxe-` and `xapp-` fire zero times on this archive — like the AWS, npm, and Anthropic shapes, they carry fixture canaries rather than a production sighting. |
| `gitlab-token` | `glpat-` + body | 20 chars | Classic GitLab PATs are 20 base64url characters. GitLab's newer *routable* tokens are longer and carry a dotted checksum suffix (`glpat-<27..300>.<2><7>`); the secret material is matched, the trailing routing suffix is left visible, which is correct — it is a checksum, not key material. |
| `npm-token` | `npm_` + body | 36 chars | npm automation/access tokens are `npm_` plus 36 base62 characters. |
| `google-api-key` | `AIza` + body | 35 chars | Google API keys are `AIza` plus 35 base64url characters — a fixed 39-character total, which is why this pattern needs no other guard. |

### The GitHub stateless installation token

GitHub began a staged rollout of a **stateless** installation-token format, `ghs_<app id>_<JWT>`,
on 2026-04-27, warning integrators that installation tokens are no longer exactly 40 characters
(docs.github.com, "About authentication to GitHub", retrieved 2026-08-24). A JWT is dot-separated,
and a run of word characters stops dead at the first dot — which would have replaced the `ghs_`
prefix and the app id and left the payload and signature on the screen. The pattern therefore
extends through a dot-separated base64url tail, and measures its length floor over the tail as
well, because the stateless format is only twenty-odd characters before its first dot.

Two guards on that extension, both review findings: it runs for **`ghs_` alone**, because that is
the only prefix GitHub gave a dotted format to, and it requires the **two** dot segments a JWT
actually has (`x.y.z`, with the header already swallowed by the word run). Applied to all five
prefixes with a one-segment threshold, it ate ordinary sentences — `ghp_…456.Then restart` lost its
`.Then`.

## 2. Structural patterns (no vendor prefix)

| id | Matches | Anchor, and why it is safe |
|---|---|---|
| `jwt` | three base64url segments, the first starting `eyJ` | `eyJ` is base64 for `{"`, so **every** base64-encoded JSON document opens with it — the prefix alone is not evidence. The **two dots** are. The signature segment may be empty (`alg: none` tokens are still tokens); the second dot is what has already established the shape. |
| `private-key-block` | `-----BEGIN … PRIVATE KEY-----` through the matching `-----END … -----` | The only multi-line pattern, and the only one that redacts a span rather than a token. `PRIVATE KEY-----` must appear on the same line as `-----BEGIN`, within 40 bytes, which is what keeps `-----BEGIN CERTIFICATE-----` (a public object) out. A **truncated** block — a transcript cut mid-paste — is redacted to the end of the text: a private key missing its footer is still a private key. |
| `authorization-header` | the credential after `Authorization: Bearer|Basic|token` | Redacts the credential only, keeping the header name and the scheme word, because `Authorization: Bearer [REDACTED:authorization-header]` tells a reader what happened and a bare marker does not. Accepts the JSON spelling (`"Authorization": "Bearer …"`) as well as the wire one. `token` joins `Bearer` and `Basic` because that is the scheme GitHub's own API documentation uses. `authorization is a topic, not a header` does not match — there is no `:`. |
| `url-credentials` | the `user:pass` of `scheme://user:pass@host` | **The colon is required.** Matching any userinfo would redact `ssh://git@github.com/surdy/munshi.git`, which is in this repository's own `Cargo.toml`. The *whole* userinfo goes, not just the password: `https://oauth2:x@…` and `https://ghp_…@…` both put the credential in the part a naive reading calls the username. |

### The word after the scheme is not automatically a credential

Review caught `authorization-header` reading three ordinary sentences as headers: *Set the
Authorization: Bearer **token** header on every request*, *Authorization: Bearer **&lt;token&gt;** is the
required format*, and *authorization: basic **understanding** of the protocol helps*. Prose is
exactly what follows a colon in a coaching report, so the credential has to be shaped like one:
**at least 16 characters**, and carrying **a digit or one of `= + /`**. Every real credential is
base62, base64, or a JWT and satisfies both; `token`, `<token>`, and `understanding` satisfy
neither. Angle brackets also left the credential charset, so a `<placeholder>` cannot be a match at
all. Sixteen is the base64 length of the shortest `Basic` pair anyone really uses — `admin:admin`
encodes to exactly that.

**Recall cost, named:** a short all-letter base64 `Basic` credential (`dXNlcjpwYXNz`) is not
matched. Twelve characters of no digits and no padding is indistinguishable from the next word of
a sentence, and this pattern will not eat sentences to catch it.

On the archive this was the difference between **224 hits and 24** — 200 of the original hits were
prose.

## 3. The generic assignment — and what the archive did to it

`secret-assignment` (and its AWS specialization `aws-secret-key`) is the weakest anchor in the set,
because it is the only one whose evidence is a *name* rather than a shape. It is also the one that
had to be rewritten after measurement.

**qanungo#8 asks for** `(?i)(token|secret|password|passwd|api_key|apikey|credential|private_key)`
as the key test, matched as a substring. That is what shipped into the first scan.

**Measured on the local mirror, 2026-08-24** — 723 blobs, 681,684 records, 3.15 GiB of real
transcript — a substring match on the key fired **17,302 times**, 98% of all hits, across 402
distinct key names and 282 of the 723 sessions. Inspecting the *keys* (never the values), the
firing names were overwhelmingly **source code**, because a transcript is mostly source code:

```
1453  tokenType          869  tokenState        597  resume_token
 381  preview_token      363  tokens            355  secrets
 283  credential         278  secretStore       246  control_token
 216  onBasicPasswordChange   202  check-theme-tokens.sh   134  onBearerTokenChange
```

Two tightenings followed. Both are structural, and neither is entropy.

1. **The key must *end* in a credential word, not merely contain one.** In every naming style there
   is, the thing a name denotes goes last: `resume_token` holds a token, `token_grant` holds a
   grant, `tokenType` holds a type. Because normalization strips `_`, `-`, and `.`, the word list
   carries the multi-word names too — `AWS_SECRET_ACCESS_KEY` normalizes to `awssecretaccesskey`
   and ends with `secretaccesskey`, not with `secret`. The list is `token`, `secret`, `password`,
   `passwd`, `passphrase`, `apikey`, `credential`, `privatekey`, `secretkey`, `secretaccesskey`,
   `accesskey`, `authkey`.
   **Recall cost, named:** a key that buries its noun (`GITHUB_TOKEN_FOR_CI`) is not matched *as an
   assignment*. A real value under such a key is usually still caught by its own vendor pattern.
2. **A bare (unquoted) value must be spellable in the credential charset and carry a digit.** The
   charset is an allow-list — base62 plus `- _ . + / =` — rather than a deny-list, and that is the
   whole point: a deny-list admitted `self.next_token(`, `Option<String>`, and `crate::Token`. The
   digit requirement is the same argument one level down: credentials are base62, base64url, or
   hex, and all three carry digits, while `token = self.next_token` does not.
   Two exceptions are kept: a bare value of ≥20 characters that is entirely alphabetic is a
   passphrase, not an expression (an expression that long would have punctuation in it); and a
   **quoted** value skips both guards entirely, because the quotes are themselves the structure —
   `"password": "letmein"` is data, not a sentence.
   **Recall cost, named:** a bare, digitless, short password (`password: letmein`) is not redacted.
   Quoting it brings it back.

### A quoted value is never *partially* redacted

The naive quoted-value scan stopped at the first `"`, escaped or not. Review's repro:
`{"password": "abc\"def123XYZ"}` matched `abc\` and left `def123XYZ` sitting beside a marker
claiming the value had been scrubbed. That is the worst outcome this module has — a visible secret
wearing the reassurance of a redaction — and it is not an edge case, because munshi transcripts are
JSONL and an escaped quote inside a value is simply how JSON writes one.

The scan now skips `\`-escaped bytes, so the value ends at its *real* closing quote. When there is
no real closing quote — the value runs past a newline, past 4096 characters, or past the end of the
text — the pattern **refuses to match at all** rather than matching short. Under-claiming leaves
the bare charset to decide; over-claiming is the failure that matters.

Also refused before either test runs: an empty value, a bare value under 6 characters, a bare value
that is entirely digits (this archive records `"input_tokens": 61184` tens of thousands of times),
a value opening a bracket or brace, and `==`, which is a comparison rather than an assignment.

**After the tightening, the same scan fires 1,231 times across 32 distinct keys** — and the
surviving key names are what a credential assignment actually looks like: `cloudflare_api_token`,
`tunnel_token`, `ts_authkey`, `github_token`, `immich_api_key`, `secret_key`, `access_token`,
`mcp_access_key`, `refreshToken`.

## 3a. Where the whole set stands on the archive

Same corpus each time — 723 blobs, 681,684 records, 3.15 GiB — scanned before review, and again
after the four review fixes. Counts only; the scan cannot report anything else.

| id | first run | after §3 tightening | after review fixes |
|---|---:|---:|---:|
| `secret-assignment` | 17,302 | 1,231 | 1,231 |
| `authorization-header` | 224 | 224 | **24** |
| `url-credentials` | 67 | 67 | 67 |
| `google-api-key` | 14 | 14 | 14 |
| `jwt` | 11 | 11 | 11 |
| `private-key-block` | 6 | 6 | 6 |
| `github-token` | 4 | 4 | 4 |
| **total** | **17,628** | **1,557** | **1,357** |
| sessions firing anything | 282 / 723 | 88 / 723 | **73 / 723** |

`anthropic-key`, `openai-key`, `aws-access-key-id`, `aws-secret-key`, `slack-token`, `gitlab-token`,
and `npm-token` fire zero times on this archive. They are held to the fixture canaries instead,
which is the whole reason every pattern class has one.

`PATTERN_REVISION` stays `2026-08-24`. The set's semantics did change under review — four patterns
now match differently — but the revision is a *date*, the changes landed on the same day as the
research, and nothing has been published under the old reading. A revision that moved would be
claiming two incomparable pattern sets where there has only ever been one released.

## 4. Profanity

A short **enumerated** list of full forms (26 entries), matched as whole words, case-insensitively,
and never as substrings. Enumerated rather than stemmed on purpose: a rule that accepted
`<entry> + s|ed|ing` would decide what it masks by arithmetic nobody reviewed, and the value of a
list is that a person can read all of it.

- **Whole-word matching is the requirement, not an optimization.** The classic failure of this
  feature is the Scunthorpe problem, and its relatives here are `class`, `assassin`, `assessment`,
  `bassline`, and `shiitake`. A word abutting a digit or an underscore is an identifier fragment
  rather than a word, so `FUCKS_GIVEN_COUNT` and `pissed_off_flag` are left alone.
- **`dick` and `prick` are deliberately absent.** One is a name and the other has an ordinary
  sense; a coaching report that masks a colleague's name has done more damage than the word it
  caught.
- **Replacement is a mask, not a marker**: first character kept, the rest starred, length
  preserved (`Fuck` → `F***`). Enough for a reader to see that something was masked without
  reading it, and not re-matchable, so the pass stays idempotent.
- **Counted under one id.** A per-word count would be the matched text spelled as a histogram, and
  the report is sworn not to carry it.

Default **off**, and that is a tunable rather than a decision — see
`FILTER_PROFANITY_BY_DEFAULT`. This archive is one person's own transcripts read by that person;
masking their own words back at them is noise. A shared dashboard is a different audience.

## 5. Deferred — deliberately not built

- **Entropy scoring.** §0. If wanted, its own flag and its own measurement.
- **Structured redaction.** The scrub is over text, not over a parsed record. A surface that wants
  to drop a whole field should drop the field.
- **Azure, Stripe, Twilio, SendGrid, Datadog, HashiCorp, PyPI, Docker Hub, SSH public keys,
  `.netrc`, `pgpass`.** No shortage of documented shapes; none of them occurs in this archive, and
  a pattern nobody has ever exercised is a pattern nobody has ever tested. They are one row each
  when a reason appears.
- **Pre-2026 GitLab and Slack legacy formats.** Not authoritatively archived; not guessed.
- **A denylist of the operator's own known secrets.** That would be a store of secrets, which is
  the thing this module exists to avoid creating.

## 6. What the layer is *not* responsible for

- **Cache permissions.** qanungo#8 also asks for `0o600`/`0o700` on the blob cache and any derived
  store. That was already true and already tested before this lane — `crates/qanungo/src/cache.rs`
  creates every directory `0o700` and every blob `0o600` and never widens them, and
  `directories_are_0700_and_blobs_are_0600` pins it. Nothing was added here for it.
- **`report` and `cost`.** Neither renders transcript content — aggregates, tool names, and
  `source_hash` references only, with archive-stated identifiers clamped through
  `format::identifier`, and canary fixtures proving it. The redactor is deliberately **not** wired
  into them: a filter over a document that carries no content can only be decoration, and
  decoration in a security control invites the reader to trust it.

## Sources

- https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/about-authentication-to-github
  (official — the `gh*_` prefix table and the 2026-04-27 stateless `ghs_APPID_JWT` rollout)
- https://github.com/gitleaks/gitleaks/blob/master/config/gitleaks.toml (community reference ruleset —
  prefix, length, and charset for GitHub, Anthropic, OpenAI, AWS, Slack, GitLab, npm, Google)
- https://github.com/gitleaks/gitleaks/blob/master/cmd/generate/config/rules/github.go (the GitHub
  rules as generated, including the `{36}` and `\w{82}` lengths)
- https://github.com/gitleaks/gitleaks/issues/1655 (GitLab routable-token format and its dotted
  checksum suffix)
- RFC 3986 §3.2.1 (URL userinfo), RFC 7519 §3 (JWT's three dot-separated segments), RFC 7617 and
  RFC 6750 (`Authorization: Basic` and `Bearer`)
- https://api.slack.com/authentication/token-types (official — `xoxb`/`xoxp`/`xoxe` and the
  app-level `xapp` prefix)
- The local mirror itself, scanned 2026-08-24: 723 blobs, 681,684 records, 3.15 GiB — the
  measurements in §3 and §3a, reproducible with
  `cargo test --release --test redaction_scan -- --ignored --nocapture`.

All retrieved 2026-08-24.
