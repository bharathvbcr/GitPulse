//! Adversarial review of the transcript-row dedup (lane W4-L1, ft-001bf7fc).
//! Every fixture is synthetic. A test that FAILS here names a defect or a stated
//! limit of the lane code; a test that passes records a held invariant.

use super::*;
use crate::ledger::{Draft, TranscriptAdvance};

fn crows(w: &str) -> Vec<String> {
    let mut objects: Vec<String> = rows(w)
        .iter()
        .filter(|e| e.actor_id.as_deref() == Some("claude-code"))
        .filter_map(|e| e.object.clone())
        .map(|o| o.rsplit('/').next().unwrap().to_string())
        .collect();
    objects.sort();
    objects
}

fn edit(w: &str, second: u32, name: &str) -> String {
    claude_edit(w, "C", second, &format!("{w}/{name}"))
}

fn forget(w: &str, path: &std::path::Path) {
    ledger::forget_transcripts(w, w, &[ledger::transcript_key(path)]).unwrap();
}

// 1a. A cursor that exists but sits behind the real position. The trigger is
// "no cursor, or a row before `from`"; a cursor behind recorded rows meets only
// rows at or after `from`, so nothing is looked up.
#[test]
fn a1a_a_stale_cursor_behind_recorded_rows_replays_them() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    let lines = [
        edit(w1, 1, "a.rs"),
        edit(w1, 2, "b.rs"),
        edit(w1, 3, "c.rs"),
    ];
    write_lines(&lpath, &lines);
    let full = std::fs::metadata(&lpath).unwrap().len();
    let first = lines[0].len() as u64 + 1;
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        assert_eq!(crows(w1), ["a.rs", "b.rs", "c.rs"]);
        // A cursor restored to an earlier position (an older copy of the
        // ledger's cursor table, a hand repair): same file, offset behind.
        ledger::advance_transcripts(
            w1,
            w1,
            vec![TranscriptAdvance {
                file_key: ledger::transcript_key(&lpath),
                from: Some(full),
                to: first,
                fingerprint: None,
                drafts: vec![],
            }],
        )
        .unwrap();
        pair.pass();
    });
    assert_eq!(
        crows(w1),
        ["a.rs", "b.rs", "c.rs"],
        "a stale cursor replayed lines"
    );
}

// 1b. Rotation: the file is rewritten so the fingerprint no longer holds and
// bytes shift. A line that did not change but moved gets a new identity.
#[test]
fn a1b_a_rewrite_that_shifts_bytes_replays_the_unchanged_lines_stated_limit() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(
        &lpath,
        &[
            edit(w1, 1, "a.rs"),
            edit(w1, 2, "b.rs"),
            edit(w1, 3, "c.rs"),
        ],
    );
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        let before = ledger::transcript_progress(w1, w1).unwrap();
        eprintln!(
            "a1b cursor before: {:?} {:?}",
            before.offsets, before.fingerprints
        );
        // First line replaced by a longer one, last line changed: the head and
        // the tail fingerprint both fail, so the read starts at 0.
        write_lines(
            &lpath,
            &[
                edit(w1, 9, "a-much-longer-name.rs"),
                edit(w1, 2, "b.rs"),
                edit(w1, 4, "c2.rs"),
            ],
        );
        let out = pair.pass();
        eprintln!(
            "a1b second pass: recorded={} skipped={} err={:?}",
            out.recorded, out.skipped_lines, out.error
        );
        let after = ledger::transcript_progress(w1, w1).unwrap();
        eprintln!(
            "a1b cursor after: {:?} len={}",
            after.offsets,
            std::fs::metadata(&lpath).unwrap().len()
        );
    });
    // The stated limit: a row's identity is its line's byte offset and digest,
    // so b.rs, unchanged but moved by the longer first line, is a new row. The
    // alternative (an occurrence count among identical lines) cannot be known
    // by a read that resumes mid-file.
    assert_eq!(
        crows(w1),
        [
            "a-much-longer-name.rs",
            "a.rs",
            "b.rs",
            "b.rs",
            "c.rs",
            "c2.rs"
        ],
        "an unchanged line whose offset moved is recorded again (stated limit)"
    );
}

// 1d. Rotation detected by the fingerprint's head and tail only: a rewrite that
// grows past the cursor and keeps the last line's tail (lines end alike, the
// timestamp included) is read from the old offset or rebased to the file's end,
// so the changed lines are never recorded. Pre-existing (ft-cb252563): a lost
// row, not a duplicate, and not the lane's doing.
#[test]
#[ignore = "pre-existing: ft-cb252563"]
fn a1d_a_rewrite_that_grows_past_the_cursor_and_keeps_the_tail_loses_its_new_lines_preexisting() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(
        &lpath,
        &[
            edit(w1, 1, "a.rs"),
            edit(w1, 2, "b.rs"),
            edit(w1, 3, "c.rs"),
        ],
    );
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        write_lines(
            &lpath,
            &[
                edit(w1, 9, "a-much-longer-name.rs"),
                edit(w1, 2, "b.rs"),
                edit(w1, 3, "c2.rs"),
            ],
        );
        pair.pass();
    });
    let files = crows(w1);
    assert!(
        files.contains(&"a-much-longer-name.rs".to_string()),
        "the new first line was never recorded: {files:?}"
    );
}

