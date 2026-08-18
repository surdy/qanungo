//! Fixture-backed proof that each of the four rules fires — and that a report built from a
//! transcript stuffed with canary strings contains none of them.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use clap::Parser;
use qanungo::cli::{Cli, Command, Window};
use qanungo::metrics::{self, SessionMetrics};
use qanungo::patwari::sha256_hex;
use qanungo::report::{Instrumentation, Report};
use qanungo::rules::{self, Finding, RuleId};
use qanungo::sync::SyncStats;

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

/// Folds a fixture exactly as the command does: from a file, streaming, under the harness the
/// name implies.
fn fold(relative: &str, source_agent: &str) -> SessionMetrics {
    let path = fixture(relative);
    let bytes = std::fs::read(&path).expect("fixture is readable");
    let source = metrics::source_for_agent(source_agent).expect("a known harness");
    let file = File::open(&path).expect("fixture is readable");
    let folded =
        metrics::fold_transcript(source, 2, BufReader::new(file)).expect("v2 is supported");
    SessionMetrics {
        source_hash: sha256_hex(&bytes),
        source_agent: source_agent.to_owned(),
        summary: folded.summary,
        tools: folded.tools,
        activity: folded.activity,
        bytes_folded: bytes.len() as u64,
    }
}

fn claude(relative: &str) -> SessionMetrics {
    fold(relative, "claude-code")
}

fn finding(findings: &[Finding], rule: RuleId) -> &Finding {
    findings
        .iter()
        .find(|finding| finding.rule == rule)
        .unwrap_or_else(|| panic!("{rule:?} did not fire"))
}

fn fires_only(session: &SessionMetrics, rule: RuleId) -> Vec<Finding> {
    let findings = rules::evaluate(std::slice::from_ref(session));
    let rules: Vec<_> = findings.iter().map(|finding| finding.rule).collect();
    assert_eq!(rules, vec![rule], "fixture must trip exactly one rule");
    findings
}

#[test]
fn high_tool_error_rate_fires_and_names_the_tool() {
    let session = claude("rules/high-tool-error-rate.jsonl");
    assert_eq!(session.tools.total.attempts, 12);
    assert_eq!(session.tools.total.errors, 6);

    let findings = fires_only(&session, RuleId::HighToolErrorRate);
    let finding = finding(&findings, RuleId::HighToolErrorRate);
    assert_eq!(finding.evidence.len(), 1);
    assert_eq!(finding.evidence[0].source_hash, session.source_hash);
    let detail = &finding.evidence[0].detail;
    assert!(
        detail.contains("session-wide 6 of 12 calls failed (50%)"),
        "{detail}"
    );
    assert!(
        detail.contains("Bash 6 of 12 calls failed (50%)"),
        "{detail}"
    );
}

/// The marathon fixture is a genuinely continuous push: 27 records five minutes apart, never a
/// break long enough to end the sitting.
#[test]
fn marathon_session_fires_on_one_continuous_sitting() {
    let session = claude("rules/marathon-session.jsonl");
    assert_eq!(session.sittings(), Some(1));
    assert_eq!(session.active_time(), Some(TimeDelta::minutes(130)));
    assert_eq!(session.active_time(), session.span());

    let findings = fires_only(&session, RuleId::MarathonSession);
    let finding = finding(&findings, RuleId::MarathonSession);
    assert_eq!(finding.evidence[0].source_hash, session.source_hash);
    assert!(
        finding.evidence[0]
            .detail
            .starts_with("longest sitting 2h 10m within a 2h 10m span across 1 sittings"),
        "{}",
        finding.evidence[0].detail
    );
}

/// The shape the old span-based rule got wrong (qanungo #14): six short sittings spread over five
/// days. Its span is 55 times the old marathon threshold and it is not a marathon — its longest
/// continuous stretch of work is a quarter of an hour.
#[test]
fn a_resumed_session_fires_the_resumed_rule_and_not_the_marathon_one() {
    let session = claude("rules/resumed-session.jsonl");
    assert_eq!(session.sittings(), Some(6));
    assert_eq!(session.active_time(), Some(TimeDelta::minutes(90)));
    assert_eq!(session.longest_sitting(), Some(TimeDelta::minutes(15)));
    assert_eq!(session.span(), Some(TimeDelta::minutes(5 * 24 * 60 + 15)));

    let findings = fires_only(&session, RuleId::ResumedSession);
    let finding = finding(&findings, RuleId::ResumedSession);
    assert_eq!(finding.evidence[0].source_hash, session.source_hash);
    assert_eq!(
        finding.evidence[0].detail,
        "active 1h 30m across 6 sittings, span 120h 15m (80.2x)"
    );
}

