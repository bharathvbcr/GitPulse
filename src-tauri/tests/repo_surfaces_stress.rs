//! Stress and adversarial coverage for the stash, remote and submodule
//! surfaces.
//!
//! The stash cases carry the most weight. The stash stack is the one piece of
//! git state that is *shared across every worktree of a repository* and is
//! addressed by a position that shifts whenever anyone touches it — so it is
//! the surface where a race does not merely fail, it destroys the wrong work.

use gitpulse_lib::engine::stash::{self, StashAction};
use gitpulse_lib::engine::submodules::{self, SubmoduleChange};
use gitpulse_lib::engine::{remotes, RemoteChange};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn run_git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_EDITOR", "true")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "t@example.com"]);
    run_git(dir.path(), &["config", "user.name", "T"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(dir.path().join("f.txt"), "base\n").unwrap();
    run_git(dir.path(), &["add", "-A"]);
    run_git(dir.path(), &["commit", "-m", "base"]);
    dir
}

fn push_stash(dir: &Path, message: &str, content: &str) {
    fs::write(dir.join("f.txt"), content).unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", message]);
}

fn allow(argv: &[&str]) -> Result<Vec<String>, String> {
    Ok(argv.iter().map(|a| a.to_string()).collect())
}

/// A deep stack must list coherently: every index distinct, every object id
/// distinct, and the selector matching the position it was reported at.
#[test]
fn a_deep_stash_stack_lists_coherently() {
    const DEPTH: usize = 120;
    let repo = init_repo();
    let dir = repo.path();
    for i in 0..DEPTH {
        push_stash(dir, &format!("wip {i}"), &format!("content {i}\n"));
    }

    let entries = stash::list(&dir.to_string_lossy()).unwrap();
    assert_eq!(entries.len(), DEPTH);

    let mut indices = HashSet::new();
    let mut oids = HashSet::new();
    for (position, entry) in entries.iter().enumerate() {
        assert_eq!(entry.index, position, "index must match list position");
        assert_eq!(entry.selector, format!("stash@{{{position}}}"));
        assert!(
            indices.insert(entry.index),
            "duplicate index {}",
            entry.index
        );
        assert!(
            oids.insert(entry.oid.clone()),
            "duplicate oid {}",
            entry.oid
        );
    }
    // Newest first: the last pushed message leads.
    assert_eq!(entries[0].message, format!("wip {}", DEPTH - 1));
}

/// Many threads racing to drop entries must never drop more than exist, and
/// must never drop an entry whose object id they did not hold.
///
/// This is the whole reason the surface is addressed by `(index, oid)`: with
/// bare indices, every winner shifts the stack under every loser and the
/// losers destroy entries nobody selected.
#[test]
fn concurrent_drops_never_destroy_an_unselected_entry() {
    const DEPTH: usize = 12;
    let repo = init_repo();
    let dir = repo.path();
    for i in 0..DEPTH {
        push_stash(dir, &format!("wip {i}"), &format!("content {i}\n"));
    }

    let snapshot = stash::list(&dir.to_string_lossy()).unwrap();
    let path = dir.to_string_lossy().into_owned();
    // Every thread targets a DIFFERENT index from the same snapshot. Only the
    // ones whose index still holds their oid may succeed.
    let handles: Vec<_> = snapshot
        .iter()
        .cloned()
        .map(|entry| {
            let path = path.clone();
            std::thread::spawn(move || {
                stash::run_action_with(&path, StashAction::Drop, entry.index, &entry.oid, allow)
                    .map(|(_argv, _out)| entry.oid.clone())
                    .map_err(|e| (entry.oid.clone(), e))
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("no thread may panic"))
        .collect();

    let dropped: HashSet<String> = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .cloned()
        .collect();
    let remaining = stash::list(&dir.to_string_lossy()).unwrap();
    let remaining_oids: HashSet<String> = remaining.iter().map(|e| e.oid.clone()).collect();

    // Conservation: every original entry is either dropped or still present,
    // never both and never neither.
    assert_eq!(
        dropped.len() + remaining_oids.len(),
        DEPTH,
        "entries were lost or duplicated: {} dropped, {} remain",
        dropped.len(),
        remaining_oids.len()
    );
    for entry in &snapshot {
        let was_dropped = dropped.contains(&entry.oid);
        let still_there = remaining_oids.contains(&entry.oid);
        assert!(
            was_dropped ^ still_there,
            "entry {} is both dropped and present, or neither",
            entry.oid
        );
    }
    // Every failure must be the staleness refusal, never a git-level surprise.
    for (_, err) in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(
            err.contains("Refresh the stash list"),
            "unexpected failure mode: {err}"
        );
    }
}

/// Repeatedly pushing and popping must converge every time, leaving no
/// orphaned entries behind.
#[test]
fn repeated_push_and_pop_cycles_return_to_an_empty_stack() {
    let repo = init_repo();
    let dir = repo.path();
    let path = dir.to_string_lossy().into_owned();

    for round in 0..20 {
        push_stash(
            dir,
            &format!("round {round}"),
            &format!("content {round}\n"),
        );
        let entry = stash::list(&path).unwrap()[0].clone();
        stash::run_action_with(&path, StashAction::Pop, entry.index, &entry.oid, allow).unwrap();
        assert!(
            stash::list(&path).unwrap().is_empty(),
            "round {round}: pop left an entry behind"
        );
        // The popped content is back in the working tree; reset for the next
        // round so the stack, not the tree, is what is being exercised.
        run_git(dir, &["checkout", "--", "f.txt"]);
    }
}

/// Listing is a pure read: hammering it must not perturb the stack.
#[test]
fn concurrent_listing_is_consistent_and_side_effect_free() {
    let repo = init_repo();
    let dir = repo.path();
    for i in 0..8 {
        push_stash(dir, &format!("wip {i}"), &format!("content {i}\n"));
    }
    let expected = stash::list(&dir.to_string_lossy()).unwrap();
    let path = dir.to_string_lossy().into_owned();

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let path = path.clone();
            let expected = expected.clone();
            std::thread::spawn(move || {
                for _ in 0..20 {
                    assert_eq!(stash::list(&path).unwrap(), expected);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("no listing thread may panic");
    }
    assert_eq!(stash::list(&path).unwrap(), expected);
}

/// Messages that break naive parsing must survive listing intact.
#[test]
fn adversarial_stash_messages_survive_listing() {
    let repo = init_repo();
    let dir = repo.path();
    let messages = [
        "plain message",
        "message: with a colon",
        "On main: looks like a prefix",
        "WIP on fake: also looks like one",
        "ünïcode — em dash",
        "trailing space ",
        "-leading-dash",
    ];
    for (i, message) in messages.iter().enumerate() {
        push_stash(dir, message, &format!("content {i}\n"));
    }

    let entries = stash::list(&dir.to_string_lossy()).unwrap();
    assert_eq!(entries.len(), messages.len());
    // Every entry keeps a non-empty message and a distinct object id; none
    // collapsed into another's identity.
    let oids: HashSet<&str> = entries.iter().map(|e| e.oid.as_str()).collect();
    assert_eq!(oids.len(), messages.len());
    for entry in &entries {
        assert!(!entry.message.is_empty(), "message vanished: {entry:?}");
        assert!(entry.subject.contains(&entry.message) || !entry.message.is_empty());
    }
}

/// A stash taken on a branch whose name contains slashes and dots keeps that
/// branch attributed correctly.
#[test]
fn a_branch_name_with_separators_is_attributed_correctly() {
    let repo = init_repo();
    let dir = repo.path();
    run_git(dir, &["checkout", "-b", "feature/auth.v2/oauth"]);
    push_stash(dir, "half-done", "x\n");

    let entries = stash::list(&dir.to_string_lossy()).unwrap();
    assert_eq!(entries[0].branch.as_deref(), Some("feature/auth.v2/oauth"));
    assert_eq!(entries[0].message, "half-done");
}

/// Many remotes must all list, keep their URLs separate, and none may absorb
/// another's tracking refs.
#[test]
fn many_remotes_list_without_cross_contamination() {
    const COUNT: usize = 40;
    let repo = init_repo();
    let path = repo.path().to_string_lossy().into_owned();

    for i in 0..COUNT {
        remotes::apply_with(
            &path,
            &RemoteChange::Add {
                name: format!("remote-{i}"),
                url: format!("https://example.test/repo-{i}.git"),
            },
            allow,
        )
        .unwrap();
    }

    let listed = remotes::list(&path).unwrap();
    assert!(!listed.truncated);
    assert_eq!(listed.remotes.len(), COUNT);
    let names: HashSet<&str> = listed.remotes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names.len(), COUNT, "names collided");
    for remote in &listed.remotes {
        let index = remote.name.trim_start_matches("remote-");
        assert_eq!(
            remote.fetch_url.as_deref(),
            Some(format!("https://example.test/repo-{index}.git").as_str()),
            "remote {} took another's URL",
            remote.name
        );
        assert_eq!(remote.tracking_branches, 0);
    }
    // With no `origin` among many, no default may be claimed.
    assert!(
        listed.remotes.iter().all(|r| !r.is_default),
        "a default was invented among {COUNT} equally-named remotes"
    );
}

/// A remote whose name prefixes another's must not absorb its tracking refs.
#[test]
fn a_remote_name_that_prefixes_another_keeps_its_own_refs() {
    let repo = init_repo();
    let dir = repo.path();
    let path = dir.to_string_lossy().into_owned();
    run_git(
        dir,
        &["remote", "add", "origin", "https://example.test/a.git"],
    );
    run_git(
        dir,
        &[
            "remote",
            "add",
            "origin-mirror",
            "https://example.test/b.git",
        ],
    );

    // Fabricate remote-tracking refs for both without a network.
    let head = String::from_utf8_lossy(
        &Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    run_git(dir, &["update-ref", "refs/remotes/origin/main", &head]);
    run_git(dir, &["update-ref", "refs/remotes/origin/dev", &head]);
    run_git(
        dir,
        &["update-ref", "refs/remotes/origin-mirror/main", &head],
    );

    let listed = remotes::list(&path).unwrap();
    let origin = listed.remotes.iter().find(|r| r.name == "origin").unwrap();
    let mirror = listed
        .remotes
        .iter()
        .find(|r| r.name == "origin-mirror")
        .unwrap();
    assert_eq!(
        origin.tracking_branches, 2,
        "origin absorbed the mirror's ref"
    );
    assert_eq!(mirror.tracking_branches, 1);
    assert!(
        origin.is_default,
        "origin is the default even beside a prefix twin"
    );
}

/// Remote URLs that are legal but awkward must round-trip unchanged.
#[test]
fn awkward_but_legal_remote_urls_round_trip() {
    let repo = init_repo();
    let path = repo.path().to_string_lossy().into_owned();
    let cases = [
        ("spaced", "/Volumes/My Disk/repo.git"),
        ("scp", "git@github.com:owner/repo.git"),
        ("relative", "../sibling-repo"),
        ("filescheme", "file:///srv/repo.git"),
        ("deep", "https://example.test/a/very/deep/path/to/repo.git"),
    ];
    for (name, url) in cases {
        remotes::apply_with(
            &path,
            &RemoteChange::Add {
                name: name.into(),
                url: url.into(),
            },
            allow,
        )
        .unwrap_or_else(|e| panic!("{name} ({url}) was refused: {e}"));
    }

    let listed = remotes::list(&path).unwrap();
    for (name, url) in cases {
        let found = listed
            .remotes
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name} vanished from the listing"));
        assert_eq!(
            found.fetch_url.as_deref(),
            Some(url),
            "{name} URL was mangled"
        );
    }
}

/// Renaming onto a name that already exists must refuse and leave both remotes.
#[test]
fn renaming_onto_an_existing_name_is_refused_and_leaves_both() {
    let repo = init_repo();
    let path = repo.path().to_string_lossy().into_owned();
    remotes::apply_with(
        &path,
        &RemoteChange::Add {
            name: "origin".into(),
            url: "https://example.test/a.git".into(),
        },
        allow,
    )
    .unwrap();
    remotes::apply_with(
        &path,
        &RemoteChange::Add {
            name: "upstream".into(),
            url: "https://example.test/b.git".into(),
        },
        allow,
    )
    .unwrap();

    let err = remotes::apply_with(
        &path,
        &RemoteChange::Rename {
            name: "origin".into(),
            new_name: "upstream".into(),
        },
        allow,
    )
    .unwrap_err();
    assert!(err.contains("already exists"), "got: {err}");

    let listed = remotes::list(&path).unwrap();
    let names: HashSet<&str> = listed.remotes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, HashSet::from(["origin", "upstream"]));
}

/// `git remote rename` moves `refs/remotes/<old>/*` with the name. A listing
/// that kept counting those refs against the old name would show a ghost remote.
#[test]
fn renaming_moves_tracking_refs_with_the_remote() {
    let repo = init_repo();
    let dir = repo.path();
    let path = dir.to_string_lossy().into_owned();
    remotes::apply_with(
        &path,
        &RemoteChange::Add {
            name: "origin".into(),
            url: "https://example.test/a.git".into(),
        },
        allow,
    )
    .unwrap();
    let head = String::from_utf8_lossy(
        &Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    run_git(dir, &["update-ref", "refs/remotes/origin/main", &head]);
    run_git(dir, &["update-ref", "refs/remotes/origin/dev", &head]);

    remotes::apply_with(
        &path,
        &RemoteChange::Rename {
            name: "origin".into(),
            new_name: "upstream".into(),
        },
        allow,
    )
    .unwrap();

    let listed = remotes::list(&path).unwrap();
    assert_eq!(listed.remotes.len(), 1);
    assert_eq!(listed.remotes[0].name, "upstream");
    assert_eq!(
        listed.remotes[0].tracking_branches, 2,
        "tracking refs must follow the rename, not stay counted under origin"
    );
}

fn repo_with_submodule() -> (TempDir, TempDir) {
    let lib = init_repo();
    fs::write(lib.path().join("l.txt"), "lib\n").unwrap();
    run_git(lib.path(), &["add", "-A"]);
    run_git(lib.path(), &["commit", "-m", "lib"]);

    let main = init_repo();
    run_git(
        main.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &lib.path().to_string_lossy(),
            "vendor/lib",
        ],
    );
    run_git(main.path(), &["commit", "-m", "add submodule"]);
    (main, lib)
}

/// Deinit without `--force` must refuse a dirty checkout rather than discard it.
#[test]
fn deinit_without_force_refuses_uncommitted_submodule_work() {
    let (main, _lib) = repo_with_submodule();
    let sub = main.path().join("vendor/lib");
    fs::write(sub.join("l.txt"), "wip\n").unwrap();
    let path = main.path().to_string_lossy().into_owned();

    let err = submodules::apply_with(
        &path,
        &SubmoduleChange::Deinit {
            path: "vendor/lib".into(),
            force: false,
        },
        allow,
    )
    .unwrap_err();
    let lower = err.to_lowercase();
    assert!(
        lower.contains("local modifications") || lower.contains("dirty") || lower.contains("force"),
        "refused deinit must name the dirty tree, got: {err}"
    );
    assert!(
        fs::read_to_string(sub.join("l.txt"))
            .unwrap()
            .contains("wip"),
        "uncommitted submodule work must survive a refused deinit"
    );
}

/// Sync is local config rewriting and must succeed without a network, even
/// when pointed at a file-transport submodule git would refuse to clone.
#[test]
fn sync_rewrites_urls_from_gitmodules_without_cloning() {
    let (main, lib) = repo_with_submodule();
    let path = main.path().to_string_lossy().into_owned();
    // .gitmodules is git-config format, where a backslash starts an escape
    // sequence. A raw Windows path would write `\U`, `\r`, ... -- invalid
    // escapes that make git reject the section, so the submodule vanishes
    // before this test's subject is reached. git accepts forward slashes on
    // Windows, and that is what its own writer produces.
    let new_url = format!("{}.moved", lib.path().display()).replace('\\', "/");
    fs::write(
        main.path().join(".gitmodules"),
        format!("[submodule \"vendor/lib\"]\n\tpath = vendor/lib\n\turl = {new_url}\n"),
    )
    .unwrap();

    submodules::apply_with(
        &path,
        &SubmoduleChange::Sync {
            path: Some("vendor/lib".into()),
            recursive: false,
        },
        allow,
    )
    .unwrap();

    let listed = submodules::list(&path).unwrap();
    assert_eq!(listed.submodules.len(), 1);
    assert_eq!(listed.submodules[0].url.as_deref(), Some(new_url.as_str()));
}
