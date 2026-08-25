//! Fixture-backed proof that each of the eight rules fires — and that a report built from a
//! transcript stuffed with canary strings contains none of them.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use clap::Parser;
use munshi_transcript::Source;
use qanungo::cli::{Cli, Command, Window};
use qanungo::evidence::{EvidenceKind, SessionAnchors};
use qanungo::metrics::{self, SessionMetrics};
use qanungo::patwari::sha256_hex;
use qanungo::redaction::Redactor;
use qanungo::report::{Instrumentation, Report};
use qanungo::rules::{self, Finding, RuleId};
use qanungo::scoring::{Lane, RulePack, Scorecard};
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
        repository: None,
        artifact_set_version: 2,
        summary: folded.summary,
        tools: folded.tools,
        activity: folded.activity,
        commands: folded.commands,
        compactions: folded.compactions,
        reviews: folded.reviews,
        anchors: folded.anchors,
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
    // The fixture's 25 shell commands are all distinct on purpose: since munshi#77 typed
    // the `command` field for claude-code, a fixture that re-ran one command 25 times
    // would trip RetryLoop too, and this fixture exists to isolate FireAndForget.
    assert_eq!(session.commands.busiest_runs(), Some(1));
    assert_eq!(session.tools.total.errors, 4);

    let findings = fires_only(&session, RuleId::FireAndForget);
    let finding = finding(&findings, RuleId::FireAndForget);
    assert!(
        finding.evidence[0].detail.contains(
            "1 user request, 50 tool activities (50.0 per request), 4 of 25 calls failed"
        )
    );
}

/// A Codex rollout whose `local_shell_call` records run the same command six times, with two
/// one-off commands interleaved so the fold has to group by value rather than count events.
/// (`local_shell_call` was the one shape carrying a `command` field when this fixture was cut;
/// munshi#77 has since typed it for the claude-code and copilot shells too.)
#[test]
fn retry_loop_fires_on_one_command_re_run_within_a_session() {
    let session = fold("rules/retry-loop.jsonl", "codex-cli");
    let churn = &session.commands;
    assert_eq!(churn.command_events, 8);
    assert_eq!(churn.distinct_commands, 3);
    assert_eq!(churn.repeats, 5);
    assert_eq!(churn.repeated_commands, 1);
    assert_eq!(churn.busiest_runs(), Some(6));
    assert_eq!(churn.untracked_events, 0);

    let findings = fires_only(&session, RuleId::RetryLoop);
    let finding = finding(&findings, RuleId::RetryLoop);
    assert_eq!(finding.evidence.len(), 1);
    assert_eq!(finding.evidence[0].source_hash, session.source_hash);
    assert_eq!(
        finding.evidence[0].detail,
        "one command run 6 times; 5 of 8 command-bearing calls were repeats (62%), across 1 \
         repeated commands"
    );
}

/// The munshi#77 pull loop closed: the interpreter now types a `command` field for claude-code
/// and copilot shell events, so fixtures that run shell commands carry a churn reading with no
/// qanungo changes. A session whose tool activity records no command still claims nothing — the
/// no-signal-no-claim posture outlives the field landing; it just applies to fewer sessions.
#[test]
fn churn_reads_exactly_the_sessions_that_record_a_command_field() {
    for (relative, agent) in [
        ("munshi/copilot-1.0.76-tool-activity.jsonl", "copilot-cli"),
        ("rules/high-tool-error-rate.jsonl", "claude-code"),
    ] {
        let session = fold(relative, agent);
        assert!(
            session.commands.measurable(),
            "{relative} runs shell commands, so the typed field must give it a reading"
        );
        assert!(session.commands.repeat_share().is_some());
    }

    let session = fold("munshi/claude-code-2.1.44-normal.jsonl", "claude-code");
    assert!(
        session.summary.tool_activities > 0,
        "the no-claim case needs tool activity to make the point"
    );
    assert!(
        !session.commands.measurable(),
        "no shell command recorded means no churn reading, not zero"
    );
    assert_eq!(session.commands.repeat_share(), None);
    assert_eq!(session.commands.busiest_runs(), None);
}

// ---------------------------------------------------------------------------
// Compaction churn (qanungo #4, munshi#77 pull A)
// ---------------------------------------------------------------------------

