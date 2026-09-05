//! The cost fold: what the archive says was billed, deduplicated, and priced.
//!
//! Two consumer rules are documented on `munshi-transcript`'s own types, and both are correctness
//! requirements rather than refinements. This module exists to hold them in one place so no
//! caller has to remember them.
//!
//! # 1. Deduplicate by `message_id` before summing
//!
//! One Claude API message reaches a transcript as *several* records — the assistant text, then
//! each of its tool calls — which share one `message.id` and repeat that message's `usage`
//! verbatim on every one of them. Adding records rather than ids counts the same message two or
//! three times: across the mirror cache, 61,184 claude-code assistant records carry 29,591
//! distinct message ids, and summing per record over-counts output tokens **2.6-fold** (68.0M
//! against the true 26.2M). [`fold_cost`] therefore takes one record's usage per id and counts
//! the rest as duplicates, and [`CostFold::duplicate_records`] reports how many that was, so the
//! divergence is visible rather than assumed.
//!
//! Usage carrying no `message_id` cannot be deduplicated at all. No source in the archive omits
//! one, but a future envelope might, and the honest handling is stated rather than chosen
//! silently: such usage is summed **per record** — over-counting rather than dropping real spend
//! — and every record of it is counted into [`Undeduplicatable`], which the report prints as a
//! flag. The same treatment applies to ids past [`MAX_TRACKED_MESSAGE_IDS`].
//!
//! # 2. Price cache writes from the per-tier buckets, never from the total
//!
//! The two prompt-cache TTLs bill at different multiples of the base input rate — 5-minute at
//! 1.25x, 1-hour at 2x — so `cache_creation_input_tokens`, which states only the sum, cannot
//! price a write. Assuming the cheaper tier under-bills claude-code by 1.6x, because it uses the
//! 1-hour cache exclusively (389,777,788 1-hour tokens against zero 5-minute ones, measured over
//! the archive on 2026-08-23).
//!
//! So the buckets are what this fold reads, and a **non-zero** split wins outright — the archive
//! holds a message whose total reads 0 while its 1-hour bucket reads 2,277, and the bucket is the
//! tier that bills.
//!
//! A write the source did not split that way is not priced at an assumed tier and not dropped
//! either: its tokens land in [`TokenTally::cache_write_untiered`], are reported, and are
//! flagged. "Did not split" covers two shapes, and lumping them together is deliberate — bucket
//! fields that are *present and zero* alongside a non-zero total say exactly as much about the
//! tier as absent ones do, which is nothing, so trusting the empty split would price real spend
//! at $0 and raise no flag at all. Neither shape has been observed in the archive; the code is
//! honest about both anyway, because the alternative is a silent error the day one appears.
//!
//! # What this module renders
//!
//! Nothing. It folds token counts and archive-stated identifiers — model ids, the `speed` /
//! `service_tier` / `inference_geo` billing modifiers, repository names — into counts and
//! dollars. No transcript text of any kind enters a type here: [`munshi_transcript::Record`]'s
//! `assistant_meta` is read and its `classification` — the user text, the assistant text, the
//! tool arguments — is never touched. See [`crate::cost_report`] for the rendering line.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::BufRead;

use chrono::{DateTime, Utc};
use munshi_transcript::{AssistantMeta, Source, TokenUsage, TranscriptStream, UnsupportedVersion};

use crate::pricing::{self, Price, Rates, Unpriced};

/// Distinct message ids remembered per session, on the same reasoning as
/// [`crate::metrics`]'s call-id and command caps: a real session bills messages in the hundreds
/// (the whole archive holds 29,591 across 623 sessions), and this exists so a pathological or
/// adversarial transcript cannot grow the fold's memory without bound. Its effect when reached is
/// stated rather than silent — usage past it is summed per record and flagged as
/// undeduplicatable, so a capped session over-counts *visibly* instead of dropping spend.
pub const MAX_TRACKED_MESSAGE_IDS: usize = 100_000;

/// The most output tokens a session may have produced and still be listed by [`PremiumFlag`].
///
/// **Arbitrary until measured**, and measured once — the whole archive as of 2026-09-04, which is
/// 907 sessions, 391 of them priced, 61 of *those* priced wholly at the day's top tier. The output
/// distribution of those 61 is long-tailed: a median of 88,389 tokens, a maximum of 482,603, and a
/// visible floor cluster of four sessions at 687, 696, 2,040 and 2,845 tokens with a gap to the
/// next one at 4,404. This constant is set in that gap. It selects 4 of 61 (6.6%), which is a
/// handful somebody can open; the median would have selected half the window, which is a census.
///
/// The cluster is where a session stops looking like a piece of work and starts looking like a
/// question and an answer — but that is a description of a *shape*, not a judgement about it. A
/// session under this floor is a small session, and whether small was the wrong place for the
/// dearest model is the reader's call and nobody else's.
pub const PREMIUM_FLAG_MAX_OUTPUT_TOKENS: u64 = 3_000;

/// The most billed messages a session may have carried and still be listed by [`PremiumFlag`].
///
/// **Arbitrary until measured**, on the same 61-session distribution: the median top-tier session
/// billed 102 messages and the largest 598, so a ceiling of eight is a handful of exchanges rather
/// than a working session. Over that archive it was **not** the binding floor — all four sessions
/// [`PREMIUM_FLAG_MAX_OUTPUT_TOKENS`] selected billed seven messages or fewer, and this one
/// excluded none of them — and it is carried anyway, because the two floors describe different
/// shapes: a session can write very little across a hundred messages, and that is a working
/// session with a quiet model rather than a small one.
///
/// Billed *messages*, deliberately, and not user requests. This fold reads
/// [`munshi_transcript::Record`]'s `assistant_meta` and never a record's classification, so the
/// count it can honestly state is the number of distinct API messages the session was billed for —
/// which is also the number the report's by-model table prints, so the two reconcile. Counting user
/// turns would mean reading the conversation, which is exactly what the cost lane's redaction line
/// forbids.
pub const PREMIUM_FLAG_MAX_MESSAGES: u64 = 8;

/// What a harness's transcripts can say about money.
///
/// Not every archived session has a cost story, and the three cases are genuinely different: one
/// can be priced in dollars, one can be counted in tokens and no further, and one says nothing at
/// all. Collapsing them would either invent Copilot dollars or hide Codex sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingSignal {
    /// Claude Code talks to the Anthropic API, whose list prices are published per token.
    AnthropicApi,
    /// Copilot records one `outputTokens` figure per assistant message and nothing else — no
    /// input, no cache, no tier — and its billing regime (premium requests before 2026-06-01,
    /// AI Credits after) is not recoverable from a transcript. Tokens, never dollars.
    TokensOnly,
    /// Codex rollouts record no per-message model or usage anywhere, so nothing about their cost
    /// can be read at any price.
    NoSignal,
}

/// Which cost story `source_agent`'s transcripts have. An unrecognized harness has none, exactly
/// as it has no interpreter.
pub fn billing_signal(source_agent: &str) -> BillingSignal {
    match crate::metrics::source_for_agent(source_agent) {
        Some(Source::ClaudeCode) => BillingSignal::AnthropicApi,
        Some(Source::Copilot) => BillingSignal::TokensOnly,
        Some(Source::Codex) | None => BillingSignal::NoSignal,
    }
}

/// Everything about a message except its token counts that decides what those tokens cost.
///
/// Usage is folded per key rather than per message because the four fields are stable across a
/// session — one model, one serving mode, one region — so the map holds a handful of entries for
/// a transcript with thousands of messages, and pricing then runs once per entry rather than once
/// per message. Each field is carried exactly as the transcript spelled it: reconciling
/// spellings, or mapping one onto a pricing family, is [`crate::pricing`]'s job and is done by
/// exact match.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct BillingKey {
    pub model: Option<String>,
    pub speed: Option<String>,
    pub service_tier: Option<String>,
    pub inference_geo: Option<String>,
}

impl BillingKey {
    /// The key one assistant record bills under.
    fn of(meta: &AssistantMeta, usage: &TokenUsage) -> Self {
        Self {
            model: meta.model.clone(),
            speed: usage.speed.clone(),
            service_tier: usage.service_tier.clone(),
            inference_geo: usage.inference_geo.clone(),
        }
    }
}

