//! Round-2 adversarial review of the deterministic transcript ulid and of root
//! retirement (lane W4-L1). Synthetic fixtures only.

use super::*;
use std::path::{Path, PathBuf};

fn forget_file(w: &str, path: &Path) {
    ledger::forget_transcripts(w, w, &[ledger::transcript_key(path)]).unwrap();
}

fn claude_rows(w: &str) -> usize {
    rows(w)
        .iter()
        .filter(|e| e.actor_id.as_deref() == Some("claude-code"))
        .count()
}

fn three_lines(w: &str) -> Vec<String> {
    (1..=3)
        .map(|n| claude_edit(w, "C", n, &format!("{w}/f{n}.rs")))
        .collect()
}

// Attack 6: a ledger holding pre-change rows (random ulid, no identity key)
// re-records once after a reset (the stated limit); the second reset records 0.
#[test]
fn u1_pre_change_random_ulid_rows_rerecord_once_then_never() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(&lpath, &three_lines(w1));
    with_roots(Some(claude.path()), None, || {
        assert_eq!(pair.pass().recorded, 3);
        // Turn every recorded row into what an older build wrote.
        let conn = rusqlite::Connection::open(ledger::ledger_path(w1)).unwrap();
        conn.execute(
            "UPDATE events SET ulid = printf('01J%023d', id),
                 detail_json = json_remove(detail_json, '$.transcript_row')",
            [],
        )
        .unwrap();
        drop(conn);
        forget_file(w1, &lpath);
        let first = pair.pass();
        assert_eq!(
            (first.recorded, first.duplicates_refused),
            (3, 0),
            "{first:?}"
        );
        assert_eq!(claude_rows(w1), 6, "the stated one-time re-record");
        forget_file(w1, &lpath);
        let second = pair.pass();
        assert_eq!(
            (second.recorded, second.duplicates_refused),
            (0, 3),
            "{second:?}"
        );
        assert_eq!(claude_rows(w1), 6);
    });
}

// Attack 2: one transcript read through two spellings of its root.
#[test]
fn i1_a_symlinked_root_spelling_records_nothing_twice() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    write_lines(&claude.path().join("slug/C.jsonl"), &three_lines(w1));
    let link_parent = tempfile::tempdir().unwrap();
    let link: PathBuf = link_parent.path().join("alias");
    std::os::unix::fs::symlink(claude.path(), &link).unwrap();
    with_roots(Some(claude.path()), None, || {
        assert_eq!(pair.pass().recorded, 3);
    });
    with_roots(Some(&link), None, || {
        let out = pair.pass();
        assert_eq!(out.recorded, 0, "{out:?}");
    });
    assert_eq!(claude_rows(w1), 3);
}

// Attack 5: a corrupt run counter in the marker (u32::MAX, overflowing, junk,
// and a marker with thousands of lines) must be parsed safely.
fn marker_key(w: &str, needle: &Path) -> String {
    let needle = needle.to_string_lossy().into_owned();
    ledger::transcript_progress(w, w)
        .unwrap()
        .fingerprints
        .into_iter()
        .find(|(_, text)| text.contains(&needle))
        .map(|(key, _)| key)
        .expect("a marker naming the root")
}

fn poison_marker(w: &str, key: &str, text: String) {
    let progress = ledger::transcript_progress(w, w).unwrap();
    ledger::advance_transcripts(
        w,
        w,
        vec![ledger::TranscriptAdvance {
            file_key: key.to_string(),
            from: progress.offsets.get(key).copied(),
            to: 1,
            fingerprint: Some(text),
            drafts: vec![],
        }],
    )
    .unwrap();
}

fn two_roots() -> (Pair, tempfile::TempDir, tempfile::TempDir, PathBuf, PathBuf) {
    let pair = Pair::new();
    let w1 = pair.wt[0].clone();
    let claude = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let a = scratch.path().join("a");
    let b = scratch.path().join("b");
    for (root, thread) in [(&a, "A"), (&b, "B")] {
        let path = rollout_path(root, thread);
        write_lines(&path, &[session_meta(thread, &w1), run(1, "ls", &w1)]);
        with_roots(Some(claude.path()), Some(root), || pair.pass());
    }
    (pair, claude, scratch, a, b)
}

#[test]
fn m1_a_counter_at_u32_max_does_not_panic_the_pass() {
    let (pair, claude, scratch, a, b) = two_roots();
    let w1 = &pair.wt[0];
    let key = marker_key(w1, &a);
    std::fs::remove_dir_all(scratch.path().join("a")).unwrap();
    for count in ["4294967295", "99999999999999999999", "-1", "x", ""] {
        poison_marker(
            w1,
            &key,
            format!(
                "{}\u{1f}{count}\n{}",
                a.to_string_lossy(),
                b.to_string_lossy()
            ),
        );
        let out = with_roots(Some(claude.path()), Some(&b), || pair.pass());
        assert_eq!(out.error, "", "{count}: {out:?}");
    }
}

#[test]
fn m2_a_marker_of_thousands_of_roots_is_bounded_and_the_pass_finishes() {
    let (pair, claude, _scratch, a, b) = two_roots();
    let w1 = &pair.wt[0];
    let key = marker_key(w1, &a);
    let junk: Vec<String> = (0..5000)
        .map(|i| format!("/nonexistent/junk-{i}"))
        .collect();
    poison_marker(
        w1,
        &key,
        format!(
            "{}\n{}\n{}",
            junk.join("\n"),
            a.to_string_lossy(),
            b.to_string_lossy()
        ),
    );
    let started = std::time::Instant::now();
    let out = with_roots(Some(claude.path()), Some(&b), || pair.pass());
    assert_eq!(out.error, "", "{out:?}");
    let stored = ledger::transcript_progress(w1, w1).unwrap().fingerprints[&key].clone();
    assert!(
        stored.lines().count() <= 8,
        "marker kept {} lines",
        stored.lines().count()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(60));
}

// Attack 5 flapping: absent 4 passes, present 1, absent 5: retired on exactly
// the fifth absent pass after the restart, not before.
#[test]
fn f1_flapping_root_retires_on_the_fifth_consecutive_absent_pass() {
    let (pair, claude, scratch, a, b) = two_roots();
    let w1 = &pair.wt[0];
    let rollout_a = rollout_path(&a, "A");
    let has = |p: &Path| {
        ledger::transcript_progress(w1, w1)
            .unwrap()
            .offsets
            .contains_key(&ledger::transcript_key(p))
    };
    let gone = scratch.path().join("a");
    let parked = scratch.path().join("parked");
    std::fs::rename(&gone, &parked).unwrap();
    for _ in 0..4 {
        with_roots(Some(claude.path()), Some(&b), || pair.pass());
    }
    std::fs::rename(&parked, &gone).unwrap();
    with_roots(Some(claude.path()), Some(&b), || pair.pass());
    std::fs::rename(&gone, &parked).unwrap();
    for n in 1..=4 {
        with_roots(Some(claude.path()), Some(&b), || pair.pass());
        assert!(has(&rollout_a), "retired early, after {n} absent passes");
    }
    with_roots(Some(claude.path()), Some(&b), || pair.pass());
    assert!(!has(&rollout_a), "not retired on the fifth absent pass");
}
