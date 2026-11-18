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
    let set = |p: &Path| crate::test_support::set_modified(p, when).unwrap();
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
    // A profile directory as Cargo writes it: `.cargo-lock` inside.
    fs::create_dir_all(dir.join("debug")).unwrap();
    fs::write(dir.join("debug/.cargo-lock"), "").unwrap();
    fs::write(dir.join("CACHEDIR.TAG"), CARGO_SIGNATURE).unwrap();
    fs::write(dir.join(".rustc_info.json"), "{}").unwrap();
    fs::write(dir.join("debug/app.o"), vec![b'x'; 4096]).unwrap();
    age(dir, days_old);
}

/// Every session folder under `root` made old at its own level, the way a
/// finished session's folder is: the session folder, its `tasks/` and
/// `scratchpad/` and what is directly in them (not what is inside a build
/// folder). A fixture that writes into a scratchpad makes its folders new.
fn quiet_sessions(root: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    let set = |p: &Path| {
        let _ = crate::test_support::set_modified(p, when);
    };
    let Ok(users) = fs::read_dir(root) else {
        return;
    };
    for user in users
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("claude-"))
    {
        for project in fs::read_dir(user.path()).into_iter().flatten().flatten() {
            for session in fs::read_dir(project.path()).into_iter().flatten().flatten() {
                for level in [
                    session.path(),
                    session.path().join("tasks"),
                    session.path().join("scratchpad"),
                ] {
                    for entry in fs::read_dir(&level).into_iter().flatten().flatten() {
                        // The entry's own time only: a dir's contents are the
                        // build folder's business (its age is tested apart).
                        if !entry.file_type().is_ok_and(|t| t.is_symlink()) {
                            set(&entry.path());
                        }
                    }
                    set(&level);
                }
            }
        }
    }
}

/// The scan as the other tests mean it: over session folders that have been
/// quiet. The recency tests call `super::scan_sessions` directly.
fn scan_sessions(root: &Path, uid: u32, ctx: &Context<'_>, measure: bool) -> Scan {
    quiet_sessions(root, 30);
    super::scan_sessions(root, uid, ctx, measure)
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
        removals: None,
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
    assert!(kept_reason(&scan, OLD).reason.contains(SESSION_UNMATCHED));
    // It is not hidden in the count of folders left alone: the scan says it as
    // its own line, naming the running sessions and what happens next.
    assert!(
        scan.issues
            .iter()
            .any(|i| i.contains("2 running Claude session(s)") && i.contains(SESSION_UNMATCHED)),
        "{:?}",
        scan.issues
    );
    // Once the count is zero the same folder is offered again.
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.kept);
    assert!(scan.issues.is_empty(), "{:?}", scan.issues);
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

#[cfg(unix)]
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
    // (The link was made just now, so it also makes the session look active;
    // it is gone before the next scan, which is about the replaced candidate.)
    fs::remove_file(pad.join("linked")).unwrap();
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
    #[cfg(unix)]
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
    // Processes the OS would not show are a check that could not finish, not a
    // rule that kept the folder.
    assert!(kept_reason(&scan, "x-target")
        .reason
        .starts_with(COULD_NOT_CHECK));
    assert!(scan.unexamined >= 1, "{}", scan.unexamined);
    assert!(!scan.complete);
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
    assert_eq!(
        (inventory.shed_repos, inventory.unchecked_folders),
        (0, 1),
        "the run counts it as a folder that could not be checked, not as load"
    );
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
            super::super::global::listed_worktrees(repo.to_str().unwrap()).is_ok(),
        )
    };
    // Free space this low reads Severe, which the real accessor treats as
    // the remedy being due, so the shed leg forces the answer to false: the
    // host is shed and the cleaner's helpers get no relief.
    let (ignored, tracked, listed) = crate::engine::process::with_disk_only_answer(false, asks);
    assert!(matches!(ignored, GitAnswer::Unknown(_)), "{ignored:?}");
    assert!(matches!(tracked, GitAnswer::Unknown(_)), "{tracked:?}");
    assert!(!listed, "shed when the governor does not say disk-only");
    // The governor says disk-only: each of the three helpers is answered.
    let (ignored, tracked, listed) = crate::engine::process::with_disk_only_answer(true, asks);
    assert_eq!(ignored, GitAnswer::Yes);
    assert_eq!(tracked, GitAnswer::No);
    assert!(listed, "the worktree listing is admitted too");
    // The relief is the helper's, not the thread's: another spawn on the same
    // thread, before and after a helper, is still shed.
    let other = || crate::engine::worktree::list_worktrees_lite(repo.to_str().unwrap()).is_ok();
    crate::engine::process::with_disk_only_answer(true, || {
        assert!(!other(), "a spawn outside the helpers is shed");
        assert_eq!(git_ignores(&repo, "tmp/a"), GitAnswer::Yes);
        assert!(!other(), "and still shed after a helper ran");
    });
    // Disk-only false (memory, swap, CPU, thermal, battery at Elevated, no
    // governor in this process) admits nothing.
    let (ignored, _, listed) = crate::engine::process::with_disk_only_answer(false, asks);
    assert!(matches!(ignored, GitAnswer::Unknown(_)), "{ignored:?}");
    assert!(!listed);

    // Through the real accessor, no seam: a governor with history. The disk
    // alone Severe answers the helpers; memory Severe beside it does not.
    drop(_disk);
    let disk_input = |memory: Level| GovernorInput {
        readings: Readings {
            free_disk: Reading::Value(8 << 30),
            ..Readings::calm()
        },
        ..GovernorInput::test_pressure(memory)
    };
    let held = crate::sysload::inject_governor_with_history(std::iter::repeat_n(
        disk_input(Level::Normal),
        10,
    ));
    assert!(crate::sysload::shed_is_disk_only());
    let (ignored, tracked, listed) = asks();
    assert_eq!(ignored, GitAnswer::Yes, "the disk alone: admitted");
    assert_eq!(tracked, GitAnswer::No);
    assert!(listed);
    assert!(
        !crate::engine::worktree::list_worktrees_lite(repo.to_str().unwrap()).is_ok(),
        "a spawn outside the helpers is still shed"
    );
    drop(held);
    let _memory = crate::sysload::inject_governor_with_history(std::iter::repeat_n(
        disk_input(Level::Critical),
        10,
    ));
    assert!(!crate::sysload::shed_is_disk_only());
    let (ignored, _, listed) = asks();
    assert_eq!(
        ignored,
        GitAnswer::Yes,
        "memory Severe beside a disk under the Severe floor: the remedy is due, admitted"
    );
    assert!(listed);
    drop(_memory);
    // Low disk only Elevated (24 GiB) with memory Severe: not due, shed.
    let mut elevated = disk_input(Level::Critical);
    elevated.readings.free_disk = Reading::Value(24 << 30);
    let _elevated = crate::sysload::inject_governor_with_history(std::iter::repeat_n(elevated, 10));
    let (ignored, _, listed) = asks();
    assert!(
        matches!(ignored, GitAnswer::Unknown(_)),
        "memory Severe beside an Elevated disk: the helpers are shed ({ignored:?})"
    );
    assert!(!listed);
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
        long_lived: vec![],
        incomplete_repos: BTreeSet::new(),
        shed_repos: 0,
        unchecked_folders: 0,
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
        removals: None,
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
        removals: None,
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