/// The dedup discipline the interpreter warns about, against a real Copilot transcript: the
/// fixture writes **five** `session.compaction_start` records and **five**
/// `session.compaction_complete` records — ten markers for five compactions — and the fold counts
/// four. One completion states `success:false` and is excluded; the starts are context and are
/// counted as compactions by nothing.
///
/// A fold over markers would have said ten, which is the number the whole phase filter exists to
/// prevent.
#[test]
fn copilot_start_and_complete_pairs_count_as_one_compaction_each() {
    let session = fold("munshi/copilot-1.0.76-compaction.jsonl", "copilot-cli");
    let compactions = &session.compactions;
    assert!(compactions.observable, "copilot types compaction markers");
    assert_eq!(compactions.started, 5, "five announcements");
    assert_eq!(compactions.failed, 1, "one completion said it failed");
    assert_eq!(
        compactions.completed, 4,
        "four compactions, not ten markers and not five completions",
    );
    assert_eq!(compactions.count(), Some(4));

    // The pre-compaction totals ride along as context, over the counted completions only — three
    // of the four stated one, and the failed attempt states nothing at all.
    assert_eq!(compactions.pre_tokens_stated, 3);
    assert_eq!(compactions.pre_tokens_max, Some(403_971));
    assert_eq!(compactions.pre_tokens_total, 160_939 + 218_150 + 403_971);

    let findings = fires_only(&session, RuleId::CompactionChurn);
    let finding = finding(&findings, RuleId::CompactionChurn);
    assert_eq!(finding.rule.evidence_kind(), EvidenceKind::Structural);
    assert!(finding.evidence[0].anchors.is_empty(), "no event to anchor");
    assert_eq!(
        finding.evidence[0].detail,
        "compacted 4 times, 1 further attempt failed; largest window compacted 404.0k tokens, \
         stated on 3 of them",
    );
}

/// Claude Code writes one record per compaction and states no outcome on it, so the failure filter
/// has to be `succeeded != Some(false)`: the `== Some(true)` spelling would score this whole
/// harness at zero compactions. The fixture's five boundaries are five compactions.
#[test]
fn claude_boundaries_count_once_each_and_are_never_read_as_failures() {
    let session = claude("munshi/claude-code-2.1.235-compaction.jsonl");
    let compactions = &session.compactions;
    assert_eq!(compactions.started, 0, "claude writes no start marker");
    assert_eq!(compactions.failed, 0, "and states no outcome to fail");
    assert_eq!(compactions.completed, 5);

    // Two of the five state a readable pre-compaction figure; the rest carry a string, a
    // non-object, or no metadata at all, and the fold reports what was stated rather than zero.
    assert_eq!(compactions.pre_tokens_stated, 2);
    assert_eq!(compactions.pre_tokens_max, Some(339_462));
    assert_eq!(compactions.mean_pre_tokens(), Some((214_864 + 339_462) / 2));

    let findings = fires_only(&session, RuleId::CompactionChurn);
    assert_eq!(
        finding(&findings, RuleId::CompactionChurn).evidence[0].detail,
        "compacted 5 times; largest window compacted 339.5k tokens, stated on 2 of them",
    );
}

/// The three-valued verdict, on three real transcripts: a session that compacted too little, one
/// that compacted enough, and one whose harness this interpreter reads no compaction for at all.
///
/// The middle case is the discipline's whole point — a Claude Code transcript with no marker in it
/// is a session that *did not compact*, which is a reading of none rather than no reading, because
/// the harness would have written one. A Codex transcript is the opposite and stays out of the rate.
#[test]
fn the_churn_verdict_separates_not_compacting_from_not_being_readable() {
    let quiet = claude("munshi/claude-code-2.1.44-normal.jsonl");
    assert_eq!(quiet.compactions.count(), Some(0));
    assert_eq!(
        RuleId::CompactionChurn.verdict(&quiet),
        Some(false),
        "the rule looked at a session that never compacted",
    );

    let thrashing = claude("munshi/claude-code-2.1.235-compaction.jsonl");
    assert_eq!(RuleId::CompactionChurn.verdict(&thrashing), Some(true));

    let codex = fold("rules/retry-loop.jsonl", "codex-cli");
    assert_eq!(codex.compactions.count(), None);
    assert_eq!(
        RuleId::CompactionChurn.verdict(&codex),
        None,
        "munshi-transcript reads no codex compaction, so nothing looked",
    );
}

