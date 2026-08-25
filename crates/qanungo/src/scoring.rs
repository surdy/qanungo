//! Practice-lane scores, and the rule-pack stamp that says when two of them may be compared.
//!
//! The five lanes are the ones qanungo #4 names — Prompt Quality, Session Hygiene, Code Review,
//! Tool Mastery, Context Management — and as of munshi#77 pull B **all five** are fed by something
//! the fold types. **A lane with no feeding signal is not scored.** It renders as unscored, with
//! the signal it is waiting for named. It never gets a default, a proxy, or a zero: the same
//! no-signal-no-claim discipline [`CommandChurn`](crate::metrics::CommandChurn) applies to one
//! session's churn, applied to a whole lane. No lane is in that state today; the rule stands for
//! the next one that is.
//!
//! # The mapping, and why each half of it is defensible
//!
//! | Lane | Fed by | Because |
//! | --- | --- | --- |
//! | Tool Mastery | pooled tool error rate; retry-loop fire rate | both are readings of *how well the tools are being driven* — calls that failed, and commands re-run until they stopped disagreeing |
//! | Session Hygiene | marathon fire rate; heavily-resumed fire rate | both are readings of *how the work is packaged into sessions* — one unbroken push, and one transcript standing in for many work items |
//! | Prompt Quality | babysitting fire rate; fire-and-forget fire rate | both are readings of *how the ask was shaped* — a hundred small steering turns, or one enormous unattended run with no checkpoint in it |
//! | Code Review | unreviewed-ship fire rate | of the sessions that *shipped*, the share that shipped with nothing having reviewed the work — read off the invocations themselves, not off a proxy for care |
//! | Context Management | compaction-churn fire rate | a session that compacted its window over and over is *managing context badly*, read off the window itself rather than off anything standing in for it |
//!
//! Marathon *sounds* like it belongs to Context Management — a context window accumulating without
//! a break — and heavily-resumed *sounds* like it too. Both were declined there on purpose while
//! that lane was dark, and both stay declined now that it is lit: what those two measure is sitting
//! length and calendar dilution, not context. Scoring Context Management off them would be scoring
//! an implication, and one signal counted into two lanes is one behaviour reported as two findings.
//! The lane waited for munshi#77 to type the compaction markers themselves, and that is what it is
//! scored from.
//!
//! **Code Review was the last dark lane, and it was lit the same way rather than by a proxy.** The
//! row it used to carry named the proxies that were available all along and declined — files edited
//! versus read, revert-and-retry cycles — and every one of them measures the *shape* of editing
//! rather than whether anything reviewed the work. munshi#77 pull B typed the invocations
//! themselves, so the lane reads the act it is named for: a review pass, invoked, in a session that
//! shipped.
//!
//! # One lane that is scored for one harness only
//!
//! Code Review scores **claude-code alone today, and that is an observability statement rather
//! than a judgement about Copilot.** Claude Code types both surfaces a review could be invoked on;
//! Copilot types its skills but records slash commands as unmarked prose, so "Copilot ran no
//! review" is a sentence the fold cannot say and Copilot leaves the rate entirely — its Code Review
//! renders [`LaneScore::NoReading`], never a zero. Reading it as a zero would have been the single
//! most flattering-to-nobody mistake available here: it would have scored a harness 0 for a habit
//! nothing observed. The lane picks Copilot up the day that surface is typed. See
//! [`review_observable`](crate::metrics::ReviewActivity) for the per-harness reasoning.
//!
//! **Read Code Review's component line, not its score.** It is the first lane in the pack whose
//! reading sits *above* [`constants::FIRE_RATE_FLOOR`] rather than far below it — 92% against a
//! 25% floor — so its penalty is saturated and the lane reads 0 anywhere in the top three quarters
//! of the range. The 0 is the clamp speaking, not the measurement, and it will not move until the
//! unreviewed rate falls below a quarter. The raw rate rides in the component line beside it and is
//! the number that tracks the habit. The floor is deliberately not special-cased for this lane;
//! see [`constants::FIRE_RATE_FLOOR`] for why that is a pack-wide decision rather than this one.
//!
//! # A lane with one component
//!
//! Context Management is fed by a single reading today, and the formula below needs no special case
//! for that — the mean over the components that read is over a set of one, so the component spends
//! the whole lane. It is a real limitation rather than a design: what a session's *utilization* was
//! before it compacted is the lane's obvious second component, and it is not here because no
//! denominator exists to divide a pre-compaction total by (the interpreter's `token_limit` is
//! absent on every Claude Code boundary and all but five Copilot pairs). The totals ride along in
//! the finding as context and are scored by nothing. The second component lands when a window size
//! does — the compaction/token wishlist row on qanungo #4 is where it waits.
//!
//! Cadence is fed into no lane either, for a different reason: sessions per active day has no
//! defensible *direction*. Four sessions a day is not better or worse than one. It stays in the
//! report's Cadence section as context, where a reader can draw their own conclusion, rather than
//! being turned into points nobody can justify.
//!
//! # The formula
//!
//! Every lane is a small set of **components**, each of which reads one rate off the window and
//! spends some fraction of its share of the lane's 100 points:
//!
//! ```text
//! penalty_i = clamp(reading_i / floor_i, 0, 1)          // 0 = nothing to penalize, 1 = all of it
//! score     = round(100 × (1 − mean(penalty_i)))        // mean over the components that read
//! ```
//!
//! Three properties are deliberate:
//!
//! - **Every component in a lane weighs the same.** Nothing observed yet says one of them
//!   deserves more, and a weight vector is exactly the sort of unmeasured knob that makes a score
//!   impossible to explain. When a measurement says otherwise, weights land then.
//! - **The mean runs over the components that *read*.** A component whose signal is absent from
//!   the window has no say — it is neither a zero penalty nor a full one. A lane where nothing
//!   read is [`LaneScore::NoReading`], not a hundred.
//! - **`clean` is zero for every component today**, so the ramp is a plain division. Every
//!   reading here is a rate whose good value is none-of-it. A reading with a non-zero clean point
//!   gets its second anchor when one exists, not before.
//!
//! # What a score does and does not mean
//!
//! **100 means nothing this pack penalizes was observed** — not that the practice is perfect. The
//! pack sees what the fold types, and that is five metrics.
//!
//! **Scores are comparable across windows under the same rule-pack stamp, and never across
//! lanes.** Each lane's constants are anchored on its own rules' thresholds; there is no shared
//! difficulty scale, so "Session Hygiene 71, Prompt Quality 100" says nothing about which is
//! going better. It says Session Hygiene has more of what its own rules penalize than it did
//! last window.
//!
//! # Harness-relative, and the blend
//!
//! Scores are computed **per `source_agent`** ([`Scorecard::fold`]) because harnesses differ in
//! what they can even express — Codex reports no tool outcome at all, so its sessions have no
//! error rate — and a fleet number that mixed them would move when the harness mix moved, with no
//! behaviour change behind it. The one blended number, [`Scorecard::fleet`], states its rule: the
//! **unweighted mean of the per-harness scores**, every harness counting once. It is stable under
//! a mix shift and unstable under a *roster* shift, which is why a fleet trend arrow is rendered
//! only when the same harnesses scored the lane on both sides.
//!
//! Sampling bias rides along with any blend: Codex is manual-archive-only in munshi, so it is
//! under-represented in the archive relative to how much it is used. The report says so wherever
//! a blended number appears.
//!
//! # The rule-pack stamp
//!
//! Scores are never frozen per session; every run recomputes all of history with the *current*
//! pack (qanungo ADR 0001). That is what makes a trend arrow trustworthy — it can only be
//! behaviour drift, because the rules on both sides are the same ones. [`RulePack`] is what makes
//! that inspectable: a digest over every rule id, every threshold, every scoring constant, the
//! lane→signal mapping, and the formula's own version tag. **Two reports are comparable iff their
//! stamps match.** It is in the instrumentation footer of every run.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::format;
use crate::metrics::{SessionMetrics, ToolTally};
use crate::rules::{RuleId, thresholds};