/// A folder holding anything Cargo, rustdoc and the coverage tools did not
/// write at its top level is not offered, however well it carries the markers:
/// someone put their own files in a build folder.
#[cfg(unix)]
#[test]
fn a_marked_target_holding_foreign_files_at_its_top_level_is_kept() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    for (name, foreign) in [
        ("with-src", "src"),
        ("with-notes", "NOTES.md"),
        ("with-dump", "dump.sql"),
    ] {
        let target = pad.join(name);
        cargo_target(&target, 30);
        if foreign == "src" {
            fs::create_dir_all(target.join("src")).unwrap();
            fs::write(target.join("src/lib.rs"), "fn main() {}").unwrap();
        } else {
            fs::write(target.join(foreign), "mine").unwrap();
        }
        age(&target, 30);
    }
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
    for (name, foreign) in [
        ("with-src", "src"),
        ("with-notes", "NOTES.md"),
        ("with-dump", "dump.sql"),
    ] {
        let reason = &kept_reason(&scan, name).reason;
        assert!(reason.contains(foreign), "{reason}");
        assert!(reason.contains("do not write"), "{reason}");
    }
    // None of that was a check that could not run.
    assert_eq!(scan.unexamined, 0);

    // The layouts seen on real folders pass: a target-triple directory holding
    // a profile directory, a nested marked target (cargo-llvm-cov), profile
    // data, `tmp`, `doc` and the macOS folder file.
    let (_temp2, root2) = temp_root();
    let pad2 = scratchpad(&root2, OLD);
    let real = pad2.join("target-real");
    cargo_target(&real, 30);
    fs::create_dir_all(real.join("aarch64-apple-darwin/release")).unwrap();
    fs::write(real.join("aarch64-apple-darwin/release/.cargo-lock"), "").unwrap();
    cargo_target(&real.join("llvm-cov-target"), 30);
    for dir in ["tmp", "doc"] {
        fs::create_dir_all(real.join(dir)).unwrap();
    }
    for file in ["a.profraw", "merged.profdata", ".DS_Store"] {
        fs::write(real.join(file), "").unwrap();
    }
    age(&real, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root2, UID, ctx, false)
    });
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.kept);

    // A symlink at the top level is not Cargo's.
    std::os::unix::fs::symlink("/", real.join("link")).unwrap();
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root2, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty());
    assert!(kept_reason(&scan, "target-real").reason.contains("link"));
}

