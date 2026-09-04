//! Fixture-backed proof that the cost lane deduplicates, prices, and refuses to price — and that
//! a report built from a transcript stuffed with canary strings contains none of them.
//!
//! The fixture is one claude-code session carrying every case the lane has to tell apart: an API
//! message split across three records, a second model, a fast-mode message, claude-code's
//! `<synthetic>` placeholder, and a model no price table has heard of. Its figures are round so a
//! rendered dollar can be checked against `docs/pricing-sources-2026-08-23.md` by eye.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::Parser;
use qanungo::cli::{Cli, Command, Window};
use qanungo::cost::{CostTotals, SessionCost, fold_cost};
use qanungo::cost_report::{CostInstrumentation, CostReport};
use qanungo::metrics;
use qanungo::patwari::sha256_hex;
use qanungo::pricing::Unpriced;
use qanungo::sync::SyncStats;

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

/// Folds a fixture exactly as the command does: from a file, streaming, under the harness the
/// name implies, and paired with the archive identity that prices it.
fn fold(relative: &str, source_agent: &str, repository: Option<&str>) -> SessionCost {
    let path = fixture(relative);
    let bytes = std::fs::read(&path).expect("fixture is readable");
    let source = metrics::source_for_agent(source_agent).expect("a known harness");
    let file = File::open(&path).expect("fixture is readable");
    let folded = fold_cost(source, 2, BufReader::new(file)).expect("v2 is supported");
    SessionCost {
        source_hash: sha256_hex(&bytes),
        source_agent: source_agent.to_owned(),
        repository: repository.map(ToOwned::to_owned),
        // The fixture's records are dated 2026-08-10, and a session is priced as of when the
        // *archive* took it rather than when the conversation happened.
        archived_at: Some(at("2026-08-10T12:00:00Z")),
        fold: folded,
        bytes_folded: bytes.len() as u64,
    }
}

fn billing_session() -> SessionCost {
    fold(
        "cost/claude-billing.jsonl",
        "claude-code",
        Some("surdy/qanungo"),
    )
}

/// The rule the lane is built on, on a real transcript: three records, one message, one charge.
#[test]
fn a_message_split_across_records_is_billed_once() {
    let session = billing_session();
    assert_eq!(session.fold.records_read, 9);
    assert_eq!(
        session.fold.messages, 5,
        "five distinct message ids across nine records",
    );
    assert_eq!(
        session.fold.duplicate_records, 2,
        "the two extra records of the split message were dropped, not summed",
    );
    assert!(!session.fold.undeduplicatable.any());

    // The split message's own figures, counted once each rather than three times.
    let opus = session
        .fold
        .usage
        .iter()
        .find(|(key, _)| key.model.as_deref() == Some("claude-opus-5") && key.speed.is_none())
        .map(|(_, tally)| *tally)
        .expect("the split message billed under opus 5 at the ordinary tier");
    assert_eq!(opus.messages, 1);
    assert_eq!(opus.input, 200_000);
    assert_eq!(opus.output, 100_000);
    assert_eq!(opus.cache_write_1h, 400_000);
    assert_eq!(opus.cache_write_5m, 0);
    assert_eq!(opus.cache_read, 1_000_000);
    assert_eq!(
        opus.cache_write_untiered, 0,
        "the per-tier buckets were present, so the total was never consulted",
    );
}

/// The whole window, priced against the committed table. Every figure below is checkable by hand:
/// Opus 5 at $5/$25 with a $10 1-hour cache write and a $0.50 cache read, its fast tier at
/// $10/$50, and Sonnet 5 at $2/$10.
#[test]
fn the_window_prices_each_model_at_its_committed_rates() {
    let totals = CostTotals::fold(&[billing_session()]);
    assert_eq!(totals.priceable_sessions, 1);

    // Opus 5, ordinary tier: $1.00 input + $2.50 output + $4.00 cache write + $0.50 cache read.
    // Opus 5, fast tier: 100k output at $50/MTok = $5.00.
    let opus = totals.by_model["claude-opus-5"];
    assert!((opus.dollars - 13.00).abs() < 1e-9, "{opus:?}");
    assert_eq!(opus.fast_messages, 1);
    // Sonnet 5: 500k output at $10/MTok.
    assert!((totals.by_model["claude-sonnet-5"].dollars - 5.00).abs() < 1e-9);
    assert!((totals.priced.dollars - 18.00).abs() < 1e-9, "{totals:?}");

    // The cache saving is the read side alone: a million tokens read back at $0.50 instead of
    // being sent again at $5.00.
    assert!((totals.priced.cache_read_dollars - 0.50).abs() < 1e-9);
    assert!((totals.priced.cache_read_at_input_rate - 5.00).abs() < 1e-9);
    assert!((totals.priced.cache_saving() - 4.50).abs() < 1e-9);

    // Everything the table could not price is named rather than folded into the total.
    assert_eq!(totals.flagged.synthetic.output, 1_000);
    assert_eq!(
        totals.flagged.unpriced[&Unpriced::UnknownModel("claude-opus-9-imaginary".to_owned())]
            .output,
        700,
    );
    assert_eq!(totals.flagged.untiered_cache_writes, 0);
}

