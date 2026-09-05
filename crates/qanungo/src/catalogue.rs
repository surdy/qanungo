//! The generated catalogue: every rule, lane, threshold, price, and pattern name this build
//! decides with, as Markdown.
//!
//! # Why it is generated
//!
//! The rules, their thresholds, the lane mapping, the scoring floors, the price table and the
//! redaction pattern set are all *data* that lives in the source, and every one of them was, until
//! this module, readable only by opening the source. A hand-written catalogue would have been a
//! second copy of that data — and a second copy of a number is a number that will eventually
//! disagree with the one the code uses, silently, in the direction that flatters whoever last
//! edited the prose.
//!
//! So nothing here is typed twice. Every value is rendered from the constant, table, or accessor
//! the runtime reads: [`RuleId::thresholds`] walks [`crate::rules::thresholds`],
//! [`Lane::components`] walks the same signal list the rule-pack digest hashes,
//! [`crate::pricing::PRICES`] is the price table itself, and [`crate::redaction::PATTERNS`] is the
//! scrub's own list. The rendered document therefore cannot drift from the build that rendered it,
//! and the committed `RULES.md` is pinned to it by an equality test — the same idiom the dashboard
//! page uses to prove it computes nothing itself.
//!
//! # It reads no archive
//!
//! `qanungo rules` is the one subcommand with no [`ArchiveArgs`](crate::cli::ArchiveArgs). It
//! describes the build, not a window of history, so requiring `--patwari-url` to print it would
//! mean a person could not read what the tool looks for until they had already finished setting
//! it up — which is exactly backwards for the document that explains the tool.
//!
//! # It is deterministic
//!
//! Nothing here reads the clock, the environment, or the filesystem: two runs of the same binary
//! render the same bytes. That is what makes the equality test a drift gate rather than a flake.
//! The one date-sensitive question the catalogue asks — which models are the table's dearest — is
//! asked as of the **newest `effective_from` in the table itself** ([`price_table_as_of`]) rather
//! than as of today, for the same reason.
//!
//! # What it deliberately does not print
//!
//! Pattern *names*, never patterns. The redaction section lists the ids the scrub reports and the
//! revision each arrived in; it carries no regex, no prefix, and no example string, because a
//! catalogue of secret shapes committed to a repository is a different document with a different
//! audience. `docs/redaction-patterns-*.md` is where that research lives.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};

use crate::ask;
use crate::cli::DEFAULT_ASK_LIMIT;
use crate::cost::{PREMIUM_FLAG_MAX_MESSAGES, PREMIUM_FLAG_MAX_OUTPUT_TOKENS};
use crate::cost_report::PREMIUM_SESSIONS_LISTED;
use crate::evidence::EvidenceKind;
use crate::format;
use crate::metrics::FOLDS;
use crate::pricing::{self, PRICE_TABLE_REVISION, PRICES, PriceRow, US_GEO_MULTIPLIER};
use crate::redaction::{
    FILTER_PROFANITY_BY_DEFAULT, PATTERN_REVISION, PATTERNS, PatternId, REDACT_SECRETS_BY_DEFAULT,
    SECRET_PATTERNS,
};
use crate::repetition;
use crate::rules::RuleId;
use crate::scoring::{Lane, RulePack, constants, fire_rate_denominator};

/// The whole catalogue, as Markdown.
pub fn render() -> String {
    let mut out = String::with_capacity(16 * 1024);
    header(&mut out);
    rules(&mut out);
    lanes(&mut out);
    metrics(&mut out);
    cost(&mut out);
    ask_lane(&mut out);
    repetition_lane(&mut out);
    redaction(&mut out);
    footer(&mut out);
    out
}

