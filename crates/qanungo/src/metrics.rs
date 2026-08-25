//! The fold: exactly five metrics over `munshi-transcript`'s typed events.
//!
//! Everything here is derivable from what the interpreter types *today* — no new transcript
//! parsing, no schema changes upstream:
//!
//! 1. **Tool error rate** — per session and per tool name, from the outcome fields a
//!    [`ToolEvent`] carries (`success`, `is_error`).
//! 2. **Tool-per-request ratio** — [`SessionSummary::tool_activities`] over
//!    [`SessionSummary::user_requests`]: how much the agent does per thing it is asked for.
//! 3. **Cadence and duration** — sessions per day, and each session's *active time*: the sum of
//!    the gaps between consecutive records, with anything longer than
//!    [`IDLE_GAP`](crate::rules::thresholds::IDLE_GAP) treated as the operator having walked
//!    away. Wall-clock span is still derived, but as context rather than as the number a rule
//!    decides on — see [`Activity`].
//! 4. **Repeated-command churn** — how much of a session's command-bearing tool activity was the
//!    same command run again, from the `command` field a [`ToolEvent`] may carry. See
//!    [`CommandChurn`].
//! 5. **Context compaction** — how many times a session compacted its own context window, from
//!    the [`Compaction`] markers a [`Record`] may carry. See [`Compactions`].
//!
//! # What counts as a tool outcome
//!
//! Only events that carry an *explicit* outcome signal enter the error-rate denominator:
//!
//! - a `success` field — Copilot's `tool.execution_complete`, which states `true`/`false`;
//! - a `tool_result` event — Claude Code's result block, where an absent `is_error` means the
//!   call succeeded.
//!
//! Invocations (`tool_use`, `tool.execution_start`, `external_tool.requested`), `skill.invoked`,
//! bare completion markers (`external_tool.completed`), and Codex's `function_call_output` carry
//! no outcome signal at all, so they count as tool *activity* but never as an attempt that
//! could have failed. That is a deliberate under-claim: a session whose harness cannot express
//! tool failure reports no error rate rather than a flattering zero. Codex transcripts are
//! entirely in that position today.
//!
//! # Attributing an outcome to a tool name
//!
//! Outcome events name the call, not the tool: Claude Code's `tool_result` carries only
//! `tool_use_id`, and Copilot's completion only `toolCallId`. The fold therefore remembers the
//! call id -> tool name mapping from the invocation events it has already streamed past, which
//! is sound because every harness emits the invocation before its result. An outcome whose call
//! id was never introduced is counted in the session total and in
//! [`ToolOutcomes::unattributed`], never guessed at.
//!
//! # What counts as a command
//!
//! Only a literal `command` **field** on a tool event enters the churn fold. The pinned
//! interpreter types that key for every shell shape it certifies (munshi#77's first field
//! promotion, the pull this metric named): Claude Code's `Bash` `tool_use`, Copilot's `bash` /
//! `local_shell` execution events, and Codex's `local_shell_call` and `function_call`/`shell`.
//! Extraction lives upstream on purpose — reaching into the `input`/`arguments` blobs from here
//! would be this crate re-parsing transcript payloads, which is precisely what read-time
//! interpretation through the shared crate exists to prevent.
//!
//! # What counts as a compaction
//!
//! One [`CompactionPhase::Complete`] marker that is **not known to have failed**, and nothing
//! else. Both halves of that sentence are the interpreter's warnings taken literally, and both
//! would be silently wrong the obvious way round — see [`Compactions`].
//!
//! Unlike every other metric here, a session with no marker in it is a *reading of none* rather
//! than no reading, for the two harnesses that write markers: Claude Code and Copilot both type
//! one, so a transcript of theirs with none in it did not compact. Codex is the opposite case and
//! gets the usual treatment — the interpreter reads no Codex compaction at all, so a Codex
//! transcript says nothing either way and claims nothing.
//!
//! A session that never records the field still has **no churn reading**, not a zero one,
//! exactly as a harness that cannot express tool failure gets no error rate rather than a
//! flattering zero. Post-promotion that is the honest minority: on the 2026-08-18 mirror,
//! 408 of 623 sessions carry at least one command, and the rest — sessions that ran no shell —
//! claim nothing.
//!
//! # Where the events were (qanungo #5)
//!
//! The fold also records **evidence anchors** — bounded per-session locators for the events a rule
//! will count, so a finding can point at them instead of only counting them. They are strictly
//! additive: not one number above is computed from an anchor, and the same fold with anchors
//! stripped produces the same verdicts, the same scores, and the same report. See
//! [`crate::evidence`].
//!
//! The one thing anchors do change is the fold's *memory*, and every list of them is capped for
//! the same reason the maps above are: a transcript is somebody else's data, and its shape must
//! not decide this process's footprint.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::BufRead;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use munshi_transcript::{
    Classification, Compaction, CompactionPhase, Event, Record, SessionSummary, Source, ToolEvent,
    TranscriptStream, UnsupportedVersion,
};

use crate::evidence::{
    EventAnchor, MAX_ANCHORED_COMMANDS, MAX_ANCHORS_PER_FINDING, SessionAnchors,
};
use crate::rules::thresholds::IDLE_GAP;

/// Upper bound on remembered call-id -> tool-name pairs per session. A transcript is one
/// conversation, so this is orders of magnitude above any real session; it exists so a
/// pathological or adversarial transcript cannot make the fold's memory grow without bound.
pub(crate) const MAX_CORRELATED_CALLS: usize = 100_000;

/// Sitting boundaries one session keeps for the structural evidence a session-shaped rule renders
/// (qanungo #5). **A tunable, not a decision.**
///
/// The archive's median session is multi-sitting and its tail runs to dozens; a dashboard row shows
/// a reader the shape of the work, not an inventory of it. The count of sittings is exact whatever
/// this is — [`Activity::sittings`] counts breaks, not remembered spans — so a session past the cap
/// renders the first twelve and says how many it did not.
pub const MAX_STRUCTURAL_SITTINGS: usize = 12;

/// Upper bound on distinct command values remembered per session, on the same reasoning as the
/// call-id cap above: a real session runs commands in the hundreds, and the fold must not
/// let a pathological transcript — thousands of never-repeated one-off commands — grow its memory
/// without bound. Lower than the call-id cap because the keys here are whole command lines rather
/// than short ids. Its effect when reached is an under-claim, never an over-claim: see
/// [`CommandChurn::untracked_events`].
pub const MAX_DISTINCT_COMMANDS: usize = 20_000;

/// Attempts and failures for one tool, or for a whole session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolTally {
    /// Tool calls that reported an outcome.
    pub attempts: u64,
    /// Of those, the ones that reported failure.
    pub errors: u64,
}

impl ToolTally {
    fn observe(&mut self, succeeded: bool) {
        self.attempts += 1;
        if !succeeded {
            self.errors += 1;
        }
    }

    /// The failure fraction, or `None` when nothing reported an outcome — an undefined rate and
    /// a zero rate must not be confused by a rule.
    pub fn error_rate(&self) -> Option<f64> {
        (self.attempts > 0).then(|| self.errors as f64 / self.attempts as f64)
    }
}

/// Tool outcomes for one session, in total and by tool name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOutcomes {
    pub total: ToolTally,
    /// Keyed by the tool name the harness recorded (`Bash`, `Edit`, `local_shell`, ...). Tool
    /// names are schema metadata, not conversation content, and are the one string a report may
    /// render verbatim.
    pub by_tool: BTreeMap<String, ToolTally>,
    /// Outcomes whose call id was never introduced by an invocation, so no tool name could be
    /// attributed. Counted rather than guessed.
    pub unattributed: u64,
}

/// Repeated-command churn for one session: how much of its command-bearing tool activity was the
/// same command being run again.
///
/// # What it counts
///
/// Tool events carrying a `command` field are grouped by that field's exact value. A value seen
/// `n` times contributes `n - 1` repeats, so a session that never re-runs anything has zero
/// repeats however many commands it ran. Which events carry the field at all — and why a session
/// without one reports nothing rather than zero — is in the module documentation.
///
/// # Exact match, on purpose
///
/// Two commands are the same command iff their recorded strings are byte-identical. No trimming,
/// no case folding, no argument reordering, no path canonicalization, no shell parsing. This is a
/// deliberate first cut, not an oversight: every normalization rule is a guess about which
/// difference between two command lines does not matter, `cd /a && make` and `cd /b && make` are
/// genuinely different work, and a wrong guess produces false *positives* — the expensive
/// direction for a coaching report that is supposed to be worth reading. Exact matching can only
/// miss churn, never invent it. Normalization is a later refinement, pulled by false negatives
/// somebody actually observed.
///
/// # The strings do not live here
///
/// The command values exist only inside [`CommandRuns`], the fold's scratch memory, for the
/// length of one transcript. What survives the fold is this struct: counts, and nothing a
/// rendering path could print even by accident. That is the redaction line held by construction
/// rather than by filtering — see [`crate::report`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandChurn {
    /// Tool events that carried a `command` field at all — the session's command-bearing
    /// activity, and the denominator of [`CommandChurn::repeat_share`].
    pub command_events: u64,
    /// Command runs that were not the first run of their value: `Σ (runs - 1)` over the tracked
    /// values.
    pub repeats: u64,
    /// Distinct command values tracked.
    pub distinct_commands: usize,
    /// Of those, the ones that ran two or more times.
    pub repeated_commands: usize,
    /// The run count of the most-repeated single value. Zero for a session with no command-bearing
    /// event; read it through [`CommandChurn::busiest_runs`], which reports that case as `None`.
    pub busiest_command_runs: u64,
    /// Command-bearing events whose value could not be tracked because the session had already
    /// filled [`MAX_DISTINCT_COMMANDS`] distinct values. They stay in `command_events` — they are
    /// real activity — but can never become repeats, so a capped session under-reports its churn
    /// rather than guessing at it. Zero for every session the archive has ever held.
    pub untracked_events: u64,
}

