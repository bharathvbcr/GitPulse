//! Event time: a ledger row's `ts_utc` is when it was appended; the time the
//! thing happened rides in `detail_json.occurred_at`, with the task that was
//! bound *then* (`task_basis`), never the one bound when the pass ran.

use super::*;

use crate::ledger::ids;
use crate::procguard::LockedSpawn;
use crate::test_support::git_in;

fn with_transcript_root<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", root);
    f()
}

fn pause() {
    std::thread::sleep(std::time::Duration::from_millis(20));
}

fn stamp(ms: u64) -> String {
    ids::iso8601_utc(ms)
}

fn call(cwd: &str, ts: &str, file: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": "ET",
        "timestamp": ts,
        "version": "2.1.241",
        "cwd": cwd,
        "message": { "content": [
            { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
        ]}
    })
    .to_string()
}

fn write_corpus(corpus: &std::path::Path, lines: &[String]) {
    let dir = corpus.join("slug");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ET.jsonl"), lines.join("\n") + "\n").unwrap();
}

/// A git checkout with one commit, canonical path.
fn git_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    git_in(&path, &["init", "-b", "main"]);
    std::fs::write(path.join("seed.txt"), "seed").unwrap();
    git_in(&path, &["add", "seed.txt"]);
    git_in(&path, &["commit", "-m", "seed"]);
    let text = path.to_string_lossy().into_owned();
    (dir, text)
}

fn detail(event: &ledger::LedgerEvent) -> serde_json::Value {
    serde_json::from_str(event.detail_json.as_deref().expect("a detail_json")).unwrap()
}

/// The transcript rows, by the file they name.
fn transcript_row(repo: &str, file: &str) -> ledger::LedgerEvent {
    ledger::tail(repo, 0, 1000)
        .unwrap()
        .into_iter()
        .find(|event| event.object.as_deref() == Some(file))
        .unwrap_or_else(|| panic!("no row for {file}"))
}

fn run_git_at(dir: &std::path::Path, epoch: i64, args: &[&str]) {
    let when = format!("@{epoch} +0000");
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            "user.email=gitpulse@test.local",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", &when)
        .env("GIT_COMMITTER_DATE", &when)
        .current_dir(dir)
        .output_locked()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_transcript_call_carries_the_time_it_happened() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().canonicalize().unwrap();
    let repo = repo.to_str().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let file = format!("{repo}/a.rs");
    write_corpus(
        corpus.path(),
        &[call(repo, "2026-09-01T12:00:05.250Z", &file)],
    );
    with_transcript_root(corpus.path(), || ingest_transcripts(repo));

    let row = transcript_row(repo, &file);
    let detail = detail(&row);
    assert_eq!(
        detail["occurred_at"], "2026-09-01T12:00:05.250Z",
        "{detail}"
    );
    assert_eq!(detail["occurred_at_basis"], "transcript", "{detail}");
    assert_ne!(
        row.ts_utc, "2026-09-01T12:00:05.250Z",
        "ts_utc is the append clock and must stay it"
    );
}

#[test]
fn an_offset_timestamp_is_converted_to_utc() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().canonicalize().unwrap();
    let repo = repo.to_str().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let file = format!("{repo}/a.rs");
    write_corpus(
        corpus.path(),
        &[call(repo, "2026-09-01T14:00:05+02:00", &file)],
    );
    with_transcript_root(corpus.path(), || ingest_transcripts(repo));

    let detail = detail(&transcript_row(repo, &file));
    assert_eq!(
        detail["occurred_at"], "2026-09-01T12:00:05.000Z",
        "{detail}"
    );
    assert_eq!(detail["occurred_at_basis"], "transcript", "{detail}");
}

#[test]
fn an_unreadable_or_future_time_is_ingest_time_and_names_no_task() {
    let (_dir, repo) = git_repo();
    crate::ledger::bindings::bind(&repo, &repo, "T-NOW").unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let garbled = format!("{repo}/garbled.rs");
    let future = format!("{repo}/future.rs");
    write_corpus(
        corpus.path(),
        &[
            call(&repo, "last tuesday", &garbled),
            call(&repo, &stamp(ids::now_millis() + 3_600_000), &future),
        ],
    );
    with_transcript_root(corpus.path(), || ingest_transcripts_into(&repo, &repo));

    for file in [&garbled, &future] {
        let row = transcript_row(&repo, file);
        let detail = detail(&row);
        assert_eq!(detail["occurred_at_basis"], "ingest", "{file}: {detail}");
        assert_eq!(detail["task_basis"], "unresolved", "{file}: {detail}");
        assert_eq!(
            row.task_id, None,
            "{file}: an untrusted time must not fall back to the current binding"
        );
        assert!(
            detail["occurred_at"].is_string(),
            "{file}: occurred_at is still stated: {detail}"
        );
    }
}

