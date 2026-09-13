//! Opening a repository is the whole setup — proved against the real `devmap`.
//!
//! The unit tests behind this drive stub binaries, which is the right way to
//! pin the decisions but cannot show that the decisions match the tool. Three
//! facts here came from measuring the installed CLI rather than reading it, and
//! each one is a place where the app and the tool could drift apart silently:
//!
//! * `devmap build` without `--manifest` writes the database and nothing else,
//!   so an "index built" that a navigator map cannot be read from is a state
//!   the tool produces on its own.
//! * `devmap build --manifest` rewrites the artifacts even when it reports the
//!   store `unchanged`, which is what makes a deleted `repo_map.json`
//!   recoverable without invalidating a healthy database.
//! * Nothing in the toolchain adds `.devmap/` to any ignore file, so a
//!   repository this app indexes gains permanent untracked state unless the
//!   app arranges otherwise.
//!
//! Skipped loudly, never silently: without an installed `devmap` this test
//! prints why and returns rather than passing on an absence.

mod common;

use gitpulse_lib::devmap;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn porcelain(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .expect("git status");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A small but real repository: the index has to have something to index, or
/// a build could succeed by doing nothing.
fn scratch_repo() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "t@example.invalid"]);
    git(root, &["config", "user.name", "Test"]);
    std::fs::write(
        root.join("main.rs"),
        "fn main() { helper(); }\nfn helper() -> u32 { 42 }\n",
    )
    .expect("write");
    std::fs::write(
        root.join("lib.rs"),
        "pub fn exported() -> u32 { 7 }\nfn unused_here() {}\n",
    )
    .expect("write");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "init"]);
    common::trust_repo(root);
    dir
}

fn devmap_installed() -> bool {
    devmap::resolve_binary().is_ok()
}

#[test]
fn opening_a_repository_leaves_it_indexed_and_clean() {
    if !devmap_installed() {
        eprintln!("SKIPPED: no `devmap` binary resolved; nothing to measure against");
        return;
    }
    let repo = scratch_repo();
    let root = repo.path();
    let path = root.to_string_lossy().into_owned();

    // 1. Initialization: ignore hygiene, then the workspace registry.
    let report =
        devmap::initialize_repository(&path, std::slice::from_ref(&path)).expect("initialize");
    assert!(
        report.exclude.is_clean(),
        "state directory not hidden: {:?}",
        report.exclude
    );
    assert!(
        report.devmap_available,
        "devmap resolved a moment ago but the report says otherwise"
    );
    let registry = report
        .workspace_registry
        .as_deref()
        .unwrap_or_else(|| panic!("no registry written: {:?}", report.workspace_reason));
    assert!(
        Path::new(registry).is_file(),
        "registry path does not exist: {registry}"
    );

    // 2. The first automatic refresh — an activation, exactly as opening a tab
    //    produces. This is the call that used to run a plain `devmap build`
    //    and leave the navigator with no document to read.
    assert!(
        !devmap::missing_artifacts(root).is_empty(),
        "fixture should start with no artifacts"
    );
    let outcome = devmap::maybe_refresh(&path, false);
    assert_eq!(
        outcome.decision,
        devmap::LiveRefreshDecision::Refresh,
        "a repository with no index must refresh: {outcome:?}"
    );
    let build = outcome.build.as_ref().expect("a build outcome");
    assert!(
        build.ok,
        "cold build failed: {}",
        build.stderr.trim().chars().take(400).collect::<String>()
    );
    assert!(
        devmap::missing_artifacts(root).is_empty(),
        "one automatic build must leave a readable map, still missing: {:?}",
        devmap::missing_artifacts(root)
    );
    assert!(outcome.artifacts_restored, "{outcome:?}");

    // 3. The map the navigator reads actually parses.
    let load = devmap::load_repo_map(&path);
    assert!(load.available, "map unreadable: {:?}", load.reason);

    // 4. And none of it is visible to the user as repository changes.
    assert_eq!(
        porcelain(root),
        "",
        "indexing must not add anything to git status"
    );
}

