//! Adversarial review of the tuning advisor and its gated apply (card
//! ft-6b68be32). Each test states the attack and the outcome it requires; the
//! ones marked BREAKS fail on the lane as reviewed.
//!
//! Nothing here runs `git maintenance start` (it installs a per-user scheduler
//! job) and nothing starts a file system monitor daemon: only `core.untrackedCache`
//! is ever applied, and every other change is attacked at the plan.

mod common;
use common::{run_git, trust_repo};
use gitpulse_lib::engine::tuning_apply::{tuning_apply, tuning_plan};
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::storage::repo_tuning::{
    inspect_with, plan_apply, recommend, GitEnv, TuningChange,
};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    common::unmetered_spawns();
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

const ABOVE: usize = 20_500;
const ABOVE_FSMONITOR: usize = 40_500;

fn init_with_files(dir: &Path, files: usize) {
    std::fs::create_dir_all(dir).unwrap();
    run_git(dir, &["init", "-q", "-b", "main"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    run_git(dir, &["config", "gc.auto", "0"]);
    let mut stream =
        String::from("commit refs/heads/main\ncommitter t <t@x> 1700000000 +0000\ndata 1\nc\n");
    for i in 0..files {
        let body = format!("file {i}\n");
        stream.push_str(&format!(
            "M 100644 inline d{}/f{i}.txt\ndata {}\n{body}",
            i % 50,
            body.len()
        ));
    }
    stream.push('\n');
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .spawn_locked()
        .expect("spawn fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    run_git(dir, &["reset", "-q", "--hard", "main"]);
}

fn local_value(dir: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--local", "--get", key])
        .current_dir(dir)
        .output_locked()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn many_files_argv() -> Vec<String> {
    ["git", "config", "--local", "core.untrackedCache", "true"]
        .map(String::from)
        .to_vec()
}

fn str_of(dir: &Path) -> &str {
    dir.to_str().unwrap()
}

// ---------------------------------------------------------------------------
// Argv integrity and TOCTOU
// ---------------------------------------------------------------------------

#[test]
fn adv_crafted_confirmed_argv_variants_run_nothing() {
    let _g = serial();
    let parent = tempfile::tempdir().unwrap();
    let main = parent.path().join("main");
    init_with_files(&main, ABOVE);
    let path = str_of(&main);
    let ok = many_files_argv();
    let mut with_c = vec![
        "git".to_string(),
        "-c".into(),
        "core.hooksPath=/evil".into(),
    ];
    with_c.extend(ok[1..].iter().cloned());
    let mut trailing = ok.clone();
    trailing.push("extra".into());
    let mut dashed_c = ok.clone();
    dashed_c.insert(1, "-C".into());
    dashed_c.insert(2, "/tmp".into());
    let mut upper = ok.clone();
    upper[3] = "core.untrackedcache ".into();
    let hostile: Vec<Vec<Vec<String>>> = vec![
        vec![with_c],
        vec![trailing],
        vec![dashed_c],
        vec![upper],
        vec![ok.clone(), ok.clone()],
        vec![vec![]],
    ];
    for confirmed in hostile {
        let refused = tuning_apply(path, &[TuningChange::UntrackedCache], &confirmed).unwrap_err();
        assert!(
            refused.message().contains("not the previewed"),
            "{confirmed:?}: {}",
            refused.message()
        );
    }
    assert_eq!(local_value(&main, "core.untrackedCache"), None);
    assert_eq!(local_value(&main, "core.hooksPath"), None);
}

#[test]
fn adv_symlink_to_an_untrusted_repository_is_refused() {
    let _g = serial();
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("untrusted");
    std::fs::create_dir_all(&target).unwrap();
    // Deliberately not `run_git`: that trusts the repository on init.
    let init = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&target)
        .output_locked()
        .unwrap();
    assert!(init.status.success());
    let link = parent.path().join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    #[cfg(not(unix))]
    return;
    let refused = tuning_apply(
        str_of(&link),
        &[TuningChange::UntrackedCache],
        &[many_files_argv()],
    )
    .unwrap_err();
    assert!(
        refused.message().to_lowercase().contains("trust"),
        "{}",
        refused.message()
    );
    assert_eq!(local_value(&target, "core.untrackedCache"), None);
}

#[test]
fn adv_config_changed_between_preview_and_confirm_is_refused() {
    let _g = serial();
    let parent = tempfile::tempdir().unwrap();
    let main = parent.path().join("main");
    init_with_files(&main, ABOVE);
    let path = str_of(&main);
    let previewed = tuning_plan(path, &[TuningChange::UntrackedCache])
        .unwrap()
        .runnable();
    // The person (or another tool) turns split index on after the preview.
    run_git(&main, &["config", "--local", "core.splitIndex", "true"]);
    let refused = tuning_apply(path, &[TuningChange::UntrackedCache], &previewed).unwrap_err();
    assert!(
        refused.message().contains("add-then-commit"),
        "{}",
        refused.message()
    );
    assert_eq!(local_value(&main, "core.untrackedCache"), None);
    // Or sets the key themselves: the stale confirm must not overwrite it.
    run_git(&main, &["config", "--local", "--unset", "core.splitIndex"]);
    run_git(
        &main,
        &["config", "--local", "core.untrackedCache", "false"],
    );
    let refused = tuning_apply(path, &[TuningChange::UntrackedCache], &previewed);
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        local_value(&main, "core.untrackedCache").as_deref(),
        Some("false")
    );
}

// ---------------------------------------------------------------------------
// Hazards
// ---------------------------------------------------------------------------

/// BREAKS: a split index is a property of the index file (its `link`
/// extension, with a `sharedindex.<hash>` file beside it), not only of
/// `core.splitIndex`: `git update-index --split-index` leaves it in place with
/// no config key, and git keeps splitting while the key is unset. Over such an
/// index, `feature.manyFiles` reproduced `BUG: wt-status.c:585: multiple renames
/// on the same target? how?` and a stuck index.lock on git 2.54 / Apple
/// Git-157 (5-file repository, scratch run in the review log). The detector's
/// header count of a real split index happened to fall below the threshold in
/// every fixture tried (6,958 of 20,500; 7,056 of 62,000), which is luck, not a
/// check: git lets the delta grow to splitIndex.maxPercentChange (20 percent)
/// of the base before it merges, so a repository past about 100,000 files can
/// clear the threshold. This fixture stands in for that index.
#[test]
fn adv_split_index_carried_by_the_index_file_is_not_offered_manyfiles() {
    let dir = tempfile::tempdir().unwrap();
    index_file(dir.path(), 2, 30_000, 30_000 * 62 + 64);
    std::fs::write(
        dir.path()
            .join("sharedindex.0123456789abcdef0123456789abcdef01234567"),
        b"DIRC",
    )
    .unwrap();
    let report = report_for(dir.path());
    let offers = recommend(&report, env_with((2, 54, 0)));
    assert!(
        offers
            .iter()
            .all(|o| o.change != TuningChange::UntrackedCache),
        "feature.manyFiles offered over an index with a shared index beside it: {offers:?}"
    );
}

/// BREAKS: `git config --local` writes the COMMON config, so manyFiles lands
/// in every linked worktree, but the conflict check only reads the effective
/// config of the worktree the click came from. A sibling worktree that sets
/// `core.splitIndex` in its own `config.worktree` is not seen.
#[test]
fn adv_split_index_in_a_sibling_worktree_is_refused() {
    let _g = serial();
    let parent = tempfile::tempdir().unwrap();
    let main = parent.path().join("main");
    init_with_files(&main, ABOVE);
    let sibling = parent.path().join("sibling");
    run_git(
        &main,
        &["worktree", "add", "-q", "-b", "sib", str_of(&sibling)],
    );
    run_git(
        &main,
        &["config", "--local", "extensions.worktreeConfig", "true"],
    );
    run_git(
        &sibling,
        &["config", "--worktree", "core.splitIndex", "true"],
    );
    let plan = tuning_plan(str_of(&main), &[TuningChange::UntrackedCache]);
    assert!(
        plan.is_err(),
        "the plan would write the shared config over a sibling worktree's split index: {plan:?}"
    );
}

#[test]
fn adv_global_split_index_is_refused() {
    let _g = serial();
    // The application's git strips GIT_CONFIG_GLOBAL and reads $HOME/.gitconfig
    // (git_cli::STRIPPED_GIT_ENV), so the fixture writes that file; `HOME` is
    // this process's hermetic home, never the machine's.
    let global = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".gitconfig");
    struct Restore(std::path::PathBuf, Option<Vec<u8>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.1 {
                Some(bytes) => std::fs::write(&self.0, bytes).unwrap(),
                None => {
                    let _ = std::fs::remove_file(&self.0);
                }
            }
        }
    }
    let before = std::fs::read(&global).ok();
    let _restore = Restore(global.clone(), before.clone());
    let parent = tempfile::tempdir().unwrap();
    let main = parent.path().join("main");
    init_with_files(&main, ABOVE);
    let mut text = before.unwrap_or_default();
    text.extend_from_slice(b"\n[core]\n\tsplitIndex = true\n");
    std::fs::write(&global, text).unwrap();
    let refused = tuning_plan(str_of(&main), &[TuningChange::UntrackedCache]).unwrap_err();
    assert!(
        refused.message().contains("add-then-commit"),
        "{}",
        refused.message()
    );
    assert_eq!(local_value(&main, "core.untrackedCache"), None);
}

