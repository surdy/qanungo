//! The redaction layer: what happens to transcript text on its way to a human surface.
//!
//! Qanungo renders no verbatim transcript content today. `report` and `cost` cite evidence by
//! `source_hash` and print aggregates, tool names, and archive-stated identifiers clamped through
//! [`crate::format::identifier`] — a construction property, not a filter, and one their own tests
//! pin with canary fixtures. This module exists because that is about to stop being true: the
//! standup lane (qanungo #9) renders munshi's summary prose, and every surface after it — the
//! dashboard's evidence view, an instructions doctor's quotes, a narrator's inputs — quotes text
//! somebody typed into a terminal. Qanungo #8 asks for the scrub to exist *before* the first
//! surface needs it, so that no lane ever ships "we'll add redaction after".
//!
//! Nothing here is wired into `report` or `cost`. Wiring it in would suggest their documents needed
//! filtering, and they do not: a filter over a document that carries no content can only ever be
//! decoration, and decoration in a security control is worse than nothing because it invites the
//! reader to trust it. This module is a library that the *next* lane flattens [`RedactionArgs`]
//! into and calls.
//!
//! # Two passes, independently switched
//!
//! - **Secrets, default on** ([`REDACT_SECRETS_BY_DEFAULT`]). Off is a deliberate, documented
//!   choice a person makes with `--no-redact`, never the default, because the failure mode is
//!   printing a live credential to a browser on the tailnet.
//! - **Profanity, default off** ([`FILTER_PROFANITY_BY_DEFAULT`]). A tunable, not a decision: the
//!   issue asks for it to exist and be switchable, and the default is off because this archive is
//!   one person's own working transcripts and masking their own swearing back at them is noise.
//!   A shared or published surface may well want it on, and flipping that constant is the whole
//!   change.
//!
//! The passes do not interact. Turning one on does not turn the other on, and the report says
//! which fired regardless of which was asked for.
//!
//! # Structure, not entropy
//!
//! Every secret pattern anchors on **structure** — a vendor prefix, a length class, a charset, a
//! separator, a key name — and never on how random a string looks. Entropy scoring is explicitly
//! deferred: it is the thing that turns a sha256 in a commit message, a base64 image, or a UUID
//! into a redaction, and a coaching report whose prose is pockmarked with `[REDACTED:…]` where the
//! text said something ordinary is a report nobody reads twice. **Precision over recall is the
//! standing trade here**, and each place it costs recall is named in
//! `docs/redaction-patterns-2026-08-24.md` beside the pattern that pays for it.
//!
//! Two guards make that concrete on the weakest anchor, the generic assignment
//! ([`PatternId::SecretAssignment`]): a *bare* value that is entirely digits is never a credential
//! (`"input_tokens": 61184` appears tens of thousands of times in this archive), and a bare value
//! that is entirely alphabetic and shorter than [`MIN_ALPHABETIC_SECRET_CHARS`] is prose
//! (`the token: a short string`). A *quoted* value skips both guards, because the quotes are
//! themselves the structure: `"password": "letmein"` is data, not a sentence.
//!
//! # Two patterns whose evidence is not a separator (qanungo #15)
//!
//! [`PatternId::ProseCredential`] and [`PatternId::PairedUsername`] were added at revision
//! `2026-08-31` from one real archived instruction, in which a credential pair was pasted twice —
//! once as prose and once as a query string — and only the query string was scrubbed:
//!
//! ```text
//! use http://…  username : feedface00 and password c0ffeec0ffee for … and
//! http://…/get.php?username=feedface00&password=[REDACTED:secret-assignment]&type=…
//! ```
//!
//! Both are anchored on structure like everything else here, but the structure is not a separator:
//!
//! - **Prose** anchors on a credential *noun* standing alone as a word, and then on the **shape of
//!   the value after it**. That shape is the whole guard, because `password manager`,
//!   `token stream`, and `the password is stored in the keychain` are the same grammar as
//!   `password c0ffeec0ffee`. Either side may sit in a code span — this archive writes that same
//!   pair as a table row too, ``user `feedface00` · pass `c0ffeec0ffee` `` — and a wrapper is taken
//!   as a boundary, never as evidence. See [`prose_credential`].
//! - **The paired username** anchors on a `username=` query parameter *in the same URL as a
//!   `password=` that fired on its own*. A username is not a secret; half of a live pair is, and
//!   only for as long as the other half is beside it. See [`paired_username`].
//!
//! # The report never carries what it matched
//!
//! [`RedactionReport`] holds counts per [`PatternId`] and nothing else — no offsets, no excerpts,
//! no matched text, not even in its `Debug`. A redactor whose report leaks the secret has
//! redacted nothing, and a report is exactly the thing that ends up in a footer, a log line, or a
//! panic message. Profanity is counted under one id for the same reason: a per-word count is the
//! matched text, spelled as a histogram.
//!
//! # Idempotence
//!
//! Scrubbing scrubbed text changes nothing. The scanner steps over any `[REDACTED:<id>]` marker it
//! meets rather than re-examining its inside, so a document that passes through two surfaces does
//! not grow nested markers, and the counts on a second pass are zero. That is a property a
//! rendering pipeline will rely on without asking.
//!
//! # What this module does *not* do
//!
//! - **The cache.** Qanungo #8 also asks for `0o600`/`0o700` on the blob cache and any derived
//!   store. That is already true and already tested — see [`crate::cache`], which creates every
//!   directory `0o700` and every blob `0o600` and never widens them. Nothing was added here for
//!   it.
//! - **Entropy scoring**, per above.
//! - **Structured redaction.** The scrub is over text, not over a parsed record. A surface that
//!   renders a field renders it through here; a surface that wants to drop a whole field should
//!   drop the field.

/// The revision of this pattern set, stamped by any document that renders scrubbed content.
///
/// The date of `docs/redaction-patterns-<revision>.md`, which is where every token shape below came
/// from — the same contract [`crate::pricing::PRICE_TABLE_REVISION`] carries for dollars. Two
/// documents claim the same scrub only when this matches: adding a pattern, widening one, or
/// retiring one moves the date and amends that file.
///
/// `2026-08-31` adds [`PatternId::ProseCredential`] and [`PatternId::PairedUsername`] for qanungo
/// #15; `docs/redaction-patterns-2026-08-31.md` is the amendment, and the `2026-08-24` file it
/// names remains the provenance of everything it does not restate.
pub const PATTERN_REVISION: &str = "2026-08-31";

/// Secrets are scrubbed unless a person says otherwise. Not a tunable.
pub const REDACT_SECRETS_BY_DEFAULT: bool = true;

/// Profanity is not masked unless asked for.
///
/// **A tunable choice, not a decision.** The archive is one person's own transcripts and this
/// surface is read by that person; masking their own words back at them is noise, not safety. A
/// shared dashboard or a published report is a different audience, and flipping this constant is
/// the whole of that change.
pub const FILTER_PROFANITY_BY_DEFAULT: bool = false;

/// Opens the marker a scrubbed secret is replaced by; closed with `]`.
pub const MARKER_OPEN: &str = "[REDACTED:";
/// Closes the marker.
pub const MARKER_CLOSE: char = ']';
/// Stands in for every character of a masked word but its first.
pub const PROFANITY_MASK: char = '*';

/// Longest `[REDACTED:<id>]` marker the idempotence skip will look for. Comfortably over the
/// longest [`PatternId::as_str`], and bounded so a lone `[REDACTED:` in ordinary text costs a
/// bounded scan rather than a walk to the end of the document.
const MAX_MARKER_CHARS: usize = 48;

/// Which pattern fired. The string form is what appears inside a marker, so these are part of the
/// rendered output and change only with [`PATTERN_REVISION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PatternId {
    /// `ghp_` / `gho_` / `ghu_` / `ghs_` / `ghr_` / `github_pat_`.
    GithubToken,
    /// `sk-ant-…`.
    AnthropicKey,
    /// `sk-…` that is not an Anthropic key.
    OpenAiKey,
    /// `AKIA…` / `ASIA…`.
    AwsAccessKeyId,
    /// An assignment whose key is an `aws_secret_access_key` spelling.
    AwsSecretKey,
    /// `xoxb-` / `xoxp-` / `xoxo-` / `xoxa-` / `xoxs-` / `xoxr-`.
    SlackToken,
    /// `glpat-…`.
    GitlabToken,
    /// `npm_…`.
    NpmToken,
    /// `AIza…`.
    GoogleApiKey,
    /// Three base64url segments, the first starting `eyJ`.
    Jwt,
    /// A whole `-----BEGIN … PRIVATE KEY-----` block.
    PrivateKeyBlock,
    /// The credential after `Authorization: Bearer|Basic|token`.
    AuthorizationHeader,
    /// The `user:pass` of a `scheme://user:pass@host` URL.
    UrlCredentials,
    /// A `KEY=VALUE` or `"key": "value"` whose key names a credential.
    SecretAssignment,
    /// A credential noun standing alone in prose, followed by a credential-shaped value and no
    /// separator at all: `password c0ffeec0ffee`.
    ProseCredential,
    /// The `username=` of a URL whose `password=` fired — half of a pair, sensitive only because
    /// the other half is beside it.
    PairedUsername,
    /// A masked word from the profanity list. Not a secret pattern; never appears in a marker.
    Profanity,
}

/// Every id, secrets and profanity alike, in report order.
pub const PATTERNS: [PatternId; 17] = [
    PatternId::GithubToken,
    PatternId::AnthropicKey,
    PatternId::OpenAiKey,
    PatternId::AwsAccessKeyId,
    PatternId::AwsSecretKey,
    PatternId::SlackToken,
    PatternId::GitlabToken,
    PatternId::NpmToken,
    PatternId::GoogleApiKey,
    PatternId::Jwt,
    PatternId::PrivateKeyBlock,
    PatternId::AuthorizationHeader,
    PatternId::UrlCredentials,
    PatternId::SecretAssignment,
    PatternId::ProseCredential,
    PatternId::PairedUsername,
    PatternId::Profanity,
];

/// The ids the secrets pass can produce — [`PATTERNS`] without [`PatternId::Profanity`].
pub const SECRET_PATTERNS: [PatternId; 16] = {
    let mut secrets = [PatternId::GithubToken; 16];
    let mut index = 0;
    while index < secrets.len() {
        secrets[index] = PATTERNS[index];
        index += 1;
    }
    secrets
};

impl PatternId {
    /// The id as it is written inside a marker and in a report.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GithubToken => "github-token",
            Self::AnthropicKey => "anthropic-key",
            Self::OpenAiKey => "openai-key",
            Self::AwsAccessKeyId => "aws-access-key-id",
            Self::AwsSecretKey => "aws-secret-key",
            Self::SlackToken => "slack-token",
            Self::GitlabToken => "gitlab-token",
            Self::NpmToken => "npm-token",
            Self::GoogleApiKey => "google-api-key",
            Self::Jwt => "jwt",
            Self::PrivateKeyBlock => "private-key-block",
            Self::AuthorizationHeader => "authorization-header",
            Self::UrlCredentials => "url-credentials",
            Self::SecretAssignment => "secret-assignment",
            Self::ProseCredential => "prose-credential",
            Self::PairedUsername => "paired-username",
            Self::Profanity => "profanity",
        }
    }

    /// Position in [`PATTERNS`], which is how [`RedactionReport`] indexes its counters.
    const fn index(self) -> usize {
        self as usize
    }
}

impl std::fmt::Display for PatternId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What a scrub fired, and nothing about what it matched.
///
/// Counts per [`PatternId`]. There is deliberately no way to ask this type for an offset, an
/// excerpt, or a matched string — including through `Debug`, which is the form these end up in
/// inside a log line or a panic. See the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RedactionReport {
    counts: [usize; PATTERNS.len()],
}

impl RedactionReport {
    /// How many times `pattern` fired.
    pub const fn count(&self, pattern: PatternId) -> usize {
        self.counts[pattern.index()]
    }

    /// How many replacements were made in total.
    pub fn total(&self) -> usize {
        self.counts.iter().sum()
    }

    /// Whether the scrub changed anything.
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// The patterns that fired, in [`PATTERNS`] order, with their counts.
    pub fn fired(&self) -> impl Iterator<Item = (PatternId, usize)> + '_ {
        PATTERNS
            .into_iter()
            .map(|pattern| (pattern, self.count(pattern)))
            .filter(|(_, count)| *count > 0)
    }

    /// Adds another report's counts into this one, for a caller scrubbing many documents.
    pub fn absorb(&mut self, other: &Self) {
        for (slot, count) in self.counts.iter_mut().zip(other.counts) {
            *slot += count;
        }
    }

    fn record(&mut self, pattern: PatternId) {
        self.counts[pattern.index()] += 1;
    }
}

/// Text that has been through a [`Redactor`], and the account of what that cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrubbed {
    /// The text as it may be rendered.
    pub text: String,
    /// What fired, by pattern.
    pub report: RedactionReport,
}

