//! Integration coverage for the repository operations module.
//!
//! `ops` decides three things a user acts on directly: which branches are safe
//! to delete, what is wrong with the commits about to be pushed, and whether a
//! release may be published. Each refuses in situations where proceeding would
//! be destructive, so the refusals matter as much as the happy paths — and the
//! module had no integration test at all, only inline unit tests.

use gitpulse_lib::ops::{
    branch_cleanup_plan, prepare_release, review_outgoing_commits, validate_release_tag,
};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be on PATH")
}

fn git_ok(dir: &Path, args: &[&str]) {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct TestRepo {
    dir: TempDir,
}

impl TestRepo {
    fn init() -> Self {
        let dir = TempDir::new().expect("tempdir");
        git_ok(dir.path(), &["init", "-b", "main"]);
        git_ok(dir.path(), &["config", "user.email", "test@example.com"]);
        git_ok(dir.path(), &["config", "user.name", "Test User"]);
        git_ok(dir.path(), &["config", "commit.gpgsign", "false"]);
        let repo = Self { dir };
        repo.commit("README.md", "hello\n", "initial commit");
        repo
    }

    fn path(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    fn commit(&self, file: &str, contents: &str, message: &str) {
        fs::write(self.dir.path().join(file), contents).expect("write");
        git_ok(self.dir.path(), &["add", "."]);
        git_ok(self.dir.path(), &["commit", "-m", message]);
    }
}

// ---------------------------------------------------------------- cleanup ---

#[test]
fn a_merged_branch_becomes_a_cleanup_candidate_and_the_current_one_never_does() {
    let repo = TestRepo::init();
    git_ok(repo.dir.path(), &["checkout", "-b", "feature/done"]);
    repo.commit("done.txt", "done\n", "feat: finish the thing");
    git_ok(repo.dir.path(), &["checkout", "main"]);
    git_ok(
        repo.dir.path(),
        &["merge", "--no-ff", "feature/done", "-m", "merge"],
    );

    let plan = branch_cleanup_plan(&repo.path()).expect("a plan");
    assert_eq!(plan.current_branch, "main");
    let names: Vec<&str> = plan.candidates.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"feature/done"),
        "merged branch should be offered: {names:?}"
    );
    // Deleting the checked-out branch is not something to propose.
    assert!(!names.contains(&"main"));
}

#[test]
fn an_unmerged_branch_is_counted_but_never_proposed_for_deletion() {
    let repo = TestRepo::init();
    git_ok(repo.dir.path(), &["checkout", "-b", "feature/wip"]);
    repo.commit("wip.txt", "wip\n", "wip: unfinished");
    git_ok(repo.dir.path(), &["checkout", "main"]);

    let plan = branch_cleanup_plan(&repo.path()).expect("a plan");
    let names: Vec<&str> = plan.candidates.iter().map(|c| c.name.as_str()).collect();
    assert!(
        !names.contains(&"feature/wip"),
        "proposing an unmerged branch would lose work: {names:?}"
    );
    assert!(plan.unmerged_branches >= 1, "it should still be counted");
    assert!(plan.total_local_branches >= 2);
}

#[test]
fn a_repository_with_only_the_default_branch_yields_no_candidates() {
    let repo = TestRepo::init();
    let plan = branch_cleanup_plan(&repo.path()).expect("a plan");
    assert!(plan.candidates.is_empty());
    assert_eq!(plan.total_local_branches, 1);
}

#[test]
fn cleanup_refuses_a_path_that_is_not_a_repository() {
    let plain = TempDir::new().expect("tempdir");
    assert!(branch_cleanup_plan(&plain.path().to_string_lossy()).is_err());
    assert!(branch_cleanup_plan("/nonexistent/repo").is_err());
}

// ----------------------------------------------------------------- review ---

#[test]
fn review_recognises_conventional_commits() {
    let repo = TestRepo::init();
    git_ok(repo.dir.path(), &["checkout", "-b", "feature/review"]);
    repo.commit("a.txt", "a\n", "feat(scope): add a thing");
    repo.commit("b.txt", "b\n", "fix: correct the other thing");

    let report = review_outgoing_commits(&repo.path()).expect("a report");
    assert!(report.reviewed_commits >= 2, "{report:?}");
    assert!(
        report.conventional_commits >= 2,
        "both messages are conventional: {report:?}"
    );
}

