//! W6-L1 adversarial review of the pre-edit collision guard (ft-9cfd5574).
//!
//! For a headless agent an Ask is a deny and a silent answer is a green light,
//! so every test here asks one question: when another worktree really holds
//! the file, or the scan could not tell, is the answer ever SILENT?
//!
//! Included from `hooks::tests`, so that module's farm fixtures are in scope.
//! A test named `w6_BREAK_...` is expected to FAIL on the head under review: it
//! pins a hole.

use super::*;
// Its methods are called only by the macOS tests below.
#[cfg(target_os = "macos")]
use crate::procguard::LockedSpawn;
use crate::test_support::git_in;
use std::path::Path;

fn sib(farm: &Farm, n: usize) -> std::path::PathBuf {
    farm.sibling_dirs[n].clone()
}

fn target(farm: &Farm, rel: &str) -> HookInput {
    edit_call(&farm.repo, &format!("{}/{rel}", farm.repo))
}

fn say(output: &HookOutput) -> String {
    format!("{:?}", output.render())
}

fn not_silent(what: &str, output: &HookOutput) {
    assert!(
        !output.is_silent(),
        "SILENT-CLEAN: {what}: another worktree holds the file but the guard said nothing"
    );
}

/// Glob metacharacters in the edited path must neither widen the pathspec
/// (a false Ask on someone else's file) nor narrow it (a miss).
#[cfg(unix)]
#[test]
fn w6_glob_metacharacters_are_matched_literally() {
    let farm = farm_bound(false, false, 1);
    let held = sib(&farm, 0);
    // The sibling holds `axb.txt`; the edit is of `a*b.txt`, another file.
    std::fs::write(held.join("axb.txt"), "theirs").unwrap();
    let quiet = guard_here(&target(&farm, "a*b.txt"));
    assert!(
        quiet.is_silent(),
        "a glob widened the pathspec: {}",
        say(&quiet)
    );
    // The sibling holds the literally named files.
    for name in ["a*b.txt", "q?.txt", "[ab].txt"] {
        std::fs::write(held.join(name), "theirs").unwrap();
        let output = guard_here(&target(&farm, name));
        assert!(asks(&output), "{name}: {}", say(&output));
    }
}

/// A name that begins with `-` or `:` is a path, not an option or a magic
/// pathspec.
#[cfg(unix)]
#[test]
fn w6_leading_dash_and_colon_names_are_paths() {
    let farm = farm_bound(false, false, 1);
    let held = sib(&farm, 0);
    for name in [
        "-rf.txt",
        "--help",
        ":colon.txt",
        ":(top)x.txt",
        ":(glob)*.txt",
        ":!seed.txt",
    ] {
        std::fs::write(held.join(name), "theirs").unwrap();
        let output = guard_here(&target(&farm, name));
        assert!(asks(&output), "{name}: {}", say(&output));
    }
}

/// An edit made through a symlinked directory is the same file.
#[cfg(unix)]
#[test]
fn w6_a_path_through_a_symlinked_directory_is_the_same_file() {
    let farm = farm_bound(false, false, 1);
    std::fs::write(sib(&farm, 0).join("seed.txt"), "theirs").unwrap();
    std::os::unix::fs::symlink(".", Path::new(&farm.repo).join("lnk")).unwrap();
    let output = guard_here(&target(&farm, "lnk/seed.txt"));
    assert!(asks(&output), "{}", say(&output));
}

/// A path outside the repository, or with `..` that does not resolve, is a
/// non-check that says so.
#[cfg(unix)]
#[test]
fn w6_paths_outside_or_with_dotdot_are_never_silent() {
    let farm = farm_bound(false, false, 1);
    std::fs::write(sib(&farm, 0).join("seed.txt"), "theirs").unwrap();
    for file in [
        format!("{}/../outside.txt", farm.repo),
        format!("{}/nonexistent/../seed.txt", farm.repo),
        "/etc/hosts".to_string(),
        format!("{}/./seed.txt/../seed.txt", farm.repo),
    ] {
        let output = guard_here(&edit_call(&farm.repo, &file));
        // seed.txt resolved through `..` is the contended file and asks; the
        // rest are said not to have been checked. Silence is never right.
        not_silent(&file, &output);
    }
}