#[test]
fn adv_existing_fsmonitor_values_are_never_overwritten() {
    let _g = serial();
    let parent = tempfile::tempdir().unwrap();
    let main = parent.path().join("main");
    init_with_files(&main, ABOVE_FSMONITOR);
    for value in [
        "false",
        ".git/hooks/query-watchman",
        "/usr/local/bin/watchman-hook",
    ] {
        run_git(&main, &["config", "--local", "core.fsmonitor", value]);
        let refused = tuning_plan(str_of(&main), &[TuningChange::Fsmonitor]);
        assert!(refused.is_err(), "{value}: {refused:?}");
        assert!(tuning_apply(str_of(&main), &[TuningChange::Fsmonitor], &[]).is_err());
        assert_eq!(local_value(&main, "core.fsmonitor").as_deref(), Some(value));
    }
}

fn index_file(dir: &Path, version: u32, entries: u32, len: usize) {
    let mut bytes = vec![0u8; len];
    bytes[..4].copy_from_slice(b"DIRC");
    bytes[4..8].copy_from_slice(&version.to_be_bytes());
    bytes[8..12].copy_from_slice(&entries.to_be_bytes());
    std::fs::write(dir.join("index"), bytes).unwrap();
}

/// A structurally valid version 2 index of `entries` short paths: the
/// detector walks the whole body now, so a header over zeros is (rightly) not
/// an index, and a test about the fsmonitor floor needs a real one.
fn valid_index_file(dir: &Path, entries: u32) {
    let mut bytes = b"DIRC".to_vec();
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&entries.to_be_bytes());
    for i in 0..entries {
        let mut entry = vec![0u8; 40];
        entry.extend([0x5A; 20]);
        let name = format!("f{i:07}");
        entry.extend_from_slice(&(name.len() as u16).to_be_bytes());
        entry.extend_from_slice(name.as_bytes());
        let total = (entry.len() + 8) & !7;
        entry.resize(total, 0);
        bytes.extend(entry);
    }
    bytes.extend([1u8; 20]);
    std::fs::write(dir.join("index"), bytes).unwrap();
}

