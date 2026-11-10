//! What `try_add_worktree_cow_macos` may clone and what git must check out.
//!
//! A clone is only right when the donor's working-tree bytes are exactly what
//! `git checkout` writes for that path in the new worktree. The oracle in
//! these tests is a plain `git worktree add` of the same commit.

use super::*;
use crate::test_support::{git_in, trust_repo};
use std::time::{Duration, SystemTime};

/// Tracked files that conversion cannot change, and so may be cloned.
const CLONEABLE: &[&str] = &[".gitattributes", "plain.txt", "sub/plain2.rs"];
/// Tracked files git must write, because the form checkout produces depends
/// on a filter, an encoding, `ident` or a CRLF conversion.
const CHECKED_OUT: &[&str] = &["run.bat", "doc.filt", "a.id", "w.enc"];

fn old_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

fn set_mtime(path: &Path) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(old_time())
        .unwrap();
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A GitPulse-shaped donor: `* text=auto eol=lf` plus a CRLF script, a real
/// clean/smudge filter, an `ident` file and a re-encoded file, all checked out
/// by git and then made old so a fresh mtime means "written by this lane".
fn donor(sandbox: &Path) -> PathBuf {
    let main = sandbox.join("main");
    std::fs::create_dir_all(main.join("sub")).unwrap();
    git_in(&main, &["init", "-b", "main"]);
    git_in(&main, &["config", "filter.up.clean", "tr A-Z a-z"]);
    git_in(&main, &["config", "filter.up.smudge", "tr a-z A-Z"]);
    trust_repo(&main);
    std::fs::write(
        main.join(".gitattributes"),
        "* text=auto eol=lf\n*.bat text eol=crlf\n*.filt filter=up\n*.id ident\n\
         *.enc text working-tree-encoding=UTF-16LE eol=lf\n",
    )
    .unwrap();
    std::fs::write(main.join("plain.txt"), "alpha\nbeta\n").unwrap();
    std::fs::write(main.join("sub/plain2.rs"), "fn main() {}\n").unwrap();
    std::fs::write(main.join("run.bat"), "echo hi\n").unwrap();
    std::fs::write(main.join("doc.filt"), "Hello\n").unwrap();
    std::fs::write(main.join("a.id"), "$Id$\n").unwrap();
    std::fs::write(main.join("w.enc"), utf16le("h\u{e9}llo\n")).unwrap();
    git_in(&main, &["add", "."]);
    git_in(&main, &["commit", "-m", "initial"]);

    // Make the donor's tree what a checkout writes (filters, CRLF, ident and
    // encoding applied), then age every file.
    for rel in CLONEABLE.iter().chain(CHECKED_OUT) {
        std::fs::remove_file(main.join(rel)).unwrap();
    }
    git_in(&main, &["checkout", "--", "."]);
    for rel in CLONEABLE.iter().chain(CHECKED_OUT) {
        set_mtime(&main.join(rel));
    }
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();
    let status = crate::engine::git_cli::git_captured(&main, &["status", "--porcelain"]).unwrap();
    assert!(
        status.stdout.is_empty(),
        "fixture precondition: donor must be clean, got {:?}",
        String::from_utf8_lossy(&status.stdout)
    );
    main
}

#[test]
fn cow_checkout_runs_on_text_auto_eol_lf_and_leaves_converted_paths_to_git() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = donor(sandbox.path());
    let oracle = sandbox.path().join("oracle");
    git_in(
        &main,
        &["worktree", "add", "-b", "oracle", oracle.to_str().unwrap()],
    );
    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");

    for rel in CLONEABLE.iter().chain(CHECKED_OUT) {
        assert_eq!(
            std::fs::read(lane.join(rel)).unwrap(),
            std::fs::read(oracle.join(rel)).unwrap(),
            "{rel} must be byte-identical to a plain `git worktree add`"
        );
    }
    for rel in CLONEABLE {
        assert_eq!(
            mtime(&lane.join(rel)),
            old_time(),
            "{rel} was not cloned: its mtime was not preserved"
        );
    }
    for rel in CHECKED_OUT {
        assert_ne!(
            mtime(&lane.join(rel)),
            old_time(),
            "{rel} was cloned from the donor but only git may write it"
        );
    }
    let status = crate::engine::git_cli::git_captured(&lane, &["status", "--porcelain"]).unwrap();
    assert!(status.stdout.is_empty(), "lane must be clean");
}