impl CommandChurn {
    /// Whether this session recorded any command at all. A `false` here means *undefined*, not
    /// "no churn": the harness never told us what it ran.
    pub fn measurable(&self) -> bool {
        self.command_events > 0
    }

    /// The fraction of command-bearing activity that was a repeat, or `None` when the session
    /// recorded no command.
    pub fn repeat_share(&self) -> Option<f64> {
        self.measurable()
            .then(|| self.repeats as f64 / self.command_events as f64)
    }

    /// How many times the most-repeated single command value ran, or `None` when the session
    /// recorded no command. One is a legitimate answer: everything ran exactly once.
    pub fn busiest_runs(&self) -> Option<u64> {
        self.measurable().then_some(self.busiest_command_runs)
    }
}

/// What one exact command value did inside one session: how often it ran, and — for the first
/// [`MAX_ANCHORED_COMMANDS`] values a session records — where.
#[derive(Debug, Default)]
struct CommandValue {
    runs: u64,
    /// Bounded at [`MAX_ANCHORS_PER_FINDING`]: the retry-loop finding offers at most that many, so
    /// keeping more would be memory spent on evidence nothing will ask for. Empty for a value the
    /// anchor cap refused, which costs the finding its excerpts and costs its *count* nothing.
    anchors: Vec<EventAnchor>,
}

/// The fold's scratch memory for [`CommandChurn`]: the only place a command string exists in this
/// crate, and it is dropped when the transcript ends.
#[derive(Debug, Default)]
struct CommandRuns {
    /// Run count per exact command value, capped at [`MAX_DISTINCT_COMMANDS`] keys.
    runs: HashMap<String, CommandValue>,
    /// Every command-bearing event, tracked or not.
    events: u64,
    /// Events the cap refused a key for.
    untracked: u64,
    /// Distinct values that have been given an anchor list, capped at [`MAX_ANCHORED_COMMANDS`].
    anchored: usize,
}

impl CommandRuns {
    /// Folds one command-bearing tool event, remembering where it was.
    fn observe(&mut self, command: &str, anchor: EventAnchor) {
        self.events += 1;
        if let Some(value) = self.runs.get_mut(command) {
            value.runs += 1;
            if value.anchors.len() < MAX_ANCHORS_PER_FINDING {
                value.anchors.push(anchor);
            }
        } else if self.runs.len() < MAX_DISTINCT_COMMANDS {
            // Anchors stop at their own cap while counting goes on to the larger one: the churn
            // numbers a rule decides on are unaffected by how much evidence this session can offer.
            let anchors = if self.anchored < MAX_ANCHORED_COMMANDS {
                self.anchored += 1;
                vec![anchor]
            } else {
                Vec::new()
            };
            self.runs
                .insert(command.to_owned(), CommandValue { runs: 1, anchors });
        } else {
            self.untracked += 1;
        }
    }

    /// Reduces the run counts to the countable summary, dropping every string, and hands back the
    /// busiest value's anchors alongside it — the events the retry-loop rule counts.
    ///
    /// The busiest value is chosen by run count and, when two values tie, by whichever ran
    /// **first**. The tie-break is not cosmetic: a `HashMap`'s iteration order varies between runs
    /// of the same process, so without it two folds of one transcript could offer different
    /// evidence for the same finding.
    fn finish(self) -> (CommandChurn, Vec<EventAnchor>) {
        let mut churn = CommandChurn {
            command_events: self.events,
            distinct_commands: self.runs.len(),
            untracked_events: self.untracked,
            ..CommandChurn::default()
        };
        let mut busiest: Option<(u64, u64, Vec<EventAnchor>)> = None;
        for value in self.runs.into_values() {
            churn.repeats += value.runs - 1;
            churn.busiest_command_runs = churn.busiest_command_runs.max(value.runs);
            if value.runs >= 2 {
                churn.repeated_commands += 1;
            }
            // A value with no anchors sorts last among its equals rather than winning a tie and
            // offering nothing.
            let first = value
                .anchors
                .first()
                .map_or(u64::MAX, |anchor| anchor.locator);
            let better = busiest.as_ref().is_none_or(|(runs, earliest, _)| {
                value.runs > *runs || (value.runs == *runs && first < *earliest)
            });
            if better {
                busiest = Some((value.runs, first, value.anchors));
            }
        }
        (
            churn,
            busiest.map(|(_, _, anchors)| anchors).unwrap_or_default(),
        )
    }
}

/// Context compactions for one session: how many times the window was compacted, and how large it
/// had grown when that happened.
///
/// # Counting is `Complete`, not markers
///
/// Copilot writes a `session.compaction_start` **and** a `session.compaction_complete` for every
/// compaction; Claude Code writes one `compact_boundary` afterwards, which the interpreter reads as
/// [`CompactionPhase::Complete`] for exactly this reason. A fold over markers would therefore count
/// every Copilot compaction twice and every Claude Code one once — a number that means two
/// different things in the same column. Counting `Complete` is what makes the two comparable, and
/// [`Compactions::started`] is kept only as context.
///
/// # Failure is `!= Some(false)`, never `== Some(true)`
///
/// Copilot states `success` on a completion; Claude Code's boundary states no outcome at all, so
/// `succeeded` is `None` on every one of them. `== Some(true)` would therefore score the whole
/// Claude Code harness at zero compactions — the rule that reads the same in both harnesses is
/// *not known to have failed*, which is what `None` means. The three completions the archive holds
/// that did fail are counted in [`Compactions::failed`] and kept out of the total, because a
/// compaction that failed did not manage the window.
///
/// # No denominator exists, so utilization is not a metric here
///
/// [`Compaction::token_limit`] is absent on every Claude Code boundary and on all but five Copilot
/// pairs, so there is no window size to divide a pre-compaction total by. The totals below are
/// therefore **context in a finding and never a scored component**: a coaching report that turned
/// "compacted at 403k tokens" into points would be scoring a number against a denominator it
/// invented. What the pack does score is the *repetition*, which needs no denominator at all.
///
/// # Absent markers, for a harness that writes them, is a real zero
///
/// [`Compactions::observable`] carries that distinction. It is decided by the interpreter that read
/// the transcript, not by whether markers turned up: Claude Code and Copilot both type markers, so
/// none found means none happened, while `munshi-transcript` reads no Codex compaction whatsoever
/// (the archive holds zero Codex sessions, so its shape was never certified) and a Codex transcript
/// is a session nothing looked at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Compactions {
    /// Whether the harness that wrote this transcript types compaction markers at all. `false` is
    /// *nobody looked*; `true` with a zero count is *nothing happened*.
    pub observable: bool,
    /// [`CompactionPhase::Complete`] markers not known to have failed — the session's compactions,
    /// on the one counting rule that means the same thing in both harnesses.
    pub completed: u64,
    /// `Complete` markers whose source stated the compaction failed. Kept out of `completed` and
    /// reported beside it: a failed compaction is a context wall hit, not a window managed.
    pub failed: u64,
    /// [`CompactionPhase::Start`] markers. Context only, and never a compaction: counting these
    /// alongside completions is the double-count the phase filter exists to prevent. A `Start`
    /// with no completion after it is a session captured mid-compaction, which the archive has
    /// never held.
    pub started: u64,
    /// Counted completions that stated how large the context was beforehand. Below `completed`
    /// wherever the source said nothing.
    pub pre_tokens_stated: u64,
    /// The largest pre-compaction context any counted completion stated.
    pub pre_tokens_max: Option<u64>,
    /// Those pre-compaction totals, summed — over the counted completions only, on the same phase
    /// filter as the count itself, since both halves of a Copilot compaction can state one.
    pub pre_tokens_total: u64,
}

impl Compactions {
    /// The session's compaction count, or `None` when nothing could look for one.
    ///
    /// `Some(0)` is a real answer and the common one: a session that never filled its window.
    pub const fn count(&self) -> Option<u64> {
        if self.observable {
            Some(self.completed)
        } else {
            None
        }
    }

    /// The mean pre-compaction context across the completions that stated one, or `None` when none
    /// did. Averaged over the completions that *spoke*, never over all of them — a completion that
    /// stated no figure is missing, not zero.
    ///
    /// Whole tokens, because a token is a whole thing and the figure is rendered at three
    /// significant digits anyway.
    pub const fn mean_pre_tokens(&self) -> Option<u64> {
        self.pre_tokens_total.checked_div(self.pre_tokens_stated)
    }

    /// Folds one record's compaction marker, if it has one.
    fn observe(&mut self, compaction: &Compaction) {
        match compaction.phase {
            CompactionPhase::Start => self.started += 1,
            CompactionPhase::Complete if compaction.succeeded == Some(false) => self.failed += 1,
            CompactionPhase::Complete => {
                self.completed += 1;
                if let Some(pre) = compaction.pre_tokens {
                    self.pre_tokens_stated += 1;
                    self.pre_tokens_total = self.pre_tokens_total.saturating_add(pre);
                    self.pre_tokens_max = Some(self.pre_tokens_max.unwrap_or(0).max(pre));
                }
            }
        }
    }
}

