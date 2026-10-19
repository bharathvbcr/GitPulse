//! Adversarial review (round 2) of the rebuilt `gitpulse_landed` and the object
//! quarantine, against real git in temporary repositories.
//!
//! A test marked `#[ignore = "adv: ..."]` pins a break: it states the answer an
//! honest tool gives and fails against the current code. The others are
//! attacks that held, kept as regression guards.

use crate::engine::provenance::{landed, Landed, LandedReason, LandedVia};
use crate::procguard::LockedSpawn;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct R(tempfile::TempDir);

impl R {
    fn new() -> Self {
        let r = R(tempfile::tempdir().expect("tempdir"));
        r.init();
        r
    }

    /// A repository whose directory name contains the list separator.
    fn with_colon() -> Self {
        let parent = tempfile::tempdir().expect("tempdir");
        let dir = parent.path().join("a:b");
        std::fs::create_dir(&dir).unwrap();
        // Keep `parent` alive by leaking it into the struct's tempdir: the
        // repository lives under it, so its removal removes both.
        let r = R(parent);
        r.init_in(&dir);
        r
    }

    fn init(&self) {
        self.init_in(self.path());
    }

    fn init_in(&self, dir: &Path) {
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output_locked()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "user.email", "t@e.com"]);
        run(&["config", "commit.gpgsign", "false"]);
        crate::test_support::trust_repo(dir);
        std::fs::write(dir.join("root.txt"), "root\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "root"]);
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn work(&self) -> PathBuf {
        let colon = self.path().join("a:b");
        if colon.is_dir() {
            colon
        } else {
            self.path().to_path_buf()
        }
    }

    fn s(&self) -> String {
        self.work().to_string_lossy().into_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.work())
            .output_locked()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn put(&self, file: &str, content: &str, message: &str) -> String {
        let target = self.work().join(file);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, content).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn land(&self, commit: &str) -> Landed {
        landed(&self.s(), commit, Some("main"))
    }

    fn objects(&self) -> PathBuf {
        let common = self.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        PathBuf::from(common).join("objects")
    }
}

fn describe(a: &Landed) -> String {
    format!(
        "landed={:?} via={:?} reason={:?} reverted={:?} error={:?}",
        a.landed, a.via, a.reason, a.reverted, a.error
    )
}

/// Every file under `root` with its bytes, in path order.
fn files(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                out.push((name, std::fs::read(&path).expect("read")));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Objects (listing and bytes, `info` and `pack` included) plus every ref.
fn state(r: &R) -> (Vec<(String, Vec<u8>)>, String) {
    (files(&r.objects()), r.git(&["for-each-ref"]))
}

/// `landed` changes neither the object database nor any ref, and does answer.
fn assert_untouched(r: &R, commit: &str, base: &str, label: &str) -> Landed {
    let before = state(r);
    assert!(
        !before.0.is_empty(),
        "{label}: an empty store proves nothing"
    );
    let answer = landed(&r.s(), commit, Some(base));
    let after = state(r);
    assert!(
        before.0 == after.0,
        "{label}: the object database changed ({} files before, {} after): {}",
        before.0.len(),
        after.0.len(),
        describe(&answer)
    );
    assert_eq!(before.1, after.1, "{label}: a ref changed");
    answer
}

// --- 1. the quarantine leaks into the real object database -----------------------

/// main and f edit the same file differently, so no route matches and
/// `merge-tree` must run (and write conflicted blobs).
fn conflicting_edit(r: &R) -> String {
    r.put("shared.txt", "root\n", "shared");
    r.git(&["checkout", "-q", "-b", "f"]);
    let tip = r.put("shared.txt", "from f\n", "f edits");
    r.git(&["checkout", "-q", "main"]);
    r.put("shared.txt", "from main\n", "main edits");
    tip
}

#[test]
fn a_conflicted_merge_writes_nothing_to_the_repository() {
    let r = R::new();
    let tip = conflicting_edit(&r);
    let a = assert_untouched(&r, &tip, "main", "conflict");
    assert_eq!(a.landed, None, "{}", describe(&a));
    assert_eq!(a.reason, Some(LandedReason::ContentNotProvable));
}

#[test]
fn a_rename_heavy_merge_writes_nothing_to_the_repository() {
    let r = R::new();
    for n in 0..30 {
        r.put(
            &format!("d/file{n}.txt"),
            &format!("line one of {n}\nline two\nline three\nline four\n"),
            "seed",
        );
    }
    r.git(&["checkout", "-q", "-b", "f"]);
    for n in 0..30 {
        std::fs::write(
            r.work().join(format!("d/file{n}.txt")),
            format!("line one of {n}\nline two\nline three\nf edit {n}\n"),
        )
        .unwrap();
    }
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "f edits everything"]);
    let tip = r.git(&["rev-parse", "HEAD"]);
    r.git(&["checkout", "-q", "main"]);
    r.git(&["mv", "d", "e"]);
    for n in 0..30 {
        std::fs::write(
            r.work().join(format!("e/file{n}.txt")),
            format!("main edit {n}\nline two\nline three\nline four\n"),
        )
        .unwrap();
    }
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "main renames and edits"]);
    let a = assert_untouched(&r, &tip, "main", "rename-heavy");
    assert_ne!(a.landed, Some(true), "{}", describe(&a));
}

