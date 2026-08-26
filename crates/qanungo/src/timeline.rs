//! The timeline: the same fold, laid on a calendar.
//!
//! qanungo #5's fifth and last code-gated slice. The scores say how the practice reads; the
//! timeline says *when the window actually happened* — how many sessions landed on each day, and
//! how much work was inside them.
//!
//! # Archive time, and saying so
//!
//! A day here is the **UTC calendar day of the session's archive completion time**: the instant
//! Patwari finished the snapshot the session was listed by
//! ([`MirroredSession::archived_at`](crate::sync::MirroredSession)), truncated to a date in UTC.
//! Three things follow, and each of them is a claim this module owes a reader.
//!
//! **It is not the transcript's own clock.** [`SessionMetrics::day`] exists too and is the day the
//! session's first record is dated. A session started at 23:40 and archived at 00:20 has two
//! honest days and they are different ones. The archive clock is the one used here because it is
//! the clock the window itself was cut on — [`Placement`](crate::command) partitions the reported
//! and comparison windows on `archived_at` — so it is the *only* clock on which the per-day counts
//! can sum back to the window's own session count. A timeline whose bars did not add up to the
//! number in the subtitle above them would be two statements about the same window that disagree.
//!
//! **It is not local time, and this view does not pretend otherwise.** The archive states an
//! instant; it does not yet state the offset the capturing machine was on. That is munshi#77's
//! local-offset pull. A per-day *volume* survives UTC intact — a day is twenty-four hours wherever
//! you stand, and a boundary that falls in the small hours moves a handful of sessions between two
//! adjacent bars — which is why the 2026-08-24 grilling ruled the timeline unblocked. The **7×24
//! heatmap is a different matter and stays deferred**: "worked at 1 a.m." and "worked on a Sunday"
//! are exactly the claims a UTC clock misplaces, and misplacing them would break the only thing
//! that view is for. Capture-side offsets shipped on 2026-08-25; the heatmap waits on offset-bearing
//! sessions accruing, not on code here.
//!
//! **A session the archive gave no readable time is on no day at all.** It is counted
//! ([`Timeline::undated`]) and never placed, the same refusal
//! [`Placement`](crate::command) already makes when it declines to guess such a session into a
//! window. In practice such a session is not in the fold at all — it could not have been placed
//! into a window to be folded — so the count is a guard against a future path, and it is served
//! rather than assumed to be zero.
//!
//! # One fold, grouped a third way
//!
//! There is no second fold and no second arithmetic, exactly as in [`crate::scopes`]. Every number
//! below is a count of sessions the coaching fold already produced, or a sum of the *same*
//! gap-aware active time [`SessionMetrics::active_time`] hands the rules and the structural
//! evidence block. Grouping is by day; the harness axis is the payload's existing one, so a
//! timeline is a selection of the fold restated over dates.
//!
//! That is also why this is [`Timeline::fold`] over an iterator of borrowed sessions rather than a
//! method on [`Folded`](crate::command::Folded): a repository scope is a `Vec<&SessionMetrics>`
//! and the whole window is a `Vec<SessionMetrics>`, and both have to reach the same code or the
//! per-scope timelines and the whole-window one could come apart.
//!
//! # The two windows stay apart
//!
//! A window opens at an *instant*, not at midnight, so one calendar day can hold sessions from
//! both halves of the window pair. The two halves are therefore folded into two separate day
//! lists rather than into one list with a flag: a straddling day appears in each, holding its own
//! half's sessions. That keeps the reconciliation exact on both sides — each list sums to its own
//! window's folded session count — and it is what lets the page draw the boundary where the window
//! actually opens instead of at the nearest midnight.
//!
//! # Numbers and dates, and nothing else
//!
//! Not one string is produced here. A day is a [`NaiveDate`], a count is a `usize`, and active time
//! is a whole number of seconds; the harness axis is served once for the whole payload as an array
//! of indices into [`crate::scopes`]'s own harness columns, so the section a browser receives has
//! no place for an archive-written byte to hide. The dashboard's recursive walk pins that as a
//! property of the wire rather than as a habit of this module.
//!
//! Seconds rather than a rendered span for the same reason: the chart *scales* by this number, and
//! a bar height cannot be computed from `"2h 10m"`. What the page does with it is arithmetic on a
//! quantity — seconds into hours for an axis tick — and not a second implementation of
//! [`crate::format::span`], which stays the one renderer of how a duration reads in prose.
//!
//! # What it costs, measured
//!
//! Against production on 2026-08-25 — 707 sessions over **26 UTC days** in 28 repositories — the
//! whole served calendar, top level plus every scope, is **15,061 B (14.7 KiB, 1.3% of the
//! payload)**: 179 day rows at 84 bytes each. It is an order of magnitude cheaper than the scopes it
//! rides beside because of the two decisions above — sparse over the calendar, positional over the
//! harness axis. See [`crate::dashboard`] for the full measurement and for what the dense
//! alternative would have cost.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::metrics::SessionMetrics;