#[test]
fn review_flags_a_message_that_would_read_badly_in_history() {
    let repo = TestRepo::init();
    git_ok(repo.dir.path(), &["checkout", "-b", "feature/sloppy"]);
    // A bare subject with no type, no detail, and a trailing period is the
    // shape the reviewer exists to catch.
    repo.commit("c.txt", "c\n", "stuff.");

    let report = review_outgoing_commits(&repo.path()).expect("a report");
    assert!(
        !report.findings.is_empty(),
        "a low-quality subject should produce a finding: {report:?}"
    );
    // Every finding must be traceable to the commit it describes.
    for finding in &report.findings {
        assert!(!finding.commit_id.is_empty());
        assert!(!finding.short_id.is_empty());
        assert!(!finding.code.is_empty());
        assert!(!finding.detail.is_empty());
    }
}

#[test]
fn review_of_a_branch_with_nothing_outgoing_is_empty_not_an_error() {
    let repo = TestRepo::init();
    let report = review_outgoing_commits(&repo.path()).expect("a report");
    // With no upstream the range is the branch against itself, so there is
    // genuinely nothing outgoing. That is an empty report, not a failure — and
    // the range is stated so the emptiness is attributable rather than mysterious.
    assert_eq!(report.range, "main..HEAD");
    assert_eq!(report.total_commits, 0);
    assert_eq!(report.reviewed_commits, 0);
    assert_eq!(report.conventional_commits, 0);
    assert!(report.findings.is_empty());
    assert!(!report.truncated);
}

// ---------------------------------------------------------------- release ---

#[test]
fn release_tags_must_be_semver_with_a_v_prefix() {
    for good in [
        "v0.0.1",
        "v1.2.3",
        "v10.20.30",
        "v1.0.0-rc.1",
        "v1.0.0+build.5",
    ] {
        assert!(
            validate_release_tag(good).is_ok(),
            "{good} should be accepted"
        );
    }
    for bad in [
        "1.2.3",    // no v
        "v1.2",     // not three components
        "v01.2.3",  // leading zero
        "v1.2.3.4", // four components
        "vX.Y.Z",
        "--tag", // flag-shaped
        "v1.2.3 extra",
        "",
        "v1.2.3/../etc",
    ] {
        assert!(
            validate_release_tag(bad).is_err(),
            "{bad:?} should be rejected"
        );
    }
}

#[test]
fn a_release_is_refused_when_the_working_tree_is_dirty() {
    let repo = TestRepo::init();
    fs::write(repo.dir.path().join("dirty.txt"), "uncommitted\n").expect("write");

    let error = prepare_release(&repo.path(), "v1.0.0", "First release")
        .expect_err("a dirty tree must block a release");
    assert!(
        error.contains("clean working tree"),
        "the refusal should name the reason: {error}"
    );
}

#[test]
fn a_release_is_refused_from_a_non_default_branch() {
    let repo = TestRepo::init();
    git_ok(
        repo.dir.path(),
        &["checkout", "-b", "feature/release-attempt"],
    );

    let error = prepare_release(&repo.path(), "v1.0.0", "First release")
        .expect_err("only the default branch may publish");
    assert!(
        error.contains("default branch"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_release_is_refused_before_its_tag_or_message_is_even_checked_against_the_repo() {
    let repo = TestRepo::init();
    // Validation order matters: a malformed tag must be rejected on its own
    // terms rather than surfacing as some later repository-state complaint.
    let bad_tag = prepare_release(&repo.path(), "release-1", "message").expect_err("bad tag");
    assert!(bad_tag.contains("SemVer"), "unexpected error: {bad_tag}");

    let empty_message =
        prepare_release(&repo.path(), "v1.0.0", "   ").expect_err("an empty message");
    assert!(
        empty_message.contains("must not be empty"),
        "unexpected: {empty_message}"
    );
}

#[test]
fn a_release_message_is_bounded() {
    let repo = TestRepo::init();
    let huge = "x".repeat(10 * 1024 * 1024);
    let error = prepare_release(&repo.path(), "v1.0.0", &huge).expect_err("an unbounded message");
    assert!(error.contains("byte limit"), "unexpected error: {error}");
}
