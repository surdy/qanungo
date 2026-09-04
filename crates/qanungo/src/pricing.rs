//! The price table: a static, date-versioned statement of what a token cost, and when.
//!
//! Every figure here comes from `docs/pricing-sources-2026-08-23.md`, which is committed beside
//! the code precisely so that a dollar in a report can be traced to a published page and a
//! retrieval date rather than to somebody's recollection of one. A row's doc comment names the
//! section of that file it came from; a row with no source does not belong in the table.
//!
//! # Dollars are claimed for Anthropic API usage only
//!
//! A claude-code session talks to the Anthropic API, whose per-token list prices are published,
//! so its tokens can be priced. A Copilot session cannot be: its billing is premium requests
//! before 2026-06-01 and GitHub AI Credits after, a transcript does not say which regime the
//! account was on, and it records no per-message input or cache figures to price even if it did.
//! [`crate::cost`] therefore reports Copilot in tokens and never in money — see the research
//! file's §2 for the whole argument.
//!
//! Even for claude-code these are **list** prices. The archive knows what was sent and to which
//! model; it does not know the account's plan, its committed-use discount, or whether the request
//! was batched. The report says "at Anthropic API list prices" everywhere it prints a total, and
//! that qualifier is load-bearing rather than decorative.
//!
//! # No row, no dollars
//!
//! A model is priced at the rates **effective at the session's archive timestamp**, taking the
//! latest row whose `effective_from` is at or before it. A model this table has never heard of,
//! or a session archived before its model's first row, is *unpriced*: its tokens are reported and
//! its dollars are not, and the report names it in the flagged section. Nothing here interpolates
//! between rows, extrapolates a family resemblance, or falls back to a "similar" model. A missing
//! price is a fact about this table; a guessed one would be a lie about the bill.
//!
//! # Modifiers
//!
//! Three usage fields change the rate rather than the token count, and all three are read
//! verbatim from what the transcript recorded:
//!
//! - `speed` — fast mode bills the same model at a distinct, higher tier ([`Rates::fast`]). A
//!   `fast` reading on a model with no fast row, or any spelling neither this build nor the
//!   research file recognizes, leaves the usage unpriced rather than priced at the base rate.
//! - `inference_geo` — [`NO_GEO_ROUTING_INFERENCE_GEO`] (`not_available`), and the field being
//!   absent, both mean the base rate: the API is stating that no geo-routing premium applied.
//!   `us` is a flat [`US_GEO_MULTIPLIER`] on every category, **but only for the models the
//!   research file scopes it to** (it records the premium as "Claude 4.6+"). That makes it a
//!   per-row property, [`PriceRow::us_geo_multiplier`], and not a global constant: this table
//!   holds a pre-4.6 model, and charging Haiku 4.5 a premium the source never documents would be
//!   inventing a rate — the one thing this module exists not to do. A `us` reading against a row
//!   with no multiplier, and any region this build does not recognize at all, leave the usage
//!   unpriced.
//! - `service_tier` — anything other than `standard` (or absent) is priced by a different
//!   schedule (batch at 0.5x, priority at a premium), and this table holds none of them, so such
//!   usage is unpriced.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

/// The revision of this table, stamped into the cost report's footer.
///
/// It is the retrieval date of the research file the rows came from, not a build date: two cost
/// reports are comparable in dollars only when this matches, exactly as two coaching reports are
/// comparable in scores only when the rule-pack stamp does.
pub const PRICE_TABLE_REVISION: &str = "2026-08-23";

/// Claude Code's placeholder `model` for messages it generated locally — an interrupt notice, an
/// error stub — which no vendor ever billed. Its tokens are real and are reported; its dollars
/// are zero by construction rather than by a missing price row, and the report keeps the two
/// cases apart.
pub const SYNTHETIC_MODEL: &str = "<synthetic>";

/// The `speed` reading that selects [`Rates::fast`].
pub const FAST_SPEED: &str = "fast";
/// The `speed` reading that means the ordinary tier, alongside the field being absent.
pub const STANDARD_SPEED: &str = "standard";
/// The `service_tier` reading this table's rates are the rates for, alongside the field being
/// absent.
pub const STANDARD_SERVICE_TIER: &str = "standard";
/// The `inference_geo` reading that carries [`US_GEO_MULTIPLIER`].
pub const US_INFERENCE_GEO: &str = "us";

