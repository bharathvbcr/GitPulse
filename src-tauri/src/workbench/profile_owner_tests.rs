//! The desktop app owns the task profile's schema: it upgrades an older profile
//! once, after the store's own backup and before the first read, and says so
//! loudly when it cannot. Manvi's refusal to upgrade a shared profile reaches
//! a person as an instruction, not as a bare code.

use super::*;
use crate::harness::protocol::WireError;
use crate::harness::HarnessError;
use std::path::Path;

fn degraded_now() -> Option<crate::supervisor::Degraded> {
    crate::supervisor::degraded()
        .into_iter()
        .find(|d| d.subsystem == PROFILE_UPGRADE_SUBSYSTEM)
}

fn version(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT version FROM work_meta WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap()
}

/// A profile as an older release left it: current tables, schema 11.
fn aged_profile(dir: &Path) -> PathBuf {
    let path = dir.join("workbench.sqlite");
    drop(Store::open(&path).unwrap());
    // `Store::open` only builds the profile; the workbench schema is created
    // by the first request, so make one before aging it.
    WorkbenchState::for_profile(&path)
        .board_request("items.list", "{}")
        .unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("UPDATE work_meta SET version = 11 WHERE id = 1", [])
        .unwrap();
    path
}

// One test, because the Diagnostics record is process-global and these steps
// read it in order.
#[test]
fn the_app_upgrades_an_older_profile_once_and_reports_when_it_cannot() {
    let dir = tempfile::tempdir().unwrap();
    let path = aged_profile(dir.path());
    let backup = dir.path().join("workbench.sqlite.pre-schema-11.bak");
    let blocker = dir
        .path()
        .join("workbench.sqlite.pre-schema-11.bak.partial");

    // A backup that cannot be written refuses the upgrade, visibly: the error
    // reaches the caller, the profile stays at 11, and Diagnostics lists it.
    std::fs::create_dir(&blocker).unwrap();
    let host = WorkbenchState::for_profile(&path);
    let error = host
        .board_request("items.list", "{}")
        .expect_err("no backup, no upgrade");
    assert_eq!(error.code, "backup_failed", "{}", error.message);
    assert_eq!(version(&path), 11);
    let record = degraded_now().expect("the failed upgrade is on the Diagnostics list");
    assert!(record.reason.contains("backup_failed"), "{record:?}");
    assert!(!record.stopped);

    // Fixed: the first read upgrades (never a read against the old schema),
    // takes the backup, and clears the record.
    std::fs::remove_dir(&blocker).unwrap();
    let listed = host.board_request("items.list", "{}").unwrap();
    assert!(listed["items"].is_array(), "{listed}");
    assert_eq!(version(&path), 12);
    assert!(backup.exists(), "the pre-upgrade copy is kept");
    assert!(degraded_now().is_none(), "recovered, so no longer listed");

    // Manvi's worker refusing for the same reason is shown as what to do.
    let refused = worker_error(HarnessError::Refused(WireError {
        code: "schema_upgrade_required".into(),
        message: "workbench schema 11 must be upgraded".into(),
        retryable: false,
    }));
    assert_eq!(refused.code, "schema_upgrade_required");
    assert!(
        refused.message.contains("restart GitPulse"),
        "{}",
        refused.message
    );
    assert!(
        refused.message.contains("schema 11"),
        "the worker's own words are kept"
    );
    assert!(degraded_now().is_some(), "and it is on Diagnostics");
    crate::supervisor::note_recovered(PROFILE_UPGRADE_SUBSYSTEM);
    // Any other refusal keeps passing through unchanged.
    let other = worker_error(HarnessError::Refused(WireError {
        code: "task_changed".into(),
        message: "moved".into(),
        retryable: false,
    }));
    assert_eq!(
        (other.code.as_str(), other.message.as_str()),
        ("task_changed", "moved")
    );
    assert!(degraded_now().is_none());
}