/// The scrub itself: two independently switched passes over a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Redactor {
    secrets: bool,
    profanity: bool,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor {
    /// A redactor at the shipped defaults: secrets on, profanity off.
    pub const fn new() -> Self {
        Self {
            secrets: REDACT_SECRETS_BY_DEFAULT,
            profanity: FILTER_PROFANITY_BY_DEFAULT,
        }
    }

    /// Turns the secrets pass on or off. Off is a person's documented choice.
    #[must_use]
    pub const fn with_secrets(mut self, on: bool) -> Self {
        self.secrets = on;
        self
    }

    /// Turns the profanity pass on or off.
    #[must_use]
    pub const fn with_profanity(mut self, on: bool) -> Self {
        self.profanity = on;
        self
    }

    /// Whether the secrets pass will run.
    pub const fn redacts_secrets(&self) -> bool {
        self.secrets
    }

    /// Whether the profanity pass will run.
    pub const fn filters_profanity(&self) -> bool {
        self.profanity
    }

    /// Scrubs `text`, returning it alongside a count of what fired.
    ///
    /// Secrets first, then profanity, so that a masked word can never split a token the secrets
    /// pass would otherwise have recognized. With both passes off this is a copy and an empty
    /// report — the honest rendering of "the operator asked for raw".
    pub fn scrub(&self, text: &str) -> Scrubbed {
        let mut report = RedactionReport::default();
        let scrubbed = if self.secrets {
            scrub_secrets(text, &mut report)
        } else {
            text.to_owned()
        };
        let scrubbed = if self.profanity {
            mask_profanity(&scrubbed, &mut report)
        } else {
            scrubbed
        };
        Scrubbed {
            text: scrubbed,
            report,
        }
    }

    /// Scrubs `text` and keeps only the text, for a call site that has nowhere to put a report.
    pub fn scrub_text(&self, text: &str) -> String {
        self.scrub(text).text
    }
}

// ---------------------------------------------------------------------------------------------
// The secrets pass
// ---------------------------------------------------------------------------------------------

/// One accepted match: the id to record, and the byte range to replace with its marker.
///
/// `start` may be later than the position the pattern anchored at — an assignment anchors on its
/// key and replaces only its value, so `api_key=` survives into the rendered text and the reader
/// can still see that a key was set.
#[derive(Debug, Clone, Copy)]
struct Hit {
    id: PatternId,
    start: usize,
    end: usize,
}

type Matcher = fn(&[u8], usize) -> Option<Hit>;

/// Every matcher, most specific first. Order is the tie-break when two matchers accept ranges
/// ending at the same byte; length wins first.
const MATCHERS: [Matcher; 15] = [
    github_token,
    anthropic_key,
    openai_key,
    aws_access_key_id,
    slack_token,
    gitlab_token,
    npm_token,
    google_api_key,
    jwt,
    private_key_block,
    authorization_header,
    url_credentials,
    secret_assignment,
    prose_credential,
    paired_username,
];

/// The matchers that recognize a credential by its *own* shape, with no surrounding evidence.
///
/// [`prose_credential`] consults these before claiming the value after a credential noun, and
/// stands down if one of them owns it. Two reasons, and the second is the load-bearing one:
/// `token ghp_…` should be reported as a `github-token` rather than as prose, and — because these
/// shapes run through dots and punctuation that the prose value charset stops at — a prose match
/// over `token eyJhbGci….eyJzdWIi….SIGNATURE` would replace the header and leave the payload and
/// the signature on the screen beside a marker. That partial redaction is the worst outcome this
/// module has, and deferring is how the least specific pattern in the set avoids causing it.
const VENDOR_MATCHERS: [Matcher; 9] = [
    github_token,
    anthropic_key,
    openai_key,
    aws_access_key_id,
    slack_token,
    gitlab_token,
    npm_token,
    google_api_key,
    jwt,
];

/// Bytes that can begin a match. A cheap reject so the scanner does not run thirteen matchers at
/// every space in a two-megabyte transcript.
static MAYBE_START: [bool; 256] = maybe_start_table();

const fn maybe_start_table() -> [bool; 256] {
    let mut table = [false; 256];
    let mut index = 0;
    while index < 256 {
        let byte = index as u8;
        table[index] = byte.is_ascii_alphanumeric()
            || byte == b'_'
            || byte == b'-'
            || byte == b'"'
            || byte == b'\'';
        index += 1;
    }
    table
}

fn scrub_secrets(text: &str, report: &mut RedactionReport) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        // A marker is not content: stepping over it is what makes a second scrub a no-op.
        if let Some(after) = marker_end(bytes, cursor) {
            cursor = after;
            continue;
        }
        if !MAYBE_START[usize::from(bytes[cursor])] {
            cursor += 1;
            continue;
        }
        let Some(hit) = best_match(bytes, cursor) else {
            cursor += 1;
            continue;
        };
        // Defensive rather than expected: every matcher anchors and terminates on ASCII, so both
        // ends are already character boundaries. Skipping rather than slicing keeps a future
        // pattern from turning a byte-offset mistake into a panic in a rendering path.
        //
        // Refusing a span that swallows a marker is what makes idempotence a property of the
        // scanner rather than of every matcher's byte classes: `Authorization: Bearer <marker>`
        // must not read the marker as this pass's credential and wrap it in a second one.
        if hit.end <= hit.start
            || !text.is_char_boundary(hit.start)
            || !text.is_char_boundary(hit.end)
            || find(&bytes[hit.start..hit.end], MARKER_OPEN.as_bytes()).is_some()
        {
            cursor += 1;
            continue;
        }
        out.push_str(&text[copied..hit.start]);
        out.push_str(MARKER_OPEN);
        out.push_str(hit.id.as_str());
        out.push(MARKER_CLOSE);
        report.record(hit.id);
        copied = hit.end;
        cursor = hit.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// The longest match anchored at `at`, ties going to the earlier matcher.
fn best_match(bytes: &[u8], at: usize) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    for matcher in MATCHERS {
        if let Some(hit) = matcher(bytes, at)
            && best.is_none_or(|current| hit.end > current.end)
        {
            best = Some(hit);
        }
    }
    best
}

/// The offset just past a `[REDACTED:<id>]` marker beginning at `at`, if one does.
fn marker_end(bytes: &[u8], at: usize) -> Option<usize> {
    if !bytes[at..].starts_with(MARKER_OPEN.as_bytes()) {
        return None;
    }
    let from = at + MARKER_OPEN.len();
    let limit = (from + MAX_MARKER_CHARS).min(bytes.len());
    bytes[from..limit]
        .iter()
        .position(|byte| *byte == MARKER_CLOSE as u8)
        .map(|offset| from + offset + 1)
}

// --- vendor-prefixed tokens ------------------------------------------------------------------

/// Body length a classic `gh?_` token must reach. GitHub's classic personal access tokens carry
/// exactly 36 base62 characters after the prefix; requiring at least that many refuses `ghp_x`
/// in a sentence about token prefixes while accepting every real one.
const GITHUB_CLASSIC_BODY_CHARS: usize = 36;

/// Dot-separated segments a tail must have before it is taken as a JWT's. A JWT is `x.y.z`, so
/// after a word-byte run has swallowed the header there are exactly **two** left.
const JWT_TAIL_SEGMENTS: usize = 2;

/// Extends a `ghs_` token through the dot-separated tail of a stateless installation token.
///
/// GitHub began issuing **stateless** installation tokens shaped `ghs_<app id>_<JWT>` in April
/// 2026, warning integrators that installation tokens are no longer 40 characters. A JWT is
/// dot-separated, and a run of word bytes stops dead at the first dot — which would replace the
/// `ghs_` prefix and leave the payload and signature on the screen.
///
/// Two guards keep that from eating prose, and both were review findings. It runs for `ghs_`
/// alone, because that is the only prefix GitHub gave a dotted format to; and it demands the
/// [`JWT_TAIL_SEGMENTS`] a JWT actually has, because one dot and one word is a sentence —
/// `ghp_…456.Then restart` must keep its `.Then`.
fn stateless_tail(bytes: &[u8], end: usize) -> usize {
    let mut extended = end;
    let mut segments = 0;
    while bytes.get(extended) == Some(&b'.') {
        let segment = run(bytes, extended + 1, is_base64url);
        if segment == 0 {
            break;
        }
        extended += 1 + segment;
        segments += 1;
    }
    if segments >= JWT_TAIL_SEGMENTS {
        extended
    } else {
        end
    }
}
/// Body length a `github_pat_` token must reach. The fine-grained format is 82 word characters
/// after the prefix; 40 is the floor kept here so a future length change does not silently stop
/// redacting.
const GITHUB_FINE_GRAINED_BODY_CHARS: usize = 40;

fn github_token(bytes: &[u8], at: usize) -> Option<Hit> {
    if !boundary_before(bytes, at) {
        return None;
    }
    const CLASSIC: [&[u8]; 5] = [b"ghp_", b"gho_", b"ghu_", b"ghs_", b"ghr_"];
    for prefix in CLASSIC {
        if bytes[at..].starts_with(prefix) {
            let body_at = at + prefix.len();
            // The length floor is measured over the tail as well, because the stateless format
            // spends most of its length there: `ghs_<app id>_<JWT>` is only twenty-odd characters
            // before its first dot.
            let word = body_at + run(bytes, body_at, is_word_byte);
            let end = if prefix == b"ghs_" {
                stateless_tail(bytes, word)
            } else {
                word
            };
            if end - body_at >= GITHUB_CLASSIC_BODY_CHARS {
                return Some(Hit {
                    id: PatternId::GithubToken,
                    start: at,
                    end,
                });
            }
        }
    }
    const FINE_GRAINED: &[u8] = b"github_pat_";
    if bytes[at..].starts_with(FINE_GRAINED) {
        let body = run(bytes, at + FINE_GRAINED.len(), is_word_byte);
        if body >= GITHUB_FINE_GRAINED_BODY_CHARS {
            return Some(Hit {
                id: PatternId::GithubToken,
                start: at,
                end: at + FINE_GRAINED.len() + body,
            });
        }
    }
    None
}

/// Body length an `sk-ant-` key must reach. The prefix alone is unambiguous — no English word and
/// no identifier convention produces it — so the floor is only there to refuse the literal
/// `sk-ant-…` a document writes when it is talking *about* the prefix.
const ANTHROPIC_BODY_CHARS: usize = 16;

fn anthropic_key(bytes: &[u8], at: usize) -> Option<Hit> {
    const PREFIX: &[u8] = b"sk-ant-";
    if !boundary_before(bytes, at) || !bytes[at..].starts_with(PREFIX) {
        return None;
    }
    let body = run(bytes, at + PREFIX.len(), is_key_body_byte);
    (body >= ANTHROPIC_BODY_CHARS).then_some(Hit {
        id: PatternId::AnthropicKey,
        start: at,
        end: at + PREFIX.len() + body,
    })
}

/// Body length a bare `sk-` key must reach.
const OPENAI_BODY_CHARS: usize = 20;

/// `sk-` is a weak anchor on its own: `sk-forward-compatibility` is a perfectly ordinary
/// kebab-case identifier of the right charset and length. The structural discriminator is
/// **charset class, not entropy** — an OpenAI key is base62 and therefore carries both an
/// uppercase letter and a digit, which kebab-case prose does not.
fn openai_key(bytes: &[u8], at: usize) -> Option<Hit> {
    const PREFIX: &[u8] = b"sk-";
    if !boundary_before(bytes, at) || !bytes[at..].starts_with(PREFIX) {
        return None;
    }
    if bytes[at..].starts_with(b"sk-ant-") {
        return None;
    }
    let body_at = at + PREFIX.len();
    let body = run(bytes, body_at, is_key_body_byte);
    if body < OPENAI_BODY_CHARS {
        return None;
    }
    let body_bytes = &bytes[body_at..body_at + body];
    let mixed =
        body_bytes.iter().any(u8::is_ascii_uppercase) && body_bytes.iter().any(u8::is_ascii_digit);
    mixed.then_some(Hit {
        id: PatternId::OpenAiKey,
        start: at,
        end: body_at + body,
    })
}

/// An AWS access key id is its four-character prefix plus exactly sixteen uppercase base36
/// characters — a fixed length, which is why this pattern needs no other guard.
const AWS_ACCESS_KEY_BODY_CHARS: usize = 16;

fn aws_access_key_id(bytes: &[u8], at: usize) -> Option<Hit> {
    const PREFIXES: [&[u8]; 2] = [b"AKIA", b"ASIA"];
    if !boundary_before(bytes, at) {
        return None;
    }
    for prefix in PREFIXES {
        if !bytes[at..].starts_with(prefix) {
            continue;
        }
        let body_at = at + prefix.len();
        let body = run(bytes, body_at, |byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit()
        });
        // Exactly sixteen: a longer run is some other SHOUTING_IDENTIFIER that happens to open
        // with these four letters, and eating it would be the false positive this pattern set is
        // built to avoid.
        if body == AWS_ACCESS_KEY_BODY_CHARS
            && !bytes.get(body_at + body).is_some_and(|b| is_word_byte(*b))
        {
            return Some(Hit {
                id: PatternId::AwsAccessKeyId,
                start: at,
                end: body_at + body,
            });
        }
    }
    None
}