/// Token counts summed over some set of messages.
///
/// Every field is a count the source actually recorded. `munshi-transcript` types an unrecorded
/// figure as `None` and warns that `None` is never a zero; this fold adds what is present and
/// adds nothing for what is absent, which under-claims a category the source did not state rather
/// than inventing one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTally {
    /// Messages whose usage was counted here — after deduplication.
    pub messages: u64,
    pub input: u64,
    pub output: u64,
    /// Cache writes at the 5-minute tier, from the per-tier bucket.
    pub cache_write_5m: u64,
    /// Cache writes at the 1-hour tier, from the per-tier bucket.
    pub cache_write_1h: u64,
    /// Cache-creation tokens with no usable per-tier split: the source stated a non-zero total
    /// and either no buckets at all, or buckets that sum to zero. Real tokens that no rate
    /// applies to, because the rate depends on the tier — reported and flagged, never priced at
    /// an assumed one. See the module docs.
    pub cache_write_untiered: u64,
    /// Messages *carrying* such a write. Deliberately not the message count of whatever tally
    /// this is — a billing key that billed three messages, one of which wrote an untiered cache,
    /// has one message here and three in [`TokenTally::messages`], and the report's flagged line
    /// means the former.
    pub cache_write_untiered_messages: u64,
    pub cache_read: u64,
    /// The share of `output` spent on extended thinking. Context, not a category: it is already
    /// inside `output` and is never added to it or priced separately.
    pub thinking: u64,
}

impl TokenTally {
    /// Folds one message's usage.
    ///
    /// Every sum saturates rather than wrapping, on the same reasoning
    /// [`crate::metrics::Activity::sittings`] saturates: these are counts read from somebody
    /// else's file, and a transcript crafted to overflow a `u64` of tokens should produce an
    /// absurd number a reader can see, never a small one they cannot.
    fn observe(&mut self, usage: &TokenUsage) {
        self.messages = self.messages.saturating_add(1);
        self.input = add(self.input, usage.input_tokens);
        self.output = add(self.output, usage.output_tokens);
        self.cache_read = add(self.cache_read, usage.cache_read_input_tokens);
        self.thinking = add(self.thinking, usage.thinking_tokens);
        self.observe_cache_write(usage);
    }

    /// Folds one message's cache-creation figures.
    ///
    /// The buckets are the billing tiers, so where the source stated a **non-zero** split it has
    /// said everything needed to price the write, and the undifferentiated total is not consulted
    /// at all — not to reconcile a residue, not to fill in an absent sibling bucket.
    ///
    /// The zero case is where care is needed, and it is the one an earlier draft got wrong. A
    /// split that sums to zero prices nothing, so if the *total* is non-zero the source has
    /// reported real cache-write spend whose tier it has not disclosed — bucket fields present
    /// and reading `0` say no more about the tier than absent ones do. Trusting the empty split
    /// there would silently drop the spend at $0 with no flag, which is the failure mode this
    /// whole module is arranged against; it is therefore treated exactly like an absent split,
    /// and flagged. A zero total alongside a zero split is simply a message that wrote no cache,
    /// and raises nothing.
    fn observe_cache_write(&mut self, usage: &TokenUsage) {
        let five_minute = usage.cache_5m_input_tokens.unwrap_or_default();
        let one_hour = usage.cache_1h_input_tokens.unwrap_or_default();
        if five_minute > 0 || one_hour > 0 {
            self.cache_write_5m = self.cache_write_5m.saturating_add(five_minute);
            self.cache_write_1h = self.cache_write_1h.saturating_add(one_hour);
            return;
        }
        let total = usage.cache_creation_input_tokens.unwrap_or_default();
        if total > 0 {
            self.cache_write_untiered = self.cache_write_untiered.saturating_add(total);
            self.cache_write_untiered_messages =
                self.cache_write_untiered_messages.saturating_add(1);
        }
    }

    /// Adds another tally into this one.
    fn absorb(&mut self, other: &Self) {
        self.messages = self.messages.saturating_add(other.messages);
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_write_5m = self.cache_write_5m.saturating_add(other.cache_write_5m);
        self.cache_write_1h = self.cache_write_1h.saturating_add(other.cache_write_1h);
        self.cache_write_untiered = self
            .cache_write_untiered
            .saturating_add(other.cache_write_untiered);
        self.cache_write_untiered_messages = self
            .cache_write_untiered_messages
            .saturating_add(other.cache_write_untiered_messages);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.thinking = self.thinking.saturating_add(other.thinking);
    }

    /// Every token counted, of any category. `thinking` is excluded because it is already part of
    /// `output`, and counting it would double one message's own reasoning.
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write_5m)
            .saturating_add(self.cache_write_1h)
            .saturating_add(self.cache_write_untiered)
            .saturating_add(self.cache_read)
    }

    /// Cache writes across both tiers, priced.
    pub fn cache_write_priceable(&self) -> u64 {
        self.cache_write_5m.saturating_add(self.cache_write_1h)
    }

    /// What this tally costs at `rates`, scaled by a modifier multiplier.
    ///
    /// [`TokenTally::cache_write_untiered`] is deliberately absent from the sum: it has no rate,
    /// and quietly charging it at either tier is the 1.6x error this whole module is arranged to
    /// avoid.
    pub fn dollars(&self, rates: Rates, multiplier: f64) -> f64 {
        pricing::dollars(self.input, rates.input, multiplier)
            + pricing::dollars(self.output, rates.output, multiplier)
            + pricing::dollars(self.cache_write_5m, rates.cache_write_5m, multiplier)
            + pricing::dollars(self.cache_write_1h, rates.cache_write_1h, multiplier)
            + pricing::dollars(self.cache_read, rates.cache_read, multiplier)
    }
}

/// Adds a figure the source may not have recorded, saturating. An absent figure adds nothing —
/// `None` is never a zero, but it is also never a number to add.
fn add(running: u64, recorded: Option<u64>) -> u64 {
    running.saturating_add(recorded.unwrap_or_default())
}

/// Usage that could not be deduplicated, and therefore was summed per record.
///
/// Both counters are records, not messages: that is the quantity a reader needs to judge how far
/// the total might be inflated, since one message can repeat across several of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Undeduplicatable {
    /// Records whose usage carried no `message_id` at all.
    pub without_a_message_id: u64,
    /// Records whose `message_id` arrived after the session had already filled
    /// [`MAX_TRACKED_MESSAGE_IDS`].
    pub past_the_id_cap: u64,
    /// Tokens carried by those records, of every category — the size of the doubt.
    pub tokens: u64,
}

impl Undeduplicatable {
    /// Whether anything at all could not be deduplicated.
    pub fn any(&self) -> bool {
        self.records() > 0
    }

    /// Records summed per record rather than per message.
    pub fn records(&self) -> u64 {
        self.without_a_message_id
            .saturating_add(self.past_the_id_cap)
    }

    fn absorb(&mut self, other: &Self) {
        self.without_a_message_id = self
            .without_a_message_id
            .saturating_add(other.without_a_message_id);
        self.past_the_id_cap = self.past_the_id_cap.saturating_add(other.past_the_id_cap);
        self.tokens = self.tokens.saturating_add(other.tokens);
    }
}

/// What one transcript's billing records add up to, before they are paired with the session's
/// archive identity.
#[derive(Debug, Clone, Default)]
pub struct CostFold {
    /// Deduplicated usage, grouped by what decides its rate.
    pub usage: BTreeMap<BillingKey, TokenTally>,
    /// Transcript records read, malformed ones included — the footer's fold-cost figure.
    pub records_read: u64,
    /// Distinct `message_id`s whose usage was counted.
    pub messages: u64,
    /// Records whose usage repeated a `message_id` already counted, and was therefore *not*
    /// added. The evidence that deduplication did something: on claude-code this is typically
    /// half again as many records as messages.
    pub duplicate_records: u64,
    pub undeduplicatable: Undeduplicatable,
}

/// Folds one transcript's billing records, streaming: one pass, memory bounded by the message-id
/// set and by the handful of billing keys a session uses.
///
/// # Errors
///
/// Returns an error when `artifact_set_version` names an artifact contract this build's
/// interpreter does not support, exactly as [`crate::metrics::fold_transcript`] does.
pub fn fold_cost(
    source: Source,
    artifact_set_version: u16,
    reader: impl BufRead,
) -> Result<CostFold, UnsupportedVersion> {
    fold_cost_tracking(
        source,
        artifact_set_version,
        reader,
        MAX_TRACKED_MESSAGE_IDS,
    )
}

