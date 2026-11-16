use super::*;
use crate::sysload::procscan::UseKind;
use std::cell::Cell;
use std::fs;

const UID: u32 = 501;
const OLD: &str = "session-0001-aaaaaaaa";

fn now() -> u64 {
    crate::storage::hygiene::global::current_time()
}

fn temp_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
    (temp, root)
}

/// Sets every entry under `path` (and `path`) modified `days` days ago.
fn age(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    let set = |p: &Path| {
        fs::File::open(p)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(when))
            .unwrap();
    };
    let mut stack = vec![path.to_path_buf()];
    let mut all = Vec::new();
    while let Some(next) = stack.pop() {
        if fs::symlink_metadata(&next).unwrap().is_dir() {
            for entry in fs::read_dir(&next).unwrap() {
                stack.push(entry.unwrap().path());
            }
        }
        all.push(next);
    }
    for p in all {
        set(&p);
    }
}

/// A Cargo target directory with Cargo's own markers and some output.
fn cargo_target(dir: &Path, days_old: u64) {
    fs::create_dir_all(dir.join("debug")).unwrap();
    fs::write(dir.join("CACHEDIR.TAG"), CARGO_SIGNATURE).unwrap();
    fs::write(dir.join(".rustc_info.json"), "{}").unwrap();
    fs::write(dir.join("debug/app.o"), vec![b'x'; 4096]).unwrap();
    age(dir, days_old);
}

fn scratchpad(root: &Path, id: &str) -> PathBuf {
    let pad = root.join(format!("claude-{UID}/proj/{id}/scratchpad"));
    fs::create_dir_all(&pad).unwrap();
    pad
}

fn idle(_: &Path) -> Result<DirUse, String> {
    Ok(DirUse::default())
}

fn run<T>(
    live: &LiveSessions,
    use_of: &dyn Fn(&Path) -> Result<DirUse, String>,
    f: impl FnOnce(&Context<'_>) -> T,
) -> T {
    let cancel = AtomicBool::new(false);
    f(&Context {
        now: now(),
        retention_days: 7,
        live,
        use_of,
        cancel: &cancel,
        deadline: Instant::now() + Duration::from_secs(60),
        target_budget: CARGO_TARGET_BUDGET,
    })
}

fn nobody() -> LiveSessions {
    LiveSessions::default()
}

fn kept_reason<'a>(scan: &'a Scan, needle: &str) -> &'a KeptItem {
    scan.kept
        .iter()
        .find(|k| k.path.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not kept: {:?}", scan.kept))
}

#[test]
fn a_finished_sessions_cargo_target_is_offered_and_only_it_is_removed() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    cargo_target(&pad.join("cargo-target"), 30);
    fs::write(pad.join("notes.txt"), "keep me").unwrap();
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.kept);
    let candidate = &scan.candidates[0];
    assert_eq!(candidate.scope, SCOPE_SESSION);
    assert!(
        candidate.evidence.iter().any(|e| e.contains(OLD)),
        "the evidence names the session: {:?}",
        candidate.evidence
    );
    assert!(candidate
        .evidence
        .iter()
        .any(|e| e.contains("Cargo target")));
    let freed = run(&nobody(), &idle, |ctx| remove(candidate, ctx, u64::MAX)).unwrap();
    assert!(freed >= 4096);
    assert!(!pad.join("cargo-target").exists());
    assert!(pad.is_dir(), "the scratchpad itself stays");
    assert_eq!(
        fs::read_to_string(pad.join("notes.txt")).unwrap(),
        "keep me"
    );
}

#[test]
fn a_running_session_keeps_its_folder_and_a_lookalike_id_does_not() {
    let (_temp, root) = temp_root();
    let running = "session-0001-aaaaaaaa";
    let longer = "session-0001-aaaaaaaa2";
    for id in [running, longer] {
        cargo_target(&scratchpad(&root, id).join("target-x"), 30);
    }
    let live = LiveSessions {
        ids: BTreeSet::from([running.to_string()]),
        ..LiveSessions::default()
    };
    let scan = run(&live, &idle, |ctx| scan_sessions(&root, UID, ctx, false));
    assert!(kept_reason(&scan, running).reason.contains("is running"));
    let offered: Vec<&str> = scan.candidates.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(offered.len(), 1, "{offered:?} / {:?}", scan.kept);
    assert!(offered[0].contains(longer), "{offered:?}");
}

