//! `--json` on every document lane, end to end against a stand-in archive.
//!
//! The stand-in speaks the same shapes `tests/mirror.rs` and `tests/dashboard.rs` do — the session
//! listing with its `latest_snapshot` projection, a session's own snapshot listing, the snapshot
//! document with its artifact list, and the artifact-content route with its `x-patwari-*` metadata
//! headers — cut down to what these tests need: one honest transcript per session, an optional
//! `summary.md` beside it, `identity`, `Content-Length`. The verified-download machinery is
//! exercised to destruction in `mirror.rs`; what is under test here is the *document*.
//!
//! Two properties carry the weight, and each is asserted per lane:
//!
//! - **The JSON is the Markdown.** Every test runs the very same command twice against the very
//!   same archive — once plain, once with `--json` — and reconciles the envelope's headline figures
//!   against the sentences the Markdown printed. A `--json` that quietly folded something else
//!   would pass no test in this file.
//! - **The scrub does not depend on the medium.** The two aggregate lanes carry no verbatim at all,
//!   proved against fixtures whose every free-text field holds a canary — the `tests/dashboard.rs`
//!   approach, applied to a document instead of to a payload. The four verbatim lanes carry prose
//!   *scrubbed*, and the planted credentials in the fixtures must not survive into any of them.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use clap::Parser;
use qanungo::cli::{Cli, Command};
use qanungo::command;
use qanungo::json::SCHEMA_VERSION;
use qanungo::patwari::sha256_hex;
use serde_json::Value;

// ---------------------------------------------------------------------------
// The stand-in archive
// ---------------------------------------------------------------------------

struct ArchivedSession {
    session_id: String,
    snapshot_id: String,
    artifact_id: String,
    source_agent: String,
    transcript: Vec<u8>,
    original_sha256: String,
    completed_at: String,
    /// The `summary.md` this snapshot carries, when it carries one. `None` is a real archive state
    /// — the standup lane names it as a gap — and is what the transcript-only fixtures stay in.
    summary: Option<Vec<u8>>,
    summary_artifact_id: String,
    summary_sha256: String,
    /// The repository the archive's own session projection records: what the cost lane cuts by and
    /// what the doctor lane groups by. `None` is a session captured outside a checkout.
    repository: Option<String>,
}

impl ArchivedSession {
    /// One archived session carrying `transcript`, completed `hours_ago` — relative to now rather
    /// than at a fixed date, so a window measured in days keeps selecting it however long this test
    /// lives.
    fn new(index: u8, source_agent: &str, transcript: &[u8], hours_ago: i64) -> Self {
        let completed = Utc::now() - TimeDelta::hours(hours_ago);
        Self {
            session_id: format!("{index:02x}").repeat(16),
            snapshot_id: format!("{:02x}", index.wrapping_add(100)).repeat(16),
            artifact_id: format!("{:02x}", index.wrapping_add(200)).repeat(16),
            source_agent: source_agent.to_owned(),
            original_sha256: sha256_hex(transcript),
            transcript: transcript.to_vec(),
            completed_at: completed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            summary: None,
            summary_artifact_id: format!("{:02x}", index.wrapping_add(50)).repeat(16),
            summary_sha256: String::new(),
            repository: None,
        }
    }

    fn with_summary(mut self, summary: &[u8]) -> Self {
        self.summary_sha256 = sha256_hex(summary);
        self.summary = Some(summary.to_vec());
        self
    }

    fn in_repository(mut self, repository: &str) -> Self {
        self.repository = Some(repository.to_owned());
        self
    }

    /// The artifact this id names, with the digest the archive declares for it.
    fn artifact(&self, id: &str) -> Option<(&[u8], &str)> {
        if id == self.artifact_id {
            return Some((&self.transcript, &self.original_sha256));
        }
        match &self.summary {
            Some(summary) if id == self.summary_artifact_id => {
                Some((summary, &self.summary_sha256))
            }
            _ => None,
        }
    }
}

/// Serves `sessions` until the test process exits, and returns its base URL.
fn spawn_archive(sessions: Vec<ArchivedSession>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sessions = Arc::new(sessions);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sessions = Arc::clone(&sessions);
            std::thread::spawn(move || serve_archive(stream, &sessions));
        }
    });
    base
}

