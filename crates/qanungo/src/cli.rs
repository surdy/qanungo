//! The command-line surface.
//!
//! P0 was one command — `qanungo report` — because the vertical slice was the point: sync, fold,
//! evaluate, emit, all in one invocation, with nothing between the archive and the Markdown on
//! stdout. `qanungo cost` (qanungo #12) is the second lane over the same slice: the same mirror,
//! the same blob cache, the same [`Window`] and its comparison window, a different fold and a
//! different document.
//!
//! Everything both commands need to reach the archive lives in [`ArchiveArgs`] and is flattened
//! into each of them, so a flag means the same thing wherever it is typed and a third lane
//! inherits it by declaring one field.
//!
//! [`RedactionArgs`] was defined one lane early for the same reason and is now live: `qanungo
//! standup` (qanungo #9) is the first command that renders archived prose, so it is the first —
//! and so far only — command to flatten it. `report` and `cost` still do not, and attaching it to
//! them would be decoration over documents that carry no content at all (see
//! [`crate::redaction`]).
//!
//! `qanungo dashboard` (qanungo #5) is the fourth lane and the first that is not a document: it
//! flattens [`ArchiveArgs`] like the rest and adds what a served surface needs — where to listen,
//! and how often to recompute. Since the evidence-excerpt slice it flattens [`RedactionArgs`] too,
//! because its excerpt route renders the text of the events a rule counted. The flag is read
//! **once, at launch**, and belongs to the process rather than to a request: see
//! [`DashboardArgs`].
//!
//! [`OutputArgs`] is the third flattened group and the newest: `--json` on every document lane,
//! Markdown still the default. It is one flag on one shared struct for the reason the other two
//! are — six spellings of "give me the data" would be six chances for one of them to mean
//! something slightly else.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use clap::{Args, Parser, Subcommand};

use crate::redaction::{FILTER_PROFANITY_BY_DEFAULT, REDACT_SECRETS_BY_DEFAULT, Redactor};

/// What to say when nobody told qanungo which archive to read.
///
/// There is deliberately **no default archive**. A Patwari instance is something a person runs;
/// there is no hosted one to fall back to, and an address compiled in here could only ever be
/// right for whoever compiled it. So the missing-URL case is not an accident to paper over with a
/// guess — it is the one piece of setup this tool cannot do for you, and it gets a sentence that
/// says how to finish it rather than a parser's shorthand. [`crate::cli`]'s caller prints this in
/// place of clap's "required arguments were not provided" line; the flag is still declared
/// `required`, so `--help` tells the same truth.
pub const MISSING_PATWARI_URL: &str = "\
qanungo reads a Patwari archive and needs that archive's base URL. There is no default, because \
the archive is yours and only you know where it lives. Pass `--patwari-url <URL>` on any command, \
or set it once in your shell — `export PATWARI_URL=https://patwari.example.net` — and every \
command picks it up from there. The README's Install section has the rest of the setup.";

/// How far back a standup reaches when nobody says otherwise.
///
/// **A tunable, not a decision.** A week is what "what have I been up to" usually means and what
/// a Monday standup covers, and it is short enough that the document stays readable when every
/// session in it is rendered in full — which is the property that separates this lane from
/// `report` and `cost`, whose output is a fixed size however long the window. The issue's own
/// text says `30d`; that produces a document nobody reads to the end of, so the default is a week
/// and `--last 30d` still says exactly what the issue asked for.
pub const DEFAULT_STANDUP_WINDOW: &str = "7d";

/// How far back the cost lane prices when nobody says otherwise.
///
/// A quarter, spelled in the units the window grammar actually has. qanungo #12 asks for "3m" and
/// the grammar deliberately has no month unit — `m` reads as either minutes or months, and a
/// coaching window that could be misread by a factor of forty thousand is worse than one that has
/// to be spelled in weeks.
///
/// Named rather than repeated because two commands now default to it: `qanungo cost --last` and the
/// dashboard's `--cost-last`, which is the *same* window under a different presentation. Two
/// literals that happened to agree would be one edit away from a page pricing a different quarter
/// from the terminal beside it.
pub const DEFAULT_COST_WINDOW: &str = "12w";

/// How many ranked matches `qanungo ask` prints when nobody says otherwise.
///
/// **A tunable, not a decision.** Ten is enough to see the shape of what matched without turning a
/// search into a document, and `--limit` widens or narrows it. Because the ranking is total, the
/// tenth line is the tenth-*best* match and not merely the tenth one the fold happened to read.
pub const DEFAULT_ASK_LIMIT: usize = 10;

/// Where `qanungo dashboard` listens when nobody says otherwise.
///
/// **Loopback**, because a surface with no authentication has to be opt-in to being reachable at
/// all; `--bind` on a tailnet address is how a phone or a TV gets at it, and the startup line says
/// out loud what that costs (see [`DashboardArgs::bind`]).
///
/// Port **8878** is one above munshi-dashboard's 8877, deliberately: the two are sibling read-only
/// dashboards over the same lineage and get run on the same laptop, so adjacent ports are one
/// fewer thing to remember. It is well clear of Patwari's own 8787.
pub const DEFAULT_DASHBOARD_BIND: &str = "127.0.0.1:8878";

/// How often the dashboard re-syncs and re-folds when nobody says otherwise.
///
/// **A tunable, not a decision.** A coaching window of thirty days does not change meaningfully
/// inside five minutes — a session has to be finished, archived, and listed before it can move a
/// number — so this is chosen to keep the archive quiet rather than to keep the page fresh.
///
/// A refresh now folds three lanes rather than one, and the measurement moved with it — twice. The
/// three lanes together measured 45.4 s warm against the production archive on 2026-08-25; qanungo
/// #1's snapshot index then cut the same refresh to **about 13 s** — coaching 6.8 s, cost 6.1 s,
/// standup 0.1 s — of which the archive itself is about 2.5 s. Five minutes therefore spends about
/// 4% of the dashboard's life talking to Patwari. Still a quiet client, and still well clear of the
/// floor below.
pub const DEFAULT_DASHBOARD_REFRESH: &str = "5m";

/// Floor under `--refresh`, refused rather than clamped.
///
/// A warm three-lane refresh measures about 13 s against the production archive, and Patwari serves
/// about eight concurrent requests. A refresh interval near the refresh's own duration is not a
/// fresher dashboard, it is a permanent polling load on a LAN archive that has other readers — so
/// somebody who typed `--refresh 5s` is told what this client will not do rather than quietly given
/// something else.
///
/// The floor stayed at a minute through both measurements — 45 s when the slice tripled what a
/// refresh costs, and about 13 s once qanungo #1's snapshot index made the refresh fold-bound — and
/// that is worth saying out loud rather than leaving as an oversight: a minute was barely above the
/// measured refresh at 45 s and is more than four times it at 13 s, so the same constant has meant
/// two quite different things. It is left where it is because the floor's job is to refuse the
/// *absurd* ask — a five-second poll — and not to second-guess an operator who has decided their
/// own archive can wear a tight loop. Raising it would be this
/// client deciding that for them; the honest place for the discouragement is the measured number in
/// [`DEFAULT_DASHBOARD_REFRESH`] above, which is what anybody choosing an interval should read.
pub const MIN_DASHBOARD_REFRESH: Duration = Duration::from_secs(60);

