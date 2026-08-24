//! Shared human-readable renderings of the numbers a report is made of.
//!
//! Findings and the report body must agree on how a duration, a rate, or a byte count reads, and
//! these are the only strings a redacted report contains besides tool names and digests — so
//! they live in one place rather than being re-improvised per call site.
//!
//! [`identifier`] is the exception that proves the rule: it is not a rendering of a number but the
//! single clamp every *archive-stated* string passes through on its way into a document, and it
//! lives here for the same reason — one rule, one place, applied by both lanes.

use std::time::Duration;

use chrono::TimeDelta;

/// Longest archive-stated identifier a report will render before replacing it wholesale.
/// Comfortably over any real model id, harness label, or repository name, and far under anything
/// that could turn a table cell or a Gaps line into a paragraph.
pub const MAX_IDENTIFIER_CHARS: usize = 64;

/// Stands in for an archive-stated identifier that is not shaped like one.
pub const INVALID_IDENTIFIER: &str = "invalid-identifier";

/// An archive-stated identifier — a model id, a billing modifier, a harness label, a repository
/// name — rendered only when it is shaped like one.
///
/// These are the only strings a report lifts out of somebody else's data, and they end up in a
/// document sworn to carry no upstream free text, so they are clamped on the same reasoning that
/// clamps Patwari's `error.code` (see [`crate::patwari`]): a peer that is confused, compromised,
/// or not the archive at all does not get to choose characters in a report. A value carrying a
/// control character, a newline, a table pipe, or a backtick — or one longer than
/// [`MAX_IDENTIFIER_CHARS`] — is replaced wholesale rather than truncated, because a prefix of
/// arbitrary text is still arbitrary text.
///
/// Deliberately permissive about everything else, including non-ASCII: `<synthetic>` and a
/// repository named in a script other than Latin are both real identifiers, and the point of the
/// clamp is the rendering surface, not the alphabet.
pub fn identifier(value: &str) -> String {
    let usable = !value.is_empty()
        && value.chars().count() <= MAX_IDENTIFIER_CHARS
        && value
            .chars()
            .all(|character| !character.is_control() && !"|`".contains(character));
    if usable {
        value.to_owned()
    } else {
        INVALID_IDENTIFIER.to_owned()
    }
}