/// A file only staged in the other worktree, or staged and then reverted in
/// its working tree (the index alone carries it), or deleted, or renamed away
/// (both names), or intent-to-add.
#[cfg(unix)]
#[test]
fn w6_staged_deleted_and_renamed_holdings_are_found() {
    let farm = farm_bound(false, false, 1);
    let held = sib(&farm, 0);
    let git = |args: &[&str]| git_in(&held, args);

    // Staged, then the working copy restored: status `M ` only.
    std::fs::write(held.join("seed.txt"), "staged").unwrap();
    git(&["add", "seed.txt"]);
    git(&["checkout", "--", "seed.txt"]);
    // `checkout -- file` restores from the index; put HEAD content back.
    std::fs::write(
        held.join("seed.txt"),
        std::fs::read(Path::new(&farm.repo).join("seed.txt")).unwrap(),
    )
    .unwrap();
    let output = guard_here(&target(&farm, "seed.txt"));
    assert!(asks(&output), "index-only change: {}", say(&output));
    git(&["reset", "-q", "--hard"]);
    assert!(
        guard_here(&target(&farm, "seed.txt")).is_silent(),
        "reset made it clean"
    );

    // Deleted.
    std::fs::remove_file(held.join("seed.txt")).unwrap();
    let output = guard_here(&target(&farm, "seed.txt"));
    assert!(asks(&output), "deleted: {}", say(&output));
    git(&["reset", "-q", "--hard"]);

    // Renamed away and staged: the old name and the new one.
    git(&["mv", "seed.txt", "moved.txt"]);
    for name in ["seed.txt", "moved.txt"] {
        let output = guard_here(&target(&farm, name));
        assert!(asks(&output), "renamed, {name}: {}", say(&output));
    }
    git(&["reset", "-q", "--hard"]);
    let _ = std::fs::remove_file(held.join("moved.txt"));

    // Intent to add.
    std::fs::write(held.join("fresh.txt"), "x").unwrap();
    git(&["add", "-N", "fresh.txt"]);
    let output = guard_here(&target(&farm, "fresh.txt"));
    assert!(asks(&output), "intent-to-add: {}", say(&output));
}

/// Case-insensitive APFS: a NEW file named `Fresh.txt` here and an untracked
/// `fresh.txt` in the other worktree are one name on this volume.
#[cfg(target_os = "macos")]
#[test]
#[allow(non_snake_case)]
fn w6_BREAK_case_variant_of_a_held_untracked_file_is_not_silent() {
    let farm = farm_bound(false, false, 1);
    let probe = Path::new(&farm.repo).join("CaseProbe");
    std::fs::write(&probe, "x").unwrap();
    if !Path::new(&farm.repo).join("caseprobe").exists() {
        eprintln!("case-sensitive volume: nothing to attack");
        return;
    }
    std::fs::remove_file(&probe).unwrap();
    std::fs::write(sib(&farm, 0).join("fresh.txt"), "theirs").unwrap();
    let output = guard_here(&target(&farm, "Fresh.txt"));
    not_silent("Fresh.txt vs the held untracked fresh.txt", &output);
}

/// A tracked file edited in the other worktree under a different case spelling
/// is canonicalised by the filesystem, so it is found.
#[cfg(target_os = "macos")]
#[test]
fn w6_case_variant_of_a_tracked_file_is_found() {
    let farm = farm_bound(false, false, 1);
    if !Path::new(&farm.repo).join("SEED.TXT").exists() {
        return;
    }
    std::fs::write(sib(&farm, 0).join("seed.txt"), "theirs").unwrap();
    let output = guard_here(&target(&farm, "SEED.TXT"));
    assert!(asks(&output), "{}", say(&output));
}

/// A path that is a FILE here and a DIRECTORY in the other worktree: the
/// pathspec returns the files under it, and the exact-path match drops them.
#[cfg(unix)]
#[test]
#[allow(non_snake_case)]
fn w6_BREAK_a_file_here_that_is_a_directory_there_is_not_silent() {
    let farm = farm_bound(false, false, 1);
    let held = sib(&farm, 0);
    std::fs::remove_file(held.join("seed.txt")).unwrap();
    std::fs::create_dir_all(held.join("seed.txt")).unwrap();
    std::fs::write(held.join("seed.txt/inner.txt"), "theirs").unwrap();
    let output = guard_here(&target(&farm, "seed.txt"));
    not_silent(
        "seed.txt is a directory with new files in the other worktree",
        &output,
    );
}