#[derive(Debug, Parser)]
#[command(
    name = "qanungo",
    version,
    about = "Read-side coaching client over the Patwari archive"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Fold recent archived sessions into a Markdown coaching report on stdout.
    Report(ReportArgs),
    /// Fold recent archived sessions into a Markdown token/cost breakdown on stdout.
    Cost(CostArgs),
    /// Narrate recent archived sessions from their own summaries, as Markdown on stdout.
    Standup(StandupArgs),
    /// Search the archived summaries for a plain-language query, ranked, as Markdown on stdout.
    Ask(AskArgs),
    /// Find instructions you have had to repeat across sessions of one repository, as Markdown on
    /// stdout.
    Doctor(DoctorArgs),
    /// Find requests you have repeated across the whole archive, and the multi-step flows they
    /// fall into, as Markdown on stdout.
    Flows(FlowsArgs),
    /// Serve the coaching report's own numbers as a read-only web page, refreshed in the
    /// background.
    Dashboard(DashboardArgs),
    /// Print the rules, lanes, thresholds, prices, and redaction pattern names this build decides
    /// with, as Markdown on stdout. Reads no archive.
    Rules(RulesArgs),
}

/// The catalogue lane, and the one command that does **not** flatten [`ArchiveArgs`].
///
/// It describes this build rather than a window of history: no session is read, no blob is
/// mirrored, and nothing is listed. Requiring `--patwari-url` to print it would mean a person
/// could not read what the tool looks for until they had finished setting the tool up, which is
/// backwards for the document that explains it — so the struct is deliberately empty rather than
/// borrowing the shared arguments and ignoring them.
///
/// Empty, and not collapsed into a bare unit variant, so that a later flag — a section filter, a
/// `--json` rendering — lands as a field here rather than as a change to the command's shape.
#[derive(Debug, Args)]
pub struct RulesArgs {}

/// How to reach the archive, shared by every lane. Flattened rather than repeated so
/// `--patwari-url` cannot come to mean two slightly different things in two subcommands.
#[derive(Debug, Args, Clone)]
pub struct ArchiveArgs {
    /// Base URL of the Patwari archive to read, e.g. `https://patwari.example.net`. Required on
    /// every command; set `PATWARI_URL` once in your shell to stop typing it.
    #[arg(
        long = "patwari-url",
        env = "PATWARI_URL",
        required = true,
        value_name = "URL"
    )]
    pub patwari_url: String,

    /// Cache root for the mirrored transcripts. Defaults to `$XDG_CACHE_HOME/qanungo`, falling
    /// back to `~/.cache/qanungo`.
    #[arg(long = "cache-dir")]
    pub cache_dir: Option<PathBuf>,

    /// Concurrent requests against the archive, 1 to 8. Kept small on purpose: Patwari is a LAN
    /// server with a modest concurrency limit, and occupying every slot it has does not make a
    /// report faster — it starves the archive's other readers.
    #[arg(long, default_value_t = crate::sync::DEFAULT_CONCURRENCY, value_parser = parse_concurrency)]
    pub concurrency: usize,
}

/// How much of what a transcript said may reach the screen, shared by every lane that renders
/// transcript content. Flattened for the same reason [`ArchiveArgs`] is: `--no-redact` must mean
/// exactly one thing, everywhere, forever.
///
/// Both flags are opt-*in* to a change from the shipped default, so the safe reading is the one a
/// person gets by typing nothing:
///
/// - `--no-redact` turns the secrets pass off. Phrased as a negation on purpose — there is no
///   `--redact`, because redaction is not something a person should have to remember to ask for,
///   and the only way to lose it is to say so out loud in a command line somebody can read back.
/// - `--filter-profanity` turns the profanity pass on. Default off; see
///   [`crate::redaction::FILTER_PROFANITY_BY_DEFAULT`] for why that is a tunable rather than a
///   decision.
#[derive(Debug, Args)]
pub struct RedactionArgs {
    /// Render transcript content without scrubbing secrets. A deliberate choice for a trusted
    /// terminal, never the default.
    #[arg(long = "no-redact", default_value_t = !REDACT_SECRETS_BY_DEFAULT)]
    pub no_redact: bool,

    /// Mask a small conservative list of profane words in rendered transcript content.
    #[arg(long = "filter-profanity", default_value_t = FILTER_PROFANITY_BY_DEFAULT)]
    pub filter_profanity: bool,
}

impl RedactionArgs {
    /// The redactor these flags ask for.
    pub const fn redactor(&self) -> Redactor {
        Redactor::new()
            .with_secrets(!self.no_redact)
            .with_profanity(self.filter_profanity)
    }
}

/// Which document a lane writes to stdout, shared by every lane that writes one.
///
/// Flattened for the reason [`ArchiveArgs`] and [`RedactionArgs`] are: `--json` has to mean the
/// same thing on all six lanes, and a seventh inherits it by declaring one field rather than by
/// somebody remembering to copy a flag.
///
/// **Markdown stays the default**, on every lane. The documents are the product — a report is meant
/// to be read by a person, piped into a pager, or pasted into a skill — and `--json` is the same
/// fold under a shape a program can index. It is never a *different* fold: see [`crate::json`],
/// which serializes what the renderers render and computes nothing of its own.
#[derive(Debug, Args, Clone, Copy)]
pub struct OutputArgs {
    /// Write this lane's own data as a JSON envelope on stdout instead of Markdown. The same
    /// numbers under the same scrub, wrapped in `schema_version`, `command`, `window`,
    /// `rule_pack`, `generated_at`, `provenance`, and `data`.
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// How far back to report, as `<count><unit>` with unit `h`, `d`, or `w`.
    #[arg(long = "last", default_value = "30d", value_parser = parse_window)]
    pub last: Window,