/// Tunable scoring constants. **Arbitrary until measured**, in exactly the sense
/// [`crate::rules::thresholds`] means it — first guesses at where a rate stops being ordinary and
/// starts costing points, written down so a later pass can replace them with observed
/// distributions.
pub mod constants {
    /// Version tag of the formula itself, mixed into the rule-pack stamp.
    ///
    /// The stamp is a hash over the numbers, so a change that alters *how* they combine — a
    /// weight vector, a non-linear ramp, a different aggregation — would otherwise leave it
    /// untouched and let two incomparable reports claim to be comparable. Bump this whenever
    /// [`super::LaneScore`]'s arithmetic changes.
    pub const FORMULA: &str = "equal-weight-mean-of-linear-penalties/1";

    /// The score of a window in which nothing this pack penalizes was observed.
    pub const CLEAN_SCORE: f64 = 100.0;

    /// Rule fire rate at which a fire-rate component spends its whole share of a lane.
    ///
    /// One eligible session in four tripping a rule is the point at which the habit is the
    /// working pattern rather than an exception. Deliberately one constant for every fire-rate
    /// component rather than five: the rules' own base rates already differ by a lot (on the
    /// 2026-08-18 archive over 60 days: babysitting 0%, fire-and-forget under 1%, marathon 4.6%,
    /// retry loop 4.9% of command-bearing sessions, heavily-resumed 9.1%), and five separate
    /// floors chosen to normalize those apart would be five unmeasured knobs pretending to be a
    /// difficulty scale. The lanes are not comparable to each other anyway — see the module docs
    /// — so the honest move is one floor, stated once.
    ///
    /// **What one floor costs: the penalty saturates, and above the floor the score stops
    /// carrying information.** A component clamps at 1.0, so every fire rate from 25% to 100%
    /// spends the same full share and reads the same. That was invisible while every rule in the
    /// pack fired between 0% and 10% — a whole order of magnitude below the floor — and the Code
    /// Review lane is the first to sit *above* it, at 92%: its lane score is 0 and would still be
    /// 0 at 30%, so the number cannot show improvement until the habit passes a threefold change.
    /// The report is built for this — **every component renders its raw reading beside its cost**
    /// ("fired on 174 of 189 … (92%)"), and that line is what moves — but a reader watching only
    /// the lane number would see a flat 0 through real progress. Raising the floor is not the fix
    /// and is not done here: it is a pack-wide constant on a shared scale, so moving it re-scores
    /// every lane and is a decision of its own, with its own measurement, not a side effect of
    /// waking one lane.
    pub const FIRE_RATE_FLOOR: f64 = 0.25;

    /// Pooled tool failure rate at which the tool-error component spends its whole share.
    ///
    /// Anchored on [`super::thresholds::SESSION_TOOL_ERROR_RATE`] rather than chosen freely: that
    /// is already the documented point at which *one session's* failure rate is worth calling
    /// out, so a whole window sitting there has spent the component. The archive's pooled rate
    /// over 60 days is 1.9% (2,385 of 125,945 calls that reported an outcome), so a healthy
    /// window costs this component about a tenth of its share.
    pub const TOOL_ERROR_RATE_FLOOR: f64 = super::thresholds::SESSION_TOOL_ERROR_RATE;

    /// Eligible sessions a fire-rate component needs before its rate counts as a reading.
    ///
    /// A fire rate over two sessions is 0%, 50%, or 100%, and none of those is information. Below
    /// this the component reports *no reading* rather than a jumpy one — which is the same
    /// discipline as a rate with no denominator, applied to a denominator too small to divide by.
    pub const MIN_SCORED_SESSIONS: usize = 5;

    /// Tool calls that reported an outcome before the pooled error rate counts as a reading.
    /// Anchored on the per-session minimum the error-rate rule already uses.
    pub const MIN_SCORED_TOOL_ATTEMPTS: u64 = super::thresholds::MIN_SESSION_TOOL_ATTEMPTS;
}

/// How many hex characters of the rule-pack digest the report prints. Enough that two packs
/// colliding is not a practical concern for a stamp whose only job is equality, short enough to
/// sit in a footer.
const STAMP_CHARS: usize = 12;

/// One practice lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    PromptQuality,
    SessionHygiene,
    CodeReview,
    ToolMastery,
    ContextManagement,
}

impl Lane {
    /// Every lane, in the order qanungo #4 names them — which is also the order the report's
    /// table renders, so an unscored lane keeps its place instead of disappearing.
    pub const ALL: [Self; 5] = [
        Self::PromptQuality,
        Self::SessionHygiene,
        Self::CodeReview,
        Self::ToolMastery,
        Self::ContextManagement,
    ];

    /// The lane's heading in the report.
    pub const fn title(self) -> &'static str {
        match self {
            Self::PromptQuality => "Prompt Quality",
            Self::SessionHygiene => "Session Hygiene",
            Self::CodeReview => "Code Review",
            Self::ToolMastery => "Tool Mastery",
            Self::ContextManagement => "Context Management",
        }
    }

    /// A stable machine name, for the rule-pack stamp.
    pub const fn key(self) -> &'static str {
        match self {
            Self::PromptQuality => "prompt-quality",
            Self::SessionHygiene => "session-hygiene",
            Self::CodeReview => "code-review",
            Self::ToolMastery => "tool-mastery",
            Self::ContextManagement => "context-management",
        }
    }

    /// What this lane is scored from — empty for a lane nothing types a signal for yet.
    const fn signals(self) -> &'static [Signal] {
        match self {
            Self::PromptQuality => &[
                Signal::FireRate(RuleId::Babysitting),
                Signal::FireRate(RuleId::FireAndForget),
            ],
            Self::SessionHygiene => &[
                Signal::FireRate(RuleId::MarathonSession),
                Signal::FireRate(RuleId::ResumedSession),
            ],
            Self::ToolMastery => &[
                Signal::PooledToolErrorRate,
                Signal::FireRate(RuleId::RetryLoop),
            ],
            Self::ContextManagement => &[Signal::FireRate(RuleId::CompactionChurn)],
            Self::CodeReview => &[Signal::FireRate(RuleId::UnreviewedShip)],
        }
    }

    /// Why this lane is not scored, for a lane nothing feeds. `None` for a lane the pack scores.
    ///
    /// Stated as the signal that is missing rather than as an apology: the sentence a reader
    /// needs is which pull would light the lane up, not that it is dark.
    ///
    /// **Every lane returns `None` today.** Code Review was the last holdout and munshi#77 pull B
    /// lit it, so nothing in this pack is dark any more. The mechanism stays because a sixth lane
    /// may arrive before the signal that feeds it, and because deleting it would mean the next
    /// dark lane silently rendering as a zero — which is the outcome the whole no-signal-no-claim
    /// discipline exists to prevent. Note that a lane can still be unscored *for one harness*
    /// without being untyped: that is [`LaneScore::NoReading`], and it is what Copilot's Code
    /// Review renders as.
    /// Left exhaustive rather than collapsed to a bare `None` so that a lane added later cannot
    /// inherit "typed" by omission — it has to say so here.
    pub const fn untyped(self) -> Option<&'static str> {
        match self {
            Self::PromptQuality
            | Self::SessionHygiene
            | Self::CodeReview
            | Self::ToolMastery
            | Self::ContextManagement => None,
        }
    }
}