/// The idle threshold is a `≤`, and the marathon threshold is a `>`. This fixture sits on both
/// lines at once: nine records exactly `IDLE_GAP` apart make one unbroken sitting of exactly
/// `MARATHON_SITTING_ACTIVE`, which is therefore *not* over it.
#[test]
fn gaps_exactly_at_the_idle_threshold_stay_in_one_sitting_and_fire_nothing() {
    let session = claude("rules/idle-gap-boundary.jsonl");
    assert_eq!(session.sittings(), Some(1));
    assert_eq!(
        session.active_time(),
        Some(rules::thresholds::MARATHON_SITTING_ACTIVE)
    );
    assert_eq!(session.longest_sitting(), session.active_time());
    assert!(
        rules::evaluate(std::slice::from_ref(&session)).is_empty(),
        "a sitting exactly at the threshold has not crossed it"
    );
}

#[test]
fn babysitting_fires_on_many_requests_and_little_tool_work() {
    let session = claude("rules/babysitting.jsonl");
    assert_eq!(session.summary.user_requests, 16);
    assert_eq!(session.summary.tool_activities, 10);

    let findings = fires_only(&session, RuleId::Babysitting);
    let finding = finding(&findings, RuleId::Babysitting);
    assert!(
        finding.evidence[0]
            .detail
            .contains("16 user requests, 10 tool activities (0.6 per request)")
    );
}

#[test]
fn fire_and_forget_fires_on_one_request_with_unattended_errors() {
    let session = claude("rules/fire-and-forget.jsonl");
    assert_eq!(session.summary.user_requests, 1);
    assert_eq!(session.summary.tool_activities, 50);
    assert_eq!(session.tools.total.errors, 4);

    let findings = fires_only(&session, RuleId::FireAndForget);
    let finding = finding(&findings, RuleId::FireAndForget);
    assert!(
        finding.evidence[0].detail.contains(
            "1 user request, 50 tool activities (50.0 per request), 4 of 25 calls failed"
        )
    );
}

#[test]
fn munshi_own_fixtures_are_healthy_sessions_and_fire_nothing() {
    let sessions = vec![
        claude("munshi/claude-code-2.1.44-normal.jsonl"),
        fold("munshi/copilot-1.0.70-envelope.jsonl", "copilot-cli"),
        fold("munshi/copilot-1.0.76-tool-activity.jsonl", "copilot-cli"),
    ];
    assert!(
        rules::evaluate(&sessions).is_empty(),
        "short healthy sessions must not trip a coaching rule"
    );
}

/// The redaction line is hard: a rendered report carries aggregates, tool names, and content
/// hashes, and nothing else. The fixture puts a canary token in every free-text transcript field
/// this crate ever touches, so a regression that started rendering any of them fails here.
#[test]
fn a_rendered_report_contains_no_verbatim_transcript_content() {
    let session = claude("rules/high-tool-error-rate.jsonl");
    let raw = std::fs::read_to_string(fixture("rules/high-tool-error-rate.jsonl")).unwrap();
    assert!(raw.contains("CANARY_"), "the fixture must carry canaries");

    let findings = rules::evaluate(std::slice::from_ref(&session));
    assert!(
        !findings.is_empty(),
        "the report under test must have findings"
    );
    let markdown = render(&[session], &findings);

    assert!(
        !markdown.contains("CANARY"),
        "a canary token reached the report:\n{markdown}"
    );
    for forbidden in [
        "CANARY_USER_REQUEST_ONE",
        "CANARY_ASSISTANT_MESSAGE",
        "CANARY_COMMAND_0",
        "CANARY_ERROR_TEXT_0",
        "rm -rf",
        "/work/fixture",
        "gitBranch",
    ] {
        assert!(
            !markdown.contains(forbidden),
            "`{forbidden}` reached the report:\n{markdown}"
        );
    }
    // Tool names are schema metadata and are the one verbatim string a report may carry.
    assert!(markdown.contains("Bash"));
}

fn render(sessions: &[SessionMetrics], findings: &[Finding]) -> String {
    let Command::Report(args) = Cli::parse_from(["qanungo", "report", "--last", "30d"]).command;
    let window: Window = args.last;
    let instrumentation = Instrumentation {
        sync: SyncStats::default(),
        fold_elapsed: Duration::from_millis(3),
        sessions_folded: sessions.len(),
        bytes_folded: sessions.iter().map(|session| session.bytes_folded).sum(),
        patwari_url: "http://127.0.0.1:8080".to_owned(),
        cache_root: PathBuf::from("/tmp/qanungo"),
    };
    Report {
        window: &window,
        generated_at: DateTime::parse_from_rfc3339("2026-08-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        sessions,
        findings,
        skipped: &[],
        instrumentation: &instrumentation,
    }
    .render()
}