#[test]
fn autocrlf_true_leaves_every_text_path_to_git_and_matches_a_plain_checkout() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = sandbox.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-b", "main"]);
    git_in(&main, &["config", "core.autocrlf", "true"]);
    trust_repo(&main);
    std::fs::write(main.join("a.txt"), "one\ntwo\n").unwrap();
    git_in(&main, &["add", "."]);
    git_in(&main, &["commit", "-m", "initial"]);
    std::fs::remove_file(main.join("a.txt")).unwrap();
    git_in(&main, &["checkout", "--", "."]);
    assert_eq!(
        std::fs::read(main.join("a.txt")).unwrap(),
        b"one\r\ntwo\r\n",
        "fixture: the donor's checkout converted to CRLF"
    );
    set_mtime(&main.join("a.txt"));
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();

    let oracle = sandbox.path().join("oracle");
    git_in(
        &main,
        &["worktree", "add", "-b", "oracle", oracle.to_str().unwrap()],
    );
    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");

    assert_eq!(
        std::fs::read(lane.join("a.txt")).unwrap(),
        std::fs::read(oracle.join("a.txt")).unwrap()
    );
    assert_ne!(
        mtime(&lane.join("a.txt")),
        old_time(),
        "a CRLF-converting configuration must not clone the donor's bytes"
    );
}

fn load1() -> f64 {
    let mut load = [0f64; 3];
    // SAFETY: `getloadavg` writes at most the 3 doubles it is given.
    unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
    load[0]
}

/// (min, median, max) of `values`, which must not be empty.
fn spread(values: &[f64]) -> (f64, f64, f64) {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    (
        sorted[0],
        sorted[sorted.len() / 2],
        sorted[sorted.len() - 1],
    )
}

/// A round whose 1-minute load average, before or after, is above this is
/// reported as contaminated, never dropped.
const QUIET_LOAD: f64 = 30.0;

fn measure_env() -> (PathBuf, usize) {
    let repo = PathBuf::from(std::env::var("GITPULSE_COW_MEASURE_REPO").expect("repo path"));
    let runs = std::env::var("GITPULSE_COW_MEASURE_N")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(5);
    trust_repo(&repo);
    (repo, runs)
}

