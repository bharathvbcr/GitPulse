use super::*;

#[test]
fn restore_turn_checkpoint_and_non_standard_ref_inventory_commands_work() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    crate::test_support::git_in(repo, &["init", "-b", "main", "."]);
    crate::test_support::git_in(repo, &["config", "user.name", "Tester"]);
    crate::test_support::git_in(repo, &["config", "user.email", "test@example.com"]);
    std::fs::write(repo.join("tracked.txt"), "v1\n").unwrap();
    crate::test_support::git_in(repo, &["add", "tracked.txt"]);
    crate::test_support::git_in(repo, &["commit", "-m", "init"]);
    crate::test_support::trust_repo(repo);

    // Capture a turn checkpoint after editing tracked.txt.
    std::fs::write(repo.join("tracked.txt"), "v2-checkpoint\n").unwrap();
    let repo_str = repo.to_str().unwrap().to_string();
    let capture = crate::engine::turn_checkpoint::capture_turn_checkpoint(
        &repo_str,
        &crate::engine::turn_checkpoint::CaptureRequest {
            event: crate::engine::turn_checkpoint::TurnEvent::TurnEnd,
            session_id: Some("sess-1"),
            label: Some("turn 1"),
        },
        &crate::engine::turn_checkpoint::CaptureConfig::production(),
    );
    let cp_ref = capture.ref_name.expect("checkpoint ref created");

    // Also create a non-standard backup ref.
    let head_oid = crate::engine::git_cli::git_text(repo, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    crate::test_support::git_in(repo, &["update-ref", "refs/backup/save-1", &head_oid]);

    // Inventory command sees both refs/gitpulse and refs/backup without mutating them.
    let inv = tauri::async_runtime::block_on(cmd_non_standard_ref_inventory(repo_str.clone()))
        .expect("cmd_non_standard_ref_inventory");
    assert!(inv.total_refs >= 2);
    assert!(inv
        .namespaces
        .iter()
        .any(|n| n.namespace == "refs/backup" && n.count == 1));
    assert!(inv
        .namespaces
        .iter()
        .any(|n| n.namespace == "refs/gitpulse" && n.count >= 1));

    // Mutate tracked.txt further and verify restore refuses without force, then succeeds with force.
    std::fs::write(repo.join("tracked.txt"), "v3-dirty\n").unwrap();
    let refused = tauri::async_runtime::block_on(cmd_restore_turn_checkpoint(
        repo_str.clone(),
        None,
        cp_ref.clone(),
        Some(false),
    ));
    assert!(refused.is_err());

    let restored = tauri::async_runtime::block_on(cmd_restore_turn_checkpoint(
        repo_str,
        None,
        cp_ref.clone(),
        Some(true),
    ))
    .expect("cmd_restore_turn_checkpoint with force");
    assert_eq!(restored.checkpoint_ref, cp_ref);
    assert_eq!(restored.branch.as_deref(), Some("refs/heads/main"));
    assert!(restored.forced);
    assert_eq!(
        std::fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "v2-checkpoint\n"
    );
}