/// The four stamps a reader compares two documents by.
fn header(out: &mut String) {
    out.push_str(
        "# What qanungo looks for\n\n\
         Every rule this build fires, every threshold it fires at, the lanes those rules score \
         into, the prices it bills at, and the patterns it scrubs.\n\n\
         **This file is generated.** It is rendered from the constants and tables the runtime \
         itself reads, so it cannot describe a rule the code does not run. Do not edit it by \
         hand — run `qanungo rules > RULES.md` and commit the result; a test fails if the two \
         disagree.\n\n",
    );
    let pack = RulePack::current();
    out.push_str("| Stamp | Value | What it pins |\n| --- | --- | --- |\n");
    let _ = writeln!(
        out,
        "| Rule pack | `{}` | Every rule id, threshold, scoring constant, and lane mapping below. \
         Two reports are comparable **iff this matches**. |",
        pack.stamp(),
    );
    let _ = writeln!(
        out,
        "| Formula | `{}` | How the readings combine into a score. Bumped when the arithmetic \
         changes, so a re-weighting cannot hide behind unchanged numbers. |",
        constants::FORMULA,
    );
    let _ = writeln!(
        out,
        "| Redaction patterns | `{PATTERN_REVISION}` | The pattern set every rendered excerpt was \
         scrubbed with. |",
    );
    let _ = writeln!(
        out,
        "| Price table | `{PRICE_TABLE_REVISION}` | The dated rates the cost lane bills at. |",
    );
    let _ = writeln!(
        out,
        "\nThe full rule-pack digest is `{}`; the short form above is its first {} characters and \
         is what the footer of every report prints.",
        pack.digest(),
        pack.stamp().len(),
    );
}

/// One section per rule, in the order [`RuleId::ALL`] runs them.
fn rules(out: &mut String) {
    let _ = writeln!(
        out,
        "\n## Rules\n\n\
         {} rules, evaluated in this order, which is also report order and the order the rule-pack \
         digest hashes them in. They are **not** mutually exclusive: one session can trip several, \
         and should then appear under each, because the findings ask for different things.\n\n\
         Every threshold is **arbitrary until measured** — a first guess at where a pattern stops \
         being ordinary work and starts being a habit worth naming. Where a number has had a \
         measurement run over it, the measurement is stated beside it; where the *Measured against* \
         column is empty, the number is still a guess and should be read as one. A rule that fires \
         constantly is evidence its threshold is wrong, not evidence the habit is everywhere.",
        RuleId::ALL.len(),
    );
    for (index, rule) in RuleId::ALL.into_iter().enumerate() {
        let _ = writeln!(
            out,
            "\n### {}. {} — `{}`\n",
            index + 1,
            rule.title(),
            rule.key(),
        );
        let _ = writeln!(out, "**Fires when.** {}\n", rule.fires_when());
        let thresholds = rule.thresholds();
        if thresholds.is_empty() {
            out.push_str("*No threshold constants — this rule's trigger is a count of none.*\n");
        } else {
            out.push_str("| Constant | Value | Measured against |\n| --- | --- | --- |\n");
            for threshold in thresholds {
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} |",
                    threshold.name,
                    threshold.value,
                    threshold.note.unwrap_or("—"),
                );
            }
        }
        let _ = writeln!(
            out,
            "\n- **Eligible sessions** — {}. A session outside that denominator is not looked at, \
             which is not the same as looking and finding nothing.",
            fire_rate_denominator(rule),
        );
        let _ = writeln!(
            out,
            "- **Evidence** — {} ({}). {}",
            rule.evidence_kind().key(),
            evidence_shape(rule.evidence_kind()),
            scored_into(rule),
        );
        let _ = writeln!(
            out,
            "- **Problem** — the report prefixes this with *n* of *m* folded sessions: “{}”",
            rule.problem_predicate(),
        );
        let _ = writeln!(out, "- **Action** — “{}”", rule.action());
    }
}

/// What a finding of this kind may show a reader.
const fn evidence_shape(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Event => {
            "it counted concrete events, so it can point at them — bounded locators into the \
             transcript, whose excerpts are scrubbed on the way out"
        }
        EvidenceKind::Structural => {
            "it measured a shape rather than an utterance, so its evidence is timestamps and \
             counts and it mints no excerpt"
        }
        EvidenceKind::Mixed => {
            "one half counted events it can point at and the other is a shape or an absence, which \
             can only be stated"
        }
    }
}