/// One reading a lane's score is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// The window's pooled tool failure rate for this harness: every call that reported an
    /// outcome, in one fraction. Preferred over the error-rate *rule's* fire rate, which would be
    /// a second and coarser reading of the same signal.
    PooledToolErrorRate,
    /// The share of the sessions a rule could have fired on that it did fire on. The denominator
    /// is the rule's own eligibility — sessions carrying the signal it reads — never the window's
    /// whole session count, so a harness that cannot express a signal dilutes nothing.
    FireRate(RuleId),
}

impl Signal {
    /// A stable machine name, for the rule-pack stamp.
    fn key(self) -> String {
        match self {
            Self::PooledToolErrorRate => "pooled-tool-error-rate".to_owned(),
            Self::FireRate(rule) => format!("fire-rate:{}", rule.key()),
        }
    }

    /// The component's label in the report.
    fn label(self) -> &'static str {
        match self {
            Self::PooledToolErrorRate => "Tool error rate",
            Self::FireRate(rule) => rule.title(),
        }
    }

    /// What the fire-rate denominator is, said in words, so a reader can see which sessions the
    /// rate is *of* without going to the source.
    fn denominator(self) -> String {
        match self {
            Self::PooledToolErrorRate => "calls that reported an outcome".to_owned(),
            Self::FireRate(RuleId::HighToolErrorRate) => {
                "sessions that reported enough tool outcomes".to_owned()
            }
            Self::FireRate(RuleId::RetryLoop) => "sessions that recorded a command".to_owned(),
            Self::FireRate(RuleId::MarathonSession) => {
                "sessions with a measurable sitting".to_owned()
            }
            Self::FireRate(RuleId::ResumedSession) => {
                "sessions with a measurable span and active time".to_owned()
            }
            Self::FireRate(RuleId::Babysitting) => format!(
                "sessions carrying {}+ user requests",
                thresholds::BABYSITTING_MIN_USER_REQUESTS
            ),
            Self::FireRate(RuleId::FireAndForget) => "single-request sessions".to_owned(),
            // Not "sessions that compacted": the denominator is every session whose harness would
            // have *recorded* a compaction, so a session that never filled its window counts as a
            // clean one rather than being left out of the rate. See
            // [`RuleId::verdict`](crate::rules::RuleId::verdict).
            Self::FireRate(RuleId::CompactionChurn) => {
                "sessions whose harness records compactions".to_owned()
            }
            // Both halves of the eligibility are in the phrase on purpose, because this is the
            // sentence a reader meets when Copilot's Code Review renders no reading. "only 0
            // sessions that shipped on a harness whose review surfaces are all typed" says the
            // harness could not be looked at; "only 0 sessions that shipped" would have implied
            // Copilot never ships, which is false — it committed in 121 sessions of the mirror.
            // See [`RuleId::verdict`](crate::rules::RuleId::verdict).
            Self::FireRate(RuleId::UnreviewedShip) => {
                "sessions that shipped on a harness whose review surfaces are all typed".to_owned()
            }
        }
    }

    /// Reads this signal off one harness's sessions.
    fn read(self, sessions: &[&SessionMetrics]) -> Reading {
        match self {
            Self::PooledToolErrorRate => {
                let mut pooled = ToolTally::default();
                for session in sessions {
                    pooled.attempts += session.tools.total.attempts;
                    pooled.errors += session.tools.total.errors;
                }
                let Some(rate) = pooled.error_rate() else {
                    return Reading::absent(
                        "no call in this window reported an outcome".to_owned(),
                    );
                };
                if pooled.attempts < constants::MIN_SCORED_TOOL_ATTEMPTS {
                    return Reading::absent(format!(
                        "only {} of the {} calls needed reported an outcome",
                        pooled.attempts,
                        constants::MIN_SCORED_TOOL_ATTEMPTS,
                    ));
                }
                Reading::present(
                    rate,
                    constants::TOOL_ERROR_RATE_FLOOR,
                    format!(
                        "{} of {} {} failed ({})",
                        pooled.errors,
                        pooled.attempts,
                        self.denominator(),
                        format::percent(rate),
                    ),
                )
            }
            Self::FireRate(rule) => {
                let verdicts = sessions.iter().filter_map(|session| rule.verdict(session));
                let (mut eligible, mut fired) = (0_usize, 0_usize);
                for verdict in verdicts {
                    eligible += 1;
                    fired += usize::from(verdict);
                }
                if eligible < constants::MIN_SCORED_SESSIONS {
                    return Reading::absent(format!(
                        "only {eligible} {}, fewer than the {} a fire rate needs",
                        self.denominator(),
                        constants::MIN_SCORED_SESSIONS,
                    ));
                }
                let rate = fired as f64 / eligible as f64;
                Reading::present(
                    rate,
                    constants::FIRE_RATE_FLOOR,
                    format!(
                        "fired on {fired} of {eligible} {} ({})",
                        self.denominator(),
                        format::percent(rate),
                    ),
                )
            }
        }
    }
}

/// What one component read, before it knows how many of its siblings also read and therefore what
/// its share of the lane is worth.
struct Reading {
    /// The fraction of its own share this reading spends, or `None` when there was no reading.
    penalty: Option<f64>,
    /// What was read, or why nothing was.
    detail: String,
}

impl Reading {
    fn present(reading: f64, floor: f64, detail: String) -> Self {
        Self {
            penalty: Some((reading / floor).clamp(0.0, 1.0)),
            detail,
        }
    }

    fn absent(detail: String) -> Self {
        Self {
            penalty: None,
            detail,
        }
    }
}

/// One component of a lane's score, as the report renders it: what it read, and what that cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub label: &'static str,
    /// What was read, or why nothing was. Aggregates only — see [`crate::report`].
    pub detail: String,
    /// Points this component took off the lane's 100, or `None` when it had no reading and
    /// therefore no say in the score.
    pub cost: Option<f64>,
}