/// A wall-clock span as `6h 12m`, `47m`, or `38s`. Coarse on purpose: a coaching report is not
/// a profiler, and a span rendered to the second invites reading precision into a number whose
/// inputs are transcript timestamps.
pub fn span(span: TimeDelta) -> String {
    let seconds = span.num_seconds().max(0);
    let (hours, minutes) = (seconds / 3600, (seconds % 3600) / 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

/// A fraction as a whole-number percentage.
pub fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

/// A ratio to one decimal place.
pub fn ratio(value: f64) -> String {
    format!("{value:.1}")
}

/// A byte count in binary units.
pub fn bytes(count: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A dollar figure, to the cent, with thousands separators.
///
/// A non-zero amount smaller than a cent renders as `<$0.01` rather than as `$0.00`: a cost
/// report that printed zero for real spend would be inviting the reader to conclude a model was
/// free. An amount that *is* zero prints `$0.00`, because that is a different claim.
pub fn dollars(amount: f64) -> String {
    if amount != 0.0 && amount.abs() < 0.005 {
        return if amount < 0.0 {
            "-<$0.01".to_owned()
        } else {
            "<$0.01".to_owned()
        };
    }
    let sign = if amount < 0.0 { "-" } else { "" };
    let cents = format!("{:.2}", amount.abs());
    let (whole, fraction) = cents.split_once('.').unwrap_or((cents.as_str(), "00"));
    format!("{sign}${}.{fraction}", grouped(whole))
}

/// Digits grouped in threes, so a six-figure token count or a four-figure bill is readable at a
/// glance.
fn grouped(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A token count in decimal units — `912`, `345.6k`, `26.2M`, `1.4B`.
///
/// Decimal rather than the binary units [`bytes`] uses, because a token is not a byte and a
/// reader comparing a count against a per-million rate is doing decimal arithmetic. Counts below
/// a thousand are printed exactly: a report that rounded 4 tokens to `0.0k` would be hiding the
/// only interesting thing about them.
pub fn tokens(count: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "k")];
    for (scale, suffix) in UNITS {
        // A unit is chosen by what the value will *round* to, not by the raw scale. Rounding to a
        // tenth can carry a count up into the next unit — 999,950 tokens is "1000.0k" to the
        // arithmetic and 1.0M to a reader — and a four-digit mantissa beside a unit prefix reads
        // as a bug. The boundary is therefore `0.99995 * scale`, which is `scale - scale/20000`.
        if count as f64 >= scale as f64 - scale as f64 / 20_000.0 {
            return format!("{:.1}{suffix}", count as f64 / scale as f64);
        }
    }
    count.to_string()
}

/// An elapsed wall-time, in the unit that keeps it readable.
pub fn elapsed(elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis < 1000 {
        format!("{millis} ms")
    } else {
        format!("{:.2} s", elapsed.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_read_coarsely() {
        assert_eq!(span(TimeDelta::seconds(38)), "38s");
        assert_eq!(span(TimeDelta::minutes(47)), "47m");
        assert_eq!(span(TimeDelta::minutes(372)), "6h 12m");
        assert_eq!(span(TimeDelta::seconds(-5)), "0s");
    }

    #[test]
    fn rates_and_ratios_round_predictably() {
        assert_eq!(percent(0.375), "38%");
        assert_eq!(percent(0.0), "0%");
        assert_eq!(ratio(12.34), "12.3");
    }

    #[test]
    fn byte_counts_use_binary_units() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2.0 KiB");
        assert_eq!(bytes(3 * 1024 * 1024), "3.0 MiB");
    }

    /// Money is rendered to the cent, and real spend below a cent says so rather than rounding
    /// itself out of existence.
    #[test]
    fn dollars_round_to_the_cent_and_never_hide_real_spend() {
        assert_eq!(dollars(0.0), "$0.00");
        assert_eq!(dollars(12.345), "$12.35");
        assert_eq!(dollars(1234.5), "$1,234.50");
        assert_eq!(dollars(1_234_567.891), "$1,234,567.89");
        assert_eq!(dollars(0.004), "<$0.01");
        assert_eq!(dollars(0.005), "$0.01");
        assert_eq!(dollars(-2.5), "-$2.50");
        assert_eq!(dollars(-0.001), "-<$0.01");
    }

    #[test]
    fn token_counts_use_decimal_units() {
        assert_eq!(tokens(4), "4");
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_500), "1.5k");
        assert_eq!(tokens(26_200_000), "26.2M");
        assert_eq!(tokens(1_400_000_000), "1.4B");
    }

    /// A count that rounds up into the next unit is promoted rather than printed with a
    /// four-digit mantissa — `1000.0k` is a rendering bug wearing a plausible face.
    #[test]
    fn a_count_that_rounds_into_the_next_unit_is_promoted() {
        assert_eq!(tokens(999_949), "999.9k");
        assert_eq!(tokens(999_950), "1.0M");
        assert_eq!(tokens(1_000_000), "1.0M");
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_000), "1.0k");
        assert_eq!(tokens(999_949_999), "999.9M");
        assert_eq!(tokens(999_950_000), "1.0B");
    }

    /// The clamp is a redaction control, not tidiness: it is the only place a peer's bytes reach a
    /// rendered document, in either lane.
    #[test]
    fn an_archive_stated_identifier_is_rendered_only_when_it_is_shaped_like_one() {
        for good in [
            "claude-opus-5",
            "<synthetic>",
            "surdy/qanungo",
            "claude-opus-4.8",
            "copilot-cli",
            "ansh/परियोजना",
        ] {
            assert_eq!(identifier(good), good);
        }
        for hostile in [
            "",
            "pipes | break | tables",
            "back`tick",
            "new\nline",
            "null\0byte",
            &"a".repeat(MAX_IDENTIFIER_CHARS + 1),
        ] {
            assert_eq!(identifier(hostile), INVALID_IDENTIFIER, "{hostile:?}");
        }
        let at_the_limit = "a".repeat(MAX_IDENTIFIER_CHARS);
        assert_eq!(identifier(&at_the_limit), at_the_limit);
    }

    #[test]
    fn elapsed_switches_unit_at_a_second() {
        assert_eq!(elapsed(Duration::from_millis(340)), "340 ms");
        assert_eq!(elapsed(Duration::from_millis(1500)), "1.50 s");
    }
}