/// A repository scratch folder is offered only when it is named like a build
/// folder, even with every marker.
#[test]
fn a_marked_folder_under_a_repositorys_tmp_needs_a_build_folder_name() {
    let (_temp, root) = temp_root();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    crate::engine::git_cli::git_global(&["init", "-q", repo.to_str().unwrap()]).unwrap();
    crate::test_support::trust_repo(&repo);
    fs::write(repo.join(".git/info/exclude"), "/tmp/\n").unwrap();
    for name in ["build-out", "x-target", "target-x", "target"] {
        cargo_target(&repo.join("tmp").join(name), 30);
    }
    let scan = run(&nobody(), &idle, |ctx| scan_repository(&repo, ctx, false));
    assert!(
        kept_reason(&scan, "build-out")
            .reason
            .contains("not named like a build folder"),
        "{:?}",
        scan.kept
    );
    // The three build-folder names are found and offered (the repository
    // ignores `tmp/`, nothing is tracked, and they are old).
    let mut offered: Vec<String> = scan
        .candidates
        .iter()
        .map(|c| {
            Path::new(&c.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    offered.sort();
    assert_eq!(
        offered,
        ["target", "target-x", "x-target"],
        "{:?}",
        scan.kept
    );
}

/// A plain folder named like a build folder, in a session's scratchpad, that
/// wraps the real targets: the children are judged one by one, the wrapper is
/// never offered, and a live session keeps them.
#[test]
fn a_build_named_wrapper_in_a_session_scratchpad_is_looked_into_one_level() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let wrapper = pad.join("target-wrap");
    fs::create_dir_all(&wrapper).unwrap();
    cargo_target(&wrapper.join("inner"), 30);
    age(&wrapper, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    let offered: Vec<&str> = scan.candidates.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(offered.len(), 1, "{offered:?} / {:?}", scan.kept);
    assert!(offered[0].ends_with("target-wrap/inner"), "{offered:?}");
    assert!(
        kept_reason(&scan, "target-wrap")
            .reason
            .contains("named like a build folder but"),
        "the wrapper itself is never offered"
    );
    let live = LiveSessions {
        ids: BTreeSet::from([OLD.to_string()]),
        ..LiveSessions::default()
    };
    let scan = run(&live, &idle, |ctx| scan_sessions(&root, UID, ctx, false));
    assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
    assert!(kept_reason(&scan, "inner").reason.contains("is running"));
    // Removing the child leaves the wrapper in place.
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    run(&nobody(), &idle, |ctx| {
        remove(&scan.candidates[0], ctx, u64::MAX)
    })
    .unwrap();
    assert!(wrapper.is_dir());
    assert!(!wrapper.join("inner").exists());
}

/// A process that begins using the folder after the judgement and before the
/// first deletion is caught by the second reading.
#[test]
fn a_process_that_starts_between_the_judgement_and_the_removal_keeps_the_folder() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let target = pad.join("cargo-target");
    cargo_target(&target, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    let candidate = &scan.candidates[0];
    let calls = Cell::new(0);
    let second_reading_finds_it = |_: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            Ok(DirUse::default())
        } else {
            Ok(DirUse {
                users: vec![(77, UseKind::WorkingDirectory)],
                ..DirUse::default()
            })
        }
    };
    let refused = run(&nobody(), &second_reading_finds_it, |ctx| {
        remove(candidate, ctx, u64::MAX)
    })
    .expect_err("the second reading finds a user");
    assert_eq!(calls.get(), 2, "read at judgement and again before removal");
    assert!(refused.contains("process 77"), "{refused}");
    assert!(target.join("debug/app.o").is_file(), "nothing was removed");
}

/// Claude sessions the registry names no id for: an id on the command line
/// identifies the session; the executable's helper modes are not sessions; a
/// session with neither stays unidentified.
#[test]
fn a_session_without_a_registry_id_is_identified_from_its_command_line() {
    let mut claude = observed("claude", true, vec![None, None, None, None]);
    for (live, pid) in claude.live.iter_mut().zip([11, 12, 13, 14]) {
        live.pid = pid;
    }
    let argv = |pid: u32| -> Option<Vec<String>> {
        let args: &[&str] = match pid {
            11 => &[
                "claude",
                crate::workbench::terminal_command::CLAUDE_SESSION_ID_FLAG,
                "session-0009-cccccccc",
            ],
            12 => &["/x/rg", "--files"],
            13 => &["claude", "--chrome-native-host"],
            _ => return None,
        };
        Some(args.iter().map(|s| (*s).to_string()).collect())
    };
    let live = LiveSessions::from_observation_with(&vec![claude], &argv);
    assert!(live.ids.contains("session-0009-cccccccc"), "{live:?}");
    assert_eq!(live.unidentified, 1, "only pid 14 has no way to be told");
    assert!(live.incomplete.is_none());
}

/// A detector that could not look is a check that could not run: the folder is
/// unexamined, the scan is incomplete, and nothing is offered.
#[test]
fn a_blind_session_detector_makes_the_folder_unexamined() {
    let (_temp, root) = temp_root();
    cargo_target(&scratchpad(&root, OLD).join("target-x"), 30);
    let blind = LiveSessions {
        incomplete: Some("the claude detector could not look: scan failed".into()),
        ..LiveSessions::default()
    };
    let scan = run(&blind, &idle, |ctx| scan_sessions(&root, UID, ctx, false));
    assert!(scan.candidates.is_empty());
    assert_eq!(scan.unexamined, 1);
    assert!(!scan.complete);
    assert!(kept_reason(&scan, OLD).reason.starts_with(COULD_NOT_CHECK));
}

/// Restores a directory's mode when dropped, so a failing assertion cannot
/// leave a fixture the temp directory cannot remove.
#[cfg(unix)]
struct Mode(PathBuf);
#[cfg(unix)]
impl Mode {
    fn set(path: &Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        Self(path.to_path_buf())
    }
}
#[cfg(unix)]
impl Drop for Mode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
fn marked_targets(parent: &Path, count: usize) {
    for n in 0..count {
        cargo_target(&parent.join(format!("t{n:02}")), 30);
    }
}

/// The rule for a failed read, once: permission denied on a folder is an answer
/// about it (kept, listed, the scan stays complete, so the scopes stay
/// approvable); any other failure may clear and is a could-not-look (the scan
/// incomplete).
#[cfg(unix)]
#[test]
fn a_folder_gitpulse_may_not_read_is_kept_and_the_scan_stays_complete() {
    let (_temp, root) = temp_root();
    let sealed = root.join("a/b/c");
    fs::create_dir_all(&sealed).unwrap();
    let _mode = Mode::set(&sealed, 0o000);
    let scan = run(&nobody(), &idle, |ctx| {
        let mut scan = Scan {
            complete: true,
            ..Scan::default()
        };
        let found = find_targets(&root, &mut scan, SCOPE_REPOSITORY, ctx);
        assert!(found.is_empty());
        scan
    });
    assert!(scan.complete, "{:?}", scan.issues);
    assert_eq!(scan.unexamined, 0, "{:?}", scan.kept);
    let item = scan
        .kept
        .iter()
        .find(|k| k.path.ends_with("a/b/c"))
        .unwrap_or_else(|| panic!("listed as kept: {:?}", scan.kept));
    assert!(item.reason.contains("may not read"), "{item:?}");
    assert!(!item.reason.starts_with(COULD_NOT_CHECK), "{item:?}");
}

#[test]
fn only_a_failure_that_may_clear_makes_the_scan_incomplete() {
    let (_temp, root) = temp_root();
    let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let busy = std::io::Error::from(std::io::ErrorKind::Other);
    assert!(matches!(read_failure("x", &denied), Refusal::Denied(_)));
    assert!(matches!(read_failure("x", &busy), Refusal::Unexamined(_)));
    let mut scan = Scan {
        complete: true,
        ..Scan::default()
    };
    scan.unreadable_io(&root, SCOPE_REPOSITORY, "it could not be listed", &denied);
    assert!(scan.complete && scan.unexamined == 0);
    scan.unreadable_io(&root, SCOPE_REPOSITORY, "it could not be listed", &busy);
    assert!(!scan.complete && scan.unexamined == 1, "{:?}", scan.kept);
}

/// A folder over its listing bound is an answer about that folder, not an
/// unfinished scan.
#[cfg(unix)]
#[test]
fn a_target_top_level_over_its_bound_is_kept_not_unexamined() {
    let (_temp, root) = temp_root();
    let target = root.join("target-x");
    cargo_target(&target, 30);
    for n in 0..=TOP_LEVEL_LISTING_BOUND {
        fs::write(target.join(format!("f{n}")), "").unwrap();
    }
    let refusal = foreign_top_level(&target, 0).unwrap_err();
    assert!(
        matches!(&refusal, Refusal::Denied(why) if why.contains("more than")),
        "{refusal:?}"
    );
}

/// A directory counts as a profile only by what Cargo writes: `deps` and
/// `.fingerprint` real directories, `.cargo-lock` a regular file, never a
/// symlink or a plain file standing in.
#[cfg(unix)]
#[test]
fn a_profile_needs_real_directories_and_a_real_lock_file() {
    let (_temp, root) = temp_root();
    let target = root.join("target-x");
    cargo_target(&target, 30);
    let layout = |name: &str, build: &dyn Fn(&Path)| {
        let dir = target.join(name);
        fs::create_dir_all(&dir).unwrap();
        build(&dir);
        foreign_top_level(&target, 0).unwrap()
    };
    // Plain files named like the directories do not make a profile.
    assert_eq!(
        layout("fake1", &|d| {
            fs::write(d.join("deps"), "").unwrap();
            fs::write(d.join(".fingerprint"), "").unwrap();
        })
        .as_deref(),
        Some("fake1")
    );
    // Symlinks named like them do not either.
    assert_eq!(
        layout("fake2", &|d| {
            std::os::unix::fs::symlink("/", d.join("deps")).unwrap();
            std::os::unix::fs::symlink("/etc/hosts", d.join(".cargo-lock")).unwrap();
        })
        .as_deref(),
        Some("fake1"),
        "the first foreign name in order is reported; fake2 is foreign too"
    );
    fs::remove_dir_all(target.join("fake1")).unwrap();
    assert_eq!(
        foreign_top_level(&target, 0).unwrap().as_deref(),
        Some("fake2")
    );
    fs::remove_dir_all(target.join("fake2")).unwrap();
    // The real thing is accepted.
    assert_eq!(
        layout("release", &|d| {
            fs::create_dir_all(d.join("deps")).unwrap();
            fs::write(d.join(".cargo-lock"), "").unwrap();
        }),
        None
    );
}

/// A scan root GitPulse may not list is a kept answer with the reason, not an
/// empty root: nothing in it was looked at.
#[cfg(unix)]
#[test]
fn a_root_that_cannot_be_listed_is_kept_not_empty() {
    let (_temp, root) = temp_root();
    let sealed = root.join("sealed");
    fs::create_dir_all(&sealed).unwrap();
    let _mode = Mode::set(&sealed, 0o000);
    let scan = run(&nobody(), &idle, |ctx| {
        let mut scan = Scan {
            complete: true,
            ..Scan::default()
        };
        let found = find_targets(&sealed, &mut scan, SCOPE_REPOSITORY, ctx);
        assert!(found.is_empty());
        scan
    });
    assert!(
        denied_and_complete(&scan),
        "a first listing GitPulse may not read follows the same rule as every other site: {:?}",
        scan.kept
    );
}

/// A wrapper folder that cannot be listed is a could-not-look with a named
/// issue; more marked children than the cap gets the cap's own message; a
/// symlinked child is never followed.
#[cfg(unix)]
#[test]
fn a_wrapper_that_cannot_be_listed_or_holds_too_many_is_said_so_and_a_symlink_is_not_followed() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    // Listable by neither: x but no r (its markers can be stat'ed, not listed).
    let sealed = pad.join("target-sealed");
    fs::create_dir_all(&sealed).unwrap();
    let _mode = Mode::set(&sealed, 0o111);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty());
    assert_eq!(scan.unexamined, 0, "{:?}", scan.kept);
    assert!(scan.complete, "a lasting refusal leaves the scan complete");
    assert!(
        scan.kept
            .iter()
            .any(|k| k.path.contains("target-sealed") && k.reason.contains("may not read it")),
        "{:?}",
        scan.kept
    );
    drop(_mode);
    fs::remove_dir(&sealed).unwrap();

    // The cap: one wrapper with more marked children than a root may offer.
    let wrap = pad.join("target-many");
    fs::create_dir_all(&wrap).unwrap();
    marked_targets(&wrap, MAX_TARGETS_PER_ROOT + 1);
    age(&wrap, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    assert_eq!(scan.candidates.len(), MAX_TARGETS_PER_ROOT);
    assert!(!scan.complete);
    assert!(
        scan.issues
            .iter()
            .any(|i| i.contains("the per-root cap") && i.contains("preview again")),
        "the same message the main loop gives: {:?}",
        scan.issues
    );
    fs::remove_dir_all(&wrap).unwrap();

    // A symlink inside a wrapper to a marked target elsewhere.
    let outside = root.join("outside-target");
    cargo_target(&outside, 30);
    let wrapper = pad.join("target-link");
    fs::create_dir_all(&wrapper).unwrap();
    std::os::unix::fs::symlink(&outside, wrapper.join("inner")).unwrap();
    age(&wrapper, 30);
    quiet_sessions(&root, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        super::scan_sessions(&root, UID, ctx, false)
    });
    assert!(
        !scan
            .candidates
            .iter()
            .any(|c| c.path.contains("inner") || c.path.contains("outside")),
        "{:?}",
        scan.candidates
    );
}