#[test]
fn a_session_that_cannot_be_shown_finished_keeps_every_folder() {
    let (_temp, root) = temp_root();
    cargo_target(&scratchpad(&root, OLD).join("target-x"), 30);
    let unidentified = LiveSessions {
        unidentified: 2,
        ..LiveSessions::default()
    };
    let scan = run(&unidentified, &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty());
    assert!(kept_reason(&scan, OLD)
        .reason
        .contains("no session id to match"));
    let blind = LiveSessions {
        incomplete: Some("the claude detector could not look: scan failed".into()),
        ..LiveSessions::default()
    };
    let scan = run(&blind, &idle, |ctx| scan_sessions(&root, UID, ctx, false));
    assert!(scan.candidates.is_empty());
    assert!(kept_reason(&scan, OLD).reason.contains("could not be told"));
}

#[test]
fn a_directory_merely_named_target_is_never_offered() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    fs::create_dir_all(pad.join("target/debug")).unwrap();
    fs::write(pad.join("target/debug/data.bin"), "work").unwrap();
    age(&pad.join("target"), 90);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty());
    assert!(kept_reason(&scan, "target")
        .reason
        .contains("named like a build folder but"));
    // A target with the markers of something else is no better.
    fs::write(pad.join("target/CACHEDIR.TAG"), "Signature: someone else").unwrap();
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty());
}

#[test]
fn a_process_using_the_folder_or_an_unreadable_one_keeps_it() {
    let (_temp, root) = temp_root();
    cargo_target(&scratchpad(&root, OLD).join("cargo-target"), 30);
    let using = |_: &Path| {
        Ok(DirUse {
            users: vec![(77, UseKind::Argument)],
            ..DirUse::default()
        })
    };
    let scan = run(&nobody(), &using, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty());
    assert!(kept_reason(&scan, "cargo-target")
        .reason
        .contains("process 77"));
    let unreadable = |_: &Path| {
        Ok(DirUse {
            unreadable: vec![78],
            ..DirUse::default()
        })
    };
    let scan = run(&nobody(), &unreadable, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty());
    assert!(kept_reason(&scan, "cargo-target")
        .reason
        .contains("could not be read"));
    let failed = |_: &Path| Err::<DirUse, String>("table unavailable".into());
    let scan = run(&nobody(), &failed, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty());
}

#[test]
fn a_folder_that_became_live_between_preview_and_delete_is_not_removed() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let target = pad.join("cargo-target");
    cargo_target(&target, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    let candidate = &scan.candidates[0];
    let calls = Cell::new(0);
    let turns_live = |_: &Path| {
        calls.set(calls.get() + 1);
        Ok(DirUse {
            users: vec![(9, UseKind::Executable)],
            ..DirUse::default()
        })
    };
    let refused = run(&nobody(), &turns_live, |ctx| {
        remove(candidate, ctx, u64::MAX)
    })
    .expect_err("a process now uses it");
    assert!(refused.contains("process 9"), "{refused}");
    assert!(target.join("debug/app.o").is_file(), "nothing was removed");
    let live = LiveSessions {
        ids: BTreeSet::from([OLD.to_string()]),
        ..LiveSessions::default()
    };
    let refused = run(&live, &idle, |ctx| remove(candidate, ctx, u64::MAX))
        .expect_err("its session came back");
    assert!(refused.contains("is running"), "{refused}");
    assert!(target.is_dir());
}

#[test]
fn a_symlink_never_leads_the_removal_outside_the_scope() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let outside = root.join("outside");
    cargo_target(&outside, 30);
    // Found through a symlink: not found at all.
    std::os::unix::fs::symlink(&outside, pad.join("linked")).unwrap();
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
    // A real candidate replaced by a symlink after the preview.
    let target = pad.join("cargo-target");
    cargo_target(&target, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    let candidate = scan
        .candidates
        .iter()
        .find(|c| c.path.ends_with("cargo-target"))
        .expect("candidate")
        .clone();
    fs::remove_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&outside, &target).unwrap();
    let refused = run(&nobody(), &idle, |ctx| remove(&candidate, ctx, u64::MAX))
        .expect_err("the path is a symlink now");
    assert!(refused.contains("Symlink"), "{refused}");
    assert!(
        outside.join("debug/app.o").is_file(),
        "the target outside survived"
    );
}

#[test]
fn a_candidate_outside_its_scope_is_refused_whatever_it_claims() {
    let (_temp, root) = temp_root();
    let elsewhere = root.join("elsewhere/cargo-target");
    cargo_target(&elsewhere, 30);
    let scope_root = root.join(format!("claude-{UID}"));
    fs::create_dir_all(&scope_root).unwrap();
    let forged = |path: &Path, scope: &str| CleanerCandidate {
        repo_path: scope_root.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        provider: String::new(),
        bytes: 1,
        scope: scope.into(),
        private_bytes: None,
        branch: None,
        evidence: vec![],
    };
    for candidate in [
        forged(&elsewhere, SCOPE_SESSION),
        forged(&scope_root, SCOPE_SESSION),
        forged(
            &scope_root.join("proj/x/scratchpad/../../.."),
            SCOPE_SESSION,
        ),
        forged(&elsewhere, "repository"),
    ] {
        assert!(
            run(&nobody(), &idle, |ctx| remove(&candidate, ctx, u64::MAX)).is_err(),
            "{}",
            candidate.path
        );
    }
    assert!(elsewhere.join("debug/app.o").is_file());
}

