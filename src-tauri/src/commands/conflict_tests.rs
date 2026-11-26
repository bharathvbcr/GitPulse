use super::*;

#[test]
fn proposal_commands_load_and_build_resolved_tree() {
    use crate::engine::land_proposals::{create_proposal, CreateProposalRequest};

    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path();
    crate::test_support::git_in(repo, &["init", "-b", "main", "."]);
    crate::test_support::git_in(repo, &["config", "user.name", "Tester"]);
    crate::test_support::git_in(repo, &["config", "user.email", "test@example.com"]);
    std::fs::write(repo.join("shared.txt"), "line1\nbase\nline3\n").unwrap();
    crate::test_support::git_in(repo, &["add", "shared.txt"]);
    crate::test_support::git_in(repo, &["commit", "-m", "base"]);

    crate::test_support::git_in(repo, &["checkout", "-b", "feat/proposal"]);
    std::fs::write(repo.join("shared.txt"), "line1\ntheirs\nline3\n").unwrap();
    crate::test_support::git_in(repo, &["add", "shared.txt"]);
    crate::test_support::git_in(repo, &["commit", "-m", "feat"]);
    let feat_oid = crate::engine::git_cli::git_text(repo, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    crate::test_support::git_in(repo, &["checkout", "main"]);
    std::fs::write(repo.join("shared.txt"), "line1\nours\nline3\n").unwrap();
    crate::test_support::git_in(repo, &["add", "shared.txt"]);
    crate::test_support::git_in(repo, &["commit", "-m", "main"]);
    let main_oid = crate::engine::git_cli::git_text(repo, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    crate::test_support::trust_repo(repo);

    let summary = create_proposal(
        repo,
        &CreateProposalRequest {
            branch: "main".to_string(),
            tip_oid: main_oid,
            source_commit_oid: feat_oid,
            resolver: "test-model".to_string(),
            requested_by: "tester".to_string(),
            files: vec![crate::engine::land_proposals::ProposedFileInput {
                path: "shared.txt".to_string(),
                mode: "100644".to_string(),
                bytes: b"line1\nmerged-by-proposal\nline3\n".to_vec(),
            }],
            expected_old_oid: None,
        },
    )
    .expect("create_proposal");

    let repo_str = repo.to_str().unwrap().to_string();
    let snapshot = tauri::async_runtime::block_on(cmd_load_conflict_proposal(
        repo_str.clone(),
        summary.ref_name.clone(),
    ))
    .expect("cmd_load_conflict_proposal");
    assert_eq!(snapshot.proposal_ref, summary.ref_name);
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(snapshot.files[0].file_path, "shared.txt");
    assert!(snapshot.files[0].proposed_marker_free);

    let built = tauri::async_runtime::block_on(cmd_build_resolved_proposal_tree(
        repo_str,
        BuildProposalTreeInput {
            proposal_ref: summary.ref_name,
            expected_proposal_commit_oid: Some(summary.proposal_commit_oid),
            target_tip_oid: None,
            resolutions: vec![ProposalFileResolutionInput {
                file_path: "shared.txt".to_string(),
                revision: Some(snapshot.files[0].snapshot.revision.clone()),
                choice: ProposalResolutionChoiceInput::AcceptProposal,
            }],
        },
    ))
    .expect("cmd_build_resolved_proposal_tree");
    assert_eq!(built.resolved_files, vec!["shared.txt".to_string()]);
    assert!(!built.tree_oid.is_empty());
}
