//! The repository tuning detector, run against repositories real git wrote.
//!
//! Index versions, `index.skipHash`, linked worktrees, maintenance registration
//! and the multi-pack-index are all produced by git itself (test setup only:
//! GitPulse's production code writes none of it outside the gated apply), then
//! read back through `inspect`. The spawn test is the proof of "zero spawns".

mod common;
use common::run_git;
use gitpulse_lib::storage::repo_tuning::{inspect, IndexReadState, RepoTuningReport};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    common::unmetered_spawns();
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn repo_with_files(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "gc.auto", "0"]);
    for i in 0..n {
        std::fs::write(dir.path().join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
    }
    run_git(dir.path(), &["add", "-A"]);
    run_git(dir.path(), &["commit", "-q", "-m", "c"]);
    dir
}

fn detect(dir: &Path) -> RepoTuningReport {
    let git_dir = dir.join(".git");
    inspect(dir, &git_dir, &git_dir, None)
}

#[test]
fn index_versions_two_three_and_four_are_read_from_real_indexes() {
    let _g = serial();
    // git writes v3 only when an entry needs extended flags, so the v3 case
    // adds an intent-to-add entry; v2 and v4 are what `--index-version` sets.
    for (version, config, ita) in [
        ("2", None, false),
        ("3", None, true),
        ("4", Some("4"), false),
    ] {
        let dir = repo_with_files(3);
        if let Some(v) = config {
            run_git(dir.path(), &["config", "index.version", v]);
        }
        if ita {
            std::fs::write(dir.path().join("planned.txt"), "later").unwrap();
            run_git(dir.path(), &["add", "-N", "planned.txt"]);
        }
        if !ita {
            run_git(dir.path(), &["update-index", "--index-version", version]);
        }
        let report = detect(dir.path());
        assert_eq!(report.index.state, IndexReadState::Present, "{report:?}");
        assert_eq!(
            report.index.version,
            Some(version.parse().unwrap()),
            "{report:?}"
        );
        assert_eq!(report.index.entries, Some(if ita { 4 } else { 3 }));
        assert_eq!(
            report.index.skip_hash,
            Some(false),
            "git writes a real checksum"
        );
    }
}

#[test]
fn a_skip_hash_index_is_read_and_flagged() {
    let _g = serial();
    let dir = repo_with_files(5);
    run_git(dir.path(), &["config", "index.skipHash", "true"]);
    run_git(dir.path(), &["update-index", "--really-refresh"]);
    run_git(dir.path(), &["add", "-A"]);
    // Force a rewrite so the config applies.
    std::fs::write(dir.path().join("extra.txt"), "x").unwrap();
    run_git(dir.path(), &["add", "extra.txt"]);
    let report = detect(dir.path());
    assert_eq!(report.index.entries, Some(6), "{report:?}");
    assert_eq!(report.index.skip_hash, Some(true), "{report:?}");
}

#[test]
fn a_linked_worktree_reads_its_own_index_and_the_common_objects() {
    let _g = serial();
    let dir = repo_with_files(4);
    run_git(dir.path(), &["gc", "-q"]);
    let wt = dir.path().join("wt");
    run_git(
        dir.path(),
        &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
    );
    std::fs::write(wt.join("only-here.txt"), "y").unwrap();
    run_git(&wt, &["add", "only-here.txt"]);
    let common = dir.path().join(".git");
    let private = common.join("worktrees").join("wt");
    let report = inspect(&wt, &private, &common, None);
    assert_eq!(
        report.index.entries,
        Some(5),
        "the worktree's index, {report:?}"
    );
    assert!(
        report.objects.packs.unwrap_or(0) >= 1,
        "the common dir's packs, {report:?}"
    );
    let main = detect(dir.path());
    assert_eq!(main.index.entries, Some(4));
}

#[test]
fn midx_and_pack_count_follow_git() {
    let _g = serial();
    let dir = repo_with_files(3);
    assert_eq!(detect(dir.path()).midx, Some(false));
    run_git(dir.path(), &["gc", "-q"]);
    run_git(dir.path(), &["multi-pack-index", "write"]);
    let report = detect(dir.path());
    assert_eq!(report.midx, Some(true), "{report:?}");
    assert_eq!(report.objects.packs, Some(1));
}

