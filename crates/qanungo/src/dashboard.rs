//! The dashboard's payload: the coaching lane's own numbers, as JSON.
//!
//! # The redaction line (hard), restated for a served surface
//!
//! This payload carries **lane scores, rule ids, counts, rendered aggregates, archive-stated
//! identifiers, and `sha256` content hashes — nothing else**. No transcript text, no summary
//! prose, no command strings, no error text, no file paths, not truncated and not in evidence.
//!
//! It is the same line [`crate::report`] holds and it is held the same way: by *construction*.
//! Every field below is read off a [`Folded`](crate::command::Folded), whose types have already
//! reduced a transcript to counts, timestamps, and a digest — the fold drops content before this
//! module can see it, so there is no string here to filter. The P0 exemption from qanungo #8
//! therefore applies unchanged, which is why the dashboard flattens no redaction flag: a scrub
//! over a payload with no content in it would be decoration on a security control.
//!
//! Two things make that stronger than a promise. The payload states it about itself —
//! `provenance.renders_verbatim` is `false` — and a fixture archive stuffed with canary strings is
//! serialized through this module in `tests/dashboard.rs`, so a field that started carrying
//! transcript text fails a test rather than reaching a browser.
//!
//! **No raw Patwari links, anywhere.** Patwari serves unredacted blobs and never redacts, so a
//! deep link from this page to an artifact would hand any tailnet device the whole transcript —
//! the correction the 2026-08-24 grilling made to qanungo #5. The archive's base URL appears once,
//! in the provenance block, as *text saying which archive these numbers came from*. The page
//! renders it as text and builds no link from it; the recall funnel stays a CLI affordance, where
//! the user's own shell already has raw access.
//!
//! # It computes nothing
//!
//! Every number here was computed by [`fold_coaching`](crate::command::fold_coaching),
//! [`rules::evaluate`](crate::rules::evaluate), and [`Scorecard::fold`] — the same three calls
//! `qanungo report` makes, on the same window pair. This module chooses a shape and a key name; it
//! does not choose a value. The arrow rules in particular are not re-derived: a per-harness trend
//! is [`Trend::between`] and a fleet trend is [`Blend::comparable`](crate::scoring::Blend::comparable),
//! the same two functions the Markdown table draws its `▲` from.
//!
//! # What V1 leaves out
//!
//! Redacted evidence excerpts (qanungo #8's own surface), the standup and cost views over folds
//! that already ship, scope selection by repository and harness, the timeline, and the 7×24 heatmap
//! — the last blocked on munshi#77's local-offset pull, because UTC misplaces every late-night
//! claim the view exists to make. Each is a later slice over this same payload, not a change to it.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::cli::{Refresh, Window};
use crate::command::Folded;
use crate::format;
use crate::metrics::Totals;
use crate::redaction::PATTERN_REVISION;
use crate::report::{self, stamp};
use crate::rules::Finding;
use crate::scoring::{Lane, LaneScore, Scorecard, Trend};

/// What the refresh loop knows and the fold does not: which run this is, when it landed, and
/// whether the last attempt to take a new one failed.
#[derive(Debug, Clone, Copy)]
pub struct Refreshed {
    /// Bumped on every swap of the served payload. An SSE client uses it to tell a genuine refresh
    /// from a reconnection.
    pub generation: u64,
    /// When this payload was published.
    pub at: DateTime<Utc>,
    /// When the *first* of the current run of failed refreshes happened, if the loop is currently
    /// failing. `None` means the numbers below are as fresh as the interval allows.
    ///
    /// Why the first failure rather than the last: what a reader needs is how old the numbers are,
    /// and that is the age of the last *success*, which the first failure dates. Reporting the most
    /// recent failure would make a dashboard that has been broken for a day look a minute old.
    pub stale_since: Option<DateTime<Utc>>,
}