#[test]
fn a_call_carries_the_task_that_was_bound_when_it_happened() {
    let (_dir, repo) = git_repo();
    let before_any_bind = ids::now_millis();
    pause();
    crate::ledger::bindings::bind(&repo, &repo, "T1").unwrap();
    pause();
    let under_t1 = ids::now_millis();
    pause();
    crate::ledger::bindings::unbind(&repo, &repo).unwrap();
    pause();
    let while_unbound = ids::now_millis();
    pause();
    crate::ledger::bindings::bind(&repo, &repo, "T2").unwrap();
    pause();
    let under_t2 = ids::now_millis();

    let corpus = tempfile::tempdir().unwrap();
    let files: Vec<(String, u64)> = [
        ("before.rs", before_any_bind - 1),
        ("t1.rs", under_t1),
        ("unbound.rs", while_unbound),
        ("t2.rs", under_t2),
    ]
    .iter()
    .map(|(name, ms)| (format!("{repo}/{name}"), *ms))
    .collect();
    let lines: Vec<String> = files
        .iter()
        .map(|(file, ms)| call(&repo, &stamp(*ms), file))
        .collect();
    write_corpus(corpus.path(), &lines);
    // The app was closed for all of it: one catch-up, after the second bind.
    with_transcript_root(corpus.path(), || ingest_transcripts_into(&repo, &repo));

    let seen = |name: &str| {
        let row = transcript_row(&repo, &format!("{repo}/{name}"));
        let basis = detail(&row)["task_basis"].as_str().unwrap().to_string();
        (row.task_id, basis)
    };
    assert_eq!(seen("t1.rs"), (Some("T1".into()), "bound".into()));
    assert_eq!(seen("t2.rs"), (Some("T2".into()), "bound".into()));
    assert_eq!(seen("unbound.rs"), (None, "unbound".into()));
    assert_eq!(seen("before.rs"), (None, "unresolved".into()));
}

#[test]
fn a_reflog_entry_carries_the_time_of_the_move_not_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    let repo = path.to_str().unwrap();
    // Commits authored long ago; the checkout that moved HEAD happened later.
    let (t0, t1) = (1_780_000_000i64, 1_785_000_000i64);
    git_in(&path, &["init", "-b", "main"]);
    std::fs::write(path.join("a.txt"), "a").unwrap();
    git_in(&path, &["add", "a.txt"]);
    run_git_at(&path, t0, &["commit", "-m", "first"]);
    std::fs::write(path.join("a.txt"), "b").unwrap();
    run_git_at(&path, t0 + 10, &["commit", "-a", "-m", "second"]);
    run_git_at(&path, t1, &["checkout", "--detach", "HEAD~1"]);

    let out = ingest_reflog(repo, 50);
    assert_eq!(out.error, "", "{out:?}");

    let moved = ledger::tail(repo, 0, 100)
        .unwrap()
        .into_iter()
        .find(|event| event.action == "reflog.checkout")
        .expect("the checkout is in the ledger");
    let detail = detail(&moved);
    assert_eq!(detail["occurred_at"], stamp(t1 as u64 * 1000), "{detail}");
    assert_eq!(detail["occurred_at_basis"], "reflog", "{detail}");
    assert_eq!(detail["selector"], "HEAD@{0}", "{detail}");
}

#[test]
fn retention_stays_on_the_append_clock() {
    // `recorded_reflog_objects` rebuilds the reflog's already-seen set from
    // the stored rows. Pruning by `occurred_at` would delete a row git still
    // holds, and the next catch-up would record it again, for ever.
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().canonicalize().unwrap();
    let repo = repo.to_str().unwrap();
    for n in 0..3 {
        ledger::append(ledger::Draft {
            repo_path: repo.to_string(),
            action: "reflog.checkout".into(),
            object: Some(format!("old-{n}")),
            detail_json: Some(
                serde_json::json!({
                    "source": "reflog",
                    "occurred_at": "2020-01-01T00:00:00.000Z",
                    "occurred_at_basis": "reflog",
                })
                .to_string(),
            ),
            ..Default::default()
        })
        .unwrap();
    }
    assert_eq!(ledger::prune_events(repo, 30, 1).unwrap(), 0);
    assert_eq!(ledger::tail(repo, 0, 100).unwrap().len(), 3);
}

