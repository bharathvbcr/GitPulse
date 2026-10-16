//! The read-only commit-graph detector, run against graphs real git wrote.
//!
//! Each state the Health hint distinguishes is built with git itself, then read
//! back through `scan_storage` (the path the UI takes) or `inspect` (the
//! detector alone). Writing a graph here is test setup; GitPulse's production
//! code holds no such command, which the last test pins over the source.

mod common;
use common::{run_git, trust_repo};
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::storage::commit_graph::{
    inspect, CommitGraphLayout, CommitGraphReport, CommitGraphState,
};
use gitpulse_lib::storage::scan_storage;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Tests here share the process-wide git config the spawn counter edits, so
/// they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn repo_with_commits(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "gc.auto", "0"]);
    for i in 0..n {
        std::fs::write(dir.path().join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
        run_git(dir.path(), &["add", "-A"]);
        run_git(dir.path(), &["commit", "-q", "-m", &format!("c{i}")]);
    }
    dir
}

fn add_commit(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), name).unwrap();
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "-m", name]);
}

fn detect(dir: &Path) -> CommitGraphReport {
    scan_storage(dir.to_str().unwrap())
        .expect("scan")
        .commit_graph
}

fn assert_state(report: &CommitGraphReport, state: CommitGraphState) {
    assert_eq!(report.state, state, "{report:?}");
    if state == CommitGraphState::Unknown {
        assert!(report.note.is_some(), "unknown must say why: {report:?}");
    }
}

#[test]
fn a_repository_without_a_graph_is_missing() {
    let _g = serial();
    let dir = repo_with_commits(3);
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Missing);
    assert_eq!(report.layout, CommitGraphLayout::None);
    assert_eq!(report.commits, None);
}

#[test]
fn a_repository_with_no_commits_has_nothing_to_accelerate() {
    let _g = serial();
    let dir = repo_with_commits(0);
    assert_state(&detect(dir.path()), CommitGraphState::NoHistory);
}

#[test]
fn a_graph_without_bloom_filters_is_reported_as_such() {
    let _g = serial();
    let dir = repo_with_commits(4);
    run_git(dir.path(), &["commit-graph", "write", "--reachable"]);
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::NoBloom);
    assert_eq!(report.bloom, Some(false));
    assert_eq!(report.head_covered, Some(true));
    assert_eq!(report.commits, Some(4));
    assert_eq!(report.layout, CommitGraphLayout::Single);
}

#[test]
fn a_graph_with_changed_paths_covering_head_is_present() {
    let _g = serial();
    let dir = repo_with_commits(4);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Present);
    assert_eq!(report.bloom, Some(true));
    assert_eq!(report.head_covered, Some(true));
}

#[test]
fn a_commit_newer_than_the_graph_makes_it_stale() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    add_commit(dir.path(), "newer.txt");
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Stale);
    assert_eq!(report.head_covered, Some(false));
}

#[test]
fn refs_in_packed_refs_and_a_detached_head_resolve() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    run_git(dir.path(), &["pack-refs", "--all", "--prune"]);
    assert!(!dir.path().join(".git/refs/heads/main").exists());
    assert_state(&detect(dir.path()), CommitGraphState::Present);
    run_git(dir.path(), &["checkout", "-q", "--detach"]);
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

#[test]
fn a_split_chain_is_read_layer_by_layer() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &[
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split",
        ],
    );
    let first = detect(dir.path());
    assert_state(&first, CommitGraphState::Present);
    assert_eq!(first.layout, CommitGraphLayout::Split);
    assert_eq!(first.layers, 1);

    add_commit(dir.path(), "second.txt");
    assert_state(&detect(dir.path()), CommitGraphState::Stale);
    run_git(
        dir.path(),
        &[
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split=no-merge",
        ],
    );
    let second = detect(dir.path());
    assert_state(&second, CommitGraphState::Present);
    assert_eq!(second.layers, 2, "{second:?}");
    assert_eq!(second.commits, Some(4));
}