/// Which lane, if any, reads this rule's fire rate.
fn scored_into(rule: RuleId) -> String {
    let key = format!("fire-rate:{}", rule.key());
    let lanes: Vec<_> = Lane::ALL
        .into_iter()
        .filter(|lane| {
            lane.components()
                .iter()
                .any(|component| component.key == key)
        })
        .map(Lane::title)
        .collect();
    if lanes.is_empty() {
        "No lane reads this rule's fire rate.".to_owned()
    } else {
        format!("Scored into **{}**.", lanes.join(", "))
    }
}

/// The lane table, its constants, and the formula.
fn lanes(out: &mut String) {
    let _ = writeln!(
        out,
        "\n## Lanes\n\n\
         {} practice lanes. Each is a small set of **components**, each of which reads one rate \
         off the window. A lane no signal feeds is never scored — it says which signal it is \
         waiting for rather than defaulting to a zero or a hundred — and a lane whose components \
         all came back empty for one harness reads *no reading*, which is also not a zero.\n",
        Lane::ALL.len(),
    );
    out.push_str("| Lane | Key | Reads | Of |\n| --- | --- | --- | --- |\n");
    for lane in Lane::ALL {
        let components = lane.components();
        if components.is_empty() {
            let _ = writeln!(
                out,
                "| {} | `{}` | *nothing typed yet* | — |",
                lane.title(),
                lane.key(),
            );
            continue;
        }
        for (index, component) in components.iter().enumerate() {
            let name = if index == 0 { lane.title() } else { "" };
            let key = if index == 0 {
                format!("`{}`", lane.key())
            } else {
                String::new()
            };
            let _ = writeln!(
                out,
                "| {name} | {key} | {} (`{}`) | {} |",
                component.label, component.key, component.denominator,
            );
        }
    }
    out.push_str("\n### The formula\n\n```text\n");
    out.push_str(
        "penalty_i = clamp(reading_i / floor_i, 0, 1)\n\
         score     = round(100 × (1 − mean(penalty_i)))    // mean over the components that read\n",
    );
    out.push_str("```\n\n");
    out.push_str(
        "In words: each component divides its reading by its floor, clamped into 0…1; the lane \
         scores **100 × (1 − mean(clamp(reading/floor, 0, 1)))** over the components that read. \
         Every component in a lane weighs the same — nothing measured says one deserves more — and \
         a component whose signal is absent has no say rather than a zero penalty.\n\n",
    );
    out.push_str("| Constant | Value | What it does |\n| --- | --- | --- |\n");
    let _ = writeln!(
        out,
        "| `FIRE_RATE_FLOOR` | {} | The fire rate at which a fire-rate component spends its whole \
         share. One floor for every rule, deliberately — and it **saturates**: every rate from \
         here to 100% costs the same, so read the component's raw reading, not the lane number, \
         where a rule fires above it. |",
        format::percent(constants::FIRE_RATE_FLOOR),
    );
    let _ = writeln!(
        out,
        "| `TOOL_ERROR_RATE_FLOOR` | {} | The pooled tool failure rate at which that component \
         spends its whole share. Anchored on `SESSION_TOOL_ERROR_RATE` rather than chosen freely. |",
        format::percent(constants::TOOL_ERROR_RATE_FLOOR),
    );
    let _ = writeln!(
        out,
        "| `MIN_SCORED_SESSIONS` | {} | Eligible sessions a fire-rate component needs before its \
         rate is a reading at all. Under it the component reports no reading rather than a jumpy \
         one. |",
        constants::MIN_SCORED_SESSIONS,
    );
    let _ = writeln!(
        out,
        "| `MIN_SCORED_TOOL_ATTEMPTS` | {} | Calls that reported an outcome before the pooled \
         error rate is a reading. |",
        constants::MIN_SCORED_TOOL_ATTEMPTS,
    );
    let _ = writeln!(
        out,
        "| `CLEAN_SCORE` | {:.0} | The score of a window in which nothing this pack penalizes was \
         observed. It means *nothing penalized was seen*, never *the practice is perfect*. |",
        constants::CLEAN_SCORE,
    );
    out.push_str(
        "\nScores are computed **per `source_agent`**, because harnesses differ in what they can \
         express and a blended number would move when the harness mix moved. The one fleet number \
         is the **unweighted mean of the per-harness scores**, every harness counting once — \
         stable under a mix shift, unstable under a roster shift, which is why a fleet trend arrow \
         is drawn only when the same harnesses scored the lane on both sides. Scores are \
         comparable across windows under the same rule pack, and **never across lanes**.\n",
    );
}

