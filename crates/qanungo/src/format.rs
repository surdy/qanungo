//! Shared human-readable renderings of the numbers a report is made of.
//!
//! Findings and the report body must agree on how a duration, a rate, or a byte count reads, and
//! these are the only strings a redacted report contains besides tool names and digests — so
//! they live in one place rather than being re-improvised per call site.
//!
//! [`identifier`] is the exception that proves the rule: it is not a rendering of a number but the
//! single clamp every *archive-stated* string passes through on its way into a document, and it
//! lives here for the same reason — one rule, one place, applied by both lanes.
//!
//! [`logged`] is the second such clamp, for the second rendering surface a peer's bytes can reach:
//! the operator's terminal. The two are different functions rather than one with a flag, because a
//! document and a log line want opposite things from a hostile value — see [`logged`].

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

/// Longest peer-stated value a log line renders before cutting it short.
///
/// Generous for any real request target and far under anything that turns one connection into a
/// screenful. The bound is on *rendered* characters, so an escape sequence cannot expand past it.
pub const MAX_LOGGED_CHARS: usize = 120;

/// Marks a value [`logged`] cut short. Ours, appended after the peer's bytes have been escaped, so
/// it can never be mistaken for something the peer sent.
pub const LOGGED_TRUNCATED: &str = "…";

/// A peer-stated value — a request method, a request target — rendered onto the operator's
/// terminal.
///
/// A terminal is a rendering surface with an interpreter behind it, so it gets the same treatment
/// every other rendering surface here gets: **a peer does not choose bytes on it**. Anything that
/// is not printable ASCII is escaped rather than passed through, which is the whole of the
/// property — an ESC, a BEL, a newline, or a stray C1 byte cannot set a window title, ring a bell,
/// hide what came before it, or forge a second log line, because none of them survives as itself.
///
/// This matters here and not only in principle: the dashboard exists to be `--bind`-exposed to an
/// unauthenticated tailnet, so its access log is written from bytes an unknown caller chose.
///
/// # Why this escapes where [`identifier`] replaces
///
/// [`identifier`] replaces a hostile value wholesale, because a prefix of arbitrary text is still
/// arbitrary text in a document sworn to carry none. A log line's job is the opposite: it exists to
/// say **what a peer actually asked for**, and the request worth reading is precisely the strange
/// one. Substituting a placeholder there would delete the diagnosis at the exact moment somebody
/// went looking for it. So nothing is dropped — it is escaped, which is lossless and legible — and
/// an over-long value is truncated with [`LOGGED_TRUNCATED`] rather than replaced, because the
/// first hundred characters of a hostile target are evidence, not contamination.
pub fn logged(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    // The bound counts the characters the *peer* sent, not the ones they rendered as: an escape
    // expands one character into six, and a ceiling on the output could be spent by a sixth of a
    // target.
    for character in value.chars().take(MAX_LOGGED_CHARS) {
        // Space is the one non-graphic character allowed through: it drives nothing, and a
        // request line's own grammar means neither field can contain one anyway.
        if character.is_ascii_graphic() || character == ' ' {
            out.push(character);
        } else {
            out.extend(character.escape_default());
        }
    }
    if value.chars().nth(MAX_LOGGED_CHARS).is_some() {
        out.push_str(LOGGED_TRUNCATED);
    }
    out
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

    /// The property the whole clamp exists for: nothing that can drive a terminal survives it. A
    /// peer that puts an ESC, a BEL, or a newline in a request line does not get to set a window
    /// title, ring a bell, or forge a second log line.
    #[test]
    fn nothing_that_can_drive_a_terminal_survives_a_log_line() {
        let hostile = "/\u{1b}]0;pwned\u{7}/fake\nSPOOFED-LOG-LINE\r/x";
        let rendered = logged(hostile);
        for byte in ['\u{1b}', '\u{7}', '\n', '\r'] {
            assert!(!rendered.contains(byte), "{byte:?} survived: {rendered:?}");
        }
        assert!(
            rendered
                .chars()
                .all(|character| character.is_ascii_graphic() || character == ' '),
            "{rendered:?}",
        );
        // Escaped, never dropped: the strange request is the one worth reading, so every byte of
        // it is still legible in the log.
        assert!(rendered.contains("\\u{1b}"), "{rendered:?}");
        assert!(rendered.contains("\\u{7}"), "{rendered:?}");
        assert!(rendered.contains("\\n"), "{rendered:?}");
        assert!(rendered.contains("SPOOFED-LOG-LINE"), "{rendered:?}");
        assert_eq!(rendered.lines().count(), 1, "one value, one line");
    }

    /// An ordinary request costs the clamp nothing, which is what keeps the log readable enough to
    /// be read at all.
    #[test]
    fn an_ordinary_request_passes_through_a_log_line_unchanged() {
        for ordinary in [
            "GET",
            "/",
            "/api/data",
            "/api/events?last=99",
            "/index.html",
            "/a-path-with_every~ordinary.char/(and)/[some]/{odd}/ones!",
        ] {
            assert_eq!(logged(ordinary), ordinary);
        }
        assert_eq!(logged(""), "");
    }

    /// Non-ASCII is escaped rather than passed through — a C1 control arrives as UTF-8 and would
    /// otherwise reach the terminal as one — and the bound counts the peer's characters, so an
    /// expanding escape cannot outrun it.
    #[test]
    fn a_log_line_is_bounded_by_the_peers_characters_not_by_its_own_escapes() {
        assert_eq!(logged("/caf\u{e9}"), "/caf\\u{e9}");
        assert_eq!(logged("/\u{9b}["), "/\\u{9b}[");

        let long = "/".to_owned() + &"a".repeat(MAX_LOGGED_CHARS * 2);
        let rendered = logged(&long);
        assert!(rendered.ends_with(LOGGED_TRUNCATED), "{rendered}");
        assert_eq!(
            rendered.chars().count(),
            MAX_LOGGED_CHARS + LOGGED_TRUNCATED.chars().count(),
        );

        // Every character escapes to six, and the bound still counts the hundred and twenty the
        // peer sent rather than the seven hundred they rendered as.
        let escapes = "\u{1b}".repeat(MAX_LOGGED_CHARS * 2);
        let rendered = logged(&escapes);
        assert!(rendered.ends_with(LOGGED_TRUNCATED));
        assert_eq!(
            rendered.matches("\\u{1b}").count(),
            MAX_LOGGED_CHARS,
            "{rendered}",
        );

        // A value exactly at the bound is not truncated, and says nothing about being cut.
        let exact = "a".repeat(MAX_LOGGED_CHARS);
        assert_eq!(logged(&exact), exact);
    }

    #[test]
    fn elapsed_switches_unit_at_a_second() {
        assert_eq!(elapsed(Duration::from_millis(340)), "340 ms");
        assert_eq!(elapsed(Duration::from_millis(1500)), "1.50 s");
    }
}