/// The `inference_geo` reading that means **no geo routing applied**, and therefore the base
/// rate — not an unknown region.
///
/// **Measured on the archive 2026-08-23**, and then confirmed by the cost lane's first production
/// run: of the 61,184 claude usage records, 61,122 read `not_available` and the remaining 62
/// record no readable value at all. `us` never occurs. This is the field's ordinary state rather
/// than an edge case — it is the API saying the request was not geo-routed — so reading it as an
/// unrecognized region flagged **100%** of 311 sessions, 35,001 messages and 8.2B tokens as
/// unpriced, which is how the misreading was found.
///
/// Distinguishing it from an absent field would be a distinction without a rate: both say no
/// premium applied, and neither selects [`PriceRow::us_geo_multiplier`], so a pre-4.6 row prices
/// normally under either. A region this build has never heard of is still flagged — that rule is
/// unchanged, and is what keeps a genuinely new routing premium from being silently priced at
/// base.
pub const NO_GEO_ROUTING_INFERENCE_GEO: &str = "not_available";

/// US-only inference bills 1.1x on every token category, **for Claude 4.6 and later** (research
/// file §1, "Other modifiers": `inference_geo "us" = 1.1x (Claude 4.6+)`).
///
/// A multiplier rather than a second set of rows because the source documents it as exactly that.
/// Which rows may carry it is [`PriceRow::us_geo_multiplier`]'s business, because the qualifier in
/// that source line is part of the fact and not decoration.
pub const US_GEO_MULTIPLIER: f64 = 1.1;

/// Tokens per unit of the published rates. Anthropic quotes dollars per million tokens (MTok),
/// and the table stores those figures verbatim so a reader can check a row against the pricing
/// page without arithmetic.
pub const TOKENS_PER_RATE_UNIT: f64 = 1_000_000.0;

/// What one million tokens cost, per category, in US dollars.
///
/// The three cache figures are stored rather than derived from `input`, even though the research
/// file records them as fixed multiples of it (5m write 1.25x, 1h write 2x, read 0.1x). Deriving
/// them would bake today's multipliers into every past row, and a multiplier change would then
/// silently re-price history — which is the same failure the `effective_from` column exists to
/// prevent. Stored figures make a multiplier change a new row like any other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    /// Writing to the 5-minute cache tier.
    pub cache_write_5m: f64,
    /// Writing to the 1-hour cache tier.
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// One model's price, from one date onward.
#[derive(Debug, Clone, Copy)]
pub struct PriceRow {
    /// The model id **as the archive spells it**. Harnesses spell the same model differently
    /// (`claude-opus-4-8` in Claude Code, `claude-opus-4.8` in Copilot) and `munshi-transcript`
    /// passes the spelling through untouched, so this table matches on the verbatim string and
    /// never on a normalized family name. A spelling that is not here is unpriced, which is the
    /// correct answer for a model nobody has confirmed a rate for.
    pub model: &'static str,
    /// The first day these rates applied, as `(year, month, day)` in UTC.
    ///
    /// A tuple rather than a `NaiveDate` because the table is a `const` and the constructor is
    /// not; [`PriceRow::effective_from`] builds the date, and
    /// [`tests::every_row_states_a_real_date`] proves every row's tuple is one.
    pub effective_from: (i32, u32, u32),
    pub rates: Rates,
    /// The fast-mode tier, for the models that have one. `None` means this build has no fast rate
    /// for the model — usage recording `speed = "fast"` against it is then unpriced rather than
    /// priced at [`PriceRow::rates`], because fast mode demonstrably bills more.
    pub fast: Option<Rates>,
    /// The US-only inference premium, for the models the research file documents it for.
    ///
    /// `Some(`[`US_GEO_MULTIPLIER`]`)` on every Claude 4.6+ row; `None` on Haiku 4.5, which
    /// predates the qualifier the source attaches to that figure. A `None` row that meets
    /// `inference_geo = "us"` yields [`Unpriced::InferenceGeo`] rather than being priced at the
    /// base rate *or* at a premium nobody published: both of those would be a claim about a bill
    /// that this table's provenance does not support, and the second would be the more expensive
    /// mistake. If a source is later found that settles the pre-4.6 case, it becomes a value here
    /// and a line in the research file, not a code change.
    pub us_geo_multiplier: Option<f64>,
}

impl PriceRow {
    /// The instant these rates took effect: midnight UTC on the stated day.
    ///
    /// UTC because the archive timestamps a session in UTC and nothing in the capture chain
    /// records a local offset (see [`crate::report`]'s note on the same question). A session
    /// archived within a day of a price change can therefore land on either side of it, which is
    /// a smaller error than inventing a timezone for it.
    ///
    /// # Archive time can postdate the conversation
    ///
    /// A row is selected as of when the archive *took* the snapshot, not when the session ran, and
    /// the two are not always the same day: a backfill batch archives months of transcripts within
    /// minutes of each other, and this archive holds exactly that shape. It is nonetheless the
    /// honest basis, because archive time is the lane's uniform clock — the one every window is cut
    /// on ([`crate::command`]) and the only one every snapshot is guaranteed to carry. It is also
    /// harmless in practice today: no model in this table has ever been repriced, so every row is a
    /// model's only row and the selection cannot differ between the two dates. The day a second row
    /// lands for one model, a backfilled session could be priced at the newer rate — which is a
    /// known consequence of the clock, stated here rather than discovered later.
    pub fn effective_from(&self) -> DateTime<Utc> {
        let (year, month, day) = self.effective_from;
        let date = NaiveDate::from_ymd_opt(year, month, day)
            .expect("every price row states a real calendar date");
        Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight is a real time"))
    }
}