/// A file inside a submodule (a nested repository) that the other worktree
/// has modified.
#[cfg(unix)]
#[test]
#[allow(non_snake_case)]
fn w6_BREAK_a_file_inside_a_nested_repository_another_worktree_modified_is_not_silent() {
    let farm = farm_bound(false, false, 1);
    let held = sib(&farm, 0);
    let nested = held.join("libsub");
    std::fs::create_dir_all(&nested).unwrap();
    git_in(&nested, &["init", "-q", "-b", "main"]);
    std::fs::write(nested.join("s.txt"), "one").unwrap();
    git_in(&nested, &["add", "."]);
    git_in(&nested, &["commit", "-qm", "s"]);
    let sha = crate::engine::git_cli::git_text(&nested, &["rev-parse", "HEAD"]).unwrap();
    git_in(
        &held,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},libsub", sha.trim()),
        ],
    );
    git_in(&held, &["commit", "-qm", "gitlink"]);
    std::fs::write(nested.join("s.txt"), "two").unwrap();
    let output = guard_here(&target(&farm, "libsub/s.txt"));
    not_silent(
        "libsub/s.txt modified inside a submodule of the other worktree",
        &output,
    );
}

/// A sibling whose directory vanished (prunable) or whose directory cannot be
/// read must make the answer partial, never clean.
#[cfg(unix)]
#[test]
fn w6_an_unreadable_or_vanished_worktree_is_a_notice_not_silence() {
    use std::os::unix::fs::PermissionsExt;
    let farm = farm_bound(false, false, 2);
    // sibling-0 vanishes.
    std::fs::remove_dir_all(sib(&farm, 0)).unwrap();
    let output = guard_here(&target(&farm, "README.md"));
    not_silent("a worktree whose directory vanished", &output);
    assert!(say(&output).contains("INCOMPLETE"), "{}", say(&output));

    // sibling-1 is unreadable.
    let dir = sib(&farm, 1);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let output = guard_here(&target(&farm, "README.md"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    not_silent("a worktree whose directory is unreadable", &output);
}

/// A locked worktree is still read.
#[cfg(unix)]
#[test]
fn w6_a_locked_worktree_is_still_scanned() {
    let farm = farm_bound(false, false, 1);
    std::fs::write(sib(&farm, 0).join("seed.txt"), "theirs").unwrap();
    git_in(
        Path::new(&farm.repo),
        &["worktree", "lock", &sib(&farm, 0).to_string_lossy()],
    );
    let output = guard_here(&target(&farm, "seed.txt"));
    assert!(asks(&output), "{}", say(&output));
}

/// A deadline that has already passed is a partial answer, and the hook's
/// decision over it is a notice.
#[cfg(unix)]
#[test]
fn w6_an_expired_deadline_is_partial_never_clean() {
    let farm = farm_bound(false, false, 3);
    std::fs::write(sib(&farm, 1).join("seed.txt"), "theirs").unwrap();
    let deadline = insights::ScanDeadline::after(std::time::Instant::now(), Duration::ZERO);
    let risk = insights::collision_for_path(&farm.repo, "seed.txt", Some(&deadline));
    assert!(!risk.ok, "{risk:?}");
    assert!(risk.truncated, "{risk:?}");
    assert!(
        risk.unscanned_worktrees > 0 || risk.failed_worktrees > 0,
        "{risk:?}"
    );
    let facts = collision_facts(&risk, Path::new(&farm.repo), "seed.txt", &|_| String::new());
    let output = collision_decision(&facts);
    not_silent("an expired deadline", &output);
}

/// Another worktree's status that FAILS (a corrupt `.git` file) next to one
/// that works: the failure is counted and the decision is not silent.
#[cfg(unix)]
#[test]
fn w6_a_worktree_with_a_corrupt_gitdir_is_partial_never_clean() {
    let farm = farm_bound(false, false, 2);
    std::fs::write(sib(&farm, 0).join(".git"), "gitdir: /nonexistent/nowhere\n").unwrap();
    let output = guard_here(&target(&farm, "README.md"));
    not_silent("a worktree whose .git file points nowhere", &output);
}

/// 18 siblings, a git that takes 2 s per status: the hook still answers inside
/// its budget, and the answer says it is partial rather than clean.
#[cfg(unix)]
#[test]
fn w6_a_slow_git_over_eighteen_worktrees_answers_inside_the_budget_and_partial() {
    let farm = farm_bound(false, false, 18);
    std::fs::write(sib(&farm, 17).join("seed.txt"), "theirs").unwrap();
    let dirs = farm.dirs.clone();
    let started = std::time::Instant::now();
    let output = crate::engine::worktree::status_probes::with_probe(
        &dirs,
        |_| std::thread::sleep(Duration::from_millis(4_000)),
        || guard_on_budget(&target(&farm, "seed.txt")),
    );
    let took = started.elapsed();
    eprintln!("slow git x18: {took:?} -> {}", say(&output));
    assert!(
        took < BUDGET + Duration::from_millis(300),
        "the hook took {took:?}; its budget is {BUDGET:?}"
    );
    not_silent("a slow git over 18 worktrees", &output);
}

/// 18 siblings and git is fast, but only the LAST one holds the file: the scan
/// cap leaves it unread, and that must not be a silent pass.
#[cfg(unix)]
#[test]
fn w6_a_holder_past_the_scan_cap_is_not_silent() {
    let farm = farm_bound(false, false, 18);
    for holder in 0..18 {
        std::fs::write(sib(&farm, holder).join("seed.txt"), "theirs").unwrap();
        let o = guard_here(&target(&farm, "seed.txt"));
        not_silent(&format!("holder {holder}"), &o);
        std::fs::remove_file(sib(&farm, holder).join("seed.txt")).ok();
        git_in(&sib(&farm, holder), &["checkout", "--", "seed.txt"]);
    }
    let marks = spawn_marks(&farm.dirs);
    let started = std::time::Instant::now();
    let output = guard_here(&target(&farm, "seed.txt"));
    let cost = spawn_cost(&farm.dirs, &marks);
    eprintln!(
        "18 siblings, uncontended-until-last: {:?}, {cost:?} -> {}",
        started.elapsed(),
        say(&output)
    );
    not_silent("the only holder is the 18th worktree", &output);
}

/// A scan slower than the hook's budget is a machine that is slow, not a file
/// nobody holds: the guard the finding tests use is given time to finish, so a
/// holder behind a status read longer than [`BUDGET`] is still asked about.
#[cfg(unix)]
#[test]
fn w6_a_status_slower_than_the_hook_budget_still_finds_the_holder() {
    let farm = farm_bound(false, false, 1);
    std::fs::write(sib(&farm, 0).join("seed.txt"), "theirs").unwrap();
    let dirs = farm.dirs.clone();
    let output = crate::engine::worktree::status_probes::with_probe(
        &dirs,
        |_| std::thread::sleep(BUDGET + Duration::from_millis(500)),
        || guard_here(&target(&farm, "seed.txt")),
    );
    assert!(asks(&output), "{}", say(&output));
}

/// The contended cost on an 18-worktree farm, measured.
#[cfg(unix)]
#[test]
fn w6_contended_cost_on_an_eighteen_worktree_farm() {
    let farm = farm_bound(true, false, 18);
    let marks = spawn_marks(&farm.dirs);
    let started = std::time::Instant::now();
    let mut call = target(&farm, "seed.txt");
    call.incoming = Some(insights::IncomingEdit::Replacing(vec!["seed".to_string()]));
    let output = guard_here(&call);
    let cost = spawn_cost(&farm.dirs, &marks);
    eprintln!(
        "contended x18 with incoming edit: {:?}, {cost:?}, asks={}",
        started.elapsed(),
        asks(&output)
    );
    assert!(asks(&output), "{}", say(&output));
}

/// S3-08 / path-scoped status: when a sibling holds an untracked directory
/// `fresh/inner.txt` and this worktree edits a file named `fresh`, or a
/// sibling holds an untracked file/symlink `fresh` and this worktree edits
/// `fresh/inner.txt`, git pathspec-limited status emits the sibling's actual
/// path rather than the queried spelling. Both directions must never be silent.
#[cfg(unix)]
#[test]
fn w6_untracked_file_vs_directory_in_both_directions_is_never_silent() {
    // Direction 1: sibling has untracked `fresh/inner.txt`; editor targets `fresh`.
    {
        let farm = farm_bound(false, false, 1);
        let dir = farm.sibling_dirs[0].join("fresh");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("inner.txt"), "theirs\n").unwrap();

        let output = guard_here(&target(&farm, "fresh"));
        assert!(asks(&output), "dir-vs-file should ask: {}", say(&output));
        assert!(
            reason_of(&output).contains("sibling-0"),
            "missing holder name: {}",
            reason_of(&output)
        );
    }

    // Direction 2: sibling has untracked file `fresh`; editor targets `fresh/inner.txt`.
    {
        let farm = farm_bound(false, false, 1);
        std::fs::write(farm.sibling_dirs[0].join("fresh"), "theirs\n").unwrap();

        let output = guard_here(&target(&farm, "fresh/inner.txt"));
        assert!(asks(&output), "file-vs-dir should ask: {}", say(&output));
        assert!(
            reason_of(&output).contains("sibling-0"),
            "missing holder name: {}",
            reason_of(&output)
        );
    }
}

/// App-served `CollisionMemo::assemble` reads whole-worktree cached paths and
/// must agree with `changed_paths_only` on case-variant filenames,
/// file-vs-directory collisions in both directions, and dirty submodules.
#[cfg(target_os = "macos")]
#[test]
fn w6_app_served_scan_agrees_on_case_directory_and_submodule_holdings() {
    // 1. Case variant on APFS: sibling dirties `fresh.txt`, editor asks for `Fresh.txt`.
    {
        let farm = farm_bound(false, false, 1);
        let _app = app_for(&farm.repo, true);
        std::fs::write(farm.sibling_dirs[0].join("fresh.txt"), "theirs\n").unwrap();
        let output = guard_here(&target(&farm, "Fresh.txt"));
        assert!(
            asks(&output),
            "case variant via app should ask: {}",
            say(&output)
        );
    }

    // 2. Untracked directory vs file: sibling dirties `fresh/inner.txt`, editor asks for `fresh`.
    {
        let farm = farm_bound(false, false, 1);
        let _app = app_for(&farm.repo, true);
        let dir = farm.sibling_dirs[0].join("fresh");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("inner.txt"), "theirs\n").unwrap();
        let output = guard_here(&target(&farm, "fresh"));
        assert!(
            asks(&output),
            "dir-vs-file via app should ask: {}",
            say(&output)
        );
    }

    // 3. Untracked file vs directory: sibling dirties `blocker`, editor asks for `blocker/inner.txt`.
    {
        let farm = farm_bound(false, false, 1);
        let _app = app_for(&farm.repo, true);
        std::fs::write(farm.sibling_dirs[0].join("blocker"), "theirs\n").unwrap();
        let output = guard_here(&target(&farm, "blocker/inner.txt"));
        assert!(
            asks(&output),
            "file-vs-dir via app should ask: {}",
            say(&output)
        );
    }

    // 4. Dirty submodule: sibling has a tracked submodule `libsub` with dirty `s.txt`.
    {
        let farm = farm_bound(false, false, 1);
        let _app = app_for(&farm.repo, true);
        let sub_src = farm.sibling_dirs[0].parent().unwrap().join("sub_src");
        std::fs::create_dir_all(&sub_src).unwrap();
        let run = |cwd: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output_locked()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} in {:?} failed: {}",
                args,
                cwd,
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&sub_src, &["init", "-q"]);
        run(&sub_src, &["config", "user.email", "test@example.com"]);
        run(&sub_src, &["config", "user.name", "Test"]);
        std::fs::write(sub_src.join("s.txt"), "clean\n").unwrap();
        run(&sub_src, &["add", "s.txt"]);
        run(&sub_src, &["commit", "-q", "-m", "sub init"]);

        let sib = &farm.sibling_dirs[0];
        let sub_dst = sib.join("libsub");
        run(
            sib,
            &[
                "-c",
                "protocol.file.allow=always",
                "clone",
                "-q",
                sub_src.to_str().unwrap(),
                sub_dst.to_str().unwrap(),
            ],
        );
        run(sib, &["add", "libsub"]);
        std::fs::write(sub_dst.join("s.txt"), "dirty\n").unwrap();

        let output = guard_here(&target(&farm, "libsub/s.txt"));
        assert!(
            asks(&output),
            "submodule via app should ask: {}",
            say(&output)
        );
    }
}