#[test]
fn a_young_unreadable_or_oversized_target_is_kept() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    cargo_target(&pad.join("young"), 1);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(kept_reason(&scan, "young")
        .reason
        .contains("Used or modified within"));

    cargo_target(&pad.join("sealed"), 30);
    let sealed = pad.join("sealed/debug");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o0)).unwrap();
        let scan = run(&nobody(), &idle, |ctx| {
            scan_sessions(&root, UID, ctx, false)
        });
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
        assert!(kept_reason(&scan, "sealed")
            .reason
            .contains("Permission denied"));
    }

    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    let candidate = scan
        .candidates
        .iter()
        .find(|c| c.path.ends_with("sealed"))
        .expect("readable again");
    let refused = run(&nobody(), &idle, |ctx| remove(candidate, ctx, 1)).expect_err("budget");
    assert!(refused.contains("byte budget"), "{refused}");
    assert!(sealed.is_dir());
}

/// Names between the scope root and a target are judged by the vendored
/// preserved-name list; names above it are not. A target under a preserved
/// directory inside the scratchpad is kept, one under an ordinary directory is
/// offered, and the list itself is asserted so a change to it fails here.
#[test]
fn a_preserved_name_between_the_scope_root_and_the_target_keeps_it() {
    assert!(devmap_query::hygiene::protected_artifact(
        "tmp/cargo-target"
    ));
    assert!(devmap_query::hygiene::protected_artifact(
        "node_modules/cargo-target"
    ));
    assert!(!devmap_query::hygiene::protected_artifact(
        "work/cargo-target"
    ));
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    cargo_target(&pad.join("tmp/cargo-target"), 30);
    cargo_target(&pad.join("work/cargo-target"), 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    let offered: Vec<&str> = scan.candidates.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(offered.len(), 1, "{offered:?} / {:?}", scan.kept);
    assert!(offered[0].ends_with("work/cargo-target"));
    assert!(
        kept_reason(&scan, "tmp/cargo-target")
            .reason
            .contains("preserved-name list"),
        "{:?}",
        scan.kept
    );
}

#[test]
fn a_scratch_target_in_a_repository_needs_git_to_ignore_it() {
    let (_temp, root) = temp_root();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    crate::engine::git_cli::git_global(&["init", "-q", repo.to_str().unwrap()]).unwrap();
    crate::test_support::trust_repo(&repo);
    fs::write(
        repo.join(".git/info/exclude"),
        ".gitpulse/scratch/target-a/\n",
    )
    .unwrap();
    cargo_target(&repo.join(".gitpulse/scratch/target-a"), 30);
    cargo_target(&repo.join(".gitpulse/scratch/target-b"), 30);
    let scan = run(&nobody(), &idle, |ctx| scan_repository(&repo, ctx, false));
    let offered: Vec<&str> = scan.candidates.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(offered.len(), 1, "{offered:?} / {:?}", scan.kept);
    assert!(offered[0].ends_with("target-a"));
    assert!(kept_reason(&scan, "target-b")
        .reason
        .contains("does not ignore"));
    let freed = run(&nobody(), &idle, |ctx| {
        remove(&scan.candidates[0], ctx, u64::MAX)
    })
    .unwrap();
    assert!(freed > 0);
    assert!(!repo.join(".gitpulse/scratch/target-a").exists());
    assert!(repo.join(".gitpulse/scratch/target-b").is_dir());
}

#[test]
fn leaked_temp_directories_are_reported_and_never_removed() {
    let (_temp, root) = temp_root();
    let big = root.join(".tmpLEAK01");
    fs::create_dir_all(&big).unwrap();
    let file = fs::File::create(big.join("blob")).unwrap();
    file.set_len(LEAK_FLOOR_BYTES + 1).unwrap();
    drop(file);
    let small = root.join(".tmpSMALL1");
    fs::create_dir_all(&small).unwrap();
    fs::write(small.join("note"), "x").unwrap();
    age(&big, 40);
    let mut issues = Vec::new();
    let leaks = run(&nobody(), &idle, |ctx| leaked_temp(&root, ctx, &mut issues));
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(leaks.len(), 1, "{leaks:?}");
    assert!(leaks[0].path.ends_with(".tmpLEAK01"));
    assert!(leaks[0].age_days >= 39);
    assert!(leaks[0].reason.contains("report only"));
    assert!(leaks[0].reason.contains("no owner can be proven"));
    assert!(
        big.join("blob").is_file() && small.is_dir(),
        "nothing is removed"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let mut full = vec![
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
    ];
    full.extend_from_slice(args);
    crate::engine::git_cli::git_text(dir, &full).unwrap();
}

/// A repository with a linked worktree, each holding `.claude/x-target` (a
/// Cargo target made 30 days ago; the worktree's is a copy made now with the
/// same old times), plus the config a session worktree is seeded with.
fn claude_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (temp, root) = temp_root();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    crate::engine::git_cli::git_global(&["init", "-q", "-b", "main", repo.to_str().unwrap()])
        .unwrap();
    crate::test_support::trust_repo(&repo);
    fs::write(repo.join("a.txt"), "a").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-qm", "base"]);
    let lane = root.join("lane");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane",
            lane.to_str().unwrap(),
        ],
    );
    crate::test_support::trust_repo(&lane);
    for checkout in [&repo, &lane] {
        let claude = checkout.join(".claude");
        fs::create_dir_all(claude.join("skills")).unwrap();
        fs::write(claude.join("skills/a.md"), "skill").unwrap();
        fs::write(claude.join("settings.local.json"), "{}").unwrap();
        cargo_target(&claude.join("x-target"), 30);
    }
    fs::create_dir_all(repo.join(".claude/worktrees/other")).unwrap();
    (temp, repo, lane)
}