/// One day's contribution from one harness.
///
/// A cell exists only where a session landed: a harness that worked no session on a day has no
/// cell rather than a zero one, and the serialized row fills the gap with a zero at that harness's
/// column. The distinction matters on the way *in* — a `BTreeMap` of only what happened is bounded
/// by the sessions folded, while a dense day × harness grid is bounded by the window's length
/// times the roster, for every scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayCell {
    /// How many sessions this harness had archived on this day.
    pub sessions: usize,
    /// The sum of those sessions' active time, in whole seconds.
    ///
    /// [`SessionMetrics::active_time`]'s number — the gap-aware one the rules reason about, not
    /// wall-clock span — so a day's bar is the work in it rather than the calendar it touched. A
    /// session with no readable activity contributes nothing here and still contributes to
    /// `sessions`, which is why the two series can disagree about which day was the busiest.
    pub active_seconds: u64,
}

/// One day of one window, by harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    /// The UTC calendar day of archive completion. See the module docs.
    pub date: NaiveDate,
    /// What each harness contributed, keyed by the archive's own `source_agent` string — the same
    /// raw key [`crate::report::harness_columns`] builds the payload's harness axis from, so a
    /// serialized row is positional against that axis and never carries a label of its own.
    pub harnesses: BTreeMap<String, DayCell>,
}

impl Day {
    /// Every session this day holds, across the harnesses in it.
    pub fn sessions(&self) -> usize {
        self.harnesses.values().map(|cell| cell.sessions).sum()
    }

    /// This day's active time, across the harnesses in it, in whole seconds.
    pub fn active_seconds(&self) -> u64 {
        self.harnesses
            .values()
            .map(|cell| cell.active_seconds)
            .sum()
    }
}

/// One window's sessions laid on a calendar, earliest day first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline {
    /// The days a session actually landed on, earliest first. **Days with nothing in them are not
    /// here**: a window is a span of dates and a fold is a set of sessions, and serving one empty
    /// row per quiet day would cost bytes per scope in proportion to the window's *length* rather
    /// than to what happened in it. The page draws the axis from the dates it is given and leaves
    /// the gaps as gaps, which is what a quiet day looks like.
    pub days: Vec<Day>,
    /// Sessions the archive gave no readable completion time for, counted and placed nowhere.
    pub undated: usize,
}