#[test]
fn a_binary_conflict_writes_nothing_to_the_repository() {
    let r = R::new();
    r.put("img.bin", "root\0bytes\n", "bin");
    r.git(&["checkout", "-q", "-b", "f"]);
    let tip = r.put("img.bin", "from f\0\u{1}\u{2}\n", "f edits bin");
    r.git(&["checkout", "-q", "main"]);
    r.put("img.bin", "from main\0\u{3}\u{4}\n", "main edits bin");
    let a = assert_untouched(&r, &tip, "main", "binary conflict");
    assert_eq!(a.landed, None, "{}", describe(&a));
}

#[test]
fn a_submodule_gitlink_conflict_writes_nothing_to_the_repository() {
    let r = R::new();
    let gitlink = |sha: &str| {
        r.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sha},sub"),
        ]);
        r.git(&["commit", "-q", "-m", &format!("sub -> {}", &sha[..7])]);
        r.git(&["rev-parse", "HEAD"])
    };
    gitlink("1111111111111111111111111111111111111111");
    r.git(&["checkout", "-q", "-b", "f"]);
    let tip = gitlink("2222222222222222222222222222222222222222");
    r.git(&["checkout", "-q", "main"]);
    gitlink("3333333333333333333333333333333333333333");
    let a = assert_untouched(&r, &tip, "main", "gitlink");
    assert_ne!(a.landed, Some(true), "{}", describe(&a));
}

#[test]
fn a_linked_worktree_writes_nothing_to_the_common_object_database() {
    let r = R::new();
    let tip = conflicting_edit(&r);
    let wt = tempfile::tempdir().expect("tempdir");
    let wt_path = wt.path().join("wt");
    r.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        wt_path.to_str().unwrap(),
        "main",
    ]);
    crate::test_support::trust_repo(&wt_path);
    let before = state(&r);
    let a = landed(wt_path.to_str().unwrap(), &tip, Some("main"));
    let after = state(&r);
    assert!(
        before.0 == after.0,
        "the common object database changed: {}",
        describe(&a)
    );
    assert_eq!(a.landed, None, "{}", describe(&a));
    assert_eq!(a.reason, Some(LandedReason::ContentNotProvable));
}

#[test]
fn a_repository_reading_objects_through_alternates_writes_nothing() {
    let origin = R::new();
    origin.put("shared.txt", "root\n", "shared");
    let root = origin.git(&["rev-parse", "HEAD"]);
    origin.put("shared.txt", "from main\n", "main edits");
    let clone = tempfile::tempdir().expect("tempdir");
    let out = Command::new("git")
        .args(["clone", "-q", "--shared", &origin.s()])
        .arg(clone.path())
        .output_locked()
        .expect("clone");
    assert!(out.status.success(), "{out:?}");
    crate::test_support::trust_repo(clone.path());
    let c = R(clone);
    c.git(&["config", "user.name", "T"]);
    c.git(&["config", "user.email", "t@e.com"]);
    c.git(&["config", "commit.gpgsign", "false"]);
    c.git(&["checkout", "-q", "-b", "g", &root]);
    let tip = c.put("shared.txt", "from g\n", "g edits");
    let origin_before = files(&origin.objects());
    let a = assert_untouched(&c, &tip, "origin/main", "alternates");
    assert_eq!(a.landed, None, "{}", describe(&a));
    assert!(files(&origin.objects()) == origin_before, "origin changed");
}

