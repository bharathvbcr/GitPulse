//! Round-2 adversarial review of the transcript-row identity ulid (lane W4-L1,
//! ft-001bf7fc). A test that FAILS here names a defect; a test that passes
//! records a held invariant. Every fixture is synthetic.

use super::*;

fn repo_dir() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_str().unwrap().to_string();
    (dir, repo)
}

fn draft(
    repo: &str,
    worktree: &str,
    session: Option<&str>,
    key: &str,
    stamp: Option<&str>,
) -> Draft {
    Draft {
        repo_path: repo.to_string(),
        worktree_path: Some(worktree.to_string()),
        actor_kind: Some(ActorKind::Agent),
        actor_id: Some("claude-code".to_string()),
        session_id: session.map(str::to_string),
        action: "session.edit".to_string(),
        outcome: Some(Outcome::Ok),
        detail_json: Some(
            serde_json::json!({"source": "transcript", TRANSCRIPT_ROW_KEY: key}).to_string(),
        ),
        event_stamp: stamp.map(|at| EventStamp {
            occurred_at: at.to_string(),
            occurred_at_basis: "transcript".to_string(),
            task_basis: "unresolved".to_string(),
        }),
        ..Default::default()
    }
}

fn commit(repo: &str, drafts: Vec<Draft>) -> TranscriptCommit {
    advance_transcripts_family(
        repo,
        vec![(
            repo,
            vec![TranscriptAdvance {
                file_key: "f".to_string(),
                from: transcript_progress(repo, repo)
                    .unwrap()
                    .offsets
                    .get("f")
                    .copied(),
                to: 1,
                fingerprint: None,
                drafts,
            }],
        )],
    )
    .unwrap()
}

fn reset(repo: &str) {
    forget_transcripts(repo, repo, &["f".to_string()]).unwrap();
}

fn dropped(repo: &str) -> u64 {
    dropped_registry()
        .lock()
        .unwrap()
        .get(&ledger_path(repo))
        .copied()
        .unwrap_or(0)
}

fn count(repo: &str) -> usize {
    tail(repo, 0, 100_000).unwrap().len()
}

// Attack 1: a time-0 transcript row sorts first by ulid. Nothing that decides
// what is newest, what is paged, or what is pruned reads the ulid.
#[test]
fn a1_zero_time_ulids_do_not_misplace_rows_in_paging_newest_or_pruning() {
    let (_d, repo) = repo_dir();
    // Appended first, but dated by its line in 2026; appended second with no
    // line time, so its ulid time part is 0 and it sorts before the first.
    let c = commit(
        &repo,
        vec![
            draft(
                &repo,
                &repo,
                Some("S"),
                "aa:0:0:01",
                Some("2026-09-01T12:00:00.000Z"),
            ),
            draft(&repo, &repo, Some("S"), "aa:1:0:02", None),
        ],
    );
    assert_eq!(c.recorded, 2);
    let rows = tail(&repo, 0, 10).unwrap();
    assert!(
        rows[0].ulid > rows[1].ulid,
        "fixture: ulid order is inverted"
    );
    assert!(rows[1].ulid.starts_with("0000000000"), "{}", rows[1].ulid);
    assert!(rows[0].id < rows[1].id);
    // Paging by cursor is by id: the zero-time row is the newest.
    let newest = tail_newest(&repo, 0, 1).unwrap();
    assert_eq!(newest[0].id, rows[1].id);
    // Pruning is by ts_utc (the append clock), never the ulid's time: a row
    // whose ulid time is 0 is not 56 years old.
    let deleted = prune_events(&repo, 365, 1).unwrap();
    assert_eq!(deleted, 0, "a time-0 ulid must not look expired");
    assert_eq!(count(&repo), 2);
}

// Attack 2: identity inputs. The same line read under spellings of its repo,
// its worktree, and an absent or empty session.
#[test]
fn a2_repo_spelling_session_absent_vs_empty_are_one_identity() {
    let (_d, repo) = repo_dir();
    assert_eq!(
        commit(&repo, vec![draft(&repo, &repo, None, "aa:0:0:01", None)]).recorded,
        1
    );
    reset(&repo);
    let slash = format!("{repo}/");
    let c = advance_transcripts_family(
        &repo,
        vec![(
            &repo,
            vec![TranscriptAdvance {
                file_key: "f".into(),
                from: None,
                to: 1,
                fingerprint: None,
                drafts: vec![draft(&slash, &repo, Some(""), "aa:0:0:01", None)],
            }],
        )],
    )
    .unwrap();
    assert_eq!(
        (c.recorded, c.refused),
        (0, 1),
        "a trailing slash on the repo and an empty session are the same row"
    );
    assert_eq!(count(&repo), 1);
}

