//! Shared human-readable renderings of the numbers a report is made of.
//!
//! Findings and the report body must agree on how a duration, a rate, or a byte count reads, and
//! these are the only strings a redacted report contains besides tool names and digests — so
//! they live in one place rather than being re-improvised per call site.

use std::time::Duration;

use chrono::TimeDelta;

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

    #[test]
    fn elapsed_switches_unit_at_a_second() {
        assert_eq!(elapsed(Duration::from_millis(340)), "340 ms");
        assert_eq!(elapsed(Duration::from_millis(1500)), "1.50 s");
    }
}