#[test]
fn registration_and_tuning_keys_come_from_git_s_own_config() {
    let _g = serial();
    let dir = repo_with_files(2);
    let global = PathBuf::from(std::env::var_os("GIT_CONFIG_GLOBAL").expect("hermetic config"));
    let home_config =
        PathBuf::from(std::env::var_os("HOME").expect("hermetic HOME")).join(".gitconfig");
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let block = format!(
        "\n[maintenance]\n\trepo = {}\n\tstrategy = incremental\n[core]\n\tuntrackedCache = true\n",
        canonical.display()
    );
    let files = [global, home_config];
    let originals: Vec<Option<String>> = files
        .iter()
        .map(|f| std::fs::read_to_string(f).ok())
        .collect();
    for (file, original) in files.iter().zip(&originals) {
        std::fs::write(
            file,
            format!("{}{block}", original.clone().unwrap_or_default()),
        )
        .unwrap();
    }
    run_git(dir.path(), &["config", "core.fsmonitor", "false"]);
    // The shared snapshot is cached; wait out its racy window like the
    // commit-graph detector's spawn test does.
    std::thread::sleep(std::time::Duration::from_millis(3300));
    let report = detect(dir.path());
    for (file, original) in files.iter().zip(originals) {
        match original {
            Some(text) => std::fs::write(file, text).unwrap(),
            None => std::fs::remove_file(file).unwrap(),
        }
    }
    assert_eq!(report.maintenance.registered, Some(true), "{report:?}");
    assert_eq!(report.maintenance.strategy.as_deref(), Some("incremental"));
    let untracked = report.key("core.untrackedcache").unwrap();
    assert_eq!(untracked.value.as_deref(), Some("true"));
    assert_eq!(
        untracked.origin,
        Some(gitpulse_lib::engine::git_reader::ConfigScope::Global),
        "{report:?}"
    );
    let fsmonitor = report.key("core.fsmonitor").unwrap();
    assert_eq!(
        fsmonitor.origin,
        Some(gitpulse_lib::engine::git_reader::ConfigScope::Local)
    );
}

/// The detector's only process is the shared, cached `config --list`; nothing
/// that reads the object store or the index (`count-objects`, `ls-files`,
/// `rev-parse`...) may run, and a warm call spawns nothing. The trace2 log is
/// the witness, with a positive control, as in `commit_graph_detect`.
#[test]
fn detection_spawns_no_git_process() {
    let _g = serial();
    let dir = repo_with_files(3);
    let log = dir.path().join("trace2.jsonl");
    let arm = format!("\n[trace2]\n\teventTarget = {}\n", log.display());
    let files = [
        PathBuf::from(std::env::var_os("GIT_CONFIG_GLOBAL").expect("hermetic config")),
        PathBuf::from(std::env::var_os("HOME").expect("hermetic HOME")).join(".gitconfig"),
    ];
    let originals: Vec<Option<String>> = files
        .iter()
        .map(|f| std::fs::read_to_string(f).ok())
        .collect();
    for (file, original) in files.iter().zip(&originals) {
        std::fs::write(
            file,
            format!("{}{arm}", original.clone().unwrap_or_default()),
        )
        .unwrap();
    }
    let starts = |log: &Path| -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"event\":\"start\""))
            .map(str::to_string)
            .collect()
    };
    run_git(dir.path(), &["rev-parse", "HEAD"]);
    let control = starts(&log).len();
    std::thread::sleep(std::time::Duration::from_millis(3300));
    let first = detect(dir.path());
    let cold = starts(&log);
    let second = detect(dir.path());
    let warm = starts(&log);
    for (file, original) in files.iter().zip(originals) {
        match original {
            Some(text) => std::fs::write(file, text).unwrap(),
            None => std::fs::remove_file(file).unwrap(),
        }
    }
    assert!(
        control >= 1,
        "the trace2 log recorded no spawn: control broken"
    );
    assert_eq!(first.index.entries, Some(3));
    assert_eq!(second.index.entries, Some(3));
    for event in &cold[control..] {
        assert!(
            event.contains("\"config\"") && event.contains("\"--list\""),
            "inspect spawned something other than `config --list`: {event}"
        );
    }
    assert_eq!(warm.len(), cold.len(), "a warm inspect spawned a process");
}

#[test]
fn a_real_split_index_is_found_in_the_index_file_and_refuses_many_files() {
    use gitpulse_lib::storage::repo_tuning::{recommend, GitEnv, TuningChange};
    let _g = serial();
    let dir = repo_with_files(25_000);
    run_git(dir.path(), &["update-index", "--split-index"]);
    let report = detect(dir.path());
    assert_eq!(report.index.state, IndexReadState::Present, "{report:?}");
    assert_eq!(report.index.split_index, Some(true), "{report:?}");
    assert!(!report.index.entries_exact);
    let env = GitEnv {
        version: Some((2, 54, 0)),
        fsmonitor_measured: true,
    };
    assert!(recommend(&report, env)
        .iter()
        .all(|o| o.change != TuningChange::UntrackedCache));
    // Undoing the split leaves a plain index that walks and counts exactly.
    run_git(dir.path(), &["update-index", "--no-split-index"]);
    let plain = detect(dir.path());
    assert_eq!(plain.index.split_index, Some(false), "{plain:?}");
    assert_eq!(plain.index.entries, Some(25_000));
}

#[test]
fn real_indexes_of_every_shape_git_writes_are_walked() {
    let _g = serial();
    for (label, setup) in [
        ("skipHash v4", &["config", "feature.manyFiles", "true"][..]),
        ("v4", &["config", "index.version", "4"][..]),
        ("plain", &["config", "gc.auto", "0"][..]),
    ] {
        let dir = repo_with_files(300);
        run_git(dir.path(), setup);
        std::fs::write(dir.path().join("new.txt"), "n").unwrap();
        run_git(dir.path(), &["add", "new.txt"]);
        run_git(dir.path(), &["status", "--porcelain"]);
        let report = detect(dir.path());
        assert_eq!(
            report.index.state,
            IndexReadState::Present,
            "{label}: {report:?}"
        );
        assert_eq!(report.index.entries, Some(301), "{label}");
        assert_eq!(report.index.split_index, Some(false), "{label}");
    }
}