/// The fast-mode tier shared by Opus 4.8 and Opus 5: $10 in / $50 out, with the cache multipliers
/// stacking on that base (research file §1, "Fast mode").
const OPUS_FAST: Rates = Rates {
    input: 10.00,
    output: 50.00,
    cache_write_5m: 12.50,
    cache_write_1h: 20.00,
    cache_read: 1.00,
};

/// Every price this build knows, newest-model-last. Order is irrelevant to lookup — [`rate_for`]
/// selects on `effective_from` — and is kept as the research file lists them so the two read the
/// same way side by side.
///
/// All five rows are launch prices: the research file found no post-launch change for any of
/// them, and records the one price event in the window (Sonnet 5's scheduled 2026-09-01 rise,
/// cancelled on 2026-08-10) as a change that will not happen. `<synthetic>` is deliberately
/// absent — it is not a model and is excluded from dollars by [`SYNTHETIC_MODEL`], not priced at
/// zero, so that a genuinely unpriced model can never be mistaken for a free one.
pub const PRICES: &[PriceRow] = &[
    // Claude Haiku 4.5, launched 2025-10-15 at $1/$5 (research file §1).
    PriceRow {
        model: "claude-haiku-4-5-20251001",
        effective_from: (2025, 10, 15),
        rates: Rates {
            input: 1.00,
            output: 5.00,
            cache_write_5m: 1.25,
            cache_write_1h: 2.00,
            cache_read: 0.10,
        },
        fast: None,
        // Pre-4.6, and the research file scopes the US premium to "Claude 4.6+". No source, no
        // multiplier: `inference_geo = "us"` against this row is flagged, not charged.
        us_geo_multiplier: None,
    },
    // Claude Opus 4.8, launched 2026-05-28 at $5/$25, with a fast tier (research file §1).
    PriceRow {
        model: "claude-opus-4-8",
        effective_from: (2026, 5, 28),
        rates: Rates {
            input: 5.00,
            output: 25.00,
            cache_write_5m: 6.25,
            cache_write_1h: 10.00,
            cache_read: 0.50,
        },
        fast: Some(OPUS_FAST),
        us_geo_multiplier: Some(US_GEO_MULTIPLIER),
    },
    // Claude Fable 5, launched 2026-06-09 at $10/$50 (research file §1). The 2026-06-12 →
    // 2026-06-30 availability gap was an export-control review, not a price change, so it is not
    // a row: a session archived inside it simply has no usage to price.
    PriceRow {
        model: "claude-fable-5",
        effective_from: (2026, 6, 9),
        rates: Rates {
            input: 10.00,
            output: 50.00,
            cache_write_5m: 12.50,
            cache_write_1h: 20.00,
            cache_read: 1.00,
        },
        fast: None,
        us_geo_multiplier: Some(US_GEO_MULTIPLIER),
    },
    // Claude Sonnet 5, launched 2026-06-30 at $2/$10 (research file §1). The introductory price
    // was made permanent on 2026-08-10, so one row covers all of its usage, past and future.
    PriceRow {
        model: "claude-sonnet-5",
        effective_from: (2026, 6, 30),
        rates: Rates {
            input: 2.00,
            output: 10.00,
            cache_write_5m: 2.50,
            cache_write_1h: 4.00,
            cache_read: 0.20,
        },
        fast: None,
        us_geo_multiplier: Some(US_GEO_MULTIPLIER),
    },
    // Claude Opus 5, launched 2026-07-24 at $5/$25, with the same fast tier as Opus 4.8
    // (research file §1).
    PriceRow {
        model: "claude-opus-5",
        effective_from: (2026, 7, 24),
        rates: Rates {
            input: 5.00,
            output: 25.00,
            cache_write_5m: 6.25,
            cache_write_1h: 10.00,
            cache_read: 0.50,
        },
        fast: Some(OPUS_FAST),
        us_geo_multiplier: Some(US_GEO_MULTIPLIER),
    },
];