#[test]
fn a_claude_build_folder_is_judged_in_the_checkout_and_in_a_worktrees_copy() {
    let (_temp, repo, lane) = claude_fixture();
    let main_target = repo.join(".claude/x-target");
    let in_main = |p: &Path| {
        if p.starts_with(&main_target) {
            Ok(DirUse {
                users: vec![(31, UseKind::Argument)],
                ..DirUse::default()
            })
        } else {
            Ok(DirUse::default())
        }
    };
    let scan = run(&nobody(), &in_main, |ctx| {
        scan_claude_targets(&[repo.clone(), lane.clone()], ctx, false)
    });
    let offered: Vec<&str> = scan.candidates.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(offered.len(), 1, "{offered:?} / {:?}", scan.kept);
    assert!(offered[0].ends_with("lane/.claude/x-target"), "{offered:?}");
    let candidate = &scan.candidates[0];
    assert_eq!(candidate.scope, SCOPE_CLAUDE);
    assert!(
        candidate
            .evidence
            .iter()
            .any(|e| e.contains("seeded copy that was never built in")),
        "{:?}",
        candidate.evidence
    );
    let in_use = kept_reason(&scan, "repo/.claude/x-target");
    assert!(in_use.reason.starts_with("in use:"), "{}", in_use.reason);
    // Removal takes the copy and nothing else under .claude, in either checkout.
    run(&nobody(), &idle, |ctx| remove(candidate, ctx, u64::MAX)).unwrap();
    assert!(!lane.join(".claude/x-target").exists());
    for kept in [
        lane.join(".claude/skills/a.md"),
        lane.join(".claude/settings.local.json"),
        repo.join(".claude/skills/a.md"),
        repo.join(".claude/settings.local.json"),
        repo.join(".claude/x-target/debug/app.o"),
    ] {
        assert!(kept.is_file(), "{}", kept.display());
    }
    assert!(repo.join(".claude/worktrees/other").is_dir());
}

#[test]
fn only_a_marked_target_directly_under_claude_is_ever_a_candidate() {
    let (_temp, repo, _lane) = claude_fixture();
    let claude = repo.join(".claude");
    // Named like a build folder, no markers; markers in a folder with another
    // name; a marked target one level too deep; config and skills.
    fs::create_dir_all(claude.join("y-target/debug")).unwrap();
    fs::write(claude.join("y-target/debug/work.bin"), "w").unwrap();
    age(&claude.join("y-target"), 30);
    cargo_target(&claude.join("notes"), 30);
    cargo_target(&claude.join("skills/deep-target"), 30);
    fs::remove_dir_all(claude.join("x-target")).unwrap();
    let scan = run(&nobody(), &idle, |ctx| {
        scan_claude_targets(std::slice::from_ref(&repo), ctx, false)
    });
    assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
    assert!(kept_reason(&scan, "y-target")
        .reason
        .contains("named like a build folder but"));
    // A forged candidate that points at anything else under .claude is refused.
    let forged = |path: PathBuf| CleanerCandidate {
        repo_path: repo.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        provider: String::new(),
        bytes: 1,
        scope: SCOPE_CLAUDE.into(),
        private_bytes: None,
        branch: None,
        evidence: vec![],
    };
    for path in [
        claude.join("notes"),
        claude.join("skills/deep-target"),
        claude.join("skills"),
        claude.join("settings.local.json"),
        claude.clone(),
        claude.join("worktrees"),
    ] {
        let shown = path.display().to_string();
        assert!(
            run(&nobody(), &idle, |ctx| remove(&forged(path), ctx, u64::MAX)).is_err(),
            "{shown}"
        );
    }
    assert!(claude.join("notes/debug/app.o").is_file());
    assert!(claude.join("skills/a.md").is_file());
}