/// Body length a Slack token must reach after its prefix.
const SLACK_BODY_CHARS: usize = 10;

/// The prefixes Slack issues, spelled out rather than derived from `xox` plus a letter class.
///
/// The letter class was a review finding: it carried an `o`, which the cited ruleset
/// (`xox[baprs]`) does not, and `xoxo-mom-and-dad-love-you` was redacting as a bot token. The
/// standing rule in the provenance file is that a pattern is never widened without a source, and
/// a class is a widening nobody has to notice. A list of literals cannot drift that way.
///
/// `xoxe-` (refresh) and `xapp-` (app-level) are current formats in the same ruleset, added here
/// with the same footing as the rest.
const SLACK_PREFIXES: [&[u8]; 7] = [
    b"xoxb-", b"xoxa-", b"xoxp-", b"xoxr-", b"xoxs-", b"xoxe-", b"xapp-",
];

fn slack_token(bytes: &[u8], at: usize) -> Option<Hit> {
    if !boundary_before(bytes, at) {
        return None;
    }
    let prefix = SLACK_PREFIXES
        .into_iter()
        .find(|prefix| bytes[at..].starts_with(prefix))?;
    let body_at = at + prefix.len();
    let body = run(bytes, body_at, |byte| {
        byte.is_ascii_alphanumeric() || byte == b'-'
    });
    (body >= SLACK_BODY_CHARS).then_some(Hit {
        id: PatternId::SlackToken,
        start: at,
        end: body_at + body,
    })
}

/// Body length a `glpat-` token must reach. GitLab personal access tokens are 20 base64url
/// characters.
const GITLAB_BODY_CHARS: usize = 20;

fn gitlab_token(bytes: &[u8], at: usize) -> Option<Hit> {
    const PREFIX: &[u8] = b"glpat-";
    if !boundary_before(bytes, at) || !bytes[at..].starts_with(PREFIX) {
        return None;
    }
    let body_at = at + PREFIX.len();
    let body = run(bytes, body_at, is_key_body_byte);
    (body >= GITLAB_BODY_CHARS).then_some(Hit {
        id: PatternId::GitlabToken,
        start: at,
        end: body_at + body,
    })
}

/// An npm automation token is `npm_` plus exactly 36 base62 characters.
const NPM_BODY_CHARS: usize = 36;

fn npm_token(bytes: &[u8], at: usize) -> Option<Hit> {
    const PREFIX: &[u8] = b"npm_";
    if !boundary_before(bytes, at) || !bytes[at..].starts_with(PREFIX) {
        return None;
    }
    let body_at = at + PREFIX.len();
    let body = run(bytes, body_at, is_base62);
    (body >= NPM_BODY_CHARS).then_some(Hit {
        id: PatternId::NpmToken,
        start: at,
        end: body_at + body,
    })
}

/// A Google API key is `AIza` plus exactly 35 base64url characters.
const GOOGLE_BODY_CHARS: usize = 35;

fn google_api_key(bytes: &[u8], at: usize) -> Option<Hit> {
    const PREFIX: &[u8] = b"AIza";
    if !boundary_before(bytes, at) || !bytes[at..].starts_with(PREFIX) {
        return None;
    }
    let body_at = at + PREFIX.len();
    let body = run(bytes, body_at, is_key_body_byte);
    (body >= GOOGLE_BODY_CHARS).then_some(Hit {
        id: PatternId::GoogleApiKey,
        start: at,
        end: body_at + body,
    })
}

/// Shortest first segment a JWT can have. `eyJ` is base64url for `{"`, so anything shorter is not
/// yet a header object.
const JWT_HEADER_CHARS: usize = 12;
/// Shortest payload segment.
const JWT_PAYLOAD_CHARS: usize = 4;

/// Anchored on the *shape*, not on the prefix alone: any base64 of a JSON document opens with
/// `eyJ`, so what makes this a token rather than an encoded blob is the two dots.
fn jwt(bytes: &[u8], at: usize) -> Option<Hit> {
    if !boundary_before(bytes, at) || !bytes[at..].starts_with(b"eyJ") {
        return None;
    }
    let header = run(bytes, at, is_base64url);
    if header < JWT_HEADER_CHARS || bytes.get(at + header) != Some(&b'.') {
        return None;
    }
    let payload_at = at + header + 1;
    let payload = run(bytes, payload_at, is_base64url);
    if payload < JWT_PAYLOAD_CHARS || bytes.get(payload_at + payload) != Some(&b'.') {
        return None;
    }
    // The signature may legitimately be empty (`alg: none`); the second dot is what has already
    // established the shape.
    let signature_at = payload_at + payload + 1;
    let signature = run(bytes, signature_at, is_base64url);
    Some(Hit {
        id: PatternId::Jwt,
        start: at,
        end: signature_at + signature,
    })
}

const PEM_BEGIN: &[u8] = b"-----BEGIN ";
const PEM_PRIVATE: &[u8] = b"PRIVATE KEY-----";
const PEM_END: &[u8] = b"-----END ";
const PEM_DASHES: &[u8] = b"-----";
/// How far past `-----BEGIN ` the `PRIVATE KEY-----` label may sit. Long enough for
/// `RSA`, `EC`, `OPENSSH`, `ENCRYPTED`, and anything of that shape; short enough that the check
/// is a constant.
const PEM_LABEL_SCAN_BYTES: usize = 40;

/// The only multi-line pattern. A truncated block — a transcript cut mid-paste — is redacted to
/// the end of the text rather than left alone: a private key missing its footer is still a
/// private key.
fn private_key_block(bytes: &[u8], at: usize) -> Option<Hit> {
    if !bytes[at..].starts_with(PEM_BEGIN) || (at > 0 && bytes[at - 1] == b'-') {
        return None;
    }
    let label_at = at + PEM_BEGIN.len();
    let limit = (label_at + PEM_LABEL_SCAN_BYTES).min(bytes.len());
    let label = &bytes[label_at..limit];
    let offset = find(label, PEM_PRIVATE)?;
    if label[..offset].contains(&b'\n') {
        return None;
    }
    let body_at = label_at + offset + PEM_PRIVATE.len();
    let end = match find(&bytes[body_at..], PEM_END) {
        Some(footer) => {
            let after = body_at + footer + PEM_END.len();
            find(&bytes[after..], PEM_DASHES)
                .map_or(bytes.len(), |tail| after + tail + PEM_DASHES.len())
        }
        None => bytes.len(),
    };
    Some(Hit {
        id: PatternId::PrivateKeyBlock,
        start: at,
        end,
    })
}

const AUTHORIZATION: &[u8] = b"authorization";

/// Shortest credential this will believe in. Sixteen characters is the base64 of the shortest
/// `Basic` pair anybody really uses (`admin:admin` encodes to exactly that), and it is comfortably
/// under any bearer token.
const MIN_CREDENTIAL_CHARS: usize = 16;

/// Keeps the header name and the scheme word and replaces only the credential, because
/// `Authorization: Bearer [REDACTED:authorization-header]` tells a reader what happened and
/// `[REDACTED:authorization-header]` alone does not.
///
/// `token` joins `Bearer` and `Basic` because that is the scheme GitHub's own documentation uses.
///
/// # The word after the scheme is not automatically a credential
///
/// A review found this pattern reading three ordinary sentences as headers: `Set the
/// Authorization: Bearer token header on every request`, `Authorization: Bearer <token> is the
/// required format`, and `authorization: basic understanding of the protocol helps`. Prose is
/// exactly what follows a colon in a coaching report, so the credential itself has to be shaped
/// like one: at least [`MIN_CREDENTIAL_CHARS`] long, and carrying a digit or one of the base64
/// characters `= + /`. Every real credential is base62, base64, or a JWT, and all of those do;
/// `token`, `<token>`, and `understanding` do none of it, and the angle brackets of a placeholder
/// are no longer in the charset at all.
///
/// **Recall cost, named:** a short all-letter base64 `Basic` credential — `dXNlcjpwYXNz` — is not
/// matched. It is twelve characters of no digits and no padding, which is indistinguishable from
/// the next word of a sentence, and this pattern is not willing to eat sentences to catch it.
fn authorization_header(bytes: &[u8], at: usize) -> Option<Hit> {
    if !boundary_before(bytes, at) || !starts_with_ignore_case(bytes, at, AUTHORIZATION) {
        return None;
    }
    let mut cursor = at + AUTHORIZATION.len();
    if bytes.get(cursor).is_some_and(|byte| is_word_byte(*byte)) {
        return None;
    }
    cursor += usize::from(matches!(bytes.get(cursor), Some(b'"' | b'\'')));
    cursor += run(bytes, cursor, is_blank);
    if bytes.get(cursor) != Some(&b':') {
        return None;
    }
    cursor += 1;
    cursor += run(bytes, cursor, is_blank);
    cursor += usize::from(matches!(bytes.get(cursor), Some(b'"' | b'\'')));
    const SCHEMES: [&[u8]; 3] = [b"bearer", b"basic", b"token"];
    let scheme = SCHEMES
        .into_iter()
        .find(|scheme| starts_with_ignore_case(bytes, cursor, scheme))?;
    cursor += scheme.len();
    let blanks = run(bytes, cursor, is_blank);
    if blanks == 0 {
        return None;
    }
    cursor += blanks;
    let credential = run(bytes, cursor, is_credential_byte);
    if credential < MIN_CREDENTIAL_CHARS {
        return None;
    }
    let shaped = bytes[cursor..cursor + credential]
        .iter()
        .any(|byte| byte.is_ascii_digit() || matches!(byte, b'=' | b'+' | b'/'));
    shaped.then_some(Hit {
        id: PatternId::AuthorizationHeader,
        start: cursor,
        end: cursor + credential,
    })
}

/// Shortest URL scheme this will believe in.
const MIN_URL_SCHEME_CHARS: usize = 2;
/// Longest userinfo; past this the `@` found is almost certainly not a URL's.
const MAX_USERINFO_CHARS: usize = 256;

/// `scheme://user:pass@host` — and only with the colon. Requiring `user:pass` rather than any
/// userinfo is what keeps `ssh://git@github.com/surdy/munshi.git`, which is in this repository's
/// own manifest, out of the report.
///
/// The whole userinfo goes, not just the password: `https://oauth2:x@…` and
/// `https://ghp_…@…` both put the credential in the part a naive reading calls the username.
fn url_credentials(bytes: &[u8], at: usize) -> Option<Hit> {
    if !boundary_before(bytes, at) || !bytes[at].is_ascii_alphabetic() {
        return None;
    }
    let scheme = run(bytes, at, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-')
    });
    if scheme < MIN_URL_SCHEME_CHARS || !bytes[at + scheme..].starts_with(b"://") {
        return None;
    }
    let userinfo_at = at + scheme + 3;
    let userinfo = run(bytes, userinfo_at, is_userinfo_byte);
    if userinfo == 0 || userinfo > MAX_USERINFO_CHARS {
        return None;
    }
    let at_sign = userinfo_at + userinfo;
    if bytes.get(at_sign) != Some(&b'@') || !bytes[userinfo_at..at_sign].contains(&b':') {
        return None;
    }
    Some(Hit {
        id: PatternId::UrlCredentials,
        start: userinfo_at,
        end: at_sign,
    })
}

/// Key names a *normalized* key (lowercased, with `_`, `-`, and `.` removed) must **end with** to
/// make the assignment a credential.
///
/// Ends-with, not contains, and the archive is what settled it. A contains-match fired 17,302
/// times over the 723 mirrored transcripts, and the keys doing it were overwhelmingly *code*:
/// `tokenType`, `tokenState`, `onBearerTokenChange`, `credentials_path`, `check-theme-tokens.sh`,
/// `splitRightTokens`. What separates those from `cloudflare_api_token` is the same convention in
/// every naming style there is — **the thing a name denotes goes last**. `resume_token` holds a
/// token; `token_grant` holds a grant.
///
/// The multi-word entries are there because normalization removes the separators, so
/// `AWS_SECRET_ACCESS_KEY` becomes `awssecretaccesskey` and ends with `secretaccesskey` rather
/// than with `secret`. The recall this costs is named in the provenance file: a key that buries
/// its noun (`GITHUB_TOKEN_FOR_CI`) is not matched *as an assignment* — though a real value under
/// it usually still is, by its own vendor pattern.
const SECRET_KEY_WORDS: [&[u8]; 12] = [
    b"token",
    b"secret",
    b"password",
    b"passwd",
    b"passphrase",
    b"apikey",
    b"credential",
    b"privatekey",
    b"secretkey",
    b"secretaccesskey",
    b"accesskey",
    b"authkey",
];

/// The normalized key that earns [`PatternId::AwsSecretKey`] instead of the generic id.
const AWS_SECRET_KEY_NAME: &[u8] = b"secretaccesskey";

