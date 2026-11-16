//! The push gate against real repositories and a real bare remote: what a
//! push is refused for, what it is let through with, and that a scan which
//! could not read everything the push sends never reads as clean.

use super::super::staged::Policy;
use super::super::tests::{aws_key_id, github_pat};
use super::super::{scans_completed, SkipReason};
use super::{guard_push_within, resolve_push, scan_range, PushRefs, RangeLimits};
use crate::engine::git_writer::GitWriter;
use crate::ipc_error::IpcErrorKind;
use crate::procguard::LockedSpawn;
use crate::test_support::{git_in, git_repo, write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

/// A working repository with one pushed commit on `main`, and its bare
/// remote `origin`.
struct Pair {
    work: TempDir,
    origin: TempDir,
}

impl Pair {
    fn new() -> Self {
        let origin = TempDir::new().expect("temp dir");
        git_in(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
        let work = git_repo();
        for (key, value) in [
            ("user.name", "Push Scan Test"),
            ("user.email", "push@example.com"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ] {
            git_in(work.path(), &["config", key, value]);
        }
        git_in(
            work.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        let pair = Self { work, origin };
        pair.commit("README.md", "hello\n", "init");
        git_in(
            pair.work.path(),
            &["push", "-q", "-u", "origin", "HEAD:main"],
        );
        pair
    }

    fn path(&self) -> String {
        self.work.path().to_str().unwrap().to_string()
    }

    /// Commit `content` to `file` with plain git, which no gate stands in front of.
    fn commit(&self, file: &str, content: &str, message: &str) -> String {
        write(self.work.path(), file, content);
        git_in(self.work.path(), &["add", "--", file]);
        git_in(self.work.path(), &["commit", "-q", "-m", message]);
        self.head()
    }

    fn head(&self) -> String {
        out(self.work.path(), &["rev-parse", "HEAD"])
    }

    fn remote_main(&self) -> String {
        out(self.origin.path(), &["rev-parse", "refs/heads/main"])
    }
}

fn out(dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    crate::test_support::pin_git(&mut command);
    let output = command
        .args(args)
        .current_dir(dir)
        .output_locked()
        .expect("git runs");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn short(oid: &str) -> &str {
    &oid[..12]
}

const ENFORCE: Policy = Policy::Enforce;

fn scan_limits(commits: usize, patch_bytes: u64, time: Duration) -> RangeLimits {
    RangeLimits {
        commits,
        patch_bytes,
        time,
    }
}

#[test]
fn a_pushed_commit_carrying_a_key_is_refused_and_the_remote_is_unchanged() {
    let pair = Pair::new();
    let remote_before = pair.remote_main();
    let token = github_pat();
    let leaked = pair.commit(
        "ci/deploy.sh",
        &format!("#!/bin/sh\n\nexport GH={token}\n"),
        "ci: deploy",
    );
    pair.commit("notes.md", "nothing here\n", "docs");

    let err = GitWriter::push(&pair.path(), None, None, false).expect_err("refused");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    let at = format!("{}:ci/deploy.sh:3", short(&leaked));
    assert!(err.message().contains(&at), "{}", err.message());
    assert!(
        err.message()
            .starts_with("Push blocked by the secret scan."),
        "{}",
        err.message()
    );
    let ipc = crate::ipc_error::run_command(|| Err::<(), _>(err)).expect_err("refused");
    let wire = serde_json::to_string(&ipc).unwrap();
    assert!(!wire.contains(&token[..10]), "the token leaked: {wire}");
    let detail = ipc.detail.expect("detail");
    assert_eq!(detail["gate"], "push");
    assert_eq!(detail["hits_total"], 1);
    assert_eq!(
        pair.remote_main(),
        remote_before,
        "nothing reached the remote"
    );
}

#[test]
fn a_clean_range_pushes_and_the_scan_is_counted() {
    let pair = Pair::new();
    pair.commit("src/lib.rs", "pub fn one() -> u8 { 1 }\n", "feat: one");
    let tip = pair.commit("src/lib.rs", "pub fn one() -> u8 { 2 }\n", "fix: two");
    let before = scans_completed();
    GitWriter::push(&pair.path(), None, None, false).expect("a clean range pushes");
    assert!(scans_completed() > before, "the in-house scan ran");
    assert_eq!(pair.remote_main(), tip);
}

#[test]
fn removing_a_key_the_remote_already_has_is_allowed() {
    let pair = Pair::new();
    pair.commit("config.env", &format!("AWS={}\n", aws_key_id()), "oops");
    // Already on the remote: pushed with plain git, past any gate.
    git_in(pair.work.path(), &["push", "-q", "origin", "HEAD:main"]);
    let tip = pair.commit("config.env", "AWS=\n", "remove the key");
    GitWriter::push(&pair.path(), None, None, false)
        .expect("a push that only deletes a key the remote has is not refused");
    assert_eq!(pair.remote_main(), tip);
}

#[test]
fn a_key_added_and_removed_inside_the_unpushed_range_is_still_refused() {
    let pair = Pair::new();
    let added = pair.commit("config.env", &format!("AWS={}\n", aws_key_id()), "oops");
    pair.commit("config.env", "AWS=\n", "remove the key");
    let err = GitWriter::push(&pair.path(), None, None, false)
        .expect_err("the key is in a commit the push sends");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    assert!(
        err.message()
            .contains(&format!("{}:config.env:1", short(&added))),
        "{}",
        err.message()
    );
}

#[test]
fn a_missing_object_fails_closed() {
    let pair = Pair::new();
    let remote_before = pair.remote_main();
    pair.commit("data.txt", "an ordinary line\n", "data");
    let blob = out(pair.work.path(), &["rev-parse", "HEAD:data.txt"]);
    let object = pair
        .work
        .path()
        .join(".git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    std::fs::remove_file(&object).expect("the blob is a loose object");

    let err = GitWriter::push(&pair.path(), None, None, false).expect_err("fails closed");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);
    assert!(
        err.message().contains("could not be scanned"),
        "{}",
        err.message()
    );
    assert_eq!(pair.remote_main(), remote_before);
}

#[test]
fn every_bound_is_a_counted_skip_that_refuses() {
    let pair = Pair::new();
    for n in 0..3 {
        pair.commit("count.txt", &format!("{n}\n"), &format!("c{n}"));
    }
    let repo = pair.work.path();
    let generous = RangeLimits::DEFAULT;
    guard_push_within(repo, None, PushRefs::Branch(None), ENFORCE, generous)
        .expect("within the default limits the range is clean");

    for (limits, reason, skipped) in [
        (
            scan_limits(2, generous.patch_bytes, generous.time),
            SkipReason::CommitLimit,
            1,
        ),
        (
            scan_limits(generous.commits, generous.patch_bytes, Duration::ZERO),
            SkipReason::TimeLimit,
            3,
        ),
        (
            scan_limits(generous.commits, 0, generous.time),
            SkipReason::BudgetExhausted,
            3,
        ),
    ] {
        let range = resolve_push(repo, None, PushRefs::Branch(None)).expect("resolves");
        let scan = scan_range(repo, &range, limits).expect("a bound is a skip, not a failure");
        assert!(!scan.tally.is_clean(), "{reason:?}");
        assert_eq!(scan.tally.skipped_total, skipped, "{reason:?}");
        assert_eq!(scan.tally.skipped[0].reason, reason);
        let err = guard_push_within(repo, None, PushRefs::Branch(None), ENFORCE, limits)
            .expect_err("a skip refuses the push");
        assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked, "{reason:?}");
        assert!(
            err.message().contains(reason.describe()),
            "{}",
            err.message()
        );
    }
}

#[test]
fn a_new_branch_reads_only_what_the_remote_does_not_have() {
    let pair = Pair::new();
    // A key already published on main is not this push's to refuse.
    pair.commit("old.env", &format!("AWS={}\n", aws_key_id()), "old");
    git_in(pair.work.path(), &["push", "-q", "origin", "HEAD:main"]);
    git_in(pair.work.path(), &["checkout", "-q", "-b", "feature"]);
    let tip = pair.commit("feature.rs", "fn feature() {}\n", "feature");

    let range = resolve_push(
        pair.work.path(),
        Some("origin"),
        PushRefs::Branch(Some("feature")),
    )
    .expect("resolves");
    let scan = scan_range(pair.work.path(), &range, RangeLimits::DEFAULT).expect("scans");
    assert_eq!(scan.commits_scanned, 1, "only the branch's own commit");
    assert!(scan.tally.is_clean());

    // The unpublished branch is pushed as `push -u origin feature`.
    GitWriter::push(&pair.path(), None, None, false).expect("pushes");
    assert_eq!(
        out(pair.origin.path(), &["rev-parse", "refs/heads/feature"]),
        tip
    );
}

#[test]
fn a_merge_is_read_for_what_it_added_not_for_its_parents() {
    let pair = Pair::new();
    let base = pair.commit("app.cfg", "mode=a\n", "base");
    git_in(pair.work.path(), &["push", "-q", "origin", "HEAD:main"]);
    // Main moves on with a key, published.
    pair.commit(
        "vendor.env",
        &format!("AWS={}\n", aws_key_id()),
        "upstream key",
    );
    pair.commit("app.cfg", "mode=main\n", "main side");
    git_in(pair.work.path(), &["push", "-q", "origin", "HEAD:main"]);
    // A topic branch from before that merges main back in, resolving a conflict.
    git_in(pair.work.path(), &["checkout", "-q", "-b", "topic", &base]);
    pair.commit("app.cfg", "mode=topic\n", "topic side");
    let merged = Command::new("git")
        .args(["merge", "-q", "--no-edit", "main"])
        .current_dir(pair.work.path())
        .output_locked()
        .expect("git runs");
    assert!(!merged.status.success(), "the merge conflicts");

    // A clean resolution: main's published key is not read again.
    write(pair.work.path(), "app.cfg", "mode=both\n");
    git_in(pair.work.path(), &["add", "app.cfg"]);
    git_in(pair.work.path(), &["commit", "-q", "--no-edit"]);
    let range = resolve_push(
        pair.work.path(),
        Some("origin"),
        PushRefs::Branch(Some("topic")),
    )
    .expect("resolves");
    let scan = scan_range(pair.work.path(), &range, RangeLimits::DEFAULT).expect("scans");
    assert!(scan.tally.is_clean(), "{:?}", scan.tally);
    assert_eq!(scan.commits_scanned, 2, "the topic commit and the merge");

    // A resolution that slips a key in is refused, at the merge.
    let token = github_pat();
    write(
        pair.work.path(),
        "app.cfg",
        &format!("mode=both\ntoken={token}\n"),
    );
    git_in(pair.work.path(), &["add", "app.cfg"]);
    git_in(pair.work.path(), &["commit", "-q", "--amend", "--no-edit"]);
    let evil = pair.head();
    let err = guard_push_within(
        pair.work.path(),
        Some("origin"),
        PushRefs::Branch(Some("topic")),
        ENFORCE,
        RangeLimits::DEFAULT,
    )
    .expect_err("refused");
    assert!(
        err.message()
            .contains(&format!("{}:app.cfg:2", short(&evil))),
        "{}",
        err.message()
    );
}

#[test]
fn an_octopus_merge_is_a_skip_never_a_pass() {
    let pair = Pair::new();
    let base = pair.head();
    for side in ["p1", "p2"] {
        git_in(pair.work.path(), &["checkout", "-q", "-b", side, &base]);
        pair.commit(&format!("{side}.txt"), &format!("{side}\n"), side);
    }
    git_in(pair.work.path(), &["checkout", "-q", "main"]);
    pair.commit("m.txt", "m\n", "m");
    git_in(pair.work.path(), &["merge", "-q", "--no-edit", "p1", "p2"]);
    let octopus = pair.head();
    let range = resolve_push(pair.work.path(), None, PushRefs::Branch(None)).expect("resolves");
    let scan = scan_range(pair.work.path(), &range, RangeLimits::DEFAULT).expect("scans");
    assert_eq!(scan.tally.skipped_total, 1, "{:?}", scan.tally);
    assert_eq!(scan.tally.skipped[0].reason, SkipReason::Unsupported);
    assert_eq!(scan.tally.skipped[0].path, short(&octopus));
}

#[test]
fn a_binary_file_in_range_is_read_whole() {
    let pair = Pair::new();
    let mut bytes = vec![0u8, 1, 2, 3];
    bytes.extend_from_slice(format!("\nkey={}\n", aws_key_id()).as_bytes());
    std::fs::write(pair.work.path().join("blob.bin"), &bytes).unwrap();
    git_in(pair.work.path(), &["add", "blob.bin"]);
    git_in(pair.work.path(), &["commit", "-q", "-m", "binary"]);
    let tip = pair.head();
    let err = GitWriter::push(&pair.path(), None, None, false).expect_err("refused");
    assert!(
        err.message()
            .contains(&format!("{}:blob.bin:2", short(&tip))),
        "{}",
        err.message()
    );
}

#[test]
fn only_the_human_override_pushes_past_a_refusal() {
    let pair = Pair::new();
    let tip = pair.commit("k.env", &format!("AWS={}\n", aws_key_id()), "key");
    let args = GitWriter::plan_push(&pair.path(), None, None, false).unwrap();
    GitWriter::push_planned_with_policy(&pair.path(), &args, Policy::Enforce)
        .expect_err("enforced");
    GitWriter::push_planned_with_policy(&pair.path(), &args, Policy::HumanOverride)
        .expect("a person chose to push anyway");
    assert_eq!(pair.remote_main(), tip);
}

#[test]
fn a_tag_push_reads_the_commits_the_tag_sends() {
    let pair = Pair::new();
    let tagged = pair.commit("k.env", &format!("AWS={}\n", aws_key_id()), "key");
    git_in(pair.work.path(), &["tag", "-a", "v1", "-m", "v1"]);
    let err = GitWriter::push_tag(&pair.path(), "origin", "v1").expect_err("refused");
    assert!(
        err.message()
            .contains(&format!("{}:k.env:1", short(&tagged))),
        "{}",
        err.message()
    );
    assert!(out(pair.origin.path(), &["tag", "--list"]).is_empty());
}

#[test]
fn pushing_the_current_branch_does_not_read_other_branches_unless_git_would_send_them() {
    let pair = Pair::new();
    git_in(pair.work.path(), &["checkout", "-q", "-b", "side"]);
    pair.commit("k.env", &format!("AWS={}\n", aws_key_id()), "key on side");
    git_in(pair.work.path(), &["checkout", "-q", "main"]);
    pair.commit("ok.txt", "ok\n", "clean on main");
    guard_push_within(
        pair.work.path(),
        None,
        PushRefs::Branch(None),
        ENFORCE,
        RangeLimits::DEFAULT,
    )
    .expect("the side branch is not sent");
    // `matching` sends every branch the remote has a namesake for; the scan
    // reads every local branch, which is never less.
    git_in(pair.work.path(), &["config", "push.default", "matching"]);
    guard_push_within(
        pair.work.path(),
        None,
        PushRefs::Branch(None),
        ENFORCE,
        RangeLimits::DEFAULT,
    )
    .expect_err("matching may send the side branch");
}

#[test]
fn a_mirror_or_a_configured_refspec_is_read_beyond_branches_and_tags() {
    // A backup or snapshot ref is neither a branch nor a tag, and git pushes
    // it all the same under `--mirror` or a refspec such as `refs/*:refs/*`.
    let pair = Pair::new();
    git_in(pair.work.path(), &["checkout", "-q", "-b", "scratch"]);
    pair.commit("k.env", &format!("AWS={}\n", aws_key_id()), "key");
    git_in(
        pair.work.path(),
        &["update-ref", "refs/backup/keep", "HEAD"],
    );
    git_in(pair.work.path(), &["checkout", "-q", "main"]);
    git_in(pair.work.path(), &["branch", "-q", "-D", "scratch"]);
    let guard = || {
        guard_push_within(
            pair.work.path(),
            None,
            PushRefs::Branch(None),
            ENFORCE,
            RangeLimits::DEFAULT,
        )
    };
    guard().expect("a plain push sends only main");

    git_in(
        pair.work.path(),
        &["config", "remote.origin.mirror", "true"],
    );
    let sent = out(
        pair.work.path(),
        &["push", "--dry-run", "--porcelain", "origin"],
    );
    assert!(sent.contains("refs/backup/keep"), "git sends it: {sent}");
    let err = guard().expect_err("a mirror push sends the backup ref");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);

    git_in(
        pair.work.path(),
        &["config", "--unset", "remote.origin.mirror"],
    );
    git_in(
        pair.work.path(),
        &["config", "remote.origin.push", "refs/*:refs/*"],
    );
    let sent = out(
        pair.work.path(),
        &["push", "--dry-run", "--porcelain", "origin"],
    );
    assert!(sent.contains("refs/backup/keep"), "git sends it: {sent}");
    let err = guard().expect_err("refs/*:refs/* sends the backup ref");
    assert_eq!(err.kind(), IpcErrorKind::SecretScanBlocked);

    // A refspec that names one branch reads that branch, not every ref: the
    // snapshot refs GitPulse keeps must not block a push that leaves them.
    git_in(
        pair.work.path(),
        &[
            "config",
            "--replace-all",
            "remote.origin.push",
            "refs/heads/main:refs/heads/main",
        ],
    );
    let sent = out(
        pair.work.path(),
        &["push", "--dry-run", "--porcelain", "origin"],
    );
    assert!(!sent.contains("refs/backup/keep"), "git leaves it: {sent}");
    guard().expect("a one-branch refspec does not send the backup ref");
}

#[test]
fn a_remote_nested_under_another_name_does_not_lend_it_its_refs() {
    let pair = Pair::new();
    let fork = TempDir::new().unwrap();
    git_in(fork.path(), &["init", "-q", "--bare", "-b", "main"]);
    // `git remote add` refuses a name nested under another remote's, but the
    // configuration can still hold one (written by hand, or by an older git).
    git_in(
        pair.work.path(),
        &[
            "config",
            "remote.origin/fork.url",
            fork.path().to_str().unwrap(),
        ],
    );
    git_in(
        pair.work.path(),
        &[
            "config",
            "remote.origin/fork.fetch",
            "+refs/heads/*:refs/remotes/origin/fork/*",
        ],
    );
    pair.commit("k.env", &format!("AWS={}\n", aws_key_id()), "key");
    // The key's commit is on `origin/fork`, not on `origin`.
    git_in(
        pair.work.path(),
        &["push", "-q", "origin/fork", "HEAD:main"],
    );
    git_in(pair.work.path(), &["fetch", "-q", "origin/fork"]);
    guard_push_within(
        pair.work.path(),
        Some("origin"),
        PushRefs::Branch(Some("main")),
        ENFORCE,
        RangeLimits::DEFAULT,
    )
    .expect_err("origin does not have what origin/fork has");
}

#[test]
fn the_users_diff_and_log_configuration_cannot_hide_a_key() {
    let pair = Pair::new();
    for (key, value) in [
        ("diff.external", "false"),
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.renames", "copies"),
        ("diff.suppressBlankEmpty", "true"),
        ("diff.interHunkContext", "10"),
        ("diff.context", "8"),
        ("diff.relative", "true"),
        ("color.ui", "always"),
        ("color.diff", "always"),
        ("log.showRoot", "false"),
        ("log.showSignature", "true"),
        ("log.diffMerges", "first-parent"),
        ("core.quotePath", "true"),
    ] {
        git_in(pair.work.path(), &["config", key, value]);
    }
    std::fs::write(pair.work.path().join(".gitattributes"), "*.env diff=hide\n").unwrap();
    git_in(pair.work.path(), &["config", "diff.hide.textconv", "true"]);
    // Blank lines between nearby edits: merged hunks would carry them as
    // context, and `suppressBlankEmpty` would print them as empty lines.
    let lines: String = (0..30).map(|n| format!("line {n}\n\n")).collect();
    pair.commit("big.txt", &lines, "spacing");
    let changed = lines
        .replace("line 7\n", &format!("line 7 AWS={}\n", aws_key_id()))
        .replace("line 9\n", "line nine\n");
    write(pair.work.path(), "big.txt", &changed);
    // A textconv filter that prints nothing, on a path git has to quote.
    write(
        pair.work.path(),
        "s p a c e.env",
        &format!("GH={}\n", github_pat()),
    );
    git_in(pair.work.path(), &["add", "-A"]);
    git_in(pair.work.path(), &["commit", "-q", "-m", "hide"]);
    let err = guard_push_within(
        pair.work.path(),
        None,
        PushRefs::Branch(None),
        ENFORCE,
        RangeLimits::DEFAULT,
    )
    .expect_err("the keys are found whatever the configuration says");
    assert_eq!(
        err.kind(),
        IpcErrorKind::SecretScanBlocked,
        "{}",
        err.message()
    );
    assert!(err.message().contains("big.txt:15"), "{}", err.message());
    assert!(
        err.message().contains("s p a c e.env:1"),
        "{}",
        err.message()
    );
}

#[test]
fn a_merge_conflict_note_with_no_change_pairs_and_scans_clean() {
    let pair = Pair::new();
    let base = pair.commit("keep.txt", "v1\n", "base");
    git_in(pair.work.path(), &["push", "-q", "origin", "HEAD:main"]);
    git_in(pair.work.path(), &["rm", "-q", "keep.txt"]);
    git_in(pair.work.path(), &["commit", "-q", "-m", "main deletes"]);
    git_in(pair.work.path(), &["push", "-q", "origin", "HEAD:main"]);
    git_in(pair.work.path(), &["checkout", "-q", "-b", "topic", &base]);
    pair.commit("keep.txt", "v2\n", "topic modifies");
    let merged = Command::new("git")
        .args(["merge", "-q", "--no-edit", "main"])
        .current_dir(pair.work.path())
        .output_locked()
        .expect("git runs");
    assert!(!merged.status.success(), "modify/delete conflicts");
    // Keep the modified file: the merge records no change to it, and
    // `--remerge-diff` prints only its conflict note, with no `--raw` entry.
    git_in(pair.work.path(), &["add", "keep.txt"]);
    git_in(pair.work.path(), &["commit", "-q", "--no-edit"]);
    let range = resolve_push(
        pair.work.path(),
        Some("origin"),
        PushRefs::Branch(Some("topic")),
    )
    .expect("resolves");
    let scan = scan_range(pair.work.path(), &range, RangeLimits::DEFAULT).expect("scans");
    assert!(scan.tally.is_clean(), "{:?}", scan.tally);
    assert_eq!(scan.commits_scanned, 2, "the topic commit and the merge");
}