fn serve_archive(mut stream: TcpStream, sessions: &[ArchivedSession]) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let path = target.split_once('?').map_or(target.as_str(), |(p, _)| p);

    let response = if path == "/api/v1/sessions" {
        json_response(&session_page(sessions))
    } else if let Some(id) = path
        .strip_prefix("/api/v1/sessions/")
        .and_then(|rest| rest.strip_suffix("/snapshots"))
    {
        json_response(&session_snapshots(sessions, id))
    } else if let Some(id) = path.strip_prefix("/api/v1/snapshots/") {
        match sessions.iter().find(|session| session.snapshot_id == id) {
            Some(session) => json_response(&snapshot_document(session)),
            None => not_found(),
        }
    } else if let Some(id) = path
        .strip_prefix("/api/v1/artifacts/")
        .and_then(|rest| rest.strip_suffix("/content"))
    {
        match sessions.iter().find_map(|session| session.artifact(id)) {
            Some((bytes, digest)) => content_response(bytes, digest),
            None => not_found(),
        }
    } else {
        not_found()
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
}

fn session_page(sessions: &[ArchivedSession]) -> String {
    let items: Vec<String> = sessions
        .iter()
        .map(|session| {
            format!(
                r#"{{"session_id":"{}","source_agent":"{}","source_session_id":"harness-{}",
                    "created_at":"{}","updated_at":"{}",
                    "latest_snapshot":{{"snapshot_id":"{}","completed_at":"{}",
                    "project":null,"repository":{},"branch":null,"source_agent_version":null,
                    "artifact_set_version":2,"snapshot_url":"/api/v1/snapshots/{}",
                    "manifest_url":"/api/v1/snapshots/{}/manifest"}},
                    "captures_url":"/api/v1/sessions/{}/captures",
                    "snapshots_url":"/api/v1/sessions/{}/snapshots"}}"#,
                session.session_id,
                session.source_agent,
                session.session_id,
                session.completed_at,
                session.completed_at,
                session.snapshot_id,
                session.completed_at,
                match &session.repository {
                    Some(name) => format!("\"{name}\""),
                    None => "null".to_owned(),
                },
                session.snapshot_id,
                session.snapshot_id,
                session.session_id,
                session.session_id,
            )
        })
        .collect();
    format!(
        r#"{{"items":[{}],"next_cursor":null,"high_watermark":null}}"#,
        items.join(",")
    )
}

fn session_snapshots(sessions: &[ArchivedSession], session_id: &str) -> String {
    let items: Vec<String> = sessions
        .iter()
        .filter(|session| session.session_id == session_id)
        .map(|session| {
            format!(
                r#"{{"snapshot_id":"{}","session_id":"{}","snapshot_fingerprint":"sha256:{}",
                    "manifest_id":"{}","manifest_sha256":"sha256:{}",
                    "completed_at":"{}","artifact_count":1,
                    "total_original_bytes":{},"total_stored_bytes":{},"capture_count":1,
                    "snapshot_url":"/api/v1/snapshots/{}",
                    "captures_url":"/api/v1/snapshots/{}/captures",
                    "manifest_url":"/api/v1/manifests/{}"}}"#,
                session.snapshot_id,
                session.session_id,
                "0".repeat(64),
                session.snapshot_id,
                "1".repeat(64),
                session.completed_at,
                session.transcript.len(),
                session.transcript.len(),
                session.snapshot_id,
                session.snapshot_id,
                session.snapshot_id,
            )
        })
        .collect();
    format!(
        r#"{{"items":[{}],"next_cursor":null,"high_watermark":null}}"#,
        items.join(",")
    )
}