/// Measurement, not a check. Creates `GITPULSE_COW_MEASURE_N` (default 5)
/// lanes of each arm from the scratch repository named by
/// `GITPULSE_COW_MEASURE_REPO`, interleaved, both through `add_worktree` (the
/// lock, validation and account marker are in both). `plain` passes neither a
/// branch nor `--detach`, which `try_add_worktree_cow_macos` declines, so
/// `add_worktree` runs the plain `git worktree add`; `cow` passes a branch.
/// Wall time is reported without and with a `sync(2)` flush of the lane's
/// freshly written pages, with the load average either side of each lane, and
/// the bytes the lane owns (clone-private; a clone of the donor owns nothing).
#[test]
#[ignore = "measurement: set GITPULSE_COW_MEASURE_REPO to a scratch clone"]
fn cow_lane_creation_cost() {
    let (repo, runs) = measure_env();
    let parent = repo.parent().unwrap().to_path_buf();
    let mut rows: Vec<(&str, f64, f64, f64, bool)> = Vec::new();
    for run in 0..runs {
        let arms = if run % 2 == 0 {
            ["plain", "cow"]
        } else {
            ["cow", "plain"]
        };
        for arm in arms {
            let name = format!("lane-{arm}-{run}");
            let lane = parent.join(&name);
            let before = load1();
            let started = std::time::Instant::now();
            let branch = (arm == "cow").then_some(name.as_str());
            add_worktree(
                repo.to_str().unwrap(),
                lane.to_str().unwrap(),
                branch,
                None,
                false,
            )
            .expect("add_worktree");
            let wall = started.elapsed().as_secs_f64() * 1000.0;
            // SAFETY: `sync(2)` takes no arguments.
            unsafe { libc::sync() };
            let flushed = started.elapsed().as_secs_f64() * 1000.0;
            let after = load1();
            let (private, logical, files, unread) =
                crate::storage::clone_size::private_tree_bytes(&lane);
            let contaminated = before > QUIET_LOAD || after > QUIET_LOAD;
            println!(
                "MEASURE arm={arm} run={run} wall_ms={wall:.0} flushed_ms={flushed:.0} private_bytes={private} logical_bytes={logical} files={files} unread={unread} load_before={before:.1} load_after={after:.1} contaminated={contaminated}"
            );
            rows.push((arm, wall, flushed, load1(), contaminated));
            git_in(
                &repo,
                &["worktree", "remove", "--force", lane.to_str().unwrap()],
            );
            git_in(&repo, &["branch", "-D", &name]);
        }
    }
    for arm in ["plain", "cow"] {
        for (label, only_quiet) in [("all", false), ("quiet", true)] {
            let of: Vec<_> = rows
                .iter()
                .filter(|row| row.0 == arm && (!only_quiet || !row.4))
                .collect();
            if of.is_empty() {
                println!("SUMMARY arm={arm} rounds={label} n=0");
                continue;
            }
            let wall: Vec<f64> = of.iter().map(|row| row.1).collect();
            let flushed: Vec<f64> = of.iter().map(|row| row.2).collect();
            let (w0, w1, w2) = spread(&wall);
            let (f0, f1, f2) = spread(&flushed);
            println!(
                "SUMMARY arm={arm} rounds={label} n={} wall_ms min/median/max={w0:.0}/{w1:.0}/{w2:.0} flushed_ms min/median/max={f0:.0}/{f1:.0}/{f2:.0}",
                of.len()
            );
        }
    }
}