#[test]
fn munshi_own_fixtures_are_healthy_sessions_and_fire_nothing() {
    let sessions = vec![
        claude("munshi/claude-code-2.1.44-normal.jsonl"),
        fold("munshi/copilot-1.0.70-envelope.jsonl", "copilot-cli"),
        fold("munshi/copilot-1.0.76-tool-activity.jsonl", "copilot-cli"),
        // The Copilot pull-B fixture belongs here; its Claude Code twin deliberately does not.
        // That one carries 15+ user records to exercise every slash-command edge case, which is
        // the Babysitting shape by construction — a property of how munshi built the fixture, not
        // a coaching finding. Its review behaviour is asserted directly instead, below.
        fold("munshi/copilot-1.0.76-invocation.jsonl", "copilot-cli"),
    ];
    let fired = rules::evaluate(&sessions);
    assert!(
        fired.is_empty(),
        "short healthy sessions must not trip a coaching rule: {:?}",
        fired.iter().map(|finding| finding.rule).collect::<Vec<_>>(),
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

/// The same line, held against the metric that exists *because* commands repeat. The churn fold
/// compares command strings in memory; a report built from it renders how many times a command
/// ran and never which command it was, so the retry-loop fixture's canaries must not survive
/// either — not in the aggregate lines, not in the evidence, not truncated.
#[test]
fn a_retry_loop_report_names_no_command_it_counted() {
    let session = fold("rules/retry-loop.jsonl", "codex-cli");
    let raw = std::fs::read_to_string(fixture("rules/retry-loop.jsonl")).unwrap();
    assert!(
        raw.contains("CANARY_RETRY_COMMAND"),
        "the fixture carries it"
    );

    let findings = rules::evaluate(std::slice::from_ref(&session));
    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == RuleId::RetryLoop),
        "the report under test must carry the churn finding"
    );
    let source_hash = session.source_hash.clone();
    let markdown = render(&[session], &findings);

    assert!(
        !markdown.contains("CANARY"),
        "a canary token reached the report:\n{markdown}"
    );
    for forbidden in [
        "CANARY_RETRY_COMMAND",
        "CANARY_ONE_OFF_A",
        "cargo test",
        "git status",
        "bash",
        "-lc",
        "/work/fixture",
    ] {
        assert!(
            !markdown.contains(forbidden),
            "`{forbidden}` reached the report:\n{markdown}"
        );
    }
    // What it does say: counts, and a hash to go and read the rest for yourself.
    assert!(markdown.contains("one command run 6 times"), "{markdown}");
    assert!(
        markdown.contains(&format!("`sha256:{source_hash}`")),
        "{markdown}"
    );
}