fn snapshot_document(session: &ArchivedSession) -> String {
    format!(
        r#"{{"snapshot_id":"{}","session_id":"{}","snapshot_fingerprint":"sha256:{}",
            "manifest_id":"{}","manifest_sha256":"sha256:{}",
            "completed_at":"{}","artifact_count":1,
            "total_original_bytes":{},"total_stored_bytes":{},"capture_count":1,
            "captures_url":"/api/v1/snapshots/{}/captures","manifest_url":"/api/v1/manifests/{}",
            "manifest":{{"schema_version":1,
                "session":{{"source_agent":"{}","source_session_id":"harness"}},
                "capture":{{"captured_at":"{}","source_cursor":null,
                    "source_state_hash":null,"source_metadata":{{}},"project":null,
                    "repository":null,"branch":null,"source_agent_version":null,
                    "artifact_set_version":2,"munshi_version":null}},
                "artifacts":[]}},
            "artifacts":[{{"artifact_id":"{}","artifact_index":0,
                "logical_path":"transcript.jsonl","media_type":"application/jsonl",
                "original_size_bytes":{},"original_sha256":"sha256:{}",
                "stored_size_bytes":{},"stored_sha256":"sha256:{}","compression":"identity",
                "metadata_url":"/api/v1/artifacts/{}",
                "content_url":"/api/v1/artifacts/{}/content"}}{}]}}"#,
        session.snapshot_id,
        session.session_id,
        "0".repeat(64),
        session.snapshot_id,
        "1".repeat(64),
        session.completed_at,
        session.transcript.len(),
        session.transcript.len(),
        session.snapshot_id,
        session.snapshot_id,
        session.source_agent,
        session.completed_at,
        session.artifact_id,
        session.transcript.len(),
        session.original_sha256,
        session.transcript.len(),
        session.original_sha256,
        session.artifact_id,
        session.artifact_id,
        summary_artifact(session),
    )
}

/// The snapshot's second artifact, when it has one. A snapshot with no `summary.md` is what the
/// standup lane calls a gap.
fn summary_artifact(session: &ArchivedSession) -> String {
    let Some(summary) = &session.summary else {
        return String::new();
    };
    format!(
        r#",{{"artifact_id":"{}","artifact_index":1,
            "logical_path":"summary.md","media_type":"text/markdown",
            "original_size_bytes":{},"original_sha256":"sha256:{}",
            "stored_size_bytes":{},"stored_sha256":"sha256:{}","compression":"identity",
            "metadata_url":"/api/v1/artifacts/{}",
            "content_url":"/api/v1/artifacts/{}/content"}}"#,
        session.summary_artifact_id,
        summary.len(),
        session.summary_sha256,
        summary.len(),
        session.summary_sha256,
        session.summary_artifact_id,
        session.summary_artifact_id,
    )
}

fn json_response(body: &str) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}

fn content_response(bytes: &[u8], digest: &str) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/octet-stream\r\n\
         Content-Length: {}\r\n\
         x-patwari-compression: identity\r\n\
         x-patwari-original-size-bytes: {}\r\n\
         x-patwari-original-sha256: sha256:{}\r\n\
         x-patwari-stored-size-bytes: {}\r\n\
         x-patwari-stored-sha256: sha256:{}\r\n\r\n",
        bytes.len(),
        bytes.len(),
        digest,
        bytes.len(),
        digest,
    )
    .into_bytes();
    response.extend_from_slice(bytes);
    response
}

fn not_found() -> Vec<u8> {
    let body = r#"{"error":{"code":"not_found","message":"not found"}}"#;
    let mut response = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}

// ---------------------------------------------------------------------------
// Running a lane twice over one archive
// ---------------------------------------------------------------------------

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

fn transcript(relative: &str) -> Vec<u8> {
    std::fs::read(fixture(relative)).expect("fixture is readable")
}

fn summary(name: &str) -> Vec<u8> {
    std::fs::read(fixture(&format!("standup/{name}"))).expect("fixture is readable")
}

/// One lane, run twice over one archive and one cache: Markdown first, then `--json`.
///
/// The cache is shared between the two runs on purpose — the second is warm, which is the ordinary
/// case and the one whose provenance figures the tests read — and the two runs fold the same
/// listing, which is what makes reconciling their numbers meaningful rather than approximate.
///
/// Every test in this file goes through here, so no test can accidentally compare a `--json`
/// document against a Markdown one taken over a different window or a different archive.
struct Lane {
    markdown: String,
    json: Value,
}

fn run(argv: &[&str], sessions: Vec<ArchivedSession>) -> Lane {
    let base = spawn_archive(sessions);
    let directory = tempfile::tempdir().expect("a scratch directory");
    let cache = directory.path().join("qanungo");
    let markdown = String::from_utf8(invoke(argv, &base, &cache, false)).expect("utf-8 Markdown");
    let json = invoke(argv, &base, &cache, true);
    let json: Value = serde_json::from_slice(&json).expect("`--json` writes a JSON document");
    Lane { markdown, json }
}

