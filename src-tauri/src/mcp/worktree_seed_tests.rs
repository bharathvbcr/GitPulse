//! A worktree the MCP tools seed with a DevMap store is brought to its own
//! tree. The seed is the main checkout's generation; DevMap's SessionStart
//! hook builds only where no store exists, so without the worktree's own
//! build an agent there would answer from the main checkout's working state.

use super::*;
use std::path::{Path, PathBuf};

/// `codeintel`'s one-generation fixture, trusted, with its source committed.
fn indexed_checkout() -> tempfile::TempDir {
    let dir = crate::codeintel::tests::repo_with_one_generation();
    let root = dir.path();
    crate::test_support::trust_repo(root);
    crate::test_support::git_in(root, &["add", "src"]);
    crate::test_support::git_in(
        root,
        &[
            "-c",
            "user.name=Seed Test",
            "-c",
            "user.email=seed@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    dir
}

/// A `devmap` that answers every `status` "not fresh" and every `build` "ok",
/// logging `<cwd> <argv>` to a file outside any worktree.
#[cfg(unix)]
fn stale_devmap(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("devmap-argv.log");
    let bin = dir.join("devmap");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n\
             printf '%s %s\\n' \"$PWD\" \"$*\" >> '{log}'\n\
             if [ \"$1\" = status ]; then printf '%s\\n' '{{\"is_fresh\":false,\"schema_outdated\":false}}'; exit 0; fi\n\
             if [ \"$1\" = build ]; then printf '%s\\n' '{{\"ok\":true}}'; exit 0; fi\n\
             exit 2\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (bin, log)
}

/// Waits (bounded, scaled) for `devmap build` to have run with `dir` as cwd.
#[cfg(unix)]
fn assert_build_runs_in(log: &Path, dir: &Path) {
    let dir = dir.canonicalize().unwrap();
    let deadline = std::time::Instant::now()
        + crate::test_support::env::coverage_relaxed(std::time::Duration::from_secs(10));
    loop {
        let seen = std::fs::read_to_string(log).unwrap_or_default();
        let built = seen.lines().any(|line| {
            line.split_once(' ').is_some_and(|(cwd, argv)| {
                Path::new(cwd).canonicalize().ok().as_deref() == Some(dir.as_path())
                    && argv.split_whitespace().next() == Some("build")
            })
        });
        if built {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no devmap build was started in the seeded worktree {}; devmap saw:\n{seen}",
            dir.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// `gitpulse_worktree_isolate` seeds a worktree that has no store, then starts
/// that worktree's own build.
#[cfg(unix)]
#[test]
fn isolate_seeds_the_store_and_starts_the_worktrees_own_build() {
    let bin_dir = tempfile::tempdir().unwrap();
    let (bin, log) = stale_devmap(bin_dir.path());
    let _devmap = crate::devmap::cli::bind_test_binary(bin.to_string_lossy().into_owned());
    let anchor = indexed_checkout();
    let lanes = tempfile::tempdir().unwrap();
    let lane = lanes.path().join("lane");
    crate::test_support::git_in(
        anchor.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane/seeded",
            lane.to_str().unwrap(),
        ],
    );

    let isolated = handle_tool_call(
        "gitpulse_worktree_isolate",
        &json!({
            "repo_path": anchor.path().to_str().unwrap(),
            "worktree_path": lane.to_str().unwrap(),
        }),
    )
    .expect("gitpulse_worktree_isolate succeeds");
    assert_eq!(isolated["devmap_seeded"], true, "{isolated}");
    assert!(devmap_query::paths::store_path(lane.canonicalize().unwrap()).is_file());
    assert_build_runs_in(&log, &lane);
}

/// `codeintel`'s indexed fixture moved to `<sandbox>/repo`, with its store
/// rebound to the new path. `gitpulse_worktree_create` and `switch` place a
/// worktree beside the repository (`../<repo>.<branch>`), so the repository
/// must sit inside a sandbox for that to stay inside the test's own tempdir.
fn indexed_checkout_in(sandbox: &Path) -> PathBuf {
    let fixture = indexed_checkout();
    let repo = sandbox.join("repo");
    std::fs::rename(fixture.path(), &repo).expect("move the fixture into the sandbox");
    let repo = repo.canonicalize().unwrap();
    let db = devmap_query::paths::store_path(&repo);
    let conn = rusqlite::Connection::open(&db).expect("open the moved store");
    let root = repo.to_str().unwrap();
    conn.execute(
        "UPDATE pending_state SET repo_root = ?1 WHERE singleton = 1",
        [root],
    )
    .unwrap();
    conn.execute("UPDATE generations SET repo_root = ?1", [root])
        .unwrap();
    drop(conn);
    crate::test_support::trust_repo(&repo);
    repo
}

/// `gitpulse_worktree_create` (no pointer) and `gitpulse_worktree_switch`
/// with `create_if_missing` both seed the new worktree and start its build.
#[cfg(unix)]
#[test]
fn create_and_switch_seed_the_store_and_start_the_worktrees_own_build() {
    let bin_dir = tempfile::tempdir().unwrap();
    let (bin, log) = stale_devmap(bin_dir.path());
    let _devmap = crate::devmap::cli::bind_test_binary(bin.to_string_lossy().into_owned());
    let sandbox = tempfile::tempdir().unwrap();
    let repo = indexed_checkout_in(sandbox.path());
    let sandbox_root = sandbox.path().canonicalize().unwrap();

    let created = handle_tool_call(
        "gitpulse_worktree_create",
        &json!({
            "repo_path": repo.to_str().unwrap(),
            "branch": "feat/seeded-create",
            "share_cache": false,
        }),
    )
    .expect("gitpulse_worktree_create succeeds");
    let made = PathBuf::from(created["worktree_path"].as_str().unwrap())
        .canonicalize()
        .unwrap();
    assert!(made.starts_with(&sandbox_root), "{}", made.display());
    assert!(
        devmap_query::paths::store_path(&made).is_file(),
        "{created}"
    );
    assert_build_runs_in(&log, &made);

    let switched = handle_tool_call(
        "gitpulse_worktree_switch",
        &json!({
            "repo_path": repo.to_str().unwrap(),
            "branch": "feat/seeded-switch",
            "create_if_missing": true,
        }),
    )
    .expect("gitpulse_worktree_switch succeeds");
    assert_eq!(switched["devmap_seeded"], true, "{switched}");
    let made = PathBuf::from(switched["worktree_path"].as_str().unwrap())
        .canonicalize()
        .unwrap();
    assert!(made.starts_with(&sandbox_root), "{}", made.display());
    assert_build_runs_in(&log, &made);
}

/// The zero-copy pointer leaves no local store, so nothing is built there: a
/// refresh would be a cold full build, the cost the seed exists to avoid.
#[cfg(unix)]
#[test]
fn isolate_with_the_zero_copy_pointer_starts_no_build() {
    let bin_dir = tempfile::tempdir().unwrap();
    let (bin, log) = stale_devmap(bin_dir.path());
    let _devmap = crate::devmap::cli::bind_test_binary(bin.to_string_lossy().into_owned());
    let anchor = indexed_checkout();
    let lanes = tempfile::tempdir().unwrap();
    let lane = lanes.path().join("lane");
    crate::test_support::git_in(
        anchor.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane/pointer",
            lane.to_str().unwrap(),
        ],
    );
    handle_tool_call(
        "gitpulse_worktree_isolate",
        &json!({
            "repo_path": anchor.path().to_str().unwrap(),
            "worktree_path": lane.to_str().unwrap(),
            "zero_copy": true,
        }),
    )
    .expect("gitpulse_worktree_isolate succeeds");
    let seen = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !seen.lines().any(|line| line.contains(" build")),
        "a pointer-only worktree was built:\n{seen}"
    );
}
