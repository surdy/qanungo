//! The redaction lane's done-bar: run the secrets pass over the transcripts this machine has
//! actually mirrored, and report only how many times each pattern fired.
//!
//! Fixtures prove a pattern *can* fire. They cannot tell you whether it fires ten thousand times a
//! day on real text, which is the only question that matters for a precision-first pattern set: a
//! pattern that eats prose looks perfect against a canary and ruins every report it touches. Every
//! qanungo lane verifies against production before it is called done (the rule thresholds were
//! pinned this way in #14, the price table's `inference_geo` reading in #12), and this is that
//! check for #8.
//!
//! Ignored by default and run by hand:
//!
//! ```text
//! cargo test --test redaction_scan -- --ignored --nocapture
//! ```
//!
//! # What it is allowed to print
//!
//! Counts. Per pattern, and a total. **Never a matched string, never a session excerpt, never a
//! digest of a session that fired** — a scan whose output has to be read carefully before it is
//! pasted anywhere is a scan that has re-created the problem the module exists to solve. That is
//! not a matter of care in this file: [`RedactionReport`] has no way to hand over anything else.
//!
//! # It reads the local cache, never the archive
//!
//! The blobs are already on this disk — [`BlobCache::digests`] is the inventory — so the scan
//! costs Patwari nothing. It streams each blob line by line rather than reading it whole: the
//! mirror holds gigabytes and a scan that needed all of it in memory would be a different kind of
//! production incident.

use std::io::{BufRead, BufReader};

use qanungo::cache::BlobCache;
use qanungo::redaction::{PATTERN_REVISION, RedactionReport, Redactor};

/// Longest line the scan will scrub whole. Transcripts carry pasted files and base64 attachments;
/// a line past this is truncated for the scan only, which can only *under*-count and never invent
/// a hit.
const MAX_SCANNED_LINE_BYTES: usize = 1 << 20;

#[test]
#[ignore = "reads the local blob cache; run with --ignored --nocapture"]
fn the_secrets_pass_over_the_local_mirror() {
    let cache = BlobCache::open_default().expect("a cache root");
    let digests = cache.digests().expect("the cache is readable");
    assert!(
        !digests.is_empty(),
        "no blobs mirrored yet — run `qanungo report` first"
    );

    let redactor = Redactor::new();
    let mut total = RedactionReport::default();
    let mut sessions_with_hits = 0usize;
    let mut lines = 0u64;
    let mut bytes = 0u64;

    for digest in &digests {
        let Ok(blob) = cache.open_blob(digest) else {
            continue;
        };
        let mut session = RedactionReport::default();
        let mut reader = BufReader::new(blob);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(read) => bytes += read as u64,
            }
            lines += 1;
            let scanned = &line[..floor_boundary(&line, MAX_SCANNED_LINE_BYTES)];
            session.absorb(&redactor.scrub(scanned).report);
        }
        if !session.is_empty() {
            sessions_with_hits += 1;
        }
        total.absorb(&session);
    }

    println!("\nredaction scan — pattern revision {PATTERN_REVISION}");
    println!(
        "{} blobs, {lines} records, {:.2} GiB scanned",
        digests.len(),
        bytes as f64 / (1024.0 * 1024.0 * 1024.0)
    );
    println!(
        "{sessions_with_hits} of {} blobs fired anything",
        digests.len()
    );
    if total.is_empty() {
        println!("  (no pattern fired)");
    }
    for (pattern, count) in total.fired() {
        println!("  {pattern:<22} {count}");
    }
    println!("  {:<22} {}", "TOTAL", total.total());
}

/// The largest offset at or below `limit` that is a character boundary, so truncating a line for
/// the scan cannot panic on a multi-byte character.
fn floor_boundary(line: &str, limit: usize) -> usize {
    let mut offset = limit.min(line.len());
    while !line.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}