/// Parses `argv` with the shared archive flags appended and runs whichever lane it names.
fn invoke(argv: &[&str], base: &str, cache: &Path, json: bool) -> Vec<u8> {
    let cache = cache.to_str().expect("a utf-8 scratch path").to_owned();
    let mut full: Vec<String> = std::iter::once("qanungo".to_owned())
        .chain(argv.iter().map(|part| (*part).to_owned()))
        .chain([
            "--patwari-url".to_owned(),
            base.to_owned(),
            "--cache-dir".to_owned(),
            cache,
        ])
        .collect();
    if json {
        full.push("--json".to_owned());
    }
    let mut out = Vec::new();
    match Cli::parse_from(&full).command {
        Command::Report(args) => command::report(&args, &mut out),
        Command::Cost(args) => command::cost(&args, &mut out),
        Command::Standup(args) => command::standup(&args, &mut out),
        Command::Ask(args) => command::ask(&args, &mut out),
        Command::Doctor(args) => command::doctor(&args, &mut out),
        Command::Flows(args) => command::flows(&args, &mut out),
        Command::Dashboard(_) | Command::Rules(_) => {
            panic!("this file drives the document lanes only")
        }
    }
    .expect("the lane runs against the stand-in archive");
    out
}

/// The six keys every `--json` document wears, whichever lane wrote it.
///
/// Asserted per lane rather than once, because the envelope is the contract a consumer indexes and
/// a lane that forgot one of them would still produce parseable JSON.
fn assert_envelope(document: &Value, command: &str) {
    assert_eq!(document["schema_version"], SCHEMA_VERSION);
    assert_eq!(document["command"], command);
    assert!(
        document["generated_at"]
            .as_str()
            .expect("a generated_at stamp")
            .ends_with('Z'),
        "every stamp on this surface is UTC: {}",
        document["generated_at"],
    );
    // The full digest, not the footer's short stamp — a machine comparing two runs wants the whole
    // thing.
    let rule_pack = document["rule_pack"].as_str().expect("a rule-pack digest");
    assert_eq!(rule_pack.len(), 64, "the full digest: {rule_pack}");
    assert!(document["window"].is_object(), "a window block");
    assert!(document["data"].is_object(), "a data block");
    // Never omitted: a number with no cost beside it is a number nobody can decide whether to
    // trust. The four figures the Markdown footer prints are all here.
    let provenance = &document["provenance"];
    for key in [
        "fold",
        "sync",
        "cache_hits",
        "cache_misses",
        "sessions_listed",
    ] {
        assert!(
            !provenance[key].is_null(),
            "`provenance.{key}` is a footer value and is never omitted: {provenance}",
        );
    }
}

// ---------------------------------------------------------------------------
// The windows
// ---------------------------------------------------------------------------

/// A window with something to say in every lane: rule firings, billable usage, and a narrative.
///
/// The repository each session is *listed* under is deliberately not the one its own `summary.md`
/// names, because the cost lane cuts by the archive's projection and the standup lane groups by
/// what the summary itself says.
fn full_archive() -> Vec<ArchivedSession> {
    vec![
        ArchivedSession::new(
            11,
            "claude-code",
            &transcript("cost/claude-billing.jsonl"),
            2,
        )
        .with_summary(&summary("qanungo-cost.md"))
        .in_repository("surdy/qanungo"),
        ArchivedSession::new(
            12,
            "claude-code",
            &transcript("rules/marathon-session.jsonl"),
            3,
        )
        .with_summary(&summary("qanungo-scoring.md"))
        .in_repository("surdy/qanungo"),
        ArchivedSession::new(
            13,
            "claude-code",
            &transcript("rules/high-tool-error-rate.jsonl"),
            4,
        )
        .with_summary(&summary("munshi-tombstone.md"))
        .in_repository("surdy/munshi"),
        ArchivedSession::new(14, "claude-code", &transcript("rules/retry-loop.jsonl"), 5)
            .with_summary(&summary("no-repository.md")),
        // No `summary.md` on any snapshot: a gap the standup and ask lanes both have to count.
        ArchivedSession::new(
            16,
            "copilot-cli",
            &transcript("cost/copilot-billing.jsonl"),
            7,
        )
        .in_repository("surdy/munshi"),
    ]
}