/// Why a usage figure carries no dollars. Each variant is reported with a count, never folded
/// into a zero.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unpriced {
    /// The record recorded token counts but no model, so nothing selects a rate.
    NoModel,
    /// The model is spelled in a way this table has no row for at all.
    UnknownModel(String),
    /// The model has rows, but none effective as early as this session's archive time.
    NoRateYet(String),
    /// The session's archive timestamp could not be read, so no row can be selected as of it.
    /// Such a session is already unplaceable in a window and never reaches the fold; the case is
    /// carried anyway so the pricing function is total rather than panicking on a shape the
    /// caller happens to filter out today.
    NoArchiveTime,
    /// `service_tier` recorded something other than `standard`. Batch and priority bill on
    /// schedules this table does not hold.
    ServiceTier(String),
    /// `speed` recorded a mode this build has no rate for — either an unrecognized spelling, or
    /// `fast` on a model with no fast row.
    Speed(String),
    /// `inference_geo` recorded a region this build has no multiplier for.
    InferenceGeo(String),
}

impl Unpriced {
    /// A short reason line for the report's flagged section.
    ///
    /// `clamp` is applied to the archive-stated value before it is interpolated, and is not
    /// optional: every variant that names one names a string a peer chose, and a rendering
    /// surface decides what it will print of it. Passing the clamp in rather than applying it
    /// afterwards is what makes that unskippable — there is no version of this string with the
    /// raw value in it for a caller to forget to clean up. See [`crate::cost_report::identifier`].
    pub fn detail(&self, clamp: fn(&str) -> String) -> String {
        match self {
            Self::NoModel => "recorded token usage but no model".to_owned(),
            Self::UnknownModel(model) => format!("no price row for model `{}`", clamp(model)),
            Self::NoRateYet(model) => format!(
                "model `{}` has no price row effective at this session's archive time",
                clamp(model),
            ),
            Self::NoArchiveTime => {
                "the archive stated a completion time this build could not read".to_owned()
            }
            Self::ServiceTier(tier) => {
                format!("service tier `{}` bills on another schedule", clamp(tier))
            }
            Self::Speed(speed) => format!("no rate for serving mode `{}`", clamp(speed)),
            Self::InferenceGeo(geo) => {
                format!("no rate multiplier for inference region `{}`", clamp(geo))
            }
        }
    }
}

/// The rates that apply to one message, or why none do.
#[derive(Debug, Clone, PartialEq)]
pub enum Price {
    /// A rate schedule and the flat multiplier to apply to every category of it.
    Priced {
        rates: Rates,
        multiplier: f64,
        /// Whether [`Rates::fast`] was selected — reported so a reader can see that fast mode is
        /// what a model's average rate is high for.
        fast: bool,
    },
    /// Recorded against [`SYNTHETIC_MODEL`]: real tokens, no vendor, no dollars.
    Unbilled,
    Unpriced(Unpriced),
}

/// What a message's usage is priced at, given the model, the modifiers, and when the session was
/// archived.
///
/// The order of the checks is the order the report reads best in, and only one reason is
/// returned: a message with both an unknown model and an odd service tier is reported under the
/// model, because that is the fact worth acting on.
pub fn price_for(
    model: Option<&str>,
    speed: Option<&str>,
    service_tier: Option<&str>,
    inference_geo: Option<&str>,
    archived_at: Option<DateTime<Utc>>,
) -> Price {
    let Some(model) = model else {
        return Price::Unpriced(Unpriced::NoModel);
    };
    if model == SYNTHETIC_MODEL {
        return Price::Unbilled;
    }
    let Some(archived_at) = archived_at else {
        return Price::Unpriced(Unpriced::NoArchiveTime);
    };
    let Some(row) = rate_for(model, archived_at) else {
        return Price::Unpriced(if PRICES.iter().any(|row| row.model == model) {
            Unpriced::NoRateYet(model.to_owned())
        } else {
            Unpriced::UnknownModel(model.to_owned())
        });
    };
    if let Some(tier) = service_tier
        && tier != STANDARD_SERVICE_TIER
    {
        return Price::Unpriced(Unpriced::ServiceTier(tier.to_owned()));
    }
    let (rates, fast) = match speed {
        None => (row.rates, false),
        Some(STANDARD_SPEED) => (row.rates, false),
        Some(FAST_SPEED) => match row.fast {
            Some(fast) => (fast, true),
            None => return Price::Unpriced(Unpriced::Speed(FAST_SPEED.to_owned())),
        },
        Some(other) => return Price::Unpriced(Unpriced::Speed(other.to_owned())),
    };
    // The region premium is the row's to grant: the research file records it for Claude 4.6+, so
    // a row predating that qualifier has no documented rate for US-only inference at all, and
    // neither the base rate nor an undocumented premium would be an honest answer for it.
    let multiplier = match inference_geo {
        // No geo routing, stated or unstated: the base rate, for every row.
        None | Some(NO_GEO_ROUTING_INFERENCE_GEO) => 1.0,
        Some(US_INFERENCE_GEO) => match row.us_geo_multiplier {
            Some(multiplier) => multiplier,
            None => return Price::Unpriced(Unpriced::InferenceGeo(US_INFERENCE_GEO.to_owned())),
        },
        Some(other) => return Price::Unpriced(Unpriced::InferenceGeo(other.to_owned())),
    };
    Price::Priced {
        rates,
        multiplier,
        fast,
    }
}