/// A folder in which something was modified within the retention is not
/// finished, whatever the detector says: a session the detector cannot see (a
/// renamed launcher, a wrapper-dropped process) still writes in its own folder.
#[test]
fn a_session_the_detector_cannot_see_is_kept_while_its_folder_is_recent() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    cargo_target(&pad.join("target-x"), 30);
    let tasks = pad.parent().unwrap().join("tasks");
    fs::create_dir_all(&tasks).unwrap();
    fs::write(tasks.join("out.log"), "running").unwrap();
    // The detector sees nothing and nothing is unidentified.
    let scan = run(&nobody(), &idle, |ctx| {
        super::scan_sessions(&root, UID, ctx, false)
    });
    assert!(scan.candidates.is_empty(), "{:?}", scan.candidates);
    assert!(kept_reason(&scan, "target-x")
        .reason
        .contains("too recent: something in the session folder"));
    assert_eq!(scan.unexamined, 0, "an answer, not a failure to look");
    // The same folder, quiet for a month, is offered, with the evidence line.
    quiet_sessions(&root, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        super::scan_sessions(&root, UID, ctx, false)
    });
    assert_eq!(scan.candidates.len(), 1, "{:?}", scan.kept);
    assert!(
        scan.candidates[0]
            .evidence
            .iter()
            .any(|e| e.contains("nothing in the session folder was modified for")),
        "{:?}",
        scan.candidates[0].evidence
    );
}