/// The archive's universal `inference_geo`, exercised the way it actually occurs. Every claude
/// record in the fixture carries `not_available` — 61,122 of the archive's 61,184 usage records do
/// — and it is the base-rate case, so the dollars are the same as if the field were absent and
/// nothing is flagged for it. Reading it as an unknown region priced the first production run at
/// zero across all 311 sessions, which is what this pins against.
#[test]
fn the_archives_own_inference_geo_prices_at_base_and_flags_nothing() {
    let raw = std::fs::read_to_string(fixture("cost/claude-billing.jsonl")).unwrap();
    assert!(
        raw.contains(r#""inference_geo":"not_available""#),
        "the fixture must carry the value the archive actually records",
    );

    let totals = CostTotals::fold(&[billing_session()]);
    assert!(
        (totals.priced.dollars - 18.00).abs() < 1e-9,
        "un-routed usage prices at base: {totals:?}",
    );
    assert!(
        !totals
            .flagged
            .unpriced
            .keys()
            .any(|reason| matches!(reason, Unpriced::InferenceGeo(_))),
        "nothing about the region is unpriced: {:?}",
        totals.flagged.unpriced,
    );

    let markdown = render(&totals);
    assert!(
        !markdown.contains("inference region"),
        "no region flag reaches the document: {markdown}"
    );
    assert!(markdown.contains("**$18.00**"), "{markdown}");
}

/// The repository cut comes from Patwari's projection, not from anything in the transcript, so a
/// session archived without one is its own row.
#[test]
fn the_repository_cut_follows_the_archives_own_projection() {
    let totals = CostTotals::fold(&[
        billing_session(),
        SessionCost {
            repository: None,
            ..fold("cost/claude-billing.jsonl", "claude-code", None)
        },
    ]);
    assert!((totals.by_repository[&Some("surdy/qanungo".to_owned())].dollars - 18.00).abs() < 1e-9);
    assert!((totals.by_repository[&None].dollars - 18.00).abs() < 1e-9);
    assert!((totals.priced.dollars - 36.00).abs() < 1e-9);
}

/// A Copilot session is counted in tokens and never in money — no dollars, no credit estimate,
/// no premium-request count — because its billing regime is not recoverable from a transcript.
#[test]
fn a_copilot_session_is_counted_in_tokens_and_never_in_dollars() {
    let totals = CostTotals::fold(&[fold(
        "cost/copilot-billing.jsonl",
        "copilot-cli",
        Some("surdy/munshi"),
    )]);
    assert_eq!(totals.token_only_sessions, 1);
    assert_eq!(totals.priced.dollars, 0.0);
    assert!(totals.by_model.is_empty());
    assert!(
        totals.by_repository.is_empty(),
        "a copilot session carries no dollars to attribute to a repository",
    );
    assert_eq!(
        totals.copilot[&Some("claude-opus-4.8".to_owned())].output,
        2_000
    );
    assert_eq!(totals.copilot[&Some("gpt-5.6-sol".to_owned())].output, 500);

    let markdown = render(&totals);
    assert!(
        markdown.contains("## Token volumes (copilot)"),
        "{markdown}"
    );
    assert!(markdown.contains("**output tokens only**"), "{markdown}");
    assert!(
        markdown.contains("| `claude-opus-4.8` | 2 | 2.0k |"),
        "{markdown}"
    );
    assert!(
        !markdown.contains('$'),
        "no dollar figure anywhere in a copilot-only window: {markdown}",
    );
    assert!(
        !markdown.contains("CANARY"),
        "the copilot fold reads no transcript content either: {markdown}",
    );
}

/// Munshi's own copilot fixtures record an `assistant.message` with no model and no
/// `outputTokens`, which is a real archive state: the session is folded, counted as token-only,
/// and claims nothing at all rather than a zero.
#[test]
fn a_copilot_session_that_recorded_no_usage_claims_nothing() {
    let totals = CostTotals::fold(&[
        fold(
            "munshi/copilot-1.0.70-envelope.jsonl",
            "copilot-cli",
            Some("surdy/munshi"),
        ),
        fold(
            "munshi/copilot-1.0.76-tool-activity.jsonl",
            "copilot-cli",
            None,
        ),
    ]);
    assert_eq!(totals.token_only_sessions, 2);
    assert!(
        totals.copilot.is_empty(),
        "no usage recorded is not a zero-token model row",
    );
    let markdown = render(&totals);
    assert!(
        !markdown.contains("## Token volumes (copilot)"),
        "{markdown}"
    );
}

/// Codex records no per-message usage at all, so its sessions are named as contributing nothing
/// rather than silently missing from the document.
#[test]
fn a_codex_fixture_contributes_no_usage_and_says_so() {
    let totals = CostTotals::fold(&[fold("rules/retry-loop.jsonl", "codex-cli", None)]);
    assert_eq!(totals.no_signal_sessions["codex-cli"], 1);
    assert_eq!(totals.priceable_sessions, 0);
    assert!(totals.priced.tokens.messages == 0);
    let markdown = render(&totals);
    assert!(
        markdown.contains("1 recording no per-message usage at all (codex-cli)"),
        "{markdown}"
    );
}

/// The redaction line is hard, and the cost lane holds it by construction rather than by filter:
/// the fold never reads a record's classification at all, so the fixture's canaries — in user
/// text, assistant text, a tool command, a tool result, a cwd, and a git branch — have no path to
/// the document.
#[test]
fn a_rendered_cost_report_contains_no_verbatim_transcript_content() {
    let raw = std::fs::read_to_string(fixture("cost/claude-billing.jsonl")).unwrap();
    assert!(raw.contains("CANARY_"), "the fixture must carry canaries");

    let totals = CostTotals::fold(&[billing_session()]);
    let markdown = render(&totals);

    assert!(
        !markdown.contains("CANARY"),
        "a canary token reached the report:\n{markdown}"
    );
    for forbidden in [
        "CANARY_USER_REQUEST_ONE",
        "CANARY_ASSISTANT_MESSAGE",
        "CANARY_COMMAND_ONE",
        "CANARY_TOOL_OUTPUT",
        "CANARY_BRANCH",
        "/work/",
        "toolu_",
        "msg_split",
        "Bash",
    ] {
        assert!(
            !markdown.contains(forbidden),
            "`{forbidden}` reached the report:\n{markdown}"
        );
    }
    // What it does say: the archive's own billing identifiers, and money.
    assert!(markdown.contains("`claude-opus-5`"), "{markdown}");
    assert!(markdown.contains("`surdy/qanungo`"), "{markdown}");
    assert!(markdown.contains("**$18.00**"), "{markdown}");
}

/// Deduplication is the load-bearing step, so the document says out loud that it happened —
/// beside the table it is a property of, rather than in the flagged section, which a clean window
/// elides entirely.
#[test]
fn the_document_states_that_it_deduplicated() {
    let markdown = render(&CostTotals::fold(&[billing_session()]));
    assert!(
        markdown.contains("Deduplication dropped 2 further records across this window"),
        "{markdown}"
    );
    let note = markdown
        .find("Deduplication dropped")
        .expect("the note is present");
    let flagged = markdown
        .find("## Unpriced / flagged")
        .expect("this fixture does have flagged usage");
    assert!(
        note < flagged,
        "the note belongs to the model table, not to the flagged section a clean window elides",
    );
}

/// A message id is the key the fold deduplicates by, and it is *not* an identifier the report
/// renders: it names one API call, which is closer to conversation than to billing metadata.
#[test]
fn the_document_names_models_and_repositories_but_never_a_message_id() {
    let markdown = render(&CostTotals::fold(&[billing_session()]));
    for message_id in ["msg_split", "msg_sonnet", "msg_synthetic", "msg_fast"] {
        assert!(!markdown.contains(message_id), "{message_id}: {markdown}");
    }
}

/// The top-tier flag on a real transcript: two exchanges on the day's dearest model, priced whole,
/// listed with the hash to read it by — and nothing of what was said in it anywhere in the
/// document. The section is the newest surface in the lane and therefore the likeliest place for
/// the redaction line to be crossed, so the canary check is repeated over it rather than assumed
/// to be covered by the one above.
#[test]
fn a_small_session_on_the_days_dearest_model_is_listed_with_its_hash_and_nothing_else() {
    let raw = std::fs::read_to_string(fixture("cost/claude-top-tier.jsonl")).unwrap();
    assert!(
        raw.contains("CANARY_TOP_TIER_"),
        "the fixture carries canaries"
    );

    let session = fold(
        "cost/claude-top-tier.jsonl",
        "claude-code",
        Some("surdy/qanungo"),
    );
    let hash = session.source_hash.clone();
    let totals = CostTotals::fold(&[session]);

    // Two messages and 1,000 output tokens of Fable 5 at $50/MTok, over 1,400 input at $10/MTok.
    assert_eq!(totals.premium.sessions, 1);
    assert_eq!(totals.premium.flagged.len(), 1);
    let flagged = &totals.premium.flagged[0];
    assert_eq!(flagged.messages, 2);
    assert_eq!(flagged.output, 1_000);
    assert_eq!(flagged.models, vec!["claude-fable-5".to_owned()]);
    assert_eq!(flagged.source_hash, hash);
    // The same dollars the by-model table counts, to the cent: the flag annotates, it does not add.
    assert!((flagged.dollars - totals.by_model["claude-fable-5"].dollars).abs() < 1e-12);
    assert!((flagged.dollars - totals.priced.dollars).abs() < 1e-12);
    assert!((flagged.dollars - 0.064).abs() < 1e-9, "{flagged:?}");

    let markdown = render(&totals);
    assert!(
        markdown.contains("## Small sessions at the top price tier"),
        "{markdown}"
    );
    assert!(
        markdown.contains(&format!(
            "| 2026-08-10T12:00:00Z | `claude-fable-5` | 2 | 1.0k | $0.06 | `{hash}` |"
        )),
        "{markdown}"
    );
    assert!(
        !markdown.contains("CANARY"),
        "the section renders aggregates and a hash, never a word of the session:\n{markdown}"
    );
    for forbidden in [
        "CANARY_TOP_TIER_REQUEST",
        "CANARY_TOP_TIER_ANSWER",
        "CANARY_TOP_TIER_COMMAND",
        "CANARY_TOP_TIER_BRANCH",
        "/work/",
        "toolu_tier",
        "msg_tier",
        "Bash",
    ] {
        assert!(
            !markdown.contains(forbidden),
            "`{forbidden}` reached the report:\n{markdown}"
        );
    }
}

/// The lane's own billing fixture is the counter-case, and deliberately so: it mixes two models,
/// carries a `<synthetic>` placeholder and a model no table has heard of, and is therefore a
/// session this build cannot state the whole shape of. It is not listed, not counted in the
/// denominator, and the section does not render at all — while every dollar it does carry is
/// unchanged.
#[test]
fn a_session_the_lane_cannot_read_whole_is_left_out_of_the_top_tier_section() {
    let totals = CostTotals::fold(&[billing_session()]);
    assert_eq!(totals.premium.sessions, 0, "{:?}", totals.premium);
    assert!(!totals.premium.any());

    let markdown = render(&totals);
    assert!(!markdown.contains("top price tier"), "{markdown}");
    // The flag changed nothing it was not supposed to: the same $18.00 the lane priced before it.
    assert!((totals.priced.dollars - 18.00).abs() < 1e-9);
    assert!(markdown.contains("**$18.00**"), "{markdown}");
}

fn window() -> Window {
    let Command::Cost(args) = Cli::parse_from(["qanungo", "cost", "--last", "12w"]).command else {
        panic!("`cost` parses as the cost command");
    };
    args.last
}

fn render(totals: &CostTotals) -> String {
    let window = window();
    let instrumentation = CostInstrumentation {
        sync: SyncStats::default(),
        fold_elapsed: Duration::from_millis(3),
        sessions_folded: totals.priceable_sessions
            + totals.token_only_sessions
            + totals.no_signal_sessions.values().sum::<usize>(),
        comparison_sessions_folded: 0,
        records_read: totals.records_read,
        bytes_folded: totals.bytes_folded,
        patwari_url: "http://127.0.0.1:8080".to_owned(),
        cache_root: PathBuf::from("/tmp/qanungo"),
    };
    CostReport {
        window: &window,
        generated_at: at("2026-08-17T12:00:00Z"),
        totals,
        // The same window on both sides, so the delta path is exercised by the redaction check
        // too and a component that started rendering a transcript string could not slip past it.
        previous: Some(totals),
        skipped: &[],
        instrumentation: &instrumentation,
    }
    .render()
}
