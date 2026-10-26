//! Real persistent fixture approvals, isolated from the developer's profile.
use gitpulse_lib::procguard::LockedSpawn;
use std::path::Path;
use std::process::Command;

/// The variables every platform reads GitPulse's profile from, pointed into
/// `home`. A process that should see an [`approve`]d repository needs all of
/// them: Linux prefers `XDG_CONFIG_HOME` to `HOME`, and CI runners set it.
pub fn profile_env(home: &Path) -> [(&'static str, std::path::PathBuf); 3] {
    [
        ("HOME", home.to_path_buf()),
        ("APPDATA", home.join("AppData")),
        ("XDG_CONFIG_HOME", home.join(".config")),
    ]
}

pub fn approve(repo: &Path, home: &Path) {
    std::fs::create_dir_all(home).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_trust::approve_subprocess_fixture",
            "--ignored",
        ])
        .envs(profile_env(home))
        .env("GITPULSE_TEST_GRANT_REPO", repo)
        .status_locked()
        .unwrap();
    assert!(status.success(), "isolated fixture approval failed");
}

#[test]
#[ignore = "fixture helper invoked explicitly in an isolated subprocess"]
fn approve_subprocess_fixture() {
    let repo = std::env::var("GITPULSE_TEST_GRANT_REPO").expect("fixture repository");
    let view = gitpulse_lib::repository_trust::inspect(&repo).unwrap();
    gitpulse_lib::repository_trust::grant(&repo, &view.identity, true).unwrap();
    // The fixture must end up covering the repository, not just the checkout
    // it named: worktrees added by a test are otherwise refused.
    assert_eq!(
        gitpulse_lib::repository_trust::inspect(&repo)
            .unwrap()
            .scope,
        gitpulse_lib::repository_trust::TrustScope::Repository
    );
}