/// The session folder's three listings: unreadable, past their bound and dated
/// in the future are each not "quiet".
#[cfg(unix)]
#[test]
fn a_session_folder_whose_age_cannot_be_read_or_trusted_is_not_quiet() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let session = pad.parent().unwrap().to_path_buf();
    quiet_sessions(&root, 30);
    let cancel = AtomicBool::new(false);
    let live = nobody();
    let ctx = Context {
        now: now(),
        retention_days: 7,
        live: &live,
        use_of: &idle,
        cancel: &cancel,
        deadline: Instant::now() + Duration::from_secs(60),
        target_budget: CARGO_TARGET_BUDGET,
        removals: None,
    };
    assert!(session_quiet(&session, &ctx).unwrap() >= 29);
    // Unreadable: a tasks folder that cannot be listed.
    let tasks = session.join("tasks");
    fs::create_dir_all(&tasks).unwrap();
    {
        let _mode = Mode::set(&tasks, 0o000);
        let refusal = session_quiet(&session, &ctx).unwrap_err();
        assert!(matches!(refusal, Refusal::Denied(_)), "{refusal:?}");
    }
    // Cut by the bound: more entries than the listing reads.
    for n in 0..=SESSION_LISTING_BOUND {
        fs::write(tasks.join(format!("f{n}")), "").unwrap();
    }
    quiet_sessions(&root, 30);
    let refusal = session_quiet(&session, &ctx).unwrap_err();
    assert!(
        matches!(&refusal, Refusal::Denied(why) if why.contains("more than")),
        "{refusal:?}"
    );
    fs::remove_dir_all(&tasks).unwrap();
    // A modification time in the future keeps it.
    let future = SystemTime::now() + Duration::from_secs(10 * 86_400);
    crate::test_support::set_modified(&pad.join("..").join("scratchpad"), future).unwrap();
    let refusal = session_quiet(&session, &ctx).unwrap_err();
    assert!(
        matches!(&refusal, Refusal::Kept(why) if why.contains("future")),
        "{refusal:?}"
    );
}