/// Measurement, not a check. Runs each phase of the clone path by itself, in
/// the order `try_add_worktree_cow_macos` runs them and through the same
/// helpers and the same git wrapper, and prints each phase's min / median /
/// max over `GITPULSE_COW_MEASURE_N` rounds. The phases are re-run here, not
/// instrumented in production code, so their sum is the clone path without
/// `add_worktree`'s own overhead. One raw plain `git worktree add` is traced
/// with `GIT_TRACE_PERFORMANCE` for the plain arm's breakdown.
#[test]
#[ignore = "measurement: set GITPULSE_COW_MEASURE_REPO to a scratch clone"]
fn cow_phase_timings() {
    use crate::procguard::LockedSpawn;
    use std::collections::HashMap;

    let (repo, runs) = measure_env();
    let parent = repo.parent().unwrap().to_path_buf();
    let mut phases: Vec<(String, Vec<f64>)> = Vec::new();
    let mut totals: Vec<f64> = Vec::new();
    let mut contaminated = 0usize;

    fn timed<T>(phases: &mut Vec<(String, Vec<f64>)>, name: &str, f: impl FnOnce() -> T) -> T {
        let started = std::time::Instant::now();
        let out = f();
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        match phases.iter_mut().find(|(n, _)| n == name) {
            Some((_, samples)) => samples.push(ms),
            None => phases.push((name.to_string(), vec![ms])),
        }
        out
    }

    for run in 0..runs {
        let before = load1();
        let round_started = std::time::Instant::now();
        let name = format!("phase-lane-{run}");
        let target = parent.join(&name);
        let target_text = target.to_str().unwrap().to_string();

        let ls_donor = timed(&mut phases, "donor: ls-files -s -z", || {
            crate::engine::git_cli::git_captured(&repo, &["ls-files", "-s", "-z"]).unwrap()
        });
        let donor_entries = parse_ls_files_stage0(&ls_donor.stdout);
        let status = timed(&mut phases, "donor: status --porcelain -z", || {
            crate::engine::git_cli::git_captured(
                &repo,
                &["status", "--porcelain=v1", "-z", "--untracked-files=no"],
            )
            .unwrap()
        });
        let dirty = parse_porcelain_dirty_paths_z(&status.stdout);
        let mut donor_clean: HashMap<String, (String, String)> = HashMap::new();
        for (mode, oid, rel) in donor_entries {
            if !dirty.contains(&rel) && matches!(mode.as_str(), "100644" | "100755") {
                donor_clean.insert(rel, (mode, oid));
            }
        }
        timed(&mut phases, "hook probe: git hook list", || {
            post_checkout_hook_runs(&repo)
        });
        timed(&mut phases, "worktree add --no-checkout", || {
            git_text(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--no-checkout",
                    "-b",
                    &name,
                    &target_text,
                ],
            )
            .unwrap()
        });
        timed(&mut phases, "target: read-tree HEAD", || {
            git_text(&target, &["read-tree", "HEAD"]).unwrap()
        });
        let ls_target = timed(&mut phases, "target: ls-files -s -z", || {
            crate::engine::git_cli::git_captured(&target, &["ls-files", "-s", "-z"]).unwrap()
        });
        let target_entries = parse_ls_files_stage0(&ls_target.stdout);
        let candidates: Vec<&str> = target_entries
            .iter()
            .filter(|(mode, oid, rel)| {
                matches!(mode.as_str(), "100644" | "100755")
                    && named_exactly_by_stdin_paths(rel)
                    && donor_clean
                        .get(rel)
                        .is_some_and(|(d_mode, d_oid)| d_mode == mode && d_oid == oid)
            })
            .map(|(_, _, rel)| rel.as_str())
            .collect();
        let converted = timed(
            &mut phases,
            "target: config x2 + check-attr --cached",
            || paths_converted_on_checkout(&target, &candidates).unwrap(),
        );
        let mut cloned: Vec<(String, String)> = Vec::new();
        let candidate_set: std::collections::HashSet<&str> = candidates.iter().copied().collect();
        timed(
            &mut phases,
            "clonefile loop (create_dir_all + clone)",
            || {
                let mut created: std::collections::HashSet<PathBuf> =
                    std::collections::HashSet::new();
                for (_, oid, rel) in &target_entries {
                    if !candidate_set.contains(rel.as_str()) || converted.contains(rel) {
                        continue;
                    }
                    let dst = target.join(rel);
                    if let Some(parent) = dst.parent() {
                        if created.insert(parent.to_path_buf()) {
                            std::fs::create_dir_all(parent).unwrap();
                        }
                    }
                    if try_apfs_clone_regular_file(&repo.join(rel), &dst) {
                        cloned.push((rel.clone(), oid.clone()));
                    }
                }
            },
        );
        let wrong = timed(&mut phases, "hash-object --stdin-paths (verify)", || {
            clones_that_are_not_their_blob(&target, &cloned).unwrap()
        });
        assert!(wrong.is_empty(), "scratch clone is clean: {wrong:?}");
        timed(&mut phases, "update-index --refresh", || {
            crate::engine::git_cli::git_captured(&target, &["update-index", "-q", "--refresh"])
                .unwrap()
        });
        timed(&mut phases, "diff-files --name-only", || {
            crate::engine::git_cli::git_captured(&target, &["diff-files", "--name-only", "-z"])
                .unwrap()
        });
        totals.push(round_started.elapsed().as_secs_f64() * 1000.0);
        let after = load1();
        if before > QUIET_LOAD || after > QUIET_LOAD {
            contaminated += 1;
        }
        println!(
            "ROUND run={run} cloned={} spawned_phases_total_ms={:.0} load_before={before:.1} load_after={after:.1}",
            cloned.len(),
            totals[run]
        );
        git_in(&repo, &["worktree", "remove", "--force", &target_text]);
        git_in(&repo, &["branch", "-D", &name]);
    }
    for (name, samples) in &phases {
        let (lo, mid, hi) = spread(samples);
        println!(
            "PHASE {name}: min/median/max_ms={lo:.1}/{mid:.1}/{hi:.1} n={}",
            samples.len()
        );
    }
    let (lo, mid, hi) = spread(&totals);
    println!(
        "PHASE_TOTAL min/median/max_ms={lo:.0}/{mid:.0}/{hi:.0} n={} contaminated_rounds={contaminated}",
        totals.len()
    );

    // The plain path's own breakdown: one raw `git worktree add` traced.
    let trace = parent.join("plain-trace.txt");
    let name = "plain-traced";
    let lane = parent.join(name);
    let mut command = std::process::Command::new("git");
    command
        .args(["worktree", "add", "-b", name, lane.to_str().unwrap()])
        .current_dir(&repo)
        .env("GIT_TRACE_PERFORMANCE", &trace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::test_support::pin_git(&mut command);
    let output = command.spawn_locked().unwrap().wait_with_output().unwrap();
    assert!(output.status.success());
    for line in std::fs::read_to_string(&trace).unwrap_or_default().lines() {
        println!("PLAIN_TRACE {line}");
    }
    git_in(
        &repo,
        &["worktree", "remove", "--force", lane.to_str().unwrap()],
    );
    git_in(&repo, &["branch", "-D", name]);
}