#[test]
fn commit_graph_settings_do_not_make_landed_write_a_graph() {
    let r = R::new();
    let tip = conflicting_edit(&r);
    r.git(&["commit-graph", "write", "--reachable"]);
    for (k, v) in [
        ("core.commitGraph", "true"),
        ("fetch.writeCommitGraph", "true"),
        ("gc.writeCommitGraph", "true"),
        ("gc.auto", "1"),
        ("feature.manyFiles", "true"),
    ] {
        r.git(&["config", k, v]);
    }
    let a = assert_untouched(&r, &tip, "main", "commit-graph");
    assert_eq!(a.landed, None, "{}", describe(&a));
}

/// `git diff` and `git log -p` apply `diff.<driver>.textconv` from the repo's own
/// configuration, and with `cachetextconv` git writes the converted blob and a
/// notes commit into the object database and `refs/notes/textconv/*`. The
/// pinned `--no-ext-diff` does not turn textconv off.
#[test]
#[ignore = "adv: git diff/log -p run repo textconv and its cache writes objects + a notes ref"]
fn a_textconv_cache_does_not_write_into_the_object_database() {
    let r = R::new();
    r.git(&["config", "diff.up.textconv", "tr a-z A-Z <"]);
    r.git(&["config", "diff.up.cachetextconv", "true"]);
    let attrs = r.work().join(".git/info/attributes");
    std::fs::write(&attrs, "*.dat diff=up\n").unwrap();
    r.put("t.dat", "root\n", "seed");
    r.git(&["checkout", "-q", "-b", "f"]);
    let tip = r.put("t.dat", "from f\n", "f edits t");
    r.git(&["checkout", "-q", "main"]);
    r.put("other.txt", "main moves\n", "main moves");
    let a = assert_untouched(&r, &tip, "main", "textconv cache");
    assert_ne!(a.landed, Some(true), "{}", describe(&a));
}

// --- 2/3. directory and environment -----------------------------------------------

#[test]
fn a_repository_path_with_the_list_separator_is_null_with_a_reason_never_false() {
    let r = R::with_colon();
    let tip = conflicting_edit(&r);
    let a = r.land(&tip);
    assert_eq!(a.landed, None, "{}", describe(&a));
    assert!(!a.error.is_empty(), "{}", describe(&a));
}

// --- 4. semantics ------------------------------------------------------------------

/// f: add a, remove a (a net-empty branch), rebase-merged by cherry-pick onto a
/// moved main. The tip's tree equals the fork's, and `unreachable_landing` takes
/// "adds nothing" to mean "not landed" before it ever asks `git cherry`.
#[test]
#[ignore = "adv: net-empty branch that was cherry-picked reads landed:false (no-content shortcut runs before cherry)"]
fn a_net_empty_branch_that_was_cherry_picked_is_not_false() {
    let r = R::new();
    r.git(&["checkout", "-q", "-b", "f"]);
    r.put("a.txt", "work\n", "add a");
    r.git(&["rm", "-q", "a.txt"]);
    r.git(&["commit", "-q", "-m", "remove a"]);
    let tip = r.git(&["rev-parse", "HEAD"]);
    r.git(&["checkout", "-q", "main"]);
    r.put("m.txt", "main moves\n", "main moves");
    r.git(&["cherry-pick", "f~1", "f"]);
    assert_eq!(r.git(&["cherry", "main", "f"]).matches('-').count(), 2);
    let a = r.land(&tip);
    assert_ne!(a.landed, Some(false), "{}", describe(&a));
}

/// An orphan commit whose tree equals the base tip's: unrelated histories, so
/// the patch search never runs, and absence is declared with no proof.
#[test]
#[ignore = "adv: unrelated history with the base's exact tree reads landed:false without proof"]
fn an_unrelated_history_holding_the_bases_exact_tree_is_not_false() {
    let r = R::new();
    r.put("b.txt", "two\n", "second");
    r.git(&["checkout", "-q", "--orphan", "o"]);
    r.git(&["commit", "-q", "-m", "imported snapshot"]);
    let tip = r.git(&["rev-parse", "HEAD"]);
    assert_eq!(
        r.git(&["rev-parse", "main^{tree}"]),
        r.git(&["rev-parse", "HEAD^{tree}"]),
        "the fixture must share the tree"
    );
    r.git(&["checkout", "-q", "main"]);
    let a = r.land(&tip);
    assert_ne!(a.landed, Some(false), "{}", describe(&a));
}

