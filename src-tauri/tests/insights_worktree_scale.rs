//! Scale, budget, and fail-closed regression coverage for GitPulse Insights
//! on 40+ worktree repositories (matching real multi-agent checkouts).

mod common;

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use waylode_lib::insights;
use waylode_lib::ledger;
use waylode_lib::procguard::LockedSpawn;

const LINKED_BRANCH_WORKTREES: usize = 35;
const LINKED_DETACHED_WORKTREES: usize = 6;
const TOTAL_WORKTREES: u32 = 1 + (LINKED_BRANCH_WORKTREES + LINKED_DETACHED_WORKTREES) as u32;
const MAIN_DIRTY_FILES: usize = 300;
const OVERLAPPING_FILES: usize = 40;

fn mcp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_waylode-mcp")
}

fn persist_trust(repo: &Path) {
    let path = repo.to_str().expect("repo path");
    let preview = waylode_lib::repository_trust::inspect(path).expect("inspect repo");
    waylode_lib::repository_trust::grant(path, &preview.identity, true).expect("grant trust");
}

struct ScaleFarm {
    _main: tempfile::TempDir,
    _linked: tempfile::TempDir,
    repo: PathBuf,
    worktrees: Vec<PathBuf>,
}

fn build_scale_farm() -> ScaleFarm {
    let main = tempfile::tempdir().expect("main dir");
    let linked = tempfile::tempdir().expect("linked dir");
    // git cannot create a worktree under a `\\?\` path, which is what
    // `canonicalize` gives on Windows; the plain spelling is canonical too.
    let plain = waylode_lib::engine::process::canonicalize_plain;
    let repo = plain(main.path()).expect("canonical main");
    let linked_root = plain(linked.path()).expect("canonical linked");

    common::run_git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join(".gitignore"), ".devcouncil/\n").expect("gitignore");
    fs::write(repo.join("seed.txt"), "seed\n").expect("seed");
    common::run_git(&repo, &["add", ".gitignore", "seed.txt"]);
    common::run_git(&repo, &["commit", "-q", "-m", "initial"]);

    let mut worktrees = Vec::with_capacity(LINKED_BRANCH_WORKTREES + LINKED_DETACHED_WORKTREES);
    for idx in 0..LINKED_BRANCH_WORKTREES {
        let wt = linked_root.join(format!("wt-branch-{idx:02}"));
        let branch = format!("agent/lane-{idx:02}");
        common::run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                wt.to_str().expect("wt str"),
            ],
        );
        fs::write(
            wt.join(format!("lane-{idx:02}.txt")),
            format!("work {idx}\n"),
        )
        .expect("commit file");
        common::run_git(&wt, &["add", "-A"]);
        common::run_git(&wt, &["commit", "-q", "-m", &format!("lane {idx}")]);
        worktrees.push(wt);
    }

    for idx in 0..LINKED_DETACHED_WORKTREES {
        let wt = linked_root.join(format!("wt-detached-{idx:02}"));
        common::run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                wt.to_str().expect("wt str"),
                "HEAD",
            ],
        );
        fs::write(
            wt.join(format!("detached-{idx:02}.txt")),
            format!("detached {idx}\n"),
        )
        .expect("detached file");
        common::run_git(&wt, &["add", "-A"]);
        common::run_git(&wt, &["commit", "-q", "-m", &format!("detached {idx}")]);
        worktrees.push(wt);
    }

    // Main checkout exceeds the old 256-path cap; worktree #0 overlaps on 40
    // files, exceeding the old 32-item collision cap.
    for n in 0..MAIN_DIRTY_FILES {
        let name = format!("dirty-{n:03}.txt");
        fs::write(repo.join(&name), format!("main {n}\n")).expect("main dirty");
        if n < OVERLAPPING_FILES {
            fs::write(worktrees[0].join(&name), format!("lane0 {n}\n")).expect("overlap dirty");
        }
    }

    persist_trust(&repo);
    for wt in &worktrees {
        persist_trust(wt);
    }

    // Initialise the ledger with one recorded event, a clean pass for the
    // active checkout, and an unfinished truncated pass left behind by an
    // already-deleted worktree (which must classify as dormant rather than
    // permanently causing exit 3).
    let repo_str = repo.to_string_lossy().into_owned();
    ledger::append(ledger::Draft {
        repo_path: repo_str.clone(),
        action: "git.commit".to_string(),
        actor_kind: Some(ledger::ActorKind::Human),
        outcome: Some(ledger::Outcome::Ok),
        ..Default::default()
    })
    .expect("seed ledger event");
    let now = ledger::ids::now_millis();
    ledger::record_catch_up(
        &repo_str,
        &repo_str,
        &ledger::CatchUpRecord {
            completed: true,
            recorded: 1,
            ..Default::default()
        },
        now,
    )
    .expect("record active catch-up");
    ledger::record_catch_up(
        &repo_str,
        &linked_root.join("deleted-worktree").to_string_lossy(),
        &ledger::CatchUpRecord {
            completed: false,
            truncated: true,
            error: "catch-up truncated: deleted worktree deferred".to_string(),
            ..Default::default()
        },
        now,
    )
    .expect("record deleted worktree catch-up");

    ScaleFarm {
        _main: main,
        _linked: linked,
        repo,
        worktrees,
    }
}