fn attrs(
    text: &str,
    eol: &str,
    crlf: &str,
    filter: &str,
    enc: &str,
    ident: &str,
) -> ConversionAttrs {
    ConversionAttrs {
        text: text.into(),
        eol: eol.into(),
        crlf: crlf.into(),
        filter: filter.into(),
        encoding: enc.into(),
        ident: ident.into(),
    }
}

/// A repository with files named so that `git hash-object --stdin-paths`
/// would read a different file than the one named: it unquotes a line that
/// starts with `"` and drops a trailing CR.
fn repo_with_awkward_names(sandbox: &Path, same_content: bool) -> PathBuf {
    let repo = sandbox.join("main");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-b", "main"]);
    trust_repo(&repo);
    let (quoted, cr) = if same_content {
        ("one\n", "three\n")
    } else {
        ("two\n", "four\n")
    };
    std::fs::write(repo.join("x"), "one\n").unwrap();
    std::fs::write(repo.join("\"x\""), quoted).unwrap();
    std::fs::write(repo.join("y"), "three\n").unwrap();
    std::fs::write(repo.join("y\r"), cr).unwrap();
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-m", "initial"]);
    repo
}

#[test]
fn a_clone_is_never_verified_against_a_file_that_stdin_paths_would_name_instead() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let repo = repo_with_awkward_names(sandbox.path(), false);
    let ls = crate::engine::git_cli::git_captured(&repo, &["ls-files", "-s", "-z"]).unwrap();
    let cloned: Vec<(String, String)> = parse_ls_files_stage0(&ls.stdout)
        .into_iter()
        .map(|(_, oid, rel)| (rel, oid))
        .collect();
    assert_eq!(cloned.len(), 4, "fixture: all four names are tracked");
    // `"x"` and `y\r` are stale; the files `--stdin-paths` would open instead
    // (`x`, `y`) hold exactly the bytes of the blobs those two should have.
    std::fs::write(repo.join("\"x\""), "stale\n").unwrap();
    std::fs::write(repo.join("x"), "two\n").unwrap();
    std::fs::write(repo.join("y\r"), "stale\n").unwrap();
    std::fs::write(repo.join("y"), "four\n").unwrap();

    let wrong = clones_that_are_not_their_blob(&repo, &cloned).unwrap();
    for name in ["\"x\"", "y\r"] {
        assert!(
            wrong.iter().any(|path| path == name),
            "{name:?} is stale and must be reported, got {wrong:?}"
        );
    }
}

