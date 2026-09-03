//! Payload budgets under repository-scale content.
//!
//! Regression: a single commit that rewrote a 400k-line file produced a
//! 43.7 MB diff. Measured end to end that cost ~144 MB of RSS in this process
//! (bytes + lossy `String` + JSON) and ~330 MB in the webview (string +
//! 533k parsed row objects) — ~475 MB for one click, on a client whose whole
//! point is being lightweight, and the viewer renders at most 300k rows of it
//! anyway. These tests build that repository for real and pin the bound.

use gitpulse_lib::engine::budget;
use gitpulse_lib::engine::GitReader;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository whose HEAD commit rewrites `lines` lines of `width` chars.
fn huge_diff_repo(lines: usize, width: usize) -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "t@t.t"]);
    git(path, &["config", "user.name", "t"]);

    let row: String = "a".repeat(width);
    let mut body = String::with_capacity(lines * (width + 1));
    for _ in 0..lines {
        body.push_str(&row);
        body.push('\n');
    }
    std::fs::write(path.join("big.txt"), &body).expect("write");
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "initial"]);

    let row2: String = "b".repeat(width);
    let mut body2 = String::with_capacity(lines * (width + 1));
    for _ in 0..lines {
        body2.push_str(&row2);
        body2.push('\n');
    }
    std::fs::write(path.join("big.txt"), &body2).expect("write");
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "rewrite"]);

    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .expect("rev-parse");
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (dir, oid)
}

#[test]
fn a_commit_diff_far_over_budget_is_capped_and_says_so() {
    // ~120k lines x 120 chars on each side of the rewrite = ~29 MB of diff,
    // comfortably past the 8 MiB budget.
    let (dir, oid) = huge_diff_repo(120_000, 120);
    let repo = dir.path().to_str().expect("utf8 path");

    let payload = GitReader::get_commit_diff(repo, &oid).expect("commit diff");

    assert!(
        payload.truncated,
        "a diff this size must be reported as truncated"
    );
    assert!(
        payload.text.len() <= budget::MAX_DIFF_BYTES,
        "payload of {} bytes exceeded the {} byte budget",
        payload.text.len(),
        budget::MAX_DIFF_BYTES
    );
    // Still useful: the head of the diff is real, parseable diff text.
    assert!(
        payload.text.starts_with("diff --git "),
        "the surviving head must still be a diff"
    );
    assert!(
        payload.text.ends_with('\n'),
        "the cut must land on a line boundary, not mid-row"
    );
    assert!(
        !payload.text.contains('\u{FFFD}'),
        "a mid-character cut must not leave a replacement char in the payload"
    );
}

#[test]
fn a_file_diff_far_over_budget_is_capped_and_says_so() {
    let (dir, _) = huge_diff_repo(120_000, 120);
    let repo = dir.path().to_str().expect("utf8 path");
    // Dirty the working tree so `git diff` (not `show`) produces the volume.
    let row: String = "c".repeat(120);
    let mut body = String::new();
    for _ in 0..120_000 {
        body.push_str(&row);
        body.push('\n');
    }
    std::fs::write(dir.path().join("big.txt"), &body).expect("write");

    let payload = GitReader::get_file_diff(repo, "big.txt", false, false).expect("file diff");
    assert!(payload.truncated, "an oversize file diff must say so");
    assert!(payload.text.len() <= budget::MAX_DIFF_BYTES);
    assert!(payload.text.ends_with('\n'));
}

/// The other half of the contract: an ordinary diff must be delivered whole,
/// with `truncated` false. A budget that quietly clipped normal work would be
/// worse than the bug it fixes.
#[test]
fn an_ordinary_diff_is_delivered_complete_and_unflagged() {
    let (dir, oid) = huge_diff_repo(200, 40);
    let repo = dir.path().to_str().expect("utf8 path");

    let payload = GitReader::get_commit_diff(repo, &oid).expect("commit diff");
    assert!(
        !payload.truncated,
        "a 200-line diff is nowhere near the budget"
    );
    assert!(payload.text.contains("-aaa"), "removals must survive");
    assert!(payload.text.contains("+bbb"), "additions must survive");
    // Every line git emitted is present: count the +/- rows.
    let adds = payload
        .text
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    assert_eq!(adds, 200, "a complete diff must keep every added line");
}

/// Blame on a large file goes through the same discipline: `--line-porcelain`
/// emits ~10 metadata lines per source line, so it outgrows its file fast.
#[test]
fn blame_on_a_large_file_stays_within_its_budget() {
    let (dir, _) = huge_diff_repo(60_000, 120);
    let repo = dir.path().to_str().expect("utf8 path");

    let lines = GitReader::get_file_blame(repo, "big.txt").expect("blame");
    // Bounded by the porcelain budget rather than by the file's line count.
    let bytes: usize = lines.iter().map(|l| l.content.len() + 96).sum();
    assert!(
        bytes <= budget::MAX_BLAME_BYTES,
        "blame payload of ~{bytes} bytes exceeded the {} byte budget",
        budget::MAX_BLAME_BYTES
    );
    assert!(!lines.is_empty(), "blame must still return what it read");
    // No fabricated record from a half-parsed porcelain block.
    assert!(
        lines.iter().all(|l| l.commit_id.len() == 40),
        "every surviving blame line must carry a whole commit id"
    );
}