#[test]
fn a_recent_or_unproven_claude_build_folder_says_which() {
    let (_temp, repo, _lane) = claude_fixture();
    cargo_target(&repo.join(".claude/fresh-target"), 1);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_claude_targets(std::slice::from_ref(&repo), ctx, false)
    });
    assert!(kept_reason(&scan, "fresh-target")
        .reason
        .starts_with("too recent:"));
    let unreadable = |_: &Path| {
        Ok(DirUse {
            unreadable: vec![5],
            ..DirUse::default()
        })
    };
    let scan = run(&nobody(), &unreadable, |ctx| {
        scan_claude_targets(std::slice::from_ref(&repo), ctx, false)
    });
    assert!(scan.candidates.is_empty());
    assert!(kept_reason(&scan, "x-target")
        .reason
        .starts_with("owner unproven:"));
}

/// A real Cargo target has a `tmp` directory and crate-named build-script
/// directories (`build/log/<hash>`). The walk passes the candidate root to the
/// name protection, which waives exactly those under a directory with both
/// Cargo markers, so such a target is offered and removed, in a session
/// scratchpad and under `.claude/` alike.
#[test]
fn a_real_shaped_cargo_target_with_tmp_is_offered_and_removed() {
    let (_temp, root) = temp_root();
    let target = scratchpad(&root, OLD).join("cargo-target");
    cargo_target(&target, 30);
    fs::create_dir_all(target.join("tmp")).unwrap();
    fs::write(target.join("tmp/x"), "x").unwrap();
    fs::create_dir_all(target.join("debug/build/log/0123abcd")).unwrap();
    fs::write(target.join("debug/build/log/0123abcd/out"), "x").unwrap();
    age(&target, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.kept);
    run(&nobody(), &idle, |ctx| {
        remove(&scan.candidates[0], ctx, u64::MAX)
    })
    .unwrap();
    assert!(!target.exists());
}

#[test]
fn a_claude_target_with_tmp_is_removed_and_a_repository_inside_it_is_not() {
    let (_temp, repo, lane) = claude_fixture();
    let copy = lane.join(".claude/x-target");
    fs::create_dir_all(copy.join("tmp")).unwrap();
    fs::write(copy.join("tmp/x"), "x").unwrap();
    fs::create_dir_all(copy.join("debug/build/log/0123abcd")).unwrap();
    fs::write(copy.join("debug/build/log/0123abcd/out"), "x").unwrap();
    age(&copy, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_claude_targets(std::slice::from_ref(&lane), ctx, false)
    });
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.kept);
    run(&nobody(), &idle, |ctx| {
        remove(&scan.candidates[0], ctx, u64::MAX)
    })
    .unwrap();
    assert!(!copy.exists());
    assert!(lane.join(".claude/skills/a.md").is_file());
    // A repository, a .env or node_modules inside the main one keep it whole.
    let main = repo.join(".claude/x-target");
    fs::create_dir_all(main.join("tmp/repo/.git")).unwrap();
    fs::write(main.join("tmp/repo/.git/config"), "x").unwrap();
    age(&main, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_claude_targets(std::slice::from_ref(&repo), ctx, false)
    });
    assert!(scan.candidates.is_empty());
    assert!(kept_reason(&scan, "x-target")
        .reason
        .contains("Protected content inside candidate"));
    assert!(main.join("tmp/repo/.git/config").is_file());
}

fn observed(
    kind: &str,
    ok: bool,
    live: Vec<Option<&str>>,
) -> crate::insights::live_sessions::KindObservation {
    crate::insights::live_sessions::KindObservation {
        kind: kind.into(),
        ok,
        error: if ok {
            String::new()
        } else {
            "scan failed".into()
        },
        live: live
            .into_iter()
            .map(|id| crate::insights::LiveSession {
                kind: kind.into(),
                pid: 1,
                entrypoint: String::new(),
                status: String::new(),
                cwd: String::new(),
                tree: None,
                run_id: None,
                parent_pid: None,
                session_id: id.map(str::to_string),
                proc_start: None,
                registry_cwd: None,
                start_us: None,
                is_caller: false,
                ide_hosted: false,
                terminal_session_id: None,
            })
            .collect(),
        ..Default::default()
    }
}