#[test]
fn paths_stdin_paths_cannot_name_exactly_are_checked_out_by_git_not_cloned() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = repo_with_awkward_names(sandbox.path(), true);
    for rel in ["x", "\"x\"", "y", "y\r"] {
        set_mtime(&main.join(rel));
    }
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();
    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");

    for rel in ["x", "y"] {
        assert_eq!(mtime(&lane.join(rel)), old_time(), "{rel:?} is cloned");
    }
    for rel in ["\"x\"", "y\r"] {
        assert_ne!(
            mtime(&lane.join(rel)),
            old_time(),
            "{rel:?} cannot be named to `git hash-object --stdin-paths`, so it is not cloned"
        );
        assert_eq!(
            std::fs::read(lane.join(rel)).unwrap(),
            std::fs::read(main.join(rel)).unwrap()
        );
    }
}

#[cfg(unix)]
#[test]
fn a_repository_with_a_post_checkout_hook_gets_it_run_by_plain_git() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = sandbox.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-b", "main"]);
    trust_repo(&main);
    std::fs::write(main.join("a.txt"), "a\n").unwrap();
    git_in(&main, &["add", "."]);
    git_in(&main, &["commit", "-m", "initial"]);
    set_mtime(&main.join("a.txt"));
    let marker = sandbox.path().join("hook-ran");
    let hook = main.join(".git/hooks/post-checkout");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\necho ran > '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();

    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");

    assert!(
        marker.exists(),
        "`git worktree add --no-checkout` skips post-checkout, so the clone path must not be taken"
    );
    assert_eq!(std::fs::read(lane.join("a.txt")).unwrap(), b"a\n");
}

/// Git 2.56 can define a hook in config (`hook.<name>.command` and
/// `hook.<name>.event`). `git rev-parse --git-path hooks/post-checkout` still
/// prints a path where no file exists, so only asking `git hook list` finds it.
#[cfg(unix)]
#[test]
fn a_post_checkout_hook_defined_in_git_config_is_run_by_plain_git() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = sandbox.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-b", "main"]);
    trust_repo(&main);
    std::fs::write(main.join("a.txt"), "a\n").unwrap();
    git_in(&main, &["add", "."]);
    git_in(&main, &["commit", "-m", "initial"]);
    set_mtime(&main.join("a.txt"));
    let marker = sandbox.path().join("hook-ran");
    git_in(
        &main,
        &[
            "config",
            "hook.mark.command",
            &format!("echo ran > '{}'", marker.display()),
        ],
    );
    git_in(&main, &["config", "hook.mark.event", "post-checkout"]);
    let listed =
        crate::engine::git_cli::git_captured(&main, &["hook", "list", "post-checkout"]).unwrap();
    if listed.status_code != 0 {
        // This git has no config-defined hooks: nothing to run, nothing to test.
        return;
    }
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();

    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");

    assert!(
        marker.exists(),
        "a hook defined in config runs on plain `git worktree add` and not on `--no-checkout`, so the clone path must not be taken"
    );
}