/// A nested marked target and a target-triple directory are looked into: the
/// first must itself pass, and every child of the second must be a profile
/// directory or an accepted sibling. A profile is also recognised by `deps` alone.
#[test]
fn a_nested_target_and_a_triple_directory_are_checked_not_waved_through() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let build = |name: &str, extra: &dyn Fn(&Path)| {
        let target = pad.join(name);
        cargo_target(&target, 30);
        extra(&target);
        age(&target, 30);
    };
    build("target-nested", &|t| {
        cargo_target(&t.join("llvm-cov-target"), 30);
        fs::create_dir_all(t.join("llvm-cov-target/my-notes")).unwrap();
        fs::write(t.join("llvm-cov-target/my-notes/a.txt"), "mine").unwrap();
    });
    build("target-triple", &|t| {
        fs::create_dir_all(t.join("aarch64-apple-darwin/release")).unwrap();
        fs::write(t.join("aarch64-apple-darwin/release/.cargo-lock"), "").unwrap();
        fs::create_dir_all(t.join("aarch64-apple-darwin/stuff")).unwrap();
    });
    build("target-deps-only", &|t| {
        fs::create_dir_all(t.join("custom/deps")).unwrap();
    });
    build("target-clean-nested", &|t| {
        cargo_target(&t.join("llvm-cov-target"), 30);
    });
    build("target-too-deep", &|t| {
        cargo_target(&t.join("a-target"), 30);
        cargo_target(&t.join("a-target/b-target"), 30);
    });
    let scan = run(&nobody(), &idle, |ctx| {
        scan_sessions(&root, UID, ctx, false)
    });
    let offered: Vec<String> = scan
        .candidates
        .iter()
        .filter_map(|c| {
            Path::new(&c.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .collect();
    let reason = |name: &str| kept_reason(&scan, name).reason.clone();
    assert!(
        reason("target-nested").contains("llvm-cov-target/my-notes"),
        "{}",
        reason("target-nested")
    );
    assert!(
        reason("target-triple").contains("aarch64-apple-darwin/stuff"),
        "{}",
        reason("target-triple")
    );
    assert!(
        reason("target-too-deep").contains("nested more than"),
        "{}",
        reason("target-too-deep")
    );
    for ok in ["target-deps-only", "target-clean-nested"] {
        assert!(
            offered.contains(&ok.to_string()),
            "{ok}: {offered:?} / {:?}",
            scan.kept
        );
    }
    assert_eq!(scan.unexamined, 0);
}

/// Every place the layout rule cannot read is could-not-look, not "foreign" and
/// not "fine".
#[cfg(unix)]
#[test]
fn the_layout_rule_names_what_it_could_not_read() {
    let (_temp, root) = temp_root();
    let target = root.join("target-x");
    cargo_target(&target, 30);
    fs::create_dir_all(target.join("triple/release")).unwrap();
    fs::write(target.join("triple/release/.cargo-lock"), "").unwrap();
    fs::create_dir_all(target.join("triple/odd")).unwrap();
    // A triple child that cannot be inspected.
    {
        let _mode = Mode::set(&target.join("triple/odd"), 0o000);
        // Inspecting `odd` itself (is_profile) fails: it is x-less.
        let refusal = foreign_top_level(&target, 0).unwrap_err();
        assert!(matches!(refusal, Refusal::Denied(_)), "{refusal:?}");
    }
    // A top level that cannot be listed.
    {
        let _mode = Mode::set(&target, 0o000);
        let refusal = foreign_top_level(&target, 0).unwrap_err();
        assert!(matches!(refusal, Refusal::Denied(_)), "{refusal:?}");
    }
    // A nested target that cannot be read.
    let nested = target.join("llvm-cov-target");
    cargo_target(&nested, 30);
    {
        let _mode = Mode::set(&nested, 0o000);
        let refusal = foreign_top_level(&target, 0).unwrap_err();
        assert!(matches!(refusal, Refusal::Denied(_)), "{refusal:?}");
    }
    // A nested target whose folder reads but whose marker file does not: the
    // folder was not judged, so it is neither accepted nor called foreign.
    {
        let _mode = Mode::set(&nested.join("CACHEDIR.TAG"), 0o000);
        let refusal = foreign_top_level(&target, 0).unwrap_err();
        assert!(matches!(refusal, Refusal::Denied(_)), "{refusal:?}");
    }
}

/// The recency guard ignores the time change of exactly the directories this
/// run removed an entry from, and nothing else: a file a live session writes
/// afterwards (in `scratchpad/`, in `tasks/` or inside a wrapper) still keeps
/// the folder, and a modification time that predates the first removal is not
/// excused.
#[test]
fn the_runs_own_removal_is_ignored_only_where_it_happened() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let session = pad.parent().unwrap().to_path_buf();
    let wrapper = pad.join("target-wrap");
    fs::create_dir_all(&wrapper).unwrap();
    fs::create_dir_all(session.join("tasks")).unwrap();
    quiet_sessions(&root, 30);
    let cancel = AtomicBool::new(false);
    let live = nobody();
    let removals = SelfRemovals::default();
    fn with_removals<'a>(
        live: &'a LiveSessions,
        cancel: &'a AtomicBool,
        removals: Option<&'a SelfRemovals>,
    ) -> Context<'a> {
        Context {
            now: now(),
            retention_days: 7,
            live,
            use_of: &idle,
            cancel,
            deadline: Instant::now() + Duration::from_secs(60),
            target_budget: CARGO_TARGET_BUDGET,
            removals,
        }
    }
    macro_rules! ctx {
        ($removals:expr) => {
            with_removals(&live, &cancel, $removals)
        };
    }
    // What an unlink does to the parent: its modification time moves.
    let unlink_in = |parent: &Path| {
        let name = parent.join(".gone");
        fs::write(&name, "").unwrap();
        fs::remove_file(&name).unwrap();
    };
    let kept = |why: Result<u64, Refusal>| matches!(why, Err(Refusal::Kept(_)));

    // Before: a quiet session, 29+ days.
    assert!(session_quiet(&session, &ctx!(Some(&removals))).unwrap() >= 29);

    // The run removed a target from `scratchpad/`.
    removals.note_removal(&session, &pad, now());
    unlink_in(&pad);
    assert!(
        kept(session_quiet(&session, &ctx!(None))),
        "without the record the parent's new time keeps the folder"
    );
    assert!(
        session_quiet(&session, &ctx!(Some(&removals))).is_ok(),
        "with it, the run's own unlink is not a session writing"
    );

    // A file a live session writes into `scratchpad/` afterwards keeps it.
    let written = pad.join("notes.txt");
    fs::write(&written, "x").unwrap();
    assert!(kept(session_quiet(&session, &ctx!(Some(&removals)))));
    fs::remove_file(&written).unwrap();
    assert!(session_quiet(&session, &ctx!(Some(&removals))).is_ok());

    // So does one in `tasks/`, which is never excused.
    let task = session.join("tasks/t.json");
    fs::write(&task, "x").unwrap();
    assert!(kept(session_quiet(&session, &ctx!(Some(&removals)))));
    fs::remove_file(&task).unwrap();
    unlink_in(&session.join("tasks"));
    assert!(
        kept(session_quiet(&session, &ctx!(Some(&removals)))),
        "a time change in tasks/ is never the run's doing"
    );
    quiet_sessions(&root, 30);
    // Same under a wrapper: removed a child of it, then a session writes in it.
    removals.note_removal(&session, &wrapper, now());
    unlink_in(&wrapper);
    assert!(session_quiet(&session, &ctx!(Some(&removals))).is_ok());
    let inside = wrapper.join("late.log");
    fs::write(&inside, "x").unwrap();
    assert!(
        kept(session_quiet(&session, &ctx!(Some(&removals)))),
        "a file written inside the wrapper after the removal is seen"
    );
    fs::remove_file(&inside).unwrap();
    quiet_sessions(&root, 30);
    // Another session folder is not covered by this one's record.
    let other = scratchpad(&root, "session-0002-bbbbbbbb");
    let other_session = other.parent().unwrap().to_path_buf();
    quiet_sessions(&root, 30);
    unlink_in(&other);
    assert!(kept(session_quiet(&other_session, &ctx!(Some(&removals)))));
}