/// Whether this interpreter reads compaction markers for a harness at all — the eligibility
/// question, asked of the reader rather than of the transcript.
///
/// Claude Code (`system`/`compact_boundary`) and Copilot (`session.compaction_start` /
/// `_complete`) both have a certified marker shape, so their transcripts answer the question either
/// way. `munshi-transcript` deliberately reads **no** Codex compaction: the rollout schema names a
/// `compacted` record, but the archive holds zero Codex sessions and the crate has never seen one
/// payload of it, so guessing at the shape would be inventing an interpretation. A Codex session is
/// therefore one nothing looked at, and it belongs in no fire rate's denominator.
const fn compaction_observable(source: Source) -> bool {
    match source {
        Source::ClaudeCode | Source::Copilot => true,
        Source::Codex => false,
    }
}

/// Gap-aware activity for one session: how much of its span was actually worked, and in how many
/// separate sittings.
///
/// # Why span is not the answer (qanungo #14)
///
/// A transcript is one *conversation*, not one *stretch of work*: an operator resumes a session
/// tomorrow and the file goes on where it stopped. Across the 2026-08-18 archive (564
/// transcripts, 575k dated records), **95.8% of summed wall-clock span is idle time** and 59.6%
/// of sessions are resumed at least once, so span overstates the work a session took by a median
/// factor of 4.9 and a p90 factor of 119.
///
/// # The fold
///
/// One pass, in *file order*, carrying a single previous timestamp and four accumulators:
///
/// ```text
/// gap_i           = max(0, t_{i+1} − t_i)      // inversions clamp to zero, records are not sorted
/// active_time     = Σ gap_i where gap_i ≤ IDLE_GAP
/// sitting         = a maximal run whose internal gaps are all ≤ IDLE_GAP
/// longest_sitting = the longest such run's own span
/// sittings        = 1 + count(gap_i > IDLE_GAP)
/// ```
///
/// Records arrive slightly out of order in 30% of sessions (0.52% of adjacencies, almost all
/// sub-second interleaving of a tool result with the message that provoked it). Sorting would
/// mean buffering every timestamp in a transcript that can reach hundreds of megabytes, and it
/// changes which sessions clear a two-hour sitting by **two** across the whole archive — so the
/// fold clamps and stays streaming.
///
/// A run's span is accumulated as the sum of its own internal gaps rather than as last − first,
/// which is the same number for ordered records and cannot go negative for inverted ones. The
/// price of the clamp is on the other side: an inverted pair re-traverses an interval the fold
/// has already counted, so on an out-of-order transcript the summed gaps can *exceed* last −
/// first, and `active_time` or `longest_sitting` can come out larger than
/// [`SessionMetrics::span`]. That is the accepted trade for staying streaming — the affected
/// adjacencies are 0.52% of the archive and almost all sub-second — but it means the two
/// quantities are not guaranteed to order, and nothing here should assume they do.
///
/// Every reading is `None` for a session with fewer than two dated records: with no adjacency
/// there is no gap, and a lone record's activity is undefined rather than zero — the same
/// discipline [`SessionMetrics::span`] already applies.
///
/// # The boundaries, and why they are here (qanungo #5)
///
/// The same fold also remembers *when* the first [`MAX_STRUCTURAL_SITTINGS`] sittings ran. That is
/// the structural evidence a session-shaped rule offers instead of an excerpt: a marathon did not
/// count an utterance, it measured a stretch of clock, so what it shows a reader is that stretch.
/// The list is bounded and the counts are not — [`Activity::sittings`] still counts every break —
/// so a fifty-sitting transcript renders twelve boundaries and says it has thirty-eight more.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// Records that carried a parseable timestamp. Two are needed before anything is defined.
    dated_records: u64,
    /// Σ of the gaps that were short enough to count as work.
    active: TimeDelta,
    /// The longest *finished* run's span; the run in progress is folded in on read.
    longest_finished_sitting: TimeDelta,
    /// The span of the run currently being accumulated.
    current_sitting: TimeDelta,
    /// Gaps over [`IDLE_GAP`], each of which ended a sitting.
    breaks: u64,
    /// The carried previous timestamp — the fold's entire memory of the *numbers*.
    previous: Option<DateTime<Utc>>,
    /// When the run currently being accumulated started.
    current_start: Option<DateTime<Utc>>,
    /// The first [`MAX_STRUCTURAL_SITTINGS`] *finished* sittings; the run in progress is appended
    /// on read, exactly as its span is.
    finished_sittings: Vec<Sitting>,
}

/// One continuous stretch of work: two timestamps, and nothing else.
///
/// A sitting of a single record has `from == to`, which is a real shape — a lone record after a
/// long break — and not a missing reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sitting {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl Sitting {
    /// How long the sitting ran, as the clock saw it. Never negative: the fold clamps inversions
    /// before a boundary is ever closed.
    pub fn span(&self) -> TimeDelta {
        (self.to - self.from).max(TimeDelta::zero())
    }
}

impl Activity {
    /// Folds the dated records of one transcript, which must be offered in file order.
    pub fn over(timestamps: impl IntoIterator<Item = DateTime<Utc>>) -> Self {
        let mut activity = Self::default();
        for at in timestamps {
            activity.observe(at);
        }
        activity
    }

    /// Folds one dated record.
    pub fn observe(&mut self, at: DateTime<Utc>) {
        if let Some(previous) = self.previous {
            let gap = (at - previous).max(TimeDelta::zero());
            if gap <= IDLE_GAP {
                self.active += gap;
                self.current_sitting += gap;
            } else {
                self.longest_finished_sitting =
                    self.longest_finished_sitting.max(self.current_sitting);
                // The sitting that just ended ran from where it started to the record *before*
                // this one; this record opens the next one.
                if let Some(from) = self.current_start
                    && self.finished_sittings.len() < MAX_STRUCTURAL_SITTINGS
                {
                    self.finished_sittings.push(Sitting { from, to: previous });
                }
                self.current_sitting = TimeDelta::zero();
                self.current_start = Some(at);
                self.breaks += 1;
            }
        } else {
            self.current_start = Some(at);
        }
        self.previous = Some(at);
        self.dated_records += 1;
    }

    /// Whether this session had two dated records to put a gap between.
    fn measurable(&self) -> bool {
        self.dated_records >= 2
    }

    /// Time inside sittings: the session's work, as opposed to its calendar footprint.
    pub fn active_time(&self) -> Option<TimeDelta> {
        self.measurable().then_some(self.active)
    }

    /// The longest continuous stretch of work — what "marathon" actually means.
    pub fn longest_sitting(&self) -> Option<TimeDelta> {
        self.measurable()
            .then(|| self.longest_finished_sitting.max(self.current_sitting))
    }

    /// How many times the session was picked back up, counting the first sitting.
    pub fn sittings(&self) -> Option<usize> {
        self.measurable().then(|| {
            usize::try_from(self.breaks)
                .unwrap_or(usize::MAX)
                .saturating_add(1)
        })
    }

    /// The first [`MAX_STRUCTURAL_SITTINGS`] sittings, in file order, with the one still in
    /// progress folded in — exactly as [`Activity::longest_sitting`] folds its span in.
    ///
    /// Empty for a session with nothing to put a gap in, on the same discipline as every other
    /// reading here: no adjacency, no sitting.
    pub fn sitting_boundaries(&self) -> Vec<Sitting> {
        if !self.measurable() {
            return Vec::new();
        }
        let mut boundaries = self.finished_sittings.clone();
        if let (Some(from), Some(to)) = (self.current_start, self.previous)
            && boundaries.len() < MAX_STRUCTURAL_SITTINGS
        {
            boundaries.push(Sitting { from, to });
        }
        boundaries
    }

    /// Sittings the cap kept out of [`Activity::sitting_boundaries`]. Reported rather than hidden:
    /// a bar chart of twelve sittings drawn for a session that had fifty would be a lie about the
    /// shape of the work.
    pub fn sittings_elided(&self) -> usize {
        self.sittings()
            .unwrap_or_default()
            .saturating_sub(self.sitting_boundaries().len())
    }
}

/// The metrics folded out of one session's transcript, plus the evidence needed to cite it.
#[derive(Debug, Clone)]
pub struct SessionMetrics {
    /// The content hash Patwari serves for this transcript — the report's only evidence handle.
    pub source_hash: String,
    /// The harness that produced the transcript (`claude-code`, `copilot-cli`, `codex-cli`).
    pub source_agent: String,
    /// The artifact-set contract the snapshot declared, carried so an anchored event can be read
    /// back out of the cached blob with the same interpreter the fold used. Nothing scores it.
    pub artifact_set_version: u16,
    /// The counting fold `munshi-transcript` already defines, restated over the same stream.
    pub summary: SessionSummary,
    pub tools: ToolOutcomes,
    /// The gap-aware reading of the same timestamps the summary took its span from.
    pub activity: Activity,
    /// Repeated-command churn, undefined for a harness that records no command field.
    pub commands: CommandChurn,
    /// Context compactions, undefined for a harness this interpreter reads no marker for.
    pub compactions: Compactions,
    /// Where the events a rule will count were. Strictly additive — see [`crate::evidence`].
    pub anchors: SessionAnchors,
    /// Transcript bytes read, for the fold-cost footer.
    pub bytes_folded: u64,
}

impl SessionMetrics {
    /// Wall-clock span between the first and last dated record.
    ///
    /// Kept as **context**, not as an input to any rule: a 119h span across 43 sittings and a
    /// 119h span worked straight through would read identically here, and only one of them is a
    /// finding. See [`Activity`].
    pub fn span(&self) -> Option<TimeDelta> {
        self.summary
            .first_timestamp
            .zip(self.summary.last_timestamp)
            .map(|(first, last)| last - first)
    }