    #[command(flatten)]
    pub archive: ArchiveArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct CostArgs {
    /// How far back to price, as `<count><unit>` with unit `h`, `d`, or `w`. The default is a
    /// quarter, spelled `12w` — see [`DEFAULT_COST_WINDOW`].
    #[arg(long = "last", default_value = DEFAULT_COST_WINDOW, value_parser = parse_window)]
    pub last: Window,

    #[command(flatten)]
    pub archive: ArchiveArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// The standup lane, and the first command to flatten [`RedactionArgs`].
///
/// Three flattened groups rather than three sets of near-identical flags: `--patwari-url` means
/// what it means in every other lane, and `--no-redact` will mean what it means in every lane
/// after this one.
#[derive(Debug, Args)]
pub struct StandupArgs {
    /// How far back to narrate, as `<count><unit>` with unit `h`, `d`, or `w`.
    #[arg(long = "last", default_value = DEFAULT_STANDUP_WINDOW, value_parser = parse_window)]
    pub last: Window,

    #[command(flatten)]
    pub archive: ArchiveArgs,

    #[command(flatten)]
    pub redaction: RedactionArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// The ask lane (qanungo #10): plain-language search over the summaries munshi already wrote.
///
/// This is qanungo's no-editor answer to "have I touched the payments API?" — the session-recall
/// funnel's first stage, given a command surface. It searches the same `summary.md` records the
/// standup lane narrates, so it flattens [`RedactionArgs`] for the same reason: a matched snippet
/// is archived prose, and prose reaches the screen only through the scrub.
///
/// # No default window, unlike every other lane
///
/// `report`, `cost`, and `standup` each default to a window, because each answers a question about
/// a stretch of recent time. Ask does not: "have I ever done this" is a question about *all* of
/// history, and a lane that quietly searched only the last week would answer a narrower question
/// than the one that was typed and call an absence a "no". So `--last` is optional here, and its
/// absence means the whole archive. When it is given it narrows the search on the same grammar,
/// for the reader who does mean "lately".
///
/// # `--verbatim` is the funnel's next stage, not a second search
///
/// A summary is a curated record, so a ranking over summaries answers "which session was this
/// about" and stops there. `--verbatim` picks the funnel up where it stops: it takes the hits this
/// run is going to *show* and searches their transcripts for the same terms. That is a bounded
/// escalation — at most `--limit` transcripts, most of them already cached — and deliberately not
/// an archive-wide full-text search, which would mean mirroring every transcript in the archive to
/// answer one question. The boundary that comes with it is stated in the document: a session no
/// summary matched is a session this never digs into.
#[derive(Debug, Args)]
pub struct AskArgs {
    /// The words to search for. Matched case-insensitively against each summary's own text; very
    /// short and very common words are dropped first, so a query cannot rank every session equally
    /// on the strength of the word "the".
    pub query: String,

    /// Narrow the search to `<count><unit>` of history, with unit `h`, `d`, or `w`. Omitted, the
    /// search covers the whole archive — see this struct's own note for why that is the default.
    #[arg(long = "last", value_parser = parse_window)]
    pub last: Option<Window>,

    /// How many ranked matches to print. Refused rather than clamped below one: a search that can
    /// show nothing is not a narrower search, it is a broken one.
    #[arg(long = "limit", default_value_t = DEFAULT_ASK_LIMIT, value_parser = parse_limit)]
    pub limit: usize,

    /// Also search the transcripts behind the matches that are shown, and quote the lines that hit.
    /// This digs into the sessions the summaries found — not the whole archive — so it fetches at
    /// most `--limit` transcripts, and a session no summary matched is still not searched. Excerpts
    /// are scrubbed like every other line this lane quotes.
    #[arg(long = "verbatim")]
    pub verbatim: bool,

    #[command(flatten)]
    pub archive: ArchiveArgs,

    #[command(flatten)]
    pub redaction: RedactionArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// The doctor lane (qanungo #11): instructions the archive shows you giving more than once.
///
/// The read-only half of the instructions doctor. It clusters near-duplicate user messages across
/// the sessions of one repository and quotes each cluster once, scrubbed, with a citation per
/// occurrence. It flattens [`RedactionArgs`] because that quotation is transcript text somebody
/// typed — this is the CLI's second verbatim surface, after `ask --verbatim`.
///
/// # No default window, like `ask`
///
/// "Have I been repeating myself?" is a question about all of history, not about the last month, so
/// `--last` is optional here and its absence means the whole archive. That is the [`AskArgs`]
/// precedent and it is chosen for the same reason: a lane that quietly searched only a recent window
/// would answer a narrower question than the one that was typed.
///
/// # It folds transcripts, so a cold run downloads the archive
///
/// Unlike `ask`, this lane reads `transcript.jsonl` rather than `summary.md`, because a summary is
/// munshi's curated prose and the thing being compared here is what a *person* typed. A transcript
/// is roughly two hundred times the bytes of the summary beside it, so a first run with no `--last`
/// mirrors the whole archive — several gigabytes — before it clusters anything. Warm runs ride the
/// shared blob cache and the snapshot index like `report` does, and the instrumentation footer
/// reports what either one actually cost rather than leaving the reader to guess.
///
/// # The cut on each section is a default, not a ceiling
///
/// Each repository's clusters are rendered best-first and cut at
/// [`crate::doctor::DEFAULT_CLUSTERS_PER_REPOSITORY`], under a line saying how many were held back.
/// `--clusters-per-repo` raises that cut, because the reader of this document is often the
/// `instructions-editor` skill and the cut lands *inside* the weight class it acts on: the first
/// production run hid two two-occurrence clusters behind that line while a two-occurrence cluster
/// above it produced a shipped instruction-file edit (qanungo #16). Raising it changes what is
/// shown and nothing that is counted — the cut is on the rendering, never on the clustering.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Narrow the reading to `<count><unit>` of history, with unit `h`, `d`, or `w`. Omitted, it
    /// covers the whole archive: "have I been repeating myself" is a question about all of history,
    /// so this lane has no default window.
    #[arg(long = "last", value_parser = parse_window)]
    pub last: Option<Window>,

    /// How many clusters each repository's section renders, best first. The rest are counted and
    /// declared as held back rather than dropped silently, so raise this to read them; the document
    /// states the number in force whenever it is not the default. Refused rather than clamped below
    /// one: a section that can show no cluster is not a shorter finding, it is a heading with a
    /// footnote under it.
    #[arg(
        long = "clusters-per-repo",
        default_value_t = crate::doctor::DEFAULT_CLUSTERS_PER_REPOSITORY,
        value_parser = parse_cluster_cap,
    )]
    pub clusters_per_repo: usize,

    #[command(flatten)]
    pub archive: ArchiveArgs,

    #[command(flatten)]
    pub redaction: RedactionArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// The flows lane (qanungo #13): requests you have repeated anywhere, and the sequences they fall
/// into.
///
/// The read-only half of the skill & agent finder. It runs [`crate::doctor`]'s own detection
/// machinery — the two share [`crate::repetition`] rather than forking it — over **one pool holding
/// every session in the reach**, and then mines the recurring two- and three-step runs of the
/// clusters that come out. It flattens [`RedactionArgs`] because those excerpts are transcript text
/// somebody typed: this is the CLI's third verbatim surface, after `ask --verbatim` and `doctor`.
///
/// # The lens is the archive, deliberately
///
/// `doctor` groups per repository because an instruction file belongs to one. This lane pools
/// everything, including the sessions the archive attributes to no repository at all, because the
/// thing it looks for — a workflow worth a skill — is worth it wherever it recurs. Repositories are
/// what a finding is *listed by* here, never what it is grouped by.
///
/// # No default window, like `ask` and `doctor`
///
/// "What do I keep doing?" is a question about all of history, so `--last` is optional and its
/// absence means the whole archive — the [`AskArgs`] precedent, chosen for its reason: a lane that
/// quietly searched only a recent window would answer a narrower question than the one that was
/// typed.
///
/// # It folds transcripts, so a cold run downloads the archive
///
/// Same substrate as `doctor`, for the same reason: what is compared is what a *person* typed, and a
/// `summary.md` is munshi's curated prose about a session rather than the session's own words. A
/// first run with no `--last` mirrors the whole archive before it clusters anything; warm runs ride
/// the shared blob cache and the snapshot index, and the footer reports what either one cost.
///
/// # Both cuts are defaults, not ceilings
///
/// `--clusters` and `--flows` raise what each section renders, on qanungo #16's finding and with its
/// semantics: the cut is on the rendering alone, every count is taken before it, zero is refused,
/// and the document states the number in force whenever it is not the default.
#[derive(Debug, Args)]
pub struct FlowsArgs {
    /// Narrow the reading to `<count><unit>` of history, with unit `h`, `d`, or `w`. Omitted, it
    /// covers the whole archive: "what do I keep doing" is a question about all of history, so this
    /// lane has no default window.
    #[arg(long = "last", value_parser = parse_window)]
    pub last: Option<Window>,

    /// How many repeated-request clusters to render, best first. The rest are counted and declared
    /// as held back rather than dropped silently, so raise this to read them; the document states
    /// the number in force whenever it is not the default.
    #[arg(
        long = "clusters",
        default_value_t = crate::flows::DEFAULT_CLUSTERS,
        value_parser = parse_cluster_cap,
    )]
    pub clusters: usize,

    /// How many multi-step flows to render, best first, on the same terms as `--clusters`.
    #[arg(
        long = "flows",
        default_value_t = crate::flows::DEFAULT_FLOWS,
        value_parser = parse_cluster_cap,
    )]
    pub flows: usize,

    #[command(flatten)]
    pub archive: ArchiveArgs,

    #[command(flatten)]
    pub redaction: RedactionArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// The dashboard lane (qanungo #5).