fn recency_context<'a>(
    live: &'a LiveSessions,
    cancel: &'a AtomicBool,
    removals: Option<&'a SelfRemovals>,
    use_of: &'a dyn Fn(&Path) -> Result<DirUse, String>,
) -> Context<'a> {
    Context {
        now: now(),
        retention_days: 7,
        live,
        use_of,
        cancel,
        deadline: Instant::now() + Duration::from_secs(60),
        target_budget: CARGO_TARGET_BUDGET,
        removals,
    }
}

/// A removal refused before it unlinked anything changed nothing, so it does
/// not excuse its parent: a write to the parent afterwards still reads as the
/// session being alive.
#[test]
fn a_removal_refused_before_its_first_unlink_does_not_excuse_its_parent() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let session = pad.parent().unwrap().to_path_buf();
    let target = pad.join("cargo-target");
    cargo_target(&target, 30);
    quiet_sessions(&root, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        super::scan_sessions(&root, UID, ctx, false)
    });
    let candidate = scan.candidates[0].clone();
    let removals = SelfRemovals::default();
    let calls = Cell::new(0);
    // The re-check right before the first unlink is the moment something is
    // added to the target's top level: its directory stamp no longer matches,
    // which the removal reads before it unlinks anything.
    let adds_a_file = |_: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            fs::write(target.join("late.log"), "x").unwrap();
        }
        Ok(DirUse::default())
    };
    let cancel = AtomicBool::new(false);
    let live = nobody();
    let ctx = recency_context(&live, &cancel, Some(&removals), &adds_a_file);
    let failure = remove_judged(&candidate, &ctx, u64::MAX).unwrap_err();
    assert!(
        matches!(&failure, Refusal::Kept(why) if why.contains("changed after preview")),
        "refused before the first unlink: {failure:?}"
    );
    assert!(target.join("debug/app.o").exists(), "nothing was removed");
    // The file the fixture added changed the target's own time: put it back,
    // so the only recent time left is the one the parent gets below.
    fs::remove_file(target.join("late.log")).unwrap();
    age(&target, 30);
    // A session writes into the scratchpad afterwards.
    fs::write(pad.join("late.txt"), "x").unwrap();
    fs::remove_file(pad.join("late.txt")).unwrap();
    let live = nobody();
    let ctx = recency_context(&live, &cancel, Some(&removals), &idle);
    assert!(
        matches!(session_quiet(&session, &ctx), Err(Refusal::Kept(_))),
        "the parent's new time is not excused by a removal that removed nothing"
    );
}

/// The instant is per parent directory: one first removed from later in the
/// run does not inherit an earlier parent's instant, so a change to it dated
/// between the two is not excused.
#[test]
fn each_parent_has_its_own_first_removal_instant() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let session = pad.parent().unwrap().to_path_buf();
    let wrapper = pad.join("target-wrap");
    fs::create_dir_all(&wrapper).unwrap();
    quiet_sessions(&root, 30);
    let removals = SelfRemovals::default();
    let t = now();
    removals.note_removal(&session, &pad, t - 600);
    removals.note_removal(&session, &wrapper, t - 300);
    // A change to the wrapper dated between the two instants.
    crate::test_support::set_modified(&wrapper, SystemTime::now() - Duration::from_secs(450))
        .unwrap();
    let cancel = AtomicBool::new(false);
    let live = nobody();
    let ctx = recency_context(&live, &cancel, Some(&removals), &idle);
    assert!(
        matches!(session_quiet(&session, &ctx), Err(Refusal::Kept(_))),
        "dated before the wrapper's own first removal, so it is a session's doing"
    );
    // At or after its own instant it is the run's.
    crate::test_support::set_modified(&wrapper, SystemTime::now() - Duration::from_secs(200))
        .unwrap();
    assert!(session_quiet(&session, &ctx).is_ok());
}

/// A marker GitPulse may not read is no evidence either way: the folder is
/// kept with that reason (a lasting property, so the scan stays complete).
#[cfg(unix)]
#[test]
fn an_unreadable_marker_is_kept_not_no_markers() {
    let (_temp, root) = temp_root();
    let target = root.join("target-x");
    cargo_target(&target, 30);
    let _mode = Mode::set(&target.join("CACHEDIR.TAG"), 0o000);
    let refusal = cargo_evidence(&target).unwrap_err();
    assert!(matches!(refusal, Refusal::Denied(_)), "{refusal:?}");
    drop(_mode);
    fs::remove_file(target.join("CACHEDIR.TAG")).unwrap();
    assert!(
        matches!(cargo_evidence(&target), Err(Refusal::Kept(_))),
        "absent is an answer"
    );
}

/// A scan that was refused permission to read something and nothing else: no
/// folder counted as unexamined, the scan complete, and a kept row for it.
#[cfg(unix)]
fn denied_and_complete(scan: &Scan) -> bool {
    scan.unexamined == 0
        && scan.complete
        && scan
            .kept
            .iter()
            .any(|k| k.reason.contains("may not read it"))
}