fn call_mcp_cli(tool: &str, repo: &Path) -> (i32, Value, String) {
    let log_dir = tempfile::tempdir().expect("log dir");
    let output = Command::new(mcp_bin())
        .args([
            "call",
            tool,
            &format!("repo_path={}", repo.to_string_lossy()),
        ])
        .env("GITPULSE_LOG_DIR", log_dir.path())
        .output_locked()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", mcp_bin()));
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let parsed: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("invalid JSON from {tool} (exit {code}): {e}\nstdout={stdout}\nstderr={stderr}")
    });
    (code, parsed, stderr)
}

#[test]
fn forty_two_worktrees_scan_completely_without_truncation_and_fail_closed_when_broken() {
    common::unmetered_spawns();
    let farm = build_scale_farm();
    let repo_str = farm.repo.to_string_lossy().into_owned();

    // 1. In-process snapshot: all 42 worktrees scanned, 0 dirty_unknown, all
    //    42 HEAD divergences measured, 300 main dirty files, 40 overlaps, no
    //    truncation, no deadline expiry.
    let snap = insights::snapshot(&repo_str);
    assert!(!snap.deadline_expired, "{:?}", snap.stages_ms);
    assert!(snap.worktrees.ok, "{}", snap.worktrees.error);
    assert_eq!(snap.worktrees.count, TOTAL_WORKTREES);
    assert_eq!(snap.worktrees.scanned, TOTAL_WORKTREES);
    assert_eq!(snap.worktrees.dirty_unknown, 0);
    assert!(!snap.worktrees.truncated);
    for wt in &snap.worktrees.items {
        assert!(wt.ahead.is_some(), "unmeasured ahead on {wt:?}");
        assert!(wt.behind.is_some(), "unmeasured behind on {wt:?}");
        assert_eq!(wt.divergence_note, None, "unexpected note on {wt:?}");
    }
    assert!(snap.changes.ok, "{}", snap.changes.error);
    assert_eq!(snap.changes.files, MAIN_DIRTY_FILES as u32);

    assert!(snap.collisions.ok, "{}", snap.collisions.error);
    assert_eq!(snap.collisions.scanned_worktrees, TOTAL_WORKTREES);
    assert_eq!(snap.collisions.unscanned_worktrees, 0);
    assert_eq!(snap.collisions.failed_worktrees, 0);
    assert!(!snap.collisions.truncated, "{:?}", snap.collisions);
    assert_eq!(snap.collisions.overlapping_files, OVERLAPPING_FILES as u32);
    assert_eq!(snap.collisions.items.len(), OVERLAPPING_FILES);

    // 2. Standalone collision_risk: all 42 worktrees scanned, no truncation.
    let risk = insights::collision_risk(&repo_str);
    assert!(risk.ok, "{}", risk.error);
    assert_eq!(risk.scanned_worktrees, TOTAL_WORKTREES);
    assert_eq!(risk.unscanned_worktrees, 0);
    assert_eq!(risk.failed_worktrees, 0);
    assert!(!risk.truncated, "{risk:?}");
    assert_eq!(risk.overlapping_files, OVERLAPPING_FILES as u32);
    assert_eq!(risk.items.len(), OVERLAPPING_FILES);

    // 3. Real `gitpulse-mcp` CLI binary under production background spawn gate:
    //    `gitpulse_collision_risk` must exit 0. `gitpulse_insights` also
    //    reports the code-intelligence facet, and this farm has no DevMap
    //    store: that facet says `available: false`, which the CLI reports as
    //    an incomplete answer (exit 3). It must be the only incomplete part;
    //    every git facet below is still checked whole.
    let (insights_code, insights_json, insights_err) =
        call_mcp_cli("gitpulse_insights", &farm.repo);
    assert_eq!(
        insights_json["codeintel"]["available"], false,
        "the farm has no DevMap store: {insights_json}"
    );
    // A logged warning is a diagnostic, not part of the answer: under load
    // the spawn gate may log that its shared budget was busy (Windows CI).
    // Everything else the CLI writes to stderr is compared exactly.
    let insights_said: String = insights_err
        .lines()
        .filter(|line| !line.contains("WARN ["))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(
        (insights_code, insights_said.as_str()),
        (
            3,
            "waylode-mcp call: the answer is incomplete (codeintel.available); read its error fields before relying on it\n"
        ),
        "the absent index must be the only incomplete part\npayload={insights_json}"
    );
    assert_eq!(insights_json["deadline_expired"], false);
    assert_eq!(insights_json["worktrees"]["scanned"], TOTAL_WORKTREES);
    assert_eq!(insights_json["worktrees"]["dirty_unknown"], 0);
    assert_eq!(insights_json["collisions"]["ok"], true);
    assert_eq!(insights_json["collisions"]["truncated"], false);
    assert_eq!(
        insights_json["collisions"]["scanned_worktrees"],
        TOTAL_WORKTREES
    );
    assert_eq!(insights_json["ledger"]["freshness_ok"], true);
    assert_eq!(insights_json["ledger"]["catch_up"]["truncated"], false);
    assert_eq!(insights_json["ledger"]["catch_up"]["stale_worktrees"], 1);

    let (risk_code, risk_json, risk_err) = call_mcp_cli("gitpulse_collision_risk", &farm.repo);
    assert_eq!(
        risk_code, 0,
        "gitpulse_collision_risk returned {risk_code}: stderr={risk_err}\npayload={risk_json}"
    );
    assert_eq!(risk_json["ok"], true);
    assert_eq!(risk_json["truncated"], false);
    assert_eq!(risk_json["scanned_worktrees"], TOTAL_WORKTREES);
    assert_eq!(risk_json["unscanned_worktrees"], 0);
    assert_eq!(risk_json["overlapping_files"], OVERLAPPING_FILES);

    // 4. Fail-closed invariant: breaking one worktree must surface `ok: false`,
    //    `failed_worktrees: 1`, `truncated: true`, and exit code 3 from CLI.
    fs::remove_dir_all(farm.worktrees.last().expect("last worktree")).expect("remove wt");

    let broken_snap = insights::snapshot(&repo_str);
    assert_eq!(broken_snap.worktrees.dirty_unknown, 1);
    assert!(!broken_snap.collisions.ok);
    assert_eq!(broken_snap.collisions.failed_worktrees, 1);
    assert!(broken_snap.collisions.truncated);

    let broken_risk = insights::collision_risk(&repo_str);
    assert!(!broken_risk.ok);
    assert_eq!(broken_risk.failed_worktrees, 1);
    assert!(broken_risk.truncated);

    let (broken_code, broken_json, _) = call_mcp_cli("gitpulse_collision_risk", &farm.repo);
    assert_eq!(broken_code, 3, "broken scan must exit 3: {broken_json}");
    assert_eq!(broken_json["ok"], false);
    assert_eq!(broken_json["failed_worktrees"], 1);
    assert_eq!(broken_json["truncated"], true);
}
