//! Markdown rendering for the cost lane, and the redaction line that rendering enforces.
//!
//! # The redaction line (hard)
//!
//! A cost report renders **aggregates and archive-stated identifiers — nothing else**. Token
//! counts, message counts, session counts, dollars; the model ids, billing modifiers, and
//! repository names the archive itself recorded; and the same clamped `error.code` Gaps
//! discipline [`crate::report`] already applies. No command strings, no error text, no message
//! excerpts, no file paths, no user or assistant prose. Not truncated, not elided: zero verbatim
//! transcript content.
//!
//! This is a property of *construction*, exactly as it is for the coaching report. Nothing in
//! this module ever receives a transcript string, because [`crate::cost`] never reads one: it
//! folds `assistant_meta` — the model, the message id, the token figures — and does not touch a
//! record's classification at all, so the user text, the assistant text, and the tool arguments
//! are not merely filtered out downstream, they are never lifted off the stream.
//!
//! The identifiers it does render are structured billing metadata rather than anything a person
//! or a model wrote: `message.model` is the harness's own field, `speed` / `service_tier` /
//! `inference_geo` are the API's, and `repository` comes from Patwari's session projection. They
//! are still peer-supplied strings reaching a rendered document, so each one goes through
//! [`crate::format::identifier`] first, on the same reasoning that clamps `error.code` — an
//! archive that is confused, compromised, or not Patwari at all does not get to put characters of
//! its choosing into a report. That clamp lives in [`crate::format`] rather than here because the
//! Gaps section both lanes share applies it too.
//!
//! Evidence remains a content hash: a reader who wants to know *which* session spent the money
//! fetches the transcript from the archive and reads it in full.
//!
//! # Honest dollars
//!
//! Every figure is labelled "at Anthropic API list prices", because that is what it is. The
//! archive knows the model and the tokens; it does not know the account's plan, its discounts, or
//! whether a subscription covered the request. Copilot sessions get token volumes and no money at
//! all — see [`crate::cost::BillingSignal`] — and no credit-equivalent is offered in their place,
//! since a transcript cannot say which of Copilot's two billing regimes the account was on.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::cli::Window;
use crate::cost::CostTotals;
use crate::format::{self, identifier};
use crate::pricing::PRICE_TABLE_REVISION;
use crate::report::{SkippedNote, change, stamp};
use crate::sync::SyncStats;

/// What a cost run cost, folded into the footer.
#[derive(Debug, Clone)]
pub struct CostInstrumentation {
    pub sync: SyncStats,
    /// Wall-time of the fold alone.
    pub fold_elapsed: Duration,
    /// Sessions folded for the reported window.
    pub sessions_folded: usize,
    /// Sessions folded for the comparison window as well — the price of the delta, kept separate
    /// for the same reason the coaching report keeps it separate.
    pub comparison_sessions_folded: usize,
    /// Transcript records read across both windows.
    pub records_read: u64,
    /// Decompressed transcript bytes the fold read, across both windows.
    pub bytes_folded: u64,
    pub patwari_url: String,
    pub cache_root: PathBuf,
}

/// Everything one cost report is rendered from.
pub struct CostReport<'a> {
    pub window: &'a Window,
    pub generated_at: DateTime<Utc>,
    pub totals: &'a CostTotals,
    /// The equal-length window immediately before it. `None` when no comparison window was asked
    /// for at all — a window so long that doubling it overflows — which is a different thing from
    /// one that came back empty, and the report says which.
    pub previous: Option<&'a CostTotals>,
    pub skipped: &'a [SkippedNote],
    pub instrumentation: &'a CostInstrumentation,
}