/// Only a Claude session can hold a Claude scratchpad, so only the Claude
/// detector decides. A Codex or Manvi session with no id, or a detector that
/// could not look, keeps nothing; an unidentified Claude session, a Claude
/// detector that failed, and no Claude entry at all each keep everything.
#[test]
fn only_the_claude_detector_decides_whether_a_scratchpad_is_finished() {
    let others_blind = vec![
        observed("claude", true, vec![Some("session-0002-bbbbbbbb")]),
        observed("codex", false, vec![]),
        observed("manvi", true, vec![None, None]),
        observed("antigravity", true, vec![None]),
    ];
    let live = LiveSessions::from_observation(&others_blind);
    assert_eq!(live.unidentified, 0, "{live:?}");
    assert!(live.incomplete.is_none(), "{live:?}");
    assert!(live.ids.contains("session-0002-bbbbbbbb"));
    let (_temp, root) = temp_root();
    cargo_target(&scratchpad(&root, OLD).join("target-x"), 30);
    let scan = run(&live, &idle, |ctx| scan_sessions(&root, UID, ctx, false));
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.kept);

    let unidentified = LiveSessions::from_observation(&vec![observed("claude", true, vec![None])]);
    assert_eq!(unidentified.unidentified, 1);
    let scan = run(&unidentified, &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(kept_reason(&scan, OLD).reason.contains("Claude session(s)"));

    let blind = LiveSessions::from_observation(&vec![observed("claude", false, vec![])]);
    assert!(blind.incomplete.is_some());
    let missing = LiveSessions::from_observation(&vec![observed("codex", true, vec![])]);
    assert!(
        missing.incomplete.is_some(),
        "no Claude entry is a detector that did not run"
    );
}

/// A check git could not answer is not "git does not ignore it". The same
/// repository, untrusted, is kept as one that could not be checked, counts as
/// unexamined and leaves the scan incomplete.
#[test]
fn a_git_question_that_could_not_run_is_unexamined_not_an_answer() {
    let (_temp, root) = temp_root();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    crate::engine::git_cli::git_global(&["init", "-q", repo.to_str().unwrap()]).unwrap();
    // Not trusted: git refuses to run in it.
    fs::write(
        repo.join(".git/info/exclude"),
        ".gitpulse/scratch/target-a/\n",
    )
    .unwrap();
    cargo_target(&repo.join(".gitpulse/scratch/target-a"), 30);
    let scan = run(&nobody(), &idle, |ctx| scan_repository(&repo, ctx, false));
    assert!(scan.candidates.is_empty());
    let kept = kept_reason(&scan, "target-a");
    assert!(kept.reason.starts_with(COULD_NOT_CHECK), "{}", kept.reason);
    assert!(!kept.reason.contains("does not ignore"), "{}", kept.reason);
    assert_eq!(scan.unexamined, 1);
    assert!(!scan.complete);
    let mut inventory = empty_inventory();
    scan.absorb_into(&mut inventory);
    assert_eq!(inventory.shed_repos, 1, "the run counts it as unexamined");
    assert!(!inventory.scopes_complete);
    assert!(inventory
        .issues
        .iter()
        .any(|i| i.contains("could not be checked")));
    // The same folder with git answering "not ignored" is an answer.
    crate::test_support::trust_repo(&repo);
    fs::write(repo.join(".git/info/exclude"), "").unwrap();
    let scan = run(&nobody(), &idle, |ctx| scan_repository(&repo, ctx, false));
    assert!(kept_reason(&scan, "target-a")
        .reason
        .contains("does not ignore"));
    assert_eq!(scan.unexamined, 0);
    assert!(scan.complete);
}

/// The cleaner's own git checks (`check-ignore`, `ls-files`, `worktree list`)
/// run on the calling thread, so the thread-local disk relief reaches them:
/// under a full disk they are shed without it and answered with it. This is
/// the proof of which checks the relief covers; `scan_storage`'s own reads are
/// not asked here and keep whatever class they run in.
#[test]
fn the_cleaners_git_checks_are_answered_under_a_full_disk_only_with_relief() {
    use crate::sysload::{GovernorInput, Level, Reading, Readings};
    let (_temp, root) = temp_root();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    crate::engine::git_cli::git_global(&["init", "-q", repo.to_str().unwrap()]).unwrap();
    crate::test_support::trust_repo(&repo);
    fs::write(repo.join(".git/info/exclude"), "tmp/\n").unwrap();
    let _disk = crate::sysload::inject_governor(GovernorInput {
        readings: Readings {
            free_disk: Reading::Value(1 << 30),
            ..Readings::calm()
        },
        ..GovernorInput::test_pressure(Level::Normal)
    });
    assert_eq!(
        crate::sysload::shed_cause(),
        Some(crate::sysload::Binding::LowDisk)
    );
    let asks = || {
        (
            git_ignores(&repo, "tmp/a"),
            git_tracks(&repo, "tmp/a"),
            crate::engine::worktree::list_worktrees_lite(repo.to_str().unwrap()).is_ok(),
        )
    };
    let (ignored, tracked, listed) = asks();
    assert!(matches!(ignored, GitAnswer::Unknown(_)), "{ignored:?}");
    assert!(matches!(tracked, GitAnswer::Unknown(_)), "{tracked:?}");
    assert!(!listed, "shed without relief");
    let (ignored, tracked, listed) = crate::engine::process::with_disk_relief(asks);
    assert_eq!(ignored, GitAnswer::Yes);
    assert_eq!(tracked, GitAnswer::No);
    assert!(
        listed,
        "the worktree listing runs on the calling thread too"
    );
    // Under another cause relief admits nothing.
    drop(_disk);
    let _swap = crate::sysload::inject_governor(GovernorInput {
        readings: Readings {
            swap_used: Reading::Value(60 << 30),
            ..Readings::calm()
        },
        ..GovernorInput::test_pressure(Level::Critical)
    });
    let (ignored, _, _) = crate::engine::process::with_disk_relief(asks);
    assert!(matches!(ignored, GitAnswer::Unknown(_)), "{ignored:?}");
}