/// One lane's standing for one harness over one window.
#[derive(Debug, Clone, PartialEq)]
pub enum LaneScore {
    /// At least one component read, so the lane has a score and the components that produced it.
    Scored {
        score: u8,
        components: Vec<Component>,
    },
    /// Signals feed this lane, but none of them read in this window. Not a zero, and not a
    /// hundred: the components are carried so the report can say which ones were silent.
    NoReading { components: Vec<Component> },
    /// Nothing types a signal for this lane at all. Carries the reason from [`Lane::untyped`].
    Untyped(&'static str),
}

impl LaneScore {
    /// Scores one lane over one harness's sessions.
    fn fold(lane: Lane, sessions: &[&SessionMetrics]) -> Self {
        if let Some(reason) = lane.untyped() {
            return Self::Untyped(reason);
        }
        let readings: Vec<_> = lane
            .signals()
            .iter()
            .map(|signal| (signal.label(), signal.read(sessions)))
            .collect();
        let measured = readings
            .iter()
            .filter(|(_, reading)| reading.penalty.is_some())
            .count();
        // The share of the lane one component that read is worth: components weigh equally, and
        // only the ones that read have a say, so silent ones enlarge nobody's share — they shrink
        // the set the mean is taken over.
        let share = constants::CLEAN_SCORE / measured.max(1) as f64;
        let components: Vec<_> = readings
            .into_iter()
            .map(|(label, reading)| Component {
                label,
                detail: reading.detail,
                cost: reading.penalty.map(|penalty| penalty * share),
            })
            .collect();
        if measured == 0 {
            return Self::NoReading { components };
        }
        let spent: f64 = components
            .iter()
            .filter_map(|component| component.cost)
            .sum();
        // Rounded to the whole point: the inputs are rates over a few hundred sessions, and a
        // score to one decimal place would invite reading precision into them that is not there.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=100 before the cast"
        )]
        let score = (constants::CLEAN_SCORE - spent).round().clamp(0.0, 100.0) as u8;
        Self::Scored { score, components }
    }

    /// The score, for a lane that has one.
    pub const fn score(&self) -> Option<u8> {
        match self {
            Self::Scored { score, .. } => Some(*score),
            _ => None,
        }
    }

    /// Every component the lane was scored from, including the ones that read nothing. Empty for
    /// a lane no signal is typed for.
    pub fn components(&self) -> &[Component] {
        match self {
            Self::Scored { components, .. } | Self::NoReading { components } => components,
            Self::Untyped(_) => &[],
        }
    }
}

/// One harness's lanes over one window.
#[derive(Debug, Clone, PartialEq)]
pub struct HarnessScores {
    /// The `source_agent` label, as the manifest states it.
    pub source_agent: String,
    /// Sessions this harness contributed to the window.
    pub sessions: usize,
    /// One entry per lane, in [`Lane::ALL`] order.
    pub lanes: Vec<LaneScore>,
}

impl HarnessScores {
    /// This harness's standing in one lane.
    pub fn lane(&self, lane: Lane) -> &LaneScore {
        let index = Lane::ALL
            .iter()
            .position(|candidate| *candidate == lane)
            .unwrap_or_default();
        &self.lanes[index]
    }
}

/// One blended fleet number, with the blend's constituents named so a trend can refuse to compare
/// two blends taken over different harnesses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blend {
    pub score: u8,
    /// The harnesses that scored the lane, in label order. Equality of this list is what makes
    /// two blends comparable.
    pub harnesses: Vec<String>,
}

impl Blend {
    /// The earlier blend's score, when the two may be compared at all — **only a blend taken over
    /// the same harnesses**.
    ///
    /// A roster change moves an unweighted mean on its own, with nobody's behaviour behind it, and
    /// reporting that as behaviour is the exact failure the blend rule exists to avoid. The rule
    /// lives here rather than at each rendering site because two surfaces now draw the same blend
    /// — the report's table and the dashboard's fleet tile — and a surface that re-derived it
    /// could quietly re-derive it wrong.
    pub fn comparable(&self, earlier: Option<Self>) -> Option<u8> {
        earlier
            .filter(|earlier| earlier.harnesses == self.harnesses)
            .map(|earlier| earlier.score)
    }
}

/// Which way a score moved between two windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Flat,
}

impl Direction {
    /// A stable machine name, for a payload that is read by something other than a person.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Flat => "flat",
        }
    }

    /// The glyph a rendering surface draws it with.
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Up => "▲",
            Self::Down => "▼",
            Self::Flat => "=",
        }
    }
}

/// One score's movement against the comparison window, before any surface decides how to draw it.
///
/// A type rather than a rendered arrow because two surfaces draw the same movement differently —
/// the report writes `▲ 3` into a table cell, the dashboard puts a direction and a magnitude into
/// a JSON field — while the *rule about when a movement may be shown at all* has to be the same
/// one in both places. [`Trend::between`] is that rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trend {
    /// The reported window's score.
    pub now: u8,
    /// The comparison window's score.
    pub was: u8,
}

impl Trend {
    /// The movement from `before` to `now`, or `None` when the comparison window did not measure
    /// the lane.
    ///
    /// **No earlier score, no trend.** An arrow drawn against a window that could not measure the
    /// lane would be reporting the archive's shape as behaviour, which is the one thing a trend
    /// must never do.
    pub const fn between(now: u8, before: Option<u8>) -> Option<Self> {
        match before {
            Some(was) => Some(Self { now, was }),
            None => None,
        }
    }

    /// Which way it went.
    pub const fn direction(self) -> Direction {
        if self.now > self.was {
            Direction::Up
        } else if self.now < self.was {
            Direction::Down
        } else {
            Direction::Flat
        }
    }

    /// How far it went, in points. Always non-negative — the sign lives in
    /// [`Trend::direction`], so no caller has to agree with any other caller about which way is
    /// positive.
    pub const fn magnitude(self) -> u8 {
        self.now.abs_diff(self.was)
    }
}

/// Every harness's lanes over one window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scorecard {
    /// One entry per harness that contributed a session, in `source_agent` label order.
    pub harnesses: Vec<HarnessScores>,
}

impl Scorecard {
    /// Scores the window, per harness.
    ///
    /// Grouping is by `source_agent` and nothing else: harness-relative first, per qanungo #4.
    pub fn fold(sessions: &[SessionMetrics]) -> Self {
        let borrowed: Vec<&SessionMetrics> = sessions.iter().collect();
        Self::fold_refs(&borrowed)
    }