/// The row in force for `model` at `at`: the latest one whose `effective_from` is at or before
/// it. `None` when the table holds no such row, which is either an unknown model or a session
/// older than its model's first price.
pub fn rate_for(model: &str, at: DateTime<Utc>) -> Option<&'static PriceRow> {
    PRICES
        .iter()
        .filter(|row| row.model == model && row.effective_from() <= at)
        .max_by_key(|row| row.effective_from())
}

/// The highest **output** rate any model in this table was published at on `at`, or `None` when no
/// row was effective yet.
///
/// This is the table's own answer to "what was the expensive end of the catalogue that day", and
/// it is derived rather than listed: no model id appears in the code that reads it, so a new row
/// moves the top tier without anything being edited beside it. That is the whole reason the flag
/// in [`crate::cost`] asks the table instead of carrying a list of "premium" models — a list would
/// be a second, undated price opinion sitting beside a dated one.
///
/// # Which rate column ranks
///
/// Output, because it is the rate a model is published and compared on, and because a session's
/// dollars are dominated by what it wrote. Nothing else in the table is consulted for the
/// ordering, and [`tests::the_rate_columns_do_not_disagree_about_which_tier_is_dearer`] pins the
/// only assumption that choice rests on: across every row, ordering by output agrees with ordering
/// by input. The day a row breaks that, the test fails and the column becomes a decision somebody
/// has to make on purpose rather than one this function has already made for them.
///
/// # Fast tiers are not rows in the ranking
///
/// [`Rates::fast`] is the same model billed differently, not a second entry in the catalogue, so
/// it is not compared here — otherwise a model would outrank its own base row and the day's
/// dearest *model* would come back as not-top-tier. A fast-mode session still bills on a top-tier
/// model if its model is one; what tier it ran at is a separate fact the report states separately.
pub fn top_tier_output_rate(at: DateTime<Utc>) -> Option<f64> {
    PRICES
        .iter()
        // Resolved per model rather than read off each row, so a repriced model contributes the
        // rate in force on the day and not every rate it has ever had.
        .filter_map(|row| rate_for(row.model, at))
        .map(|row| row.rates.output)
        .max_by(f64::total_cmp)
}

/// Whether `model` was one of the table's dearest models on `at`.
///
/// Ties are exact and are all top tier: two models published at the same output rate are the same
/// tier. The machinery exists for a future tie; **today's table has none** — the Opus fast schedule
/// equals Fable 5's base rates, but fast tiers do not rank ([`top_tier_output_rate`]), so no two
/// *models* share the top. A rate that is merely *close* to the top is not a tie and is not top
/// tier either — a near-miss band would be this build inventing a threshold the pricing page does
/// not state, which is the one thing [`crate::pricing`] exists not to do.
///
/// A model with no row effective on `at` — unknown, or older than its first price — is not top
/// tier, because it has no published rate to compare at all.
pub fn is_top_tier_model(model: &str, at: DateTime<Utc>) -> bool {
    match (rate_for(model, at), top_tier_output_rate(at)) {
        (Some(row), Some(top)) => row.rates.output.total_cmp(&top) == std::cmp::Ordering::Equal,
        _ => false,
    }
}