/// A window whose sessions trip several rules and whose every free-text field carries a canary.
///
/// No snapshot here carries a `summary.md`, which is what makes the whole-document canary sweep
/// below a statement about `report` and `cost` rather than about which fixture happened to be
/// chosen — the same care `tests/dashboard.rs` takes with its own no-verbatim proof.
fn canary_archive() -> Vec<ArchivedSession> {
    vec![
        ArchivedSession::new(
            1,
            "claude-code",
            &transcript("rules/high-tool-error-rate.jsonl"),
            2,
        ),
        ArchivedSession::new(
            2,
            "claude-code",
            &transcript("rules/marathon-session.jsonl"),
            3,
        ),
        ArchivedSession::new(3, "codex-cli", &transcript("rules/retry-loop.jsonl"), 4),
        ArchivedSession::new(
            4,
            "claude-code",
            &transcript("rules/unreviewed-ship.jsonl"),
            5,
        ),
    ]
}

/// Two sessions of one repository that give the same instruction twice — a cluster, in both the
/// doctor lane's per-repository reading and the flows lane's pooled one.
///
/// The instruction carries two planted credentials, which is what makes the same fixture serve as
/// the scrub proof for both lanes: the excerpt has to come back with the words around them and
/// markers where they were.
fn repeated_instruction_archive() -> Vec<ArchivedSession> {
    vec![
        ArchivedSession::new(
            31,
            "claude-code",
            &transcript("doctor/repeated-rule.jsonl"),
            2,
        )
        .in_repository("surdy/qanungo"),
        ArchivedSession::new(
            32,
            "claude-code",
            &transcript("doctor/repeated-rule-restated.jsonl"),
            3,
        )
        .in_repository("surdy/qanungo"),
    ]
}

/// The credentials planted in the fixtures above. Neither has ever been real; each is a shape with
/// `CANARY` spelled through its body, as everywhere else in this tree.
const PLANTED: [&str; 3] = [
    "ghp_CANARYCANARYCANARYCANARYCANARYCANARY",
    "sk-ant-api03-CANARYSECRETCANARYSECRETCANARYSECRET99",
    "AKIACANARY0EXAMPLE99",
];

// ---------------------------------------------------------------------------
// One test per lane: the envelope, and the Markdown's own headline numbers
// ---------------------------------------------------------------------------

/// `report --json` carries the envelope, and every lane score in it is the score the table printed.
///
/// Reconciled row by row rather than in total: the fleet cell of the Markdown table is the last
/// column of the row a lane's title opens, and `data.lanes[].fleet` is what the JSON says the same
/// number is. A `--json` that folded a different window would disagree on the first scored lane.
#[test]
fn the_report_lanes_are_the_scores_the_table_printed() {
    let lane = run(&["report", "--last", "30d"], full_archive());
    assert_envelope(&lane.json, "report");
    assert_eq!(lane.json["window"]["last"], "30d");

    let lanes = lane.json["data"]["lanes"]
        .as_array()
        .expect("`data.lanes` is the array the README's example indexes");
    assert_eq!(lanes.len(), 5, "the five lanes, always");
    let mut scored = 0;
    for entry in lanes {
        let title = entry["title"].as_str().expect("a lane title");
        let row = lane
            .markdown
            .lines()
            .find(|line| line.starts_with(&format!("| {title} |")))
            .unwrap_or_else(|| panic!("the table has a row for {title}:\n{}", lane.markdown));
        let fleet = row
            .trim_end_matches('|')
            .rsplit('|')
            .next()
            .expect("a fleet cell")
            .trim()
            .to_owned();
        match entry["fleet"]["state"].as_str() {
            Some("scored") => {
                scored += 1;
                let score = entry["fleet"]["score"].as_u64().expect("a fleet score");
                // The cell is the score with an optional arrow glued to it.
                assert!(
                    fleet.starts_with(&score.to_string()),
                    "{title}: JSON says {score}, the table says `{fleet}`",
                );
            }
            Some("no-reading") => assert_eq!(fleet, "no reading", "{title}"),
            Some("not-scored") => assert_eq!(fleet, "not scored", "{title}"),
            other => panic!("{title}: unexpected fleet state {other:?}"),
        }
    }
    assert!(
        scored > 0,
        "this window must score something:\n{}",
        lane.markdown
    );

    // The footer's own figures, in the envelope rather than dropped on the floor.
    assert_eq!(
        lane.json["provenance"]["sessions_folded"].as_u64(),
        Some(5),
        "{}",
        lane.json["provenance"],
    );
    assert_eq!(lane.json["provenance"]["renders_verbatim"], false);
    assert!(
        lane.markdown
            .contains(lane.json["rule_pack"].as_str().unwrap()[..12].trim()),
        "the footer stamps the pack the envelope names",
    );
}