/// Builds the served JSON document.
///
/// One call per refresh, never per request: the body is serialized once and handed to every reader
/// as bytes, so a hundred open tabs cost one fold and one serialization between them.
pub fn payload(window: &Window, refresh: &Refresh, folded: &Folded, refreshed: Refreshed) -> Value {
    // The same two questions the report asks in the same order: is there a comparison window at
    // all, and if so what did it score? A window too long to place an equal-length one before it
    // has no `before`, and therefore no arrow anywhere on the page.
    let comparison_opens_at = folded
        .compared
        .then(|| window.comparison_opens_at(folded.generated_at))
        .flatten();
    let now = Scorecard::fold(&folded.sessions);
    let before = comparison_opens_at.map(|_| Scorecard::fold(&folded.previous));
    let columns = report::harness_columns(&now, before.as_ref());

    json!({
        "window": {
            "last": window.to_string(),
            "opens_at": stamp(window.opens_at(folded.generated_at)),
            "comparison_opens_at": comparison_opens_at.map(stamp),
            "generated_at": stamp(folded.generated_at),
            "compared": folded.compared,
        },
        "sessions": sessions_value(folded),
        "lanes": Lane::ALL
            .iter()
            .map(|lane| lane_value(*lane, &now, before.as_ref(), &columns))
            .collect::<Vec<_>>(),
        "findings": folded.findings.iter().map(finding_value).collect::<Vec<_>>(),
        "provenance": provenance_value(window, refresh, folded, refreshed),
    })
}

/// How much of the window each harness contributed, so a reader can weigh a per-harness score
/// against the sample behind it.
///
/// Harness labels are the *archive's* strings, so they go through [`format::identifier`] on the way
/// out — the same clamp the report's Gaps lines pass through, for the same reason: a manifest can
/// state whatever it likes, and a served page is a rendering surface a peer does not get to choose
/// characters on.
fn sessions_value(folded: &Folded) -> Value {
    let totals = Totals::fold(&folded.sessions);
    let by_harness: BTreeMap<String, usize> = totals
        .by_agent
        .iter()
        .map(|(agent, count)| (format::identifier(agent), *count))
        .collect();
    json!({
        "folded": folded.instrumentation.sessions_folded,
        "comparison_folded": folded.instrumentation.comparison_sessions_folded,
        "by_harness": by_harness,
    })
}

/// One practice lane: the fleet number, and the per-harness split behind it.
///
/// The three states a lane can be in are kept apart here exactly as the report's table keeps them
/// apart, because collapsing any two of them is how a score becomes a lie. `scored` is a reading;
/// `no-reading` is a fed lane whose signals were all silent this window; `not-scored` is a lane
/// nothing types a signal for at all, and it carries the sentence naming the pull that would light
/// it up. None of the three is ever a zero.
fn lane_value(
    lane: Lane,
    now: &Scorecard,
    before: Option<&Scorecard>,
    columns: &[String],
) -> Value {
    let fleet = match now.fleet(lane) {
        Some(blend) => {
            let comparable = blend.comparable(before.and_then(|card| card.fleet(lane)));
            json!({
                "state": "scored",
                "score": blend.score,
                "harnesses": blend.harnesses.iter().map(|agent| format::identifier(agent)).collect::<Vec<_>>(),
                "trend": trend_value(Trend::between(blend.score, comparable)),
            })
        }
        None if lane.untyped().is_some() => json!({ "state": "not-scored" }),
        None => json!({ "state": "no-reading" }),
    };
    json!({
        "key": lane.key(),
        "title": lane.title(),
        "reason": lane.untyped(),
        "fleet": fleet,
        "harnesses": columns
            .iter()
            .map(|column| harness_value(lane, column, now, before))
            .collect::<Vec<_>>(),
    })
}

/// One harness's standing in one lane.
///
/// `no-sessions` is its own state rather than a missing entry: the columns are the union of both
/// windows' harnesses, so a harness that stopped appearing is a fact the page shows instead of one
/// it hides — the same reason [`report::harness_columns`] takes the union in the first place.
fn harness_value(
    lane: Lane,
    source_agent: &str,
    now: &Scorecard,
    before: Option<&Scorecard>,
) -> Value {
    let label = format::identifier(source_agent);
    let Some(harness) = now.harness(source_agent) else {
        return json!({
            "source_agent": label,
            "sessions": 0,
            "state": "no-sessions",
        });
    };
    let score = harness.lane(lane);
    let earlier = before
        .and_then(|card| card.harness(source_agent))
        .and_then(|harness| harness.lane(lane).score());
    let (state, value, trend) = match score {
        LaneScore::Scored { score, .. } => (
            "scored",
            Some(*score),
            trend_value(Trend::between(*score, earlier)),
        ),
        LaneScore::NoReading { .. } => ("no-reading", None, Value::Null),
        LaneScore::Untyped(_) => ("not-scored", None, Value::Null),
    };
    json!({
        "source_agent": label,
        "sessions": harness.sessions,
        "state": state,
        "score": value,
        "trend": trend,
        "components": score
            .components()
            .iter()
            .map(|component| json!({
                "label": component.label,
                "detail": component.detail,
                "cost": component.cost,
            }))
            .collect::<Vec<_>>(),
    })
}