/// The folds the rules read.
fn metrics(out: &mut String) {
    let _ = writeln!(
        out,
        "\n## Folds\n\n\
         What the fold derives from `munshi-transcript`'s typed events, before any rule looks at \
         it. {} of them; every rule and every lane above reads one of these and nothing else.\n",
        FOLDS.len(),
    );
    out.push_str("| Fold | Reads |\n| --- | --- |\n");
    for fold in FOLDS {
        let _ = writeln!(out, "| {} | {} |", fold.name, fold.reads);
    }
}

/// The price table, and the premium-waste flag's floors.
fn cost(out: &mut String) {
    let as_of = price_table_as_of();
    let _ = writeln!(
        out,
        "\n## Cost\n\n\
         Dollars are Anthropic API **list** prices, per million tokens, from the row effective at \
         each session's archive time. A model with no row is unpriced rather than free, and a \
         harness whose billing is not recoverable from a transcript gets token volumes and no \
         money at all.\n\n\
         Price table revision `{PRICE_TABLE_REVISION}`. *Top tier* below is resolved as of \
         {}, the newest date in the table itself, so this document says the same thing whenever it \
         is rendered.\n",
        as_of.format("%Y-%m-%d"),
    );
    out.push_str(
        "| Model | From | Input | Output | Cache write 5m | Cache write 1h | Cache read | Fast \
         tier | US premium | Top tier |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | --- | \
         ---: | --- |\n",
    );
    for row in PRICES {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            row.model,
            row.effective_from().format("%Y-%m-%d"),
            format::dollars(row.rates.input),
            format::dollars(row.rates.output),
            format::dollars(row.rates.cache_write_5m),
            format::dollars(row.rates.cache_write_1h),
            format::dollars(row.rates.cache_read),
            fast_tier(row),
            row.us_geo_multiplier
                .map_or_else(|| "—".to_owned(), |multiplier| format!("×{multiplier}")),
            if pricing::is_top_tier_model(row.model, as_of) {
                "yes"
            } else {
                "no"
            },
        );
    }
    let _ = writeln!(
        out,
        "\nA fast-mode session bills at the fast column where the model has one; where it does \
         not, `speed = \"fast\"` is **unpriced** rather than billed at the base rate. A `US \
         premium` of ×{US_GEO_MULTIPLIER} applies to US-only inference on the rows that document \
         one; a row with none that meets US inference is unpriced, because pricing it either way \
         would be a claim the table's sources do not support.",
    );
    let _ = writeln!(
        out,
        "\n### Premium waste\n\n\
         One flag, not a judgement: sessions billed **wholly at the day's top tier** that were \
         very small. Whether small was the wrong place for the dearest model is the reader's \
         call.\n"
    );
    out.push_str("| Constant | Value | What it does |\n| --- | --- | --- |\n");
    let _ = writeln!(
        out,
        "| `PREMIUM_FLAG_MAX_OUTPUT_TOKENS` | {} tokens | The most output tokens a top-tier session may \
         have produced and still be listed. Set in the gap below a floor cluster in the real \
         distribution (2026-09-04: 4 of 61 wholly-top-tier sessions, 6.6%). |",
        PREMIUM_FLAG_MAX_OUTPUT_TOKENS,
    );
    let _ = writeln!(
        out,
        "| `PREMIUM_FLAG_MAX_MESSAGES` | {PREMIUM_FLAG_MAX_MESSAGES} | The most billed messages \
         such a session may have carried. A handful of exchanges rather than a working session; it \
         bound nothing on the archive it was measured against and is carried because the two \
         floors describe different shapes. |",
    );
    let _ = writeln!(
        out,
        "| `PREMIUM_SESSIONS_LISTED` | {PREMIUM_SESSIONS_LISTED} | How many flagged sessions the \
         report lists before it stops and says how many more there were. |",
    );
}