#[test]
fn a_chain_naming_a_missing_layer_could_not_be_checked_not_missing() {
    let _g = serial();
    let dir = repo_with_commits(2);
    run_git(
        dir.path(),
        &[
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--split",
        ],
    );
    let chain_dir = dir.path().join(".git/objects/info/commit-graphs");
    for entry in std::fs::read_dir(&chain_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "graph") {
            std::fs::remove_file(path).unwrap();
        }
    }
    assert_state(&detect(dir.path()), CommitGraphState::Unknown);
}

#[test]
fn corrupt_graph_files_could_not_be_checked() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let graph = dir.path().join(".git/objects/info/commit-graph");
    let good = std::fs::read(&graph).unwrap();

    // Truncated mid-table, wrong signature, an offset past the end, an empty file.
    let mut bad_offset = good.clone();
    bad_offset[8 + 4..8 + 12].copy_from_slice(&u64::MAX.to_be_bytes());
    let mut bad_magic = good.clone();
    bad_magic[0] = b'X';
    let mut bad_fanout_total = good.clone();
    // The fanout chunk follows the chunk table; bump its last slot so the OID
    // lookup size no longer matches.
    let table_end = 8 + (good[6] as usize + 1) * 12;
    bad_fanout_total[table_end + 1020..table_end + 1024].copy_from_slice(&1000u32.to_be_bytes());
    for (label, bytes) in [
        ("truncated", good[..20].to_vec()),
        ("empty", Vec::new()),
        ("bad magic", bad_magic),
        ("bad offset", bad_offset),
        ("bad fanout", bad_fanout_total),
    ] {
        // Git writes the graph read-only.
        std::fs::remove_file(&graph).unwrap();
        std::fs::write(&graph, bytes).unwrap();
        let report = detect(dir.path());
        assert_eq!(
            report.state,
            CommitGraphState::Unknown,
            "{label}: {report:?}"
        );
        assert!(report.note.is_some(), "{label}");
    }
}

#[test]
fn a_linked_worktree_reads_the_graph_in_the_common_dir() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let parent = tempfile::tempdir().unwrap();
    let wt: PathBuf = parent.path().join("wt");
    run_git(
        dir.path(),
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    trust_repo(&wt);
    let report = detect(&wt);
    assert_state(&report, CommitGraphState::Present);
    assert!(
        wt.join(".git").is_file(),
        "a linked worktree has a .git file"
    );

    // A commit made only in the linked worktree is newer than the graph.
    add_commit(&wt, "only-here.txt");
    assert_state(&detect(&wt), CommitGraphState::Stale);
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

#[test]
fn a_shallow_clone_is_not_judged() {
    let _g = serial();
    let source = repo_with_commits(4);
    let parent = tempfile::tempdir().unwrap();
    let clone = parent.path().join("shallow");
    run_git(
        parent.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", source.path().display()),
            clone.to_str().unwrap(),
        ],
    );
    trust_repo(&clone);
    assert_state(&detect(&clone), CommitGraphState::Unknown);
}

#[test]
fn replace_refs_turn_the_graph_off_so_nothing_is_judged() {
    let _g = serial();
    let dir = repo_with_commits(3);
    run_git(dir.path(), &["replace", "HEAD", "HEAD~1"]);
    let report = detect(dir.path());
    assert_state(&report, CommitGraphState::Unknown);
    assert!(
        report.note.as_deref().unwrap_or("").contains("replace"),
        "{report:?}"
    );
}