/// A movement, or the explicit absence of one. `null` is the only honest answer when the
/// comparison window could not measure the lane — see [`Trend::between`].
fn trend_value(trend: Option<Trend>) -> Value {
    match trend {
        Some(trend) => json!({
            "direction": trend.direction().key(),
            "glyph": trend.direction().glyph(),
            "points": trend.magnitude(),
            "was": trend.was,
        }),
        None => Value::Null,
    }
}

/// One finding: the rule that fired, the report's own Problem and Action wording, how many sessions
/// it fired on, and the hashes of those sessions.
///
/// The Problem and Action strings are lifted from [`crate::rules`] rather than re-worded for the
/// web, so the page and the CLI give the same advice in the same sentences. The per-session
/// evidence *detail* lines the Markdown carries are deliberately not here: V1 renders Problem,
/// Action, and hash references, and a hash is the whole of what this surface offers as evidence
/// until the redacted-excerpt slice lands behind qanungo #8.
fn finding_value(finding: &Finding) -> Value {
    json!({
        "rule": finding.rule.key(),
        "title": finding.rule.title(),
        "problem": finding.problem,
        "action": finding.action,
        "sessions_affected": finding.evidence.len(),
        "source_hashes": finding
            .evidence
            .iter()
            .map(|evidence| evidence.source_hash.clone())
            .collect::<Vec<_>>(),
    })
}