impl CostReport<'_> {
    /// Renders the cost report as Markdown.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Cost report — last {}\n", self.window);
        self.render_window(&mut out);
        self.render_priced(&mut out);
        self.render_copilot(&mut out);
        self.render_flagged(&mut out);
        self.render_gaps(&mut out);
        self.render_footer(&mut out);
        out
    }

    fn render_window(&self, out: &mut String) {
        let _ = writeln!(
            out,
            "Sessions archived since {} (UTC), folded at {}.",
            stamp(self.window.opens_at(self.generated_at)),
            stamp(self.generated_at),
        );
        let totals = self.totals;
        let sessions = totals.priceable_sessions
            + totals.token_only_sessions
            + totals.no_signal_sessions.values().sum::<usize>();
        if sessions == 0 {
            out.push_str(
                "\nNo archived session fell in this window, so there is nothing to price yet.\n",
            );
            return;
        }
        let mut parts = Vec::new();
        if totals.priceable_sessions > 0 {
            parts.push(format!(
                "{} priced in dollars (claude-code)",
                totals.priceable_sessions
            ));
        }
        if totals.token_only_sessions > 0 {
            parts.push(format!(
                "{} counted in tokens only (copilot)",
                totals.token_only_sessions
            ));
        }
        for (agent, count) in &totals.no_signal_sessions {
            parts.push(format!(
                "{count} recording no per-message usage at all ({})",
                identifier(agent),
            ));
        }
        let _ = writeln!(out, "\n**{sessions} sessions** — {}.", parts.join(", "));
    }

    /// The dollars section: the total, the models behind it, what caching saved, and where the
    /// money went by repository.
    fn render_priced(&self, out: &mut String) {
        out.push_str("\n## Cost (claude-code, at API list prices)\n\n");
        let totals = self.totals;
        if !totals.priced_anything() {
            out.push_str(
                "No claude-code session in this window carried usage this build could price.",
            );
            // "Nothing at all" is a claim about the whole document, not about the dollars: a
            // window of copilot sessions has a populated token table further down, and a harness
            // that records no usage is itself something the archive said. Printing the stronger
            // sentence directly above either of them would be contradicted by the next heading.
            let recorded_elsewhere = totals.flagged.any()
                || !totals.copilot.is_empty()
                || !totals.no_signal_sessions.is_empty();
            out.push_str(if recorded_elsewhere {
                " What the archive did record is below.\n"
            } else {
                " The archive recorded no billing signal here at all.\n"
            });
            return;
        }
        let _ = writeln!(
            out,
            "**{}** across {} sessions and {} billed messages.\n",
            format::dollars(totals.priced.dollars),
            totals.priceable_sessions,
            totals.priced.tokens.messages,
        );
        let _ = writeln!(
            out,
            "These are **Anthropic API list prices** at the rates in force when each session was \
             archived (price table {PRICE_TABLE_REVISION}, sourced in \
             `docs/pricing-sources-{PRICE_TABLE_REVISION}.md`). The archive knows which model \
             answered and how many tokens it read and wrote; it does not know the account's \
             billing plan, so this is what the usage would list at and not necessarily what was \
             charged for it.",
        );
        self.render_delta(out);
        self.render_models(out);
        self.render_cache_economics(out);
        self.render_repositories(out);
    }

    /// The window-over-window move on the total.
    ///
    /// Drawn only where **both** windows priced something, on exactly the coaching report's rule:
    /// an arrow against a window that could not measure would be reporting the archive's shape as
    /// spending. The direction is direction only — ▲ is more money, which is not a verdict.
    fn render_delta(&self, out: &mut String) {
        let comparison_opens_at = self.window.comparison_opens_at(self.generated_at);
        let (Some(previous), Some(comparison_opens_at)) = (self.previous, comparison_opens_at)
        else {
            out.push_str(
                "\nNo comparison: this window is too long to place an equal-length one before \
                 it.\n",
            );
            return;
        };
        if !previous.priced_anything() {
            let _ = writeln!(
                out,
                "\nNo comparison: the archive holds no priced session between {} and {} (UTC).",
                stamp(comparison_opens_at),
                stamp(self.window.opens_at(self.generated_at)),
            );
            return;
        }
        let _ = writeln!(
            out,
            "\nMovement against the equal-length window before it, {} → {} (UTC): **{}**, from {} \
             across {} sessions to this window's {}. ▲ is more money, not a worse window. Both \
             windows are cut on archive time — the clock that selected them — so a long-lived \
             transcript resumed across the boundary is archived again and appears in this one \
             only, carrying its earlier spend with it.",
            stamp(comparison_opens_at),
            stamp(self.window.opens_at(self.generated_at)),
            change(
                Some(self.totals.priced.dollars),
                Some(previous.priced.dollars),
                format::dollars,
            ),
            format::dollars(previous.priced.dollars),
            previous.priceable_sessions,
            self.totals.priceable_sessions,
        );
    }

    fn render_models(&self, out: &mut String) {
        out.push_str("\n| Model | Messages | Input | Output | Cache write 5m | Cache write 1h | ");
        out.push_str(
            "Cache read | Cost |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        );
        let mut rows: Vec<_> = self.totals.by_model.iter().collect();
        // Most expensive first: the reason to read this table is to find where the money went.
        rows.sort_by(|(left_model, left), (right_model, right)| {
            right
                .dollars
                .total_cmp(&left.dollars)
                .then_with(|| left_model.cmp(right_model))
        });
        for (model, priced) in rows {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} | {} |",
                identifier(model),
                priced.tokens.messages,
                format::tokens(priced.tokens.input),
                format::tokens(priced.tokens.output),
                format::tokens(priced.tokens.cache_write_5m),
                format::tokens(priced.tokens.cache_write_1h),
                format::tokens(priced.tokens.cache_read),
                format::dollars(priced.dollars),
            );
        }
        let fast = self
            .totals
            .by_model
            .values()
            .map(|priced| priced.fast_messages)
            .sum::<u64>();
        if fast > 0 {
            let _ = writeln!(
                out,
                "\nFast mode: {fast} of those messages ran in it, priced at its own tier rather \
                 than the model's base row — which is why a model's realized rate can sit above \
                 that row.",
            );
        }
        // The deduplication note lives with the table it is a property of rather than in the
        // flagged section, because a run that dropped duplicates did the *right* thing — and a
        // clean window would otherwise elide the one number showing that it happened at all.
        if self.totals.duplicate_records > 0 {
            let _ = writeln!(
                out,
                "\nDeduplication dropped {} further records across this window that repeated a \
                 message already counted: one API message reaches a transcript as several records \
                 — the assistant text, then each of its tool calls — repeating its usage verbatim, \
                 so summing records instead of messages would have inflated every figure above.",
                self.totals.duplicate_records,
            );
        }
        let thinking = self.totals.priced.tokens.thinking;
        if thinking > 0 {
            let _ = writeln!(
                out,
                "\n{} of the output tokens above were extended thinking. That is a share of \
                 output, not a category beside it, so it is already inside the Output column and \
                 inside the cost.",
                format::tokens(thinking),
            );
        }
    }

    /// What the prompt cache saved, stated as the difference it actually made.
    fn render_cache_economics(&self, out: &mut String) {
        let priced = &self.totals.priced;
        if priced.tokens.cache_read == 0 {
            return;
        }
        let _ = writeln!(
            out,
            "\n**Cache economics** — {} tokens were served from the prompt cache for {}. Sending \
             the same tokens as fresh input would have cost {}, so caching saved {} of a {} bill \
             that would otherwise have been {}.",
            format::tokens(priced.tokens.cache_read),
            format::dollars(priced.cache_read_dollars),
            format::dollars(priced.cache_read_at_input_rate),
            format::dollars(priced.cache_saving()),
            format::dollars(priced.dollars),
            format::dollars(priced.dollars + priced.cache_saving()),
        );
        if priced.tokens.cache_write_priceable() > 0 {
            let _ = writeln!(
                out,
                "The writes that filled it — {} tokens at the 5-minute tier, {} at the 1-hour — \
                 are already inside the total above, so the saving quoted here is the read side \
                 alone and is not net of them.",
                format::tokens(priced.tokens.cache_write_5m),
                format::tokens(priced.tokens.cache_write_1h),
            );
        }
    }

    /// Where the money went, by the repository the archive recorded for each session.
    ///
    /// There is deliberately no by-machine cut: Patwari's session projection carries `project`,
    /// `repository`, `branch`, and `source_agent_version`, and no hostname. Splitting by machine
    /// would mean inventing one, so the cut is simply absent rather than approximated.
    fn render_repositories(&self, out: &mut String) {
        if self.totals.by_repository.is_empty() {
            return;
        }
        out.push_str("\n### By repository\n\n");
        out.push_str(
            "As the archive recorded it on each session's latest snapshot. A session captured \
             outside a checkout has no repository and is its own row rather than being folded \
             into somebody else's.\n\n",
        );
        out.push_str("| Repository | Messages | Tokens | Cost |\n| --- | ---: | ---: | ---: |\n");
        let mut rows: Vec<_> = self.totals.by_repository.iter().collect();
        rows.sort_by(|(left_name, left), (right_name, right)| {
            right
                .dollars
                .total_cmp(&left.dollars)
                .then_with(|| left_name.cmp(right_name))
        });
        for (repository, priced) in rows {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                match repository {
                    Some(name) => format!("`{}`", identifier(name)),
                    None => "(no repository)".to_owned(),
                },
                priced.tokens.messages,
                format::tokens(priced.tokens.total()),
                format::dollars(priced.dollars),
            );
        }
    }

    /// Copilot's side: volumes, the limitation that makes them volumes, and no money.
    fn render_copilot(&self, out: &mut String) {
        if self.totals.copilot.is_empty() {
            return;
        }
        out.push_str("\n## Token volumes (copilot)\n\n");
        out.push_str(
            "Copilot records **output tokens only** — one figure per assistant message, with no \
             per-message input, cache, or tier — and a transcript does not say whether the \
             account was on premium requests or on usage-based credits, so these sessions are \
             reported in tokens and carry no dollars, no credit estimate, and no premium-request \
             count.\n\n",
        );
        out.push_str("| Model | Messages | Output tokens |\n| --- | ---: | ---: |\n");
        let mut rows: Vec<_> = self.totals.copilot.iter().collect();
        rows.sort_by(|(left_model, left), (right_model, right)| {
            right
                .output
                .cmp(&left.output)
                .then_with(|| left_model.cmp(right_model))
        });
        for (model, volumes) in rows {
            let _ = writeln!(
                out,
                "| {} | {} | {} |",
                match model {
                    Some(name) => format!("`{}`", identifier(name)),
                    None => "(no model recorded)".to_owned(),
                },
                volumes.messages,
                format::tokens(volumes.output),
            );
        }
    }

    /// Everything the fold counted and did not turn into money, with the reason and the size.
    /// Elided entirely when there is nothing to flag.
    fn render_flagged(&self, out: &mut String) {
        let flagged = &self.totals.flagged;
        if !flagged.any() {
            return;
        }
        out.push_str("\n## Unpriced / flagged\n\n");
        out.push_str(
            "Tokens the archive recorded that this build did not turn into dollars, and the usage \
             it priced under a caveat. None of it is estimated, interpolated, or rounded into the \
             total above.\n\n",
        );
        if flagged.synthetic.messages > 0 {
            let _ = writeln!(
                out,
                "- **Unbilled (synthetic)** — {} messages, {} tokens ({} output). Claude Code's \
                 `<synthetic>` placeholder for messages it generated locally; no vendor billed \
                 them, so they are excluded from dollars by construction rather than for want of \
                 a price.",
                flagged.synthetic.messages,
                format::tokens(flagged.synthetic.total()),
                format::tokens(flagged.synthetic.output),
            );
        }
        for (reason, tally) in &flagged.unpriced {
            let _ = writeln!(
                out,
                "- **Unpriced** — {} messages, {} tokens: {}.",
                tally.messages,
                format::tokens(tally.total()),
                reason.detail(identifier),
            );
        }
        if flagged.untiered_cache_writes > 0 {
            let _ = writeln!(
                out,
                "- **Cache writes with no tier** — {} tokens across {} messages stated only as a \
                 total, with neither the 5-minute nor the 1-hour bucket present. The two tiers \
                 bill at different multiples of input, so such a write has no rate and is left out \
                 of the total rather than charged at an assumed tier.",
                format::tokens(flagged.untiered_cache_writes),
                flagged.untiered_cache_write_messages,
            );
        }
        if flagged.undeduplicatable.any() {
            let undeduplicatable = &flagged.undeduplicatable;
            let _ = writeln!(
                out,
                "- **Undeduplicatable usage** — {} records ({} carrying no message id, {} past a \
                 session's message-id cap) carrying {} tokens were summed *per record*. One API \
                 message can reach a transcript as several records repeating its usage verbatim, \
                 so this share of the total may be counted more than once — over-counting rather \
                 than dropping real spend, and named here either way.",
                undeduplicatable.records(),
                undeduplicatable.without_a_message_id,
                undeduplicatable.past_the_id_cap,
                format::tokens(undeduplicatable.tokens),
            );
        }
    }

    fn render_gaps(&self, out: &mut String) {
        if self.skipped.is_empty() {
            return;
        }
        out.push_str("\n## Gaps\n\n");
        out.push_str("These archived sessions contributed nothing to the fold:\n\n");
        for note in self.skipped {
            let _ = writeln!(out, "- {} — {}", note.count, note.reason);
        }
    }

    fn render_footer(&self, out: &mut String) {
        let instrumentation = self.instrumentation;
        out.push_str("\n---\n\n");
        out.push_str(
            "This report renders aggregates and the model, modifier, and repository identifiers \
             the archive itself recorded — never transcript content. To read a session in full, \
             ask the archive for its artifact and fetch the `content_url` that comes back (the \
             filter takes the bare digest, without the `sha256:` prefix):\n\n",
        );
        let _ = writeln!(
            out,
            "    GET {}/api/v1/artifacts?original_sha256=<hash>\n",
            instrumentation.patwari_url.trim_end_matches('/'),
        );
        let comparison = match instrumentation.comparison_sessions_folded {
            0 => String::new(),
            count => format!(" (+{count} for the comparison window)"),
        };
        let _ = writeln!(
            out,
            "_Instrumentation — sync {} · fold {} · {} sessions{comparison} · {} records · {} \
             folded · cache {} hits / {} misses ({} transferred) · price table {} · archive {} · \
             cache {}_",
            format::elapsed(instrumentation.sync.elapsed),
            format::elapsed(instrumentation.fold_elapsed),
            instrumentation.sessions_folded,
            instrumentation.records_read,
            format::bytes(instrumentation.bytes_folded),
            instrumentation.sync.cache_hits,
            instrumentation.sync.cache_misses,
            format::bytes(instrumentation.sync.bytes_transferred),
            PRICE_TABLE_REVISION,
            instrumentation.patwari_url,
            display_path(&instrumentation.cache_root),
        );
    }
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use munshi_transcript::Source;

    use super::*;
    use crate::cost::{CostFold, SessionCost, fold_cost};
    use crate::format::INVALID_IDENTIFIER;

    fn window(spelling: &str) -> Window {
        let crate::cli::Command::Cost(args) =
            crate::cli::Cli::parse_from(["qanungo", "cost", "--last", spelling]).command
        else {
            panic!("the cost subcommand parses");
        };
        args.last
    }

    fn instrumentation() -> CostInstrumentation {
        CostInstrumentation {
            sync: SyncStats {
                sessions_listed: 1,
                cache_hits: 1,
                cache_misses: 0,
                bytes_transferred: 0,
                elapsed: Duration::from_millis(90),
            },
            fold_elapsed: Duration::from_millis(4),
            sessions_folded: 1,
            comparison_sessions_folded: 0,
            records_read: 12,
            bytes_folded: 4096,
            patwari_url: "http://127.0.0.1:8080".to_owned(),
            cache_root: PathBuf::from("/tmp/qanungo"),
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn claude_session(
        model: &str,
        message_id: &str,
        usage: &str,
        repository: Option<&str>,
    ) -> SessionCost {
        let record = format!(
            r#"{{"type":"assistant","uuid":"{message_id}-r","timestamp":"2026-08-01T10:00:00.000Z","message":{{"role":"assistant","id":"{message_id}","model":"{model}","content":[{{"type":"text","text":"x"}}],"usage":{usage}}}}}"#
        );
        SessionCost {
            source_hash: "0".repeat(64),
            source_agent: "claude-code".to_owned(),
            repository: repository.map(ToOwned::to_owned),
            archived_at: Some(at("2026-08-10T00:00:00Z")),
            fold: fold_cost(Source::ClaudeCode, 2, record.as_bytes()).unwrap(),
            bytes_folded: 4096,
        }
    }

    fn render(totals: &CostTotals, previous: Option<&CostTotals>) -> String {
        let window = window("12w");
        let instrumentation = instrumentation();
        CostReport {
            window: &window,
            generated_at: at("2026-08-17T12:00:00Z"),
            totals,
            previous,
            skipped: &[],
            instrumentation: &instrumentation,
        }
        .render()
    }

    /// One million output tokens of Opus 5 and a million cached reads: the numbers are round so
    /// the rendered dollars can be read against the price table by eye.
    fn priced_window() -> CostTotals {
        CostTotals::fold(&[claude_session(
            "claude-opus-5",
            "msg_1",
            r#"{"input_tokens":0,"output_tokens":1000000,"cache_read_input_tokens":1000000,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":100000}}"#,
            Some("surdy/qanungo"),
        )])
    }

    #[test]
    fn an_empty_window_still_reports_and_still_instruments() {
        let markdown = render(&CostTotals::default(), None);
        assert!(markdown.starts_with("# Cost report — last 12w"));
        assert!(markdown.contains("nothing to price yet"));
        assert!(markdown.contains("_Instrumentation —"));
        assert!(markdown.contains("price table 2026-08-23"));
        assert!(!markdown.contains("## Unpriced / flagged"));
        assert!(!markdown.contains("## Token volumes"));
    }

    /// The headline the whole lane exists for, with the token volumes behind it and the honest
    /// label on the money.
    #[test]
    fn the_total_carries_the_models_behind_it_and_the_list_price_caveat() {
        let markdown = render(&priced_window(), None);
        // 1M output at $25 + 1M cache read at $0.50 + 100k 1h cache write at $10.
        assert!(
            markdown.contains("**$26.50** across 1 sessions"),
            "{markdown}"
        );
        assert!(
            markdown.contains("**Anthropic API list prices**"),
            "{markdown}"
        );
        assert!(
            markdown.contains("does not know the account's billing plan"),
            "{markdown}"
        );
        assert!(
            markdown.contains("| `claude-opus-5` | 1 | 0 | 1.0M | 0 | 100.0k | 1.0M | $26.50 |"),
            "{markdown}"
        );
    }

    /// The cache line says what caching actually saved, in the money the reader is looking at.
    #[test]
    fn the_cache_line_prices_the_reads_against_what_they_would_have_cost_as_input() {
        let markdown = render(&priced_window(), None);
        assert!(
            markdown.contains(
                "**Cache economics** — 1.0M tokens were served from the prompt cache for $0.50"
            ),
            "{markdown}"
        );
        assert!(
            markdown.contains("would have cost $5.00, so caching saved $4.50"),
            "{markdown}"
        );
    }

    /// The repository cut, including the row for sessions the archive recorded no repository for
    /// — which is a real state and is named rather than merged.
    #[test]
    fn the_repository_cut_names_the_sessions_with_no_repository() {
        let totals = CostTotals::fold(&[
            claude_session(
                "claude-sonnet-5",
                "msg_1",
                r#"{"output_tokens":1000000}"#,
                Some("surdy/qanungo"),
            ),
            claude_session(
                "claude-sonnet-5",
                "msg_2",
                r#"{"output_tokens":500000}"#,
                None,
            ),
        ]);
        let markdown = render(&totals, None);
        assert!(markdown.contains("### By repository"), "{markdown}");
        assert!(
            markdown.contains("| `surdy/qanungo` | 1 | 1.0M | $10.00 |"),
            "{markdown}"
        );
        assert!(
            markdown.contains("| (no repository) | 1 | 500.0k | $5.00 |"),
            "{markdown}"
        );
    }

    /// An arrow appears where both windows priced something, points the right way, and carries
    /// the size of the move in money.
    #[test]
    fn a_delta_is_drawn_only_against_a_window_that_priced_something() {
        let previous = CostTotals::fold(&[claude_session(
            "claude-sonnet-5",
            "msg_0",
            r#"{"output_tokens":1000000}"#,
            None,
        )]);
        let markdown = render(&priced_window(), Some(&previous));
        assert!(
            markdown.contains("**▲ $16.50**, from $10.00 across 1 sessions to this window's 1"),
            "{markdown}"
        );

        // A comparison window that priced nothing gets a sentence, never an arrow against zero.
        let empty = CostTotals::default();
        let markdown = render(&priced_window(), Some(&empty));
        assert!(
            markdown.contains("No comparison: the archive holds no priced session between"),
            "{markdown}"
        );
        assert!(!markdown.contains('▲'), "{markdown}");

        // And with no comparison window at all, the report says which of the two it is.
        let markdown = render(&priced_window(), None);
        assert!(
            markdown.contains("too long to place an equal-length one before it"),
            "{markdown}"
        );
    }

    /// Copilot gets volumes and the one sentence that says why it gets nothing more.
    #[test]
    fn copilot_volumes_state_the_limitation_and_carry_no_money() {
        let transcript = r#"{"type":"assistant.message","timestamp":"2026-08-01T10:00:00.000Z","data":{"content":"one","messageId":"m1","model":"claude-opus-4.8","outputTokens":128}}"#;
        let session = SessionCost {
            source_agent: "copilot-cli".to_owned(),
            fold: fold_cost(Source::Copilot, 2, transcript.as_bytes()).unwrap(),
            ..claude_session("unused", "unused", r#"{"output_tokens":0}"#, None)
        };
        let markdown = render(&CostTotals::fold(&[session]), None);
        assert!(
            markdown.contains("## Token volumes (copilot)"),
            "{markdown}"
        );
        assert!(markdown.contains("**output tokens only**"), "{markdown}");
        assert!(
            markdown.contains("no dollars, no credit estimate, and no premium-request count"),
            "{markdown}"
        );
        assert!(
            markdown.contains("| `claude-opus-4.8` | 1 | 128 |"),
            "{markdown}"
        );
        assert!(
            !markdown.contains("$"),
            "copilot must not be dollarized: {markdown}"
        );
    }

    /// Everything the fold refused to price is named, counted, and kept apart from everything
    /// else it refused to price for a different reason.
    #[test]
    fn the_flagged_section_names_each_kind_of_refusal_separately() {
        let totals = CostTotals::fold(&[
            claude_session("<synthetic>", "msg_1", r#"{"output_tokens":500}"#, None),
            claude_session("claude-opus-9", "msg_2", r#"{"output_tokens":700}"#, None),
            claude_session(
                "claude-opus-5",
                "msg_3",
                r#"{"input_tokens":10,"cache_creation_input_tokens":4096}"#,
                None,
            ),
        ]);
        let markdown = render(&totals, None);
        assert!(markdown.contains("## Unpriced / flagged"), "{markdown}");
        assert!(
            markdown.contains("**Unbilled (synthetic)** — 1 messages, 500 tokens"),
            "{markdown}"
        );
        assert!(
            markdown.contains(
                "**Unpriced** — 1 messages, 700 tokens: no price row for model `claude-opus-9`"
            ),
            "{markdown}"
        );
        assert!(
            markdown.contains("**Cache writes with no tier** — 4.1k tokens across 1 messages"),
            "{markdown}"
        );
    }

    /// The clamp itself is pinned in [`crate::format`]; what matters here is that it is actually
    /// applied on the way into the tables rather than merely available to be.
    #[test]
    fn a_hostile_model_id_cannot_break_out_of_a_table_cell() {
        let totals = CostTotals::fold(&[claude_session(
            "evil | model",
            "msg_1",
            r#"{"output_tokens":10}"#,
            None,
        )]);
        let markdown = render(&totals, None);
        assert!(!markdown.contains("evil | model"), "{markdown}");
        assert!(markdown.contains(INVALID_IDENTIFIER), "{markdown}");
    }

    /// The footer is the fold-cost record, so it carries every quantity a later decision would be
    /// argued from — including the price-table revision two reports must share to be comparable.
    #[test]
    fn the_footer_carries_every_instrumented_quantity() {
        let markdown = render(&priced_window(), None);
        let footer = markdown
            .lines()
            .find(|line| line.starts_with("_Instrumentation"))
            .expect("the footer is always present");
        assert!(footer.contains("sync 90 ms"), "{footer}");
        assert!(footer.contains("fold 4 ms"), "{footer}");
        assert!(footer.contains("1 sessions"), "{footer}");
        assert!(footer.contains("12 records"), "{footer}");
        assert!(footer.contains("4.0 KiB folded"), "{footer}");
        assert!(footer.contains("cache 1 hits / 0 misses"), "{footer}");
        assert!(footer.contains("price table 2026-08-23"), "{footer}");
    }

    /// "No billing signal at all" is a claim about the whole document. A window of copilot
    /// sessions has a populated token table further down it, so the stronger sentence must not
    /// appear above one — and the same goes for a harness that records nothing, which is itself
    /// something the archive told us.
    #[test]
    fn a_window_with_no_dollars_does_not_deny_signal_it_is_about_to_print() {
        let transcript = r#"{"type":"assistant.message","timestamp":"2026-08-01T10:00:00.000Z","data":{"content":"one","messageId":"m1","model":"claude-opus-4.8","outputTokens":128}}"#;
        let copilot = SessionCost {
            source_agent: "copilot-cli".to_owned(),
            fold: fold_cost(Source::Copilot, 2, transcript.as_bytes()).unwrap(),
            ..claude_session("unused", "unused", r#"{"output_tokens":0}"#, None)
        };
        let markdown = render(&CostTotals::fold(&[copilot]), None);
        assert!(
            markdown.contains(
                "No claude-code session in this window carried usage this build \
                               could price. What the archive did record is below."
            ),
            "{markdown}"
        );
        assert!(
            !markdown.contains("no billing signal here at all"),
            "the copilot table two headings down contradicts that: {markdown}"
        );
        assert!(
            markdown.contains("| `claude-opus-4.8` | 1 | 128 |"),
            "{markdown}"
        );

        // A harness that records nothing is also something the archive said.
        let codex = SessionCost {
            source_agent: "codex-cli".to_owned(),
            fold: CostFold::default(),
            ..claude_session("unused", "unused", r#"{"output_tokens":0}"#, None)
        };
        let markdown = render(&CostTotals::fold(&[codex]), None);
        assert!(
            !markdown.contains("no billing signal here at all"),
            "{markdown}"
        );

        // And a genuinely empty window still says the strong thing, because there it is true.
        let markdown = render(&CostTotals::default(), None);
        assert!(
            markdown.contains("The archive recorded no billing signal here at all."),
            "{markdown}"
        );
    }

    /// A harness that records no usage is named rather than left out, so a reader can tell a
    /// silent harness from an absent one.
    #[test]
    fn a_harness_with_no_usage_signal_is_named_in_the_window_line() {
        let codex = SessionCost {
            source_agent: "codex-cli".to_owned(),
            fold: CostFold::default(),
            ..claude_session("unused", "unused", r#"{"output_tokens":0}"#, None)
        };
        let markdown = render(&CostTotals::fold(&[codex]), None);
        assert!(
            markdown.contains("1 recording no per-message usage at all (codex-cli)"),
            "{markdown}"
        );
    }

    #[test]
    fn gaps_are_stated_rather_than_swallowed() {
        let window = window("12w");
        let instrumentation = instrumentation();
        let totals = priced_window();
        let markdown = CostReport {
            window: &window,
            generated_at: at("2026-08-17T12:00:00Z"),
            totals: &totals,
            previous: None,
            skipped: &[SkippedNote {
                count: 2,
                reason: "claude-code: snapshot has no transcript artifact".to_owned(),
            }],
            instrumentation: &instrumentation,
        }
        .render();
        assert!(markdown.contains("## Gaps"), "{markdown}");
        assert!(
            markdown.contains("- 2 — claude-code: snapshot has no transcript artifact"),
            "{markdown}"
        );
    }
}