#[test]
fn a_deleted_map_artifact_heals_on_the_next_activation() {
    if !devmap_installed() {
        eprintln!("SKIPPED: no `devmap` binary resolved; nothing to measure against");
        return;
    }
    let repo = scratch_repo();
    let root = repo.path();
    let path = root.to_string_lossy().into_owned();
    devmap::initialize_repository(&path, &[]).expect("initialize");
    assert_eq!(
        devmap::maybe_refresh(&path, false).decision,
        devmap::LiveRefreshDecision::Refresh
    );
    assert!(devmap::missing_artifacts(root).is_empty());

    // A settled repository skips, cheaply.
    devmap::clear_live_echo_cooldown(&path);
    let settled = devmap::maybe_refresh(&path, false);
    assert_eq!(
        settled.decision,
        devmap::LiveRefreshDecision::SkipFresh,
        "{settled:?}"
    );

    // Now delete the navigator artifact behind the app's back. The real CLI
    // still reports `is_fresh: true` for the *store*, which is why store
    // freshness alone cannot be the gate.
    std::fs::remove_file(devmap::repo_map_path(root)).expect("remove map");
    let status = devmap::cli_status(&path);
    assert_eq!(
        status
            .status
            .as_ref()
            .and_then(|value| value.get("is_fresh"))
            .and_then(|value| value.as_bool()),
        Some(true),
        "the premise of this test: the store still calls itself fresh"
    );

    devmap::clear_live_echo_cooldown(&path);
    let healed = devmap::maybe_refresh(&path, false);
    assert_eq!(
        healed.decision,
        devmap::LiveRefreshDecision::Refresh,
        "a missing artifact must override store freshness: {healed:?}"
    );
    assert!(healed.artifacts_restored, "{healed:?}");
    assert!(devmap::missing_artifacts(root).is_empty());
    assert_eq!(porcelain(root), "");
}

/// A second initialization of the same repository must be a no-op, because the
/// app runs it every time the open-tab set changes.
#[test]
fn repeated_initialization_changes_nothing() {
    if !devmap_installed() {
        eprintln!("SKIPPED: no `devmap` binary resolved; nothing to measure against");
        return;
    }
    let repo = scratch_repo();
    let root = repo.path();
    let path = root.to_string_lossy().into_owned();

    let first = devmap::initialize_repository(&path, std::slice::from_ref(&path)).expect("first");
    let exclude = root.join(".git/info/exclude");
    let after_first = std::fs::read_to_string(&exclude).expect("exclude");
    let registry_after_first =
        std::fs::read_to_string(first.workspace_registry.as_deref().expect("registry"))
            .expect("registry");

    let second = devmap::initialize_repository(&path, std::slice::from_ref(&path)).expect("second");
    assert!(
        matches!(
            second.exclude,
            devmap::ExcludeOutcome::AlreadyIgnored { .. }
        ),
        "{:?}",
        second.exclude
    );
    assert_eq!(
        std::fs::read_to_string(&exclude).expect("exclude"),
        after_first
    );
    assert_eq!(
        std::fs::read_to_string(second.workspace_registry.as_deref().expect("registry"))
            .expect("registry"),
        registry_after_first
    );
    assert_eq!(porcelain(root), "");
}

/// The measurement the fix is built on, kept as a test so a future `devmap`
/// that changed it would be caught here rather than in a user's empty Map pane.
#[test]
fn a_plain_build_still_writes_no_consumer_artifacts() {
    if !devmap_installed() {
        eprintln!("SKIPPED: no `devmap` binary resolved; nothing to measure against");
        return;
    }
    let repo = scratch_repo();
    let root = repo.path();
    let path = root.to_string_lossy().into_owned();
    devmap::initialize_repository(&path, &[]).expect("initialize");

    let build = devmap::refresh(&path).expect("plain build");
    assert!(build.ok, "plain build failed: {}", build.stderr.trim());
    assert_eq!(
        devmap::missing_artifacts(root),
        vec!["repo_map.json", "code_graph.json"],
        "if this ever passes, `devmap build` gained the manifest and the \
         escalation in live.rs can be reconsidered — it is not wrong either way"
    );

    // And the manifest build fixes it, reporting the store unchanged while
    // doing so. That combination is why `artifacts_restored` exists.
    let manifest = devmap::build(&path).expect("manifest build");
    assert!(
        manifest.ok,
        "manifest build failed: {}",
        manifest.stderr.trim()
    );
    assert!(devmap::missing_artifacts(root).is_empty());
    assert_eq!(
        manifest
            .report
            .as_ref()
            .and_then(|report| report.get("unchanged"))
            .and_then(|value| value.as_bool()),
        Some(true),
        "the artifact-writing build reports the store unchanged"
    );
}