/// `cost --json` carries the envelope, and its total is the sentence the report opened with.
#[test]
fn the_cost_total_is_the_dollars_the_report_printed() {
    let lane = run(&["cost", "--last", "12w"], full_archive());
    assert_envelope(&lane.json, "cost");
    assert_eq!(lane.json["window"]["last"], "12w");

    let priced = &lane.json["data"]["priced"];
    assert_eq!(priced["priced_anything"], true, "{}", lane.markdown);
    let headline = format!(
        "**{}** across {} sessions and {} billed messages.",
        priced["dollars_rendered"].as_str().expect("rendered money"),
        priced["sessions"].as_u64().expect("priced sessions"),
        priced["messages"].as_u64().expect("billed messages"),
    );
    assert!(
        lane.markdown.contains(&headline),
        "the JSON total is not the printed one.\nexpected: {headline}\n\n{}",
        lane.markdown,
    );
    assert_eq!(
        lane.json["provenance"]["records_read"], lane.json["data"]["records_read"],
        "the footer counts what the fold read",
    );
    assert_eq!(lane.json["provenance"]["renders_verbatim"], false);
}

/// `standup --json` carries the envelope, and its session count is the one the narrative claimed.
#[test]
fn the_standup_session_count_is_the_one_the_narrative_claimed() {
    let lane = run(&["standup", "--last", "30d"], full_archive());
    assert_envelope(&lane.json, "standup");

    let data = &lane.json["data"];
    let sessions = data["sessions"].as_u64().expect("a session count");
    let repositories = data["repositories_narrated"]
        .as_u64()
        .expect("a repository count");
    assert!(
        sessions > 0,
        "this window narrates something:\n{}",
        lane.markdown
    );
    assert!(
        lane.markdown
            .contains(&format!("**{sessions} sessions across {repositories} ")),
        "the JSON count is not the narrated one.\n{}",
        lane.markdown,
    );
    // Every session the JSON lists is a session the Markdown gave a heading to.
    for group in data["repositories"].as_array().expect("repository groups") {
        for session in group["sessions"].as_array().expect("sessions in a group") {
            let title = session["title"].as_str().expect("a session title");
            assert!(
                lane.markdown.contains(&format!("### {title}")),
                "`{title}` is in the JSON and not in the document",
            );
        }
    }
    assert_eq!(lane.json["provenance"]["renders_verbatim"], true);
    assert_eq!(lane.json["provenance"]["redaction"]["secrets"], true);
}

/// `ask --json` carries the envelope, and its hits are the hits the document ranked.
#[test]
fn the_ask_hits_are_the_ones_the_document_ranked() {
    let lane = run(&["ask", "price the window", "--limit", "5"], full_archive());
    assert_envelope(&lane.json, "ask");
    // A lifetime question has no window, and says so in a word rather than in a duration.
    assert!(lane.json["window"]["last"].is_null());
    assert_eq!(lane.json["window"]["scope"], "all-history");

    let data = &lane.json["data"];
    assert_eq!(data["state"], "ranked", "{}", lane.markdown);
    assert_eq!(data["limit"], 5);
    assert_eq!(data["verbatim_requested"], false);
    let hits = data["hits"].as_array().expect("ranked hits");
    assert!(!hits.is_empty(), "the query must match:\n{}", lane.markdown);
    assert_eq!(
        hits.len() as u64,
        data["total_matches"].as_u64().unwrap().min(5),
        "the shown count reconciles with the total",
    );
    for (index, hit) in hits.iter().enumerate() {
        let heading = format!("### {}. {}", index + 1, hit["title"].as_str().unwrap());
        assert!(
            lane.markdown.contains(&heading),
            "`{heading}` is in the JSON and not in the document",
        );
    }
    // The lane's own honesty pair: what was searched and what could not be.
    assert!(
        lane.markdown
            .contains(&format!("Searched {} ", data["searched"].as_u64().unwrap())),
        "{}",
        lane.markdown,
    );
    assert_eq!(
        lane.json["provenance"]["sessions_unsearchable"],
        data["unsearchable"],
    );
}