// ---- adversarial cases -------------------------------------------------

/// A small deterministic generator, so a failure names its seed.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 20) % bound
    }
}

const MAX_MS: u64 = 253_402_300_799_999; // 9999-12-31T23:59:59.999Z

/// Format then parse, with an independent reference: render the same instant
/// as local time in a random offset, and the parser must recover the instant.
#[test]
fn the_parser_round_trips_every_instant_through_every_offset() {
    let mut rng = Lcg(0x9E37_79B9_7F4A_7C15);
    for round in 0..200_000 {
        let ms = rng.next(MAX_MS + 1);
        let offset_min = rng.next(2 * (23 * 60 + 59) + 1) as i64 - (23 * 60 + 59);
        let local = ms as i64 + offset_min * 60_000;
        if !(0..=MAX_MS as i64).contains(&local) {
            continue;
        }
        let zulu = ids::iso8601_utc(local as u64);
        let sign = if offset_min < 0 { '-' } else { '+' };
        let magnitude = offset_min.abs();
        let text = format!(
            "{}{sign}{:02}:{:02}",
            &zulu[..23],
            magnitude / 60,
            magnitude % 60
        );
        assert_eq!(
            ids::parse_event_time(&text),
            Some(ms),
            "round {round}: {text} (offset {offset_min} min)"
        );
        // The Z form and a fraction of 1..=9 digits agree with it.
        assert_eq!(ids::parse_event_time(&ids::iso8601_utc(ms)), Some(ms));
        let digits = 1 + rng.next(9) as usize;
        let frac = format!("{:03}", ms % 1000) + &"0".repeat(6);
        let spelled = format!("{}.{}Z", &ids::iso8601_utc(ms)[..19], &frac[..digits.max(3)]);
        assert_eq!(ids::parse_event_time(&spelled), Some(ms), "{spelled}");
    }
}

/// Every one-byte mutation of a valid time either parses to an instant that
/// formats and re-parses to itself, or is refused. Never a panic.
#[test]
fn mutated_times_never_panic_and_never_name_a_different_instant_than_they_spell() {
    let mut rng = Lcg(42);
    let seeds = [
        "2026-09-01T12:00:05.250Z",
        "2026-09-01T12:00:05.250+14:00",
        "2026-09-01T12:00:05-00:00",
        "9999-12-31T23:59:59.999999999Z",
        "1970-01-01T00:00:00Z",
    ];
    for seed in seeds {
        for _ in 0..20_000 {
            let mut bytes = seed.as_bytes().to_vec();
            for _ in 0..1 + rng.next(3) {
                let at = rng.next(bytes.len() as u64 + 1) as usize;
                match rng.next(3) {
                    0 if at < bytes.len() => bytes[at] = rng.next(256) as u8,
                    1 => bytes.insert(at, rng.next(256) as u8),
                    _ if at < bytes.len() => {
                        bytes.remove(at);
                    }
                    _ => {}
                }
            }
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            if let Some(ms) = ids::parse_event_time(&text) {
                assert!(text.is_ascii(), "{text:?}");
                assert_eq!(ids::parse_event_time(&ids::iso8601_utc(ms)), Some(ms), "{text:?}");
            }
        }
    }
}