/// The fold itself, with the message-id cap as a parameter.
///
/// The cap is a parameter for one reason: so that a test can exercise **this** code path at a
/// size a test can build. A cap of 100,000 is not reachable by a fixture, and a test that
/// re-implemented the branch at a smaller size would keep passing while the real one rotted —
/// which is precisely what an earlier draft of the cap test did.
fn fold_cost_tracking(
    source: Source,
    artifact_set_version: u16,
    reader: impl BufRead,
    tracked_message_ids: usize,
) -> Result<CostFold, UnsupportedVersion> {
    let stream = TranscriptStream::new(source, artifact_set_version, reader)?;
    let mut fold = CostFold::default();
    let mut seen: HashSet<String> = HashSet::new();
    for item in stream {
        fold.records_read = fold.records_read.saturating_add(1);
        let Ok(record) = &item else { continue };
        let Some(meta) = &record.assistant_meta else {
            continue;
        };
        let Some(usage) = &meta.usage else { continue };
        // Deduplication decides whether this record's usage counts at all; the record is dropped
        // outright when its message has already been counted, because the figures repeat verbatim
        // and adding them again is the 2.6x inflation the module docs describe.
        match &meta.message_id {
            Some(id) if seen.contains(id) => {
                fold.duplicate_records = fold.duplicate_records.saturating_add(1);
                continue;
            }
            Some(id) if seen.len() < tracked_message_ids => {
                seen.insert(id.clone());
                fold.messages = fold.messages.saturating_add(1);
            }
            Some(_) => {
                fold.undeduplicatable.past_the_id_cap =
                    fold.undeduplicatable.past_the_id_cap.saturating_add(1);
                fold.undeduplicatable.tokens = fold
                    .undeduplicatable
                    .tokens
                    .saturating_add(tokens_of(usage));
            }
            None => {
                fold.undeduplicatable.without_a_message_id =
                    fold.undeduplicatable.without_a_message_id.saturating_add(1);
                fold.undeduplicatable.tokens = fold
                    .undeduplicatable
                    .tokens
                    .saturating_add(tokens_of(usage));
            }
        }
        fold.usage
            .entry(BillingKey::of(meta, usage))
            .or_default()
            .observe(usage);
    }
    Ok(fold)
}

/// Every token one message's usage records, for the undeduplicatable tally's magnitude figure.
fn tokens_of(usage: &TokenUsage) -> u64 {
    let mut tally = TokenTally::default();
    tally.observe(usage);
    tally.total()
}

/// One session's billing records, with the archive identity that prices them.
#[derive(Debug, Clone)]
pub struct SessionCost {
    /// The transcript's content hash — the handle for reading the session in full.
    pub source_hash: String,
    /// The harness that produced it, which decides whether it has dollars at all.
    pub source_agent: String,
    /// The repository the archive recorded for this session's latest snapshot, when it recorded
    /// one. `None` is a real archive state, not a lookup failure — a session captured outside a
    /// checkout has no repository — and is reported as its own row rather than merged into
    /// somebody's.
    pub repository: Option<String>,
    /// When the archive finished the snapshot: **archive time**, which is both the clock the
    /// window is cut on and the clock a price row is selected as of.
    pub archived_at: Option<DateTime<Utc>>,
    pub fold: CostFold,
    /// Transcript bytes read, for the footer.
    pub bytes_folded: u64,
}

/// Priced usage for one model, one repository, or the window as a whole.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PricedTokens {
    pub tokens: TokenTally,
    pub dollars: f64,
    /// Messages billed at the fast tier. Reported because it is the ordinary reason a model's
    /// realized rate sits above its base row, and a reader comparing the two would otherwise
    /// suspect the table.
    pub fast_messages: u64,
    /// What this group's cache reads would have cost at the same schedule's *input* rate — what
    /// the prompt would have cost if it had not been cached.
    pub cache_read_at_input_rate: f64,
    /// What they actually cost.
    pub cache_read_dollars: f64,
}

impl PricedTokens {
    fn absorb(&mut self, other: &Self) {
        self.tokens.absorb(&other.tokens);
        self.dollars += other.dollars;
        self.fast_messages += other.fast_messages;
        self.cache_read_at_input_rate += other.cache_read_at_input_rate;
        self.cache_read_dollars += other.cache_read_dollars;
    }

    /// What caching saved this group, in dollars: the difference between reading the tokens back
    /// and sending them again. Never negative — cache reads are a tenth of input everywhere in
    /// the table — but computed rather than assumed, so a future rate that inverted it would show
    /// up as a negative saving instead of as a silently wrong claim.
    pub fn cache_saving(&self) -> f64 {
        self.cache_read_at_input_rate - self.cache_read_dollars
    }
}

/// Usage counted but not priced, with the reason kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flagged {
    /// `<synthetic>` usage: claude-code's own locally-generated messages, which no vendor billed.
    /// Its own line rather than an unpriced reason, because it is a known non-purchase and not a
    /// gap in the price table.
    pub synthetic: TokenTally,
    /// Usage no rate could be selected for, grouped by why.
    pub unpriced: BTreeMap<Unpriced, TokenTally>,
    /// Cache writes stated only as a total, summed across the window.
    pub untiered_cache_writes: u64,
    /// Messages carrying such a write.
    pub untiered_cache_write_messages: u64,
    pub undeduplicatable: Undeduplicatable,
}

impl Flagged {
    /// Whether anything at all is flagged. An empty flagged section is elided rather than printed
    /// as a row of zeroes.
    pub fn any(&self) -> bool {
        self.synthetic.messages > 0
            || !self.unpriced.is_empty()
            || self.untiered_cache_writes > 0
            || self.undeduplicatable.any()
    }
}

/// One session whose whole measured usage was priced at the top tier of the price table on the day
/// the archive took it, and whose size fell under both of [`PremiumFlag`]'s floors.
///
/// Every field is an aggregate or an archive-stated identifier, so this carries the cost lane's
/// redaction line unchanged: the models are the harness's own strings and `source_hash` is the
/// content digest a reader fetches the session with. There is no excerpt, no title, and no
/// repository-shaped narrative here — the *reading* is the numbers, and the transcript behind them
/// is one archive request away for anyone who wants it.
#[derive(Debug, Clone, PartialEq)]
pub struct PremiumSession {
    /// The transcript's content hash — the handle for reading the session in full.
    pub source_hash: String,
    /// When the archive took it, which is also the day its tier was read as of. Always `Some` for
    /// a session that reaches this list — nothing prices without an archive time — and carried as
    /// an option anyway rather than unwrapped, on [`SessionCost::archived_at`]'s own reasoning.
    pub archived_at: Option<DateTime<Utc>>,
    /// The top-tier models it billed under, deduplicated and sorted. More than one only where the
    /// table priced two models identically that day.
    pub models: Vec<String>,
    pub dollars: f64,
    pub output: u64,
    /// Distinct API messages billed — the same quantity the by-model table counts.
    pub messages: u64,
}

/// Sessions the window priced entirely at the day's dearest published rate, and the small ones
/// among them.
///
/// # What "premium" is allowed to mean here
///
/// Exactly [`crate::pricing::is_top_tier_model`]: the model whose published output rate was the
/// highest of any row effective on the session's archive date, plus any model tied with it
/// exactly. No model id is named in this file. A list of "expensive models" would be a second
/// price opinion with no date on it sitting beside a dated table, and it would go stale silently
/// the first time the catalogue moved — which the price table is arranged never to do.
///
/// # Which sessions are eligible at all
///
/// Only claude-code sessions, because only they have dollars and a tier: a Copilot session records
/// output tokens and nothing else and its billing regime is not recoverable from a transcript, so
/// it has no rate to be at the top of ([`BillingSignal`]), and a Codex session records no usage at
/// all. Neither ever appears here, in any window.
///
/// And, among those, only sessions this build could read *whole*: every priced billing key at a
/// top-tier model, and no tokens at all on any key that did not price — no cheaper model beside it,
/// no unpriced model, no token-carrying `<synthetic>` placeholder. That refusal is what makes the
/// three figures below the session's entire measured production rather than a share of it, and it
/// is the same posture as the rest of the lane: a session whose shape this build cannot state in
/// full is one it declines to characterize.
///
/// # It is a reading, not a verdict
///
/// Nothing here scores, ranks against a target, or says a cheaper model would have done. A
/// transcript records what a session cost and how much it wrote; it does not record what the
/// session was worth, and no arithmetic over the first two produces the third. The flag exists so
/// a reader can look at a handful of specific sessions and decide for themselves — which is why it
/// lists them, with the hash to go and read each one, instead of reporting a rate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PremiumFlag {
    /// Sessions that met the eligibility above, whatever their size — the denominator the flagged
    /// count is a share of, without which "three sessions" is a number with no scale.
    pub sessions: usize,
    /// Those of them under both floors, dearest first and then by hash: a deterministic order, so
    /// two runs over one window render the same document.
    pub flagged: Vec<PremiumSession>,
}

impl PremiumFlag {
    /// Whether anything at all is flagged. An empty flag renders nothing — the section is elided,
    /// not printed as a heading over an empty table.
    pub fn any(&self) -> bool {
        !self.flagged.is_empty()
    }

    /// What the flagged sessions cost between them, in the dollars the window's own total already
    /// contains. Summed rather than stored so it cannot disagree with the list it is a total of.
    pub fn dollars(&self) -> f64 {
        self.flagged.iter().map(|session| session.dollars).sum()
    }

    /// Output tokens across the flagged sessions.
    pub fn output(&self) -> u64 {
        self.flagged
            .iter()
            .fold(0, |total, session| total.saturating_add(session.output))
    }

    /// Billed messages across the flagged sessions.
    pub fn messages(&self) -> u64 {
        self.flagged
            .iter()
            .fold(0, |total, session| total.saturating_add(session.messages))
    }
}

