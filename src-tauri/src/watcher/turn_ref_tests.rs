//! A turn checkpoint ref write (`refs/gitpulse/turns/`) must not refresh the
//! repository, and no other ref may stop doing so.

use super::*;

fn roots() -> (PathBuf, Vec<PathBuf>) {
    let git = PathBuf::from("/repo/.git");
    let roots = vec![git.clone()];
    (git, roots)
}

#[test]
fn a_write_under_the_turn_checkpoint_namespace_is_noise() {
    let (git, roots) = roots();
    for relative in [
        "refs/gitpulse/turns",
        "refs/gitpulse/turns/0123456789abcdef",
        "refs/gitpulse/turns/0123456789abcdef/0001794753657179",
        "refs/gitpulse/turns/0123456789abcdef/0001794753657179.lock",
        "logs/refs/gitpulse/turns/0123456789abcdef/0001794753657179",
    ] {
        assert!(
            is_git_internal_noise(&git.join(relative), &roots),
            "{relative} must not refresh the repository"
        );
    }
}

#[test]
fn every_other_ref_still_moves_the_repository() {
    let (git, roots) = roots();
    for relative in [
        "refs/heads/main",
        "refs/heads/gitpulse/turns/x",
        "refs/tags/v1",
        "refs/remotes/origin/main",
        "refs/gitpulse/ops/7",
        "refs/gitpulse",
        "refs/gitpulse/turnstile/1",
        "refs/turns/1",
        "packed-refs",
        "HEAD",
        "logs/HEAD",
    ] {
        let path = git.join(relative);
        assert!(
            !is_git_internal_noise(&path, &roots) || relative == "logs/HEAD",
            "{relative} must still be admitted"
        );
        if relative != "logs/HEAD" {
            assert_eq!(git_path_kind(&path, &roots), ChangeKind::Refs, "{relative}");
        }
    }
}

#[test]
fn a_checkpoint_in_the_common_dir_of_a_linked_worktree_is_noise_too() {
    let common = PathBuf::from("/repo/.git");
    let private = common.join("worktrees/feature");
    let roots = vec![private, common.clone()];
    assert!(is_git_internal_noise(
        &common.join("refs/gitpulse/turns/0123456789abcdef/0001"),
        &roots
    ));
    assert!(!is_git_internal_noise(
        &common.join("refs/gitpulse/ops/1"),
        &roots
    ));
}