#[test]
fn time_edge_cases_are_decided_exactly() {
    use ids::parse_event_time as p;
    assert_eq!(p("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(p("1970-01-01T00:00:00+00:00"), Some(0));
    assert_eq!(p("1970-01-01T00:00:00-00:00"), Some(0));
    assert_eq!(p("1970-01-01T14:00:00+14:00"), Some(0));
    assert_eq!(p("1969-12-31T10:00:00-14:00"), Some(0));
    assert_eq!(p("1970-01-01T00:00:00+14:00"), None, "before the epoch");
    assert_eq!(p("9999-12-31T23:59:59.999Z"), Some(MAX_MS));
    assert_eq!(p("9999-12-31T23:59:59.999999999Z"), Some(MAX_MS));
    assert_eq!(p("9999-12-31T23:59:59-14:00"), Some(MAX_MS - 999 + 14 * 3_600_000));
    assert_eq!(p("2026-09-01T12:00:05.1234567890Z"), None);
    assert_eq!(p("2026-09-01T12:00:60Z"), None, "a leap second");
    assert_eq!(p("2026-09-01T12:00Z"), None, "no seconds");
    assert_eq!(p("2026-09-01T12:00:05z"), None);
    assert_eq!(p("2026-09-01T12:00:05.250z"), None);
    assert_eq!(p("2026-09-01T12:00:05.250 Z"), None);
    assert_eq!(p("\u{FF12}\u{FF10}26-09-01T12:00:05Z"), None, "full-width digits");
    assert_eq!(p("2026-09-01T12:00:05\u{FF0B}01:00"), None);
    assert_eq!(p("2026-09-01T12:00:05.\u{0663}Z"), None, "arabic-indic digit");
    assert_eq!(p("2026-09-01T12:00:05Z\0"), None);
    assert_eq!(p("2026-09-01T12:00:05.-1Z"), None);
    assert_eq!(p("2026-09-01T12:00:05+1:00"), None);
    assert_eq!(p("2026-09-01T12:00:05+01:0"), None);
    assert_eq!(p("0000-03-01T00:00:00Z"), None);
    assert_eq!(p("2100-02-29T00:00:00Z"), None, "2100 is not a leap year");
    assert!(p("2000-02-29T00:00:00Z").is_some());
    assert_eq!(p(&"9".repeat(1 << 20)), None);
}

/// The skew bound, with the clock injected: exactly now + 60 s is believed,
/// one millisecond past it is not.
#[test]
fn the_future_skew_bound_is_inclusive_at_sixty_seconds() {
    let now = 1_800_000_000_000u64;
    let task = |_| panic!("an untrusted time must not look anything up");
    let at = |ms: u64, called: &mut bool| {
        EventTime::at("transcript", Some(ms), now, |_| {
            *called = true;
            ledger::bindings::TaskAtTime::Unbound
        })
    };
    let mut called = false;
    let edge = at(now + 60_000, &mut called);
    assert!(called);
    assert_eq!((edge.basis, edge.occurred_at.as_str()), ("transcript", stamp(now + 60_000).as_str()));
    let mut called = false;
    let over = at(now + 60_001, &mut called);
    assert!(!called, "no history lookup for an untrusted time");
    assert_eq!(over.basis, "ingest");
    assert_eq!(over.occurred_at, stamp(now));
    assert_eq!(over.task, ledger::bindings::TaskAtTime::Unresolved);
    let none = EventTime::at("transcript", None, now, task);
    assert_eq!(none.basis, "ingest");
    // The epoch itself is accepted: an ancient time is believed.
    let epoch = EventTime::at("reflog", Some(0), now, |_| ledger::bindings::TaskAtTime::Unbound);
    assert_eq!(epoch.basis, "reflog");
    // No arithmetic overflow at the top of the range.
    let top = EventTime::at("reflog", Some(u64::MAX), u64::MAX - 1, |_| ledger::bindings::TaskAtTime::Unbound);
    assert_eq!(top.basis, "reflog");
}

/// Transcript timestamps are agent-controlled bytes. None may reach the row
/// except as the formatted instant it parses to.
#[test]
fn a_crafted_timestamp_reaches_the_row_only_as_a_formatted_instant() {
    let (_dir, repo) = git_repo();
    let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    let crafted = [
        format!("2026-09-01T12:00:05.250Z{secret}"),
        format!("2026-09-01T12:00:05.250\",\"task_basis\":\"bound\",\"x\":\"{secret}"),
        format!("2026-09-01T12:00:05.{secret}Z"),
        format!("{secret}-09-01T12:00:05.250Z"),
        "2026-09-01T12:00:05.250\u{0}Z".to_string(),
        "2026-09-01T12:00:05.9999999999Z".to_string(),
        "2026-09-01T12:00:05.250+00:00".to_string(),
    ];
    let corpus = tempfile::tempdir().unwrap();
    let files: Vec<String> = (0..crafted.len()).map(|n| format!("{repo}/c{n}.rs")).collect();
    let lines: Vec<String> = crafted.iter().zip(&files).map(|(t, f)| call(&repo, t, f)).collect();
    write_corpus(corpus.path(), &lines);
    with_transcript_root(corpus.path(), || ingest_transcripts_into(&repo, &repo));
    for (ts, file) in crafted.iter().zip(&files) {
        let Some(row) = ledger::tail(&repo, 0, 1000)
            .unwrap()
            .into_iter()
            .find(|event| event.object.as_deref() == Some(file.as_str()))
        else {
            continue; // a refused line leaves no row; that is not a leak
        };
        let text = row.detail_json.clone().unwrap();
        assert!(!text.contains(secret), "{ts:?} leaked: {text}");
        let detail = detail(&row);
        let occurred = detail["occurred_at"].as_str().unwrap();
        assert_eq!(occurred.len(), 24, "{ts:?}: {text}");
        assert!(ids::parse_event_time(occurred).is_some(), "{ts:?}: {text}");
        assert!(detail.get("x").is_none(), "{ts:?}: {text}");
        let keys: Vec<_> = detail.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys.iter().filter(|k| k.starts_with("occurred_at") || *k == "task_basis").count(),
            3,
            "{ts:?}: {text}"
        );
    }
}

/// The ledger's own database, opened directly so a test can write rows the
/// writer would not.
fn raw_conn<T>(
    repo: &str,
    f: impl FnOnce(&rusqlite::Connection) -> Result<T, ledger::LedgerError>,
) -> Result<T, ledger::LedgerError> {
    let conn = rusqlite::Connection::open(ledger::ledger_path(repo))
        .map_err(|e| ledger::LedgerError::new("open", e.to_string()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
    f(&conn)
}

/// Inserts a bind row for a path the family never registered, as one left
/// behind by a removed worktree.
fn put_bind(repo: &str, worktree: &str, ms: u64, ulid: &str, task: &str) {
    raw_conn(repo, |conn| {
        conn.execute(
            "INSERT INTO events (ulid, ts_utc, repo_path, worktree_path, actor_kind, task_id,
                                 action, outcome)
             VALUES (?1, ?2, ?3, ?4, 'human', ?5, 'worktree.bind', 'ok')",
            rusqlite::params![ulid, ids::iso8601_utc(ms), repo, worktree, task],
        )
        .map_err(|e| ledger::LedgerError::new("insert", e.to_string()))?;
        Ok(())
    })
    .unwrap();
}

/// The brief's cap says a removed worktree past 256 is `unresolved` even with
/// history; within it, history decides, and a removed worktree with none is
/// `unresolved` rather than the host's task.
#[test]
fn departed_owners_resolve_from_their_own_history_up_to_the_cap() {
    let (_dir, repo) = git_repo();
    ledger::bindings::bind(&repo, &repo, "T-HOST").unwrap();
    let bound_at = ids::now_millis() - 120_000;
    let called_at = stamp(ids::now_millis() - 60_000);
    let total = 258usize;
    let gone = |n: usize| format!("{repo}/.claude/worktrees/gone-{n:03}");
    for n in 0..total {
        // Even owners have a bind; odd ones never had one.
        if n % 2 == 0 {
            put_bind(&repo, &gone(n), bound_at, &format!("01H{n:023}"), &format!("T-G{n}"));
        }
    }
    let corpus = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (0..total)
        .map(|n| call(&gone(n), &called_at, &format!("{}/f.rs", gone(n))))
        .collect();
    write_corpus(corpus.path(), &lines);
    let out = with_transcript_root(corpus.path(), || ingest_transcripts_into(&repo, &repo));
    assert_eq!(out.error, "", "{out:?}");

    let rows = ledger::tail(&repo, 0, 10_000).unwrap();
    let task_of = |n: usize| {
        let file = format!("{}/f.rs", gone(n));
        let row = rows.iter().find(|e| e.object.as_deref() == Some(file.as_str())).unwrap_or_else(|| panic!("no row {n}"));
        (row.task_id.clone(), detail(row)["task_basis"].as_str().unwrap().to_string())
    };
    assert_eq!(task_of(0), (Some("T-G0".into()), "bound".into()));
    assert_eq!(task_of(1), (None, "unresolved".into()), "never the host's task");
    // First-met order is line order, so owner 256 is the first past the cap.
    assert_eq!(out.departed_overflow, (total - 256) as i64, "{out:?}");
    assert_eq!(task_of(256), (None, "unresolved".into()), "past the cap, with history");
}

/// The trust boundary, characterised: a transcript is written by the agent, so
/// the agent chooses the time and with it the task a row carries.
#[test]
fn an_agent_chosen_time_chooses_the_task_its_call_is_attributed_to() {
    let (_dir, repo) = git_repo();
    let now = ids::now_millis();
    ledger::bindings::bind(&repo, &repo, "T-NOW").unwrap();
    put_bind(&repo, &repo, now - 600_000, &format!("01H{:023}", 1), "T-OLD");
    let corpus = tempfile::tempdir().unwrap();
    let file = format!("{repo}/forged.rs");
    // Stamped inside T-OLD's window although it is being replayed under T-NOW.
    write_corpus(corpus.path(), &[call(&repo, &stamp(now - 300_000), &file)]);
    with_transcript_root(corpus.path(), || ingest_transcripts_into(&repo, &repo));
    let row = transcript_row(&repo, &file);
    assert_eq!(row.task_id.as_deref(), Some("T-OLD"));
    assert_eq!(detail(&row)["occurred_at_basis"], "transcript");
}

/// A repository moved or renamed on disk carries its ledger with it, and every
/// row names the old path. Ingest must still record what happened.
#[test]
fn a_ledger_carried_to_a_new_path_still_records_new_calls() {
    let (_dir, repo) = git_repo();
    ledger::bindings::bind(&repo, &repo, "T1").unwrap();
    raw_conn(&repo, |conn| {
        conn.execute("UPDATE events SET repo_path = '/old/location/of/the/repo'", [])
            .map_err(|e| ledger::LedgerError::new("update", e.to_string()))?;
        Ok(())
    })
    .unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let file = format!("{repo}/after-move.rs");
    write_corpus(corpus.path(), &[call(&repo, &stamp(ids::now_millis()), &file)]);
    let out = with_transcript_root(corpus.path(), || ingest_transcripts_into(&repo, &repo));
    eprintln!("moved-repo pass: {out:?}");
    let stored: i64 = raw_conn(&repo, |conn| {
        conn.query_row("SELECT count(*) FROM events WHERE object = ?1", [&file], |r| r.get(0))
            .map_err(|e| ledger::LedgerError::new("q", e.to_string()))
    })
    .unwrap();
    assert_eq!(stored, 1, "the call was lost: {out:?}");
    assert_eq!(out.error, "", "a person must be able to see why, and must not see noise: {out:?}");
}

/// The reflog selector the pane hands back to git stays a revision that
/// resolves to the entry's own commit, for every entry, and the 200 cap holds.
#[test]
fn every_reflog_selector_resolves_to_its_own_commit_and_the_cap_holds() {
    // Recent on purpose: a `git log -g` walk hides entries older than git's
    // reflog expiry (90 days), which cut this fixture to 7 entries at 2026-05.
    let base = i64::try_from(ids::now_millis() / 1000).unwrap() - 100_000;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    let repo = path.to_str().unwrap();
    git_in(&path, &["init", "-b", "main"]);
    for n in 0..205 {
        run_git_at(&path, base + n / 3, &["commit", "--allow-empty", "-m", &format!("c{n}")]);
    }
    run_git_at(&path, base + 500, &["checkout", "-b", "side"]);
    run_git_at(&path, base + 501, &["checkout", "main"]);
    let entries = crate::engine::git_reader::GitReader::get_reflog(repo, 200).unwrap();
    assert_eq!(entries.len(), 200);
    for (idx, entry) in entries.iter().enumerate() {
        assert_eq!(entry.index, idx);
        assert_eq!(entry.selector, format!("HEAD@{{{idx}}}"));
        assert!(entry.timestamp >= base, "{entry:?}");
        let resolved = std::process::Command::new("git")
            .args(["rev-parse", &entry.selector])
            .current_dir(&path)
            .output_locked()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&resolved.stdout).trim(),
            entry.commit_id,
            "{}",
            entry.selector
        );
    }
}

/// A short reflog dated past git's 90-day expiry is still read. (A 205-commit
/// fixture dated 138 days back came back as 7 entries from `git reflog -n200`
/// itself, with or without `--date=unix`, so a large old fixture is not a
/// sound way to test the 200 cap; the cap test above uses recent dates.)
#[test]
fn a_reflog_entry_a_hundred_days_old_is_still_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    let repo = path.to_str().unwrap();
    let old = i64::try_from(ids::now_millis() / 1000).unwrap() - 100 * 86_400;
    git_in(&path, &["init", "-b", "main"]);
    std::fs::write(path.join("a.txt"), "a").unwrap();
    git_in(&path, &["add", "a.txt"]);
    run_git_at(&path, old, &["commit", "-m", "first"]);
    run_git_at(&path, old + 10, &["checkout", "-b", "side"]);
    let entries = crate::engine::git_reader::GitReader::get_reflog(repo, 50).unwrap();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0].timestamp, old + 10);
}