    /// The same fold over a **selection** of a window's sessions.
    ///
    /// The dashboard's repository scopes (qanungo #5) are exactly this: the sessions a fold already
    /// produced, grouped a second way and scored by the same arithmetic. A borrowing entry point
    /// rather than a second implementation, on purpose — a scope whose scores came out of a
    /// parallel formula would drift from the all/all numbers beside it the first time either
    /// changed, and "the page disagrees with itself depending on a dropdown" is not a bug anybody
    /// can act on. [`Self::fold`] is this function over everything.
    ///
    /// A selection changes no rule. A scope's fire-rate denominators are still the rules' own
    /// eligibility over the sessions in it, [`constants::MIN_SCORED_SESSIONS`] still applies, and a
    /// scope too small to read scores nothing rather than a phantom number. The fleet blend inside
    /// a scope is the unweighted mean over **the harnesses present in that scope**, under the same
    /// roster rule — see [`Self::fleet`].
    pub fn fold_refs(sessions: &[&SessionMetrics]) -> Self {
        let mut by_agent: BTreeMap<&str, Vec<&SessionMetrics>> = BTreeMap::new();
        for session in sessions {
            by_agent
                .entry(session.source_agent.as_str())
                .or_default()
                .push(*session);
        }
        Self {
            harnesses: by_agent
                .into_iter()
                .map(|(source_agent, sessions)| HarnessScores {
                    source_agent: source_agent.to_owned(),
                    sessions: sessions.len(),
                    lanes: Lane::ALL
                        .iter()
                        .map(|lane| LaneScore::fold(*lane, &sessions))
                        .collect(),
                })
                .collect(),
        }
    }

    /// One harness's scores, when it contributed to this window.
    pub fn harness(&self, source_agent: &str) -> Option<&HarnessScores> {
        self.harnesses
            .iter()
            .find(|harness| harness.source_agent == source_agent)
    }

    /// The fleet number for one lane: **the unweighted mean of the per-harness scores**, every
    /// harness that scored the lane counting once.
    ///
    /// Unweighted on purpose. A session-weighted mean would move whenever the harness mix moved —
    /// a month with more Copilot in it would show a "trend" nobody's behaviour produced — which
    /// is precisely the failure qanungo #4 names. The cost of the choice is that the blend is
    /// sensitive to the *roster*: a harness appearing or disappearing changes it. That is why
    /// [`Blend::harnesses`] is carried, and why the report renders a fleet arrow only when the two
    /// windows blended the same harnesses.
    pub fn fleet(&self, lane: Lane) -> Option<Blend> {
        let scored: Vec<_> = self
            .harnesses
            .iter()
            .filter_map(|harness| {
                harness
                    .lane(lane)
                    .score()
                    .map(|score| (harness.source_agent.clone(), score))
            })
            .collect();
        if scored.is_empty() {
            return None;
        }
        let total: u32 = scored.iter().map(|(_, score)| u32::from(*score)).sum();
        let mean = f64::from(total) / scored.len() as f64;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a mean of values in 0..=100 is in 0..=100"
        )]
        Some(Blend {
            score: mean.round() as u8,
            harnesses: scored.into_iter().map(|(agent, _)| agent).collect(),
        })
    }
}

/// Everything a score is a function of, and the digest that stands for it.
///
/// Two reports are comparable **iff their digests match**. That is the whole contract: a trend
/// arrow across a pack change would be reporting rule drift as behaviour drift, and the stamp is
/// how a reader tells the two apart without diffing the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulePack {
    entries: Vec<(String, String)>,
    digest: String,
}

impl RulePack {
    /// The pack this build computes with.
    pub fn current() -> Self {
        let entries = pack_entries();
        let digest = digest_of(&entries);
        Self { entries, digest }
    }

    /// The full lowercase hex digest.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The short form the report stamps into its footer.
    pub fn stamp(&self) -> &str {
        &self.digest[..STAMP_CHARS]
    }

    /// Every named value the digest covers, in hashing order.
    pub fn entries(&self) -> &[(String, String)] {
        &self.entries
    }
}

/// Every value a score depends on, named and rendered exactly.
///
/// Order is fixed and the names are hashed alongside the values, so reordering or renaming an
/// entry moves the digest as surely as changing a number does — which is correct: a reader
/// comparing two reports is entitled to know that *anything* about the pack changed.
///
/// Floats are hashed by their bit pattern rather than by a decimal rendering. `0.2` has no exact
/// binary form, and a stamp that rounded it could call two genuinely different thresholds equal.
fn pack_entries() -> Vec<(String, String)> {
    let mut entries = vec![("formula".to_owned(), constants::FORMULA.to_owned())];
    for rule in RuleId::ALL {
        entries.push((format!("rule.{}", rule.key()), rule.title().to_owned()));
    }
    let mut push = |name: &str, value: String| entries.push((name.to_owned(), value));
    push(
        "threshold.session-tool-error-rate",
        float(thresholds::SESSION_TOOL_ERROR_RATE),
    );
    push(
        "threshold.min-session-tool-attempts",
        thresholds::MIN_SESSION_TOOL_ATTEMPTS.to_string(),
    );
    push(
        "threshold.tool-error-rate",
        float(thresholds::TOOL_ERROR_RATE),
    );
    push(
        "threshold.min-tool-attempts",
        thresholds::MIN_TOOL_ATTEMPTS.to_string(),
    );
    push(
        "threshold.retry-loop-repeats",
        thresholds::RETRY_LOOP_REPEATS.to_string(),
    );
    push("threshold.idle-gap", millis(thresholds::IDLE_GAP));
    push(
        "threshold.marathon-sitting-active",
        millis(thresholds::MARATHON_SITTING_ACTIVE),
    );
    push(
        "threshold.resumed-span-to-active",
        float(thresholds::RESUMED_SPAN_TO_ACTIVE),
    );
    push(
        "threshold.resumed-min-sittings",
        thresholds::RESUMED_MIN_SITTINGS.to_string(),
    );
    push(
        "threshold.babysitting-tools-per-request",
        float(thresholds::BABYSITTING_TOOLS_PER_REQUEST),
    );
    push(
        "threshold.babysitting-min-user-requests",
        thresholds::BABYSITTING_MIN_USER_REQUESTS.to_string(),
    );
    push(
        "threshold.fire-and-forget-tools-per-request",
        float(thresholds::FIRE_AND_FORGET_TOOLS_PER_REQUEST),
    );
    push(
        "threshold.fire-and-forget-user-requests",
        thresholds::FIRE_AND_FORGET_USER_REQUESTS.to_string(),
    );
    push(
        "threshold.compaction-churn-completions",
        thresholds::COMPACTION_CHURN_COMPLETIONS.to_string(),
    );
    push("scoring.clean-score", float(constants::CLEAN_SCORE));
    push("scoring.fire-rate-floor", float(constants::FIRE_RATE_FLOOR));
    push(
        "scoring.tool-error-rate-floor",
        float(constants::TOOL_ERROR_RATE_FLOOR),
    );
    push(
        "scoring.min-scored-sessions",
        constants::MIN_SCORED_SESSIONS.to_string(),
    );
    push(
        "scoring.min-scored-tool-attempts",
        constants::MIN_SCORED_TOOL_ATTEMPTS.to_string(),
    );
    for lane in Lane::ALL {
        let signals = lane.signals();
        let value = if signals.is_empty() {
            "untyped".to_owned()
        } else {
            signals
                .iter()
                .map(|signal| signal.key())
                .collect::<Vec<_>>()
                .join(",")
        };
        entries.push((format!("lane.{}", lane.key()), value));
    }
    entries
}