/// A query with no searchable word in it is answered as a document rather than as a ranking.
#[test]
fn a_query_with_no_searchable_word_is_its_own_state() {
    let lane = run(&["ask", "a an the"], full_archive());
    assert_envelope(&lane.json, "ask");
    assert_eq!(lane.json["data"]["state"], "no-searchable-terms");
    assert!(
        lane.json["data"]["hits"].as_array().unwrap().is_empty(),
        "nothing was ranked, so nothing is shown",
    );
    // The Markdown makes the same refusal, and neither run touched the archive to make it.
    assert_eq!(lane.json["provenance"]["sessions_listed"], 0);
}

/// `doctor --json` carries the envelope, and every cluster in it is a cluster the document quoted.
#[test]
fn the_doctor_clusters_are_the_ones_the_document_quoted() {
    let lane = run(&["doctor"], repeated_instruction_archive());
    assert_envelope(&lane.json, "doctor");
    assert_eq!(lane.json["window"]["scope"], "all-history");

    let data = &lane.json["data"];
    assert_eq!(
        data["clusters_per_repo"],
        qanungo::doctor::DEFAULT_CLUSTERS_PER_REPOSITORY,
    );
    let repositories = data["repositories"]
        .as_array()
        .expect("repository sections");
    assert!(
        !repositories.is_empty(),
        "the fixture repeats an instruction:\n{}",
        lane.markdown,
    );
    let mut quoted = 0;
    for section in repositories {
        let repository = section["repository"].as_str().expect("a repository");
        assert!(lane.markdown.contains(&format!("### {repository}")));
        for cluster in section["clusters"].as_array().expect("clusters") {
            quoted += 1;
            let excerpt = cluster["excerpt"].as_str().expect("an excerpt");
            assert!(
                lane.markdown.contains(&format!("> {excerpt}")),
                "an excerpt in the JSON is not in the document: {excerpt}",
            );
            assert!(
                cluster["citations"].as_array().expect("citations").len()
                    <= cluster["occurrences"].as_u64().unwrap() as usize,
                "a cluster cannot cite more occurrences than it counted",
            );
        }
    }
    assert_eq!(
        quoted as u64,
        data["clusters"].as_u64().expect("a cluster count"),
        "the rendered clusters and the counted ones agree in this window",
    );
    assert_eq!(lane.json["provenance"]["renders_verbatim"], true);
}

/// `flows --json` carries the envelope, and its clusters are the ones the document quoted.
#[test]
fn the_flows_clusters_are_the_ones_the_document_quoted() {
    let lane = run(&["flows"], repeated_instruction_archive());
    assert_envelope(&lane.json, "flows");

    let data = &lane.json["data"];
    assert_eq!(data["clusters_cap"], qanungo::flows::DEFAULT_CLUSTERS);
    assert_eq!(data["flows_cap"], qanungo::flows::DEFAULT_FLOWS);
    let clusters = data["clusters"].as_array().expect("clusters");
    assert!(
        !clusters.is_empty(),
        "the fixture repeats a request:\n{}",
        lane.markdown,
    );
    assert_eq!(
        clusters.len() as u64,
        data["clusters_found"].as_u64().unwrap(),
        "nothing was cut in this window, so the two counts agree",
    );
    for cluster in clusters {
        let excerpt = cluster["excerpt"].as_str().expect("an excerpt");
        assert!(
            lane.markdown.contains(&format!("> {excerpt}")),
            "an excerpt in the JSON is not in the document: {excerpt}",
        );
    }
    assert_eq!(lane.json["provenance"]["renders_verbatim"], true);
}

// ---------------------------------------------------------------------------
// The scrub does not depend on the medium
// ---------------------------------------------------------------------------

