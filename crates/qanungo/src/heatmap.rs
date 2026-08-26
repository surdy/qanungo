//! The heatmap: the same fold, laid on the operator's own clock.
//!
//! qanungo #5's habits view. The timeline says *when the window happened* on the archive's clock;
//! the heatmap says *when in the operator's own day* the work happened — which hours of which
//! weekday they actually code. It is the one view UTC genuinely breaks, and the whole reason
//! [`SessionMetrics::utc_offset`] exists: plotting in UTC "misplaces the late-night claims", so a
//! `1 a.m.` cell and a `Sunday` row are exactly the two statements a missing offset gets wrong.
//!
//! # Transcript time, shifted local
//!
//! A cell here is an **hour of the local weekday the session's work began on**. Two shifts stand
//! between a raw archive fact and that cell, and each is a claim this module owes a reader.
//!
//! **It is transcript time, not archive time.** The clock is
//! [`SessionSummary::first_timestamp`](munshi_transcript::SessionSummary) — when the first record
//! of the *conversation* was written, which is when the operator started working — and emphatically
//! not [`SessionMetrics::archived_at`], which is when Patwari finished the snapshot. The timeline
//! uses archive time because that is the clock the window was cut on and the only one its bars can
//! reconcile against ([`crate::timeline`] says so at length). The heatmap uses transcript time
//! because the question is about the *work*, not the *capture*: a session archived at nine the next
//! morning was not worked at nine the next morning.
//!
//! **It is local time, and that is the point.** Each session's [`SessionMetrics::utc_offset`] — the
//! capturing machine's own offset, e.g. `-07:00` — is applied to the UTC instant before the
//! weekday and the hour are read off it. Applying the offset can move the reading across a midnight
//! and therefore onto a different weekday: a session whose first record is `03:00Z` on a Monday,
//! captured at `-07:00`, began at `20:00` on the **Sunday** local, and that is the cell it lands on.
//! The shift is per session and never a constant: the archive today happens to be entirely
//! US-Pacific (`-07:00` on both machines), but nothing here assumes one offset — a mixed-offset
//! archive places each session on its own clock.
//!
//! # A session with no offset is on no cell, counted
//!
//! A session whose snapshot recorded no [`utc_offset`](SessionMetrics::utc_offset) cannot be put in
//! a *local* hour at all — its UTC instant is known and its local one is not — so it is placed on
//! **no cell** and counted in [`Heatmap::no_offset`], never guessed onto a cell by assuming a zone.
//! That is exactly the refusal [`crate::timeline`] makes for a session with no `archived_at`:
//! counted, not placed, and surfaced so the page can say why the cells are a few short instead of
//! the reader discovering it. A session that carries an offset but no readable first-activity
//! timestamp is the same shape for a different reason and is counted in [`Heatmap::undated`].
//!
//! Both counts should be near-zero on today's archive — every current capture carries an offset —
//! but `no_offset` is a *general* real state (the metadata accrues only from the capture machine's
//! 2026-08-25 deploy, so any session captured before it has none), so it is served rather than
//! assumed away.
//!
//! # What magnitude a cell carries, and why the whole session lands on one
//!
//! Each session contributes to **exactly one** cell: the local hour and weekday its *first activity*
//! falls in. That cell's two magnitudes mirror the timeline's [`DayCell`](crate::timeline::DayCell):
//!
//! - `sessions` — how many sessions *started* in that local hour-of-week. A clean habits reading.
//! - `active_seconds` — the same gap-aware [`SessionMetrics::active_time`] the rules and the
//!   timeline reason about, summed over the sessions that started there. It is the work *of* those
//!   sessions, attributed to the hour they began — not a claim that the clock was inside that hour
//!   while the work happened.
//!
//! That second sentence is the honest limit of the design, and it was a deliberate choice over a
//! more precise-looking one. Distributing a session's active seconds across the local hours it
//! actually spanned would be a better habits picture *if the data allowed it* — and it does not.
//! The fold does not keep a session's per-record timestamps: [`Activity`](crate::metrics::Activity)
//! folds them into accumulators and at most [`MAX_STRUCTURAL_SITTINGS`](crate::metrics::MAX_STRUCTURAL_SITTINGS)
//! sitting boundaries, then drops the stream. Two consequences rule the finer view out:
//!
//! 1. **Recovering the per-hour distribution would mean re-folding transcripts.** This module, like
//!    [`crate::scopes`] and [`crate::timeline`], is a *re-grouping* of facts the coaching fold
//!    already produced — its whole justification is that it re-groups rather than re-folds. Reaching
//!    back into the megabyte transcripts to spread seconds across hours would break that.
//! 2. **The one finer signal that survives the fold — the sitting boundaries — is capped and does
//!    not reconcile.** It keeps only the first twelve sittings, and its summed spans are not even
//!    guaranteed to equal [`active_time`](crate::metrics::Activity::active_time) (the out-of-order
//!    clamp can make them disagree; see [`Activity`](crate::metrics::Activity)). Distributing over a
//!    capped, non-reconciling list would trade the one property this family of views must keep —
//!    the cells summing back to the session count above them — for a precision the fold cannot
//!    honestly supply.
//!
//! So the whole session lands on its first-activity hour, both magnitudes reconcile exactly
//! (`Σ cells + no_offset + undated == the selection's session count`), and the `active_seconds`
//! series is documented as *work attributed to its start hour* rather than pretending to an
//! hour-by-hour spread it cannot support. Distributing across spanned hours is the refinement a
//! future per-record retention would unlock; the note is here so the reader gets the reason, not
//! only the shape.
//!
//! # Numbers and indices, and nothing else
//!
//! Not one string is produced here. A cell is a `(weekday, hour)` pair of small integers — weekday
//! `0 = Monday … 6 = Sunday` ([`chrono::Weekday::num_days_from_monday`]), hour `0..=23` — and the
//! harness axis is served once for the whole payload as an array of indices into [`crate::scopes`]'s
//! own harness columns, exactly as the timeline serves it. A section made only of integers has
//! nowhere for an archive-written byte to hide, which the dashboard's recursive walk pins as a
//! property of the wire. The weekday and hour *labels* live in the page, which is page code and not
//! archive data — and the page never builds a date through `Date`, because re-expressing a local
//! hour through the reader's own zone is the exact confusion this view exists to avoid.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, FixedOffset, Timelike, Utc};