///
/// Flattens [`ArchiveArgs`] like every other lane, because the dashboard is the *same* fold behind
/// a different presentation and a `--patwari-url` that meant something else here would be a
/// dashboard describing a different archive from the report beside it.
///
/// It flattens [`RedactionArgs`] as of the evidence-excerpt slice, and the flag is **launch-time
/// only**. That is not an implementation convenience, it is the control: a per-request toggle a
/// browser could flip is not a redaction control, it is a redaction bypass with a query string, and
/// this surface is meant to be `--bind`-exposed to an unauthenticated tailnet. Every reader of a
/// given process gets the same scrub, the served payload states which one
/// ([`crate::dashboard`]), and startup says so out loud — loudly indeed when `--no-redact` meets a
/// routable address (see [`crate::dashboard_server::posture_line`]).
///
/// # Three windows, one page
///
/// The standup-and-cost slice adds two more windows, because the three lanes are not asking the
/// same question and a single `--last` would force two of them to answer the wrong one. A coaching
/// score wants a month; a bill wants a quarter; a standup wants a week a person can read to the end
/// of. So each section keeps its own lane's default — `--last 30d`, `--cost-last 12w`,
/// `--standup-last 7d` — and all three parse through the same window grammar, so a spelling means
/// same span wherever it is typed.
///
/// They are three flags rather than a per-section override of one: an operator who narrows the
/// coaching window has said nothing about what the bill should cover, and silently moving the other
/// two would be the page inventing an intent.
#[derive(Debug, Args)]
pub struct DashboardArgs {
    /// How far back to score, as `<count><unit>` with unit `h`, `d`, or `w`. The comparison window
    /// the trend arrows are drawn against is the equal-length one before it, as in `report`.
    #[arg(long = "last", default_value = "30d", value_parser = parse_window)]
    pub last: Window,

    /// How far back the cost section prices, as `<count><unit>` with unit `h`, `d`, or `w`.
    /// Defaults to `qanungo cost`'s own quarter so the page and the CLI answer the same question.
    #[arg(long = "cost-last", default_value = DEFAULT_COST_WINDOW, value_parser = parse_window)]
    pub cost_last: Window,

    /// How far back the standup section narrates, as `<count><unit>` with unit `h`, `d`, or `w`.
    /// Defaults to `qanungo standup`'s own week, for the same reason.
    #[arg(long = "standup-last", default_value = DEFAULT_STANDUP_WINDOW, value_parser = parse_window)]
    pub standup_last: Window,

    #[command(flatten)]
    pub archive: ArchiveArgs,

    /// Address to listen on. **Nothing here authenticates a caller.** A loopback address keeps the
    /// page on this machine; a tailnet address publishes it to every device on the tailnet, which
    /// is the intended way to read it from a phone or a TV and is safe exactly to the extent the
    /// tailnet is. Startup says which of the two is happening, in one line, whichever it is.
    #[arg(long, default_value = DEFAULT_DASHBOARD_BIND)]
    pub bind: SocketAddr,

    /// How often to re-sync and re-fold in the background, as `<count><unit>` with unit `s`, `m`,
    /// or `h`.
    #[arg(long, default_value = DEFAULT_DASHBOARD_REFRESH, value_parser = parse_refresh)]
    pub refresh: Refresh,

    #[command(flatten)]
    pub redaction: RedactionArgs,
}

/// A background refresh interval, kept in the spelling the operator typed so the provenance footer
/// can echo it back — the same discipline [`Window`] holds, for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refresh {
    text: String,
    interval: Duration,
}

impl Refresh {
    /// How long the refresh loop sleeps between folds.
    pub const fn interval(&self) -> Duration {
        self.interval
    }
}

impl fmt::Display for Refresh {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

/// Parses `30s`, `5m`, `1h`. A different grammar from [`parse_window`] on purpose: a refresh
/// interval is a number of seconds or minutes and a *window* deliberately is not, so the two
/// parsers accept disjoint units rather than one accepting both and inviting `--last 5m`.
fn parse_refresh(value: &str) -> Result<Refresh, String> {
    let (digits, unit) = value.split_at(
        value
            .find(|character: char| !character.is_ascii_digit())
            .ok_or_else(|| format!("`{value}` has no unit; try `5m`"))?,
    );
    let count: u64 = digits
        .parse()
        .map_err(|_| format!("`{value}` does not start with a count; try `5m`"))?;
    let seconds = match unit {
        "s" => Some(count),
        "m" => count.checked_mul(60),
        "h" => count.checked_mul(3600),
        other => return Err(format!("`{other}` is not an interval unit; use s, m, or h")),
    }
    .ok_or_else(|| format!("`{value}` is too long an interval"))?;
    let interval = Duration::from_secs(seconds);
    if interval < MIN_DASHBOARD_REFRESH {
        return Err(format!(
            "must be at least {}s — a warm three-lane refresh takes about 13 s against the archive, \
             and refreshing anywhere near that is a polling load on a LAN server rather than a \
             fresher dashboard",
            MIN_DASHBOARD_REFRESH.as_secs(),
        ));
    }
    Ok(Refresh {
        text: value.to_owned(),
        interval,
    })
}

/// A report window, kept in the spelling the operator typed so the report can echo it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    text: String,
    delta: TimeDelta,
}

impl Window {
    /// How far back the window reaches.
    pub const fn delta(&self) -> TimeDelta {
        self.delta
    }

    /// The instant the reported window opens.
    pub fn opens_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - self.delta
    }

    /// The instant the *comparison* window opens: one further window back, so the two are equal
    /// in length and adjacent. `None` when doubling the window would overflow — a report can then
    /// still be written, it simply carries no trend arrows.
    ///
    /// This lives here rather than in the report or the command because three places need the
    /// same boundary and they must agree exactly: the mirror lists from it, the fold partitions
    /// on it, and the report labels the two windows with it.
    pub fn comparison_opens_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        Some(now - self.delta.checked_mul(2)?)
    }
}