    /// The UTC calendar day the session started on, for the cadence fold.
    pub fn day(&self) -> Option<NaiveDate> {
        self.summary.first_timestamp.map(|first| first.date_naive())
    }

    /// Time spent inside sittings — the duration a coaching report reasons about.
    pub fn active_time(&self) -> Option<TimeDelta> {
        self.activity.active_time()
    }

    /// The session's longest continuous stretch of work.
    pub fn longest_sitting(&self) -> Option<TimeDelta> {
        self.activity.longest_sitting()
    }

    /// How many separate sittings the transcript was worked in.
    pub fn sittings(&self) -> Option<usize> {
        self.activity.sittings()
    }

    /// How much larger the session's calendar footprint is than the work inside it. `None` when
    /// there is no gap to measure. A session whose records are *all* more than [`IDLE_GAP`] apart
    /// has zero active time, and this is then infinite — a maximally diluted transcript, which is
    /// exactly what the resumed-session rule is looking for. A session whose span is also zero
    /// (every record on the same instant) yields `NaN`, which compares false against every
    /// threshold and so fires nothing.
    /// Taken in milliseconds rather than seconds: a session whose entire activity is under a
    /// second is a real shape (a burst of records, then nothing), and truncating it to zero would
    /// report it as having no work at all rather than as having very little.
    pub fn span_to_active(&self) -> Option<f64> {
        let (span, active) = self.span().zip(self.active_time())?;
        Some(span.num_milliseconds() as f64 / active.num_milliseconds() as f64)
    }

    /// Tool activities per user request, or `None` when the session recorded no user request.
    pub fn tools_per_request(&self) -> Option<f64> {
        (self.summary.user_requests > 0)
            .then(|| self.summary.tool_activities as f64 / self.summary.user_requests as f64)
    }
}

/// What one transcript fold produced, before it is paired with its archive identity.
#[derive(Debug, Clone, Default)]
pub struct Fold {
    pub summary: SessionSummary,
    pub tools: ToolOutcomes,
    pub activity: Activity,
    pub commands: CommandChurn,
    pub compactions: Compactions,
    pub anchors: SessionAnchors,
}

/// Folds one transcript, streaming: one pass, no buffering of records, memory bounded by the
/// per-tool tallies, the call-id correlation map, and the command-run map.
///
/// # Errors
///
/// Returns an error when `artifact_set_version` names an artifact contract this build's
/// interpreter does not support — the transcript is then left uninterpreted rather than read
/// with stale assumptions.
pub fn fold_transcript(
    source: Source,
    artifact_set_version: u16,
    reader: impl BufRead,
) -> Result<Fold, UnsupportedVersion> {
    let stream = TranscriptStream::new(source, artifact_set_version, reader)?;
    let mut fold = Fold::default();
    // Decided by the interpreter this fold is reading with, before a single record is seen: what a
    // *missing* marker means is a property of the harness, not of the transcript.
    fold.compactions.observable = compaction_observable(source);
    let mut names: HashMap<String, String> = HashMap::new();
    let mut commands = CommandRuns::default();
    // The locator every anchor is keyed by: the tool event's ordinal in file order. It is counted
    // here, beside the events, rather than read off `summary.tool_activities` — the two are the
    // same number today and one of them is a locator, which is a contract, and the other is a
    // metric, which is not.
    let mut tool_events = 0_u64;
    for item in stream {
        fold.summary.observe(&item);
        let Ok(record) = &item else { continue };
        // In file order, every dated record — content or bookkeeping — is evidence that somebody
        // or something was still at the keyboard, so the gap fold sees all of them.
        if let Some(at) = record.timestamp {
            fold.activity.observe(at);
        }
        // A third independent read-pass, exactly as the interpreter promotes it: every marker record
        // is `Ignored` bookkeeping, so this is deliberately not inside the `Content` arm below.
        if let Some(compaction) = compaction_of(record) {
            fold.compactions.observe(compaction);
        }
        if let Classification::Content { events } = &record.classification {
            for event in events {
                if let Event::Tool(tool) = event {
                    tool_events += 1;
                    let anchor = |tool_name: Option<&str>| EventAnchor {
                        locator: tool_events,
                        record: record.record,
                        line: record.line,
                        at: record.timestamp,
                        tool: tool_name.map(ToOwned::to_owned),
                    };
                    observe_tool(
                        &mut fold.tools,
                        &mut fold.anchors,
                        &mut names,
                        tool,
                        &anchor,
                    );
                    // Compared in memory, counted, and dropped: the value never leaves this
                    // function, and nothing downstream of the fold can render it. What survives is
                    // the anchor — where the run was, never what it ran.
                    if let Some(command) = tool.fields.get("command") {
                        commands.observe(command, anchor(tool.name()));
                    }
                }
            }
        }
    }
    let (churn, busiest_runs) = commands.finish();
    fold.commands = churn;
    fold.anchors.command_runs = busiest_runs;
    Ok(fold)
}

/// The compaction a record marks, if it marks one. A named function rather than a `Box` deref at
/// the call site, so the one place this crate reads the promoted field is greppable.
fn compaction_of(record: &Record) -> Option<&Compaction> {
    record.compaction.as_deref()
}

/// Folds one tool event: remember what it names, then count what it reports — and, when what it
/// reports is a failure, remember where it was.
fn observe_tool(
    outcomes: &mut ToolOutcomes,
    anchors: &mut SessionAnchors,
    names: &mut HashMap<String, String>,
    tool: &ToolEvent,
    anchor: &impl Fn(Option<&str>) -> EventAnchor,
) {
    if let (Some(call_id), Some(name)) = (tool.call_id(), tool.name())
        && names.len() < MAX_CORRELATED_CALLS
    {
        names.insert(call_id.to_owned(), name.to_owned());
    }
    let Some(succeeded) = outcome(tool) else {
        return;
    };
    outcomes.total.observe(succeeded);
    let name = tool.name().map(ToOwned::to_owned).or_else(|| {
        tool.call_id()
            .and_then(|call_id| names.get(call_id).cloned())
    });
    if !succeeded {
        anchors.observe_error(name.as_deref(), anchor(name.as_deref()));
    }
    match name {
        Some(name) => outcomes.by_tool.entry(name).or_default().observe(succeeded),
        None => outcomes.unattributed += 1,
    }
}

/// Whether this event reports a tool outcome, and whether that outcome succeeded. See the module
/// docs for why only these two shapes count.
///
/// Visible to the crate because [`crate::evidence`] answers the same question when it serves an
/// excerpt, and an excerpt that called an event a failure on rules the fold does not use would be a
/// second definition of "failed".
pub(crate) fn outcome(tool: &ToolEvent) -> Option<bool> {
    if let Some(success) = tool.fields.get("success") {
        return Some(success == "true");
    }
    if tool.event() == Some("tool_result") {
        return Some(tool.fields.get("is_error").map(String::as_str) != Some("true"));
    }
    None
}

/// Maps a manifest's `session.source_agent` label onto the interpreter that reads that harness's
/// transcripts. An unrecognized harness is reported as such rather than guessed at.
pub fn source_for_agent(source_agent: &str) -> Option<Source> {
    match source_agent {
        "copilot-cli" | "copilot" => Some(Source::Copilot),
        "claude-code" | "claude" => Some(Source::ClaudeCode),
        "codex-cli" | "codex" => Some(Source::Codex),
        _ => None,
    }
}

/// Sessions per day across the reported window, and the span distribution behind it.
///
/// **Days are UTC calendar days**, not the operator's local days, because that is the only
/// boundary the transcripts themselves define — record timestamps are RFC 3339 and normalized to
/// UTC by `munshi-transcript`, and no capture records the harness's local zone. A late-evening
/// session west of Greenwich therefore lands on the following UTC day, so "active days" and
/// "busiest day" can disagree with a local-time intuition by one. The report says UTC where it
/// prints them; correcting for a real local zone is a later question, not a silent guess here.
#[derive(Debug, Clone, Default)]
pub struct Cadence {
    /// Sessions per UTC calendar day, by the day each session's first record landed on.
    pub per_day: BTreeMap<NaiveDate, usize>,
    /// Sessions whose records carried no parseable timestamp at all.
    pub undated: usize,
    /// Every *dated* session's wall-clock span, ascending. Context for the actives below.
    ///
    /// The two series do not cover the same sessions, and the difference is not an oversight: a
    /// session with a single dated record has a span (of zero — first and last are the same
    /// record) but no measurable activity, so it appears here and not in `actives`. Comparing
    /// their lengths, or pairing them off by index, would be wrong.
    pub spans: Vec<TimeDelta>,
    /// Every *measurable* session's active time, ascending — the duration distribution a report
    /// reasons about. Sessions with fewer than two dated records are absent, per [`Activity`].
    pub actives: Vec<TimeDelta>,
    /// Active time summed over the window.
    pub total_active: TimeDelta,
    /// Wall-clock span summed over the window. Larger than [`Cadence::total_active`] by a factor
    /// of roughly twenty across the real archive, which is the whole point of reporting both.
    pub total_span: TimeDelta,
    /// Sittings summed over the window.
    pub total_sittings: usize,
}

