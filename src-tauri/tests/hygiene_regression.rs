use gitpulse_lib::storage::{scan_storage, ReclaimSafety};
use std::{fs, process::Command};

#[test]
fn untracked_environments_agent_state_and_scratch_are_not_safe_cleanup() {
    let root = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(root.path())
        .status()
        .unwrap()
        .success());
    for name in [
        ".venv",
        ".claude",
        ".devcouncil",
        ".terraform",
        "tmp",
        "logs",
        ".gopath",
        ".gomodcache",
    ] {
        fs::create_dir(root.path().join(name)).unwrap();
        fs::write(root.path().join(name).join("valuable-data"), b"keep me").unwrap();
    }
    let report = scan_storage(root.path().to_str().unwrap()).unwrap();
    assert_eq!(report.reclaim.len(), 8);
    for item in report.reclaim {
        assert_eq!(
            item.safety,
            ReclaimSafety::NeedsReview,
            "{} is not disposable merely because it is untracked",
            item.label
        );
        assert!(
            !item.action.starts_with("rm "),
            "must not suggest blanket deletion"
        );
    }
}

#[test]
fn capped_artifact_inventory_never_looks_complete_to_a_global_cleaner() {
    let root = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(root.path())
        .status()
        .unwrap()
        .success());
    for i in 0..70 {
        let path = root.path().join(format!("project-{i}/__pycache__"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("module.pyc"), b"generated").unwrap();
    }
    let report = scan_storage(root.path().to_str().unwrap()).unwrap();
    assert_eq!(report.artifacts.len(), 64);
    assert!(
        report.scan.truncated,
        "a capped candidate list cannot authorize a complete global sweep"
    );
}