/// What the numbers cost and what they may be compared against.
///
/// The instrumentation footer of every CLI run, plus the three facts only a long-lived process has:
/// which refresh this is, when it landed, and whether the last attempt failed. Durations and byte
/// counts arrive **pre-rendered** by [`crate::format`] alongside their raw values — the renderings
/// are that module's job, and a second implementation of them in JavaScript would drift from the
/// footer this block is supposed to mirror.
fn provenance_value(
    window: &Window,
    refresh: &Refresh,
    folded: &Folded,
    refreshed: Refreshed,
) -> Value {
    let instrumentation = &folded.instrumentation;
    json!({
        "window": window.to_string(),
        "sessions_listed": instrumentation.sync.sessions_listed,
        "sessions_folded": instrumentation.sessions_folded,
        "comparison_sessions_folded": instrumentation.comparison_sessions_folded,
        "fold": format::elapsed(instrumentation.fold_elapsed),
        "fold_millis": u64::try_from(instrumentation.fold_elapsed.as_millis()).unwrap_or(u64::MAX),
        "sync": format::elapsed(instrumentation.sync.elapsed),
        "sync_millis": u64::try_from(instrumentation.sync.elapsed.as_millis()).unwrap_or(u64::MAX),
        "bytes_folded": format::bytes(instrumentation.bytes_folded),
        "bytes_transferred": format::bytes(instrumentation.sync.bytes_transferred),
        "cache_hits": instrumentation.sync.cache_hits,
        "cache_misses": instrumentation.sync.cache_misses,
        "rule_pack": instrumentation.rule_pack.stamp(),
        "rule_pack_digest": instrumentation.rule_pack.digest(),
        "redaction_pattern_revision": PATTERN_REVISION,
        // Text, never a link. See the module docs: Patwari serves unredacted blobs, so a browser
        // deep-link into it is a transcript disclosure wearing a convenience.
        "patwari_url": instrumentation.patwari_url,
        "cache_root": instrumentation.cache_root.display().to_string(),
        "refresh_interval": refresh.to_string(),
        "refreshed_at": stamp(refreshed.at),
        "generation": refreshed.generation,
        "stale_since": refreshed.stale_since.map(stamp),
        "gaps": folded
            .skipped
            .iter()
            .map(|note| json!({ "count": note.count, "reason": note.reason }))
            .collect::<Vec<_>>(),
        // A machine-checkable statement of this module's own contract. If it is ever true, this
        // page needs a redaction story before it needs anything else.
        "renders_verbatim": false,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use chrono::TimeDelta;
    use clap::Parser;
    use munshi_transcript::SessionSummary;

    use super::*;
    use crate::cli::{Cli, Command};
    use crate::metrics::{Activity, CommandChurn, SessionMetrics, ToolOutcomes};
    use crate::report::Instrumentation;
    use crate::rules;
    use crate::scoring::RulePack;
    use crate::sync::SyncStats;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn window(spelling: &str) -> Window {
        let Command::Dashboard(args) =
            Cli::parse_from(["qanungo", "dashboard", "--last", spelling]).command
        else {
            panic!("`dashboard` parses as the dashboard command");
        };
        args.last
    }

    fn refresh() -> Refresh {
        let Command::Dashboard(args) = Cli::parse_from(["qanungo", "dashboard"]).command else {
            panic!("`dashboard` parses as the dashboard command");
        };
        args.refresh
    }

    /// The same `hygiene_window` shape the scoring and report tests use: `count` sessions, the
    /// first `marathons` of which work one long unbroken push.
    fn hygiene_window(source_agent: &str, count: usize, marathons: usize) -> Vec<SessionMetrics> {
        let first = at("2026-08-10T09:00:00Z");
        let step = crate::rules::thresholds::IDLE_GAP;
        let marathon = crate::rules::thresholds::MARATHON_SITTING_ACTIVE;
        (0..count)
            .map(|index| {
                let worked = if index < marathons {
                    marathon + step
                } else {
                    marathon / 4
                };
                let steps = worked.num_minutes() / step.num_minutes();
                let timestamps: Vec<_> = (0..=steps).map(|n| first + step * n as i32).collect();
                let last = *timestamps.last().expect("at least the first record");
                SessionMetrics {
                    source_hash: format!("{index:02x}").repeat(32),
                    source_agent: source_agent.to_owned(),
                    summary: SessionSummary {
                        user_requests: 4,
                        tool_activities: 20,
                        first_timestamp: Some(first),
                        last_timestamp: Some(last),
                        ..SessionSummary::default()
                    },
                    tools: ToolOutcomes::default(),
                    activity: Activity::over(timestamps),
                    commands: CommandChurn::default(),
                    bytes_folded: 1024,
                }
            })
            .collect()
    }

    fn folded(sessions: Vec<SessionMetrics>, previous: Vec<SessionMetrics>) -> Folded {
        let findings = rules::evaluate(&sessions);
        Folded {
            generated_at: at("2026-08-17T12:00:00Z"),
            instrumentation: Instrumentation {
                sync: SyncStats {
                    sessions_listed: sessions.len() + previous.len(),
                    cache_hits: 2,
                    cache_misses: 1,
                    bytes_transferred: 4096,
                    elapsed: Duration::from_millis(120),
                },
                fold_elapsed: Duration::from_millis(7),
                sessions_folded: sessions.len(),
                comparison_sessions_folded: previous.len(),
                bytes_folded: 8192,
                rule_pack: RulePack::current(),
                patwari_url: "http://127.0.0.1:8080".to_owned(),
                cache_root: PathBuf::from("/tmp/qanungo"),
            },
            compared: true,
            sessions,
            previous,
            findings,
            skipped: Vec::new(),
        }
    }

    fn refreshed() -> Refreshed {
        Refreshed {
            generation: 3,
            at: at("2026-08-17T12:00:00Z"),
            stale_since: None,
        }
    }

    fn built(sessions: Vec<SessionMetrics>, previous: Vec<SessionMetrics>) -> Value {
        payload(
            &window("7d"),
            &refresh(),
            &folded(sessions, previous),
            refreshed(),
        )
    }

    fn lane<'a>(payload: &'a Value, key: &str) -> &'a Value {
        payload["lanes"]
            .as_array()
            .expect("lanes is an array")
            .iter()
            .find(|lane| lane["key"] == key)
            .unwrap_or_else(|| panic!("{key} is a lane"))
    }

    /// Every lane keeps its place, in the order qanungo #4 names them, whether or not it scored.
    #[test]
    fn all_five_lanes_are_present_in_the_order_the_issue_names_them() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let keys: Vec<_> = payload["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|lane| lane["key"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            keys,
            vec![
                "prompt-quality",
                "session-hygiene",
                "code-review",
                "tool-mastery",
                "context-management",
            ]
        );
    }

    /// A scored lane carries the score, the per-harness split, and the readings that produced it —
    /// the same numbers the report's table and its "why the scores are what they are" section show.
    #[test]
    fn a_scored_lane_carries_its_score_and_the_readings_behind_it() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let hygiene = lane(&payload, "session-hygiene");
        assert_eq!(hygiene["fleet"]["state"], "scored");
        assert_eq!(hygiene["fleet"]["score"], 50);
        assert_eq!(hygiene["reason"], Value::Null);

        let harness = &hygiene["harnesses"][0];
        assert_eq!(harness["source_agent"], "claude-code");
        assert_eq!(harness["sessions"], 20);
        assert_eq!(harness["state"], "scored");
        assert_eq!(harness["score"], 50);
        assert_eq!(harness["components"][0]["label"], "Marathon session");
        assert_eq!(harness["components"][0]["cost"], 50.0);
        assert!(
            harness["components"][0]["detail"]
                .as_str()
                .unwrap()
                .starts_with("fired on 5 of 20"),
        );
    }

    /// A lane nothing types a signal for is never a zero and never a hundred: it is `not-scored`,
    /// in every column, carrying the sentence that names the pull it is waiting for.
    #[test]
    fn an_unfed_lane_is_not_scored_everywhere_and_says_what_it_waits_for() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        for key in ["code-review", "context-management"] {
            let lane = lane(&payload, key);
            assert_eq!(lane["fleet"]["state"], "not-scored", "{key}");
            assert_eq!(lane["fleet"]["score"], Value::Null, "{key}");
            assert_eq!(lane["harnesses"][0]["state"], "not-scored", "{key}");
            assert_eq!(lane["harnesses"][0]["score"], Value::Null, "{key}");
            assert!(
                lane["reason"]
                    .as_str()
                    .expect("an unfed lane says why")
                    .contains("no signal typed for this lane yet"),
                "{key}",
            );
        }
    }

    /// A fed lane whose signals were all silent is a third state, and must not read as either of
    /// the other two. Five sessions with no tool outcome and no command is exactly that.
    #[test]
    fn a_fed_but_silent_lane_reads_as_no_reading_rather_than_as_a_hundred() {
        let payload = built(hygiene_window("claude-code", 5, 0), Vec::new());
        let mastery = lane(&payload, "tool-mastery");
        assert_eq!(mastery["fleet"]["state"], "no-reading");
        assert_eq!(mastery["fleet"]["score"], Value::Null);
        assert_eq!(mastery["harnesses"][0]["state"], "no-reading");
        assert_eq!(
            mastery["reason"],
            Value::Null,
            "the lane is fed; it was silent"
        );
    }

    /// An arrow appears where both windows measured the lane, points the right way, and carries
    /// the size of the move and the score it moved from.
    #[test]
    fn a_lane_measured_in_both_windows_carries_a_trend() {
        let improved = built(
            hygiene_window("claude-code", 20, 2),
            hygiene_window("claude-code", 20, 5),
        );
        let trend = &lane(&improved, "session-hygiene")["harnesses"][0]["trend"];
        assert_eq!(trend["direction"], "up");
        assert_eq!(trend["glyph"], "▲");
        assert_eq!(trend["points"], 30);
        assert_eq!(trend["was"], 50);
        assert_eq!(
            lane(&improved, "session-hygiene")["fleet"]["trend"]["direction"],
            "up"
        );

        let worsened = built(
            hygiene_window("claude-code", 20, 5),
            hygiene_window("claude-code", 20, 2),
        );
        assert_eq!(
            lane(&worsened, "session-hygiene")["harnesses"][0]["trend"]["direction"],
            "down",
        );

        let flat = built(
            hygiene_window("claude-code", 20, 3),
            hygiene_window("claude-code", 20, 3),
        );
        let flat = &lane(&flat, "session-hygiene")["harnesses"][0]["trend"];
        assert_eq!(flat["direction"], "flat");
        assert_eq!(flat["points"], 0);
    }

    /// The rule the arrows live by: a lane the comparison window could not measure gets `null`,
    /// never an arrow drawn against nothing.
    #[test]
    fn a_lane_the_comparison_window_could_not_measure_carries_no_trend() {
        // Two eligible sessions is under the minimum a fire rate needs, so the earlier window has
        // no reading to compare against.
        let payload = built(
            hygiene_window("claude-code", 20, 5),
            hygiene_window("claude-code", 2, 2),
        );
        let hygiene = lane(&payload, "session-hygiene");
        assert_eq!(hygiene["harnesses"][0]["score"], 50);
        assert_eq!(hygiene["harnesses"][0]["trend"], Value::Null);
        assert_eq!(hygiene["fleet"]["trend"], Value::Null);

        // And with no comparison window folded at all, nothing on the page carries one.
        let alone = built(hygiene_window("claude-code", 20, 5), Vec::new());
        assert_eq!(
            lane(&alone, "session-hygiene")["fleet"]["trend"],
            Value::Null
        );
    }

    /// The fleet blend is the unweighted mean of the harness scores, and its trend appears only
    /// when the same harnesses blended it in both windows — a roster change moves the mean with
    /// nobody's behaviour behind it.
    #[test]
    fn a_fleet_trend_needs_the_same_roster_on_both_sides() {
        let mut both = hygiene_window("claude-code", 20, 10);
        both.extend(hygiene_window("copilot-cli", 20, 0));
        let mut earlier_both = hygiene_window("claude-code", 20, 10);
        earlier_both.extend(hygiene_window("copilot-cli", 20, 0));

        let same_roster = built(both.clone(), earlier_both);
        let fleet = &lane(&same_roster, "session-hygiene")["fleet"];
        assert_eq!(fleet["score"], 75, "the unweighted mean of 50 and 100");
        assert_eq!(fleet["harnesses"], json!(["claude-code", "copilot-cli"]));
        assert_eq!(fleet["trend"]["direction"], "flat");

        // The comparison window is one harness short: the blends are means over different rosters
        // and must not be compared at all.
        let changed_roster = built(both, hygiene_window("claude-code", 20, 10));
        assert_eq!(
            lane(&changed_roster, "session-hygiene")["fleet"]["trend"],
            Value::Null,
        );
    }

    /// A harness that scored last window and contributed nothing to this one keeps its column and
    /// says so, rather than vanishing and quietly changing what the fleet number is a mean of.
    #[test]
    fn a_harness_that_stopped_appearing_keeps_its_column() {
        let payload = built(hygiene_window("claude-code", 20, 5), {
            let mut previous = hygiene_window("claude-code", 20, 5);
            previous.extend(hygiene_window("copilot-cli", 20, 0));
            previous
        });
        let harnesses = lane(&payload, "session-hygiene")["harnesses"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(harnesses.len(), 2);
        assert_eq!(harnesses[1]["source_agent"], "copilot-cli");
        assert_eq!(harnesses[1]["state"], "no-sessions");
        assert_eq!(harnesses[1]["sessions"], 0);
    }

    /// A finding carries the rule's identity, the report's own wording, the count, and the hashes —
    /// and nothing that could only have come from a transcript.
    #[test]
    fn a_finding_carries_problem_action_counts_and_hashes() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let findings = payload["findings"].as_array().unwrap();
        let marathon = findings
            .iter()
            .find(|finding| finding["rule"] == "marathon-session")
            .expect("the marathon rule fires on this window");
        assert_eq!(marathon["title"], "Marathon session");
        assert_eq!(marathon["sessions_affected"], 5);
        assert_eq!(marathon["source_hashes"].as_array().unwrap().len(), 5);
        assert!(
            marathon["problem"]
                .as_str()
                .unwrap()
                .starts_with("5 of 20 folded sessions worked for more than"),
        );
        assert!(
            marathon["action"]
                .as_str()
                .unwrap()
                .starts_with("Split the work at the next natural boundary"),
        );
        for hash in marathon["source_hashes"].as_array().unwrap() {
            let hash = hash.as_str().unwrap();
            assert_eq!(hash.len(), 64);
            assert!(hash.chars().all(|character| character.is_ascii_hexdigit()));
        }
    }

    /// The provenance block is the CLI's instrumentation footer plus the three facts only a
    /// long-lived process has, and it states this module's own contract about itself.
    #[test]
    fn the_provenance_block_carries_the_footer_and_the_refresh() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        let provenance = &payload["provenance"];
        assert_eq!(provenance["window"], "7d");
        assert_eq!(provenance["sessions_folded"], 20);
        assert_eq!(provenance["fold"], "7 ms");
        assert_eq!(provenance["sync"], "120 ms");
        assert_eq!(provenance["bytes_folded"], "8.0 KiB");
        assert_eq!(provenance["cache_hits"], 2);
        assert_eq!(provenance["cache_misses"], 1);
        assert_eq!(provenance["rule_pack"], RulePack::current().stamp());
        assert_eq!(provenance["redaction_pattern_revision"], PATTERN_REVISION);
        assert_eq!(provenance["refresh_interval"], "5m");
        assert_eq!(provenance["generation"], 3);
        assert_eq!(provenance["stale_since"], Value::Null);
        assert_eq!(provenance["renders_verbatim"], false);
    }

    /// A refresh that failed does not blank the page and does not pretend to be fresh: the last
    /// good numbers stay, dated by when they stopped being current.
    #[test]
    fn a_failing_refresh_dates_the_numbers_rather_than_hiding_them() {
        let stale = payload(
            &window("7d"),
            &refresh(),
            &folded(hygiene_window("claude-code", 20, 5), Vec::new()),
            Refreshed {
                generation: 9,
                at: at("2026-08-17T12:00:00Z"),
                stale_since: Some(at("2026-08-17T11:30:00Z")),
            },
        );
        assert_eq!(stale["provenance"]["stale_since"], "2026-08-17T11:30:00Z");
        assert_eq!(stale["lanes"].as_array().unwrap().len(), 5);
        assert_eq!(stale["sessions"]["folded"], 20);
    }

    /// A window too long to place an equal-length one before it has no comparison window, so the
    /// payload says so and carries no arrow anywhere.
    #[test]
    fn a_window_with_no_comparison_says_so_and_draws_nothing() {
        let mut folded = folded(hygiene_window("claude-code", 20, 5), Vec::new());
        folded.compared = false;
        let payload = payload(&window("7d"), &refresh(), &folded, refreshed());
        assert_eq!(payload["window"]["compared"], false);
        assert_eq!(payload["window"]["comparison_opens_at"], Value::Null);
        assert_eq!(
            lane(&payload, "session-hygiene")["fleet"]["trend"],
            Value::Null
        );
    }

    /// A harness label is the archive's string, and a served page is a rendering surface a peer
    /// does not get to choose characters on. The clamp is [`format::identifier`]'s, the same one
    /// the report's Gaps lines pass through.
    #[test]
    fn a_hostile_harness_label_is_clamped_before_it_reaches_the_payload() {
        let hostile = "back`tick\nand a newline";
        let payload = built(hygiene_window(hostile, 20, 5), Vec::new());
        let serialized = serde_json::to_string(&payload).unwrap();
        assert!(!serialized.contains("back`tick"), "{serialized}");
        assert!(
            serialized.contains(format::INVALID_IDENTIFIER),
            "the label is replaced wholesale, not truncated: {serialized}",
        );
        assert_eq!(
            payload["sessions"]["by_harness"][format::INVALID_IDENTIFIER],
            20,
        );
    }

    /// The window pair the arrows are drawn across is labelled explicitly, in UTC, because UTC is
    /// the only clock the transcripts carry.
    #[test]
    fn both_windows_are_labelled_in_utc() {
        let payload = built(hygiene_window("claude-code", 20, 5), Vec::new());
        assert_eq!(payload["window"]["last"], "7d");
        assert_eq!(payload["window"]["generated_at"], "2026-08-17T12:00:00Z");
        assert_eq!(payload["window"]["opens_at"], "2026-08-10T12:00:00Z");
        assert_eq!(
            payload["window"]["comparison_opens_at"],
            "2026-08-03T12:00:00Z",
        );
        assert_eq!(
            window("7d").delta(),
            TimeDelta::days(7),
            "the fixture window is the one the labels above were computed from",
        );
    }
}