use crate::metrics::SessionMetrics;

/// One local hour-of-week's contribution from one harness.
///
/// The same shape as the timeline's [`DayCell`](crate::timeline::DayCell), and for the same
/// reason: a cell exists only where a session landed, so a sparse map of what happened is bounded by
/// the sessions folded rather than by the 7×24 grid times the roster for every scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HourCell {
    /// How many sessions this harness *started* in this local hour-of-week.
    pub sessions: usize,
    /// The sum of those sessions' gap-aware active time, in whole seconds — the work of the
    /// sessions that began here, attributed to their start hour. See the module docs for why the
    /// whole session's seconds land on one cell rather than being spread across the hours it spanned.
    pub active_seconds: u64,
}

/// One local hour of one weekday, by harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The local weekday, `0 = Monday … 6 = Sunday`. See the module docs.
    pub weekday: u8,
    /// The local hour, `0..=23`.
    pub hour: u8,
    /// What each harness contributed, keyed by the archive's own `source_agent` string — the same
    /// raw key [`crate::report::harness_columns`] builds the payload's harness axis from, so a
    /// serialized cell is positional against that axis and never carries a label of its own.
    pub harnesses: BTreeMap<String, HourCell>,
}

impl Cell {
    /// Every session this cell holds, across the harnesses in it.
    pub fn sessions(&self) -> usize {
        self.harnesses.values().map(|cell| cell.sessions).sum()
    }

    /// This cell's active time, across the harnesses in it, in whole seconds.
    pub fn active_seconds(&self) -> u64 {
        self.harnesses
            .values()
            .map(|cell| cell.active_seconds)
            .sum()
    }
}

/// One window's sessions laid on the local hour-of-week grid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heatmap {
    /// The local hour-of-week cells a session actually started in, ordered `(weekday, hour)`.
    /// **Cells with nothing in them are not here**: a habits grid is 168 slots and a fold is a set
    /// of sessions, and serving one empty slot per quiet hour would cost bytes per scope in
    /// proportion to the grid rather than to what happened. The page draws all 168 slots and fills
    /// the ones it is given, leaving the rest as the empty cells they are.
    pub cells: Vec<Cell>,
    /// Sessions whose snapshot recorded no `utc_offset`, so they cannot be placed in a *local*
    /// hour. On no cell, and counted — never guessed onto a cell by assuming a zone. See the module
    /// docs.
    pub no_offset: usize,
    /// Sessions that carry an offset but no readable first-activity timestamp, so there is no
    /// instant to shift. On no cell, and counted, on the same discipline as [`Heatmap::no_offset`].
    pub undated: usize,
}