/// Whether a row has a fast tier, and at what output rate.
fn fast_tier(row: &PriceRow) -> String {
    row.fast.map_or_else(
        || "—".to_owned(),
        |fast| {
            format!(
                "{} in / {} out",
                format::dollars(fast.input),
                format::dollars(fast.output),
            )
        },
    )
}

/// The instant the *Top tier* column is resolved at: the newest `effective_from` in the table.
///
/// Deliberately not `Utc::now()`. The column would then be a function of the day the document was
/// rendered, and the equality test that pins `RULES.md` would fail the first time a row's date
/// passed — turning the drift gate into a calendar alarm. The newest row's own date is the latest
/// instant at which every row in this build is in force, which is the question the column asks.
fn price_table_as_of() -> DateTime<Utc> {
    PRICES
        .iter()
        .map(PriceRow::effective_from)
        .max()
        .unwrap_or_else(Utc::now)
}

/// The ask lane's rubric and its minimums.
fn ask_lane(out: &mut String) {
    out.push_str(
        "\n## Ask\n\n\
         `qanungo ask` is a deterministic ranked search over the archive's own summaries — no \
         model, no third service. A query is lower-cased and split on anything that is not a \
         letter or a digit; each surviving term scores once per field it appears in, weighted:\n\n",
    );
    out.push_str("| Field | Weight | Quotable |\n| --- | ---: | --- |\n");
    for field in ask::rubric() {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            field.label,
            field.weight,
            if field.quotable { "yes" } else { "no" },
        );
    }
    out.push_str(
        "\n*Quotable* decides which field a hit's snippet is drawn from: a repository name ranks \
         well and reads as one word out of context, so the snippet prefers a prose field that \
         matched and falls back to a keyword only when none did.\n\n",
    );
    out.push_str("| Constant | Value | What it does |\n| --- | --- | --- |\n");
    let _ = writeln!(
        out,
        "| `MIN_TERM_CHARS` | {} | Shortest query word kept. Shorter fragments match almost \
         everything and rank nothing. |",
        ask::MIN_TERM_CHARS,
    );
    let _ = writeln!(
        out,
        "| `MAX_SNIPPET_CHARS` | {} | Longest snippet rendered. A snippet is a pointer into a \
         summary, not the summary; the bound is on already-scrubbed text, so cutting one short \
         can only hide detail. |",
        ask::MAX_SNIPPET_CHARS,
    );
    let _ = writeln!(
        out,
        "| `DEFAULT_ASK_LIMIT` | {DEFAULT_ASK_LIMIT} | Ranked matches printed when `--limit` says \
         otherwise. `--verbatim` escalates into at most this many transcripts — the funnel is \
         bounded to what the summary search already surfaced, never an archive-wide grep. |",
    );
    let _ = writeln!(
        out,
        "| stop words | {} | Function words dropped before scoring. Deliberately short and \
         English-only: a long list would quietly refuse to search for real words. |",
        ask::stop_words().len(),
    );
}