fn env_with(version: (u32, u32, u32)) -> GitEnv {
    GitEnv {
        version: Some(version),
        fsmonitor_measured: true,
    }
}

fn report_for(dir: &Path) -> gitpulse_lib::storage::repo_tuning::RepoTuningReport {
    inspect_with(dir, dir, dir, None, Ok(&[]))
}

/// BREAKS: `core.fsmonitor=true` as a boolean arrived in git 2.36.0 together
/// with the built-in daemon. 2.35.2 is a security-only release of 2.35.1, so
/// it still reads `true` as a hook path; the guard stops at "2.35.2 or newer".
/// (Inferred from git's release history, not re-run: no 2.35.x git is
/// installed here.)
#[test]
fn adv_fsmonitor_boolean_is_refused_on_git_before_2_36() {
    let dir = tempfile::tempdir().unwrap();
    valid_index_file(dir.path(), 45_000);
    let report = report_for(dir.path());
    assert_eq!(report.index.entries, Some(45_000), "{report:?}");
    for version in [(2, 35, 1), (2, 35, 2), (2, 35, 9), (2, 34, 0)] {
        let refused = plan_apply(&report, env_with(version), "/x", &[TuningChange::Fsmonitor]);
        assert!(refused.is_err(), "git {version:?}: {refused:?}");
    }
    let unknown = GitEnv {
        version: None,
        fsmonitor_measured: true,
    };
    assert!(plan_apply(&report, unknown, "/x", &[TuningChange::Fsmonitor]).is_err());
    assert!(plan_apply(
        &report,
        env_with((2, 36, 0)),
        "/x",
        &[TuningChange::Fsmonitor]
    )
    .is_ok());
}