/// Without `--binary` a binary diff is one `Binary files ... differ` line, which
/// carries no content for `patch-id` to tell apart.
#[test]
fn an_unrelated_binary_change_on_the_base_is_not_a_squash_of_this_one() {
    let r = R::new();
    r.put("one.bin", "root\0one\n", "bin one");
    r.put("two.bin", "root\0two\n", "bin two");
    r.git(&["checkout", "-q", "-b", "f"]);
    let tip = r.put("one.bin", "from f\0one\n", "f edits one.bin");
    r.git(&["checkout", "-q", "main"]);
    r.put("two.bin", "main\0edit of two\n", "main edits two.bin");
    let a = r.land(&tip);
    assert_ne!(a.landed, Some(true), "{}", describe(&a));
}

/// The squash was edited on the way in (so no patch matches) and the file was
/// deleted later: merging adds the file back cleanly, which `tree_route` reads
/// as "absent, provably".
#[test]
#[ignore = "adv: an edited squash whose file was later deleted reads landed:false"]
fn an_edited_squash_whose_file_was_later_deleted_is_not_false() {
    let r = R::new();
    r.git(&["checkout", "-q", "-b", "f"]);
    let tip = r.put("a.txt", "work\n", "f adds a");
    r.git(&["checkout", "-q", "main"]);
    r.git(&["merge", "-q", "--squash", "f"]);
    std::fs::write(r.work().join("a.txt"), "work, fixed up\n").unwrap();
    r.git(&["add", "-A"]);
    r.git(&["commit", "-q", "-m", "squash f with a fixup"]);
    r.git(&["rm", "-q", "a.txt"]);
    r.git(&["commit", "-q", "-m", "remove the feature"]);
    let a = r.land(&tip);
    assert_ne!(a.landed, Some(false), "{}", describe(&a));
}

/// `git revert <c>` of a commit that came in through a `--no-ff` merge: the
/// revert names `c`, not the merge, and only the merge is searched.
#[test]
#[ignore = "adv: revert of the asked commit (not the merge commit) reads reverted:false"]
fn reverting_a_commit_of_a_merged_branch_is_found() {
    let r = R::new();
    r.git(&["checkout", "-q", "-b", "f"]);
    let c = r.put("a.txt", "work\n", "f adds a");
    r.git(&["checkout", "-q", "main"]);
    r.git(&["merge", "-q", "--no-ff", "-m", "merge f", "f"]);
    r.git(&["revert", "--no-edit", &c]);
    let a = r.land(&c);
    assert_eq!(a.via, Some(LandedVia::Merge), "{}", describe(&a));
    assert_ne!(a.reverted, Some(false), "{}", describe(&a));
}

/// A chain of five reverts (revert, reland, revert, reland, revert) leaves the
/// work reverted; the chain is followed four deep and the fifth is dropped.
#[test]
#[ignore = "adv: a revert chain longer than REVERT_CHAIN_LIMIT reads the parity of the cut chain"]
fn a_revert_chain_longer_than_the_limit_is_not_a_confident_not_reverted() {
    let r = R::new();
    r.git(&["checkout", "-q", "-b", "f"]);
    let c = r.put("a.txt", "work\n", "f adds a");
    r.git(&["checkout", "-q", "main"]);
    r.git(&["merge", "-q", "--ff-only", "f"]);
    let mut current = c.clone();
    for _ in 0..5 {
        r.git(&["revert", "--no-edit", &current]);
        current = r.git(&["rev-parse", "HEAD"]);
    }
    let a = r.land(&c);
    // Five reverts: the work is out. Honest answers are true or null.
    assert_ne!(a.reverted, Some(false), "{}", describe(&a));
}

fn many_commits_on_main(r: &R, count: usize) {
    let mut stream = String::new();
    for n in 0..count {
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter T <t@e.com> {} +0000\ndata 1\nx\n{}\n",
            1_700_000_000 + n,
            if n == 0 {
                "from refs/heads/main^0\n"
            } else {
                ""
            },
        ));
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(r.work())
        .stdin(Stdio::piped())
        .spawn_locked()
        .expect("fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    r.git(&["reset", "-q", "--hard", "main"]);
}