/// Shortest key that can end in a keyword (`token`).
const MIN_KEY_CHARS: usize = 5;
/// Longest key this will normalize. A "key" longer than this is a sentence.
const MAX_KEY_CHARS: usize = 64;
/// Shortest *bare* value this will believe is a credential.
const MIN_BARE_SECRET_CHARS: usize = 6;
/// A bare value that is entirely alphabetic and shorter than this is prose, not a credential —
/// `the token: a short string` must survive a coaching report intact.
///
/// **This is one of the two places precision is bought with recall**, and the price is named: a
/// weak all-letter password (`password: letmein`) is not redacted bare. Quoting it
/// (`password: "letmein"`) is enough to bring it back, because a quoted value is structure rather
/// than a sentence.
pub const MIN_ALPHABETIC_SECRET_CHARS: usize = 20;

fn secret_assignment(bytes: &[u8], at: usize) -> Option<Hit> {
    let quote = match bytes[at] {
        byte @ (b'"' | b'\'') => Some(byte),
        byte if is_key_name_byte(byte) => None,
        _ => return None,
    };
    if quote.is_none() && !boundary_before(bytes, at) {
        return None;
    }
    let key_at = at + usize::from(quote.is_some());
    let key = run(bytes, key_at, is_key_name_byte);
    if !(MIN_KEY_CHARS..=MAX_KEY_CHARS).contains(&key) {
        return None;
    }
    let mut cursor = key_at + key;
    if let Some(quote) = quote {
        if bytes.get(cursor) != Some(&quote) {
            return None;
        }
        cursor += 1;
    }
    cursor += run(bytes, cursor, is_blank);
    // Cheap structural gate before the key is normalized at all: no separator, no assignment.
    if !matches!(bytes.get(cursor), Some(b'=' | b':')) {
        return None;
    }
    cursor += 1;
    // `==` is a comparison, not an assignment.
    if bytes.get(cursor) == Some(&b'=') {
        return None;
    }
    cursor += run(bytes, cursor, is_blank);
    let name = normalized_key(&bytes[key_at..key_at + key])?;
    let normalized = &name.0[..name.1];
    if !SECRET_KEY_WORDS
        .iter()
        .any(|word| normalized.ends_with(word))
    {
        return None;
    }
    let id = if normalized.ends_with(AWS_SECRET_KEY_NAME) {
        PatternId::AwsSecretKey
    } else {
        PatternId::SecretAssignment
    };

    let (value_at, value_end, quoted) = match bytes.get(cursor) {
        Some(&quote @ (b'"' | b'\'')) => {
            let value_at = cursor + 1;
            (value_at, quoted_value_end(bytes, value_at, quote)?, true)
        }
        _ => {
            let value = run(bytes, cursor, is_bare_value_byte);
            (cursor, cursor + value, false)
        }
    };
    let value = &bytes[value_at..value_end];
    if value.is_empty() {
        return None;
    }
    if !quoted {
        // A bare number is a measurement. This archive records `"input_tokens": 61184` tens of
        // thousands of times, and a key ending in `token` is exactly what that is.
        if value.len() < MIN_BARE_SECRET_CHARS || value.iter().all(u8::is_ascii_digit) {
            return None;
        }
        // The other half of what the archive taught this pattern. An unquoted value carrying no
        // digit at all is, in a transcript, almost always an *expression*: `token = self.next`,
        // `password = config.password`. A credential is base62, base64url, or hex, and all three
        // carry digits. The exception kept is the long all-letter passphrase, which has no digits
        // by design and is not an expression either, because an expression that long would have
        // punctuation in it.
        let digits = value.iter().any(u8::is_ascii_digit);
        let passphrase =
            value.len() >= MIN_ALPHABETIC_SECRET_CHARS && value.iter().all(u8::is_ascii_alphabetic);
        if !digits && !passphrase {
            return None;
        }
    }
    Some(Hit {
        id,
        start: value_at,
        end: value_end,
    })
}

/// Longest quoted value this will scan for a closing quote. A value longer than this is a pasted
/// document, not a credential, and the scan gives up rather than running to the end of a
/// two-megabyte record looking for a quote that is not coming.
const MAX_QUOTED_VALUE_CHARS: usize = 4096;

/// Where a quoted value ends: the offset of its closing `quote`, with `\`-escaped bytes skipped.
///
/// **A review finding, and the invariant it establishes is that a quoted value is never
/// *partially* redacted.** The naive run stopped at the first `"`, escaped or not, so
/// `{"password": "abc\"def123XYZ"}` matched `abc\` and left `def123XYZ` sitting beside a marker
/// claiming the value had been scrubbed — the worst outcome this module has, because it is a
/// visible secret wearing the reassurance of a redaction. Munshi transcripts are JSONL, so an
/// escaped quote inside a value is not an edge case, it is the normal encoding.
///
/// `None` — no match at all, rather than a short one — when the value runs past a newline, past
/// [`MAX_QUOTED_VALUE_CHARS`], or past the end of the text. Refusing to match leaves the bare
/// charset to decide, which under-claims; matching short would over-claim, and over-claiming is
/// the failure that matters.
fn quoted_value_end(bytes: &[u8], from: usize, quote: u8) -> Option<usize> {
    let mut cursor = from;
    let limit = (from + MAX_QUOTED_VALUE_CHARS).min(bytes.len());
    while cursor < limit {
        match bytes[cursor] {
            b'\n' => return None,
            // An escape covers whatever follows it, the quote included — unless what follows is a
            // line ending or nothing at all, which is a broken value rather than an escaped one.
            b'\\' => match bytes.get(cursor + 1) {
                None | Some(b'\n') => return None,
                Some(_) => cursor += 2,
            },
            byte if byte == quote => return Some(cursor),
            _ => cursor += 1,
        }
    }
    None
}

/// The key lowercased with `_`, `-`, and `.` dropped, in a stack buffer plus its length. `None`
/// when it does not fit, which [`MAX_KEY_CHARS`] has already made impossible for a real key.
fn normalized_key(key: &[u8]) -> Option<([u8; MAX_KEY_CHARS], usize)> {
    let mut buffer = [0u8; MAX_KEY_CHARS];
    let mut length = 0;
    for byte in key {
        if matches!(byte, b'_' | b'-' | b'.') {
            continue;
        }
        *buffer.get_mut(length)? = byte.to_ascii_lowercase();
        length += 1;
    }
    Some((buffer, length))
}

// --- prose, and the pair a URL splits across two parameters -----------------------------------

/// The nouns that, standing alone as a word, say outright that what follows is a credential.
///
/// Whole words, case-insensitively, and **single** words: `api key` and `secret key` are not
/// matched as phrases, which is a named recall cost rather than an oversight. A second word is a
/// second place for prose to get in, and `secret` and `token` alone already carry most of the
/// traffic.
///
/// `username` is deliberately absent, here as in [`SECRET_KEY_WORDS`]: a username is not a secret,
/// and the one case where it travels as half of a live pair is [`paired_username`]'s, which has the
/// password beside it as evidence. `key` is absent because it is an ordinary English word — *the
/// key insight*, *the key is idempotence* — and this pattern's only guard is what comes next.
///
/// **`pass` is here because the archive spells it that way.** Review's eyeball pass found the same
/// production credential written a second way — ``user `feedface00` · pass `c0ffeec0ffee` `` — where
/// neither the noun nor the wrapper was one this pattern knew. It is the riskiest entry in the list
/// (*a boarding pass*, *pass the build*, *pass-through*), and it is carried on the same evidence as
/// the rest: it fires on this archive only where a credential is, and nowhere else in 933,292
/// records.
const PROSE_SECRET_WORDS: [&[u8]; 9] = [
    b"password",
    b"passwords",
    b"passwd",
    b"passphrase",
    b"pass",
    b"pwd",
    b"token",
    b"secret",
    b"apikey",
];

/// What a prose value may be wrapped in: a markdown code span or a pair of straight quotes.
///
/// The archive writes credentials in running text and in tables, and a table writes them
/// ``pass `c0ffeec0ffee` ``. The wrapper is *not* treated as extra evidence — the value inside still
/// has to pass every test a bare one does — it is only accepted as a boundary, because otherwise the
/// backtick fails the single-space adjacency and is not in the value charset either, and the whole
/// spelling is invisible.
///
/// **Whole value or nothing.** A wrapped value that does not close in the value charset is refused
/// outright rather than matched up to the first stray byte; the escaped-quote review finding on
/// [`quoted_value_end`] is the same invariant, and a marker beside half a credential is the worst
/// outcome this module has.
const PROSE_WRAPPERS: [u8; 3] = *b"`\"'";

/// Longest entry in [`PROSE_SECRET_WORDS`]; a word longer than this cannot be one.
const MAX_PROSE_WORD_CHARS: usize = 10;

/// The bytes any [`PROSE_SECRET_WORDS`] entry can begin with, in either case.
///
/// The same kind of cheap reject as [`MAYBE_START`]. This is the only matcher that has to *measure
/// a word* before it can rule itself out, so without a first-byte test it measures one at every
/// letter of a two-megabyte transcript; with it, only at four letters in either case.
/// `prose_first_bytes_cover_every_noun` pins the list to the nouns rather than to memory, because a
/// noun this misses is a pattern that silently never fires.
const PROSE_FIRST_BYTES: [u8; 8] = *b"pPtTsSaA";

/// The words allowed to stand between the noun and the value. *The password is c0ffeec0ffee* is the
/// form a person actually types, and a copula carries no meaning a redactor should trip over.
///
/// Exactly one of them, and nothing else: every additional word between the anchor and the value is
/// another sentence this pattern could wander into.
const PROSE_FILLER_WORDS: [&[u8]; 2] = [b"is", b"was"];

/// Shortest value [`prose_credential`] will believe. Twelve is the length of the production
/// sighting that opened qanungo #15 (`c0ffeec0ffee`, twelve hex characters), and it is long enough
/// that the English words carrying a digit which could otherwise follow a credential noun —
/// `sha256`, `base64url`, `argon2id`, `oauth2`, `utf8` — are all too short to reach it.
const MIN_PROSE_SECRET_CHARS: usize = 12;

/// A credential noun, a space, and a value shaped like a credential — with **no separator at all**,
/// which is the whole gap this pattern closes.
///
/// [`secret_assignment`] requires a `:` or `=` binding the key to the value, and the real archived
/// instruction behind qanungo #15 wrote the same pair twice: `password=c0ffeec0ffee` in a query
/// string, which fired, and `password c0ffeec0ffee` in the sentence above it, which did not.
///
/// # The value shape is the entire guard
///
/// There is no structure on the left to lean on — a noun and a space is also how *password
/// manager*, *token stream*, *token count*, and *the password is stored in the keychain* are
/// written. So the value has to look like a credential and nothing else:
///
/// - **The credential charset**, base62 plus `-` and `_`. Narrower than a bare assignment's on
///   purpose: `.` and `/` are what turn a value into a file path, a version, or the next sentence.
/// - **At least [`MIN_PROSE_SECRET_CHARS`]**, which is what keeps every ordinary next word out.
/// - **A digit and a letter.** All-letters is an English word however long it is, and all-digits is
///   a measurement — this archive writes `"input_tokens": 61184` tens of thousands of times, and
///   `token 61184` is the same number with the punctuation dropped.
///
/// And one guard on the gap rather than on the value, which the archive scan asked for: **exactly
/// one space**, never a tab and never a run of them. A sentence puts one space between two words;
/// several spaces or a tab is a *column*, and the first scan of this pattern found it reading one —
/// `ok  mosquitto-passwd     deadbeef0badf00d  -> /run/…/passwd`, a deploy log's aligned digest
/// column under a resource whose name happens to end in `passwd`. A digest is the thing this
/// module refuses hardest to redact.
///
/// # Wrappers are boundaries, never evidence
///
/// The noun, the value, or both may sit in a markdown code span or a pair of straight quotes —
/// ``pass `c0ffeec0ffee` ``, `` the `password` c0ffeec0ffee ``. Both spellings are in this archive
/// and both were invisible to the first cut, because a backtick is neither a space nor a value byte.
/// A wrapper is accepted as a *boundary* only: it buys the value nothing, unlike
/// [`secret_assignment`]'s quoted form, where the quotes are structure a separator has already
/// vouched for. A wrapped noun must close in the delimiter it opened with, and a wrapped value must
/// close in the value charset or the match is refused whole.
///
/// **Recall cost, named:** an all-letter prose passphrase (`password correcthorsebatterystaple`) is
/// not matched, though the assignment form of it still is. Bare prose has no quotes to fall back
/// on, and a twenty-letter run is a word this pattern is not willing to eat.
///
/// Two further stand-downs, both about *partial* redaction rather than about prose: a value with a
/// dotted word tail is left whole for whoever owns it, and a value one of the [`VENDOR_MATCHERS`]
/// recognizes is left to that matcher, which knows how far it runs.
fn prose_credential(bytes: &[u8], at: usize) -> Option<Hit> {
    if !PROSE_FIRST_BYTES.contains(&bytes[at]) || !boundary_before(bytes, at) {
        return None;
    }
    let word = run(bytes, at, |byte| byte.is_ascii_alphabetic());
    if word > MAX_PROSE_WORD_CHARS || !word_is(bytes, at, word, &PROSE_SECRET_WORDS) {
        return None;
    }
    let mut cursor = at + word;
    // A noun in a code span closes it before the gap begins. The *matching* delimiter, so that a
    // stray quote on one side alone cannot manufacture a boundary.
    if at > 0
        && PROSE_WRAPPERS.contains(&bytes[at - 1])
        && bytes.get(cursor) == Some(&bytes[at - 1])
    {
        cursor += 1;
    }
    // One space, not a separator and not a column: `password: x` and `password=x` are the
    // assignment's, a noun running straight into a word byte (`password_reset`) is an identifier
    // rather than a sentence, and `passwd\t<hex>` is a table.
    if !single_space(bytes, cursor) {
        return None;
    }
    cursor += 1;
    // At most one copula, and only when it too is followed by a single space.
    let filler = run(bytes, cursor, |byte| byte.is_ascii_alphabetic());
    if word_is(bytes, cursor, filler, &PROSE_FILLER_WORDS) && single_space(bytes, cursor + filler) {
        cursor += filler + 1;
    }
    let wrapper = bytes
        .get(cursor)
        .copied()
        .filter(|byte| PROSE_WRAPPERS.contains(byte));
    let value_at = cursor + usize::from(wrapper.is_some());
    let value_end = value_at + run(bytes, value_at, is_prose_value_byte);
    let value = &bytes[value_at..value_end];
    if value.len() < MIN_PROSE_SECRET_CHARS
        || !value.iter().any(u8::is_ascii_digit)
        || !value.iter().any(u8::is_ascii_alphabetic)
    {
        return None;
    }
    // A value carrying an `_` and no lowercase letter at all is SHOUTING_SNAKE_CASE: the naming
    // convention of an environment variable or a CI secret, which is to say a **name** rather than
    // a value. The wrapped form found this on the archive — *Paste into GitHub secret
    // `DEVELOPER_ID_CERTIFICATE_BASE64`* — and redacting it does not merely cost precision, it
    // destroys the runbook step the name is the subject of. Credentials are base62 or hex and carry
    // lowercase; this is the argument `aws_access_key_id` already makes when it refuses a longer
    // SHOUTING_IDENTIFIER run.
    if value.contains(&b'_') && !value.iter().any(u8::is_ascii_lowercase) {
        return None;
    }
    match wrapper {
        // Whole value or nothing: a wrapper that does not close where the value charset ends was
        // wrapping something this pattern cannot see the end of.
        Some(wrapper) if bytes.get(value_end) != Some(&wrapper) => return None,
        // A dotted tail — a JWT, a routable GitLab token, a hostname — is longer than this charset
        // can see. Matching its first segment would be a partial redaction, so stand down. Only the
        // bare form can have one; a wrapped value has just been proved to end at its wrapper.
        None if bytes.get(value_end) == Some(&b'.')
            && bytes.get(value_end + 1).is_some_and(|b| is_word_byte(*b)) =>
        {
            return None;
        }
        _ => {}
    }
    if VENDOR_MATCHERS
        .into_iter()
        .any(|matcher| matcher(bytes, value_at).is_some())
    {
        return None;
    }
    Some(Hit {
        id: PatternId::ProseCredential,
        start: value_at,
        end: value_end,
    })
}

