//! End-to-end regression for audit C1: resolving one conflict in a
//! mixed-EOL file must rewrite only the conflict hunk, never re-normalize
//! the whole document's line endings.

use gitpulse_lib::diff::{ConflictResolutionChoice, ConflictResolver, FileSegment};
use gitpulse_lib::engine::GitWriter;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

struct TestRepo {
    dir: TempDir,
}

impl TestRepo {
    fn init() -> Self {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
        // Windows installs git with core.autocrlf=true in its system config, so a
        // fixture repo inherits it and git rewrites LF to CRLF on checkout --
        // silently breaking every assertion below that compares exact bytes.
        // These tests own their line endings; pin the policy rather than
        // inherit the host's.
        run_git(dir.path(), &["config", "core.autocrlf", "false"]);
        Self { dir }
    }

    fn path_str(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    fn write(&self, rel: &str, content: &str) {
        fs::write(self.dir.path().join(rel), content).unwrap();
    }

    fn commit_all(&self, message: &str) {
        run_git(self.dir.path(), &["add", "-A"]);
        run_git(self.dir.path(), &["commit", "-m", message]);
    }

    fn git_out(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.dir.path())
            .env("GIT_AUTHOR_NAME", "Test User")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test User")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 30-line base file: every line LF except line 15, which is CRLF.
fn base_file() -> String {
    let mut out = String::new();
    for i in 1..=30 {
        if i == 15 {
            out.push_str(&format!("line-{i:02}\r\n"));
        } else {
            out.push_str(&format!("line-{i:02}\n"));
        }
    }
    out
}

fn with_line(content: &str, n: usize, replacement: &str) -> String {
    let mut out = String::new();
    for (idx, line) in content.split_terminator('\n').enumerate() {
        let num = idx + 1;
        if num == n {
            // Replacement keeps the original line's own terminator kind.
            if line.ends_with('\r') {
                out.push_str(replacement);
                out.push_str("\r\n");
            } else {
                out.push_str(replacement);
                out.push('\n');
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[test]
fn resolving_mixed_eol_conflict_rewrites_only_the_hunk_region() {
    let repo = TestRepo::init();
    repo.write("mixed.txt", &base_file());
    repo.commit_all("base: mixed eol file");
    let path = repo.path_str();

    GitWriter::create_branch(&path, "theirs", None).unwrap();
    GitWriter::checkout_branch(&path, "theirs").unwrap();
    repo.write("mixed.txt", &with_line(&base_file(), 8, "theirs-08"));
    repo.commit_all("feat: theirs edit");

    GitWriter::checkout_branch(&path, "main").unwrap();
    repo.write("mixed.txt", &with_line(&base_file(), 8, "ours-08"));
    repo.commit_all("feat: ours edit");

    let merge = GitWriter::merge_branch(&path, "theirs", false);
    let conflicted = merge.is_err() || {
        gitpulse_lib::engine::GitReader::get_status(&path)
            .unwrap()
            .iter()
            .any(|s| s.is_conflicted)
    };
    assert!(conflicted, "expected a conflicting merge");

    let worktree = fs::read_to_string(repo.dir.path().join("mixed.txt")).unwrap();
    assert!(worktree.contains("<<<<<<<"), "merge must leave markers");
    assert!(
        worktree.contains("line-15\r"),
        "conflicted worktree copy preserves the CRLF line"
    );

    let mut doc = ConflictResolver::parse("mixed.txt", &worktree);
    assert_eq!(doc.total_conflicts, 1);
    let conflict = doc
        .segments
        .iter_mut()
        .find(|seg| matches!(seg, FileSegment::Conflict(_)))
        .expect("conflict chunk must exist");
    if let FileSegment::Conflict(chunk) = conflict {
        chunk.resolution = ConflictResolutionChoice::Custom("resolved-08".to_string());
    }
    let resolved = ConflictResolver::render_resolved(&doc).unwrap();

    // Byte-exact expectation: identical to ours' version except line 8.
    let expected = with_line(&with_line(&base_file(), 8, "resolved-08"), 15, "line-15");
    assert_eq!(
        resolved, expected,
        "resolution must be surgical: only the conflict line changes"
    );
    assert!(resolved.contains("line-15\r\n"), "CRLF line survives");
    assert_eq!(resolved.matches("\r\n").count(), 1);

    fs::write(repo.dir.path().join("mixed.txt"), &resolved).unwrap();
    run_git(repo.dir.path(), &["add", "mixed.txt"]);

    // Index vs HEAD: exactly one line replaced — not a whole-file rewrite.
    let numstat = repo.git_out(&["diff", "--cached", "--numstat", "--", "mixed.txt"]);
    let mut parts = numstat.split_whitespace();
    let added: usize = parts.next().unwrap_or("999").parse().unwrap_or(999);
    let removed: usize = parts.next().unwrap_or("999").parse().unwrap_or(999);
    assert_eq!(
        (added, removed),
        (1, 1),
        "only the resolved hunk may differ from HEAD; got numstat: {numstat}"
    );
}