fn render(sessions: &[SessionMetrics], findings: &[Finding]) -> String {
    let Command::Report(args) = Cli::parse_from(["qanungo", "report", "--last", "30d"]).command
    else {
        panic!("`report` parses as the report command");
    };
    let window: Window = args.last;
    let instrumentation = Instrumentation {
        sync: SyncStats::default(),
        fold_elapsed: Duration::from_millis(3),
        sessions_folded: sessions.len(),
        comparison_sessions_folded: sessions.len(),
        bytes_folded: sessions.iter().map(|session| session.bytes_folded).sum(),
        rule_pack: RulePack::current(),
        patwari_url: "http://127.0.0.1:8080".to_owned(),
        cache_root: PathBuf::from("/tmp/qanungo"),
    };
    Report {
        window: &window,
        generated_at: DateTime::parse_from_rfc3339("2026-08-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        sessions,
        // The canary tests render the same sessions into both windows on purpose: the scoring and
        // trend paths are then exercised by the redaction check too, so a component that started
        // rendering a command string could not slip past it.
        previous: sessions,
        compared: true,
        findings,
        skipped: &[],
        instrumentation: &instrumentation,
    }
    .render()
}

// ---------------------------------------------------------------------------
// Evidence anchors (qanungo #5)
// ---------------------------------------------------------------------------

/// The load-bearing property of the whole slice: **anchors change nothing**.
///
/// The same window is evaluated twice — once as the fold produced it, once with every anchor
/// stripped before any rule sees it — and the two must agree on every finding, every Problem and
/// Action sentence, every evidence line, every count, and every lane score. This is the fixture-level
/// half of the guarantee `qanungo report` proves against production by diffing its Markdown.
#[test]
fn anchors_are_additive_and_change_no_verdict_no_detail_and_no_score() {
    let sessions = vec![
        claude("rules/high-tool-error-rate.jsonl"),
        claude("rules/error-with-planted-secret.jsonl"),
        claude("rules/marathon-session.jsonl"),
        claude("rules/resumed-session.jsonl"),
        claude("rules/babysitting.jsonl"),
        claude("rules/fire-and-forget.jsonl"),
        fold("rules/retry-loop.jsonl", "codex-cli"),
    ];
    let anchored = rules::evaluate(&sessions);
    assert!(
        anchored
            .iter()
            .any(|finding| finding.evidence.iter().any(|line| !line.anchors.is_empty())),
        "the control is worthless unless the anchored fold actually anchored something",
    );

    // The same sessions with the anchors taken away — the fold this crate had before the slice.
    let control: Vec<SessionMetrics> = sessions
        .iter()
        .cloned()
        .map(|mut session| {
            session.anchors = SessionAnchors::default();
            session
        })
        .collect();
    let unanchored = rules::evaluate(&control);

    let stripped: Vec<Finding> = anchored
        .iter()
        .cloned()
        .map(|mut finding| {
            for line in &mut finding.evidence {
                line.anchors.clear();
            }
            finding
        })
        .collect();
    assert_eq!(
        stripped, unanchored,
        "a finding differs by more than its anchors",
    );

    // And the scores the report prints are the same two scorecards.
    let with = Scorecard::fold(&sessions);
    let without = Scorecard::fold(&control);
    for lane in Lane::ALL {
        assert_eq!(
            with.fleet(lane).map(|blend| blend.score),
            without.fleet(lane).map(|blend| blend.score),
            "{lane:?}",
        );
    }
    assert_eq!(RulePack::current().digest(), RulePack::current().digest());
    assert_eq!(
        render(&sessions, &anchored),
        render(&control, &unanchored),
        "the rendered report is byte-identical with and without anchors",
    );
}

/// The error-rate rule anchors the calls that failed — the events it counted — and each one
/// resolves back to that event's own text.
#[test]
fn the_error_rule_anchors_the_failures_it_counted() {
    let session = claude("rules/high-tool-error-rate.jsonl");
    let findings = rules::evaluate(std::slice::from_ref(&session));
    let finding = finding(&findings, RuleId::HighToolErrorRate);
    assert_eq!(finding.rule.evidence_kind(), EvidenceKind::Event);

    let anchors = &finding.evidence[0].anchors;
    assert_eq!(anchors.len(), 6, "six calls failed, six are offered");
    for anchor in anchors {
        assert_eq!(anchor.tool.as_deref(), Some("Bash"));
        assert!(anchor.at.is_some(), "the record was dated");
        assert!(anchor.locator >= 1);
    }
    let locators: Vec<u64> = anchors.iter().map(|anchor| anchor.locator).collect();
    let mut ascending = locators.clone();
    ascending.sort_unstable();
    assert_eq!(locators, ascending, "file order");

    // Every anchor resolves, and resolves to a *failure* carrying that call's own error text.
    let bytes = std::fs::read(fixture("rules/high-tool-error-rate.jsonl")).unwrap();
    for anchor in anchors {
        let excerpt = extract(&bytes, Source::ClaudeCode, anchor.locator)
            .expect("the anchor resolves")
            .redacted(&Redactor::new());
        assert_eq!(excerpt.locator, anchor.locator);
        assert_eq!(excerpt.record, anchor.record);
        assert_eq!(excerpt.tool.as_deref(), Some("Bash"));
        assert_eq!(excerpt.outcome, Some(false));
        assert_eq!(excerpt.event.as_deref(), Some("tool_result"));
        assert!(
            excerpt
                .output
                .as_deref()
                .expect("a failing result carries its text")
                .contains("CANARY_ERROR_TEXT_"),
        );
    }
}

/// The retry-loop rule anchors the runs of the **busiest** command — not the session's other
/// repetition, which its evidence line reports as context and the rule does not decide on. Every
/// anchor resolves to the same command string, which is the whole claim the rule makes.
#[test]
fn the_retry_rule_anchors_the_runs_of_the_one_command_it_counted() {
    let session = fold("rules/retry-loop.jsonl", "codex-cli");
    let findings = rules::evaluate(std::slice::from_ref(&session));
    let finding = finding(&findings, RuleId::RetryLoop);

    let anchors = &finding.evidence[0].anchors;
    assert_eq!(
        anchors.len() as u64,
        session.commands.busiest_runs().unwrap(),
        "one anchor per counted run",
    );

    let bytes = std::fs::read(fixture("rules/retry-loop.jsonl")).unwrap();
    let commands: Vec<String> = anchors
        .iter()
        .map(|anchor| {
            let excerpt = extract(&bytes, Source::Codex, anchor.locator)
                .expect("the anchor resolves")
                .redacted(&Redactor::new());
            excerpt.command.expect("a shell event carries its command")
        })
        .collect();
    assert!(
        commands.windows(2).all(|pair| pair[0] == pair[1]),
        "the anchors point at runs of one value, which is what the rule counted",
    );
    // The transcript's other commands ran too, and are not what fired the rule.
    let raw = String::from_utf8(bytes).unwrap();
    assert!(raw.contains("CANARY_ONE_OFF_A"), "the fixture has one-offs");
    assert!(
        !commands[0].contains("CANARY_ONE_OFF_A"),
        "a one-off is not a run of the busiest command",
    );
}

/// Fire-and-forget is honestly split: its ratio component is a shape, its `errors > 0` component
/// counts events, so it offers anchors *and* is marked as mixed.
#[test]
fn fire_and_forget_anchors_its_error_component_only() {
    let session = claude("rules/fire-and-forget.jsonl");
    let findings = rules::evaluate(std::slice::from_ref(&session));
    let finding = finding(&findings, RuleId::FireAndForget);
    assert_eq!(finding.rule.evidence_kind(), EvidenceKind::Mixed);
    assert_eq!(
        finding.evidence[0].anchors.len() as u64,
        session.tools.total.errors,
        "one anchor per failure, all four under the cap",
    );
}

/// A rule that measured a shape anchors nothing at all, in every session it fired on.
#[test]
fn session_shaped_rules_anchor_nothing() {
    for (relative, rule) in [
        ("rules/marathon-session.jsonl", RuleId::MarathonSession),
        ("rules/resumed-session.jsonl", RuleId::ResumedSession),
        ("rules/babysitting.jsonl", RuleId::Babysitting),
        // Compaction churn counts records rather than measuring a shape, and is still structural:
        // a marker carries no verbatim to excerpt, and its record has no ordinal in the tool-event
        // space a locator is keyed by. What it shows a reader is the count and the session's shape.
        (
            "munshi/claude-code-2.1.235-compaction.jsonl",
            RuleId::CompactionChurn,
        ),
    ] {
        let session = claude(relative);
        let findings = rules::evaluate(std::slice::from_ref(&session));
        let finding = finding(&findings, rule);
        assert_eq!(finding.rule.evidence_kind(), EvidenceKind::Structural);
        for line in &finding.evidence {
            assert!(line.anchors.is_empty(), "{relative} anchored an event");
        }
        // And it has the structure to show instead.
        assert!(session.sittings().is_some(), "{relative}");
        assert!(
            !session.activity.sitting_boundaries().is_empty(),
            "{relative}"
        );
    }
}

/// Reads one anchored event back out of transcript bytes, as the excerpt route does.
fn extract(bytes: &[u8], source: Source, locator: u64) -> Option<qanungo::evidence::RawExcerpt> {
    qanungo::evidence::extract(source, 2, std::io::BufReader::new(bytes), locator)
        .expect("v2 is supported")
}

// ---------------------------------------------------------------------------
// Shipped without review (qanungo #4, munshi#77 pull B)
// ---------------------------------------------------------------------------

/// **Ship detection.** The commit surface, on the three shapes the archive actually writes: the
/// compound `git add -A && … && git commit …` line, a `git -C <path> commit` with a global flag
/// before the subcommand, and a plain `git commit`.
///
/// The negative in the same fixture is the one that matters: `git log --oneline -5 | grep -i
/// commit` runs no commit, and a substring test for "git commit" would have counted it. So would
/// one that only looked at the head of the line.
#[test]
fn a_commit_is_detected_on_every_shape_the_archive_writes() {
    let session = claude("rules/unreviewed-ship.jsonl");
    let reviews = &session.reviews;
    assert!(reviews.observable, "claude-code types every review surface");
    assert_eq!(
        reviews.commits, 3,
        "three commits: compound, -C-prefixed, and plain — and not the `git log | grep commit`",
    );
    assert!(reviews.shipped());
    assert_eq!(reviews.review_passes, 0, "nothing reviewed it");
    assert_eq!(
        reviews.skill_invocations, 1,
        "one skill ran, and it was not a review"
    );
}

/// The classifier itself, at the unit level — the decision `munshi-transcript` explicitly hands
/// to this consumer.
#[test]
fn review_pass_classification_admits_review_tokens_and_nothing_else() {
    for name in [
        "code-review",
        "security-review",
        "review",
        "pr-review",
        "Code-Review",
    ] {
        assert!(metrics::is_review_pass(name), "{name} is a review pass");
    }
    for name in [
        // Real skill names from the mirror that are not reviews.
        "artifact-design",
        "session-recall",
        "run",
        "claude-api",
        "update-config",
        // The quality pass that is deliberately excluded: it disclaims bug-hunting.
        "simplify",
        // The substring trap. `interview-prep` contains "review".
        "interview-prep",
        "interview",
        "previewer",
    ] {
        assert!(
            !metrics::is_review_pass(name),
            "{name} is not a review pass"
        );
    }
}

/// **Slash commands never count as a review pass**, even when they are named like one.
///
/// The borrowed munshi fixture carries `<command-name>/security-review</command-name>` as a typed
/// slash command and a `SlashCommand` *tool* invoking `/code-review`. Neither is a review pass
/// here: in this harness a review is invoked through the `Skill` tool. The slash surface's job in
/// this lane is to make the harness *observable*, never to be counted.
#[test]
fn a_review_named_slash_command_is_not_a_review_pass() {
    let session = claude("munshi/claude-code-2.1.235-invocation.jsonl");
    let reviews = &session.reviews;
    assert!(reviews.observable);
    // Exactly two, and the exactness is the whole point of the test. The fixture's skill surface
    // holds `code-review` and `security-review` — those two are counted — beside `simplify`,
    // `artifact-design` and `run`, which are not, and beside the two traps this test is named for:
    // a `SlashCommand` *tool* invoking `/code-review`, and a typed
    // `<command-name>/security-review</command-name>`. A `>=` bound would pass even if both traps
    // started counting, which is precisely the regression it exists to catch.
    assert_eq!(
        reviews.review_passes, 2,
        "exactly the two Skill-invoked reviews, and neither slash-shaped decoy",
    );
    // It ships nothing, so it is not in the rate at all however much it reviewed.
    assert_eq!(reviews.commits, 0, "`cargo test` is not a commit");
    assert_eq!(
        RuleId::UnreviewedShip.verdict(&session),
        None,
        "a session that shipped nothing is not eligible",
    );
}

/// **Eligibility, per harness.** Copilot types its skills but records slash commands as unmarked
/// prose, so it is *partially* observable — and partial is not enough to assert that no review
/// ran. Its sessions leave the rate entirely rather than scoring a failure nothing observed.
///
/// The borrowed fixture is exactly that shape: `/chronicle improve` sits in a `user.message` as
/// prose with no marker, beside a real `skill.invoked`.
#[test]
fn copilot_is_couldnt_look_because_its_slash_surface_is_untyped() {
    let session = fold("munshi/copilot-1.0.76-invocation.jsonl", "copilot-cli");
    let reviews = &session.reviews;
    assert!(
        !reviews.observable,
        "copilot's slash surface is prose, so `copilot ran no review` is unsayable",
    );
    assert!(
        reviews.skill_invocations > 0,
        "its skill surface *is* typed — this is partial observability, not none",
    );
    assert_eq!(
        RuleId::UnreviewedShip.verdict(&session),
        None,
        "couldn't-look, whatever it shipped",
    );

    // And the claim holds even when such a session ships: eligibility is the harness first.
    let mut shipped = session;
    shipped.reviews.commits = 4;
    assert!(shipped.reviews.shipped());
    assert!(!shipped.reviews.shipped_observably());
    assert_eq!(RuleId::UnreviewedShip.verdict(&shipped), None);
}

/// The three verdicts, on three fixtures: **fired**, **did not fire**, **could not look**.
#[test]
fn the_rule_answers_three_ways() {
    let unreviewed = claude("rules/unreviewed-ship.jsonl");
    assert_eq!(RuleId::UnreviewedShip.verdict(&unreviewed), Some(true));

    let reviewed = claude("rules/reviewed-ship.jsonl");
    assert_eq!(
        RuleId::UnreviewedShip.verdict(&reviewed),
        Some(false),
        "it shipped and a review pass ran",
    );
    assert_eq!(reviewed.reviews.commits, 1);
    assert_eq!(reviewed.reviews.review_passes, 1);

    let copilot = fold("munshi/copilot-1.0.76-invocation.jsonl", "copilot-cli");
    assert_eq!(RuleId::UnreviewedShip.verdict(&copilot), None);
}

/// The finding itself: it fires, it is the only rule this fixture trips, and its detail line says
/// what shipped and what ran instead of a review.
#[test]
fn unreviewed_ship_fires_and_says_what_it_ran_instead() {
    let session = claude("rules/unreviewed-ship.jsonl");
    let findings = fires_only(&session, RuleId::UnreviewedShip);
    let finding = finding(&findings, RuleId::UnreviewedShip);
    assert_eq!(finding.evidence.len(), 1);
    assert_eq!(finding.evidence[0].source_hash, session.source_hash);
    assert_eq!(
        finding.evidence[0].detail,
        "committed 3 times; no review pass among 1 skill invocation",
    );

    // The reviewed twin trips nothing at all.
    let reviewed = claude("rules/reviewed-ship.jsonl");
    assert!(
        rules::evaluate(std::slice::from_ref(&reviewed)).is_empty(),
        "a session that reviewed before shipping is clean",
    );
}

/// **The evidence decision, end to end.** The rule is `Mixed`: its ship half anchors the commits
/// it counted, and its review half — an absence — anchors nothing because there is nothing to
/// anchor.
///
/// Every anchor resolves through the ordinary excerpt route, and the planted credential in the
/// first commit message is scrubbed on the way out. That is the reason anchoring a commit is
/// defensible at all: a commit message is operator-written text, and the route that serves it
/// already owes it the redaction layer.
#[test]
fn the_unreviewed_ship_rule_anchors_the_commits_and_redacts_their_messages() {
    let session = claude("rules/unreviewed-ship.jsonl");
    let findings = rules::evaluate(std::slice::from_ref(&session));
    let finding = finding(&findings, RuleId::UnreviewedShip);
    assert_eq!(finding.rule.evidence_kind(), EvidenceKind::Mixed);

    let anchors = &finding.evidence[0].anchors;
    assert_eq!(anchors.len(), 3, "one anchor per counted ship");
    let locators: Vec<u64> = anchors.iter().map(|anchor| anchor.locator).collect();
    let mut ascending = locators.clone();
    ascending.sort_unstable();
    assert_eq!(locators, ascending, "file order");
    for anchor in anchors {
        assert_eq!(anchor.tool.as_deref(), Some("Bash"));
        assert!(anchor.at.is_some());
    }

    let bytes = std::fs::read(fixture("rules/unreviewed-ship.jsonl")).unwrap();
    let redactor = Redactor::new();
    let mut saw_subject = false;
    for anchor in anchors {
        let excerpt = extract(&bytes, Source::ClaudeCode, anchor.locator)
            .expect("the anchor resolves")
            .redacted(&redactor);
        assert_eq!(excerpt.locator, anchor.locator);
        assert_eq!(excerpt.record, anchor.record);
        assert_eq!(excerpt.tool.as_deref(), Some("Bash"));
        let rendered = format!("{excerpt:?}");
        assert!(
            !rendered.contains("ghp_CANARYCANARYCANARYCANARYCANARYCANARY"),
            "the planted credential survived the scrub: {rendered}",
        );
        if rendered.contains("CANARY_COMMIT_SUBJECT") {
            saw_subject = true;
        }
    }
    assert!(
        saw_subject,
        "the commit message is served — that is what makes the anchor worth having",
    );
}