/// Whether `at` holds exactly one space — one, so that a run of them or a tab, which is a table's
/// column rule rather than a sentence's word break, is refused.
fn single_space(bytes: &[u8], at: usize) -> bool {
    bytes.get(at) == Some(&b' ') && !bytes.get(at + 1).is_some_and(|byte| is_blank(*byte))
}

/// Whether the `length` bytes at `at` are one of `words`, ignoring case, as a whole word.
fn word_is(bytes: &[u8], at: usize, length: usize, words: &[&[u8]]) -> bool {
    if length == 0
        || bytes
            .get(at + length)
            .is_some_and(|byte| is_word_byte(*byte))
    {
        return false;
    }
    words
        .iter()
        .any(|word| word.len() == length && starts_with_ignore_case(bytes, at, word))
}

/// The query-parameter names [`paired_username`] will scrub beside a password.
const PAIRED_USER_KEYS: [&[u8]; 2] = [b"username", b"user"];

/// The password-class parameter names that make one of those sensitive. Narrower than
/// [`SECRET_KEY_WORDS`] on purpose: `session=` and `token=` beside a `user=` are a session and a
/// user id, which is what half the URLs in any archive look like. It is the *password* that turns
/// the username into the other half of a live login.
const PAIRED_PASSWORD_WORDS: [&[u8]; 3] = [b"password", b"passwd", b"passphrase"];

/// How far either side of the username this will look for its password. Two kilobytes is longer
/// than any real URL and short enough that the search is a constant rather than a walk over a
/// pasted document.
const MAX_URL_SCAN_BYTES: usize = 2048;

/// The `username=` of a URL whose `password=` fired on its own.
///
/// A username is not a secret — [`SECRET_KEY_WORDS`] leaves it out deliberately, and prose
/// usernames stay readable, because a report that redacts the person's own login name everywhere it
/// appears has become noise. What is sensitive is a **live pair**, and the archived instruction
/// behind qanungo #15 is exactly that: `?username=feedface00&password=…` in one URL, where scrubbing
/// the password alone leaves the reader holding one usable half.
///
/// So the evidence for this pattern is entirely *adjacency*, and it is spelled structurally:
///
/// - The name is a query parameter — the byte before it is `?` or `&` — and not a word in a
///   sentence.
/// - It sits inside a URL: the run of URL bytes reaching back from it contains a `://`.
/// - Somewhere in that same run, before or after, a password-class parameter is present **and
///   [`secret_assignment`] accepts it**. Reusing the assignment matcher rather than re-testing the
///   value is what keeps the two in step: `?username=x&password=` with an empty or prose value
///   scrubs nothing, so there is no pair, so the username stays.
///
/// The scrub stops at the query value's charset, so `&` and the parameters after it survive and the
/// URL still reads as a URL.
fn paired_username(bytes: &[u8], at: usize) -> Option<Hit> {
    if at == 0 || !matches!(bytes[at - 1], b'?' | b'&') {
        return None;
    }
    let key = PAIRED_USER_KEYS
        .into_iter()
        .find(|key| starts_with_ignore_case(bytes, at, key))?;
    if bytes.get(at + key.len()) != Some(&b'=') {
        return None;
    }
    let value_at = at + key.len() + 1;
    let value_end = value_at + run(bytes, value_at, is_query_value_byte);
    if value_end == value_at || !url_password_fires(bytes, at) {
        return None;
    }
    Some(Hit {
        id: PatternId::PairedUsername,
        start: value_at,
        end: value_end,
    })
}

/// Whether the URL surrounding `at` carries a password-class parameter that would itself be
/// redacted.
fn url_password_fires(bytes: &[u8], at: usize) -> bool {
    let from = at - back_run(bytes, at, MAX_URL_SCAN_BYTES, is_url_byte);
    // Without a scheme this is not a URL, and a bare `?a=b&password=c` in a shell line or a piece
    // of source is not the pair this pattern is about.
    if find(&bytes[from..at], b"://").is_none() {
        return false;
    }
    // Bounded on this side too, and by slicing rather than by trusting the run to stop: the
    // constant says "either side", and a document with no whitespace in it would otherwise walk
    // forward to the end of a two-megabyte record.
    let ceiling = (at + MAX_URL_SCAN_BYTES).min(bytes.len());
    let to = at + run(&bytes[..ceiling], at, is_url_byte);
    (from..to).any(|index| {
        index > 0
            && matches!(bytes[index - 1], b'?' | b'&')
            && is_password_parameter(bytes, index)
            && secret_assignment(bytes, index).is_some()
    })
}

/// Whether the parameter name at `index` normalizes to a password-class word.
fn is_password_parameter(bytes: &[u8], index: usize) -> bool {
    let key = run(bytes, index, is_key_name_byte);
    if !(MIN_KEY_CHARS..=MAX_KEY_CHARS).contains(&key) || bytes.get(index + key) != Some(&b'=') {
        return false;
    }
    normalized_key(&bytes[index..index + key]).is_some_and(|(buffer, length)| {
        PAIRED_PASSWORD_WORDS
            .iter()
            .any(|word| buffer[..length].ends_with(word))
    })
}

/// How many consecutive bytes *before* `at`, up to `limit`, satisfy `allowed`.
fn back_run(bytes: &[u8], at: usize, limit: usize, allowed: impl Fn(u8) -> bool) -> usize {
    let floor = at.saturating_sub(limit);
    bytes[floor..at]
        .iter()
        .rev()
        .take_while(|byte| allowed(**byte))
        .count()
}

// --- byte classes ----------------------------------------------------------------------------

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_base62(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

/// The charset of a URL-safe vendor key: base62 plus `-` and `_`.
fn is_key_body_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'=')
}

fn is_key_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn is_blank(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

/// What a bare (unquoted) assigned value may be made of: **the credential charset**, which is
/// base62 plus the six punctuation marks base64url, hex-with-separators, and `.env` values use.
///
/// An allow-list rather than a deny-list, and that is the whole point. A deny-list admitted
/// `self.next_token(`, `Option<String>`, and `crate::Token` — the assignments a transcript is
/// actually full of, because a transcript is mostly source code. None of those is expressible in
/// this charset, and every real credential is.
fn is_bare_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'/' | b'=')
}

/// What a credential after `Bearer` may be made of. Wider than a bare value — a base64 Basic
/// credential ends in `=` and may contain `/` and `+` — and narrower than a line.
fn is_credential_byte(byte: u8) -> bool {
    byte.is_ascii_graphic()
        && !matches!(
            byte,
            b',' | b';' | b'"' | b'\'' | b'`' | b'[' | b']' | b'{' | b'}' | b'<' | b'>'
        )
}

/// What a value found in *prose* may be made of: base62 plus `-` and `_`, and nothing else.
///
/// Narrower than [`is_bare_value_byte`], because a bare assignment has its separator as evidence
/// and prose has none. `.` and `/` are the two that matter: they are how a value becomes a file
/// path, a version number, a hostname, or the first word of the next sentence, and every one of
/// those would be a false positive with a credential noun sitting in front of it.
fn is_prose_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// What a URL query value may be made of. Stops at `&`, `#`, `=`, and anything non-graphic, so a
/// scrubbed parameter never swallows the parameters after it.
fn is_query_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'%' | b'+')
}

/// What a URL may be made of, for the purpose of deciding that two parameters are in the *same*
/// one. Whitespace and quoting end it, as does anything a URL is conventionally wrapped in.
fn is_url_byte(byte: u8) -> bool {
    byte.is_ascii_graphic()
        && !matches!(
            byte,
            b'"' | b'\'' | b'`' | b'<' | b'>' | b'\\' | b'|' | b'^' | b'{' | b'}' | b'[' | b']'
        )
}

fn is_userinfo_byte(byte: u8) -> bool {
    byte.is_ascii_graphic()
        && !matches!(
            byte,
            b'/' | b'?'
                | b'#'
                | b'@'
                | b'['
                | b']'
                | b'"'
                | b'\''
                | b'`'
                | b'\\'
                | b'<'
                | b'>'
                | b','
        )
}

// --- small scanning helpers ------------------------------------------------------------------

/// How many consecutive bytes from `from` satisfy `allowed`.
fn run(bytes: &[u8], from: usize, allowed: impl Fn(u8) -> bool) -> usize {
    bytes.get(from..).map_or(0, |tail| {
        tail.iter().take_while(|byte| allowed(**byte)).count()
    })
}

/// Whether the byte before `at` cannot be part of the same token.
fn boundary_before(bytes: &[u8], at: usize) -> bool {
    at == 0 || !is_word_byte(bytes[at - 1])
}

fn starts_with_ignore_case(bytes: &[u8], at: usize, literal: &[u8]) -> bool {
    bytes
        .get(at..at + literal.len())
        .is_some_and(|window| window.eq_ignore_ascii_case(literal))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------------------------
// The profanity pass
// ---------------------------------------------------------------------------------------------

/// The wordlist, in full forms rather than stems.
///
/// Conservative on purpose, and enumerated rather than generated: a stemmer that accepted
/// `<entry> + s|ed|ing` would decide what it masks by arithmetic nobody reviewed, and the point of
/// a list is that a person can read all of it. `dick` and `prick` are deliberately absent — one is
/// a name and the other has an ordinary sense, and a coaching report that masks a colleague's name
/// has done more damage than the word it caught.
///
/// Matched as **whole words**, case-insensitively. Never as substrings: `Scunthorpe`, `class`, and
/// `assassin` are ordinary words, and the classic failure of this feature is masking them.
const PROFANITY: [&[u8]; 26] = [
    b"arse",
    b"arsehole",
    b"arseholes",
    b"ass",
    b"asshole",
    b"assholes",
    b"bastard",
    b"bastards",
    b"bitch",
    b"bitches",
    b"bollocks",
    b"bullshit",
    b"cunt",
    b"cunts",
    b"fuck",
    b"fucked",
    b"fucker",
    b"fuckers",
    b"fucking",
    b"fucks",
    b"motherfucker",
    b"motherfuckers",
    b"shit",
    b"shits",
    b"shitting",
    b"shitty",
];

/// Longest entry in [`PROFANITY`]; a word longer than this cannot be one.
const MAX_PROFANITY_CHARS: usize = 16;

fn mask_profanity(text: &str, report: &mut RedactionReport) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        // Markers are the secrets pass's output, not content — and a pattern id is not a word this
        // pass gets to rewrite.
        if let Some(after) = marker_end(bytes, cursor) {
            cursor = after;
            continue;
        }
        if !bytes[cursor].is_ascii_alphabetic() || !boundary_before(bytes, cursor) {
            cursor += 1;
            continue;
        }
        let word = run(bytes, cursor, |byte| byte.is_ascii_alphabetic());
        let end = cursor + word;
        // A word abutting a digit or an underscore is an identifier fragment, not a word.
        let standalone = !bytes.get(end).is_some_and(|byte| is_word_byte(*byte));
        if standalone && word <= MAX_PROFANITY_CHARS && is_profane(&bytes[cursor..end]) {
            out.push_str(&text[copied..cursor]);
            out.push(char::from(bytes[cursor]));
            for _ in 1..word {
                out.push(PROFANITY_MASK);
            }
            report.record(PatternId::Profanity);
            copied = end;
        }
        cursor = end;
    }
    out.push_str(&text[copied..]);
    out
}