#[test]
fn when_git_cannot_say_whether_a_hook_runs_the_clone_path_is_not_taken() {
    let file_says = |answer| move || answer;
    // `git hook list` answers: 0 hooks run, 1 none do.
    assert_eq!(
        post_checkout_hook_decision(Some(0), file_says(Some(false))),
        Some(true)
    );
    assert_eq!(
        post_checkout_hook_decision(Some(1), file_says(Some(true))),
        Some(false)
    );
    // A git without the subcommand (usage error): the hook file is the answer.
    assert_eq!(
        post_checkout_hook_decision(Some(129), file_says(Some(false))),
        Some(false)
    );
    assert_eq!(
        post_checkout_hook_decision(Some(129), file_says(Some(true))),
        Some(true)
    );
    // Neither can answer: unknown, which the caller reads as "has a hook".
    assert_eq!(
        post_checkout_hook_decision(Some(129), file_says(None)),
        None
    );
    assert_eq!(
        post_checkout_hook_decision(Some(128), file_says(Some(false))),
        None
    );
    assert_eq!(
        post_checkout_hook_decision(None, file_says(Some(false))),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_hook_that_is_not_executable_does_not_stop_the_clone() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = sandbox.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-b", "main"]);
    trust_repo(&main);
    std::fs::write(main.join("a.txt"), "a\n").unwrap();
    git_in(&main, &["add", "."]);
    git_in(&main, &["commit", "-m", "initial"]);
    set_mtime(&main.join("a.txt"));
    std::fs::write(main.join(".git/hooks/post-checkout"), "#!/bin/sh\nexit 1\n").unwrap();
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();

    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");
    assert_eq!(
        mtime(&lane.join("a.txt")),
        old_time(),
        "git skips a non-executable hook, so the clone path is still right"
    );
}

#[test]
fn conversion_decision_table() {
    let u = "unspecified";
    // (attributes, default_crlf, converted)
    let table = [
        (attrs(u, u, u, u, u, u), false, false),
        (attrs(u, u, u, u, u, u), true, true),
        (attrs("auto", "lf", u, u, u, u), true, false),
        (attrs("set", "lf", u, u, u, u), false, false),
        (attrs("set", "crlf", u, u, u, u), false, true),
        (attrs("auto", "crlf", u, u, u, u), false, true),
        (attrs("unset", "crlf", u, u, u, u), true, false),
        (attrs("set", u, u, u, u, u), true, true),
        (attrs("set", u, u, u, u, u), false, false),
        (attrs("auto", "weird", u, u, u, u), false, true),
        (attrs(u, "lf", "input", u, u, u), false, true),
        (attrs("unset", u, "unset", u, u, u), false, true),
        (attrs(u, "lf", u, "lfs", u, u), false, true),
        (attrs(u, "lf", u, "unset", u, u), false, false),
        (attrs(u, "lf", u, u, "UTF-16LE", u), false, true),
        (attrs(u, "lf", u, u, u, "set"), false, true),
        (attrs(u, "lf", u, u, u, "unset"), false, false),
    ];
    for (a, default_crlf, expected) in table {
        assert_eq!(
            a.converted_on_checkout(default_crlf),
            expected,
            "{a:?} with default_crlf={default_crlf}"
        );
    }
}

#[test]
fn check_attr_output_must_name_every_attribute_for_every_path() {
    let full = b"a\0text\0auto\0a\0eol\0lf\0a\0crlf\0unspecified\0a\0filter\0unspecified\0\
                 a\0working-tree-encoding\0unspecified\0a\0ident\0unspecified\0";
    let parsed = parse_conversion_attrs(full).unwrap();
    assert_eq!(parsed["a"].text, "auto");
    assert_eq!(parsed["a"].eol, "lf");
    let without_ident = full.strip_suffix(b"a\0ident\0unspecified\0").unwrap();
    assert!(
        parse_conversion_attrs(without_ident).is_err(),
        "a path missing an attribute is an error, not a path with nothing to convert"
    );
    assert!(parse_conversion_attrs(b"a\0text\0auto\0").is_err());
}

#[test]
fn a_clone_whose_bytes_are_not_its_blob_is_reported() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let repo = sandbox.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-b", "main"]);
    trust_repo(&repo);
    std::fs::write(repo.join("same.txt"), "same\n").unwrap();
    std::fs::write(repo.join("other.txt"), "committed\n").unwrap();
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-m", "initial"]);
    let ls = crate::engine::git_cli::git_captured(&repo, &["ls-files", "-s", "-z"]).unwrap();
    let cloned: Vec<(String, String)> = parse_ls_files_stage0(&ls.stdout)
        .into_iter()
        .map(|(_, oid, rel)| (rel, oid))
        .collect();
    std::fs::write(repo.join("other.txt"), "stale CRLF form\r\n").unwrap();

    assert_eq!(
        clones_that_are_not_their_blob(&repo, &cloned).unwrap(),
        vec!["other.txt".to_string()]
    );
    assert!(clones_that_are_not_their_blob(&repo, &[])
        .unwrap()
        .is_empty());
}
