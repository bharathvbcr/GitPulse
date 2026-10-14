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