fn empty_inventory() -> super::super::global::CleanerInventory {
    super::super::global::CleanerInventory {
        repositories: 0,
        visited_entries: 0,
        partial: false,
        issues: vec![],
        candidates: vec![],
        kept: vec![],
        leaked_temp: vec![],
        incomplete_repos: BTreeSet::new(),
        shed_repos: 0,
        scopes_complete: true,
    }
}

/// `<repo>/tmp` is a scratch root like any other: a real marked target under
/// `tmp/a/target` is offered (the root's own name is not consulted), a target
/// whose path between the root and itself names a repository is refused.
#[test]
fn a_marked_target_under_a_repositorys_tmp_is_offered_and_one_under_a_git_dir_is_not() {
    let (_temp, root) = temp_root();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    crate::engine::git_cli::git_global(&["init", "-q", repo.to_str().unwrap()]).unwrap();
    crate::test_support::trust_repo(&repo);
    fs::write(repo.join(".git/info/exclude"), "/tmp/\n").unwrap();
    cargo_target(&repo.join("tmp/a/target"), 30);
    let scan = run(&nobody(), &idle, |ctx| scan_repository(&repo, ctx, false));
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.kept);
    assert!(scan.candidates[0].path.ends_with("tmp/a/target"));
    let freed = run(&nobody(), &idle, |ctx| {
        remove(&scan.candidates[0], ctx, u64::MAX)
    })
    .unwrap();
    assert!(freed > 0);
    assert!(!repo.join("tmp/a/target").exists());
    // Forged: a target below a repository's own metadata.
    let hidden = repo.join("tmp/x/.git/y/target");
    cargo_target(&hidden, 30);
    let forged = CleanerCandidate {
        repo_path: repo.to_string_lossy().into_owned(),
        path: hidden.to_string_lossy().into_owned(),
        provider: String::new(),
        bytes: 1,
        scope: SCOPE_REPOSITORY.into(),
        private_bytes: None,
        branch: None,
        evidence: vec![],
    };
    let refused = run(&nobody(), &idle, |ctx| remove(&forged, ctx, u64::MAX))
        .expect_err("names between the scratch root and the target are consulted");
    assert!(refused.contains("preserved-name list"), "{refused}");
    assert!(hidden.join("debug/app.o").is_file());
}

/// A scan whose own deadline has passed says so; it is never a clean answer.
#[test]
fn a_scan_past_its_deadline_is_incomplete_and_says_so() {
    let (_temp, root) = temp_root();
    cargo_target(&scratchpad(&root, OLD).join("target-x"), 30);
    let cancel = AtomicBool::new(false);
    let live = nobody();
    let ctx = Context {
        now: now(),
        retention_days: 7,
        live: &live,
        use_of: &idle,
        cancel: &cancel,
        deadline: Instant::now(),
        target_budget: CARGO_TARGET_BUDGET,
    };
    let scan = scan_sessions(&root, UID, &ctx, false);
    assert!(scan.candidates.is_empty());
    assert!(!scan.complete);
    assert!(
        scan.issues.iter().any(|i| i.contains("time limit")),
        "{:?}",
        scan.issues
    );
    let mut issues = Vec::new();
    let big = root.join(".tmpLEAK01");
    fs::create_dir_all(&big).unwrap();
    leaked_temp(&root, &ctx, &mut issues);
    assert!(
        issues.iter().any(|i| i.contains("time limit")),
        "{issues:?}"
    );
    // A temp directory that cannot be listed is an issue, not an empty answer.
    let mut issues = Vec::new();
    let found = leaked_temp(&root.join("missing"), &ctx, &mut issues);
    assert!(found.is_empty());
    assert!(issues.iter().any(|i| i.contains("could not be listed")));
}