fn unlanded_branch_then_main_moves(count: usize) -> (R, String) {
    let r = R::new();
    r.git(&["checkout", "-q", "-b", "f"]);
    let tip = r.put("u.txt", "unlanded\n", "f adds u");
    r.git(&["checkout", "-q", "main"]);
    many_commits_on_main(&r, count);
    assert_eq!(
        r.git(&[
            "rev-list",
            "--count",
            "--no-merges",
            &format!("{tip}~1..main")
        ]),
        count.to_string()
    );
    (r, tip)
}

#[test]
fn a_base_of_exactly_1000_commits_since_the_fork_proves_absence() {
    let (r, tip) = unlanded_branch_then_main_moves(1_000);
    let a = r.land(&tip);
    assert_eq!(a.landed, Some(false), "{}", describe(&a));
}

#[test]
fn a_base_of_1001_commits_since_the_fork_is_search_depth_exceeded() {
    let (r, tip) = unlanded_branch_then_main_moves(1_001);
    let a = r.land(&tip);
    assert_eq!(a.landed, None, "{}", describe(&a));
    assert_eq!(a.reason, Some(LandedReason::SearchDepthExceeded));
}

#[test]
fn a_squash_at_exactly_the_1000th_commit_is_found_and_the_1001st_is_not_lost_to_false() {
    for (extra, expect_found) in [(999usize, true), (1_000, false)] {
        let r = R::new();
        r.git(&["checkout", "-q", "-b", "f"]);
        let tip = r.put("a.txt", "work\n", "f adds a");
        r.git(&["checkout", "-q", "main"]);
        r.git(&["merge", "-q", "--squash", "f"]);
        r.git(&["commit", "-q", "-m", "squash f"]);
        many_commits_on_main(&r, extra);
        let a = r.land(&tip);
        assert_eq!(a.landed, Some(true), "extra={extra}: {}", describe(&a));
        if expect_found {
            assert_eq!(a.via, Some(LandedVia::Squash), "{}", describe(&a));
        }
    }
}

#[test]
fn landed_at_follows_a_renamed_branch_and_never_head() {
    let r = R::new();
    r.git(&["checkout", "-q", "-b", "f"]);
    let c = r.put("a.txt", "work\n", "f adds a");
    r.git(&["checkout", "-q", "main"]);
    let merged = Command::new("git")
        .args(["merge", "-q", "--ff-only", "f"])
        .current_dir(r.work())
        .env("GIT_COMMITTER_DATE", "@1600000000 +0000")
        .output_locked()
        .expect("merge");
    assert!(merged.status.success());
    // Checkouts later: HEAD's reflog now has newer entries than the landing.
    r.git(&["checkout", "-q", "f"]);
    r.git(&["checkout", "-q", "main"]);
    r.git(&["branch", "-m", "main", "trunk"]);
    for base in [Some("trunk"), None] {
        let a = landed(&r.s(), &c, base);
        assert_eq!(a.landed, Some(true), "{}", describe(&a));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let at = a.landed_at.expect("a reflog places it");
        // The merge's reflog entry is stamped by the real clock, not the pinned
        // committer date; what must hold is that it is not a later checkout's
        // entry, which the two checkouts above (a second apart at most) would be.
        assert!(at <= now, "{at} is in the future");
        assert_eq!(
            a.landed_at_source,
            Some(crate::engine::provenance::LandedAtSource::Reflog)
        );
    }
}

#[test]
fn a_detached_head_base_places_by_commit_date_and_says_so() {
    let r = R::new();
    let c = r.put("a.txt", "work\n", "a");
    r.git(&["checkout", "-q", "--detach", "HEAD"]);
    let a = landed(&r.s(), &c, None);
    assert_eq!(a.landed, Some(true), "{}", describe(&a));
    assert_eq!(
        a.landed_at_source,
        Some(crate::engine::provenance::LandedAtSource::CommitDate)
    );
    assert!(!a.landed_at_reason.is_empty());
}

// --- 5. partial clones -------------------------------------------------------------