impl Timeline {
    /// Lays a selection of the fold on the calendar.
    ///
    /// Takes borrowed sessions so the whole window (`Vec<SessionMetrics>`) and a repository scope
    /// (`Vec<&SessionMetrics>`) reach the same code — see the module docs.
    pub fn fold<'a>(sessions: impl IntoIterator<Item = &'a SessionMetrics>) -> Self {
        let mut days: BTreeMap<NaiveDate, BTreeMap<String, DayCell>> = BTreeMap::new();
        let mut undated = 0;
        for session in sessions {
            let Some(date) = session.archive_day() else {
                undated += 1;
                continue;
            };
            let cell = days
                .entry(date)
                .or_default()
                .entry(session.source_agent.clone())
                .or_default();
            cell.sessions += 1;
            // Negative is not a state `active_time` can be in — it is a sum of forward gaps — but
            // it is a `TimeDelta`, and clamping at the seam is cheaper than trusting a type's
            // range. A session with no readable activity adds nothing and is still counted above.
            cell.active_seconds += session
                .active_time()
                .map_or(0, |active| u64::try_from(active.num_seconds()).unwrap_or(0));
        }
        Self {
            days: days
                .into_iter()
                .map(|(date, harnesses)| Day { date, harnesses })
                .collect(),
            undated,
        }
    }

    /// How many days this window actually put a session on — the provenance figure.
    pub fn days_covered(&self) -> usize {
        self.days.len()
    }

    /// Every session on the calendar. Equal to the selection's session count less
    /// [`Timeline::undated`], which is the reconciliation the dashboard's tests pin.
    pub fn sessions(&self) -> usize {
        self.days.iter().map(Day::sessions).sum()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use munshi_transcript::SessionSummary;

    use super::*;
    use crate::evidence::SessionAnchors;
    use crate::metrics::{Activity, CommandChurn, Compactions, ReviewActivity, ToolOutcomes};

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn date(value: &str) -> NaiveDate {
        value.parse().expect("an ISO date")
    }

    /// One session, archived at `archived_at`, whose transcript holds `records` timestamps a
    /// quarter of the idle gap apart — so its active time is exactly `(records - 1) × step`.
    fn session(
        index: usize,
        source_agent: &str,
        archived_at: Option<&str>,
        records: i32,
    ) -> SessionMetrics {
        let step = crate::rules::thresholds::IDLE_GAP / 4;
        let first = at("2026-08-10T09:00:00Z");
        let timestamps: Vec<_> = (0..records).map(|n| first + step * n).collect();
        SessionMetrics {
            source_hash: format!("{index:02x}").repeat(32),
            source_agent: source_agent.to_owned(),
            repository: None,
            archived_at: archived_at.map(at),
            hostname: None,
            utc_offset: None,
            artifact_set_version: 2,
            summary: SessionSummary {
                first_timestamp: timestamps.first().copied(),
                last_timestamp: timestamps.last().copied(),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes::default(),
            activity: Activity::over(timestamps),
            commands: CommandChurn::default(),
            compactions: Compactions::default(),
            reviews: ReviewActivity::default(),
            anchors: SessionAnchors::default(),
            bytes_folded: 0,
        }
    }

    /// The boundary the whole view rests on: a UTC day ends at `23:59:59Z` and the next one starts
    /// at `00:00:00Z`, and one second between two sessions puts them on different bars.
    ///
    /// Stated against the two instants either side of midnight rather than against a mid-afternoon
    /// pair, because midnight is the only place a day boundary can be got wrong.
    #[test]
    fn a_second_either_side_of_midnight_is_two_days() {
        let timeline = Timeline::fold(&[
            session(1, "claude-code", Some("2026-08-10T23:59:59Z"), 2),
            session(2, "claude-code", Some("2026-08-11T00:00:00Z"), 2),
            // The last instant of the second day, to pin the far edge too.
            session(3, "claude-code", Some("2026-08-11T23:59:59.999Z"), 2),
        ]);
        let days: Vec<NaiveDate> = timeline.days.iter().map(|day| day.date).collect();
        assert_eq!(days, vec![date("2026-08-10"), date("2026-08-11")]);
        assert_eq!(timeline.days[0].sessions(), 1);
        assert_eq!(timeline.days[1].sessions(), 2);
        assert_eq!(timeline.days_covered(), 2);
        assert_eq!(timeline.sessions(), 3);
    }

    /// A day is split by harness, and the split is keyed on the archive's own `source_agent` — the
    /// key the payload's harness axis is built from, so a row can be positional against it.
    #[test]
    fn a_day_carries_one_cell_per_harness_and_sums_across_them() {
        let timeline = Timeline::fold(&[
            session(1, "claude-code", Some("2026-08-10T10:00:00Z"), 5),
            session(2, "claude-code", Some("2026-08-10T18:00:00Z"), 3),
            session(3, "copilot-cli", Some("2026-08-10T20:00:00Z"), 2),
            session(4, "copilot-cli", Some("2026-08-12T20:00:00Z"), 2),
        ]);
        assert_eq!(timeline.days_covered(), 2);
        let first = &timeline.days[0];
        assert_eq!(first.date, date("2026-08-10"));
        assert_eq!(first.harnesses.len(), 2);
        assert_eq!(first.harnesses["claude-code"].sessions, 2);
        assert_eq!(first.harnesses["copilot-cli"].sessions, 1);
        assert_eq!(first.sessions(), 3);

        // Active time is the fold's own gap-aware number, summed: four and two steps of a quarter
        // idle gap on the first harness, one on the second.
        let step = crate::rules::thresholds::IDLE_GAP / 4;
        let seconds = |steps: i32| u64::try_from((step * steps).num_seconds()).unwrap();
        assert_eq!(first.harnesses["claude-code"].active_seconds, seconds(6));
        assert_eq!(first.harnesses["copilot-cli"].active_seconds, seconds(1));
        assert_eq!(first.active_seconds(), seconds(7));
    }

    /// The two series measure different things and are allowed to disagree about the busiest day.
    /// A day of many short sessions and a day of one long one is exactly the shape the toggle
    /// exists to show, so it is pinned rather than assumed.
    #[test]
    fn sessions_per_day_and_active_time_per_day_can_name_different_days() {
        let timeline = Timeline::fold(&[
            session(1, "claude-code", Some("2026-08-10T10:00:00Z"), 2),
            session(2, "claude-code", Some("2026-08-10T11:00:00Z"), 2),
            session(3, "claude-code", Some("2026-08-10T12:00:00Z"), 2),
            session(4, "claude-code", Some("2026-08-11T10:00:00Z"), 20),
        ]);
        assert_eq!(timeline.days[0].sessions(), 3);
        assert_eq!(timeline.days[1].sessions(), 1);
        assert!(timeline.days[1].active_seconds() > timeline.days[0].active_seconds());
    }

    /// A session with no readable archive time is on no bar. It is counted, because a view that
    /// silently dropped it would report a day count and a session count that cannot both be true.
    #[test]
    fn a_session_the_archive_gave_no_time_for_is_counted_and_placed_nowhere() {
        let timeline = Timeline::fold(&[
            session(1, "claude-code", Some("2026-08-10T10:00:00Z"), 2),
            session(2, "claude-code", None, 2),
        ]);
        assert_eq!(timeline.days_covered(), 1);
        assert_eq!(timeline.sessions(), 1);
        assert_eq!(timeline.undated, 1);
    }

    /// A session whose transcript records no activity at all still happened: it is a bar on the
    /// sessions series and nothing on the active-time one, never a missing day.
    #[test]
    fn a_session_with_no_readable_activity_is_still_a_session_that_day() {
        let mut quiet = session(1, "claude-code", Some("2026-08-10T10:00:00Z"), 1);
        quiet.activity = Activity::default();
        assert_eq!(quiet.active_time(), None);
        let timeline = Timeline::fold(&[quiet]);
        assert_eq!(timeline.days[0].sessions(), 1);
        assert_eq!(timeline.days[0].active_seconds(), 0);
    }

    /// An empty selection is an empty timeline, not a day of zeroes. A repository scope with no
    /// coaching session in the window is a real state — the bill or the narrative put it on the
    /// list — and it must have nothing to draw rather than a phantom bar.
    #[test]
    fn an_empty_selection_draws_nothing() {
        let timeline = Timeline::fold(std::iter::empty());
        assert_eq!(timeline, Timeline::default());
        assert_eq!(timeline.days_covered(), 0);
        assert_eq!(timeline.sessions(), 0);
    }

    /// The fold is a *selection* of sessions the coaching fold already produced, so the same code
    /// has to take the whole window's owned vector and a scope's borrowed one. This is the shape
    /// [`crate::scopes`] hands it, spelled out so a signature change cannot quietly break it.
    #[test]
    fn owned_and_borrowed_selections_reach_the_same_fold() {
        let owned = vec![
            session(1, "claude-code", Some("2026-08-10T10:00:00Z"), 3),
            session(2, "copilot-cli", Some("2026-08-11T10:00:00Z"), 3),
        ];
        let borrowed: Vec<&SessionMetrics> = owned.iter().collect();
        assert_eq!(
            Timeline::fold(&owned),
            Timeline::fold(borrowed.iter().copied()),
        );
    }

    /// The claim the module docs make about the two clocks, held as a test: a session worked before
    /// midnight and archived after it is on the *archive's* day, which is the day the window was
    /// cut on and therefore the day the counts reconcile against.
    #[test]
    fn the_day_is_archive_time_and_not_the_transcripts_own_first_record() {
        let mut overnight = session(1, "claude-code", Some("2026-08-11T00:20:00Z"), 4);
        let started = at("2026-08-10T23:40:00Z");
        overnight.summary.first_timestamp = Some(started);
        overnight.summary.last_timestamp = Some(started + TimeDelta::minutes(40));
        overnight.activity = Activity::over(vec![started, started + TimeDelta::minutes(40)]);

        assert_eq!(overnight.day(), Some(date("2026-08-10")));
        assert_eq!(overnight.archive_day(), Some(date("2026-08-11")));
        let timeline = Timeline::fold(std::slice::from_ref(&overnight));
        assert_eq!(timeline.days[0].date, date("2026-08-11"));
    }

    /// Every field this module reads is one the fold already produced, and the two it added to
    /// [`SessionMetrics`] are inert everywhere else. The guard is cheap and the claim is load
    /// bearing: the whole argument for the slice is that it re-groups rather than re-folds.
    #[test]
    fn the_timeline_reads_only_what_the_fold_already_produced() {
        let sessions = vec![session(1, "claude-code", Some("2026-08-10T10:00:00Z"), 4)];
        let before = crate::scoring::Scorecard::fold(&sessions);
        let _ = Timeline::fold(&sessions);
        let after = crate::scoring::Scorecard::fold(&sessions);
        assert_eq!(before.harnesses.len(), after.harnesses.len());
        for lane in crate::scoring::Lane::ALL {
            assert_eq!(
                before.fleet(lane).map(|blend| blend.score),
                after.fleet(lane).map(|blend| blend.score),
                "{lane:?} moved",
            );
        }
    }
}