/// A target past the walk budget is kept for being too large to verify, which
/// is neither "protected" nor "unexamined".
#[test]
fn a_target_too_large_to_verify_says_so() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    cargo_target(&pad.join("target-x"), 30);
    for n in 0..20 {
        fs::write(pad.join(format!("target-x/debug/f{n}")), "x").unwrap();
    }
    age(&pad.join("target-x"), 30);
    let cancel = AtomicBool::new(false);
    let live = nobody();
    let ctx = Context {
        now: now(),
        retention_days: 7,
        live: &live,
        use_of: &idle,
        cancel: &cancel,
        deadline: Instant::now() + Duration::from_secs(60),
        target_budget: tree::Budget {
            entries: 10,
            seconds: 30,
        },
    };
    let scan = scan_sessions(&root, UID, &ctx, false);
    assert!(scan.candidates.is_empty());
    let kept = kept_reason(&scan, "target-x");
    assert!(
        kept.reason
            .contains("too large to verify within 10 entries / 30 s"),
        "{}",
        kept.reason
    );
    assert_eq!(scan.unexamined, 0);
}

/// More folders than a scan finds Cargo targets for are reported, not silently
/// dropped.
#[test]
fn more_targets_than_the_cap_are_reported() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    for n in 0..=MAX_TARGETS_PER_ROOT {
        cargo_target(&pad.join(format!("target-{n:02}")), 30);
    }
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert_eq!(scan.candidates.len(), MAX_TARGETS_PER_ROOT);
    assert!(!scan.complete);
    assert!(
        scan.issues
            .iter()
            .any(|i| i.contains("more than") && i.contains("not examined")),
        "{:?}",
        scan.issues
    );
}

/// Folding a scan into an inventory is bounded: past the candidate cap the
/// inventory is partial and no preview; the kept list is capped and the
/// overflow counted.
#[test]
fn absorbing_a_scan_respects_the_candidate_and_kept_caps() {
    let candidate = |n: usize| CleanerCandidate {
        repo_path: "/r".into(),
        path: format!("/r/t{n}"),
        provider: String::new(),
        bytes: 1,
        scope: SCOPE_REPOSITORY.into(),
        private_bytes: None,
        branch: None,
        evidence: vec![],
    };
    let mut inventory = empty_inventory();
    let scan = Scan {
        candidates: (0..600).map(candidate).collect(),
        complete: true,
        ..Scan::default()
    };
    scan.absorb_into(&mut inventory);
    assert_eq!(inventory.candidates.len(), 512);
    assert!(inventory.partial && !inventory.scopes_complete);

    let mut inventory = empty_inventory();
    let mut scan = Scan {
        complete: true,
        ..Scan::default()
    };
    for n in 0..(MAX_KEPT + 30) {
        scan.keep(Path::new(&format!("/k{n}")), SCOPE_SESSION, "kept");
    }
    assert_eq!(scan.unlisted, 30);
    scan.absorb_into(&mut inventory);
    assert_eq!(inventory.kept.len(), MAX_KEPT);
    assert!(inventory
        .issues
        .iter()
        .any(|i| i.contains("30 more agent scratch folder(s)")));
    assert!(inventory.scopes_complete);
}

/// What the real wiring sees: a child process whose working directory is the
/// target, found through the real process table. Killed by its recorded pid.
#[test]
fn a_real_child_holding_the_target_keeps_it_through_the_real_process_table() {
    use crate::procguard::LockedSpawn;
    let (_temp, root) = temp_root();
    let target = scratchpad(&root, OLD).join("target-x");
    cargo_target(&target, 30);
    let mut child = std::process::Command::new("sh")
        .args(["-c", "read line"])
        .current_dir(&target)
        .stdin(std::process::Stdio::piped())
        .spawn_locked()
        .unwrap();
    let pid = i32::try_from(child.id()).unwrap();
    let real_use = |dir: &Path| crate::sysload::procscan::dir_use(dir);
    let held = crate::test_support::env::wait_until(Duration::from_secs(20), || {
        crate::sysload::procscan::dir_use(&target)
            .is_ok_and(|u| u.users.iter().any(|(p, _)| *p == pid))
    });
    let scan = run(&nobody(), &real_use, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    let candidate = scan.candidates.first().cloned();
    let refused = candidate.as_ref().map(|c| {
        run(&nobody(), &real_use, |ctx| remove(c, ctx, u64::MAX))
            .expect_err("a process holds it as its working directory")
    });
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(held, "the child never showed in the process table");
    assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
    assert!(refused.is_none());
    let reason = &kept_reason(&scan, "target-x").reason;
    assert!(reason.starts_with("in use:"), "{reason}");
    assert!(reason.contains(&format!("process {pid}")), "{reason}");
    assert!(target.is_dir());
}