/// A folder this scan may not read (permission denied, a lasting property) is
/// kept with that reason and leaves the scan complete, in every scope; absent
/// is just absent. The transient failures that stay unexamined are covered by
/// `only_a_failure_that_may_clear_makes_the_scan_incomplete`.
#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_inspected_is_kept_in_every_scan() {
    // Sessions: the scope root listable, a project folder not inspectable.
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let scope_root = root.join(format!("claude-{UID}"));
    let _scope = Mode::set(&scope_root, 0o444);
    let scan = run(&nobody(), &idle, |ctx| {
        super::scan_sessions(&root, UID, ctx, false)
    });
    assert!(denied_and_complete(&scan), "{:?}", scan.kept);
    drop(_scope);
    let _ = pad;
    // Repository scratch.
    let (_temp2, root2) = temp_root();
    let repo = root2.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let _repo = Mode::set(&repo, 0o444);
    let scan = run(&nobody(), &idle, |ctx| scan_repository(&repo, ctx, false));
    assert!(denied_and_complete(&scan), "{:?}", scan.kept);
    drop(_repo);
    // `.claude` targets.
    let (_temp3, root3) = temp_root();
    let checkout = root3.join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    let _checkout = Mode::set(&checkout, 0o444);
    let scan = run(&nobody(), &idle, |ctx| {
        scan_claude_targets(std::slice::from_ref(&checkout), ctx, false)
    });
    assert!(denied_and_complete(&scan), "{:?}", scan.kept);
    drop(_checkout);
    // Absent is absent: no scratch, no `.claude`, nothing unexamined.
    let scan = run(&nobody(), &idle, |ctx| scan_repository(&repo, ctx, false));
    assert_eq!((scan.unexamined, scan.complete), (0, true));
    // A folder whose entries cannot be inspected inside a scratch root.
    let (_temp4, root4) = temp_root();
    let pad4 = scratchpad(&root4, OLD);
    cargo_target(&pad4.join("target-a"), 30);
    let _pad = Mode::set(&pad4, 0o444);
    let scan = run(&nobody(), &idle, |ctx| {
        super::scan_sessions(&root4, UID, ctx, false)
    });
    assert!(denied_and_complete(&scan), "{:?}", scan.kept);
}

/// The judgement's own could-not-look cases: a path component that cannot be
/// inspected, and a walk that ran out of time (as against one past its entry
/// budget, which is an answer).
#[cfg(unix)]
#[test]
fn a_judgement_that_could_not_look_or_ran_out_of_time_is_unexamined() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let target = pad.join("cargo-target");
    cargo_target(&target, 30);
    quiet_sessions(&root, 30);
    let scope_root = root.join(format!("claude-{UID}"));
    let cancel = AtomicBool::new(false);
    let live = nobody();
    let context = |budget: tree::Budget| Context {
        now: now(),
        retention_days: 7,
        live: &live,
        use_of: &idle,
        cancel: &cancel,
        deadline: Instant::now() + Duration::from_secs(60),
        target_budget: budget,
        removals: None,
    };
    let session = pad.parent().unwrap().to_path_buf();
    let id = OLD;
    let judged = |ctx: &Context<'_>| {
        judge(
            &scope_root,
            &target,
            &pad,
            Some((id, session.as_path())),
            ctx,
        )
        .err()
    };
    assert!(
        judged(&context(CARGO_TARGET_BUDGET)).is_none(),
        "the fixture is judged fine"
    );
    // Time: an unexamined row, so the run comes back sooner.
    let timed_out = judged(&context(tree::Budget {
        entries: 1_000_000,
        seconds: 0,
    }))
    .unwrap();
    assert!(matches!(timed_out, Refusal::Unexamined(_)), "{timed_out:?}");
    // Entries: the tree is too large, which is an answer.
    let too_large = judged(&context(tree::Budget {
        entries: 3,
        seconds: 30,
    }))
    .unwrap();
    assert!(
        matches!(&too_large, Refusal::Kept(why) if why.contains("too large")),
        "{too_large:?}"
    );
    // A component that cannot be inspected on the way to the target.
    let _pad = Mode::set(&pad, 0o444);
    let unreadable = judged(&context(CARGO_TARGET_BUDGET)).unwrap();
    assert!(
        matches!(unreadable, Refusal::Denied(_)),
        "a folder GitPulse may not read is a kept answer: {unreadable:?}"
    );
}

/// A removal that stops part way after unlinking files reports those bytes,
/// instead of reading as a refusal that removed nothing.
#[test]
fn a_removal_stopped_part_way_reports_the_bytes_it_removed() {
    let (_temp, root) = temp_root();
    let pad = scratchpad(&root, OLD);
    let target = pad.join("cargo-target");
    cargo_target(&target, 30);
    quiet_sessions(&root, 30);
    let scan = run(&nobody(), &idle, |ctx| {
        super::scan_sessions(&root, UID, ctx, false)
    });
    let candidate = scan.candidates[0].clone();
    let calls = Cell::new(0);
    // The second reading (right before the first unlink) is the moment a build
    // touches `.rustc_info.json`, which is unlinked last: everything before it
    // is already gone when the removal reaches it.
    let touches_a_late_file = |_: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            fs::write(target.join(".rustc_info.json"), "{\"changed\":true}").unwrap();
        }
        Ok(DirUse::default())
    };
    let cancel = AtomicBool::new(false);
    let live = nobody();
    let ctx = Context {
        now: now(),
        retention_days: 7,
        live: &live,
        use_of: &touches_a_late_file,
        cancel: &cancel,
        deadline: Instant::now() + Duration::from_secs(60),
        target_budget: CARGO_TARGET_BUDGET,
        removals: None,
    };
    let failure = remove_judged(&candidate, &ctx, u64::MAX).unwrap_err();
    let Refusal::PartlyRemoved { why, bytes } = &failure else {
        panic!("a partial removal is not a plain refusal: {failure:?}");
    };
    assert!(
        *bytes >= 4096,
        "the 4096-byte file was already unlinked: {bytes}"
    );
    assert!(why.contains("changed after preview"), "{why}");
    assert!(
        !target.join("debug/app.o").exists(),
        "the earlier entries are gone"
    );
    assert!(
        target.join(".rustc_info.json").is_file(),
        "the changed one stays"
    );
    assert!(failure.message().contains("partly removed"));
}