fn is_profane(word: &[u8]) -> bool {
    let mut lowered = [0u8; MAX_PROFANITY_CHARS];
    let Some(slot) = lowered.get_mut(..word.len()) else {
        return false;
    };
    for (target, byte) in slot.iter_mut().zip(word) {
        *target = byte.to_ascii_lowercase();
    }
    let lowered = &lowered[..word.len()];
    PROFANITY.contains(&lowered)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One planted canary per secret pattern. Every value is obviously fake — the bodies are
    /// keyboard filler of the right shape — and every one is a *complete* example, because a
    /// pattern set tested only on the prefix would pass while redacting nothing real.
    ///
    /// [`PatternId::PairedUsername`] is the one id with no row here, and the reason is the pattern
    /// itself: its evidence is a password beside it, so any string that fires it fires twice and
    /// cannot be a single-fire canary. It is held to
    /// `a_username_is_scrubbed_only_beside_a_password_in_the_same_url` instead.
    const CANARIES: [(PatternId, &str); 20] = [
        (
            PatternId::GithubToken,
            "the classic one is ghp_FAKEfake0123456789ABCDEFabcdef012345 in the log",
        ),
        (
            PatternId::GithubToken,
            "and gho_FAKEfake0123456789ABCDEFabcdef012345 alongside it",
        ),
        (
            // The stateless installation format GitHub began issuing in April 2026, whose JWT
            // tail a word-byte run would have stopped short of.
            PatternId::GithubToken,
            "ghs_123456_eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJGQUtFIn0.FAKEfakeSIGNATURE",
        ),
        (
            PatternId::GithubToken,
            "github_pat_11FAKEFAKE0aaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
        (
            PatternId::AnthropicKey,
            "export it as sk-ant-api03-FAKEfake0123456789-ABCDEFabcdefFAKE and move on",
        ),
        (
            PatternId::OpenAiKey,
            "sk-FAKEfake0123456789ABCDEFabcdef0123456789ABCDEF was in the env",
        ),
        (
            PatternId::AwsAccessKeyId,
            "AKIAFAKEFAKEFAKE1234 was printed by the terminal",
        ),
        (
            PatternId::AwsSecretKey,
            "aws_secret_access_key=FAKEfake0123456789ABCDEFabcdef0123456789",
        ),
        (
            PatternId::SlackToken,
            "xoxb-000000000000-111111111111-FAKEfakeFAKEfakeFAKEfake",
        ),
        (
            // Refresh and app-level tokens: current formats in the ruleset the rest came from,
            // and — like the AWS and npm shapes — zero-firing on this archive, which is exactly
            // why they need a canary rather than a production sighting.
            PatternId::SlackToken,
            "xoxe-1-FAKEfakeFAKEfakeFAKEfake",
        ),
        (
            PatternId::SlackToken,
            "xapp-1-A0FAKEFAKE-000000000000-FAKEfakeFAKEfake",
        ),
        (
            PatternId::GitlabToken,
            "glpat-FAKEfake0123456789AB in the runner config",
        ),
        (
            PatternId::NpmToken,
            "npm_FAKEfake0123456789ABCDEFabcdef012345 in .npmrc",
        ),
        (
            PatternId::GoogleApiKey,
            "AIzaFAKEfake0123456789ABCDEFabcdef01234 came from the console",
        ),
        (
            PatternId::Jwt,
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJGQUtFIn0.FAKEfakeSIGNATUREvalue",
        ),
        (
            PatternId::PrivateKeyBlock,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nFAKEfakeFAKEfake\nmore==\n-----END OPENSSH PRIVATE KEY-----",
        ),
        (
            PatternId::AuthorizationHeader,
            "Authorization: Bearer FAKEfake0123456789ABCDEFabcdef",
        ),
        (
            PatternId::UrlCredentials,
            "cloned https://surdy:FAKEfakePASSWORD@example.com/repo.git today",
        ),
        (
            PatternId::SecretAssignment,
            "PATWARI_API_KEY=FAKEfake0123456789ABCDEF was exported",
        ),
        (
            // The qanungo #15 shape: a credential noun, a space, and no separator at all.
            PatternId::ProseCredential,
            "use the portal and password fakefake0123 to sign in",
        ),
    ];

    /// The fake bodies above, so a test can assert none of them survives anywhere.
    const CANARY_BODIES: [&str; 13] = [
        "ghp_FAKEfake0123456789ABCDEFabcdef012345",
        "sk-ant-api03-FAKEfake0123456789-ABCDEFabcdefFAKE",
        "AKIAFAKEFAKEFAKE1234",
        "xoxb-000000000000-111111111111-FAKEfakeFAKEfakeFAKEfake",
        "glpat-FAKEfake0123456789AB",
        "npm_FAKEfake0123456789ABCDEFabcdef012345",
        "AIzaFAKEfake0123456789ABCDEFabcdef01234",
        "eyJzdWIiOiJGQUtFIn0",
        "FAKEfakeFAKEfake",
        "FAKEfake0123456789ABCDEFabcdef",
        "FAKEfakePASSWORD",
        "FAKEfake0123456789ABCDEF",
        "fakefake0123",
    ];

    fn secrets() -> Redactor {
        Redactor::new()
    }

    /// The done-bar of the pattern set: every class has a canary, and every canary is both
    /// recognized *as its own pattern* and gone from the text.
    #[test]
    fn every_planted_canary_is_recognized_and_removed() {
        for (expected, text) in CANARIES {
            let scrubbed = secrets().scrub(text);
            assert_eq!(
                scrubbed.report.count(expected),
                1,
                "{expected} did not fire on {text:?}"
            );
            assert_eq!(
                scrubbed.report.total(),
                1,
                "extra patterns fired on {text:?}"
            );
            assert!(
                scrubbed.text.contains(&format!("{MARKER_OPEN}{expected}]")),
                "no marker for {expected} in {:?}",
                scrubbed.text
            );
        }
    }

    /// Nothing of a planted secret survives into the rendered text, whichever pattern caught it.
    #[test]
    fn no_planted_secret_body_survives_the_scrub() {
        let corpus = CANARIES.map(|(_, text)| text).join("\n");
        let scrubbed = secrets().scrub(&corpus);
        for body in CANARY_BODIES {
            assert!(
                !scrubbed.text.contains(body),
                "{body} survived into {:?}",
                scrubbed.text
            );
        }
    }

    /// The property the whole design turns on: a report that carried what it matched would have
    /// redacted nothing. `Debug` is checked, not just the accessors, because `Debug` is the form a
    /// report reaches a log line or a panic message in.
    #[test]
    fn the_report_never_carries_what_it_matched() {
        let corpus = CANARIES.map(|(_, text)| text).join("\n");
        let scrubbed = Redactor::new().with_profanity(true).scrub(&corpus);
        let rendered = format!(
            "{:?} {:?}",
            scrubbed.report,
            scrubbed.report.fired().collect::<Vec<_>>()
        );
        for body in CANARY_BODIES {
            assert!(!rendered.contains(body), "{body} leaked into the report");
        }
        for fragment in ["FAKE", "fake", "surdy", "PATWARI"] {
            assert!(
                !rendered.contains(fragment),
                "{fragment} leaked into the report"
            );
        }
        assert!(scrubbed.report.total() >= CANARIES.len());
    }

    /// A document that passes through two surfaces must not grow nested markers, and the second
    /// scrub must report nothing.
    #[test]
    fn scrubbing_scrubbed_text_changes_nothing() {
        let corpus = format!(
            "{}\nthis fucking thing again\n",
            CANARIES.map(|(_, text)| text).join("\n")
        );
        let redactor = Redactor::new().with_profanity(true);
        let once = redactor.scrub(&corpus);
        let twice = redactor.scrub(&once.text);
        assert_eq!(twice.text, once.text);
        assert!(
            twice.report.is_empty(),
            "a second scrub fired {:?}",
            twice.report.fired().collect::<Vec<_>>()
        );
    }

    /// A marker is not content: neither pass may take one apart, whichever pass wrote it.
    #[test]
    fn a_marker_is_never_re_examined() {
        let text = "key=[REDACTED:secret-assignment] and [REDACTED:jwt] stay put";
        let scrubbed = Redactor::new().with_profanity(true).scrub(text);
        assert_eq!(scrubbed.text, text);
        assert!(scrubbed.report.is_empty());
    }

    #[test]
    fn the_defaults_are_secrets_on_and_profanity_off() {
        let redactor = Redactor::new();
        assert!(redactor.redacts_secrets());
        assert!(!redactor.filters_profanity());
        assert_eq!(redactor, Redactor::default());
        const { assert!(REDACT_SECRETS_BY_DEFAULT) };
        const { assert!(!FILTER_PROFANITY_BY_DEFAULT) };
    }

    /// Four independent combinations, not two: turning one pass on must not turn the other on,
    /// and turning secrets off must not disable profanity.
    #[test]
    fn the_two_passes_are_independently_switched() {
        let text = "sk-FAKEfake0123456789ABCDEFabcdef0123456789ABCDEF, this shit again";
        let cases = [
            (false, false, false, false),
            (true, false, true, false),
            (false, true, false, true),
            (true, true, true, true),
        ];
        for (secrets_on, profanity_on, expect_secret, expect_profanity) in cases {
            let scrubbed = Redactor::new()
                .with_secrets(secrets_on)
                .with_profanity(profanity_on)
                .scrub(text);
            assert_eq!(
                scrubbed.report.count(PatternId::OpenAiKey) > 0,
                expect_secret,
                "secrets={secrets_on} profanity={profanity_on}"
            );
            assert_eq!(
                scrubbed.report.count(PatternId::Profanity) > 0,
                expect_profanity,
                "secrets={secrets_on} profanity={profanity_on}"
            );
        }
        let raw = Redactor::new().with_secrets(false).scrub(text);
        assert_eq!(raw.text, text, "both passes off is a copy");
        assert!(raw.report.is_empty());
    }

    /// The precision half of the trade. Every line here is ordinary text a coaching report could
    /// plausibly quote, and a redactor that ate any of it would be worse than none.
    #[test]
    fn ordinary_prose_and_ordinary_code_survive_untouched() {
        const INNOCENT: [&str; 18] = [
            "the token: a short string the tokenizer emits",
            "\"input_tokens\": 61184, \"output_tokens\": 2048",
            "max_tokens=4096",
            "sk-forward-compatibility-shim is the module name",
            "we set the api_key from 1Password rather than the env",
            "password: letmein",
            "AKIAFAKEFAKEFAKE1234567890 is not a key id, it is too long",
            "ghp_short",
            "ssh://git@github.com/surdy/munshi.git",
            "https://patwari.clusterfault.com/v1/sessions",
            "the secret sauce is that it does nothing clever",
            "credential helper: osxkeychain",
            "AIzaSHORT",
            "eyJhbGciOiJIUzI1NiJ9 alone is not a token",
            "authorization is a topic, not a header",
            "xoxb-short",
            "-----BEGIN CERTIFICATE-----",
            "npm_install failed",
        ];
        for text in INNOCENT {
            let scrubbed = secrets().scrub(text);
            assert_eq!(
                scrubbed.text,
                text,
                "redacted ordinary text: {:?}",
                scrubbed.report.fired().collect::<Vec<_>>()
            );
            assert!(scrubbed.report.is_empty());
        }
    }

    /// The assignment pattern as the archive left it. A contains-match on the key fired 17,302
    /// times over the 723 mirrored transcripts, and almost all of it was *source code*, because a
    /// transcript is mostly source code. Both tightenings are pinned here: the key must **end** in
    /// a credential word, and a bare value must be spellable in the credential charset and carry a
    /// digit.
    #[test]
    fn an_assignment_is_anchored_on_the_end_of_its_key_and_the_charset_of_its_value() {
        const CODE: [&str; 8] = [
            // The noun is not last: these names are *about* a credential, they do not hold one.
            "\"tokenType\": \"bearer\"",
            "tokenState = Idle",
            "onBearerTokenChange(handler)",
            "let credentials_path = home.join(\".aws\")",
            "splitRightTokens = 3",
            // The key is right, but the value is an expression rather than a credential.
            "token = self.next_token",
            "password: Option<String>",
            "let api_key = config.api_key.clone();",
        ];
        for text in CODE {
            let scrubbed = secrets().scrub(text);
            assert_eq!(scrubbed.text, text, "redacted source code: {text:?}");
        }
        const CREDENTIALS: [(&str, PatternId); 4] = [
            (
                "CLOUDFLARE_API_TOKEN=FAKEfake0123456789abcdef",
                PatternId::SecretAssignment,
            ),
            (
                "ts_authkey: FAKEfake0123456789",
                PatternId::SecretAssignment,
            ),
            (
                "AWS_SECRET_ACCESS_KEY=FAKEfake0123456789ABCDEFabcdef0123456789",
                PatternId::AwsSecretKey,
            ),
            (
                "password = correcthorsebatterystaple",
                PatternId::SecretAssignment,
            ),
        ];
        for (text, expected) in CREDENTIALS {
            let scrubbed = secrets().scrub(text);
            assert_eq!(scrubbed.report.count(expected), 1, "missed {text:?}");
        }
    }

    /// The worst outcome this module has is a *partial* redaction: a marker claiming a value was
    /// scrubbed with half of it still on the screen beside it. A review found the quoted-value
    /// run stopping at the first `"` whether or not it was escaped — and munshi transcripts are
    /// JSONL, so an escaped quote inside a value is the normal encoding, not an edge case.
    #[test]
    fn an_escaped_quote_inside_a_value_does_not_split_the_redaction() {
        let scrubbed = secrets().scrub(r#"{"password": "abc\"def123XYZ"}"#);
        assert_eq!(
            scrubbed.text,
            r#"{"password": "[REDACTED:secret-assignment]"}"#
        );
        assert!(
            !scrubbed.text.contains("def123XYZ"),
            "half the value survived: {:?}",
            scrubbed.text
        );
        // Several escapes, and an escaped backslash immediately before the real closing quote —
        // the case where miscounting escapes runs the value on past its own end.
        let scrubbed = secrets().scrub(r#"{"api_key": "a\"b\\c123\"d\\"}"#);
        assert_eq!(
            scrubbed.text,
            r#"{"api_key": "[REDACTED:secret-assignment]"}"#
        );
        // A quote that never closes is refused outright rather than matched short.
        for unterminated in [
            "{\"password\": \"abc\\\"def123XYZ\n",
            "{\"password\": \"abc\\",
        ] {
            let scrubbed = secrets().scrub(unterminated);
            assert_eq!(scrubbed.text, unterminated, "matched an unterminated value");
        }
    }

    /// The word after a scheme is not automatically a credential. All three of these are ordinary
    /// sentences a coaching report could quote, and all three were being redacted before review.
    #[test]
    fn a_sentence_after_the_scheme_word_is_not_a_credential() {
        const PROSE: [&str; 4] = [
            "Set the Authorization: Bearer token header on every request",
            "Authorization: Bearer <token> is the required format",
            "authorization: basic understanding of the protocol helps",
            "Authorization: Basic auth is what the endpoint wants",
        ];
        for text in PROSE {
            let scrubbed = secrets().scrub(text);
            assert_eq!(scrubbed.text, text, "redacted a sentence: {text:?}");
        }
        // A credential still is one: long enough, and carrying base64's own characters.
        for text in [
            "Authorization: Bearer FAKEfake0123456789ABCDEFabcdef",
            "Authorization: Basic YWRtaW46YWRtaW4=",
        ] {
            let scrubbed = secrets().scrub(text);
            assert_eq!(
                scrubbed.report.count(PatternId::AuthorizationHeader),
                1,
                "missed {text:?}"
            );
        }
    }

    /// `xox` plus a letter class was a widening nobody sourced: the cited ruleset has no `o`, and
    /// `xoxo-…` is a sign-off, not a bot token.
    #[test]
    fn the_slack_prefixes_are_the_sourced_ones_and_no_others() {
        let scrubbed = secrets().scrub("xoxo-mom-and-dad-love-you");
        assert_eq!(scrubbed.text, "xoxo-mom-and-dad-love-you");
        assert!(scrubbed.report.is_empty());
        for prefix in ["xoxb", "xoxa", "xoxp", "xoxr", "xoxs", "xoxe", "xapp"] {
            let text = format!("{prefix}-FAKEfake0123456789");
            assert_eq!(
                secrets().scrub(&text).report.count(PatternId::SlackToken),
                1,
                "missed {text}"
            );
        }
    }

    /// The stateless-token tail is `ghs_`-only and wants the two dot segments a JWT actually has.
    /// One dot and one word is a sentence, and it was eating `.Then`.
    #[test]
    fn the_stateless_token_tail_does_not_eat_the_next_sentence() {
        let scrubbed = secrets().scrub("ghp_FAKEfake0123456789ABCDEFabcdef012345.Then restart");
        assert_eq!(scrubbed.text, "[REDACTED:github-token].Then restart");

        // A `ghs_` stateless token goes whole, JWT tail included.
        let stateless = "ghs_123456_eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJGQUtFIn0.FAKEfakeSIGNATURE";
        let scrubbed = secrets().scrub(stateless);
        assert_eq!(scrubbed.text, "[REDACTED:github-token]");
        // And a `ghs_` followed by one dotted word keeps the word.
        let scrubbed = secrets().scrub("ghs_FAKEfake0123456789ABCDEFabcdef012345.Then restart");
        assert_eq!(scrubbed.text, "[REDACTED:github-token].Then restart");
    }

    /// A quoted value is structure rather than a sentence, so it skips the prose guards that a
    /// bare value has to pass. This is the documented way back to the recall the guards cost.
    #[test]
    fn a_quoted_value_is_data_even_when_a_bare_one_would_read_as_prose() {
        let scrubbed = secrets().scrub("\"password\": \"letmein\"");
        assert_eq!(scrubbed.report.count(PatternId::SecretAssignment), 1);
        assert_eq!(
            scrubbed.text,
            "\"password\": \"[REDACTED:secret-assignment]\""
        );
    }

    /// The assignment keeps its key and the header keeps its scheme: a reader has to be able to
    /// tell *that* a credential was there, which is half of what makes a coaching report useful.
    #[test]
    fn a_scrub_leaves_the_shape_of_what_it_removed() {
        let cases = [
            (
                "PATWARI_API_KEY=FAKEfake0123456789ABCDEF",
                "PATWARI_API_KEY=[REDACTED:secret-assignment]",
            ),
            (
                "Authorization: Bearer FAKEfake0123456789ABCDEFabcdef",
                "Authorization: Bearer [REDACTED:authorization-header]",
            ),
            (
                "https://surdy:FAKEfakePASSWORD@example.com/repo.git",
                "https://[REDACTED:url-credentials]@example.com/repo.git",
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(secrets().scrub(text).text, expected);
        }
    }

    /// A key block goes whole, header and footer included, and a truncated one goes to the end of
    /// the text rather than being left as a key with no footer.
    #[test]
    fn a_private_key_block_is_removed_whole_even_when_truncated() {
        let complete = "before\n-----BEGIN RSA PRIVATE KEY-----\nAAAA\nBBBB\n-----END RSA PRIVATE KEY-----\nafter";
        let scrubbed = secrets().scrub(complete);
        assert_eq!(scrubbed.text, "before\n[REDACTED:private-key-block]\nafter");

        let truncated = "before\n-----BEGIN EC PRIVATE KEY-----\nAAAA\nBBBB";
        let scrubbed = secrets().scrub(truncated);
        assert_eq!(scrubbed.text, "before\n[REDACTED:private-key-block]");
    }

    /// The Scunthorpe test, and its relatives. Substring matching is the classic failure of this
    /// feature and the reason the list is matched on whole words only.
    #[test]
    fn profanity_is_matched_as_a_whole_word_only() {
        let redactor = Redactor::new().with_profanity(true);
        const INNOCENT: [&str; 8] = [
            "Scunthorpe is in Lincolnshire",
            "the class assassin passed the bitchute check",
            "shiitake mushrooms",
            "assessment of the massive dataset",
            "arsenal, bassist, cassette",
            "he was pissed_off_flag in the enum",
            "FUCKS_GIVEN_COUNT is an identifier",
            "the compass, the bassline, and the cassowary",
        ];
        for text in INNOCENT {
            let scrubbed = redactor.scrub(text);
            assert_eq!(scrubbed.text, text, "masked inside a word: {text:?}");
        }
    }

    /// The mask keeps the first character and the length, which is enough for a reader to see
    /// that something was masked without reading it.
    #[test]
    fn profanity_is_masked_case_insensitively_keeping_shape() {
        let redactor = Redactor::new().with_profanity(true);
        let scrubbed = redactor.scrub("What the Fuck, this SHIT is bollocks.");
        assert_eq!(scrubbed.text, "What the F***, this S*** is b*******.");
        assert_eq!(scrubbed.report.count(PatternId::Profanity), 3);
        // One id, not one per word: a per-word count is the matched text spelled as a histogram.
        assert_eq!(scrubbed.report.fired().count(), 1);
    }

    /// The scrub is a rendering path, and a rendering path that panics on a non-ASCII transcript
    /// is a bug that reaches production the first time someone works in Hindi.
    #[test]
    fn text_around_a_secret_survives_byte_for_byte_including_non_ascii() {
        let text = "परियोजना की कुंजी ghp_FAKEfake0123456789ABCDEFabcdef012345 है — मिटाओ";
        let scrubbed = secrets().scrub(text);
        assert_eq!(
            scrubbed.text,
            "परियोजना की कुंजी [REDACTED:github-token] है — मिटाओ"
        );
    }

    /// Ids are what a marker and a report footer are made of, so they must be unique, stable, and
    /// safe to render inside `[REDACTED:…]` and a Markdown table alike.
    #[test]
    fn pattern_ids_are_unique_and_safe_to_render() {
        let mut ids: Vec<_> = PATTERNS.iter().map(|pattern| pattern.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "pattern ids must be unique");
        for pattern in PATTERNS {
            let id = pattern.as_str();
            assert!(!id.is_empty());
            assert!(
                id.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
                "{id} is not a kebab-case id"
            );
            assert!(id.len() + MARKER_OPEN.len() < MAX_MARKER_CHARS);
            assert_eq!(PATTERNS[pattern.index()], pattern);
        }
        assert_eq!(SECRET_PATTERNS.len(), PATTERNS.len() - 1);
        assert!(!SECRET_PATTERNS.contains(&PatternId::Profanity));
    }

    /// The prose matcher rules itself out on the first byte before it measures a word, so that
    /// byte list has to cover every noun — a noun it does not cover would be a pattern that
    /// silently never fires.
    #[test]
    fn prose_first_bytes_cover_every_noun() {
        for word in PROSE_SECRET_WORDS {
            assert!(
                PROSE_FIRST_BYTES.contains(&word[0]),
                "{} can never be reached",
                String::from_utf8_lossy(word)
            );
            assert!(word.len() <= MAX_PROSE_WORD_CHARS);
            assert!(word.iter().all(u8::is_ascii_lowercase));
        }
        // Upper case is the same word: the archive writes `Password` and `Token` in prose too.
        for text in ["the Password FAKEfake0123", "a TOKEN FAKEfake0123"] {
            assert_eq!(
                secrets()
                    .scrub(text)
                    .report
                    .count(PatternId::ProseCredential),
                1,
                "missed {text:?}"
            );
        }
    }

    /// The revision is a date because two documents are comparable only when the pattern set that
    /// produced them was the same, and a date is what names the research file beside it.
    #[test]
    fn the_pattern_revision_names_the_provenance_file() {
        assert_eq!(PATTERN_REVISION.len(), "2026-08-24".len());
        assert!(
            PATTERN_REVISION
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
        );
    }

    /// A caller scrubbing a whole archive needs the counts to add up across documents without
    /// ever holding the documents.
    #[test]
    fn reports_absorb_one_another() {
        let redactor = secrets();
        let mut total = RedactionReport::default();
        for (_, text) in CANARIES {
            total.absorb(&redactor.scrub(text).report);
        }
        assert_eq!(total.total(), CANARIES.len());
        assert_eq!(total.count(PatternId::GithubToken), 4);
    }

    /// Two secrets on one line, and a secret at each end of the text: the scanner has to resume
    /// correctly after a replacement rather than swallowing the rest of the line.
    #[test]
    fn several_secrets_in_one_document_are_each_replaced() {
        let text = "ghp_FAKEfake0123456789ABCDEFabcdef012345 then AKIAFAKEFAKEFAKE1234 then glpat-FAKEfake0123456789AB";
        let scrubbed = secrets().scrub(text);
        assert_eq!(
            scrubbed.text,
            "[REDACTED:github-token] then [REDACTED:aws-access-key-id] then [REDACTED:gitlab-token]"
        );
        assert_eq!(scrubbed.report.total(), 3);
    }

    /// The instruction that opened qanungo #15, in shape: the same pair written twice, once as
    /// prose and once as a query string, with only the query string's password scrubbed.
    ///
    /// **The values are fake and the host is `example.com`, deliberately.** The production string
    /// is quoted in issue #15; a test file is not somewhere to keep a live credential, and §5 of
    /// the provenance file already refuses "a denylist of the operator's own known secrets" on
    /// exactly that ground. Every length and charset class is the original's: a ten-character
    /// username, a twelve-character hex password, both spelled twice.
    const ISSUE_15: &str = "use http://line.example.com username : fake64f1ab and password fakefake0123 for xtream. and http://line.example.com/get.php?username=fake64f1ab&password=fakefake0123&type=m3u_plus";

    /// The gap itself. Before this revision the prose password and the URL username were both
    /// readable beside a marker claiming the string had been scrubbed.
    #[test]
    fn the_issue_15_instruction_loses_both_halves_of_the_pair() {
        let scrubbed = secrets().scrub(ISSUE_15);
        assert_eq!(
            scrubbed.text,
            "use http://line.example.com username : fake64f1ab and password [REDACTED:prose-credential] for xtream. and http://line.example.com/get.php?username=[REDACTED:paired-username]&password=[REDACTED:secret-assignment]&type=m3u_plus"
        );
        assert_eq!(scrubbed.report.count(PatternId::ProseCredential), 1);
        assert_eq!(scrubbed.report.count(PatternId::PairedUsername), 1);
        assert_eq!(scrubbed.report.count(PatternId::SecretAssignment), 1);
        assert!(
            !scrubbed.text.contains("fakefake0123"),
            "a password survived: {:?}",
            scrubbed.text
        );
        // The prose username survives, and that is the design: a username is not a secret, and a
        // report that redacts the reader's own login everywhere it appears is noise. Only the half
        // of a *live pair* goes.
        assert!(scrubbed.text.contains("username : fake64f1ab"));
        // Twice through two surfaces changes nothing.
        let twice = secrets().scrub(&scrubbed.text);
        assert_eq!(twice.text, scrubbed.text);
        assert!(twice.report.is_empty());
    }

    /// The over-match minefield, and the only thing standing in it is the value shape. Every line
    /// here is a credential noun followed by an ordinary word, which is the same grammar as the
    /// production string.
    #[test]
    fn a_credential_noun_followed_by_ordinary_words_is_prose() {
        const PROSE: [&str; 16] = [
            "password manager",
            "password field",
            "password reset flow",
            "the password is stored in the keychain",
            "token stream",
            "token count",
            "the token was truncated",
            "secret sauce",
            "password hashing with argon2id",
            "token base64url encoding",
            "the token is 4096",
            "output token 61184 of 200000",
            "password 1234567890123456",
            "passphrase correcthorsebatterystaple",
            "token abc123def",
            "secret 2026-08",
        ];
        for text in PROSE {
            let scrubbed = secrets().scrub(text);
            assert_eq!(
                scrubbed.text,
                text,
                "redacted prose: {:?}",
                scrubbed.report.fired().collect::<Vec<_>>()
            );
        }
        // And the shapes that are credentials, with and without the copula.
        for text in [
            "password fakefake0123",
            "the password is fakefake0123",
            "the token was FAKEfake01234567",
            "X-Auth-Token fakeFAKE01234567",
            "apikey fake_fake_0123",
            "pwd fakefake0123-01",
        ] {
            assert_eq!(
                secrets()
                    .scrub(text)
                    .report
                    .count(PatternId::ProseCredential),
                1,
                "missed {text:?}"
            );
        }
    }

    /// The gate spelled out as a grid: every noun crossed with every value shape, so a widening of
    /// either list cannot pass without a fire count moving.
    #[test]
    fn the_prose_gate_is_the_noun_list_crossed_with_the_value_shape() {
        const NOUNS: [(&str, bool); 11] = [
            ("password", true),
            ("passwd", true),
            ("passphrase", true),
            ("pass", true),
            ("pwd", true),
            ("token", true),
            ("secret", true),
            ("apikey", true),
            // Not nouns: a username is not a secret, `key` is an English word, and `api` is only
            // half of one.
            ("username", false),
            ("key", false),
            ("api", false),
        ];
        const VALUES: [(&str, bool); 9] = [
            ("fakefake0123", true),       // twelve, mixed: the production shape
            ("fakefake012", false),       // eleven: under the floor
            ("fakefakefake", false),      // no digit: a word, however long
            ("123456789012", false),      // no letter: a measurement
            ("fake_fake_0123", true),     // the credential charset's own separators
            ("fake/fake/0123", false),    // a path — the run stops at `/`, leaving four characters
            ("fake.fake.0123", false),    // dotted: somebody else's token, left whole
            ("../../etc/passwd0", false), // not even a value
            // SHOUTING_SNAKE_CASE is a CI secret's *name*, and the archive writes it in a code span
            // right after the noun: `Paste into GitHub secret `DEVELOPER_ID_CERTIFICATE_BASE64``.
            ("DEVELOPER_ID_CERTIFICATE_BASE64", false),
        ];
        // A wrapper is a boundary and never evidence, so it may not move a single expectation: the
        // noun and the value shape decide, wrapped or bare. A value the wrapper cannot close on —
        // a path, a dotted token — stays refused, now because the wrapper is not where the value
        // charset ran out.
        for (noun, is_noun) in NOUNS {
            for (value, is_value) in VALUES {
                for gap in [" ", " is ", " was "] {
                    for wrapper in ["", "`", "\"", "'"] {
                        let text = format!("and the {noun}{gap}{wrapper}{value}{wrapper} follows");
                        let fired = secrets()
                            .scrub(&text)
                            .report
                            .count(PatternId::ProseCredential);
                        assert_eq!(
                            fired,
                            usize::from(is_noun && is_value),
                            "{text:?} fired {fired} times"
                        );
                    }
                }
            }
        }
        // A column is not a sentence. The archive scan caught this pattern reading a deploy log's
        // aligned digest column under a resource name ending in `passwd`, and one space is the
        // difference between a sentence and a table.
        for text in [
            "ok       mosquitto-passwd     deadbeef0badf00d  -> /run/mosquitto/passwd",
            "ok\tmosquitto-passwd\tdeadbeef0badf00d",
            "the password  fakefake0123 is two spaces from its noun",
            "the password is  fakefake0123",
        ] {
            let scrubbed = secrets().scrub(text);
            assert_eq!(scrubbed.text, text, "redacted a column: {text:?}");
        }
        // A separator is the assignment's evidence, not this pattern's: the same noun and value
        // bound by `:` or `=` must still be reported as an assignment.
        for text in ["password: fakefake0123", "password=fakefake0123"] {
            let scrubbed = secrets().scrub(text);
            assert_eq!(scrubbed.report.count(PatternId::SecretAssignment), 1);
            assert_eq!(scrubbed.report.count(PatternId::ProseCredential), 0);
        }
        // The noun may be in a code span too, and it must close in the delimiter it opened with: a
        // stray one on a single side is not a boundary.
        for (text, expected) in [
            ("the `password` fakefake0123 is live", 1),
            ("the \"password\" fakefake0123 is live", 1),
            ("the `password\" fakefake0123 is live", 0),
            ("the password` fakefake0123 is live", 0),
        ] {
            let fired = secrets()
                .scrub(text)
                .report
                .count(PatternId::ProseCredential);
            assert_eq!(fired, expected, "{text:?} fired {fired} times");
        }
        // Half a wrapped value is never rendered: a wrapper that does not close where the value
        // charset ends refuses the whole match rather than matching up to the stray byte.
        for text in [
            "pass `fakefake0123 and then some",
            "pass \"fakefake0123\n",
            "pass `fakefake0123.more`",
        ] {
            let scrubbed = secrets().scrub(text);
            assert_eq!(scrubbed.text, text, "matched inside an unclosed wrapper");
        }
    }

    /// The second spelling of the same production credential, which review's eyeball pass found
    /// still readable after the first cut shipped: a table row where the noun is `pass` and the
    /// value is a code span, so neither the noun list nor the single-space adjacency reached it.
    ///
    /// Values fake and shape-identical, as in [`ISSUE_15`].
    #[test]
    fn the_table_spelling_of_the_same_pair_is_scrubbed_too() {
        let scrubbed = secrets().scrub("- user `fake64f1ab` · pass `fakefake0123`");
        assert_eq!(
            scrubbed.text,
            "- user `fake64f1ab` · pass `[REDACTED:prose-credential]`"
        );
        assert_eq!(scrubbed.report.count(PatternId::ProseCredential), 1);
        // The wrapper survives on both sides, which is what makes the row still read as a row —
        // and what makes a second scrub a no-op rather than a nested marker.
        let twice = secrets().scrub(&scrubbed.text);
        assert_eq!(twice.text, scrubbed.text);
        assert!(twice.report.is_empty());
        // `pass` is the riskiest noun in the list, so its ordinary senses are pinned here.
        for text in [
            "pass the build and move on",
            "a boarding pass 12345678 for tomorrow",
            "pass-through is the default",
            "the tests pass consistently on main",
            "pass 12345678901234567890",
            "compass fakefake0123 is not a noun",
        ] {
            let scrubbed = secrets().scrub(text);
            assert_eq!(scrubbed.text, text, "redacted prose: {text:?}");
        }
    }

    /// The least specific pattern in the set must never claim a value one of the vendor shapes
    /// owns — not for the id, but because those shapes run through dots the prose charset stops at,
    /// and a match over the first segment of a JWT is a *partial* redaction.
    #[test]
    fn a_vendor_token_after_a_noun_keeps_its_own_pattern_and_its_whole_length() {
        let cases = [
            (
                "token ghp_FAKEfake0123456789ABCDEFabcdef012345",
                PatternId::GithubToken,
                "token [REDACTED:github-token]",
            ),
            (
                "the token is eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJGQUtFIn0.FAKEfakeSIG",
                PatternId::Jwt,
                "the token is [REDACTED:jwt]",
            ),
            (
                "secret AIzaFAKEfake0123456789ABCDEFabcdef01234",
                PatternId::GoogleApiKey,
                "secret [REDACTED:google-api-key]",
            ),
        ];
        for (text, expected, rendered) in cases {
            let scrubbed = secrets().scrub(text);
            assert_eq!(scrubbed.text, rendered, "partial or misattributed match");
            assert_eq!(scrubbed.report.count(expected), 1);
            assert_eq!(scrubbed.report.count(PatternId::ProseCredential), 0);
            assert_eq!(scrubbed.report.total(), 1);
        }
    }

    /// A username is scrubbed for exactly one reason — the live password beside it in the same
    /// URL — and every way of removing that reason must bring the username back.
    #[test]
    fn a_username_is_scrubbed_only_beside_a_password_in_the_same_url() {
        let paired =
            "https://example.com/get.php?username=fake64f1ab&password=fakefake0123&type=m3u";
        let scrubbed = secrets().scrub(paired);
        assert_eq!(
            scrubbed.text,
            "https://example.com/get.php?username=[REDACTED:paired-username]&password=[REDACTED:secret-assignment]&type=m3u"
        );
        assert_eq!(scrubbed.report.count(PatternId::PairedUsername), 1);
        // Order is not evidence: the password may come first.
        let reversed = "https://example.com/get.php?password=fakefake0123&user=fake64f1ab";
        let scrubbed = secrets().scrub(reversed);
        assert_eq!(
            scrubbed.text,
            "https://example.com/get.php?password=[REDACTED:secret-assignment]&user=[REDACTED:paired-username]"
        );
        // A second scrub finds no pair, because there is no longer a password to pair with.
        let twice = secrets().scrub(&scrubbed.text);
        assert_eq!(twice.text, scrubbed.text);
        assert!(twice.report.is_empty());

        const UNPAIRED: [&str; 6] = [
            // No password parameter at all.
            "https://example.com/api?username=fake64f1ab&type=m3u",
            // A password parameter whose value the assignment pattern refuses — an empty value, a
            // bare number, and an expression. No live half, so no pair.
            "https://example.com/api?username=fake64f1ab&password=",
            "https://example.com/api?username=fake64f1ab&password=12345678",
            // A different URL: whitespace ends the one the username is in.
            "https://example.com/a?username=fake64f1ab and https://example.com/b?password=fakefake0123",
            // Not a URL at all — no scheme, so this is a shell line or a struct literal.
            "?username=fake64f1ab&password=fakefake0123",
            // Not a query parameter: a sentence about the field.
            "the username fake64f1ab goes with password=fakefake0123 in https://example.com/x",
        ];
        for text in UNPAIRED {
            let scrubbed = secrets().scrub(text);
            assert_eq!(
                scrubbed.report.count(PatternId::PairedUsername),
                0,
                "scrubbed an unpaired username: {text:?}"
            );
            assert!(
                scrubbed.text.contains("fake64f1ab"),
                "the username went anyway: {:?}",
                scrubbed.text
            );
        }
    }

    #[test]
    fn an_empty_document_scrubs_to_an_empty_document() {
        let scrubbed = Redactor::new().with_profanity(true).scrub("");
        assert!(scrubbed.text.is_empty());
        assert!(scrubbed.report.is_empty());
        assert_eq!(secrets().scrub_text("plain"), "plain");
    }
}