/// Copilot's token volumes for one model. No dollars, by construction — see [`BillingSignal`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopilotTokens {
    pub messages: u64,
    pub output: u64,
}

/// The window's cost, folded over its sessions.
#[derive(Debug, Clone, Default)]
pub struct CostTotals {
    /// Priced Anthropic API usage across the window.
    pub priced: PricedTokens,
    /// The same, by the model id the transcript recorded.
    pub by_model: BTreeMap<String, PricedTokens>,
    /// The same, by the repository the archive recorded. `None` keys the sessions with no
    /// repository, which the report names rather than hides.
    pub by_repository: BTreeMap<Option<String>, PricedTokens>,
    /// Copilot output volumes, by the model id the transcript recorded.
    pub copilot: BTreeMap<Option<String>, CopilotTokens>,
    /// claude-code sessions folded.
    pub priceable_sessions: usize,
    /// Copilot sessions folded.
    pub token_only_sessions: usize,
    /// Sessions whose harness records no per-message usage at all, by agent label. Counted so the
    /// report can say a harness contributed nothing rather than leaving it unmentioned.
    pub no_signal_sessions: BTreeMap<String, usize>,
    pub flagged: Flagged,
    /// Sessions priced wholly at the day's top tier, and the small ones among them. An annotation
    /// on the window and never a part of it: nothing in [`PremiumFlag`] moves a dollar or a token
    /// in any figure above, and every dollar it names is already inside
    /// [`CostTotals::priced`] and inside its model's row.
    pub premium: PremiumFlag,
    /// Records read across every folded session.
    pub records_read: u64,
    /// Records dropped as repeats of a message already counted, across every folded session.
    pub duplicate_records: u64,
    pub bytes_folded: u64,
}

impl CostTotals {
    /// Prices and sums the window's sessions.
    ///
    /// Every session is priced at the rates in force **at its own archive time**, so a window
    /// spanning a price change reports each half at what it actually cost rather than the whole
    /// at today's rate.
    pub fn fold(sessions: &[SessionCost]) -> Self {
        let mut totals = Self::default();
        for session in sessions {
            totals.records_read += session.fold.records_read;
            totals.duplicate_records += session.fold.duplicate_records;
            totals.bytes_folded += session.bytes_folded;
            totals
                .flagged
                .undeduplicatable
                .absorb(&session.fold.undeduplicatable);
            match billing_signal(&session.source_agent) {
                BillingSignal::AnthropicApi => totals.absorb_priceable(session),
                BillingSignal::TokensOnly => totals.absorb_token_only(session),
                BillingSignal::NoSignal => {
                    *totals
                        .no_signal_sessions
                        .entry(session.source_agent.clone())
                        .or_default() += 1;
                }
            }
        }
        // Deterministic, and sorted once at the end rather than maintained per session: dearest
        // first, because the reason to read a list of small expensive sessions is to see the
        // expensive ones, and by hash under a tie so two runs over one window agree byte for byte.
        totals.premium.flagged.sort_by(|left, right| {
            right
                .dollars
                .total_cmp(&left.dollars)
                .then_with(|| left.source_hash.cmp(&right.source_hash))
        });
        totals
    }

    /// Folds one claude-code session: price each billing key, then attribute the result to the
    /// window, the model, and the repository.
    fn absorb_priceable(&mut self, session: &SessionCost) {
        self.priceable_sessions += 1;
        let mut reading = PremiumReading::default();
        for (key, tally) in &session.fold.usage {
            match pricing::price_for(
                key.model.as_deref(),
                key.speed.as_deref(),
                key.service_tier.as_deref(),
                key.inference_geo.as_deref(),
                session.archived_at,
            ) {
                Price::Priced {
                    rates,
                    multiplier,
                    fast,
                } => {
                    let priced = PricedTokens {
                        tokens: *tally,
                        dollars: tally.dollars(rates, multiplier),
                        fast_messages: if fast { tally.messages } else { 0 },
                        cache_read_at_input_rate: pricing::dollars(
                            tally.cache_read,
                            rates.input,
                            multiplier,
                        ),
                        cache_read_dollars: pricing::dollars(
                            tally.cache_read,
                            rates.cache_read,
                            multiplier,
                        ),
                    };
                    self.priced.absorb(&priced);
                    let model = key
                        .model
                        .clone()
                        .expect("a priced key names the model it was priced by");
                    reading.observe_priced(&model, session.archived_at, tally, priced.dollars);
                    self.by_model.entry(model).or_default().absorb(&priced);
                    self.by_repository
                        .entry(session.repository.clone())
                        .or_default()
                        .absorb(&priced);
                    // Flagged only for usage that was *otherwise* priced, which is the whole
                    // point of the line: the rest of these messages is in the total and this part
                    // of them is not. Usage that went unpriced for some other reason already has
                    // its own flagged line covering every token in it, and counting it here as
                    // well would list the same tokens twice under two different explanations.
                    if tally.cache_write_untiered > 0 {
                        self.flagged.untiered_cache_writes = self
                            .flagged
                            .untiered_cache_writes
                            .saturating_add(tally.cache_write_untiered);
                        self.flagged.untiered_cache_write_messages = self
                            .flagged
                            .untiered_cache_write_messages
                            .saturating_add(tally.cache_write_untiered_messages);
                    }
                }
                Price::Unbilled => {
                    reading.observe_unpriced(tally);
                    self.flagged.synthetic.absorb(tally);
                }
                Price::Unpriced(reason) => {
                    reading.observe_unpriced(tally);
                    self.flagged
                        .unpriced
                        .entry(reason)
                        .or_default()
                        .absorb(tally);
                }
            }
        }
        if let Some(premium) = reading.settle(&session.source_hash, session.archived_at) {
            self.premium.sessions += 1;
            if premium.output <= PREMIUM_FLAG_MAX_OUTPUT_TOKENS
                && premium.messages <= PREMIUM_FLAG_MAX_MESSAGES
            {
                self.premium.flagged.push(premium);
            }
        }
    }

    /// Folds one Copilot session: output volumes by model, and nothing that looks like money.
    fn absorb_token_only(&mut self, session: &SessionCost) {
        self.token_only_sessions += 1;
        for (key, tally) in &session.fold.usage {
            let entry = self.copilot.entry(key.model.clone()).or_default();
            entry.messages += tally.messages;
            entry.output += tally.output;
        }
    }

    /// Whether any priced session contributed anything at all.
    pub fn priced_anything(&self) -> bool {
        self.priced.tokens.messages > 0
    }
}

/// One session's answer to [`PremiumFlag`]'s eligibility question, accumulated as its billing keys
/// are priced.
///
/// A separate accumulator rather than three locals in [`CostTotals::absorb_priceable`] because the
/// question it answers is a conjunction over *every* key — one cheaper model, or one token on a key
/// that did not price, and the session is not one this build will characterize — and a conjunction
/// spread across a match arm is the kind of thing that quietly becomes a disjunction.
#[derive(Debug)]
struct PremiumReading {
    models: BTreeSet<String>,
    dollars: f64,
    output: u64,
    messages: u64,
    /// Whether every key that priced did so at a top-tier model. Starts `true` — the vacuous truth
    /// a conjunction starts from, which is why this default is written out rather than derived.
    wholly_top_tier: bool,
    /// Whether anything priced at all. A session with no billable usage is not a cheap top-tier
    /// session; it is a session with nothing to read.
    priced_anything: bool,
    /// Tokens on keys that did not price — unpriced models, and `<synthetic>` placeholders. Any at
    /// all and the figures above stop being the whole of what the session produced.
    unpriced_tokens: u64,
}

impl Default for PremiumReading {
    fn default() -> Self {
        Self {
            models: BTreeSet::new(),
            dollars: 0.0,
            output: 0,
            messages: 0,
            wholly_top_tier: true,
            priced_anything: false,
            unpriced_tokens: 0,
        }
    }
}

impl PremiumReading {
    /// Folds one priced billing key.
    fn observe_priced(
        &mut self,
        model: &str,
        archived_at: Option<DateTime<Utc>>,
        tally: &TokenTally,
        dollars: f64,
    ) {
        self.priced_anything = true;
        if archived_at.is_some_and(|at| pricing::is_top_tier_model(model, at)) {
            self.models.insert(model.to_owned());
        } else {
            self.wholly_top_tier = false;
        }
        self.dollars += dollars;
        self.output = self.output.saturating_add(tally.output);
        self.messages = self.messages.saturating_add(tally.messages);
    }

    /// Folds one key that carried no dollars, for either reason.
    fn observe_unpriced(&mut self, tally: &TokenTally) {
        self.unpriced_tokens = self.unpriced_tokens.saturating_add(tally.total());
    }