/// Dollars for `tokens` at `rate` dollars per million, scaled by a modifier multiplier.
pub fn dollars(tokens: u64, rate: f64, multiplier: f64) -> f64 {
    tokens as f64 / TOKENS_PER_RATE_UNIT * rate * multiplier
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// The dates are stored as tuples so the table can be a `const`, which means nothing but a
    /// test stops `(2026, 13, 40)` from reaching a report as a panic.
    #[test]
    fn every_row_states_a_real_date() {
        for row in PRICES {
            let effective = row.effective_from();
            assert_eq!(
                effective.format("%H:%M:%S").to_string(),
                "00:00:00",
                "{} is not effective from midnight",
                row.model,
            );
        }
    }

    /// Every rate is a positive number of dollars, and the cache rates keep the published
    /// ordering: a 1-hour write costs more than a 5-minute one, which costs more than the input
    /// it caches, which costs far more than reading it back. A transposed pair of columns would
    /// otherwise be invisible until somebody wondered why cache reads were expensive.
    #[test]
    fn every_rate_is_positive_and_ordered_as_published() {
        for row in PRICES {
            for rates in [Some(row.rates), row.fast].into_iter().flatten() {
                for rate in [
                    rates.input,
                    rates.output,
                    rates.cache_write_5m,
                    rates.cache_write_1h,
                    rates.cache_read,
                ] {
                    assert!(rate > 0.0, "{} priced a category at {rate}", row.model);
                }
                assert!(rates.output > rates.input, "{}", row.model);
                assert!(rates.cache_write_1h > rates.cache_write_5m, "{}", row.model);
                assert!(rates.cache_write_5m > rates.input, "{}", row.model);
                assert!(rates.cache_read < rates.input, "{}", row.model);
            }
        }
    }

    /// The whole point of the `effective_from` column: a session is priced at what its model cost
    /// *then*. A session archived the day before a model existed is unpriced, not free and not
    /// priced at the launch rate.
    #[test]
    fn a_row_is_selected_by_the_sessions_archive_time() {
        let launch = at("2026-07-24T00:00:00Z");
        assert_eq!(
            rate_for("claude-opus-5", launch).map(|row| row.rates.input),
            Some(5.00),
            "the launch instant itself is inside the row",
        );
        assert_eq!(
            rate_for("claude-opus-5", launch + chrono::TimeDelta::days(20))
                .map(|row| row.rates.output),
            Some(25.00),
        );
        assert!(
            rate_for("claude-opus-5", launch - chrono::TimeDelta::seconds(1)).is_none(),
            "a second before the model existed there is no rate to charge",
        );
    }

    /// Two rows for one model resolve to the later one, which is what makes a future price change
    /// a row rather than an edit. Built here rather than added to the shipped table, which
    /// currently holds no model with two prices.
    #[test]
    fn the_latest_effective_row_wins_when_a_model_is_repriced() {
        let latest = |rows: &[PriceRow], at: DateTime<Utc>| {
            rows.iter()
                .filter(|row| row.effective_from() <= at)
                .max_by_key(|row| row.effective_from())
                .map(|row| row.rates.input)
        };
        let repriced = [
            PriceRow {
                model: "example",
                effective_from: (2026, 1, 1),
                rates: Rates {
                    input: 1.0,
                    output: 2.0,
                    cache_write_5m: 1.25,
                    cache_write_1h: 2.0,
                    cache_read: 0.1,
                },
                fast: None,
                us_geo_multiplier: None,
            },
            PriceRow {
                model: "example",
                effective_from: (2026, 6, 1),
                rates: Rates {
                    input: 3.0,
                    output: 6.0,
                    cache_write_5m: 3.75,
                    cache_write_1h: 6.0,
                    cache_read: 0.3,
                },
                fast: None,
                us_geo_multiplier: None,
            },
        ];
        assert_eq!(latest(&repriced, at("2026-03-01T00:00:00Z")), Some(1.0));
        assert_eq!(latest(&repriced, at("2026-06-01T00:00:00Z")), Some(3.0));
        assert_eq!(latest(&repriced, at("2026-09-01T00:00:00Z")), Some(3.0));
    }

    /// Fast mode is a different tier, not a surcharge, and a model without one is not quietly
    /// billed at its base rate.
    #[test]
    fn fast_mode_selects_the_fast_tier_where_one_exists() {
        let priced = price_for(
            Some("claude-opus-5"),
            Some(FAST_SPEED),
            None,
            None,
            Some(at("2026-08-01T00:00:00Z")),
        );
        let Price::Priced { rates, fast, .. } = priced else {
            panic!("opus 5 has a fast tier: {priced:?}");
        };
        assert!(fast);
        assert_eq!(rates.input, 10.00);
        assert_eq!(rates.output, 50.00);

        // The same model at the ordinary tier, and with the field absent, is the base rate.
        for speed in [None, Some(STANDARD_SPEED)] {
            let base = price_for(
                Some("claude-opus-5"),
                speed,
                None,
                None,
                Some(at("2026-08-01T00:00:00Z")),
            );
            assert!(
                matches!(base, Price::Priced { rates, fast: false, .. } if rates.input == 5.00),
                "{base:?}",
            );
        }

        // A model with no fast row is unpriced rather than charged the base rate.
        assert_eq!(
            price_for(
                Some("claude-sonnet-5"),
                Some(FAST_SPEED),
                None,
                None,
                Some(at("2026-08-01T00:00:00Z")),
            ),
            Price::Unpriced(Unpriced::Speed(FAST_SPEED.to_owned())),
        );
    }

    /// The archive's universal reading. `not_available` is the API saying no geo routing applied,
    /// so it prices at base on every row — including the pre-4.6 one, which has no US premium to
    /// decline. Treating it as an unknown region flagged the entire first production run.
    #[test]
    fn no_geo_routing_prices_at_base_on_every_row() {
        let when = Some(at("2026-08-01T00:00:00Z"));
        for model in [
            "claude-opus-5",
            "claude-sonnet-5",
            // Pre-4.6, and so carrying no US premium at all: "no routing applied" is still the
            // base rate for it, because there is no premium in question.
            "claude-haiku-4-5-20251001",
        ] {
            for geo in [None, Some(NO_GEO_ROUTING_INFERENCE_GEO)] {
                let priced = price_for(Some(model), None, None, geo, when);
                assert!(
                    matches!(priced, Price::Priced { multiplier, .. } if multiplier == 1.0),
                    "{model} at geo {geo:?}: {priced:?}",
                );
            }
        }
        // Absent and `not_available` are the same claim, so they must price identically.
        assert_eq!(
            price_for(Some("claude-opus-5"), None, None, None, when),
            price_for(
                Some("claude-opus-5"),
                None,
                None,
                Some(NO_GEO_ROUTING_INFERENCE_GEO),
                when,
            ),
        );
    }

    #[test]
    fn us_inference_multiplies_every_category_and_an_unknown_region_prices_nothing() {
        let us = price_for(
            Some("claude-sonnet-5"),
            None,
            None,
            Some(US_INFERENCE_GEO),
            Some(at("2026-08-01T00:00:00Z")),
        );
        assert!(
            matches!(us, Price::Priced { multiplier, .. } if multiplier == US_GEO_MULTIPLIER),
            "{us:?}",
        );
        assert!((dollars(1_000_000, 2.00, US_GEO_MULTIPLIER) - 2.20).abs() < 1e-9);

        // A region this build has never heard of is still flagged, which is what keeps a new
        // routing premium from being silently priced at base the way `not_available` now is.
        for unknown in ["moon", "eu", "apac", "NOT_AVAILABLE"] {
            assert_eq!(
                price_for(
                    Some("claude-sonnet-5"),
                    None,
                    None,
                    Some(unknown),
                    Some(at("2026-08-01T00:00:00Z")),
                ),
                Price::Unpriced(Unpriced::InferenceGeo(unknown.to_owned())),
                "{unknown}",
            );
        }
    }

    /// The premium is scoped by its own source line — "Claude 4.6+" — so the one pre-4.6 row in
    /// this table does not get it. Neither the base rate nor an undocumented 1.1x is an honest
    /// answer for US-only inference on Haiku 4.5, so it is flagged instead, and the same model
    /// with no region recorded still prices normally.
    #[test]
    fn a_model_the_research_does_not_scope_the_region_premium_to_is_not_charged_one() {
        let haiku = "claude-haiku-4-5-20251001";
        let when = Some(at("2026-08-01T00:00:00Z"));
        assert_eq!(
            price_for(Some(haiku), None, None, Some(US_INFERENCE_GEO), when),
            Price::Unpriced(Unpriced::InferenceGeo(US_INFERENCE_GEO.to_owned())),
        );
        for ordinary in [None, Some(NO_GEO_ROUTING_INFERENCE_GEO)] {
            assert!(
                matches!(
                    price_for(Some(haiku), None, None, ordinary, when),
                    Price::Priced { multiplier, .. } if multiplier == 1.0,
                ),
                "a row with no region premium still prices un-routed usage: {ordinary:?}",
            );
        }

        // The scoping is a property of the row, not of this test: exactly the rows the research
        // file calls 4.6+ carry the multiplier.
        for row in PRICES {
            let expected = row.model != haiku;
            assert_eq!(
                row.us_geo_multiplier.is_some(),
                expected,
                "{} carries the wrong region-premium scoping",
                row.model,
            );
        }
    }

    /// `<synthetic>` is claude-code's own placeholder, not a model: its tokens are reported and
    /// its dollars are absent by construction. Distinct from an unpriced model, because one is a
    /// known non-purchase and the other is a gap in this table.
    #[test]
    fn synthetic_messages_are_unbilled_rather_than_unpriced() {
        assert_eq!(
            price_for(
                Some(SYNTHETIC_MODEL),
                None,
                None,
                None,
                Some(at("2026-08-01T00:00:00Z")),
            ),
            Price::Unbilled,
        );
        assert!(!PRICES.iter().any(|row| row.model == SYNTHETIC_MODEL));
    }

    /// The fallthrough that must never become a guess: an unrecognized model, a session older
    /// than its model, a batch tier, and a missing model each end in their own flagged reason.
    #[test]
    fn anything_the_table_cannot_price_says_which_thing_it_could_not_price() {
        let now = Some(at("2026-08-01T00:00:00Z"));
        assert_eq!(
            price_for(Some("gpt-5.6-sol"), None, None, None, now),
            Price::Unpriced(Unpriced::UnknownModel("gpt-5.6-sol".to_owned())),
        );
        assert_eq!(
            price_for(
                Some("claude-opus-5"),
                None,
                None,
                None,
                Some(at("2026-01-01T00:00:00Z")),
            ),
            Price::Unpriced(Unpriced::NoRateYet("claude-opus-5".to_owned())),
        );
        assert_eq!(
            price_for(Some("claude-opus-5"), None, Some("batch"), None, now),
            Price::Unpriced(Unpriced::ServiceTier("batch".to_owned())),
        );
        assert_eq!(
            price_for(None, None, None, None, now),
            Price::Unpriced(Unpriced::NoModel),
        );
        assert_eq!(
            price_for(Some("claude-opus-5"), None, None, None, None),
            Price::Unpriced(Unpriced::NoArchiveTime),
        );
        // And the ordinary case, so the four refusals above are not the only outcome available.
        assert!(matches!(
            price_for(Some("claude-opus-5"), None, Some("standard"), None, now),
            Price::Priced { .. },
        ));
    }

    /// The top tier is read off the table as of a date, so it *moves* — and the boundary is the
    /// same midnight [`rate_for`] selects on. Before Fable 5 existed the dearest model in the
    /// catalogue was Opus 4.8 at $25 of output; from its launch instant the top is $50, and Opus
    /// 4.8 stops being top tier that same instant without anything in this build being edited.
    #[test]
    fn the_top_tier_is_whatever_the_table_says_it_was_that_day() {
        let launch = at("2026-06-09T00:00:00Z");
        let before = launch - chrono::TimeDelta::seconds(1);
        assert_eq!(top_tier_output_rate(before), Some(25.00));
        assert!(is_top_tier_model("claude-opus-4-8", before));
        assert!(
            !is_top_tier_model("claude-fable-5", before),
            "not yet priced"
        );

        assert_eq!(top_tier_output_rate(launch), Some(50.00));
        assert!(is_top_tier_model("claude-fable-5", launch));
        assert!(
            !is_top_tier_model("claude-opus-4-8", launch),
            "a model does not stay top tier because it used to be",
        );

        // Nothing at all is priced before the table's first row, so there is no top tier to name —
        // which is a different answer from "everything is".
        assert_eq!(top_tier_output_rate(at("2020-01-01T00:00:00Z")), None);
        assert!(!is_top_tier_model(
            "claude-opus-5",
            at("2020-01-01T00:00:00Z")
        ));
    }

    /// A model the table has never heard of is not top tier, and neither is one whose row has not
    /// come into effect yet: both have no published rate to rank at all.
    #[test]
    fn a_model_with_no_effective_row_is_not_top_tier() {
        let now = at("2026-08-01T00:00:00Z");
        assert!(!is_top_tier_model("gpt-5.6-sol", now));
        assert!(!is_top_tier_model(SYNTHETIC_MODEL, now));
        assert!(!is_top_tier_model(
            "claude-opus-5",
            at("2026-01-01T00:00:00Z"),
        ));
        // And the ordinary case beside the three refusals, so the function is not merely saying no.
        assert!(is_top_tier_model("claude-fable-5", now));
        assert!(!is_top_tier_model("claude-sonnet-5", now));
    }

    /// Ties are exact and are shared. Today's table holds no second model at $50 of output, so the
    /// case is built here rather than asserted against the shipped rows — and the near-miss beside
    /// it is the point: $49.99 is not a tie, because a band around the top would be this build
    /// inventing a rate relationship the pricing page never published.
    #[test]
    fn an_exact_tie_shares_the_top_tier_and_a_near_miss_does_not() {
        let top = |rates: &[f64]| {
            rates
                .iter()
                .copied()
                .max_by(f64::total_cmp)
                .expect("a rate to rank")
        };
        let rates = [50.00, 50.00, 49.99, 25.00];
        let ceiling = top(&rates);
        let tied: Vec<bool> = rates
            .iter()
            .map(|rate| rate.total_cmp(&ceiling) == std::cmp::Ordering::Equal)
            .collect();
        assert_eq!(tied, vec![true, true, false, false]);
    }

    /// [`top_tier_output_rate`] ranks on the output column alone. That is only safe while the
    /// table's columns agree about which schedule is dearer, so the agreement is pinned rather
    /// than assumed: a future row that charged more for input and less for output would make the
    /// choice of column load-bearing, and this test is what turns that into a decision somebody
    /// makes on purpose instead of one the ranking has quietly already made.
    #[test]
    fn the_rate_columns_do_not_disagree_about_which_tier_is_dearer() {
        for left in PRICES {
            for right in PRICES {
                let (output, input) = (
                    left.rates.output.total_cmp(&right.rates.output),
                    left.rates.input.total_cmp(&right.rates.input),
                );
                assert_eq!(
                    output, input,
                    "{} and {} rank differently by output than by input",
                    left.model, right.model,
                );
            }
        }
    }

    #[test]
    fn dollars_are_per_million_tokens() {
        assert!((dollars(1_000_000, 5.00, 1.0) - 5.00).abs() < 1e-9);
        assert!((dollars(250_000, 20.00, 1.0) - 5.00).abs() < 1e-9);
        assert_eq!(dollars(0, 50.00, 1.0), 0.0);
    }
}
