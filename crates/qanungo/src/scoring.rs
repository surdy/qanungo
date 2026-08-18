//! Practice-lane scores, and the rule-pack stamp that says when two of them may be compared.
//!
//! The five lanes are the ones qanungo #4 names — Prompt Quality, Session Hygiene, Code Review,
//! Tool Mastery, Context Management — and only three of them are fed by anything the fold types
//! today. **A lane with no feeding signal is not scored.** It renders as unscored, with the
//! signal it is waiting for named. It never gets a default, a proxy, or a zero: the same
//! no-signal-no-claim discipline [`CommandChurn`](crate::metrics::CommandChurn) applies to one
//! session's churn, applied to a whole lane.
//!
//! # The mapping, and why each half of it is defensible
//!
//! | Lane | Fed by | Because |
//! | --- | --- | --- |
//! | Tool Mastery | pooled tool error rate; retry-loop fire rate | both are readings of *how well the tools are being driven* — calls that failed, and commands re-run until they stopped disagreeing |
//! | Session Hygiene | marathon fire rate; heavily-resumed fire rate | both are readings of *how the work is packaged into sessions* — one unbroken push, and one transcript standing in for many work items |
//! | Prompt Quality | babysitting fire rate; fire-and-forget fire rate | both are readings of *how the ask was shaped* — a hundred small steering turns, or one enormous unattended run with no checkpoint in it |
//! | Code Review | nothing | no typed signal reports review activity at all |
//! | Context Management | nothing | no typed signal reports compaction or context utilization at all |
//!
//! The two empty rows are the interesting ones. Marathon *sounds* like context management — a
//! context window accumulating without a break — and heavily-resumed *sounds* like it too. Both
//! are declined here on purpose: what the fold measures is sitting length and calendar dilution,
//! not context. Scoring Context Management off them would be scoring an implication, and one
//! signal counted into two lanes is one behaviour reported as two findings. The lane stays
//! unscored until munshi#77 types compaction events and per-request token usage.
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
//! pack sees what the fold types, and that is four metrics.
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
            Self::CodeReview | Self::ContextManagement => &[],
        }
    }

    /// Why this lane is not scored, for a lane nothing feeds. `None` for a lane the pack scores.
    ///
    /// Stated as the signal that is missing rather than as an apology: the sentence a reader
    /// needs is which pull would light the lane up, not that it is dark.
    pub const fn untyped(self) -> Option<&'static str> {
        match self {
            Self::CodeReview => Some(
                "no signal typed for this lane yet — nothing in the event stream reports review \
                 activity (files edited versus read, diffs reviewed, revert-and-retry cycles), so \
                 the lane has no reading and takes no default",
            ),
            Self::ContextManagement => Some(
                "no signal typed for this lane yet — nothing in the event stream reports \
                 compaction events, pre-compaction utilization, or per-request token usage, so \
                 the lane has no reading and takes no default. Sitting length and calendar \
                 dilution are deliberately *not* borrowed for it: they are Session Hygiene's \
                 readings, and one signal counted into two lanes is one behaviour reported twice",
            ),
            _ => None,
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
        let mut by_agent: BTreeMap<&str, Vec<&SessionMetrics>> = BTreeMap::new();
        for session in sessions {
            by_agent
                .entry(session.source_agent.as_str())
                .or_default()
                .push(session);
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
    use crate::metrics::{Activity, CommandChurn, ToolOutcomes};

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

    /// A lane nothing types a signal for refuses to score, and says which pull would light it up.
    /// It is never a zero and never a default.
    #[test]
    fn an_unfed_lane_refuses_to_score() {
        let sessions = hygiene_window(20, 0);
        for lane in [Lane::CodeReview, Lane::ContextManagement] {
            let score = lane_of(&sessions, lane);
            assert_eq!(score.score(), None, "{lane:?} must not carry a score");
            let LaneScore::Untyped(reason) = score else {
                panic!("{lane:?} has no typed signal");
            };
            assert!(reason.contains("no signal typed for this lane yet"));
        }
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
}