    /// The session, if this build read the whole of it at the top tier.
    fn settle(
        self,
        source_hash: &str,
        archived_at: Option<DateTime<Utc>>,
    ) -> Option<PremiumSession> {
        (self.wholly_top_tier && self.priced_anything && self.unpriced_tokens == 0).then(|| {
            PremiumSession {
                source_hash: source_hash.to_owned(),
                archived_at,
                models: self.models.into_iter().collect(),
                dollars: self.dollars,
                output: self.output,
                messages: self.messages,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A claude-code assistant record, spelled as the 2.1.x envelope spells one.
    fn claude_record(message_id: &str, model: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"{message_id}-r","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"assistant","id":"{message_id}","model":"{model}","content":[{{"type":"text","text":"working"}}],"usage":{usage}}}}}"#
        )
    }

    /// The usage object claude-code writes today: input, output, the cache total *and* its
    /// per-tier split, and a cache read.
    fn usage(input: u64, output: u64, cache_1h: u64, cache_read: u64) -> String {
        format!(
            r#"{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":{cache_1h},"cache_creation":{{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":{cache_1h}}},"cache_read_input_tokens":{cache_read},"service_tier":"standard"}}"#
        )
    }

    fn fold_claude(transcript: &str) -> CostFold {
        fold_cost(Source::ClaudeCode, 2, transcript.as_bytes()).expect("v2 is supported")
    }

    fn session(fold: CostFold, repository: Option<&str>) -> SessionCost {
        SessionCost {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            repository: repository.map(ToOwned::to_owned),
            archived_at: Some(at("2026-08-01T12:00:00Z")),
            fold,
            bytes_folded: 0,
        }
    }

    /// The rule the whole fold turns on: one API message split across three records repeats its
    /// usage verbatim, and summing records rather than ids inflates the total. The fixture below
    /// is the archive's shape in miniature — three records, one message — and the assertion pins
    /// both numbers so the divergence is a measured 3x rather than a claim.
    #[test]
    fn one_message_split_across_records_is_counted_once() {
        let split = [
            claude_record("msg_1", "claude-opus-5", &usage(100, 40, 1_000, 5_000)),
            claude_record("msg_1", "claude-opus-5", &usage(100, 40, 1_000, 5_000)),
            claude_record("msg_1", "claude-opus-5", &usage(100, 40, 1_000, 5_000)),
        ]
        .join("\n");
        let fold = fold_claude(&split);
        assert_eq!(fold.records_read, 3);
        assert_eq!(fold.messages, 1, "three records, one billed message");
        assert_eq!(fold.duplicate_records, 2);

        let tally = fold.usage.values().next().expect("one billing key");
        assert_eq!(tally.messages, 1);
        assert_eq!(tally.output, 40, "not 120");
        assert_eq!(tally.input, 100);
        assert_eq!(tally.cache_read, 5_000);
        assert_eq!(tally.cache_write_1h, 1_000);

        // And the arithmetic the dedup exists to prevent, stated beside it: had the fold summed
        // records, every figure above would have been three times larger.
        let per_record = 3 * tally.output;
        assert_eq!(per_record, 120);
    }

    /// Distinct messages are distinct, however similar their usage: deduplication keys on the id
    /// and never on the figures, or a session that billed the same amount twice would lose half
    /// its spend.
    #[test]
    fn distinct_message_ids_are_summed_even_with_identical_usage() {
        let transcript = [
            claude_record("msg_1", "claude-opus-5", &usage(100, 40, 0, 0)),
            claude_record("msg_2", "claude-opus-5", &usage(100, 40, 0, 0)),
        ]
        .join("\n");
        let fold = fold_claude(&transcript);
        assert_eq!(fold.messages, 2);
        assert_eq!(fold.duplicate_records, 0);
        assert_eq!(fold.usage.values().next().unwrap().output, 80);
    }

    /// Usage with no id at all cannot be deduplicated, so it is summed per record — the
    /// over-counting direction, because dropping it would silently lose real spend — and every
    /// record of it is flagged.
    #[test]
    fn usage_without_a_message_id_is_summed_per_record_and_flagged() {
        let anonymous = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let fold = fold_claude(&[anonymous, anonymous].join("\n"));
        assert_eq!(fold.messages, 0, "nothing could be counted as a message");
        assert_eq!(fold.duplicate_records, 0);
        assert_eq!(fold.undeduplicatable.without_a_message_id, 2);
        assert_eq!(fold.undeduplicatable.records(), 2);
        assert_eq!(fold.undeduplicatable.tokens, 30);
        assert_eq!(
            fold.usage.values().next().unwrap().output,
            10,
            "both records were summed rather than one of them dropped",
        );
    }

    /// Cache writes are priced from the tiers, so a message whose total disagrees with its
    /// buckets is folded at the buckets — the archive holds exactly this shape, a total of 0
    /// against a 1-hour bucket of 2,277, and the bucket is what bills.
    #[test]
    fn the_per_tier_buckets_win_when_they_disagree_with_the_total() {
        let disagreeing = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","id":"msg_1","model":"claude-opus-5","content":[{"type":"text","text":"x"}],"usage":{"cache_creation_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":2277}}}}"#;
        let tally = *fold_claude(disagreeing)
            .usage
            .values()
            .next()
            .expect("one key");
        assert_eq!(tally.cache_write_1h, 2_277);
        assert_eq!(tally.cache_write_5m, 0);
        assert_eq!(
            tally.cache_write_untiered, 0,
            "the total is not consulted once a bucket is present",
        );

        // Priced at the 1-hour rate, which is 2x input and 1.6x the 5-minute one. Reading the
        // total instead would have charged this message nothing at all.
        let priced = CostTotals::fold(&[session(fold_claude(disagreeing), None)]);
        let expected = pricing::dollars(2_277, 10.00, 1.0);
        assert!(
            (priced.priced.dollars - expected).abs() < 1e-12,
            "{priced:?}"
        );
        assert!(priced.priced.dollars > 0.0);
    }

    /// The other half of the same rule: a write stated *only* as a total has no tier, so it has
    /// no rate. It is carried, reported, and flagged — never charged at an assumed tier, which
    /// would under-bill claude-code by 1.6x given that it writes to the 1-hour cache exclusively.
    #[test]
    fn a_cache_write_with_no_tier_is_flagged_rather_than_priced() {
        let untiered = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","id":"msg_1","model":"claude-opus-5","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":10,"cache_creation_input_tokens":4096}}}"#;
        let fold = fold_claude(untiered);
        let tally = *fold.usage.values().next().expect("one key");
        assert_eq!(tally.cache_write_untiered, 4_096);
        assert_eq!(tally.cache_write_priceable(), 0);

        let totals = CostTotals::fold(&[session(fold, None)]);
        assert_eq!(totals.flagged.untiered_cache_writes, 4_096);
        assert_eq!(totals.flagged.untiered_cache_write_messages, 1);
        // The input tokens on the same message are still priced: the flag is about the write,
        // not about the message.
        let expected = pricing::dollars(10, 5.00, 1.0);
        assert!(
            (totals.priced.dollars - expected).abs() < 1e-12,
            "{totals:?}"
        );
    }

    /// The shape an earlier draft dropped on the floor: buckets *present and both zero* beside a
    /// non-zero total. An empty split prices nothing, so trusting it would have billed 40,960 real
    /// cache-write tokens at $0 and raised no flag at all — a silent loss, which is worse than
    /// either an over-charge or a stated gap. Present-and-zero says no more about the tier than
    /// absent does, so it is treated identically.
    #[test]
    fn a_zero_split_beside_a_non_zero_total_is_untiered_rather_than_free() {
        let zero_split = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","id":"msg_1","model":"claude-opus-5","content":[{"type":"text","text":"x"}],"usage":{"cache_creation_input_tokens":40960,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}}}"#;
        let tally = *fold_claude(zero_split)
            .usage
            .values()
            .next()
            .expect("one key");
        assert_eq!(tally.cache_write_untiered, 40_960);
        assert_eq!(tally.cache_write_priceable(), 0);

        let totals = CostTotals::fold(&[session(fold_claude(zero_split), None)]);
        assert_eq!(totals.flagged.untiered_cache_writes, 40_960);
        assert_eq!(totals.flagged.untiered_cache_write_messages, 1);
        assert_eq!(totals.priced.dollars, 0.0, "nothing here has a rate");
        assert!(totals.flagged.any(), "and the gap is stated, not swallowed");
    }

    /// The flagged line counts the messages *carrying* an untiered write, not every message that
    /// happened to bill under the same key. Three messages, one of them writing an untiered
    /// cache: the report must say one.
    #[test]
    fn the_untiered_write_flag_counts_the_messages_that_carry_one() {
        let plain = |id: &str| {
            claude_record(
                id,
                "claude-opus-5",
                r#"{"input_tokens":10,"output_tokens":5}"#,
            )
        };
        let carrying = claude_record(
            "msg_3",
            "claude-opus-5",
            r#"{"input_tokens":10,"cache_creation_input_tokens":4096}"#,
        );
        let fold = fold_claude(&[plain("msg_1"), plain("msg_2"), carrying].join("\n"));
        let tally = *fold.usage.values().next().expect("one billing key");
        assert_eq!(tally.messages, 3);
        assert_eq!(tally.cache_write_untiered_messages, 1);

        let totals = CostTotals::fold(&[session(fold, None)]);
        assert_eq!(totals.flagged.untiered_cache_write_messages, 1);
        assert_eq!(totals.flagged.untiered_cache_writes, 4_096);
    }

    /// Usage that went unpriced for some other reason already has a flagged line covering every
    /// token in it, so its untiered writes are not listed a second time under a different
    /// explanation. The tokens are reported once, not twice.
    #[test]
    fn an_unpriced_keys_untiered_write_is_not_listed_twice() {
        let unknown_model = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","id":"msg_1","model":"claude-opus-9","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":10,"cache_creation_input_tokens":4096}}}"#;
        let totals = CostTotals::fold(&[session(fold_claude(unknown_model), None)]);
        assert_eq!(
            totals.flagged.untiered_cache_writes, 0,
            "the unpriced-model line already accounts for these tokens",
        );
        assert_eq!(
            totals.flagged.unpriced[&Unpriced::UnknownModel("claude-opus-9".to_owned())].total(),
            4_106,
        );
    }

    /// A zero total with no buckets is not an unpriced write — there is nothing to price.
    #[test]
    fn a_zero_cache_write_raises_no_flag() {
        let none_written = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","id":"msg_1","model":"claude-opus-5","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":10,"cache_creation_input_tokens":0}}}"#;
        let totals = CostTotals::fold(&[session(fold_claude(none_written), None)]);
        assert_eq!(totals.flagged.untiered_cache_writes, 0);
        assert!(!totals.flagged.any(), "{:?}", totals.flagged);
    }

    /// The window aggregate: two models, two repositories, one total, and a cache saving that is
    /// the difference between reading tokens back and sending them again.
    #[test]
    fn the_window_is_grouped_by_model_and_by_repository() {
        let opus = session(
            fold_claude(&claude_record(
                "msg_1",
                "claude-opus-5",
                &usage(1_000_000, 0, 0, 1_000_000),
            )),
            Some("surdy/qanungo"),
        );
        let sonnet = session(
            fold_claude(&claude_record(
                "msg_2",
                "claude-sonnet-5",
                &usage(0, 1_000_000, 0, 0),
            )),
            None,
        );
        let totals = CostTotals::fold(&[opus, sonnet]);
        assert_eq!(totals.priceable_sessions, 2);

        // Opus 5: $5 of input plus $0.50 of cache read. Sonnet 5: $10 of output.
        let opus_dollars = totals.by_model["claude-opus-5"].dollars;
        assert!((opus_dollars - 5.50).abs() < 1e-9, "{opus_dollars}");
        assert!((totals.by_model["claude-sonnet-5"].dollars - 10.00).abs() < 1e-9);
        assert!((totals.priced.dollars - 15.50).abs() < 1e-9);

        // The repository cut splits the same money, and the session with none is its own row
        // rather than being merged into the named one.
        assert!(
            (totals.by_repository[&Some("surdy/qanungo".to_owned())].dollars - 5.50).abs() < 1e-9
        );
        assert!((totals.by_repository[&None].dollars - 10.00).abs() < 1e-9);

        // A million cached tokens read back at $0.50 rather than sent again at $5.00.
        assert!((totals.priced.cache_read_dollars - 0.50).abs() < 1e-9);
        assert!((totals.priced.cache_read_at_input_rate - 5.00).abs() < 1e-9);
        assert!((totals.priced.cache_saving() - 4.50).abs() < 1e-9);
    }

    /// Fable 5.1 bills its own cache read, not its predecessor's. The two models share every
    /// other column, so a fold that resolved 5.1 through 5's row would look right in input and
    /// output and be wrong by 4x on the one figure that dominates a claude-code session: reads.
    /// The fixture is the real archive's shape in miniature — almost all of the tokens are cache
    /// reads — so the divergence is a measured number rather than a claim.
    #[test]
    fn fable_5_1_reads_its_cache_at_its_own_rate_and_not_fable_5s() {
        let session_on = |model: &str| SessionCost {
            archived_at: Some(at("2026-09-05T00:00:00Z")),
            ..session(
                fold_claude(&claude_record("msg_1", model, &usage(0, 0, 0, 4_000_000))),
                None,
            )
        };

        let newer = CostTotals::fold(&[session_on("claude-fable-5-1")]);
        assert_eq!(newer.priceable_sessions, 1);
        assert!(
            newer.flagged.unpriced.is_empty(),
            "{:?}",
            newer.flagged.unpriced,
        );
        // Four million tokens read back at $0.25 per million, not at $1.00.
        assert!((newer.priced.cache_read_dollars - 1.00).abs() < 1e-9);
        assert!((newer.priced.dollars - 1.00).abs() < 1e-9);
        assert!((newer.by_model["claude-fable-5-1"].dollars - 1.00).abs() < 1e-9);
        // The saving is measured against 5.1's own input rate, which it shares with Fable 5.
        assert!((newer.priced.cache_read_at_input_rate - 40.00).abs() < 1e-9);
        assert!((newer.priced.cache_saving() - 39.00).abs() < 1e-9);

        // The same tokens on the predecessor, so the 4x is stated and not assumed.
        let older = CostTotals::fold(&[session_on("claude-fable-5")]);
        assert!((older.priced.cache_read_dollars - 4.00).abs() < 1e-9);
        assert!(
            (older.priced.cache_read_dollars - 4.0 * newer.priced.cache_read_dollars).abs() < 1e-9,
            "reading 5.1's cache at 5's rate would have billed four times over",
        );
    }

    /// Synthetic messages are counted in tokens and excluded from dollars, and an unpriced model
    /// is a different flag from either — a report that merged the three would be unable to say
    /// whether a gap was a placeholder, a missing row, or a bug.
    #[test]
    fn synthetic_and_unpriced_usage_stay_out_of_the_total_and_out_of_each_other() {
        let synthetic = fold_claude(&claude_record("msg_1", "<synthetic>", &usage(0, 500, 0, 0)));
        let unknown = fold_claude(&claude_record(
            "msg_2",
            "claude-opus-9",
            &usage(0, 700, 0, 0),
        ));
        let totals = CostTotals::fold(&[session(synthetic, None), session(unknown, None)]);
        assert_eq!(totals.priced.dollars, 0.0);
        assert!(!totals.priced_anything());
        assert_eq!(totals.flagged.synthetic.output, 500);
        assert_eq!(totals.flagged.synthetic.messages, 1);
        assert_eq!(
            totals.flagged.unpriced[&Unpriced::UnknownModel("claude-opus-9".to_owned())].output,
            700,
        );
        assert!(totals.flagged.any());
    }

    /// Copilot records one output figure per message and nothing else, so it gets volumes and no
    /// dollars — not an estimate, not a credit equivalent, nothing.
    #[test]
    fn copilot_sessions_contribute_tokens_and_never_dollars() {
        let transcript = concat!(
            r#"{"type":"assistant.message","timestamp":"2026-08-01T10:00:00.000Z","data":{"content":"one","messageId":"m1","model":"claude-opus-4.8","outputTokens":128}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-08-01T10:01:00.000Z","data":{"content":"two","messageId":"m2","model":"claude-opus-4.8","outputTokens":64}}"#,
        );
        let fold = fold_cost(Source::Copilot, 2, transcript.as_bytes()).unwrap();
        assert_eq!(fold.messages, 2);
        let copilot = SessionCost {
            source_agent: "copilot-cli".to_owned(),
            ..session(fold, Some("surdy/munshi"))
        };
        let totals = CostTotals::fold(&[copilot]);
        assert_eq!(totals.token_only_sessions, 1);
        assert_eq!(totals.priced.dollars, 0.0);
        assert!(totals.by_model.is_empty());
        assert!(totals.by_repository.is_empty());
        let volumes = totals.copilot[&Some("claude-opus-4.8".to_owned())];
        assert_eq!(volumes.messages, 2);
        assert_eq!(volumes.output, 192);
    }

    /// Codex records no per-message usage at all, so its sessions are named as contributing
    /// nothing rather than quietly absent.
    #[test]
    fn a_harness_with_no_usage_signal_is_counted_rather_than_ignored() {
        assert_eq!(billing_signal("claude-code"), BillingSignal::AnthropicApi);
        assert_eq!(billing_signal("copilot-cli"), BillingSignal::TokensOnly);
        assert_eq!(billing_signal("codex-cli"), BillingSignal::NoSignal);
        assert_eq!(billing_signal("future-harness"), BillingSignal::NoSignal);

        let codex = SessionCost {
            source_agent: "codex-cli".to_owned(),
            ..session(CostFold::default(), None)
        };
        let totals = CostTotals::fold(&[codex]);
        assert_eq!(totals.no_signal_sessions["codex-cli"], 1);
        assert_eq!(totals.priceable_sessions, 0);
    }

    /// A session is priced at the rates in force when the archive took it, so one window spanning
    /// a model's launch prices the halves differently rather than applying today's row to all of
    /// it.
    #[test]
    fn each_session_is_priced_as_of_its_own_archive_time() {
        let before = SessionCost {
            archived_at: Some(at("2026-07-01T00:00:00Z")),
            ..session(
                fold_claude(&claude_record(
                    "msg_1",
                    "claude-opus-5",
                    &usage(0, 1_000_000, 0, 0),
                )),
                None,
            )
        };
        let after = SessionCost {
            archived_at: Some(at("2026-08-01T00:00:00Z")),
            ..session(
                fold_claude(&claude_record(
                    "msg_2",
                    "claude-opus-5",
                    &usage(0, 1_000_000, 0, 0),
                )),
                None,
            )
        };
        let totals = CostTotals::fold(&[before, after]);
        // Opus 5 launched 2026-07-24: only the later session has a rate.
        assert!((totals.priced.dollars - 25.00).abs() < 1e-9, "{totals:?}");
        assert_eq!(
            totals.flagged.unpriced[&Unpriced::NoRateYet("claude-opus-5".to_owned())].output,
            1_000_000,
        );
    }

    /// The cap under-claims deduplication rather than dropping spend: once the id set is full a
    /// new id cannot be tracked, so its record is summed per record and flagged, exactly like a
    /// record with no id at all.
    ///
    /// Driven through [`fold_cost_tracking`] at a cap of two rather than re-implementing the
    /// branch beside it. A test that rebuilt the logic would keep passing while the real one
    /// rotted, which is the whole failure mode a bound like this invites: it is unreachable by any
    /// fixture at its shipped size, so the only way to pin it is to shrink it.
    #[test]
    fn the_message_id_cap_flags_rather_than_drops() {
        let transcript = [
            claude_record("msg_1", "claude-opus-5", &usage(0, 10, 0, 0)),
            claude_record("msg_2", "claude-opus-5", &usage(0, 20, 0, 0)),
            // The set is full: this id can never be remembered, so its record cannot be
            // deduplicated against anything.
            claude_record("msg_3", "claude-opus-5", &usage(0, 40, 0, 0)),
            // Nor can its repeat, which is therefore counted a second time rather than dropped.
            claude_record("msg_3", "claude-opus-5", &usage(0, 40, 0, 0)),
            // An id already in the set still deduplicates normally past the cap.
            claude_record("msg_1", "claude-opus-5", &usage(0, 10, 0, 0)),
        ]
        .join("\n");
        let fold = fold_cost_tracking(Source::ClaudeCode, 2, transcript.as_bytes(), 2)
            .expect("v2 is supported");

        assert_eq!(fold.records_read, 5);
        assert_eq!(fold.messages, 2, "only two ids fit");
        assert_eq!(
            fold.duplicate_records, 1,
            "the tracked id's repeat is still dropped",
        );
        assert_eq!(fold.undeduplicatable.past_the_id_cap, 2);
        assert_eq!(fold.undeduplicatable.without_a_message_id, 0);
        assert_eq!(fold.undeduplicatable.tokens, 80);
        assert!(fold.undeduplicatable.any());
        // 10 + 20 for the tracked ids, then 40 twice for the one that could not be tracked: the
        // over-count is real, and it is exactly what the flag above is warning about.
        assert_eq!(fold.usage.values().next().unwrap().output, 110);

        // The same transcript under the shipped cap deduplicates everything and flags nothing.
        let uncapped = fold_claude(&transcript);
        assert_eq!(uncapped.messages, 3);
        assert_eq!(uncapped.duplicate_records, 2);
        assert!(!uncapped.undeduplicatable.any());
        assert_eq!(uncapped.usage.values().next().unwrap().output, 70);

        // And the real cap is far above any session the archive holds — 29,591 message ids
        // across all 623 of them, measured 2026-08-23.
        const { assert!(MAX_TRACKED_MESSAGE_IDS >= 29_591) };
    }

    // -----------------------------------------------------------------------
    // The top-tier flag
    // -----------------------------------------------------------------------

    /// A session whose whole usage is one small billing key, at `model`, archived at `archived_at`.
    fn small_session(hash: &str, model: &str, archived_at: &str, output: u64) -> SessionCost {
        SessionCost {
            source_hash: hash.to_owned(),
            archived_at: Some(at(archived_at)),
            ..session(
                fold_claude(&claude_record(
                    "msg_1",
                    model,
                    &format!(r#"{{"output_tokens":{output}}}"#),
                )),
                None,
            )
        }
    }

    /// The flag's whole premise: "premium" is the table's own top tier on the session's own date,
    /// never a list of models. Fable 5 at $50 of output is the dearest row today, so a small
    /// session on it is listed and an equally small one on Opus 5 at $25 is not — and the second
    /// half of that is the point, because Opus is the model somebody would have put on a list.
    #[test]
    fn only_the_days_dearest_published_model_is_read_as_premium() {
        let totals = CostTotals::fold(&[
            small_session(
                &"a".repeat(64),
                "claude-fable-5",
                "2026-08-01T00:00:00Z",
                500,
            ),
            small_session(
                &"b".repeat(64),
                "claude-opus-5",
                "2026-08-01T00:00:00Z",
                500,
            ),
            small_session(
                &"c".repeat(64),
                "claude-sonnet-5",
                "2026-08-01T00:00:00Z",
                500,
            ),
        ]);
        assert_eq!(totals.premium.sessions, 1, "{:?}", totals.premium);
        assert_eq!(totals.premium.flagged.len(), 1);
        assert_eq!(totals.premium.flagged[0].source_hash, "a".repeat(64));
        assert_eq!(totals.premium.flagged[0].models, vec!["claude-fable-5"]);
        assert_eq!(totals.premium.flagged[0].output, 500);
        assert_eq!(totals.premium.flagged[0].messages, 1);
        // $50/MTok on 500 output tokens, and the same dollars the by-model row carries.
        let expected = pricing::dollars(500, 50.00, 1.0);
        assert!((totals.premium.flagged[0].dollars - expected).abs() < 1e-12);
        assert!((totals.by_model["claude-fable-5"].dollars - expected).abs() < 1e-12);
    }

    /// The tier is read as of the session's **own** archive time, so the same model and the same
    /// tokens flag on one side of a launch and not on the other. Before Fable 5 existed, Opus 4.8
    /// was the dearest row in the table and a small session on it was a small session at the top
    /// tier; from the launch instant it is not, and nothing in this build was edited to say so.
    #[test]
    fn the_tier_moves_with_the_table_rather_than_with_the_build() {
        let before = CostTotals::fold(&[small_session(
            &"a".repeat(64),
            "claude-opus-4-8",
            "2026-06-08T23:59:59Z",
            500,
        )]);
        assert_eq!(before.premium.flagged.len(), 1, "{:?}", before.premium);

        let after = CostTotals::fold(&[small_session(
            &"a".repeat(64),
            "claude-opus-4-8",
            "2026-06-09T00:00:00Z",
            500,
        )]);
        assert_eq!(after.premium.sessions, 0, "{:?}", after.premium);
        assert!(!after.premium.any());
        // And the money is untouched by which side of the boundary it fell on: the flag annotates
        // the window, it does not price it.
        assert!((before.priced.dollars - after.priced.dollars).abs() < 1e-12);
    }

    /// Both floors, and both directions of each. A session over either one is not listed, and a
    /// session at exactly either one is — the floors are inclusive, which is the reading the
    /// report's own sentence ("at most") states.
    #[test]
    fn a_session_over_either_floor_is_not_listed() {
        let output = |tokens: u64| {
            CostTotals::fold(&[small_session(
                &"a".repeat(64),
                "claude-fable-5",
                "2026-08-01T00:00:00Z",
                tokens,
            )])
        };
        assert_eq!(
            output(PREMIUM_FLAG_MAX_OUTPUT_TOKENS).premium.flagged.len(),
            1
        );
        let over = output(PREMIUM_FLAG_MAX_OUTPUT_TOKENS + 1);
        assert_eq!(over.premium.flagged.len(), 0);
        assert_eq!(
            over.premium.sessions, 1,
            "still a top-tier session, and still counted as the denominator it is",
        );

        // The message floor, exercised on its own: tiny output spread across too many messages.
        let messages = |count: u64| {
            let transcript = (0..count)
                .map(|index| {
                    claude_record(
                        &format!("msg_{index}"),
                        "claude-fable-5",
                        r#"{"output_tokens":1}"#,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            CostTotals::fold(&[SessionCost {
                archived_at: Some(at("2026-08-01T00:00:00Z")),
                ..session(fold_claude(&transcript), None)
            }])
        };
        assert_eq!(messages(PREMIUM_FLAG_MAX_MESSAGES).premium.flagged.len(), 1);
        let chatty = messages(PREMIUM_FLAG_MAX_MESSAGES + 1);
        assert_eq!(chatty.premium.flagged.len(), 0);
        assert_eq!(chatty.premium.sessions, 1);
    }

    /// Copilot has no rate and therefore no tier, and Codex records no usage at all: neither can
    /// reach the list in any window, however small its sessions are. The lane's standing rule,
    /// pinned here rather than left to the fact that `absorb_priceable` happens not to be called
    /// for them.
    #[test]
    fn copilot_and_codex_sessions_never_reach_the_top_tier_list() {
        let copilot = SessionCost {
            source_agent: "copilot-cli".to_owned(),
            fold: fold_cost(
                Source::Copilot,
                2,
                r#"{"type":"assistant.message","timestamp":"2026-08-01T10:00:00.000Z","data":{"content":"one","messageId":"m1","model":"claude-fable-5","outputTokens":100}}"#.as_bytes(),
            )
            .unwrap(),
            ..small_session(&"a".repeat(64), "claude-fable-5", "2026-08-01T00:00:00Z", 100)
        };
        let codex = SessionCost {
            source_agent: "codex-cli".to_owned(),
            fold: CostFold::default(),
            ..small_session(
                &"b".repeat(64),
                "claude-fable-5",
                "2026-08-01T00:00:00Z",
                100,
            )
        };
        let totals = CostTotals::fold(&[copilot, codex]);
        assert_eq!(totals.premium.sessions, 0, "{:?}", totals.premium);
        assert!(!totals.premium.any());
    }

    /// Only sessions this build read *whole*. A cheaper model beside the dear one, a model with no
    /// rate in force on the day, and a token-bearing `<synthetic>` placeholder each leave a session
    /// out — because the three figures the list would print for it would be a share of that session
    /// rather than all of it. A zero-token placeholder takes nothing away, so it does not.
    #[test]
    fn a_session_this_build_could_not_read_whole_is_not_characterized() {
        let two_models = SessionCost {
            archived_at: Some(at("2026-08-01T00:00:00Z")),
            ..session(
                fold_claude(
                    &[
                        claude_record("msg_1", "claude-fable-5", r#"{"output_tokens":100}"#),
                        claude_record("msg_2", "claude-opus-5", r#"{"output_tokens":100}"#),
                    ]
                    .join("\n"),
                ),
                None,
            )
        };
        assert_eq!(CostTotals::fold(&[two_models]).premium.sessions, 0);

        // Fable 5.1 has a row, but not one effective on this session's archive date: a session
        // older than its model's first price is unpriced just as squarely as an unknown model is,
        // and it takes the reading down the same way.
        let with_unpriced = SessionCost {
            archived_at: Some(at("2026-08-01T00:00:00Z")),
            ..session(
                fold_claude(
                    &[
                        claude_record("msg_1", "claude-fable-5", r#"{"output_tokens":100}"#),
                        claude_record("msg_2", "claude-fable-5-1", r#"{"output_tokens":100}"#),
                    ]
                    .join("\n"),
                ),
                None,
            )
        };
        assert_eq!(CostTotals::fold(&[with_unpriced]).premium.sessions, 0);

        let with_synthetic = |output: u64| SessionCost {
            archived_at: Some(at("2026-08-01T00:00:00Z")),
            ..session(
                fold_claude(
                    &[
                        claude_record("msg_1", "claude-fable-5", r#"{"output_tokens":100}"#),
                        claude_record(
                            "msg_2",
                            pricing::SYNTHETIC_MODEL,
                            &format!(r#"{{"output_tokens":{output}}}"#),
                        ),
                    ]
                    .join("\n"),
                ),
                None,
            )
        };
        assert_eq!(
            CostTotals::fold(&[with_synthetic(0)]).premium.flagged.len(),
            1,
            "a placeholder that carried no tokens took nothing away from the reading",
        );
        assert_eq!(
            CostTotals::fold(&[with_synthetic(50)]).premium.sessions,
            0,
            "one that carried tokens did",
        );
    }

    /// Two models tied at the top of the table share the tier, and a session that used both is
    /// still a session read whole — it names both. Built against a date on which the shipped table
    /// has no tie, by pricing usage that resolves to the same top rate through the fast tier.
    #[test]
    fn a_fast_tier_session_is_still_a_session_on_its_own_model() {
        // Opus 5's fast tier bills at Fable 5's base rate, but the model it bills is still Opus 5,
        // whose *published* row is $25 — so the day's top tier is Fable's and this is not on it.
        let fast = SessionCost {
            archived_at: Some(at("2026-08-01T00:00:00Z")),
            ..session(
                fold_claude(&claude_record(
                    "msg_1",
                    "claude-opus-5",
                    r#"{"output_tokens":100,"speed":"fast"}"#,
                )),
                None,
            )
        };
        let totals = CostTotals::fold(&[fast]);
        assert_eq!(
            totals.premium.sessions, 0,
            "a tier is not a model: what ranks is the row the catalogue publishes",
        );
        assert_eq!(totals.priced.fast_messages, 1, "and it still billed fast");
    }

    /// The order is deterministic and dearest-first, so two runs over one window render the same
    /// document. Ties on dollars fall back to the hash, which is the only other thing a listed
    /// session carries that cannot collide.
    #[test]
    fn the_listed_sessions_are_ordered_dearest_first_then_by_hash() {
        let totals = CostTotals::fold(&[
            small_session(
                &"c".repeat(64),
                "claude-fable-5",
                "2026-08-01T00:00:00Z",
                100,
            ),
            small_session(
                &"a".repeat(64),
                "claude-fable-5",
                "2026-08-01T00:00:00Z",
                900,
            ),
            small_session(
                &"b".repeat(64),
                "claude-fable-5",
                "2026-08-01T00:00:00Z",
                100,
            ),
        ]);
        let order: Vec<&str> = totals
            .premium
            .flagged
            .iter()
            .map(|session| &session.source_hash[..1])
            .collect();
        assert_eq!(order, vec!["a", "b", "c"]);

        // The totals are sums over the whole list, so a render that caps the table still states
        // what every listed session came to.
        assert_eq!(totals.premium.output(), 1_100);
        assert_eq!(totals.premium.messages(), 3);
        let expected = pricing::dollars(1_100, 50.00, 1.0);
        assert!((totals.premium.dollars() - expected).abs() < 1e-12);
    }

    /// The flag annotates and never moves a number: the window's dollars, tokens, by-model, and
    /// by-repository cuts are identical whether or not a session in it happened to be small.
    #[test]
    fn the_flag_moves_no_dollar_and_no_token_in_the_window() {
        let sessions = [
            small_session(
                &"a".repeat(64),
                "claude-fable-5",
                "2026-08-01T00:00:00Z",
                100,
            ),
            small_session(
                &"b".repeat(64),
                "claude-opus-5",
                "2026-08-01T00:00:00Z",
                100,
            ),
        ];
        let totals = CostTotals::fold(&sessions);
        assert!(totals.premium.any());

        let flagged_dollars = totals.premium.dollars();
        assert!(flagged_dollars > 0.0);
        assert!(
            totals.priced.dollars > flagged_dollars,
            "the flagged session's dollars are inside the total, not the whole of it",
        );
        assert_eq!(totals.priced.tokens.output, 200);
        assert_eq!(totals.priced.tokens.messages, 2);
        assert_eq!(totals.by_model.len(), 2);
        assert!(
            !totals.flagged.any(),
            "and nothing here is an unpriced flag"
        );
    }

    /// Token sums saturate rather than wrapping: these are counts read from somebody else's file,
    /// and an absurd number a reader can see beats a small one they cannot.
    #[test]
    fn token_sums_saturate_rather_than_wrapping() {
        let mut tally = TokenTally {
            output: u64::MAX - 1,
            ..TokenTally::default()
        };
        tally.absorb(&TokenTally {
            output: 10,
            input: 5,
            ..TokenTally::default()
        });
        assert_eq!(tally.output, u64::MAX);
        assert_eq!(tally.input, 5);
        assert_eq!(tally.total(), u64::MAX);
    }

    /// Thinking tokens are already inside `output`, so they are reported and never added.
    #[test]
    fn thinking_tokens_are_context_rather_than_a_category() {
        let thinking = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","id":"msg_1","model":"claude-opus-5","content":[{"type":"text","text":"x"}],"usage":{"output_tokens":1000,"output_tokens_details":{"thinking_tokens":600}}}}"#;
        let tally = *fold_claude(thinking).usage.values().next().unwrap();
        assert_eq!(tally.output, 1_000);
        assert_eq!(tally.thinking, 600);
        assert_eq!(
            tally.total(),
            1_000,
            "thinking is not a category of its own"
        );

        let totals = CostTotals::fold(&[session(fold_claude(thinking), None)]);
        let expected = pricing::dollars(1_000, 25.00, 1.0);
        assert!((totals.priced.dollars - expected).abs() < 1e-12);
    }
}