/// A float rendered by its exact bit pattern — see [`pack_entries`].
fn float(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

/// A duration rendered in whole milliseconds, the unit `TimeDelta` is exact in.
fn millis(value: chrono::TimeDelta) -> String {
    format!("{}ms", value.num_milliseconds())
}

/// The digest over a pack's named values: `name=value` per line, sha256, lowercase hex.
fn digest_of(entries: &[(String, String)]) -> String {
    let mut canonical = String::new();
    for (name, value) in entries {
        let _ = writeln!(canonical, "{name}={value}");
    }
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
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

    /// A session whose records are `gaps_minutes` apart, which decides its span, its active time,
    /// and its sittings together — exactly as a real fold does.
    fn session(source_agent: &str, gaps_minutes: &[i64]) -> SessionMetrics {
        let first = at("2026-08-01T09:00:00Z");
        let mut last = first;
        let mut timestamps = vec![first];
        for gap in gaps_minutes {
            last += TimeDelta::minutes(*gap);
            timestamps.push(last);
        }
        SessionMetrics {
            source_hash: "0".repeat(64),
            source_agent: source_agent.to_owned(),
            repository: None,
            artifact_set_version: 2,
            anchors: SessionAnchors::default(),
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
            // Both fixture harnesses type compaction markers, so these are sessions the churn rule
            // can look at, and each one compacted nothing.
            compactions: Compactions {
                observable: true,
                ..Compactions::default()
            },
            reviews: ReviewActivity::default(),
            bytes_folded: 0,
        }
    }

    /// Minutes of continuous work, as gaps the fold keeps inside one sitting.
    fn continuous(minutes: i64) -> Vec<i64> {
        let step = thresholds::IDLE_GAP.num_minutes();
        let mut gaps = vec![step; usize::try_from(minutes / step).unwrap_or_default()];
        if minutes % step != 0 {
            gaps.push(minutes % step);
        }
        gaps
    }

    /// `count` claude-code sessions, the first `marathons` of which work one long unbroken push.
    fn hygiene_window(count: usize, marathons: usize) -> Vec<SessionMetrics> {
        let marathon = thresholds::MARATHON_SITTING_ACTIVE.num_minutes();
        (0..count)
            .map(|index| {
                let minutes = if index < marathons {
                    marathon + 10
                } else {
                    marathon / 4
                };
                session("claude-code", &continuous(minutes))
            })
            .collect()
    }

    fn lane_of(sessions: &[SessionMetrics], lane: Lane) -> LaneScore {
        Scorecard::fold(sessions)
            .harness("claude-code")
            .expect("the fixture harness contributed sessions")
            .lane(lane)
            .clone()
    }

    /// The formula, end to end, on a window whose only penalized reading is the marathon fire
    /// rate: 5 of 20 sessions is a fire rate of 25%, exactly the floor, so that component spends
    /// its whole half of the lane and the lane scores 50.
    #[test]
    fn a_component_at_its_floor_spends_exactly_its_share() {
        let sessions = hygiene_window(20, 5);
        let LaneScore::Scored { score, components } = lane_of(&sessions, Lane::SessionHygiene)
        else {
            panic!("the hygiene lane is fed and the window has readings");
        };
        assert_eq!(score, 50);
        assert_eq!(components.len(), 2);
        assert_eq!(components[0].label, "Marathon session");
        assert_eq!(components[0].cost, Some(50.0));
        assert!(
            components[0].detail.starts_with("fired on 5 of 20"),
            "{}",
            components[0].detail
        );
        // The other half read too — nothing was heavily resumed — and cost nothing.
        assert_eq!(components[1].label, "Heavily resumed session");
        assert_eq!(components[1].cost, Some(0.0));
    }

    /// The two ends of the ramp, and the clamp past it. A rate over the floor cannot push a lane
    /// below zero or take more than its component's share.
    #[test]
    fn the_penalty_ramp_is_linear_between_zero_and_the_floor_and_clamps_past_it() {
        assert_eq!(
            lane_of(&hygiene_window(20, 0), Lane::SessionHygiene).score(),
            Some(100)
        );
        // 10% of the 25% floor is two fifths of the component's 50 points.
        assert_eq!(
            lane_of(&hygiene_window(20, 2), Lane::SessionHygiene).score(),
            Some(80)
        );
        assert_eq!(
            lane_of(&hygiene_window(20, 5), Lane::SessionHygiene).score(),
            Some(50)
        );
        // Every session a marathon: four times the floor, still exactly its own share.
        assert_eq!(
            lane_of(&hygiene_window(20, 20), Lane::SessionHygiene).score(),
            Some(50)
        );
    }

    /// Same inputs, same score, every time — and the same score whichever order the sessions
    /// arrive in. A score is a fact you can recompute (README design rule 3).
    #[test]
    fn scoring_is_deterministic_under_repetition_and_reordering() {
        let sessions = hygiene_window(20, 3);
        let once = Scorecard::fold(&sessions);
        assert_eq!(once, Scorecard::fold(&sessions));

        let mut shuffled = sessions.clone();
        shuffled.reverse();
        assert_eq!(once, Scorecard::fold(&shuffled));
    }

    /// **No lane in the pack is dark any more.** Code Review was the last one, and munshi#77 pull
    /// B lit it — so the `Untyped` state, which exists to keep an unfed lane from rendering as a
    /// zero, is now carried by nothing. The loop this asserts is the point: a lane leaves this
    /// list only when a real signal arrives for it.
    #[test]
    fn no_lane_is_untyped_now_that_code_review_is_lit() {
        for lane in Lane::ALL {
            assert_eq!(lane.untyped(), None, "{lane:?} still claims to be unfed");
        }
        assert_eq!(
            Lane::CodeReview.signals().len(),
            1,
            "Code Review is scored from the unreviewed-ship fire rate alone",
        );
    }

    /// A harness whose review surfaces are not all typed reads **no reading** in this lane — never
    /// a zero, and never a hundred.
    ///
    /// This is the copilot case in miniature, and it is the whole reason the rule's eligibility is
    /// two conditions rather than one. A session that shipped is only in the rate if something
    /// could have seen a review; when nothing could, the lane has no say, and the component's
    /// detail is what tells a reader it was observability rather than behaviour.
    #[test]
    fn a_harness_whose_review_surface_is_untyped_reads_nothing() {
        let sessions = hygiene_window(20, 0);
        let score = lane_of(&sessions, Lane::CodeReview);
        assert_eq!(score.score(), None, "Code Review must not carry a score");
        assert!(
            matches!(score, LaneScore::NoReading { .. }),
            "an unobservable harness has no reading, not a zero: {score:?}",
        );
        let component = &score.components()[0];
        assert_eq!(component.cost, None, "a silent component has no say");
        assert!(
            component
                .detail
                .contains("sessions that shipped on a harness whose review surfaces are all typed"),
            "the detail must name observability as the reason: {}",
            component.detail,
        );
    }

    /// The lane, scored. A window of observable sessions that all shipped and none reviewed is a
    /// 100% fire rate, four times the shared floor, so the one component spends the whole lane and
    /// Code Review scores **0**.
    ///
    /// The uncomfortable number is the assertion on purpose: this rule has no threshold to soften,
    /// so a window that never reviews what it ships scores zero and the report says so.
    #[test]
    fn a_window_that_never_reviews_what_it_ships_scores_zero() {
        let sessions = review_window(20, 20);
        let LaneScore::Scored { score, components } = lane_of(&sessions, Lane::CodeReview) else {
            panic!("an observable window that shipped has a reading");
        };
        assert_eq!(score, 0);
        assert_eq!(components.len(), 1);
        assert!(
            components[0].detail.starts_with("fired on 20 of 20"),
            "{}",
            components[0].detail,
        );
    }

    /// The other end of the same lane: every ship reviewed is a zero fire rate and a clean 100.
    #[test]
    fn a_window_that_reviews_every_ship_scores_a_clean_hundred() {
        let sessions = review_window(20, 0);
        assert_eq!(lane_of(&sessions, Lane::CodeReview).score(), Some(100));
    }

    /// The shared floor applies here like anywhere else: 5 of 20 unreviewed is 25%, exactly
    /// [`constants::FIRE_RATE_FLOOR`], so the single component spends the entire lane. A tenth of
    /// the floor costs a tenth of it.
    #[test]
    fn the_fire_rate_floor_applies_to_this_lane_like_any_other() {
        assert_eq!(
            lane_of(&review_window(20, 5), Lane::CodeReview).score(),
            Some(0)
        );
        assert_eq!(
            lane_of(&review_window(40, 1), Lane::CodeReview).score(),
            Some(90)
        );
    }

    /// Sessions that shipped nothing are not in the denominator at all, however many of them
    /// there are — a session that read code and answered a question did not skip a review.
    #[test]
    fn sessions_that_shipped_nothing_leave_the_rate_entirely() {
        let mut sessions = review_window(6, 6);
        for _ in 0..40 {
            let mut idle = session("claude-code", &continuous(10));
            idle.reviews = ReviewActivity {
                observable: true,
                commits: 0,
                review_passes: 0,
                skill_invocations: 3,
            };
            sessions.push(idle);
        }
        let score = lane_of(&sessions, Lane::CodeReview);
        assert_eq!(score.score(), Some(0));
        assert!(
            score.components()[0].detail.starts_with("fired on 6 of 6"),
            "{}",
            score.components()[0].detail,
        );
    }

    /// `count` observable claude-code sessions that all shipped, the first `unreviewed` of which
    /// ran no review pass.
    fn review_window(count: usize, unreviewed: usize) -> Vec<SessionMetrics> {
        (0..count)
            .map(|index| {
                let mut session = session("claude-code", &continuous(10));
                session.reviews = ReviewActivity {
                    observable: true,
                    commits: 2,
                    review_passes: u64::from(index >= unreviewed),
                    skill_invocations: 1,
                };
                session
            })
            .collect()
    }

    /// The lane munshi#77 woke: a window of sessions that never compacted scores a clean hundred,
    /// off the one component the lane has.
    #[test]
    fn context_management_scores_from_the_compaction_churn_rate() {
        let clean = hygiene_window(20, 0);
        let LaneScore::Scored { score, components } = lane_of(&clean, Lane::ContextManagement)
        else {
            panic!("every fixture session is one the rule can look at");
        };
        assert_eq!(score, 100);
        assert_eq!(components.len(), 1, "a single-component lane");
        assert_eq!(components[0].label, "Compaction churn");
        assert_eq!(
            components[0].detail,
            "fired on 0 of 20 sessions whose harness records compactions (0%)",
        );

        // Five of the twenty thrashing is a 25% fire rate — exactly the floor — and with one
        // component that spends the whole lane rather than half of it.
        let mut thrashing = clean;
        for session in thrashing.iter_mut().take(5) {
            session.compactions.completed = thresholds::COMPACTION_CHURN_COMPLETIONS;
        }
        let LaneScore::Scored { score, components } = lane_of(&thrashing, Lane::ContextManagement)
        else {
            panic!("the component reads");
        };
        assert_eq!(components[0].cost, Some(100.0));
        assert_eq!(score, 0);
    }

    /// A harness this interpreter reads no compaction for is a harness that cannot be scored on
    /// this lane — not one that scores a hundred. Codex sessions leave the denominator entirely.
    #[test]
    fn a_harness_with_no_compaction_marker_gets_no_context_management_reading() {
        let mut sessions = hygiene_window(20, 0);
        for session in &mut sessions {
            session.source_agent = "codex-cli".to_owned();
            session.compactions.observable = false;
        }
        let lane = Scorecard::fold(&sessions)
            .harness("codex-cli")
            .expect("codex contributed sessions")
            .lane(Lane::ContextManagement)
            .clone();
        assert_eq!(lane.score(), None, "{lane:?}");
        let LaneScore::NoReading { components } = lane else {
            panic!("the lane is fed, but nothing in this window could look");
        };
        assert!(
            components[0]
                .detail
                .contains("only 0 sessions whose harness")
        );
    }

    /// A fed lane whose signals are all silent is also not a score. The window below has five
    /// sessions with no tool outcome and no command, so both Tool Mastery components read
    /// nothing — which must not come out as a hundred.
    #[test]
    fn a_fed_lane_with_no_reading_is_not_a_hundred() {
        let sessions = hygiene_window(5, 0);
        let score = lane_of(&sessions, Lane::ToolMastery);
        assert_eq!(score.score(), None);
        let LaneScore::NoReading { components } = score else {
            panic!("tool mastery is fed but silent here");
        };
        assert_eq!(components.len(), 2);
        assert!(components.iter().all(|component| component.cost.is_none()));
        assert!(
            components[0]
                .detail
                .contains("no call in this window reported an outcome"),
            "{}",
            components[0].detail
        );
    }

    /// A component that reads nothing has no say: the mean runs over the components that read, so
    /// the surviving one carries the whole lane rather than being averaged against a phantom zero.
    #[test]
    fn a_silent_component_shrinks_the_mean_rather_than_scoring_zero() {
        // Twenty sessions, four of them marathons — a fire rate of 20%, four fifths of the way to
        // the floor. The dilution component is silenced by dropping the summary timestamps the
        // span is derived from, leaving the sittings the marathon rule reads intact.
        let mut sessions = hygiene_window(20, 4);
        for session in &mut sessions {
            session.summary.first_timestamp = None;
            session.summary.last_timestamp = None;
        }
        let LaneScore::Scored { score, components } = lane_of(&sessions, Lane::SessionHygiene)
        else {
            panic!("the marathon component still reads");
        };
        assert_eq!(
            components[0].cost,
            Some(80.0),
            "the surviving component carries the whole lane"
        );
        assert!(components[1].cost.is_none(), "no dilution is measurable");
        assert_eq!(
            score, 20,
            "averaging the silent component in as a zero would have scored 60"
        );
    }

    /// Below the minimum, a fire rate is not a reading. Four eligible sessions cannot produce a
    /// rate this pack will spend points on, however many of them fired.
    #[test]
    fn a_fire_rate_over_too_few_sessions_is_no_reading_at_all() {
        let below = Scorecard::fold(&hygiene_window(
            constants::MIN_SCORED_SESSIONS - 1,
            constants::MIN_SCORED_SESSIONS - 1,
        ));
        let lane = below.harnesses[0].lane(Lane::SessionHygiene);
        assert_eq!(lane.score(), None, "{lane:?}");
        assert!(
            lane.components()[0].detail.contains("fewer than the"),
            "{:?}",
            lane.components()[0]
        );

        // One more eligible session and the same all-marathon window scores.
        let at_minimum = Scorecard::fold(&hygiene_window(
            constants::MIN_SCORED_SESSIONS,
            constants::MIN_SCORED_SESSIONS,
        ));
        assert_eq!(
            at_minimum.harnesses[0].lane(Lane::SessionHygiene).score(),
            Some(50)
        );
    }

    /// Harness-relative first: two harnesses in one window are scored separately, and the fleet
    /// number is the unweighted mean of their scores — not of their sessions. Twenty clean
    /// Copilot sessions do not drown out five bad Claude Code ones.
    #[test]
    fn the_fleet_blend_is_an_unweighted_mean_of_the_harness_scores() {
        let mut sessions = hygiene_window(20, 10);
        for session in &mut sessions {
            session.source_agent = "claude-code".to_owned();
        }
        sessions.extend(
            hygiene_window(20, 0)
                .into_iter()
                .map(|mut session| {
                    session.source_agent = "copilot-cli".to_owned();
                    session
                })
                .collect::<Vec<_>>(),
        );
        let card = Scorecard::fold(&sessions);
        assert_eq!(card.harnesses.len(), 2);
        assert_eq!(
            card.harness("claude-code")
                .unwrap()
                .lane(Lane::SessionHygiene)
                .score(),
            Some(50),
        );
        assert_eq!(
            card.harness("copilot-cli")
                .unwrap()
                .lane(Lane::SessionHygiene)
                .score(),
            Some(100),
        );
        let blend = card
            .fleet(Lane::SessionHygiene)
            .expect("both harnesses scored");
        assert_eq!(blend.score, 75);
        assert_eq!(blend.harnesses, vec!["claude-code", "copilot-cli"]);
        // A lane nobody scored has no blend either.
        assert_eq!(card.fleet(Lane::CodeReview), None);
    }

    /// The stamp is stable across calls and covers every rule and every lane by name, so a rule
    /// or lane added without a pack entry cannot slip through unstamped.
    #[test]
    fn the_rule_pack_stamp_is_stable_and_covers_every_rule_and_lane() {
        let pack = RulePack::current();
        assert_eq!(pack.digest(), RulePack::current().digest());
        assert_eq!(pack.stamp().len(), STAMP_CHARS);
        assert!(pack.digest().starts_with(pack.stamp()));
        assert!(pack.digest().chars().all(|c| c.is_ascii_hexdigit()));
        for rule in RuleId::ALL {
            let name = format!("rule.{}", rule.key());
            assert!(
                pack.entries().iter().any(|(entry, _)| *entry == name),
                "{name} is unstamped",
            );
        }
        for lane in Lane::ALL {
            let name = format!("lane.{}", lane.key());
            assert!(
                pack.entries().iter().any(|(entry, _)| *entry == name),
                "{name} is unstamped",
            );
        }
        // Every entry is named once: a duplicated name would let one value shadow another.
        let mut names: Vec<_> = pack.entries().iter().map(|(name, _)| name).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique);
    }

    /// The property the whole stamp exists for: change any threshold, any scoring constant, the
    /// formula tag, or the lane mapping, and the digest moves. Exercised by mutating the entry
    /// list rather than the constants, because the constants are `const`.
    #[test]
    fn changing_any_pack_value_changes_the_digest() {
        let pack = RulePack::current();
        for index in 0..pack.entries().len() {
            let mut mutated = pack.entries().to_vec();
            mutated[index].1.push_str("-changed");
            assert_ne!(
                digest_of(&mutated),
                pack.digest(),
                "changing {} left the stamp alone",
                mutated[index].0,
            );
        }
        // Renaming an entry moves it too — the names are hashed alongside the values.
        let mut renamed = pack.entries().to_vec();
        renamed[0].0.push_str("-renamed");
        assert_ne!(digest_of(&renamed), pack.digest());
        // And so does reordering, because the canonical form is a sequence.
        let mut reordered = pack.entries().to_vec();
        reordered.swap(0, 1);
        assert_ne!(digest_of(&reordered), pack.digest());
    }

    /// Two thresholds that differ only past the decimal rendering must still stamp differently:
    /// the digest hashes float bit patterns, not printed decimals.
    #[test]
    fn near_identical_floats_do_not_share_a_stamp() {
        assert_ne!(float(0.2), float(0.2 + f64::EPSILON));
        assert_eq!(float(0.2), float(0.2));
    }

    /// The rule every surface draws an arrow by: no earlier score, no trend — and a magnitude that
    /// is always the distance, with the sign carried by the direction rather than by a convention
    /// each caller has to remember.
    #[test]
    fn a_trend_needs_both_windows_and_keeps_its_sign_in_the_direction() {
        assert_eq!(Trend::between(80, None), None);

        let improved = Trend::between(80, Some(50)).expect("both windows measured it");
        assert_eq!(improved.direction(), Direction::Up);
        assert_eq!(improved.magnitude(), 30);

        let worsened = Trend::between(50, Some(80)).expect("both windows measured it");
        assert_eq!(worsened.direction(), Direction::Down);
        assert_eq!(worsened.magnitude(), 30);

        let flat = Trend::between(70, Some(70)).expect("both windows measured it");
        assert_eq!(flat.direction(), Direction::Flat);
        assert_eq!(flat.magnitude(), 0);

        for direction in [Direction::Up, Direction::Down, Direction::Flat] {
            assert!(!direction.key().is_empty());
            assert!(!direction.glyph().is_empty());
        }
    }

    /// Two blends compare only when they are means over the *same* harnesses. A roster change
    /// moves an unweighted mean with nobody's behaviour behind it.
    #[test]
    fn two_blends_over_different_harnesses_never_compare() {
        let blend = |score: u8, harnesses: &[&str]| Blend {
            score,
            harnesses: harnesses.iter().map(|name| (*name).to_owned()).collect(),
        };
        let both = blend(75, &["claude-code", "copilot-cli"]);
        assert_eq!(
            both.comparable(Some(blend(60, &["claude-code", "copilot-cli"]))),
            Some(60),
        );
        assert_eq!(both.comparable(None), None);
        assert_eq!(both.comparable(Some(blend(60, &["claude-code"]))), None);
        // Order is part of the identity, because the list is built in label order on both sides:
        // two lists that differ only in order did not come from the same fold.
        assert_eq!(
            both.comparable(Some(blend(60, &["copilot-cli", "claude-code"]))),
            None,
        );
    }
}