impl Cadence {
    /// Folds the cadence metric over already-folded sessions.
    pub fn fold(sessions: &[SessionMetrics]) -> Self {
        let mut cadence = Self::default();
        for session in sessions {
            match session.day() {
                Some(day) => *cadence.per_day.entry(day).or_default() += 1,
                None => cadence.undated += 1,
            }
            if let Some(span) = session.span() {
                cadence.spans.push(span);
                cadence.total_span += span;
            }
            if let Some(active) = session.active_time() {
                cadence.actives.push(active);
                cadence.total_active += active;
            }
            cadence.total_sittings += session.sittings().unwrap_or_default();
        }
        cadence.spans.sort_unstable();
        cadence.actives.sort_unstable();
        cadence
    }

    /// Days on which at least one session started.
    pub fn active_days(&self) -> usize {
        self.per_day.len()
    }

    /// Sessions per *active* day. Reported instead of a flat per-calendar-day rate because a
    /// week with four sessions on one day is a different working pattern from one with four
    /// sessions spread across four days, and the flat rate hides the difference.
    pub fn sessions_per_active_day(&self) -> Option<f64> {
        let days = self.active_days();
        (days > 0).then(|| self.per_day.values().sum::<usize>() as f64 / days as f64)
    }

    /// The median session span, taken as the lower median so the value is always an observed
    /// span rather than an interpolated one.
    pub fn median_span(&self) -> Option<TimeDelta> {
        median(&self.spans)
    }

    /// The longest session span in the window.
    pub fn longest_span(&self) -> Option<TimeDelta> {
        self.spans.last().copied()
    }

    /// The median session's active time, on the same lower-median convention.
    pub fn median_active(&self) -> Option<TimeDelta> {
        median(&self.actives)
    }

    /// The most active time any one session in the window carried.
    pub fn longest_active(&self) -> Option<TimeDelta> {
        self.actives.last().copied()
    }
}

/// The lower median of an ascending series, so the reported value is always one that was
/// observed.
fn median(ascending: &[TimeDelta]) -> Option<TimeDelta> {
    ascending
        .get(ascending.len().saturating_sub(1) / 2)
        .copied()
}

/// Repeated-command churn summed over the window.
///
/// Deliberately not a summed [`CommandChurn`]: `distinct_commands` and `repeated_commands` are
/// per-session counts of *values*, and adding them across sessions would count one command that
/// two sessions both ran as two commands. What aggregates soundly is the activity, the repeats,
/// and how many sessions were in each state — so that is all this carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChurnTotals {
    /// Sessions that recorded at least one command — the only ones with a churn reading at all.
    pub sessions_with_commands: usize,
    /// Of those, the ones that re-ran at least one command.
    pub sessions_with_repeats: usize,
    pub command_events: u64,
    pub repeats: u64,
    /// The highest single-command run count reached by any one session in the window.
    pub busiest_command_runs: u64,
    /// Command-bearing events no session could track, per [`CommandChurn::untracked_events`].
    pub untracked_events: u64,
}

impl ChurnTotals {
    /// The window's repeat fraction, or `None` when nothing in it recorded a command.
    pub fn repeat_share(&self) -> Option<f64> {
        (self.command_events > 0).then(|| self.repeats as f64 / self.command_events as f64)
    }
}

/// Window-wide totals, for the report's aggregate section.
#[derive(Debug, Clone, Default)]
pub struct Totals {
    pub sessions: usize,
    pub user_requests: usize,
    pub assistant_messages: usize,
    pub tool_activities: usize,
    pub malformed_records: usize,
    pub tools: ToolTally,
    pub by_tool: BTreeMap<String, ToolTally>,
    pub churn: ChurnTotals,
    pub by_agent: BTreeMap<String, usize>,
    pub bytes_folded: u64,
    /// The window's first and last observed record timestamps.
    pub first_timestamp: Option<DateTime<Utc>>,
    pub last_timestamp: Option<DateTime<Utc>>,
}

impl Totals {
    /// Sums the folded sessions.
    pub fn fold(sessions: &[SessionMetrics]) -> Self {
        let mut totals = Self {
            sessions: sessions.len(),
            ..Self::default()
        };
        for session in sessions {
            totals.user_requests += session.summary.user_requests;
            totals.assistant_messages += session.summary.assistant_messages;
            totals.tool_activities += session.summary.tool_activities;
            totals.malformed_records += session.summary.malformed_records;
            totals.tools.attempts += session.tools.total.attempts;
            totals.tools.errors += session.tools.total.errors;
            totals.bytes_folded += session.bytes_folded;
            let churn = &session.commands;
            if churn.measurable() {
                totals.churn.sessions_with_commands += 1;
                totals.churn.command_events += churn.command_events;
                totals.churn.repeats += churn.repeats;
                totals.churn.untracked_events += churn.untracked_events;
                totals.churn.busiest_command_runs = totals
                    .churn
                    .busiest_command_runs
                    .max(churn.busiest_command_runs);
                if churn.repeats > 0 {
                    totals.churn.sessions_with_repeats += 1;
                }
            }
            *totals
                .by_agent
                .entry(session.source_agent.clone())
                .or_default() += 1;
            for (name, tally) in &session.tools.by_tool {
                let entry = totals.by_tool.entry(name.clone()).or_default();
                entry.attempts += tally.attempts;
                entry.errors += tally.errors;
            }
            if let Some(first) = session.summary.first_timestamp {
                totals.first_timestamp =
                    Some(totals.first_timestamp.map_or(first, |old| old.min(first)));
            }
            if let Some(last) = session.summary.last_timestamp {
                totals.last_timestamp =
                    Some(totals.last_timestamp.map_or(last, |old| old.max(last)));
            }
        }
        totals
    }