#[test]
fn a2_worktree_trailing_slash_is_the_same_row() {
    let (_d, repo) = repo_dir();
    assert_eq!(
        commit(
            &repo,
            vec![draft(&repo, &repo, Some("S"), "aa:0:0:01", None)]
        )
        .recorded,
        1
    );
    reset(&repo);
    let slash = format!("{repo}/");
    let c = commit(
        &repo,
        vec![draft(&repo, &slash, Some("S"), "aa:0:0:01", None)],
    );
    assert_eq!(
        c.recorded, 0,
        "worktree spelled with a trailing slash re-recorded"
    );
    assert_eq!(count(&repo), 1);
}

// Attack 3: a genuine re-read is never a failed write, however the key looks
// after the redactor has had its pass over the detail, and 1000 resets leave
// the persistent counters where they were.
#[test]
fn a3_genuine_rereads_are_never_counted_as_failed_writes_across_1000_resets() {
    let (_d, repo) = repo_dir();
    let keys: Vec<String> = (0u64..300)
        .map(|i| {
            format!(
                "{:016x}:{}:{}:{:016x}",
                ids::fnv1a64(&i.to_le_bytes()),
                i * 37,
                i % 3,
                ids::fnv1a64(&(i + 9999).to_le_bytes())
            )
        })
        .collect();
    let drafts = || {
        keys.iter()
            .map(|k| draft(&repo, &repo, Some("S"), k, Some("2026-09-01T12:00:00.000Z")))
            .collect::<Vec<_>>()
    };
    assert_eq!(commit(&repo, drafts()).recorded, 300);
    let before = dropped(&repo);
    let mut refused = 0;
    for _ in 0..1000 {
        reset(&repo);
        let c = commit(&repo, drafts());
        assert_eq!(c.recorded, 0);
        refused += c.refused;
    }
    assert_eq!(refused, 300 * 1000, "every re-read is a counted duplicate");
    assert_eq!(dropped(&repo), before, "a re-read is not a failed write");
    assert_eq!(count(&repo), 300);
    // The cursor table holds one row, however many resets ran.
    let progress = transcript_progress(&repo, &repo).unwrap();
    assert_eq!(progress.offsets.len(), 1);
}

// Attack 4a: the same ulid and the same transcript key but different content in
// the held row (a corrupted sibling): must be counted, not silently refused.
#[test]
fn a4_a_held_row_with_the_same_key_and_different_content_is_counted_not_silently_held() {
    let (_d, repo) = repo_dir();
    assert_eq!(
        commit(
            &repo,
            vec![draft(&repo, &repo, Some("S"), "aa:0:0:01", None)]
        )
        .recorded,
        1
    );
    with_conn(&repo, |conn| {
        conn.execute(
            "UPDATE events SET action = 'git.commit', object = 'other'",
            [],
        )
        .map_err(|e| LedgerError::new("fixture", e.to_string()))?;
        Ok(())
    })
    .unwrap();
    reset(&repo);
    let before = dropped(&repo);
    let c = commit(
        &repo,
        vec![draft(&repo, &repo, Some("S"), "aa:0:0:01", None)],
    );
    assert_eq!(c.recorded, 0);
    assert_eq!(
        (c.refused, dropped(&repo)),
        (0, before + 1),
        "a different row under the ulid was silently taken for a duplicate"
    );
}

// Attack 4b: the same through consolidation's import.
#[test]
fn a4_consolidation_counts_a_same_ulid_same_key_row_whose_content_differs() {
    let (_d, repo) = repo_dir();
    assert_eq!(
        commit(
            &repo,
            vec![draft(&repo, &repo, Some("S"), "aa:0:0:01", None)]
        )
        .recorded,
        1
    );
    let mut incoming = tail(&repo, 0, 10).unwrap().remove(0);
    incoming.action = "git.commit".to_string();
    incoming.object = Some("other".to_string());
    let before = dropped(&repo);
    let outcome = import_legacy_page(&repo, "k", "l", &[incoming], 1);
    let counted = dropped(&repo) > before || outcome.is_err();
    assert!(
        counted,
        "a different legacy row was skipped without a trace: {outcome:?}"
    );
}

// Attack 4c: a non-transcript legacy row under a held ulid still errors.
#[test]
fn a4_consolidation_refuses_a_different_non_transcript_row_under_a_held_ulid() {
    let (_d, repo) = repo_dir();
    assert_eq!(
        commit(
            &repo,
            vec![draft(&repo, &repo, Some("S"), "aa:0:0:01", None)]
        )
        .recorded,
        1
    );
    let mut incoming = tail(&repo, 0, 10).unwrap().remove(0);
    incoming.detail_json = Some("{}".to_string());
    incoming.action = "git.commit".to_string();
    let outcome = import_legacy_page(&repo, "k", "l", &[incoming], 1);
    assert_eq!(outcome.unwrap_err().code, "legacy_ulid_conflict");
}
