//! Adversarial checks on sequencer detection.

use super::*;

fn paths_in(dir: &Path) -> Vec<PathBuf> {
    PROBE_ORDER.iter().map(|probe| dir.join(probe)).collect()
}

fn classify_todo(todo: &[u8]) -> (Option<OperationKind>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    std::fs::create_dir_all(&paths[IDX_SEQUENCER]).unwrap();
    std::fs::write(paths[IDX_SEQUENCER].join("todo"), todo).unwrap();
    let mut warnings = Vec::new();
    let kind = classify(dir.path(), &paths, &mut warnings)
        .unwrap()
        .map(|(kind, _)| kind);
    (kind, warnings)
}

/// A revert sequence whose todo is larger than the control-file cap is still a
/// revert: the first step is at the head of the file, and offering
/// `cherry-pick --abort` for it strands the user.
#[test]
fn an_oversized_revert_todo_is_still_a_revert() {
    let mut todo = b"revert abc1234 first\n".to_vec();
    let line = b"revert abc1234 more of them\n";
    while (todo.len() as u64) <= MAX_CONTROL_FILE_BYTES + 1024 {
        todo.extend_from_slice(line);
    }
    let (kind, _) = classify_todo(&todo);
    assert_eq!(kind, Some(OperationKind::Revert));
}

/// A todo that is nothing but comments up to the cap is not an operation we
/// can name, but it is not idle either: fail closed.
#[test]
fn an_oversized_comment_only_todo_does_not_read_as_idle() {
    let mut todo = Vec::new();
    while (todo.len() as u64) <= MAX_CONTROL_FILE_BYTES + 1024 {
        todo.extend_from_slice(b"# padding padding padding padding padding padding\n");
    }
    let (kind, warnings) = classify_todo(&todo);
    assert!(kind.is_some(), "an unreadable sequencer must not look idle");
    assert!(!warnings.is_empty());
}

#[test]
fn mixed_empty_and_exec_todos_classify_by_first_step_and_never_idle_a_live_step() {
    assert_eq!(
        classify_todo(b"pick a one\nrevert b two\n").0,
        Some(OperationKind::CherryPick)
    );
    assert_eq!(
        classify_todo(b"revert a one\npick b two\n").0,
        Some(OperationKind::Revert)
    );
    assert_eq!(classify_todo(b"").0, None);
    assert_eq!(classify_todo(b"\n  \n# c\n").0, None);
    let (kind, warnings) = classify_todo(b"# c\nexec make test\npick a one\n");
    assert_eq!(kind, Some(OperationKind::CherryPick));
    assert!(!warnings.is_empty(), "an exec-led todo must warn");
    assert_eq!(
        classify_todo(b"\xff\xfe revert\n").0,
        Some(OperationKind::CherryPick),
        "non-UTF-8 junk must not panic or read as idle"
    );
}

#[test]
fn an_abort_safety_file_alone_is_not_an_operation() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    std::fs::create_dir_all(&paths[IDX_SEQUENCER]).unwrap();
    std::fs::write(paths[IDX_SEQUENCER].join("abort-safety"), "abc\n").unwrap();
    std::fs::write(paths[IDX_SEQUENCER].join("head"), "abc\n").unwrap();
    let mut warnings = Vec::new();
    assert!(classify(dir.path(), &paths, &mut warnings)
        .unwrap()
        .is_none());
    assert!(warnings.is_empty());
}

/// `.git/sequencer` belongs to one worktree. A linked worktree must not report
/// the main worktree's parked sequence, and must report its own.
#[test]
fn the_sequencer_is_per_worktree() {
    let main = crate::test_support::repo_parked_in_a_sequence(false);
    let main_path = main.path().canonicalize().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let linked = parent
        .path()
        .join("linked")
        .canonicalize()
        .unwrap_or_else(|_| parent.path().join("linked"));
    crate::test_support::git_in(
        &main_path,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let linked = linked.canonicalize().unwrap();
    crate::test_support::trust_repo(&linked);
    assert_eq!(
        detect_kind(&main_path).unwrap(),
        Some(OperationKind::CherryPick)
    );
    assert_eq!(
        detect_kind(&linked).unwrap(),
        None,
        "main's sequencer leaked"
    );

    // Park the linked worktree on its own sequence, in its own git dir.
    let (private, _common) = crate::repository_trust::git_directories(&linked).unwrap();
    assert_ne!(private, main_path.join(".git"));
    std::fs::create_dir_all(private.join("sequencer")).unwrap();
    std::fs::write(private.join("sequencer/todo"), "revert abc1234 x\n").unwrap();
    assert_eq!(detect_kind(&linked).unwrap(), Some(OperationKind::Revert));
    assert_eq!(
        detect_kind(&main_path).unwrap(),
        Some(OperationKind::CherryPick),
        "the linked worktree's sequencer must not change main's"
    );
}