    /// Window-wide tool activities per user request.
    pub fn tools_per_request(&self) -> Option<f64> {
        (self.user_requests > 0).then(|| self.tool_activities as f64 / self.user_requests as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold_claude(transcript: &str) -> Fold {
        fold_transcript(Source::ClaudeCode, 2, transcript.as_bytes()).unwrap()
    }

    fn metrics(fold: Fold) -> SessionMetrics {
        SessionMetrics {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            summary: fold.summary,
            tools: fold.tools,
            activity: fold.activity,
            commands: fold.commands,
            compactions: fold.compactions,
            anchors: fold.anchors,
            bytes_folded: 0,
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A stand-in anchor at one locator, for the scratch-memory tests: what the churn fold does
    /// with an anchor is bound it, and none of these tests is about where an event was.
    fn locator(locator: u64) -> EventAnchor {
        EventAnchor {
            locator,
            record: locator,
            line: locator,
            at: None,
            tool: None,
        }
    }

    /// An activity folded from a start time plus the gaps, in minutes, between records.
    fn activity(gaps_minutes: &[i64]) -> Activity {
        let mut at = at("2026-08-01T09:00:00Z");
        let mut timestamps = vec![at];
        for gap in gaps_minutes {
            at += TimeDelta::minutes(*gap);
            timestamps.push(at);
        }
        Activity::over(timestamps)
    }

    /// A Claude Code transcript with `calls` tool calls, the first `failures` of which fail.
    fn claude_tool_transcript(tool: &str, calls: usize, failures: usize) -> String {
        let mut lines = vec![format!(
            r#"{{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"user","content":"kickoff"}}}}"#
        )];
        for index in 0..calls {
            lines.push(format!(
                r#"{{"type":"assistant","uuid":"a{index}","timestamp":"2026-08-01T10:0{}:00.000Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_{index}","name":"{tool}","input":{{}}}}]}}}}"#,
                index % 10
            ));
            let is_error = index < failures;
            lines.push(format!(
                r#"{{"type":"user","uuid":"r{index}","timestamp":"2026-08-01T10:0{}:30.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_{index}","content":"result","is_error":{is_error}}}]}}}}"#,
                index % 10
            ));
        }
        lines.join("\n")
    }

    #[test]
    fn tool_error_rate_counts_only_outcome_bearing_events() {
        let fold = fold_claude(&claude_tool_transcript("Bash", 4, 1));
        // Eight tool events (four invocations, four results); only the results report outcomes.
        assert_eq!(fold.summary.tool_activities, 8);
        assert_eq!(fold.tools.total.attempts, 4);
        assert_eq!(fold.tools.total.errors, 1);
        assert!((fold.tools.total.error_rate().unwrap() - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn tool_error_rate_is_attributed_per_tool_name_through_the_call_id() {
        let mut transcript = claude_tool_transcript("Bash", 2, 2);
        transcript.push('\n');
        transcript.push_str(&claude_tool_transcript("Read", 2, 0));
        let fold = fold_claude(&transcript);
        assert_eq!(
            fold.tools.by_tool["Bash"],
            ToolTally {
                attempts: 2,
                errors: 2
            }
        );
        assert_eq!(
            fold.tools.by_tool["Read"],
            ToolTally {
                attempts: 2,
                errors: 0
            }
        );
        assert_eq!(fold.tools.unattributed, 0);
    }

    /// Claude Code omits `is_error` entirely on a successful tool result — the classifier only
    /// ever inserts the field when it is `true`. An absent field must therefore read as success,
    /// not as "no outcome", or every clean session would report an undefined error rate.
    #[test]
    fn a_tool_result_without_an_is_error_field_counts_as_a_success() {
        let transcript = concat!(
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:01.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"file contents"}]}}"#,
        );
        let fold = fold_claude(transcript);
        assert_eq!(
            fold.tools.total,
            ToolTally {
                attempts: 1,
                errors: 0
            }
        );
        assert_eq!(fold.tools.total.error_rate(), Some(0.0));
        assert_eq!(
            fold.tools.by_tool["Read"],
            ToolTally {
                attempts: 1,
                errors: 0
            }
        );
    }

    /// The inverse of the above, so the pair pins both readings of the same absent field.
    #[test]
    fn a_tool_result_with_is_error_true_counts_as_a_failure() {
        let transcript = concat!(
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:01.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"no such file","is_error":true}]}}"#,
        );
        let fold = fold_claude(transcript);
        assert_eq!(
            fold.tools.total,
            ToolTally {
                attempts: 1,
                errors: 1
            }
        );
    }

    #[test]
    fn an_outcome_whose_call_was_never_introduced_is_counted_not_guessed() {
        let transcript = r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_orphan","content":"result","is_error":true}]}}"#;
        let fold = fold_claude(transcript);
        assert_eq!(
            fold.tools.total,
            ToolTally {
                attempts: 1,
                errors: 1
            }
        );
        assert_eq!(fold.tools.unattributed, 1);
        assert!(fold.tools.by_tool.is_empty());
    }

    #[test]
    fn an_empty_tally_has_no_error_rate_rather_than_a_zero_one() {
        assert_eq!(ToolTally::default().error_rate(), None);
        assert_eq!(
            ToolTally {
                attempts: 4,
                errors: 0
            }
            .error_rate(),
            Some(0.0)
        );
    }

    #[test]
    fn copilot_success_fields_drive_the_outcome_directly() {
        let transcript = concat!(
            r#"{"type":"tool.execution_start","timestamp":"2026-08-01T10:00:00.000Z","data":{"toolCallId":"c1","toolName":"local_shell"}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-08-01T10:00:01.000Z","data":{"toolCallId":"c1","success":false,"error":{"message":"boom"}}}"#,
            "\n",
            r#"{"type":"tool.execution_start","timestamp":"2026-08-01T10:00:02.000Z","data":{"toolCallId":"c2","toolName":"local_shell"}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-08-01T10:00:03.000Z","data":{"toolCallId":"c2","success":true}}"#,
        );
        let fold = fold_transcript(Source::Copilot, 2, transcript.as_bytes()).unwrap();
        assert_eq!(
            fold.tools.by_tool["local_shell"],
            ToolTally {
                attempts: 2,
                errors: 1
            }
        );
    }

    /// A Codex rollout whose `local_shell_call` records run `commands` in order — the one shape
    /// in the pinned interpreter that puts a `command` field on a tool event.
    fn codex_shell_transcript(commands: &[&str]) -> String {
        let mut lines = vec![
            r#"{"timestamp":"2026-08-01T10:00:00.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]}}"#
                .to_owned(),
        ];
        for (index, command) in commands.iter().enumerate() {
            lines.push(format!(
                r#"{{"timestamp":"2026-08-01T10:0{}:00.000Z","type":"response_item","payload":{{"type":"local_shell_call","call_id":"call_{index}","action":{{"type":"exec","command":["bash","-lc","{command}"]}}}}}}"#,
                index % 10,
            ));
        }
        lines.join("\n")
    }

    fn fold_codex(transcript: &str) -> Fold {
        fold_transcript(Source::Codex, 2, transcript.as_bytes()).unwrap()
    }

    /// The under-claim that matters most: Claude Code keeps the shell command inside `input`, so
    /// its tool events carry no `command` field, and the fold must report *nothing* about churn
    /// rather than a zero that would read as "this session never repeated itself".
    #[test]
    fn a_transcript_with_no_command_fields_makes_no_churn_claim() {
        let fold = fold_claude(&claude_tool_transcript("Bash", 6, 0));
        assert_eq!(fold.summary.tool_activities, 12, "there was tool activity");
        assert_eq!(fold.commands, CommandChurn::default());
        assert!(!fold.commands.measurable());
        assert_eq!(fold.commands.repeat_share(), None);
        assert_eq!(fold.commands.busiest_runs(), None);
    }

    #[test]
    fn repeats_are_counted_per_exact_command_value() {
        let fold = fold_codex(&codex_shell_transcript(&[
            "cargo test",
            "cargo test",
            "git status",
            "cargo test",
            "git status",
            "ls",
        ]));
        let churn = fold.commands;
        assert_eq!(churn.command_events, 6);
        assert_eq!(churn.distinct_commands, 3);
        // 3x `cargo test` and 2x `git status` are two repeats and one repeat; `ls` ran once.
        assert_eq!(churn.repeats, 3);
        assert_eq!(churn.repeated_commands, 2);
        assert_eq!(churn.busiest_runs(), Some(3));
        assert_eq!(churn.repeat_share(), Some(0.5));
    }

    /// A session that runs plenty of commands and repeats none of them has a churn reading, and
    /// that reading is zero — the distinction the whole `Option` discipline exists for.
    #[test]
    fn commands_that_never_repeat_are_measured_at_zero_churn() {
        let fold = fold_codex(&codex_shell_transcript(&["ls", "pwd", "whoami"]));
        let churn = fold.commands;
        assert!(churn.measurable());
        assert_eq!(churn.repeats, 0);
        assert_eq!(churn.repeated_commands, 0);
        assert_eq!(churn.repeat_share(), Some(0.0));
        assert_eq!(churn.busiest_runs(), Some(1));
    }

    /// Exact match means exact: two command lines that a human would call the same command are
    /// two commands here. Pinned so that adding normalization later is a visible decision rather
    /// than a silent one.
    #[test]
    fn near_identical_commands_are_different_commands() {
        let fold = fold_codex(&codex_shell_transcript(&[
            "cargo test",
            "cargo  test",
            "cargo test ",
            "CARGO TEST",
        ]));
        assert_eq!(fold.commands.distinct_commands, 4);
        assert_eq!(fold.commands.repeats, 0);
    }

    /// The memory bound, exercised at a cap of three rather than at twenty thousand: once the map
    /// is full a new command value cannot be tracked, so it is counted as activity and as
    /// explicitly untracked. Values already in the map keep counting their repeats, and the
    /// reported churn is a floor.
    #[test]
    fn the_distinct_command_cap_under_claims_rather_than_guesses() {
        let mut at = 0;
        let mut ran = |runs: &mut CommandRuns, command: &str| {
            at += 1;
            runs.observe(command, locator(at));
        };
        let mut runs = CommandRuns::default();
        for command in ["a", "b", "c", "a"] {
            ran(&mut runs, command);
        }
        // Four values, one of them a repeat, all inside the real cap.
        ran(&mut runs, "d");
        ran(&mut runs, "d");
        let (churn, _) = runs.finish();
        assert_eq!(churn.command_events, 6);
        assert_eq!(churn.distinct_commands, 4);
        assert_eq!(churn.untracked_events, 0);

        // Now the same stream against a map that is already full.
        let mut capped = CommandRuns::default();
        for index in 0..MAX_DISTINCT_COMMANDS {
            ran(&mut capped, &format!("command-{index}"));
        }
        assert_eq!(capped.runs.len(), MAX_DISTINCT_COMMANDS);
        ran(&mut capped, "command-0");
        ran(&mut capped, "beyond-the-cap");
        ran(&mut capped, "beyond-the-cap");
        let (churn, _) = capped.finish();
        assert_eq!(churn.distinct_commands, MAX_DISTINCT_COMMANDS);
        assert_eq!(
            churn.command_events,
            MAX_DISTINCT_COMMANDS as u64 + 3,
            "every command-bearing event is still counted as activity"
        );
        assert_eq!(
            churn.untracked_events, 2,
            "the two runs of the untracked value are named, not silently dropped"
        );
        assert_eq!(
            churn.repeats, 1,
            "only the value the map already held could contribute a repeat"
        );
    }

    /// A Copilot session whose compactions are written as `start`/`complete` pairs — the shape the
    /// phase filter exists for. Six markers, three compactions.
    #[test]
    fn copilot_compaction_pairs_are_counted_once_each() {
        let transcript = concat!(
            r#"{"type":"session.compaction_start","timestamp":"2026-08-01T10:00:00.000Z","data":{"currentTokens":100000,"tokenLimit":200000}}"#,
            "\n",
            r#"{"type":"session.compaction_complete","timestamp":"2026-08-01T10:01:00.000Z","data":{"success":true,"preCompactionTokens":100000,"postCompactionTokens":9000}}"#,
            "\n",
            r#"{"type":"session.compaction_start","timestamp":"2026-08-01T11:00:00.000Z","data":{"currentTokens":180000}}"#,
            "\n",
            r#"{"type":"session.compaction_complete","timestamp":"2026-08-01T11:01:00.000Z","data":{"success":true,"preCompactionTokens":180000}}"#,
            "\n",
            r#"{"type":"session.compaction_start","timestamp":"2026-08-01T12:00:00.000Z","data":{"currentTokens":150000}}"#,
            "\n",
            r#"{"type":"session.compaction_complete","timestamp":"2026-08-01T12:01:00.000Z","data":{"success":true}}"#,
        );
        let fold = fold_transcript(Source::Copilot, 2, transcript.as_bytes()).unwrap();
        let compactions = fold.compactions;
        assert!(compactions.observable);
        assert_eq!(compactions.started, 3, "the announcements are context");
        assert_eq!(
            compactions.completed, 3,
            "three compactions, not six markers"
        );
        assert_eq!(compactions.failed, 0);
        // Summed over the counted phase only: the starts state `currentTokens` for the same
        // quantity, and adding both halves would double every Copilot figure.
        assert_eq!(compactions.pre_tokens_stated, 2);
        assert_eq!(compactions.pre_tokens_total, 280_000);
        assert_eq!(compactions.pre_tokens_max, Some(180_000));
        assert_eq!(compactions.mean_pre_tokens(), Some(140_000));
    }

    /// `succeeded != Some(false)`, held from both sides. A Copilot completion that failed is
    /// excluded; a Claude Code boundary, which states no outcome at all, is not.
    #[test]
    fn a_failed_completion_is_excluded_and_a_silent_one_is_not() {
        let copilot = concat!(
            r#"{"type":"session.compaction_complete","timestamp":"2026-08-01T10:01:00.000Z","data":{"success":true,"preCompactionTokens":100000}}"#,
            "\n",
            r#"{"type":"session.compaction_complete","timestamp":"2026-08-01T11:01:00.000Z","data":{"success":false,"error":"summarizer timed out"}}"#,
        );
        let fold = fold_transcript(Source::Copilot, 2, copilot.as_bytes()).unwrap();
        assert_eq!(fold.compactions.completed, 1);
        assert_eq!(fold.compactions.failed, 1);

        // The same filter over an envelope that never states an outcome. Reading this as
        // `== Some(true)` would score the whole harness at zero compactions.
        let claude = concat!(
            r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-08-01T10:00:00.000Z","compactMetadata":{"trigger":"manual","preTokens":214864,"postTokens":8156}}"#,
            "\n",
            r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-08-01T11:00:00.000Z","compactMetadata":{"trigger":"auto","preTokens":339462,"postTokens":10705}}"#,
        );
        let fold = fold_claude(claude);
        assert_eq!(fold.compactions.completed, 2);
        assert_eq!(fold.compactions.failed, 0);
        assert_eq!(fold.compactions.started, 0);
        assert_eq!(fold.compactions.pre_tokens_max, Some(339_462));
    }

    /// Observability is decided by the interpreter before a record is read, so a harness that
    /// types markers reports a real zero and one that does not reports nothing at all. Both
    /// transcripts below are the same healthy session with no compaction in it.
    #[test]
    fn a_harness_that_types_markers_reports_zero_and_one_that_does_not_reports_nothing() {
        let quiet = claude_tool_transcript("Bash", 2, 0);
        assert_eq!(fold_claude(&quiet).compactions.count(), Some(0));
        assert_eq!(
            fold_transcript(Source::Copilot, 2, quiet.as_bytes())
                .unwrap()
                .compactions
                .count(),
            Some(0),
        );
        assert_eq!(
            fold_codex(&codex_shell_transcript(&["ls"]))
                .compactions
                .count(),
            None,
            "munshi-transcript certifies no codex compaction shape, so nothing looked",
        );
    }

    /// A marker record is `Ignored` bookkeeping and stays that way: reading the compaction off it
    /// must add no event, no tool activity, and no user request to the counting fold beside it.
    #[test]
    fn a_compaction_marker_stays_bookkeeping_in_every_other_metric() {
        let transcript = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"user","content":"go"}}"#,
            "\n",
            r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-08-01T10:05:00.000Z","compactMetadata":{"preTokens":100000}}"#,
        );
        let fold = fold_claude(transcript);
        assert_eq!(fold.compactions.completed, 1);
        assert_eq!(fold.summary.user_requests, 1);
        assert_eq!(fold.summary.tool_activities, 0);
        // It is still a dated record, so the gap fold sees it — somebody was at the keyboard.
        assert_eq!(fold.activity.active_time(), Some(TimeDelta::minutes(5)));
    }

    #[test]
    fn tools_per_request_is_undefined_without_a_user_request() {
        let one_request = metrics(fold_claude(&claude_tool_transcript("Bash", 3, 0)));
        // One user request, three invocations plus three results.
        assert_eq!(one_request.tools_per_request(), Some(6.0));

        let toolless = metrics(fold_claude(
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
        ));
        assert_eq!(toolless.tools_per_request(), None);
    }

    #[test]
    fn span_comes_from_the_first_and_last_record_timestamps() {
        let transcript = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T09:00:00.000Z","message":{"role":"user","content":"start"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T14:30:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"end"}]}}"#,
        );
        let session = metrics(fold_claude(transcript));
        assert_eq!(session.span(), Some(TimeDelta::minutes(330)));
        assert_eq!(
            session.day(),
            Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap())
        );
    }

    #[test]
    fn cadence_groups_sessions_by_their_starting_day() {
        let day = |date: &str, span_minutes: i64, gaps: &[i64]| SessionMetrics {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            summary: SessionSummary {
                first_timestamp: Some(at(date)),
                last_timestamp: Some(at(date) + TimeDelta::minutes(span_minutes)),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes::default(),
            activity: activity(gaps),
            commands: CommandChurn::default(),
            compactions: Compactions::default(),
            anchors: SessionAnchors::default(),
            bytes_folded: 0,
        };
        let sessions = vec![
            // 30 minutes of span, worked straight through.
            day("2026-08-01T09:00:00Z", 30, &[10, 10, 10]),
            // 90 minutes of span, but two sittings of 10 minutes inside it.
            day("2026-08-01T13:00:00Z", 90, &[10, 60, 10]),
            day("2026-08-03T09:00:00Z", 60, &[5, 5, 5]),
        ];
        let cadence = Cadence::fold(&sessions);
        assert_eq!(cadence.active_days(), 2);
        assert_eq!(cadence.sessions_per_active_day(), Some(1.5));
        assert_eq!(cadence.median_span(), Some(TimeDelta::minutes(60)));
        assert_eq!(cadence.longest_span(), Some(TimeDelta::minutes(90)));
        // The duration distribution the report leads with is the active one, and it ranks the
        // sessions differently from their spans: the 90-minute span is the *least* active of the
        // three.
        assert_eq!(cadence.median_active(), Some(TimeDelta::minutes(20)));
        assert_eq!(cadence.longest_active(), Some(TimeDelta::minutes(30)));
        assert_eq!(cadence.total_active, TimeDelta::minutes(65));
        assert_eq!(cadence.total_span, TimeDelta::minutes(180));
        assert_eq!(cadence.total_sittings, 4);
        assert_eq!(cadence.undated, 0);
    }

    /// The gap fold's arithmetic, over a session that works, walks away, comes back, and works
    /// again.
    #[test]
    fn active_time_counts_only_the_gaps_under_the_idle_threshold() {
        // 10m + 5m of work, a 3h break, then 12m + 10m of work. The break is idle time and is
        // counted nowhere; it is not shortened, capped, or averaged in.
        let session = activity(&[10, 5, 180, 12, 10]);
        assert_eq!(session.active_time(), Some(TimeDelta::minutes(37)));
        assert_eq!(session.longest_sitting(), Some(TimeDelta::minutes(22)));
        assert_eq!(session.sittings(), Some(2));
    }

    /// `≤ IDLE_GAP` keeps a sitting whole and `> IDLE_GAP` breaks it — pinned to the second,
    /// because every sitting in the archive is decided by this comparison.
    #[test]
    fn a_gap_of_exactly_the_idle_threshold_stays_inside_the_sitting() {
        let start = at("2026-08-01T09:00:00Z");
        let on_the_line = Activity::over([start, start + IDLE_GAP]);
        assert_eq!(on_the_line.sittings(), Some(1));
        assert_eq!(on_the_line.active_time(), Some(IDLE_GAP));
        assert_eq!(on_the_line.longest_sitting(), Some(IDLE_GAP));

        let over_the_line = Activity::over([start, start + IDLE_GAP + TimeDelta::seconds(1)]);
        assert_eq!(over_the_line.sittings(), Some(2));
        assert_eq!(over_the_line.active_time(), Some(TimeDelta::zero()));
        assert_eq!(over_the_line.longest_sitting(), Some(TimeDelta::zero()));
    }

    /// Out-of-order records are real (30% of archived sessions carry at least one), and the fold
    /// clamps rather than sorts: sorting would mean buffering every timestamp in a transcript
    /// that can reach hundreds of megabytes, to move two sessions across the marathon line in the
    /// whole archive. A negative gap must never subtract from active time.
    #[test]
    fn an_inverted_pair_of_records_clamps_to_a_zero_gap() {
        let start = at("2026-08-01T09:00:00Z");
        let inverted = Activity::over([
            start,
            start + TimeDelta::minutes(10),
            // The tool result the harness wrote a moment *before* the message that provoked it.
            start + TimeDelta::minutes(9),
            start + TimeDelta::minutes(12),
        ]);
        assert_eq!(inverted.active_time(), Some(TimeDelta::minutes(13)));
        assert_eq!(inverted.sittings(), Some(1));
        // And the cost of clamping rather than sorting, pinned rather than left to be discovered:
        // the fold re-traverses the minute it walked back over, so the summed gaps come out one
        // minute *longer* than the 12m between the earliest and latest record.
        assert!(inverted.active_time().unwrap() > TimeDelta::minutes(12));
    }

    /// Sub-second work is real — a burst of records and then nothing — and the span-to-active
    /// ratio has to stay finite for it, or a rule reads "no work at all" off a session that had
    /// some.
    #[test]
    fn a_sub_second_active_time_still_divides_into_a_finite_ratio() {
        let start = at("2026-08-01T09:00:00Z");
        let session = SessionMetrics {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            artifact_set_version: 2,
            summary: SessionSummary {
                first_timestamp: Some(start),
                last_timestamp: Some(start + TimeDelta::hours(9)),
                ..SessionSummary::default()
            },
            tools: ToolOutcomes::default(),
            activity: Activity::over([start, start + TimeDelta::milliseconds(900)]),
            commands: CommandChurn::default(),
            compactions: Compactions::default(),
            anchors: SessionAnchors::default(),
            bytes_folded: 0,
        };
        assert_eq!(
            session.active_time(),
            Some(TimeDelta::milliseconds(900)),
            "the fold itself keeps sub-second gaps"
        );
        let ratio = session.span_to_active().expect("both are measurable");
        assert!(ratio.is_finite(), "{ratio}");
        assert!((ratio - 36_000.0).abs() < 1.0, "{ratio}");
    }

    /// One record has no adjacency, so it has no gap: activity is undefined rather than zero,
    /// exactly as `span()` is.
    #[test]
    fn a_session_with_fewer_than_two_dated_records_has_no_activity() {
        let empty = Activity::default();
        assert_eq!(empty.active_time(), None);
        assert_eq!(empty.longest_sitting(), None);
        assert_eq!(empty.sittings(), None);

        let lone = Activity::over([at("2026-08-01T09:00:00Z")]);
        assert_eq!(lone.active_time(), None);
        assert_eq!(lone.sittings(), None);
    }

    /// The fold reads timestamps off the stream in file order, so a real transcript's activity
    /// comes out of `fold_transcript` rather than out of a hand-built `Activity`.
    #[test]
    fn folding_a_transcript_derives_its_activity_from_the_record_timestamps() {
        let transcript = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-01T09:00:00.000Z","message":{"role":"user","content":"start"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-01T09:05:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}"#,
            "\n",
            // Four hours away from the keyboard.
            r#"{"type":"user","uuid":"u2","timestamp":"2026-08-01T13:05:00.000Z","message":{"role":"user","content":"back"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a2","timestamp":"2026-08-01T13:07:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
        );
        let session = metrics(fold_claude(transcript));
        assert_eq!(session.span(), Some(TimeDelta::minutes(247)));
        assert_eq!(session.active_time(), Some(TimeDelta::minutes(7)));
        assert_eq!(session.longest_sitting(), Some(TimeDelta::minutes(5)));
        assert_eq!(session.sittings(), Some(2));
        assert_eq!(session.span_to_active(), Some(247.0 / 7.0));
    }

    #[test]
    fn totals_sum_per_tool_tallies_across_sessions() {
        let sessions = vec![
            metrics(fold_claude(&claude_tool_transcript("Bash", 2, 2))),
            metrics(fold_claude(&claude_tool_transcript("Bash", 2, 0))),
        ];
        let totals = Totals::fold(&sessions);
        assert_eq!(totals.sessions, 2);
        assert_eq!(
            totals.by_tool["Bash"],
            ToolTally {
                attempts: 4,
                errors: 2
            }
        );
        assert_eq!(totals.by_agent["claude-code"], 2);
    }

    /// The window aggregate counts only the sessions that had a reading, so a mirror that is
    /// mostly harnesses without a command field cannot dilute the churn of the ones that have it.
    #[test]
    fn churn_totals_count_only_sessions_that_recorded_a_command() {
        let sessions = vec![
            metrics(fold_codex(&codex_shell_transcript(&[
                "make", "make", "make", "ls",
            ]))),
            metrics(fold_codex(&codex_shell_transcript(&["ls", "pwd"]))),
            // No command field anywhere: absent from both the numerator and the denominator.
            metrics(fold_claude(&claude_tool_transcript("Bash", 8, 0))),
        ];
        let churn = Totals::fold(&sessions).churn;
        assert_eq!(churn.sessions_with_commands, 2);
        assert_eq!(churn.sessions_with_repeats, 1);
        assert_eq!(churn.command_events, 6);
        assert_eq!(churn.repeats, 2);
        assert_eq!(churn.busiest_command_runs, 3);
        assert_eq!(churn.untracked_events, 0);
        assert_eq!(churn.repeat_share(), Some(2.0 / 6.0));

        // And a window with no command anywhere has no share at all, rather than zero.
        let blind = Totals::fold(&sessions[2..]).churn;
        assert_eq!(blind, ChurnTotals::default());
        assert_eq!(blind.repeat_share(), None);
    }

    /// Anchors are bounded and counts are not, which is the whole shape of the trade: a session
    /// that failed forty calls still *reports* forty, and offers ten places to look.
    #[test]
    fn the_anchor_cap_bounds_the_evidence_and_never_the_count() {
        let fold = fold_claude(&claude_tool_transcript("Bash", 40, 40));
        assert_eq!(fold.tools.total.attempts, 40);
        assert_eq!(fold.tools.total.errors, 40, "every call is still counted");
        assert_eq!(
            fold.anchors.tool_errors["Bash"].len(),
            MAX_ANCHORS_PER_FINDING,
        );
        // The ones kept are the first, in file order — a session's evidence should start where the
        // trouble started, not wherever the fold happened to stop.
        let locators: Vec<u64> = fold.anchors.tool_errors["Bash"]
            .iter()
            .map(|anchor| anchor.locator)
            .collect();
        assert_eq!(
            locators,
            (1..=MAX_ANCHORS_PER_FINDING)
                .map(|n| n as u64 * 2)
                .collect::<Vec<_>>()
        );
    }

    /// The locator is the tool event's ordinal in file order, and it has to mean the same thing to
    /// the fold that mints it and to the reader that resolves it — so this pins it against the two
    /// events of each call rather than against a record number.
    #[test]
    fn an_anchor_locator_is_the_tool_events_ordinal_in_file_order() {
        let fold = fold_claude(&claude_tool_transcript("Bash", 3, 3));
        let locators: Vec<u64> = fold.anchors.tool_errors["Bash"]
            .iter()
            .map(|anchor| anchor.locator)
            .collect();
        // Invocation, result, invocation, result, ...: the failures are the even ordinals.
        assert_eq!(locators, vec![2, 4, 6]);
        // And the records they name are the result records, which are every second record after
        // the opening user message.
        let records: Vec<u64> = fold.anchors.tool_errors["Bash"]
            .iter()
            .map(|anchor| anchor.record)
            .collect();
        assert_eq!(records, vec![3, 5, 7]);
    }

    /// A failure the fold could not attribute is anchored without a name rather than filed under a
    /// tool that did not run it — the same discipline `unattributed` already holds for the count.
    #[test]
    fn an_unattributable_failure_is_anchored_under_no_tool() {
        let transcript = r#"{"type":"user","uuid":"r1","timestamp":"2026-08-01T10:00:00.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_orphan","content":"result","is_error":true}]}}"#;
        let fold = fold_claude(transcript);
        assert_eq!(fold.tools.unattributed, 1);
        assert!(fold.anchors.tool_errors.is_empty());
        assert_eq!(fold.anchors.unattributed_errors.len(), 1);
        assert_eq!(fold.anchors.unattributed_errors[0].tool, None);
    }

    /// Sitting boundaries are the same machinery the durations come from, read out rather than
    /// re-derived: three sittings of work with two long breaks between them.
    #[test]
    fn sitting_boundaries_are_the_sittings_the_gap_fold_counted() {
        let session = activity(&[10, 5, 180, 12, 10, 240, 10, 5]);
        assert_eq!(session.sittings(), Some(3));
        let boundaries = session.sitting_boundaries();
        assert_eq!(boundaries.len(), 3);
        assert_eq!(boundaries[0].span(), TimeDelta::minutes(15));
        assert_eq!(boundaries[1].span(), TimeDelta::minutes(22));
        assert_eq!(boundaries[2].span(), TimeDelta::minutes(15));
        assert_eq!(
            boundaries.iter().map(Sitting::span).sum::<TimeDelta>(),
            session.active_time().unwrap(),
            "the drawn sittings account for exactly the active time",
        );
        // Adjacent and non-overlapping, in file order.
        assert!(boundaries[0].to < boundaries[1].from);
        assert!(boundaries[1].to < boundaries[2].from);
        assert_eq!(session.sittings_elided(), 0);
    }

    /// Past the cap the boundaries stop and the *counts* do not: a fifty-sitting transcript draws
    /// twelve and says how many it did not draw.
    #[test]
    fn the_boundary_cap_elides_rather_than_misrepresenting_the_shape() {
        // Each sitting is one 10-minute gap, separated by an hour away from the keyboard.
        let mut gaps = Vec::new();
        for sitting in 0..MAX_STRUCTURAL_SITTINGS + 8 {
            if sitting > 0 {
                gaps.push(60);
            }
            gaps.push(10);
        }
        let session = activity(&gaps);
        assert_eq!(session.sittings(), Some(MAX_STRUCTURAL_SITTINGS + 8));
        assert_eq!(session.sitting_boundaries().len(), MAX_STRUCTURAL_SITTINGS);
        assert_eq!(session.sittings_elided(), 8);
        assert_eq!(
            session.active_time(),
            Some(TimeDelta::minutes(
                10 * (MAX_STRUCTURAL_SITTINGS as i64 + 8)
            )),
            "the duration is over every sitting, drawn or not",
        );
    }

    /// A session with no adjacency has no sitting to draw, exactly as it has no duration to state.
    #[test]
    fn a_session_with_one_record_draws_no_sitting() {
        assert!(Activity::default().sitting_boundaries().is_empty());
        assert!(
            Activity::over([at("2026-08-01T09:00:00Z")])
                .sitting_boundaries()
                .is_empty()
        );
    }

    #[test]
    fn agent_labels_map_onto_interpreters() {
        assert_eq!(source_for_agent("claude-code"), Some(Source::ClaudeCode));
        assert_eq!(source_for_agent("copilot-cli"), Some(Source::Copilot));
        assert_eq!(source_for_agent("codex-cli"), Some(Source::Codex));
        assert_eq!(source_for_agent("some-future-harness"), None);
    }

    #[test]
    fn an_unsupported_artifact_set_version_refuses_the_fold() {
        assert!(fold_transcript(Source::ClaudeCode, 99, &b""[..]).is_err());
    }
}