impl Heatmap {
    /// Lays a selection of the fold on the local hour-of-week grid.
    ///
    /// Takes borrowed sessions so the whole window (`Vec<SessionMetrics>`) and a repository scope
    /// (`Vec<&SessionMetrics>`) reach the same code — the shape [`crate::scopes`] hands it, exactly
    /// as [`crate::timeline`] takes it.
    pub fn fold<'a>(sessions: impl IntoIterator<Item = &'a SessionMetrics>) -> Self {
        let mut cells: BTreeMap<(u8, u8), BTreeMap<String, HourCell>> = BTreeMap::new();
        let mut no_offset = 0;
        let mut undated = 0;
        for session in sessions {
            // Offset first: without it there is no local clock at all, whatever the timestamp says,
            // and the missing offset is the state this whole view waited on. A session with an
            // offset but no first-activity instant is a different, rarer refusal.
            let Some(offset) = session.utc_offset.as_deref().and_then(parse_offset) else {
                no_offset += 1;
                continue;
            };
            let Some(slot) = local_slot(session.summary.first_timestamp, offset) else {
                undated += 1;
                continue;
            };
            let cell = cells
                .entry(slot)
                .or_default()
                .entry(session.source_agent.clone())
                .or_default();
            cell.sessions += 1;
            // The same clamp the timeline makes at the same seam: active_time is a sum of forward
            // gaps and cannot be negative, but it is a TimeDelta and clamping is cheaper than
            // trusting the type's range. A session with no readable activity adds nothing here and
            // is still counted above.
            cell.active_seconds += session
                .active_time()
                .map_or(0, |active| u64::try_from(active.num_seconds()).unwrap_or(0));
        }
        Self {
            cells: cells
                .into_iter()
                .map(|((weekday, hour), harnesses)| Cell {
                    weekday,
                    hour,
                    harnesses,
                })
                .collect(),
            no_offset,
            undated,
        }
    }

    /// How many of the 168 local hour-of-week slots this window actually put a session on — the
    /// provenance figure, beside the timeline's `days_covered`.
    pub fn cells_covered(&self) -> usize {
        self.cells.len()
    }

    /// Every session placed on the grid. Equal to the selection's session count less
    /// [`Heatmap::no_offset`] and [`Heatmap::undated`] — the reconciliation the dashboard's tests
    /// pin.
    pub fn sessions(&self) -> usize {
        self.cells.iter().map(Cell::sessions).sum()
    }
}

/// The local `(weekday, hour)` an instant falls in once shifted into `offset`, or `None` when there
/// is no instant to shift.
///
/// Weekday is `0 = Monday … 6 = Sunday`; hour is `0..=23`. Both are read off the *shifted* clock, so
/// an offset that carries the instant across a midnight lands it on the adjacent weekday — which is
/// the whole reason the offset is applied before either is read.
fn local_slot(at: Option<DateTime<Utc>>, offset: FixedOffset) -> Option<(u8, u8)> {
    let local = at?.with_timezone(&offset);
    let weekday = local.weekday().num_days_from_monday() as u8;
    let hour = local.hour() as u8;
    Some((weekday, hour))
}