/// `report --json` and `cost --json` carry **no** verbatim transcript content.
///
/// The `tests/dashboard.rs` proof, applied to the two documents: fixtures whose every free-text
/// field holds a canary, folded into a window that actually fires rules, and then the whole
/// serialized document swept for any of them. Both lanes hold this by *construction* — their folds
/// have already reduced a transcript to counts, hashes and positions — so a canary reaching the
/// wire would mean a new field started carrying text rather than that a filter missed one.
#[test]
fn the_report_and_cost_documents_carry_no_verbatim_transcript_content() {
    for relative in ["rules/high-tool-error-rate.jsonl", "rules/retry-loop.jsonl"] {
        let raw = std::fs::read_to_string(fixture(relative)).unwrap();
        assert!(raw.contains("CANARY_"), "{relative} must carry canaries");
    }

    let report = run(&["report", "--last", "30d"], canary_archive());
    assert!(
        !report.json["data"]["findings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the document under test must have findings",
    );
    let cost = run(&["cost", "--last", "12w"], canary_archive());
    assert!(cost.json["data"]["records_read"].as_u64().unwrap() > 0);

    // What the coaching document does carry: the hashes and positions a reader redeems in their own
    // shell. Asserted on `report` alone because the cost document cites a session by hash only when
    // it has a top-tier row to list, and this window has none.
    assert!(
        serde_json::to_string(&report.json)
            .unwrap()
            .contains("\"source_hash\""),
        "`report --json` cites the sessions it counted",
    );

    for (name, lane) in [("report", &report), ("cost", &cost)] {
        let serialized = serde_json::to_string(&lane.json).expect("the document re-serializes");
        assert!(
            !serialized.contains("CANARY"),
            "a canary token reached `{name} --json`",
        );
        for forbidden in [
            "CANARY_USER_REQUEST_ONE",
            "CANARY_ASSISTANT_MESSAGE",
            "CANARY_COMMAND_0",
            "CANARY_ERROR_TEXT_0",
            "CANARY_RETRY_COMMAND",
            "CANARY_ONE_OFF_A",
            "rm -rf",
            "cargo test",
            "git status",
            "/work/fixture",
            "gitBranch",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "`{forbidden}` reached `{name} --json`",
            );
        }
        assert_eq!(lane.json["provenance"]["renders_verbatim"], false);
    }
}

/// The four lanes that *do* render prose scrub it in JSON exactly as they scrub it in Markdown.
///
/// Not "carry nothing" — these documents are supposed to carry the archive's own words — but
/// "carry them through the same scrub". The fold is what redacts, before either renderer sees a
/// string ([`qanungo::json`]'s module docs), so this test is the proof that the JSON path does not
/// reach around it: the planted credentials are absent from both documents, and the two agree about
/// what fired.
#[test]
fn the_verbatim_lanes_scrub_the_json_exactly_as_they_scrub_the_markdown() {
    let planted_in_summary = std::fs::read_to_string(fixture("standup/qanungo-cost.md")).unwrap();
    for secret in PLANTED {
        assert!(
            planted_in_summary.contains(secret),
            "the summary fixture must plant {secret}",
        );
    }

    for (name, lane) in [
        (
            "standup",
            run(&["standup", "--last", "30d"], full_archive()),
        ),
        (
            "ask",
            run(&["ask", "price the window", "--limit", "5"], full_archive()),
        ),
        ("doctor", run(&["doctor"], repeated_instruction_archive())),
        ("flows", run(&["flows"], repeated_instruction_archive())),
    ] {
        let serialized = serde_json::to_string(&lane.json).expect("the document re-serializes");
        for secret in PLANTED {
            assert!(
                !serialized.contains(secret),
                "`{name} --json` leaked a planted credential",
            );
            assert!(
                !lane.markdown.contains(secret),
                "`{name}` leaked a planted credential into Markdown",
            );
        }
        // The scrub is stated as a posture and reported as counts, and the two documents agree
        // about the counts because there is only one fold behind them.
        assert_eq!(
            lane.json["provenance"]["redaction"]["secrets"], true,
            "`{name} --json` states the posture it was run under",
        );
        assert_eq!(lane.json["provenance"]["renders_verbatim"], true, "{name}");
        assert!(
            lane.json["data"]["redaction"]["total"].is_number(),
            "`{name} --json` reports what the scrub fired: {}",
            lane.json["data"]["redaction"],
        );
    }
}

/// `--json` never prints Markdown, and the Markdown run never prints JSON.
///
/// One assertion rather than six, because the two documents share one entry point per lane and what
/// would break here is that entry point writing both.
#[test]
fn a_json_run_writes_no_markdown_and_a_markdown_run_writes_no_json() {
    for argv in [
        vec!["report", "--last", "30d"],
        vec!["cost", "--last", "12w"],
        vec!["standup", "--last", "30d"],
    ] {
        let lane = run(&argv, full_archive());
        assert!(
            lane.markdown.starts_with('#'),
            "{argv:?} still writes Markdown by default",
        );
        let serialized = serde_json::to_string_pretty(&lane.json).unwrap();
        assert!(
            !serialized.contains("\n# "),
            "{argv:?} put a Markdown heading in its JSON",
        );
    }
}