#[test]
fn a_repository_with_alternates_is_not_called_missing() {
    let _g = serial();
    let source = repo_with_commits(3);
    run_git(
        source.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let parent = tempfile::tempdir().unwrap();
    let clone = parent.path().join("shared");
    run_git(
        parent.path(),
        &[
            "clone",
            "-q",
            "--shared",
            source.path().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    trust_repo(&clone);
    assert!(clone.join(".git/objects/info/alternates").exists());
    let report = detect(&clone);
    assert_state(&report, CommitGraphState::Unknown);
    assert!(
        report.note.as_deref().unwrap_or("").contains("alternates"),
        "{report:?}"
    );
}

#[test]
fn a_bare_repository_is_read_in_place() {
    let _g = serial();
    let source = repo_with_commits(3);
    let parent = tempfile::tempdir().unwrap();
    let bare = parent.path().join("bare.git");
    run_git(
        parent.path(),
        &[
            "clone",
            "-q",
            "--bare",
            source.path().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    trust_repo(&bare);
    assert_state(&detect(&bare), CommitGraphState::Missing);
    run_git(
        &bare,
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    assert_state(&detect(&bare), CommitGraphState::Present);
}

#[test]
fn a_sha256_graph_is_read() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", "--object-format=sha256"])
        .current_dir(dir.path())
        .output_locked()
        .expect("spawn git");
    if !init.status.success() {
        eprintln!("git lacks sha256 support; case skipped");
        return;
    }
    trust_repo(dir.path());
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "user.name", "T"]);
    run_git(dir.path(), &["config", "user.email", "t@example.com"]);
    add_commit(dir.path(), "a.txt");
    add_commit(dir.path(), "b.txt");
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    assert_state(&detect(dir.path()), CommitGraphState::Present);
}

#[test]
fn a_hostile_head_is_unresolved_not_followed() {
    let _g = serial();
    let dir = repo_with_commits(2);
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    let git_dir = dir.path().join(".git");
    std::fs::write(git_dir.join("HEAD"), "ref: ../../../../etc/hosts\n").unwrap();
    let report = inspect(&git_dir, &git_dir);
    assert_state(&report, CommitGraphState::Unknown);
    assert_eq!(report.head_covered, None);
}

/// The detector is files only. Git's own trace2 log is the witness: a positive
/// control proves the log would have recorded a spawn, then the detector runs
/// with the log armed and must add none.
#[test]
fn detection_spawns_no_git_process() {
    let _g = serial();
    let dir = repo_with_commits(3);
    let git_dir = dir.path().join(".git");
    let config = PathBuf::from(std::env::var_os("GIT_CONFIG_GLOBAL").expect("hermetic config"));
    let original = std::fs::read_to_string(&config).unwrap_or_default();
    let log = dir.path().join("trace2.jsonl");
    std::fs::write(
        &config,
        format!("{original}\n[trace2]\n\teventTarget = {}\n", log.display()),
    )
    .unwrap();

    let starts = |log: &Path| {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"event\":\"start\""))
            .count()
    };
    run_git(dir.path(), &["rev-parse", "HEAD"]);
    let control = starts(&log);
    let missing = inspect(&git_dir, &git_dir);
    let after_inspect = starts(&log);
    let scanned = scan_storage(dir.path().to_str().unwrap());
    std::fs::write(&config, original).unwrap();

    assert!(
        control >= 1,
        "the trace2 log recorded no spawn: control broken"
    );
    assert_eq!(missing.state, CommitGraphState::Missing);
    assert_eq!(after_inspect, control, "inspect spawned a git process");
    assert!(scanned.is_ok());
}

/// No production file may build a commit-graph write argv: writing one is
/// Owner Decision 3, and `git_config_posture.rs` guards the config half of the
/// same rule. The Health hint prints the command as display text for the
/// person to run, so the literal appears only in a TypeScript template string.
#[test]
fn no_production_rust_builds_a_commit_graph_write_argv() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    let mut offenders = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let name = rel.rsplit('/').next().unwrap();
        if name == "tests.rs" || name.ends_with("_tests.rs") || rel.starts_with("test_support/") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        // Production text only: drop an inline `#[cfg(test)] mod tests`.
        let production = text.split("#[cfg(test)]").next().unwrap_or("");
        // Whitespace-free, so `"commit-graph",\n "write"` and a spelled-out
        // `commit-graph write` both match; the bare path component
        // `join("commit-graph")` (reading the file) does not.
        let squashed: String = production.split_whitespace().collect();
        if squashed.contains("\"commit-graph\",\"write\"")
            || production.contains("commit-graph write")
        {
            offenders.push(rel);
        }
    }
    assert!(offenders.is_empty(), "commit-graph argv in {offenders:?}");
}
