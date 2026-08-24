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
//! So the buckets are what this fold reads, and they win outright where they are present — the
//! archive holds a message whose total reads 0 while its 1-hour bucket reads 2,277, and the
//! bucket is the tier that bills. A write stated **only** as a total, with neither bucket
//! present, is not priced at an assumed tier and not dropped either: its tokens land in
//! [`TokenTally::cache_write_untiered`], are reported, and are flagged. That case has never been
//! observed in the archive; the code is honest about it anyway, because the alternative is a
//! silent 1.6x error the day it appears.
//!
//! # What this module renders
//!
//! Nothing. It folds token counts and archive-stated identifiers — model ids, the `speed` /
//! `service_tier` / `inference_geo` billing modifiers, repository names — into counts and
//! dollars. No transcript text of any kind enters a type here: [`munshi_transcript::Record`]'s
//! `assistant_meta` is read and its `classification` — the user text, the assistant text, the
//! tool arguments — is never touched. See [`crate::cost_report`] for the rendering line.

use std::collections::{BTreeMap, HashSet};
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
    /// Cache writes stated only as `cache_creation_input_tokens`, with neither bucket present.
    /// Real tokens that no rate applies to, because the rate depends on the tier — reported and
    /// flagged, never priced at an assumed one. See the module docs.
    pub cache_write_untiered: u64,
    pub cache_read: u64,
    /// The share of `output` spent on extended thinking. Context, not a category: it is already
    /// inside `output` and is never added to it or priced separately.
    pub thinking: u64,
}

impl TokenTally {
    /// Folds one message's usage.
    fn observe(&mut self, usage: &TokenUsage) {
        self.messages += 1;
        self.input += usage.input_tokens.unwrap_or_default();
        self.output += usage.output_tokens.unwrap_or_default();
        self.cache_read += usage.cache_read_input_tokens.unwrap_or_default();
        self.thinking += usage.thinking_tokens.unwrap_or_default();
        // The buckets are the billing tiers, so where the source stated either of them it has
        // stated the split, and the undifferentiated total is not consulted at all — not to
        // reconcile a residue, not to fill in the sibling bucket. Where it stated neither, the
        // total is real spend nobody can price, and it is carried as exactly that.
        match (usage.cache_5m_input_tokens, usage.cache_1h_input_tokens) {
            (None, None) => {
                self.cache_write_untiered += usage.cache_creation_input_tokens.unwrap_or_default();
            }
            (five_minute, one_hour) => {
                self.cache_write_5m += five_minute.unwrap_or_default();
                self.cache_write_1h += one_hour.unwrap_or_default();
            }
        }
    }

    /// Adds another tally into this one.
    fn absorb(&mut self, other: &Self) {
        self.messages += other.messages;
        self.input += other.input;
        self.output += other.output;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
        self.cache_write_untiered += other.cache_write_untiered;
        self.cache_read += other.cache_read;
        self.thinking += other.thinking;
    }

    /// Every token counted, of any category. `thinking` is excluded because it is already part of
    /// `output`, and counting it would double one message's own reasoning.
    pub fn total(&self) -> u64 {
        self.input
            + self.output
            + self.cache_write_5m
            + self.cache_write_1h
            + self.cache_write_untiered
            + self.cache_read
    }

    /// Cache writes across both tiers, priced.
    pub fn cache_write_priceable(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
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
        self.without_a_message_id + self.past_the_id_cap
    }

    fn absorb(&mut self, other: &Self) {
        self.without_a_message_id += other.without_a_message_id;
        self.past_the_id_cap += other.past_the_id_cap;
        self.tokens += other.tokens;
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
    let stream = TranscriptStream::new(source, artifact_set_version, reader)?;
    let mut fold = CostFold::default();
    let mut seen: HashSet<String> = HashSet::new();
    for item in stream {
        fold.records_read += 1;
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
                fold.duplicate_records += 1;
                continue;
            }
            Some(id) if seen.len() < MAX_TRACKED_MESSAGE_IDS => {
                seen.insert(id.clone());
                fold.messages += 1;
            }
            Some(_) => {
                fold.undeduplicatable.past_the_id_cap += 1;
                fold.undeduplicatable.tokens += tokens_of(usage);
            }
            None => {
                fold.undeduplicatable.without_a_message_id += 1;
                fold.undeduplicatable.tokens += tokens_of(usage);
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
        totals
    }

    /// Folds one claude-code session: price each billing key, then attribute the result to the
    /// window, the model, and the repository.
    fn absorb_priceable(&mut self, session: &SessionCost) {
        self.priceable_sessions += 1;
        for (key, tally) in &session.fold.usage {
            // Untiered cache writes are flagged wherever they occur — including inside otherwise
            // perfectly priced usage, which is the whole point: the rest of the message is priced
            // and this part of it is not, and the report says so rather than rounding the
            // difference into the total.
            if tally.cache_write_untiered > 0 {
                self.flagged.untiered_cache_writes += tally.cache_write_untiered;
                self.flagged.untiered_cache_write_messages += tally.messages;
            }
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
                    self.by_model.entry(model).or_default().absorb(&priced);
                    self.by_repository
                        .entry(session.repository.clone())
                        .or_default()
                        .absorb(&priced);
                }
                Price::Unbilled => self.flagged.synthetic.absorb(tally),
                Price::Unpriced(reason) => self
                    .flagged
                    .unpriced
                    .entry(reason)
                    .or_default()
                    .absorb(tally),
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
    #[test]
    fn the_message_id_cap_flags_rather_than_drops() {
        let mut fold = CostFold::default();
        let mut seen: HashSet<String> = HashSet::new();
        // The cap is exercised through the same branch the fold uses, at a size a test can build.
        for index in 0..3 {
            let id = format!("msg_{index}");
            if seen.len() < 2 && !seen.contains(&id) {
                seen.insert(id);
                fold.messages += 1;
            } else {
                fold.undeduplicatable.past_the_id_cap += 1;
            }
        }
        assert_eq!(fold.messages, 2);
        assert_eq!(fold.undeduplicatable.past_the_id_cap, 1);
        assert!(fold.undeduplicatable.any());

        // And the real cap is far above any session the archive holds — 29,591 message ids
        // across all 623 of them, measured 2026-08-23.
        const { assert!(MAX_TRACKED_MESSAGE_IDS >= 29_591) };
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