/// The repetition machinery both `doctor` and `flows` are built on.
fn repetition_lane(out: &mut String) {
    out.push_str(
        "\n## Repetition\n\n\
         What `qanungo doctor` (one repository) and `qanungo flows` (the whole archive) compare \
         messages with. Two messages are the same request when they share enough four-word \
         phrases; everything below bounds that comparison. These report repetition and never \
         cause: an ordering in a transcript is not a reason.\n\n",
    );
    out.push_str("| Constant | Value | What it does |\n| --- | --- | --- |\n");
    for (name, value, what) in [
        (
            "SHINGLE_WORDS",
            repetition::SHINGLE_WORDS.to_string(),
            "Words in one shingle — the phrase length two messages are compared on.",
        ),
        (
            "MIN_CLUSTERABLE_WORDS",
            repetition::MIN_CLUSTERABLE_WORDS.to_string(),
            "Shortest message compared at all. \"yes\", \"continue\" and \"do it\" are the most \
             repeated things anybody types and mean nothing; the floor is set where a sentence \
             starts. Shorter messages are counted, not silently passed over.",
        ),
        (
            "SIMILARITY_THRESHOLD_PERCENT",
            format!("{}%", repetition::SIMILARITY_THRESHOLD_PERCENT),
            "How much of the **shorter** message's phrases the two must share. Containment rather \
             than Jaccard, so a short rule restated inside a long request still matches.",
        ),
        (
            "MIN_CLUSTER_SESSIONS",
            repetition::MIN_CLUSTER_SESSIONS.to_string(),
            "Distinct conversations a cluster must span before it is reported. Repetition inside \
             one session is a conversation, not a finding.",
        ),
        (
            "SAME_CONVERSATION_PERCENT",
            format!("{}%", repetition::SAME_CONVERSATION_PERCENT),
            "Shared messages above which two sessions are read as **one conversation captured \
             twice** and merged. Without it, a replayed transcript turns every message of one \
             conversation into a repeated request.",
        ),
        (
            "MIN_SHARED_INSTRUCTIONS",
            repetition::MIN_SHARED_INSTRUCTIONS.to_string(),
            "Messages two sessions must share before the merge rule is consulted at all, so a \
             percentage over a tiny denominator cannot delete the finding it is protecting.",
        ),
        (
            "MAX_SHINGLE_POSTINGS",
            repetition::MAX_SHINGLE_POSTINGS.to_string(),
            "Messages a phrase may appear in before it is skipped for candidate gathering. Bounds \
             the work, never the truth: skipping a phrase can only lower a measured overlap.",
        ),
        (
            "MAX_CITATIONS_PER_CLUSTER",
            repetition::MAX_CITATIONS_PER_CLUSTER.to_string(),
            "Occurrences one cluster cites before the list is cut short. The total travels beside \
             them, so a cut list is never mistaken for the whole.",
        ),
    ] {
        let _ = writeln!(out, "| `{name}` | {value} | {what} |");
    }
}

/// The scrub's pattern names — names only.
fn redaction(out: &mut String) {
    let _ = writeln!(
        out,
        "\n## Redaction\n\n\
         Every surface that renders archived text scrubs it first, and stamps the pattern revision \
         it scrubbed with. The ids below are what a marker and a footer report; **the patterns \
         themselves are not printed here**, and that is deliberate — a catalogue of secret shapes \
         is a different document. The research lives in `docs/redaction-patterns-*.md`, one file \
         per revision.\n\n\
         Pattern revision `{PATTERN_REVISION}`: {} ids, {} of them secret patterns.\n",
        PATTERNS.len(),
        SECRET_PATTERNS.len(),
    );
    out.push_str("| Id | Kind | Added in |\n| --- | --- | --- |\n");
    for pattern in PATTERNS {
        let _ = writeln!(
            out,
            "| `{}` | {} | `{}` |",
            pattern.as_str(),
            if matches!(pattern, PatternId::Profanity) {
                "profanity"
            } else {
                "secret"
            },
            pattern.added_in(),
        );
    }
    let _ = writeln!(
        out,
        "\nSecrets are scrubbed **{}** and profanity is masked **{}**. There is no `--redact`: the \
         scrub is not something a person should have to remember to ask for, and the only way to \
         lose it is `--no-redact`, which the document's own footer then confesses to.",
        if REDACT_SECRETS_BY_DEFAULT {
            "by default"
        } else {
            "only on request"
        },
        if FILTER_PROFANITY_BY_DEFAULT {
            "by default"
        } else {
            "only on request"
        },
    );
}

/// How to regenerate, said at the end where somebody who has just read a stale number is looking.
fn footer(out: &mut String) {
    let _ = writeln!(
        out,
        "\n---\n\n\
         Generated by `qanungo rules`, which reads no archive and needs no `--patwari-url`. \
         Regenerate with `qanungo rules > RULES.md`; the test \
         `rules_md_matches_the_rendered_catalogue` fails while the committed file and the build \
         disagree. Every number above is rendered from the constant the runtime reads — \
         `crates/qanungo/src/rules.rs`, `scoring.rs`, `metrics.rs`, `pricing.rs`, `cost.rs`, \
         `ask.rs`, `repetition.rs`, `redaction.rs` — so a threshold cannot be changed without this \
         document changing with it.",
    );
}
