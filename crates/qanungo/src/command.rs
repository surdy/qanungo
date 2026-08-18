//! The `report` command: the vertical slice's spine.
//!
//! sync → fold → evaluate → emit, in one pass, with the fold timed separately from the network
//! so the instrumentation footer measures what it claims to measure.

use std::collections::BTreeMap;
use std::io::{self, BufReader, Write};
use std::time::Instant;

use chrono::Utc;
use thiserror::Error;

use crate::cache::BlobCache;
use crate::cli::ReportArgs;
use crate::metrics::{self, SessionMetrics};
use crate::patwari::{PatwariError, ReadClient};
use crate::report::{Instrumentation, Report, SkippedNote};
use crate::rules;
use crate::sync::{self, MirroredSession, Skip, SkipReason};

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("could not reach the archive at {url}: {source}")]
    Archive {
        url: String,
        #[source]
        source: PatwariError,
    },
    #[error("could not open the transcript cache: {0}")]
    Cache(#[source] io::Error),
    #[error("could not write the report: {0}")]
    Output(#[source] io::Error),
}

/// Runs `qanungo report`, writing Markdown to `out`.
///
/// # Errors
///
/// Returns an error when the cache is unusable, the archive window cannot be listed, or the
/// report cannot be written. A single unreadable session is a reported gap, not a failure.
pub fn report(args: &ReportArgs, out: &mut impl Write) -> Result<(), CommandError> {
    let cache = match &args.cache_dir {
        Some(dir) => BlobCache::open(dir),
        None => BlobCache::open_default(),
    }
    .map_err(CommandError::Cache)?;

    let client =
        ReadClient::connect(&args.patwari_url).map_err(|source| CommandError::Archive {
            url: args.patwari_url.clone(),
            source,
        })?;

    let generated_at = Utc::now();
    let activity_from =
        (generated_at - args.last.delta()).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mirror =
        sync::sync(&client, &cache, &activity_from, args.concurrency).map_err(|source| {
            CommandError::Archive {
                url: args.patwari_url.clone(),
                source,
            }
        })?;

    let fold_started = Instant::now();
    let mut sessions = Vec::with_capacity(mirror.sessions.len());
    let mut skipped = mirror.skipped;
    for mirrored in &mirror.sessions {
        match fold_one(&cache, mirrored) {
            Ok(session) => sessions.push(session),
            Err(reason) => skipped.push(Skip {
                source_agent: mirrored.source_agent.clone(),
                reason,
            }),
        }
    }
    let fold_elapsed = fold_started.elapsed();

    let findings = rules::evaluate(&sessions);
    let instrumentation = Instrumentation {
        sync: mirror.stats,
        fold_elapsed,
        sessions_folded: sessions.len(),
        bytes_folded: sessions.iter().map(|session| session.bytes_folded).sum(),
        patwari_url: args.patwari_url.clone(),
        cache_root: cache.root().to_path_buf(),
    };
    let markdown = Report {
        window: &args.last,
        generated_at,
        sessions: &sessions,
        findings: &findings,
        skipped: &summarize(&skipped),
        instrumentation: &instrumentation,
    }
    .render();

    out.write_all(markdown.as_bytes())
        .map_err(CommandError::Output)
}

/// Folds one cached transcript, streaming it off disk rather than reading it whole.
fn fold_one(cache: &BlobCache, mirrored: &MirroredSession) -> Result<SessionMetrics, SkipReason> {
    let source = metrics::source_for_agent(&mirrored.source_agent)
        .ok_or_else(|| SkipReason::UnknownAgent(mirrored.source_agent.clone()))?;
    let blob = cache
        .open_blob(&mirrored.source_hash)
        .map_err(|error| SkipReason::Unreadable(format!("cache read failed: {error}")))?;
    let fold =
        metrics::fold_transcript(source, mirrored.artifact_set_version, BufReader::new(blob))
            .map_err(|error| SkipReason::Unreadable(error.to_string()))?;
    Ok(SessionMetrics {
        source_hash: mirrored.source_hash.clone(),
        source_agent: mirrored.source_agent.clone(),
        summary: fold.summary,
        tools: fold.tools,
        activity: fold.activity,
        // The archive's declared original size, already verified against the transferred bytes,
        // so the footer's "bytes folded" needs no second pass over the file to count.
        bytes_folded: mirrored.size_bytes,
    })
}

/// Groups skips by reason so a systematic gap reads as one line.
fn summarize(skipped: &[Skip]) -> Vec<SkippedNote> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for skip in skipped {
        let reason = match &skip.reason {
            SkipReason::NoTranscript => {
                format!("{}: snapshot has no transcript artifact", skip.source_agent)
            }
            SkipReason::UnknownAgent(agent) => {
                format!("{agent}: no interpreter for this harness in this build")
            }
            SkipReason::Unreadable(detail) => format!("{}: {detail}", skip.source_agent),
        };
        *counts.entry(reason).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(reason, count)| SkippedNote { count, reason })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_are_grouped_by_reason() {
        let skips = vec![
            Skip {
                source_agent: "claude-code".to_owned(),
                reason: SkipReason::NoTranscript,
            },
            Skip {
                source_agent: "claude-code".to_owned(),
                reason: SkipReason::NoTranscript,
            },
            Skip {
                source_agent: "future".to_owned(),
                reason: SkipReason::UnknownAgent("future".to_owned()),
            },
        ];
        let notes = summarize(&skips);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].count, 2);
        assert!(notes[0].reason.contains("no transcript artifact"));
        assert_eq!(notes[1].count, 1);
    }
}
