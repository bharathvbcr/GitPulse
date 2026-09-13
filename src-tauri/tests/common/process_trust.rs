//! Real persistent fixture approvals, isolated from the developer's profile.
use std::path::Path;
use std::process::Command;

pub fn approve(repo: &Path, home: &Path) {
    std::fs::create_dir_all(home).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_trust::approve_subprocess_fixture",
            "--ignored",
        ])
        .env("HOME", home)
        .env("APPDATA", home.join("AppData"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GITPULSE_TEST_GRANT_REPO", repo)
        .status()
        .unwrap();
    assert!(status.success(), "isolated fixture approval failed");
}

#[test]
#[ignore = "fixture helper invoked explicitly in an isolated subprocess"]
fn approve_subprocess_fixture() {
    let repo = std::env::var("GITPULSE_TEST_GRANT_REPO").expect("fixture repository");
    let view = gitpulse_lib::repository_trust::inspect(&repo).unwrap();
    gitpulse_lib::repository_trust::grant(&repo, &view.identity, true).unwrap();
    assert!(
        gitpulse_lib::repository_trust::inspect(&repo)
            .unwrap()
            .trusted
    );
}