// 1c. A cursor deleted by forget, the file grown meanwhile.
#[test]
fn a1c_forget_then_the_file_returns_with_one_more_line_records_exactly_that_line() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(&lpath, &[edit(w1, 1, "a.rs"), edit(w1, 2, "b.rs")]);
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        forget(w1, &lpath);
        append_text(&lpath, &(edit(w1, 3, "c.rs") + "\n"));
        pair.pass();
    });
    assert_eq!(crows(w1), ["a.rs", "b.rs", "c.rs"]);
}

// 2a. One file reached through two spellings of its root.
#[cfg(unix)]
#[test]
fn a2a_one_file_through_a_symlinked_root_is_not_recorded_twice() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let holder = tempfile::tempdir().unwrap();
    let real = holder.path().join("real");
    let link = holder.path().join("link");
    write_lines(
        &real.join("slug/C.jsonl"),
        &[edit(w1, 1, "a.rs"), edit(w1, 2, "b.rs")],
    );
    std::os::unix::fs::symlink(&real, &link).unwrap();
    with_roots(Some(&real), None, || pair.pass());
    with_roots(Some(&link), None, || pair.pass());
    assert_eq!(
        crows(w1),
        ["a.rs", "b.rs"],
        "the second spelling replayed the file"
    );
}

// 2b. Two identical calls inside one line: distinct ordinals, so two rows, and
// they stay two after a reset.
#[test]
fn a2b_two_identical_calls_in_one_line_stay_two_rows_after_a_reset() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    let file = format!("{w1}/same.rs");
    let block = serde_json::json!({"type":"tool_use","name":"Edit","input":{"file_path":file}});
    let line = serde_json::json!({
        "type":"assistant","sessionId":"C","timestamp":stamp(1),"version":"2.1.241","cwd":w1,
        "message":{"content":[block.clone(), block]}
    })
    .to_string();
    write_lines(&lpath, &[line]);
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        assert_eq!(crows(w1).len(), 2);
        forget(w1, &lpath);
        pair.pass();
    });
    assert_eq!(crows(w1).len(), 2);
}

// 2c. The identity survives the redactor, the object cap and a huge detail, and
// two different rows never share a stored identity.
#[test]
fn a2c_the_stored_identity_is_the_drafted_identity_under_redaction_and_caps() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let secret_session = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    let mut expected = Vec::new();
    let mut drafts = Vec::new();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for i in 0..300u64 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let row = ledger::TranscriptRow {
            file_key: format!("{:016x}", state),
            offset: state >> 40,
            ordinal: (i % 3) as usize,
            digest: state.rotate_left(17),
        };
        let branch = match i % 3 {
            0 => "feature/token=abc123abc123abc123abc123".to_string(),
            1 => "x".repeat(200_000),
            _ => format!("b\u{1}\\u0041-{i}"),
        };
        expected.push(row.key());
        drafts.push(Draft {
            repo_path: repo.clone(),
            action: "session.edit".into(),
            object: Some(format!("{repo}/{}", "d/".repeat(3000))),
            session_id: Some(if i % 2 == 0 {
                secret_session.into()
            } else {
                format!("S{i}")
            }),
            detail_json: Some(
                serde_json::json!({
                    "source":"transcript","git_branch":branch,
                    ledger::TRANSCRIPT_ROW_KEY: row.key(),
                })
                .to_string(),
            ),
            ..Default::default()
        });
    }
    let advance = |drafts: Vec<Draft>, from: Option<u64>, to: u64| TranscriptAdvance {
        file_key: "ffff".into(),
        from,
        to,
        fingerprint: None,
        drafts,
    };
    let first =
        ledger::advance_transcripts(&repo, &repo, vec![advance(drafts.clone(), None, 10)]).unwrap();
    assert_eq!(first, 300);
    let stored: Vec<String> = ledger::tail(&repo, 0, 1000)
        .unwrap()
        .iter()
        .map(|e| {
            let detail: serde_json::Value =
                serde_json::from_str(e.detail_json.as_deref().unwrap_or("{}")).unwrap_or_default();
            detail
                .get(ledger::TRANSCRIPT_ROW_KEY)
                .and_then(|v| v.as_str())
                .unwrap_or("<missing or cut>")
                .to_string()
        })
        .collect();
    let mut want = expected.clone();
    want.sort();
    let mut got = stored.clone();
    got.sort();
    assert_eq!(got, want, "a stored identity differs from the drafted one");
    // Cursor lost, the same rows offered again: nothing recorded.
    ledger::forget_transcripts(&repo, &repo, &["ffff".into()]).unwrap();
    let again = ledger::advance_transcripts(&repo, &repo, vec![advance(drafts, None, 10)]).unwrap();
    assert_eq!(again, 0, "redaction or caps broke the identity match");
}