/// Parses an RFC3339 UTC offset — `"-07:00"`, `"+05:30"`, or a bare `"Z"` — into a
/// [`FixedOffset`], or `None` for anything else.
///
/// The archive writes these off the capture machine's libc, so the shape is `±HH:MM`; `Z` is
/// accepted as `+00:00` for completeness. Anything the archive could not have written — a malformed
/// string, an out-of-range field — yields `None` and the session is counted as unplaceable rather
/// than placed on a guessed hour. Written by hand rather than leaned on chrono's parser because what
/// is in the archive is the offset alone, not a full timestamp to hang it on.
fn parse_offset(text: &str) -> Option<FixedOffset> {
    if text == "Z" {
        return FixedOffset::east_opt(0);
    }
    let sign = match text.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (hours, minutes) = text.get(1..)?.split_once(':')?;
    // Exactly two digits each, so a stray sign or a sprawling field is rejected rather than
    // half-read: `+7:0` and `+07:00:00` are both malformed offsets, not offsets to salvage.
    if hours.len() != 2 || minutes.len() != 2 {
        return None;
    }
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use munshi_transcript::SessionSummary;

    use super::*;
    use crate::evidence::SessionAnchors;
    use crate::metrics::{Activity, CommandChurn, Compactions, ReviewActivity, ToolOutcomes};

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// One session whose first activity is at `first` and whose snapshot recorded `utc_offset`, with
    /// `records` timestamps a quarter of the idle gap apart — so its active time is exactly
    /// `(records - 1) × step`, and its first-activity instant (the placement clock) is `first`.
    ///
    /// `first == None` is a session with no readable activity timestamp at all; `utc_offset == None`
    /// is a session whose snapshot never recorded an offset.
    fn session(
        index: usize,
        source_agent: &str,
        first: Option<&str>,
        utc_offset: Option<&str>,
        records: i32,
    ) -> SessionMetrics {
        let step = crate::rules::thresholds::IDLE_GAP / 4;
        let timestamps: Vec<_> = first
            .map(|first| (0..records).map(|n| at(first) + step * n).collect())
            .unwrap_or_default();
        SessionMetrics {
            source_hash: format!("{index:02x}").repeat(32),
            source_agent: source_agent.to_owned(),
            repository: None,
            archived_at: Some(at("2026-08-12T09:00:00Z")),
            hostname: None,
            utc_offset: utc_offset.map(ToOwned::to_owned),
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

    /// The one cell a single-session heatmap put anything on: its `(weekday, hour)` and its count.
    fn only_cell(heatmap: &Heatmap) -> (u8, u8, usize) {
        assert_eq!(heatmap.cells.len(), 1, "expected exactly one cell");
        let cell = &heatmap.cells[0];
        (cell.weekday, cell.hour, cell.sessions())
    }

    /// The base case: a session's local hour is its first-activity instant shifted by its own
    /// offset. `16:00Z` on a Monday at `-07:00` is `09:00` the same Monday local.
    #[test]
    fn a_session_lands_on_its_first_activity_local_hour() {
        // 2026-08-10 is a Monday; num_days_from_monday(Monday) == 0.
        let heatmap = Heatmap::fold(&[session(
            1,
            "claude-code",
            Some("2026-08-10T16:00:00Z"),
            Some("-07:00"),
            2,
        )]);
        assert_eq!(only_cell(&heatmap), (0, 9, 1));
        assert_eq!(heatmap.cells_covered(), 1);
        assert_eq!(heatmap.sessions(), 1);
        assert_eq!(heatmap.no_offset, 0);
        assert_eq!(heatmap.undated, 0);
    }

    /// The claim the whole view rests on: applying the offset can carry the instant across a
    /// midnight and onto a different weekday. `03:00Z` on a Monday at `-07:00` is `20:00` on the
    /// **Sunday** local — the late-night, wrong-weekday claim a UTC grid misplaces, placed right.
    #[test]
    fn an_offset_that_crosses_midnight_moves_the_weekday() {
        // 2026-08-10 Monday 03:00Z − 7h = 2026-08-09 Sunday 20:00 local. Sunday == 6.
        let backward = Heatmap::fold(&[session(
            1,
            "claude-code",
            Some("2026-08-10T03:00:00Z"),
            Some("-07:00"),
            2,
        )]);
        assert_eq!(only_cell(&backward), (6, 20, 1));

        // And forward across the other midnight: 2026-08-10 Monday 22:00Z + 5:30 = 2026-08-11
        // Tuesday 03:30 local. Tuesday == 1, hour 3. A non-Pacific, non-hour-aligned offset, so the
        // code is exercised on more than the archive's single uniform shift.
        let forward = Heatmap::fold(&[session(
            2,
            "copilot-cli",
            Some("2026-08-10T22:00:00Z"),
            Some("+05:30"),
            2,
        )]);
        assert_eq!(only_cell(&forward), (1, 3, 1));
    }

    /// A session whose snapshot recorded no offset cannot be put in a local hour. It is on no cell,
    /// and it is counted — a view that silently dropped it would report a cell count and a session
    /// count that cannot both be true, the same refusal the timeline makes for a missing archive
    /// time.
    #[test]
    fn a_session_with_no_offset_is_counted_and_placed_nowhere() {
        let heatmap = Heatmap::fold(&[
            session(
                1,
                "claude-code",
                Some("2026-08-10T16:00:00Z"),
                Some("-07:00"),
                2,
            ),
            session(2, "claude-code", Some("2026-08-10T16:00:00Z"), None, 2),
        ]);
        assert_eq!(heatmap.cells_covered(), 1);
        assert_eq!(heatmap.sessions(), 1);
        assert_eq!(heatmap.no_offset, 1);
        assert_eq!(heatmap.undated, 0);
    }

    /// A session with an offset but no readable first-activity timestamp is the same refusal for a
    /// different reason: nothing to shift, so no cell, and counted under `undated` rather than
    /// `no_offset`.
    #[test]
    fn a_session_with_an_offset_but_no_activity_time_is_undated() {
        let heatmap = Heatmap::fold(&[session(1, "claude-code", None, Some("-07:00"), 0)]);
        assert_eq!(heatmap.cells_covered(), 0);
        assert_eq!(heatmap.sessions(), 0);
        assert_eq!(heatmap.no_offset, 0);
        assert_eq!(heatmap.undated, 1);
    }

    /// A malformed offset is not a zone to guess at: the session is counted unplaceable, exactly as
    /// a missing one is. Each shape the archive could never have written must fail closed.
    #[test]
    fn a_malformed_offset_is_unplaceable_not_guessed() {
        for bad in [
            "",
            "-7:00",
            "+07",
            "07:00",
            "+07:00:00",
            "+24:00",
            "+00:60",
            "PST",
        ] {
            let heatmap = Heatmap::fold(&[session(
                1,
                "claude-code",
                Some("2026-08-10T16:00:00Z"),
                Some(bad),
                2,
            )]);
            assert_eq!(heatmap.no_offset, 1, "{bad:?} should not place");
            assert_eq!(heatmap.cells_covered(), 0, "{bad:?} should not place");
        }
        // ...while the well-formed shapes, including a bare Z, all place.
        for good in ["-07:00", "+05:30", "+00:00", "Z"] {
            let heatmap = Heatmap::fold(&[session(
                1,
                "claude-code",
                Some("2026-08-10T16:00:00Z"),
                Some(good),
                2,
            )]);
            assert_eq!(heatmap.no_offset, 0, "{good:?} should place");
            assert_eq!(heatmap.cells_covered(), 1, "{good:?} should place");
        }
    }

    /// A cell is split by harness on the archive's own `source_agent`, and two sessions that started
    /// in the same local hour on different harnesses share a cell without their counts merging.
    #[test]
    fn a_cell_carries_one_entry_per_harness_and_sums_across_them() {
        let heatmap = Heatmap::fold(&[
            session(
                1,
                "claude-code",
                Some("2026-08-10T16:00:00Z"),
                Some("-07:00"),
                5,
            ),
            session(
                2,
                "claude-code",
                Some("2026-08-10T16:30:00Z"),
                Some("-07:00"),
                3,
            ),
            session(
                3,
                "copilot-cli",
                Some("2026-08-10T16:10:00Z"),
                Some("-07:00"),
                2,
            ),
        ]);
        // All three began in the 09:00 local hour on Monday.
        assert_eq!(heatmap.cells_covered(), 1);
        let cell = &heatmap.cells[0];
        assert_eq!((cell.weekday, cell.hour), (0, 9));
        assert_eq!(cell.harnesses.len(), 2);
        assert_eq!(cell.harnesses["claude-code"].sessions, 2);
        assert_eq!(cell.harnesses["copilot-cli"].sessions, 1);
        assert_eq!(cell.sessions(), 3);

        // Active seconds are the fold's own gap-aware number, summed: four and two steps on the
        // first harness, one on the second.
        let step = crate::rules::thresholds::IDLE_GAP / 4;
        let seconds = |steps: i32| u64::try_from((step * steps).num_seconds()).unwrap();
        assert_eq!(cell.harnesses["claude-code"].active_seconds, seconds(6));
        assert_eq!(cell.harnesses["copilot-cli"].active_seconds, seconds(1));
        assert_eq!(cell.active_seconds(), seconds(7));
    }

    /// The whole session's active seconds land on its start-hour cell — the documented magnitude
    /// choice — so a long session that ran across many hours still contributes to exactly one cell,
    /// and `sessions()` reconciles to the placed count.
    #[test]
    fn a_long_session_contributes_its_whole_active_time_to_its_start_hour() {
        let heatmap = Heatmap::fold(&[session(
            1,
            "claude-code",
            Some("2026-08-10T16:00:00Z"),
            Some("-07:00"),
            40,
        )]);
        assert_eq!(heatmap.cells_covered(), 1);
        let step = crate::rules::thresholds::IDLE_GAP / 4;
        let expected = u64::try_from((step * 39).num_seconds()).unwrap();
        assert_eq!(heatmap.cells[0].active_seconds(), expected);
        assert_eq!(heatmap.sessions(), 1);
    }

    /// A session whose transcript records no activity still started somewhere: it is a count on its
    /// cell and nothing on the active-time series, never a missing cell — so the two series can name
    /// different busiest cells, exactly as the timeline's two do.
    #[test]
    fn a_session_with_no_readable_activity_is_still_a_session_in_its_hour() {
        let mut quiet = session(
            1,
            "claude-code",
            Some("2026-08-10T16:00:00Z"),
            Some("-07:00"),
            1,
        );
        quiet.activity = Activity::default();
        assert_eq!(quiet.active_time(), None);
        let heatmap = Heatmap::fold(&[quiet]);
        assert_eq!(heatmap.cells[0].sessions(), 1);
        assert_eq!(heatmap.cells[0].active_seconds(), 0);
    }

    /// An empty selection is an empty heatmap, not a grid of zeroes — a repository scope with no
    /// session in the window is a real state and must have nothing to draw.
    #[test]
    fn an_empty_selection_draws_nothing() {
        let heatmap = Heatmap::fold(std::iter::empty());
        assert_eq!(heatmap, Heatmap::default());
        assert_eq!(heatmap.cells_covered(), 0);
        assert_eq!(heatmap.sessions(), 0);
    }

    /// The fold is a selection, so the same code has to take the whole window's owned vector and a
    /// scope's borrowed one and produce the same grid — the shape [`crate::scopes`] hands it.
    #[test]
    fn owned_and_borrowed_selections_reach_the_same_fold() {
        let owned = vec![
            session(
                1,
                "claude-code",
                Some("2026-08-10T16:00:00Z"),
                Some("-07:00"),
                3,
            ),
            session(
                2,
                "copilot-cli",
                Some("2026-08-11T22:00:00Z"),
                Some("+05:30"),
                3,
            ),
        ];
        let borrowed: Vec<&SessionMetrics> = owned.iter().collect();
        assert_eq!(
            Heatmap::fold(&owned),
            Heatmap::fold(borrowed.iter().copied()),
        );
    }

    /// Each offset is the session's own, not a constant: two sessions at the same UTC instant but
    /// different offsets land on different cells. The archive is uniform today, and this is the
    /// guard that the code does not bake that in.
    #[test]
    fn each_session_is_placed_on_its_own_offset() {
        let heatmap = Heatmap::fold(&[
            session(
                1,
                "claude-code",
                Some("2026-08-10T16:00:00Z"),
                Some("-07:00"),
                2,
            ),
            session(
                2,
                "copilot-cli",
                Some("2026-08-10T16:00:00Z"),
                Some("+00:00"),
                2,
            ),
        ]);
        // -07:00 -> 09:00 Monday; +00:00 -> 16:00 Monday. Same weekday, different hour, two cells.
        assert_eq!(heatmap.cells_covered(), 2);
        let hours: Vec<u8> = heatmap.cells.iter().map(|cell| cell.hour).collect();
        assert_eq!(hours, vec![9, 16]);
    }

    /// Every field this module reads is one the fold already produced, and the offset it reads is
    /// inert everywhere else. The guard is cheap and the claim is load bearing: the whole argument
    /// for the slice is that it re-groups rather than re-folds.
    #[test]
    fn the_heatmap_reads_only_what_the_fold_already_produced() {
        let sessions = vec![session(
            1,
            "claude-code",
            Some("2026-08-10T16:00:00Z"),
            Some("-07:00"),
            4,
        )];
        let before = crate::scoring::Scorecard::fold(&sessions);
        let _ = Heatmap::fold(&sessions);
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

    /// The placement clock is transcript time, not archive time — the difference from the timeline,
    /// held as a test. A session archived days after it was worked places on the hour it was
    /// *worked*, so `archived_at` moving must not move a cell.
    #[test]
    fn placement_is_transcript_time_not_archive_time() {
        let mut worked = session(
            1,
            "claude-code",
            Some("2026-08-10T16:00:00Z"),
            Some("-07:00"),
            2,
        );
        let before = Heatmap::fold(std::slice::from_ref(&worked));
        // Re-archive it a week later; the work is unchanged.
        worked.archived_at = Some(at("2026-08-19T02:00:00Z"));
        let after = Heatmap::fold(std::slice::from_ref(&worked));
        assert_eq!(before, after);
        assert_eq!(only_cell(&after), (0, 9, 1));
    }
}