/// A repository whose `remote.origin` is a promisor (spelled by `config_text`,
/// appended verbatim to `.git/config`), whose remote is an `ext::` helper that
/// drops a marker file when git runs it, and whose `f` blob is missing. Returns
/// the repository, `f`'s tip and the marker path.
fn promisor_repo(config_text: &str) -> (R, String, PathBuf) {
    let r = R::new();
    r.git(&["checkout", "-q", "-b", "f"]);
    let tip = r.put("u.txt", "unlanded work\n", "f adds u");
    r.git(&["checkout", "-q", "main"]);
    r.put("m.txt", "main moves\n", "main moves");
    let marker = r.work().join("fetch-was-spawned");
    let script = r.path().join("helper.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let blob = r.git(&["rev-parse", &format!("{tip}:u.txt")]);
    let object = r
        .work()
        .join(".git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    std::fs::remove_file(&object).expect("the blob is a loose object");
    let config = r.work().join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[protocol \"ext\"]\n\tallow = always\n[remote \"origin\"]\n\turl = ext::{}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n{config_text}",
        script.display()
    ));
    std::fs::write(&config, text).unwrap();
    (r, tip, marker)
}

fn assert_refused_without_fetching(config_text: &str) {
    let (r, tip, marker) = promisor_repo(config_text);
    let a = r.land(&tip);
    assert!(
        !marker.exists(),
        "git ran the promisor remote's helper: {}",
        describe(&a)
    );
    assert_eq!(
        a.reason,
        Some(LandedReason::PartialCloneFetchRequired),
        "{}",
        describe(&a)
    );
}

#[test]
fn promisor_true_is_refused_without_a_fetch() {
    assert_refused_without_fetching("\tpromisor = true\n");
}

#[test]
fn extensions_partialclone_is_refused_without_a_fetch() {
    assert_refused_without_fetching(
        "\tpromisor = true\n[core]\n\trepositoryformatversion = 1\n[extensions]\n\tpartialclone = origin\n",
    );
}

#[test]
#[ignore = "adv: remote.<n>.promisor=yes is a promisor to git but not to is_partial_clone, which matches only the word true"]
fn promisor_yes_is_refused_without_a_fetch() {
    assert_refused_without_fetching("\tpromisor = yes\n");
}

#[test]
#[ignore = "adv: remote.<n>.promisor=1 is a promisor to git but not to is_partial_clone"]
fn promisor_one_is_refused_without_a_fetch() {
    assert_refused_without_fetching("\tpromisor = 1\n");
}

#[test]
#[ignore = "adv: remote.<n>.promisor=on is a promisor to git but not to is_partial_clone"]
fn promisor_on_is_refused_without_a_fetch() {
    assert_refused_without_fetching("\tpromisor = on\n");
}

#[test]
#[ignore = "adv: a bare `promisor` key (implicit true) is a promisor to git but not to is_partial_clone"]
fn a_bare_promisor_key_is_refused_without_a_fetch() {
    assert_refused_without_fetching("\tpromisor\n");
}

#[test]
fn a_promisor_in_the_common_config_is_refused_from_a_linked_worktree() {
    let (r, tip, marker) = promisor_repo("\tpromisor = true\n");
    let wt = tempfile::tempdir().expect("tempdir");
    let wt_path = wt.path().join("wt");
    r.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        wt_path.to_str().unwrap(),
        "main",
    ]);
    crate::test_support::trust_repo(&wt_path);
    let a = landed(wt_path.to_str().unwrap(), &tip, Some("main"));
    assert!(!marker.exists(), "{}", describe(&a));
    assert_eq!(a.reason, Some(LandedReason::PartialCloneFetchRequired));
}

#[test]
fn a_promisor_only_in_the_worktree_local_config_is_refused_from_that_worktree() {
    let (r, tip, marker) = promisor_repo("");
    r.git(&["config", "extensions.worktreeConfig", "true"]);
    let wt = tempfile::tempdir().expect("tempdir");
    let wt_path = wt.path().join("wt");
    r.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        wt_path.to_str().unwrap(),
        "main",
    ]);
    crate::test_support::trust_repo(&wt_path);
    let local = Command::new("git")
        .args(["config", "--worktree", "remote.origin.promisor", "true"])
        .current_dir(&wt_path)
        .output_locked()
        .expect("git config");
    assert!(local.status.success(), "{local:?}");
    let a = landed(wt_path.to_str().unwrap(), &tip, Some("main"));
    assert!(!marker.exists(), "{}", describe(&a));
    assert_eq!(a.reason, Some(LandedReason::PartialCloneFetchRequired));
}