// 3. Cost: 100k rows in one session, a reset triggers the lookup.
#[test]
fn a3_a_reset_over_100k_rows_is_correct_and_timed() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    let lines: Vec<String> = (0..100_000u32)
        .map(|i| edit(w1, i % 60, &format!("f{i}.rs")))
        .collect();
    write_lines(&lpath, &lines);
    with_roots(Some(claude.path()), None, || {
        let t = std::time::Instant::now();
        let first = pair.pass();
        eprintln!("a3 first pass: {:?} {}", t.elapsed(), first.recorded);
        assert_eq!(first.recorded, 100_000, "{first:?}");
        forget(w1, &lpath);
        let t = std::time::Instant::now();
        let second = pair.pass();
        eprintln!(
            "a3 reset pass (100k held): {:?} recorded={}",
            t.elapsed(),
            second.recorded
        );
        assert_eq!(second.recorded, 0, "{second:?}");
        eprintln!("a3 CatchUp after reset: {second:?}");
    });
}

// 3b. One session id shared by both worktrees: the lookup is keyed by session
// first, so a reset of one worktree scans the other's rows too.
#[test]
fn a3b_a_session_shared_across_worktrees_resets_each_without_cross_talk() {
    let pair = Pair::new();
    let [w1, w2] = &pair.wt;
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    let mut lines = Vec::new();
    for i in 0..3000u32 {
        lines.push(edit(w1, i % 60, &format!("one{i}.rs")));
        lines.push(edit(w2, i % 60, &format!("two{i}.rs")));
    }
    write_lines(&lpath, &lines);
    with_roots(Some(claude.path()), None, || {
        assert_eq!(pair.pass().recorded, 6000);
        forget(w1, &lpath);
        let t = std::time::Instant::now();
        let again = pair.pass();
        eprintln!(
            "a3b reset of w1 (w2 holds the same session): {:?}",
            t.elapsed()
        );
        assert_eq!(again.recorded, 0, "{again:?}");
    });
    let count: i64 = rusqlite::Connection::open(ledger::ledger_path(w1))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE action LIKE 'session.%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 6000);
}

// 4. Another connection holds the write lock across the pass.
#[test]
fn a4_a_foreign_writer_holding_the_lock_delays_the_pass_without_losing_or_duplicating() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(&lpath, &[edit(w1, 1, "a.rs"), edit(w1, 2, "b.rs")]);
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        forget(w1, &lpath);
        let db = ledger::ledger_path(w1);
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let other = rusqlite::Connection::open(&db).unwrap();
                other
                    .busy_timeout(std::time::Duration::from_secs(30))
                    .unwrap();
                other.execute_batch("BEGIN IMMEDIATE").unwrap();
                held_tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1500));
                other.execute_batch("COMMIT").unwrap();
            });
            held_rx.recv().unwrap();
            let out = pair.pass();
            assert_eq!(out.error, "", "{out:?}");
            assert_eq!(out.recorded, 0, "{out:?}");
        });
    });
    assert_eq!(crows(w1), ["a.rs", "b.rs"]);
}

// 5. Cursor reset mid-file: half recorded, then the file grows and the cursor goes.
#[test]
fn a5_a_reset_with_half_the_file_recorded_records_exactly_the_other_half() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(
        &lpath,
        &[
            edit(w1, 1, "a.rs"),
            edit(w1, 2, "b.rs"),
            edit(w1, 3, "c.rs"),
        ],
    );
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        append_text(
            &lpath,
            &[
                edit(w1, 4, "d.rs"),
                edit(w1, 5, "e.rs"),
                edit(w1, 6, "f.rs"),
            ]
            .join("\n"),
        );
        append_text(&lpath, "\n");
        forget(w1, &lpath);
        let out = pair.pass();
        assert_eq!(out.recorded, 3, "{out:?}");
    });
    assert_eq!(crows(w1), ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"]);
}

// 6. Skipped duplicates leave no trace in the pass result.
#[test]
fn a6_a_skipped_duplicate_is_visible_nowhere_in_the_pass_result() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(&lpath, &[edit(w1, 1, "a.rs"), edit(w1, 2, "b.rs")]);
    let (clean, reset) = with_roots(Some(claude.path()), None, || {
        let clean = pair.pass();
        forget(w1, &lpath);
        (clean, pair.pass())
    });
    eprintln!("a6 first {clean:?}\na6 reset {reset:?}");
    assert_eq!(reset.recorded, 0);
    assert_eq!(
        reset.duplicates_refused, 2,
        "two rows were skipped as already held, and the pass says so nowhere"
    );
}