// ---------------------------------------------------------------------------
// Detector honesty
// ---------------------------------------------------------------------------

#[test]
fn adv_unreadable_indexes_are_never_offered() {
    let env = env_with((2, 54, 0));
    let big = 30_000u32 as usize * 62 + 64;
    let cases: [(&str, Box<dyn Fn(&Path)>); 5] = [
        (
            "version 5",
            Box::new(move |d: &Path| index_file(d, 5, 30_000, big)),
        ),
        (
            "bad signature",
            Box::new(move |d: &Path| {
                index_file(d, 2, 30_000, big);
                let mut b = std::fs::read(d.join("index")).unwrap();
                b[0] = b'X';
                std::fs::write(d.join("index"), b).unwrap();
            }),
        ),
        (
            "huge count, tiny file",
            Box::new(|d: &Path| index_file(d, 2, u32::MAX, 64)),
        ),
        (
            "shorter than a header",
            Box::new(|d: &Path| std::fs::write(d.join("index"), b"DIRC").unwrap()),
        ),
        ("missing", Box::new(|_: &Path| {})),
    ];
    for (name, make) in cases {
        let dir = tempfile::tempdir().unwrap();
        make(dir.path());
        let report = report_for(dir.path());
        assert!(report.index.entries.is_none(), "{name}: {:?}", report.index);
        assert!(
            recommend(&report, env)
                .iter()
                .all(|o| o.change != TuningChange::UntrackedCache),
            "{name}: offered over an unreadable index"
        );
    }
}

/// BREAKS (low): a header that claims 30,000 entries over a file that is
/// 2 MB of zeros (cut off mid-write, or not an index at all after its first
/// twelve bytes) passes the "could the file hold that many" check and is
/// reported as 30,000 tracked files, which clears the manyFiles threshold. The
/// trailer is read for skipHash but never verified as a checksum.
#[test]
fn adv_a_header_over_a_garbage_body_is_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    index_file(dir.path(), 2, 30_000, 2_000_000);
    let report = report_for(dir.path());
    let offers = recommend(&report, env_with((2, 54, 0)));
    assert!(
        offers
            .iter()
            .all(|o| o.change != TuningChange::UntrackedCache),
        "offered over a body that is not an index: {offers:?}"
    );
}

// ---------------------------------------------------------------------------
// Worktrees
// ---------------------------------------------------------------------------

/// Applied from a linked worktree, the write lands in the common config, with
/// and without `extensions.worktreeConfig`, which is what the preview's scope
/// ("shared by its worktrees") says.
#[test]
fn adv_apply_from_a_linked_worktree_writes_the_common_config() {
    let _g = serial();
    for worktree_config in [false, true] {
        let parent = tempfile::tempdir().unwrap();
        let main = parent.path().join("main");
        init_with_files(&main, ABOVE);
        if worktree_config {
            run_git(
                &main,
                &["config", "--local", "extensions.worktreeConfig", "true"],
            );
        }
        let linked = parent.path().join("linked");
        run_git(
            &main,
            &["worktree", "add", "-q", "-b", "lnk", str_of(&linked)],
        );
        trust_repo(&linked);
        let plan = tuning_plan(str_of(&linked), &[TuningChange::UntrackedCache]).unwrap();
        let outcome = tuning_apply(
            str_of(&linked),
            &[TuningChange::UntrackedCache],
            &plan.runnable(),
        )
        .unwrap();
        assert_eq!(
            format!("{:?}", outcome.results[0].status),
            "Applied",
            "{outcome:?}"
        );
        assert_eq!(
            local_value(&main, "core.untrackedCache").as_deref(),
            Some("true"),
            "worktreeConfig={worktree_config}"
        );
        let own = linked.join(".git");
        assert!(own.is_file(), "a linked worktree's .git is a gitfile");
        let worktree_file = Command::new("git")
            .args(["config", "--worktree", "--get", "core.untrackedCache"])
            .current_dir(&linked)
            .output_locked()
            .unwrap();
        assert!(
            !worktree_file.status.success() || !worktree_config,
            "the key must not land in config.worktree"
        );
    }
}