impl fmt::Display for Window {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

/// Refuses a worker count outside `1..=`[`crate::sync::MAX_CONCURRENCY`] rather than silently
/// clamping it: somebody who typed `--concurrency 64` has a belief about what the tool will do,
/// and the honest answer is that this client will not do it.
fn parse_concurrency(value: &str) -> Result<usize, String> {
    let count: usize = value
        .parse()
        .map_err(|_| format!("`{value}` is not a worker count"))?;
    if !(1..=crate::sync::MAX_CONCURRENCY).contains(&count) {
        return Err(format!(
            "must be between 1 and {} — Patwari is a LAN archive with a modest concurrency limit",
            crate::sync::MAX_CONCURRENCY,
        ));
    }
    Ok(count)
}

/// Refuses a result limit of zero rather than printing a search that can match nothing: somebody
/// who typed `--limit 0` has a belief about what the tool will do, and an empty ranking is not it.
fn parse_limit(value: &str) -> Result<usize, String> {
    let count: usize = value
        .parse()
        .map_err(|_| format!("`{value}` is not a result count"))?;
    if count == 0 {
        return Err("a limit of zero would print no matches".to_owned());
    }
    Ok(count)
}

/// Refuses a cluster cap of zero for [`parse_limit`]'s reason, spelled for what this one cuts: a
/// section rendering none of its clusters is not a shorter finding, it is a repository heading with
/// nothing under it but a line saying how much is hidden. Kept separate from [`parse_limit`] so each
/// message can name the thing the operator was actually setting.
fn parse_cluster_cap(value: &str) -> Result<usize, String> {
    let count: usize = value
        .parse()
        .map_err(|_| format!("`{value}` is not a cluster count"))?;
    if count == 0 {
        return Err("a cap of zero would render every cluster as hidden".to_owned());
    }
    Ok(count)
}

/// Parses `30d`, `12h`, `2w`. Deliberately not a general duration grammar: a coaching window is
/// always a round number of hours, days, or weeks, and a parser that also accepted `90m` would
/// invite windows too short for any of the metrics to mean anything.
fn parse_window(value: &str) -> Result<Window, String> {
    let (digits, unit) = value.split_at(
        value
            .find(|character: char| !character.is_ascii_digit())
            .ok_or_else(|| format!("`{value}` has no unit; try `30d`"))?,
    );
    let count: i64 = digits
        .parse()
        .map_err(|_| format!("`{value}` does not start with a count; try `30d`"))?;
    if count == 0 {
        return Err("a window of zero covers nothing".to_owned());
    }
    let delta = match unit {
        "h" => TimeDelta::try_hours(count),
        "d" => TimeDelta::try_days(count),
        "w" => TimeDelta::try_weeks(count),
        other => return Err(format!("`{other}` is not a window unit; use h, d, or w")),
    }
    .ok_or_else(|| format!("`{value}` is too long a window"))?;
    Ok(Window {
        text: value.to_owned(),
        delta,
    })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn parses_hour_day_and_week_windows() {
        assert_eq!(parse_window("12h").unwrap().delta(), TimeDelta::hours(12));
        assert_eq!(parse_window("30d").unwrap().delta(), TimeDelta::days(30));
        assert_eq!(parse_window("2w").unwrap().delta(), TimeDelta::weeks(2));
        assert_eq!(parse_window("30d").unwrap().to_string(), "30d");
    }

    /// The comparison window is the equal-length one immediately before the reported one: two
    /// adjacent halves of twice the window, with no gap and no overlap between them.
    #[test]
    fn the_comparison_window_is_adjacent_and_equal_in_length() {
        let now = DateTime::parse_from_rfc3339("2026-08-18T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let window = parse_window("30d").unwrap();
        let opens = window.opens_at(now);
        let compares = window.comparison_opens_at(now).unwrap();
        assert_eq!(opens, now - TimeDelta::days(30));
        assert_eq!(compares, now - TimeDelta::days(60));
        assert_eq!(opens - compares, now - opens);
    }

    #[test]
    fn rejects_windows_that_would_report_on_nothing() {
        for bad in ["", "d", "30", "30m", "0d", "-1d", "30days"] {
            assert!(parse_window(bad).is_err(), "`{bad}` must not parse");
        }
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn concurrency_past_the_archives_capacity_is_refused_not_clamped() {
        assert_eq!(parse_concurrency("1").unwrap(), 1);
        assert_eq!(
            parse_concurrency("8").unwrap(),
            crate::sync::MAX_CONCURRENCY
        );
        for bad in ["0", "9", "64", "-1", "many"] {
            assert!(parse_concurrency(bad).is_err(), "`{bad}` must be refused");
        }
        for command in ["report", "cost", "standup", "doctor", "flows", "dashboard"] {
            assert!(
                Cli::try_parse_from([
                    "qanungo",
                    command,
                    "--patwari-url",
                    "http://127.0.0.1:8080",
                    "--concurrency",
                    "64"
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn report_defaults_to_thirty_days_over_the_named_archive() {
        let Command::Report(args) = Cli::parse_from([
            "qanungo",
            "report",
            "--patwari-url",
            "http://127.0.0.1:8080",
        ])
        .command
        else {
            panic!("`report` parses as the report command");
        };
        assert_eq!(args.last.to_string(), "30d");
        assert_eq!(args.archive.patwari_url, "http://127.0.0.1:8080");
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(args.archive.cache_dir.is_none());
    }

    /// There is no default archive, on any lane: a run that names none is a usage error rather
    /// than a run against an address somebody else compiled in. The binary answers it with
    /// [`MISSING_PATWARI_URL`], which has to keep naming both ways of supplying one.
    ///
    /// The parse half is skipped when the caller's own shell exports `PATWARI_URL`, because then
    /// nothing *is* missing — the environment is the second way to give the flag a value, and clap
    /// reads it before it decides anything is absent.
    #[test]
    fn a_run_with_no_archive_url_is_a_usage_error() {
        if std::env::var_os("PATWARI_URL").is_none() {
            for command in ["report", "cost", "standup", "doctor", "flows", "dashboard"] {
                let error = Cli::try_parse_from(["qanungo", command])
                    .expect_err("no archive URL is a usage error");
                assert_eq!(
                    error.kind(),
                    clap::error::ErrorKind::MissingRequiredArgument
                );
                assert!(error.to_string().contains("--patwari-url"), "{command}");
            }
        }
        assert!(MISSING_PATWARI_URL.contains("--patwari-url"));
        assert!(MISSING_PATWARI_URL.contains("PATWARI_URL"));
    }

    /// The cost lane's default is a quarter spelled in the units the grammar actually has. The
    /// month unit the issue asked for is *not* accepted, here as anywhere else: `12w` is a window
    /// a reader can only read one way.
    #[test]
    fn cost_defaults_to_a_quarter_spelled_in_weeks() {
        let Command::Cost(args) =
            Cli::parse_from(["qanungo", "cost", "--patwari-url", "http://127.0.0.1:8080"]).command
        else {
            panic!("`cost` parses as the cost command");
        };
        assert_eq!(args.last.to_string(), "12w");
        assert_eq!(args.last.delta(), TimeDelta::weeks(12));
        assert_eq!(args.archive.patwari_url, "http://127.0.0.1:8080");
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "cost",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--last",
                "3m"
            ])
            .is_err()
        );
    }

    /// The standup lane narrates one window a person can read to the end of. The issue's `30d` is
    /// still typeable; it is simply not what a run with no flags means.
    #[test]
    fn standup_defaults_to_a_week_of_the_named_archive() {
        let Command::Standup(args) = Cli::parse_from([
            "qanungo",
            "standup",
            "--patwari-url",
            "http://127.0.0.1:8080",
        ])
        .command
        else {
            panic!("`standup` parses as the standup command");
        };
        assert_eq!(args.last.to_string(), DEFAULT_STANDUP_WINDOW);
        assert_eq!(args.last.delta(), TimeDelta::days(7));
        assert_eq!(args.archive.patwari_url, "http://127.0.0.1:8080");
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(args.archive.cache_dir.is_none());

        let Command::Standup(month) = Cli::parse_from([
            "qanungo",
            "standup",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--last",
            "30d",
        ])
        .command
        else {
            panic!("`standup` parses as the standup command");
        };
        assert_eq!(month.last.delta(), TimeDelta::days(30));
    }

    /// Every lane reaches the archive through the same flattened arguments, so a flag means the
    /// same thing wherever it is typed.
    #[test]
    fn every_lane_takes_the_same_archive_arguments() {
        let arguments = ["--patwari-url", "http://127.0.0.1:9", "--concurrency", "2"];
        let parse = |command: &str| {
            Cli::parse_from(
                ["qanungo", command]
                    .into_iter()
                    .chain(arguments)
                    .collect::<Vec<_>>(),
            )
            .command
        };
        let (
            Command::Report(report),
            Command::Cost(cost),
            Command::Standup(standup),
            Command::Doctor(doctor),
            Command::Dashboard(dashboard),
        ) = (
            parse("report"),
            parse("cost"),
            parse("standup"),
            parse("doctor"),
            parse("dashboard"),
        )
        else {
            panic!("each subcommand parses as itself");
        };
        for archive in [
            &cost.archive,
            &standup.archive,
            &doctor.archive,
            &dashboard.archive,
        ] {
            assert_eq!(report.archive.patwari_url, archive.patwari_url);
            assert_eq!(report.archive.concurrency, archive.concurrency);
        }
    }

    /// The redaction flags go live on exactly the lanes that render transcript content: `standup`,
    /// which prints archived prose, and `dashboard`, which serves evidence excerpts. `report` and
    /// `cost` still refuse them, and that refusal is the point — a redaction flag on a document
    /// carrying no content would invite a reader to trust a control that is not doing anything.
    #[test]
    fn only_the_lanes_that_render_content_take_the_redaction_flags() {
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "standup",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--no-redact"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "dashboard",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--no-redact"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "dashboard",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--filter-profanity"
            ])
            .is_ok()
        );
        // Ask renders matched summary prose, so it flattens the flags like the other two content
        // lanes — with a query, since the query is required.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "ask",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "payments",
                "--no-redact"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "ask",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "payments",
                "--filter-profanity"
            ])
            .is_ok()
        );
        // Doctor quotes the repeated instruction itself, so it takes them too.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "doctor",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--no-redact"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "doctor",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--filter-profanity"
            ])
            .is_ok()
        );
        // Flows quotes the repeated request and every step of a flow — the third verbatim surface.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "flows",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--no-redact"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "flows",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--filter-profanity"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "report",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--no-redact"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "cost",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--filter-profanity"
            ])
            .is_err()
        );
    }

    /// Flows is the third lane with no default window, for `ask` and `doctor`'s reason: "what do I
    /// keep doing" is a lifetime question. It takes no positional argument either — there is
    /// nothing to search *for*, only history to read.
    #[test]
    fn flows_reads_all_history_until_a_window_narrows_it() {
        let Command::Flows(args) =
            Cli::parse_from(["qanungo", "flows", "--patwari-url", "http://127.0.0.1:8080"]).command
        else {
            panic!("`flows` parses as the flows command");
        };
        assert!(args.last.is_none(), "no window means all of history");
        assert_eq!(args.clusters, crate::flows::DEFAULT_CLUSTERS);
        assert_eq!(args.flows, crate::flows::DEFAULT_FLOWS);
        assert_eq!(args.archive.patwari_url, "http://127.0.0.1:8080");
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(args.archive.cache_dir.is_none());
        assert!(!args.redaction.no_redact, "the scrub is the default");

        let Command::Flows(scoped) = Cli::parse_from([
            "qanungo",
            "flows",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--last",
            "4w",
        ])
        .command
        else {
            panic!("`flows` parses as the flows command");
        };
        assert_eq!(
            scoped.last.expect("a window was given").delta(),
            TimeDelta::weeks(4),
        );

        // The grammar is the crate's, so the month unit is refused here too, and the lane takes no
        // positional argument.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "flows",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--last",
                "5m"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "flows",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "payments"
            ])
            .is_err()
        );
    }

    /// Both of the flows lane's cuts are numbers the operator can move, refused at zero on
    /// `--clusters-per-repo`'s reasoning: a section that can show no finding is not a shorter
    /// document, it is a heading with a footnote under it.
    #[test]
    fn flows_takes_two_caps_and_refuses_zero_on_either() {
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "flows",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--clusters",
                "0"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "flows",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--flows",
                "0"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "flows",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--clusters",
                "many"
            ])
            .is_err()
        );

        let Command::Flows(args) = Cli::parse_from([
            "qanungo",
            "flows",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--clusters",
            "50",
            "--flows",
            "5",
        ])
        .command
        else {
            panic!("`flows` parses as the flows command");
        };
        assert_eq!(args.clusters, 50);
        assert_eq!(args.flows, 5);

        // They are this lane's own cuts: neither `doctor` nor any other lane learned them, and this
        // lane did not learn `doctor`'s.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "doctor",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--clusters",
                "50"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "flows",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--clusters-per-repo",
                "50"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "report",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--flows",
                "5"
            ])
            .is_err()
        );
    }

    /// Doctor is the second lane with no default window, for `ask`'s reason: "have I been repeating
    /// myself" is a lifetime question. It takes no positional argument at all — there is nothing to
    /// search *for*, only history to read.
    #[test]
    fn doctor_reads_all_history_until_a_window_narrows_it() {
        let Command::Doctor(args) = Cli::parse_from([
            "qanungo",
            "doctor",
            "--patwari-url",
            "http://127.0.0.1:8080",
        ])
        .command
        else {
            panic!("`doctor` parses as the doctor command");
        };
        assert!(args.last.is_none(), "no window means all of history");
        assert_eq!(
            args.clusters_per_repo,
            crate::doctor::DEFAULT_CLUSTERS_PER_REPOSITORY,
        );
        assert_eq!(args.archive.patwari_url, "http://127.0.0.1:8080");
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert!(args.archive.cache_dir.is_none());
        assert!(!args.redaction.no_redact, "the scrub is the default");

        let Command::Doctor(scoped) = Cli::parse_from([
            "qanungo",
            "doctor",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--last",
            "4w",
        ])
        .command
        else {
            panic!("`doctor` parses as the doctor command");
        };
        assert_eq!(
            scoped.last.map(|window| window.delta()),
            Some(TimeDelta::weeks(4)),
        );

        // The window shares the one grammar, and there is no query to give it.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "doctor",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--last",
                "5m"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "doctor",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "payments"
            ])
            .is_err()
        );
    }

    /// What `doctor` builds out of the flags is what every other rendering lane builds: typing
    /// nothing scrubs secrets and leaves swearing alone.
    #[test]
    fn the_doctor_lane_redacts_by_default_and_stops_only_when_told_to() {
        let redactor = |flags: &[&str]| {
            let Command::Doctor(args) = Cli::parse_from(
                [
                    "qanungo",
                    "doctor",
                    "--patwari-url",
                    "http://127.0.0.1:8080",
                ]
                .into_iter()
                .chain(flags.iter().copied())
                .collect::<Vec<_>>(),
            )
            .command
            else {
                panic!("`doctor` parses as the doctor command");
            };
            args.redaction.redactor()
        };
        assert_eq!(redactor(&[]), crate::redaction::Redactor::new());
        assert!(redactor(&[]).redacts_secrets());
        assert!(!redactor(&[]).filters_profanity());
        assert!(!redactor(&["--no-redact"]).redacts_secrets());
        assert!(redactor(&["--filter-profanity"]).filters_profanity());
    }

    /// Ask is the one lane with no default window: a run with just a query searches the whole
    /// archive, and `--last` is what narrows it. The query itself is required — a search with no
    /// terms is a usage error, not an empty run.
    #[test]
    fn ask_searches_all_history_until_a_window_narrows_it() {
        let Command::Ask(args) = Cli::parse_from([
            "qanungo",
            "ask",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "payments API",
        ])
        .command
        else {
            panic!("`ask` parses as the ask command");
        };
        assert_eq!(args.query, "payments API");
        assert!(args.last.is_none(), "no window means all of history");
        assert_eq!(args.limit, DEFAULT_ASK_LIMIT);
        assert!(!args.verbatim, "the escalation is opt-in");
        assert_eq!(args.archive.patwari_url, "http://127.0.0.1:8080");

        let Command::Ask(scoped) = Cli::parse_from([
            "qanungo",
            "ask",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "payments",
            "--last",
            "30d",
        ])
        .command
        else {
            panic!("`ask` parses as the ask command");
        };
        assert_eq!(
            scoped.last.map(|window| window.delta()),
            Some(TimeDelta::days(30))
        );

        // No query is a usage error, and the window still shares the one grammar.
        assert!(
            Cli::try_parse_from(["qanungo", "ask", "--patwari-url", "http://127.0.0.1:8080"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "ask",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "x",
                "--last",
                "5m"
            ])
            .is_err()
        );
    }

    /// `--verbatim` is a plain opt-in switch that composes with everything else the lane takes: a
    /// window, a limit, and the redaction flags. It takes no value — how far the escalation
    /// reaches is `--limit`'s business, and a second knob for the same bound would let the two
    /// disagree.
    #[test]
    fn verbatim_is_an_opt_in_switch_that_composes_with_the_other_ask_flags() {
        let Command::Ask(args) = Cli::parse_from([
            "qanungo",
            "ask",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "payments",
            "--verbatim",
            "--last",
            "12w",
            "--limit",
            "3",
            "--no-redact",
        ])
        .command
        else {
            panic!("`ask` parses as the ask command");
        };
        assert!(args.verbatim);
        assert_eq!(args.limit, 3);
        assert_eq!(
            args.last.map(|window| window.delta()),
            Some(TimeDelta::weeks(12))
        );
        assert!(args.redaction.no_redact);

        // It is a flag, not an option, and it belongs to this lane alone.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "ask",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "payments",
                "--verbatim",
                "5"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "standup",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--verbatim"
            ])
            .is_err()
        );
    }

    /// A result limit is refused at zero rather than clamped, and defaults to ten.
    #[test]
    fn ask_refuses_a_limit_of_zero() {
        assert_eq!(parse_limit("1").unwrap(), 1);
        assert_eq!(parse_limit("25").unwrap(), 25);
        for bad in ["0", "-1", "lots", ""] {
            assert!(parse_limit(bad).is_err(), "`{bad}` must be refused");
        }
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "ask",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "x",
                "--limit",
                "0"
            ])
            .is_err()
        );
        let Command::Ask(args) = Cli::parse_from([
            "qanungo",
            "ask",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "x",
            "--limit",
            "3",
        ])
        .command
        else {
            panic!("`ask` parses as the ask command");
        };
        assert_eq!(args.limit, 3);
    }

    /// The doctor lane's per-repository cut is a number the operator can move, refused at zero on
    /// [`parse_limit`]'s rule and belonging to this lane alone (qanungo #16).
    #[test]
    fn doctor_takes_a_cluster_cap_and_refuses_zero() {
        assert_eq!(parse_cluster_cap("1").unwrap(), 1);
        assert_eq!(parse_cluster_cap("50").unwrap(), 50);
        for bad in ["0", "-1", "all", ""] {
            assert!(parse_cluster_cap(bad).is_err(), "`{bad}` must be refused");
        }
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "doctor",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--clusters-per-repo",
                "0"
            ])
            .is_err()
        );

        let Command::Doctor(args) = Cli::parse_from([
            "qanungo",
            "doctor",
            "--patwari-url",
            "http://127.0.0.1:8080",
            "--clusters-per-repo",
            "50",
        ])
        .command
        else {
            panic!("`doctor` parses as the doctor command");
        };
        assert_eq!(args.clusters_per_repo, 50);

        // It is the doctor's own cut, not a limit the other lanes learned.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "ask",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "x",
                "--clusters-per-repo",
                "50"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "report",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--clusters-per-repo",
                "50"
            ])
            .is_err()
        );
    }

    /// The dashboard's scrub is decided once, on the command line, and typing nothing is the safe
    /// reading here exactly as it is for `standup`. There is no `--redact`, and — pinned in
    /// `tests/dashboard.rs` over the wire — no query string that means one.
    #[test]
    fn the_dashboard_redacts_by_default_and_stops_only_when_told_to() {
        let redactor = |flags: &[&str]| {
            let Command::Dashboard(args) = Cli::parse_from(
                [
                    "qanungo",
                    "dashboard",
                    "--patwari-url",
                    "http://127.0.0.1:8080",
                ]
                .into_iter()
                .chain(flags.iter().copied())
                .collect::<Vec<_>>(),
            )
            .command
            else {
                panic!("`dashboard` parses as the dashboard command");
            };
            args.redaction.redactor()
        };
        assert_eq!(redactor(&[]), crate::redaction::Redactor::new());
        assert!(redactor(&[]).redacts_secrets());
        assert!(!redactor(&[]).filters_profanity());
        assert!(!redactor(&["--no-redact"]).redacts_secrets());
        assert!(redactor(&["--filter-profanity"]).filters_profanity());
    }

    /// A run with no flags serves the loopback default over thirty days, refreshing on the named
    /// interval — the same window `report` defaults to, because the dashboard is that report's
    /// numbers under a different presentation.
    #[test]
    fn the_dashboard_defaults_to_loopback_thirty_days_and_the_named_interval() {
        let Command::Dashboard(args) = Cli::parse_from([
            "qanungo",
            "dashboard",
            "--patwari-url",
            "http://127.0.0.1:8080",
        ])
        .command
        else {
            panic!("`dashboard` parses as the dashboard command");
        };
        assert_eq!(args.last.to_string(), "30d");
        assert_eq!(args.last.delta(), TimeDelta::days(30));
        assert_eq!(args.archive.patwari_url, "http://127.0.0.1:8080");
        assert_eq!(args.archive.concurrency, crate::sync::DEFAULT_CONCURRENCY);
        assert_eq!(args.bind.to_string(), DEFAULT_DASHBOARD_BIND);
        assert!(args.bind.ip().is_loopback());
        assert_eq!(args.refresh.to_string(), DEFAULT_DASHBOARD_REFRESH);
        assert_eq!(args.refresh.interval(), Duration::from_secs(300));
    }

    /// The dashboard folds three lanes over three windows, and each one defaults to the window its
    /// own command defaults to — so a page nobody configured shows the same three answers the three
    /// CLI runs with no flags would print.
    #[test]
    fn the_dashboards_three_windows_default_to_their_own_lanes_defaults() {
        let Command::Dashboard(dashboard) = Cli::parse_from([
            "qanungo",
            "dashboard",
            "--patwari-url",
            "http://127.0.0.1:8080",
        ])
        .command
        else {
            panic!("`dashboard` parses as the dashboard command");
        };
        let (Command::Report(report), Command::Cost(cost), Command::Standup(standup)) = (
            Cli::parse_from([
                "qanungo",
                "report",
                "--patwari-url",
                "http://127.0.0.1:8080",
            ])
            .command,
            Cli::parse_from(["qanungo", "cost", "--patwari-url", "http://127.0.0.1:8080"]).command,
            Cli::parse_from([
                "qanungo",
                "standup",
                "--patwari-url",
                "http://127.0.0.1:8080",
            ])
            .command,
        ) else {
            panic!("each lane parses as itself");
        };
        assert_eq!(dashboard.last, report.last, "30d, as `report` scores");
        assert_eq!(dashboard.cost_last, cost.last, "12w, as `cost` prices");
        assert_eq!(
            dashboard.standup_last, standup.last,
            "7d, as `standup` narrates",
        );
        assert_eq!(dashboard.cost_last.delta(), TimeDelta::weeks(12));
        assert_eq!(dashboard.standup_last.delta(), TimeDelta::days(7));
    }

    /// The two new flags share `--last`'s grammar exactly: the same units accepted, the same units
    /// refused, and the same refusal of a window that covers nothing. A second grammar would be a
    /// second definition of what `12w` means on one command line.
    #[test]
    fn the_extra_window_flags_share_the_window_grammar() {
        let windows = |flags: &[&str]| {
            let Command::Dashboard(args) = Cli::parse_from(
                [
                    "qanungo",
                    "dashboard",
                    "--patwari-url",
                    "http://127.0.0.1:8080",
                ]
                .into_iter()
                .chain(flags.iter().copied())
                .collect::<Vec<_>>(),
            )
            .command
            else {
                panic!("`dashboard` parses as the dashboard command");
            };
            (args.last, args.cost_last, args.standup_last)
        };

        // Each flag moves its own window and neither of the others.
        let (last, cost, standup) = windows(&["--cost-last", "4w"]);
        assert_eq!(cost.delta(), TimeDelta::weeks(4));
        assert_eq!(last.to_string(), "30d");
        assert_eq!(standup.to_string(), DEFAULT_STANDUP_WINDOW);

        let (last, cost, standup) = windows(&["--standup-last", "48h"]);
        assert_eq!(standup.delta(), TimeDelta::hours(48));
        assert_eq!(last.to_string(), "30d");
        assert_eq!(cost.to_string(), DEFAULT_COST_WINDOW);

        // All three at once, each keeping the spelling it was typed in so provenance can echo it.
        let (last, cost, standup) =
            windows(&["--last", "14d", "--cost-last", "2w", "--standup-last", "3d"]);
        assert_eq!(last.to_string(), "14d");
        assert_eq!(cost.to_string(), "2w");
        assert_eq!(standup.to_string(), "3d");

        for flag in ["--cost-last", "--standup-last"] {
            for bad in ["", "d", "30", "30m", "0d", "-1d", "30days", "5s"] {
                assert!(
                    Cli::try_parse_from([
                        "qanungo",
                        "dashboard",
                        "--patwari-url",
                        "http://127.0.0.1:8080",
                        flag,
                        bad
                    ])
                    .is_err(),
                    "{flag} {bad:?} must not parse",
                );
            }
            for good in ["12h", "30d", "12w"] {
                assert!(
                    Cli::try_parse_from([
                        "qanungo",
                        "dashboard",
                        "--patwari-url",
                        "http://127.0.0.1:8080",
                        flag,
                        good
                    ])
                    .is_ok(),
                    "{flag} {good} is a window",
                );
            }
        }

        // And they belong to the dashboard alone: the single-lane commands each have one window.
        for command in ["report", "cost", "standup"] {
            assert!(
                Cli::try_parse_from([
                    "qanungo",
                    command,
                    "--patwari-url",
                    "http://127.0.0.1:8080",
                    "--cost-last",
                    "4w"
                ])
                .is_err()
            );
            assert!(
                Cli::try_parse_from([
                    "qanungo",
                    command,
                    "--patwari-url",
                    "http://127.0.0.1:8080",
                    "--standup-last",
                    "3d"
                ])
                .is_err()
            );
        }
    }

    /// A routable bind parses — that is the tailnet case, and refusing it here would be refusing
    /// the whole point of the lane. What it must not do is happen silently; the posture line is
    /// [`crate::dashboard_server`]'s half of that bargain.
    #[test]
    fn a_routable_bind_parses_because_the_tailnet_is_the_point() {
        let bind = |address: &str| {
            let Command::Dashboard(args) = Cli::parse_from([
                "qanungo",
                "dashboard",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--bind",
                address,
            ])
            .command
            else {
                panic!("`dashboard` parses as the dashboard command");
            };
            args.bind
        };
        assert!(!bind("0.0.0.0:8878").ip().is_loopback());
        assert!(!bind("192.0.2.1:9000").ip().is_loopback());
        assert!(bind("[::1]:8878").ip().is_loopback());
        // A malformed address is a usage error before a socket is ever opened.
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "dashboard",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--bind",
                "nowhere"
            ])
            .is_err()
        );
    }

    /// The refresh grammar is disjoint from the window grammar: an interval is seconds, minutes,
    /// or hours, and a window is hours, days, or weeks. Neither parser accepts the other's units,
    /// so `--last 5m` and `--refresh 30d` are both refused rather than silently meaning something.
    #[test]
    fn the_refresh_grammar_and_the_window_grammar_do_not_overlap() {
        assert_eq!(
            parse_refresh("60s").unwrap().interval(),
            Duration::from_secs(60)
        );
        assert_eq!(
            parse_refresh("5m").unwrap().interval(),
            Duration::from_secs(300)
        );
        assert_eq!(
            parse_refresh("2h").unwrap().interval(),
            Duration::from_secs(7200)
        );
        assert_eq!(parse_refresh("5m").unwrap().to_string(), "5m");

        for bad in ["", "m", "5", "5d", "5w", "0s", "-1m", "5 minutes"] {
            assert!(parse_refresh(bad).is_err(), "`{bad}` must not parse");
        }
        assert!(parse_window("5m").is_err());
        assert!(parse_window("60s").is_err());
    }

    /// The floor is refused, not clamped: somebody who typed `--refresh 5s` has a belief about
    /// what this tool will do to the archive, and the honest answer is that it will not.
    #[test]
    fn a_refresh_faster_than_the_floor_is_refused_not_clamped() {
        let refused = parse_refresh("5s").expect_err("under the floor");
        assert!(refused.contains("at least 60s"), "{refused}");
        assert!(refused.contains("polling load"), "{refused}");
        assert!(parse_refresh("59s").is_err());
        assert!(parse_refresh("60s").is_ok());
        assert!(
            Cli::try_parse_from([
                "qanungo",
                "dashboard",
                "--patwari-url",
                "http://127.0.0.1:8080",
                "--refresh",
                "5s"
            ])
            .is_err()
        );
    }

    /// [`RedactionArgs`] is exercised through the same stand-in the redaction lane pinned it with,
    /// so the flag surface stays pinned independently of which commands happen to flatten it.
    #[derive(Debug, Parser)]
    struct RenderingLane {
        #[command(flatten)]
        redaction: RedactionArgs,
    }

    #[test]
    fn the_redaction_flags_are_well_formed() {
        RenderingLane::command().debug_assert();
    }

    /// What `standup` builds out of the flags is the same redactor the stand-in builds out of
    /// them: typing nothing scrubs secrets and leaves swearing alone.
    #[test]
    fn the_standup_lane_redacts_by_default_and_stops_only_when_told_to() {
        let redactor = |flags: &[&str]| {
            let Command::Standup(args) = Cli::parse_from(
                [
                    "qanungo",
                    "standup",
                    "--patwari-url",
                    "http://127.0.0.1:8080",
                ]
                .into_iter()
                .chain(flags.iter().copied())
                .collect::<Vec<_>>(),
            )
            .command
            else {
                panic!("`standup` parses as the standup command");
            };
            args.redaction.redactor()
        };
        assert_eq!(redactor(&[]), crate::redaction::Redactor::new());
        assert!(redactor(&[]).redacts_secrets());
        assert!(!redactor(&[]).filters_profanity());
        assert!(!redactor(&["--no-redact"]).redacts_secrets());
        assert!(redactor(&["--filter-profanity"]).filters_profanity());
    }

    /// Typing nothing must be the safe reading: secrets scrubbed, profanity left alone.
    #[test]
    fn rendering_defaults_to_redacting_secrets_and_not_filtering_profanity() {
        let lane = RenderingLane::parse_from(["lane"]);
        assert!(!lane.redaction.no_redact);
        assert!(!lane.redaction.filter_profanity);
        let redactor = lane.redaction.redactor();
        assert!(redactor.redacts_secrets());
        assert!(!redactor.filters_profanity());
        assert_eq!(redactor, crate::redaction::Redactor::new());
    }

    /// The two passes are switched independently on the command line as well as in the library:
    /// four flag combinations, four distinct redactors.
    #[test]
    fn the_two_redaction_flags_move_independently() {
        let cases = [
            (vec![], true, false),
            (vec!["--no-redact"], false, false),
            (vec!["--filter-profanity"], true, true),
            (vec!["--no-redact", "--filter-profanity"], false, true),
        ];
        for (flags, secrets, profanity) in cases {
            let lane = RenderingLane::parse_from(["lane"].into_iter().chain(flags.iter().copied()));
            let redactor = lane.redaction.redactor();
            assert_eq!(redactor.redacts_secrets(), secrets, "{flags:?}");
            assert_eq!(redactor.filters_profanity(), profanity, "{flags:?}");
        }
    }

    /// There is no `--redact`: redaction is not a thing to remember to ask for, and losing it has
    /// to be said out loud.
    #[test]
    fn there_is_no_flag_that_turns_redaction_on() {
        assert!(RenderingLane::try_parse_from(["lane", "--redact"]).is_err());
        assert!(RenderingLane::try_parse_from(["lane", "--no-filter-profanity"]).is_err());
    }
}
