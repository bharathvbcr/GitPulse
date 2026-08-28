use gitpulse_lib::engine::GitReader;
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
        Self { dir }
    }

    fn path_str(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    fn write(&self, rel: &str, content: &str) {
        let dest = self.dir.path().join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(dest, content).unwrap();
    }

    fn commit_all(&self, message: &str) {
        run_git(self.dir.path(), &["add", "-A"]);
        run_git(self.dir.path(), &["commit", "-m", message]);
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

fn staged_rename_entry(repo: &TestRepo) -> gitpulse_lib::engine::FileStatus {
    let statuses = GitReader::get_status(&repo.path_str()).expect("get_status");
    assert_eq!(statuses.len(), 1, "exactly one status entry: {statuses:?}");
    statuses.into_iter().next().unwrap()
}

#[test]
fn staged_simple_rename_reports_new_path_and_original_old_path() {
    let repo = TestRepo::init();
    repo.write("file.txt", "one\ntwo\n");
    repo.commit_all("chore: seed");
    run_git(repo.dir.path(), &["mv", "file.txt", "renamed.txt"]);

    let entry = staged_rename_entry(&repo);
    assert_eq!(entry.status_code, "R ");
    assert!(entry.is_staged);
    assert_eq!(entry.path, "renamed.txt", "first porcelain field is NEW");
    assert_eq!(
        entry.old_path.as_deref(),
        Some("file.txt"),
        "second porcelain field is OLD"
    );
}

#[test]
fn staged_rename_into_directory_keeps_both_sides() {
    let repo = TestRepo::init();
    repo.write("docs/guide.md", "text\n");
    repo.commit_all("chore: seed");
    fs::create_dir_all(repo.dir.path().join("src")).unwrap();
    run_git(repo.dir.path(), &["mv", "docs/guide.md", "src/guide.md"]);

    let entry = staged_rename_entry(&repo);
    assert_eq!(entry.path, "src/guide.md");
    assert_eq!(entry.old_path.as_deref(), Some("docs/guide.md"));
}

#[test]
fn staged_unicode_rename_is_unquoted_in_z_mode() {
    let repo = TestRepo::init();
    repo.write("uni/résumé.txt", "contenu\n");
    repo.commit_all("chore: seed");
    run_git(repo.dir.path(), &["mv", "uni/résumé.txt", "uni/履歴書.txt"]);

    let entry = staged_rename_entry(&repo);
    assert_eq!(entry.path, "uni/履歴書.txt");
    assert_eq!(entry.old_path.as_deref(), Some("uni/résumé.txt"));
}

#[test]
fn staged_copy_reports_source_as_old_path() {
    let repo = TestRepo::init();
    repo.write("orig.txt", "same\nlines\n");
    repo.commit_all("chore: seed");
    // A copy is only reported when the source itself changed in the same
    // change set; status.renames=copies turns copy detection on.
    run_git(repo.dir.path(), &["config", "diff.renames", "copies"]);
    run_git(repo.dir.path(), &["config", "status.renames", "copies"]);
    fs::copy(
        repo.dir.path().join("orig.txt"),
        repo.dir.path().join("dup.txt"),
    )
    .unwrap();
    repo.write("orig.txt", "same\nlines\nplus one\n");
    run_git(repo.dir.path(), &["add", "-A"]);

    let statuses = GitReader::get_status(&repo.path_str()).expect("get_status");
    let copy = statuses
        .iter()
        .find(|s| s.status_code.starts_with('C'))
        .expect("copy entry");
    assert_eq!(copy.path, "dup.txt");
    assert_eq!(copy.old_path.as_deref(), Some("orig.txt"));
}

#[test]
fn staged_brace_rename_additions_resolve_through_numstat_new_path() {
    let repo = TestRepo::init();
    repo.write("v1/app.txt", "a\nb\n");
    repo.commit_all("chore: seed");
    repo.write("v1/app.txt", "a\nb\nc\nd\n");
    run_git(repo.dir.path(), &["mv", "v1/app.txt", "v1/renamed.txt"]);
    run_git(repo.dir.path(), &["add", "-A"]);

    let entry = staged_rename_entry(&repo);
    assert_eq!(entry.path, "v1/renamed.txt");
    // The cached numstat key is the brace form `v1/{app.txt => renamed.txt}`;
    // it must resolve to this entry instead of degrading to (0, 0).
    assert_eq!((entry.additions, entry.deletions), (2, 0));
}

#[test]
fn commit_file_list_reassembles_brace_rename_paths() {
    let repo = TestRepo::init();
    repo.write("src/sub/f.rs", "fn a() {}\nfn b() {}\n");
    repo.commit_all("chore: seed");
    fs::create_dir_all(repo.dir.path().join("src/other")).unwrap();
    repo.write("src/sub/f.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
    run_git(repo.dir.path(), &["mv", "src/sub/f.rs", "src/other/f.rs"]);
    repo.commit_all("feat: move f.rs");

    let head = GitReader::head_id(&repo.path_str()).expect("head");
    let details = GitReader::get_commit_details(&repo.path_str(), &head).expect("details");
    assert_eq!(details.changed_files.len(), 1);
    let file = &details.changed_files[0];
    assert_eq!(file.path, "src/other/f.rs", "brace form must reassemble");
    assert_eq!(file.status_code, "R");
    assert!(!file.path.contains('{') && !file.path.contains('}'));
    assert_eq!((file.additions, file.deletions), (1, 0));

    let files = GitReader::get_commit_files(&repo.path_str(), &head).expect("files");
    assert_eq!(files[0].path, "src/other/f.rs");

    let details_again =
        GitReader::get_commit_details(&repo.path_str(), &head).expect("details again");
    assert_eq!(details_again.total_additions, 1);
}

#[test]
fn untracked_files_report_actual_line_counts_as_additions() {
    let repo = TestRepo::init();
    repo.write("tracked.txt", "seed\n");
    repo.commit_all("chore: seed");

    // Add untracked text file with 5 lines
    repo.write("untracked.txt", "line1\nline2\nline3\nline4\nline5\n");
    // Add untracked text file in a nested subdirectory without trailing newline (3 lines)
    repo.write(
        "nested/deep/file.ts",
        "const a = 1;\nconst b = 2;\nexport { a, b };",
    );

    let statuses = GitReader::get_status(&repo.path_str()).expect("get_status");
    let u1 = statuses
        .iter()
        .find(|s| s.path == "untracked.txt")
        .expect("untracked.txt entry");
    assert_eq!(u1.status_code, "??");
    assert_eq!(
        u1.additions, 5,
        "untracked file must count its 5 added lines"
    );
    assert_eq!(u1.deletions, 0);

    let u2 = statuses
        .iter()
        .find(|s| s.path == "nested/deep/file.ts")
        .expect("nested/deep/file.ts entry");
    assert_eq!(u2.status_code, "??");
    assert_eq!(
        u2.additions, 3,
        "untracked nested file must count its 3 added lines"
    );
    assert_eq!(u2.deletions, 0);
}

#[test]
fn partially_staged_mm_file_combines_staged_and_unstaged_churn() {
    let repo = TestRepo::init();
    repo.write("file.txt", "line1\nline2\nline3\n");
    repo.commit_all("chore: seed");

    // 1. Stage an addition of 2 lines
    repo.write("file.txt", "line1\nline2\nline3\nstaged1\nstaged2\n");
    run_git(repo.dir.path(), &["add", "file.txt"]);

    // 2. Add 3 more unstaged lines in working tree
    repo.write(
        "file.txt",
        "line1\nline2\nline3\nstaged1\nstaged2\nwork1\nwork2\nwork3\n",
    );

    let statuses = GitReader::get_status(&repo.path_str()).expect("get_status");
    let entry = statuses
        .iter()
        .find(|s| s.path == "file.txt")
        .expect("file.txt entry");
    assert_eq!(entry.status_code, "MM");
    assert!(entry.is_staged);
    // Staged: 2 additions. Unstaged: 3 additions. Total = 5 additions!
    assert_eq!(
        (entry.additions, entry.deletions),
        (5, 0),
        "MM file must sum both staged (2) and unstaged (3) additions"
    );
}

#[test]
fn partially_staged_am_file_combines_staged_addition_and_worktree_churn() {
    let repo = TestRepo::init();
    repo.write("initial.txt", "init\n");
    repo.commit_all("chore: seed");

    // 1. Stage a new file with 10 lines
    let staged_content = (1..=10)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    repo.write("brand_new.txt", &staged_content);
    run_git(repo.dir.path(), &["add", "brand_new.txt"]);

    // 2. Modify in working tree: append 4 more lines
    let work_content = staged_content + "work 1\nwork 2\nwork 3\nwork 4\n";
    repo.write("brand_new.txt", &work_content);

    let statuses = GitReader::get_status(&repo.path_str()).expect("get_status");
    let entry = statuses
        .iter()
        .find(|s| s.path == "brand_new.txt")
        .expect("brand_new.txt entry");
    assert_eq!(entry.status_code, "AM");
    assert!(entry.is_staged);
    // Staged: 10 additions. Unstaged: 4 additions. Total = 14 additions!
    assert_eq!(
        (entry.additions, entry.deletions),
        (14, 0),
        "AM file must sum staged 10 additions + worktree 4 additions"
    );
}

#[test]
fn untracked_binary_and_empty_files_report_zero_churn() {
    let repo = TestRepo::init();
    repo.write("tracked.txt", "seed\n");
    repo.commit_all("chore: seed");

    // Empty file
    repo.write("empty.txt", "");
    // Binary file with null bytes
    let bin_path = repo.dir.path().join("image.png");
    fs::write(bin_path, [0x89, 0x50, 0x4E, 0x47, 0x00, 0x00, 0x00, 0x0D]).unwrap();

    let statuses = GitReader::get_status(&repo.path_str()).expect("get_status");
    let empty = statuses
        .iter()
        .find(|s| s.path == "empty.txt")
        .expect("empty.txt");
    assert_eq!(empty.status_code, "??");
    assert_eq!((empty.additions, empty.deletions), (0, 0));

    let bin = statuses
        .iter()
        .find(|s| s.path == "image.png")
        .expect("image.png");
    assert_eq!(bin.status_code, "??");
    assert_eq!((bin.additions, bin.deletions), (0, 0));
}

#[test]
fn untracked_crlf_and_no_newline_files_count_accurately() {
    let repo = TestRepo::init();
    repo.write("tracked.txt", "seed\n");
    repo.commit_all("chore: seed");

    // CRLF line endings (4 lines)
    repo.write("crlf.txt", "line1\r\nline2\r\nline3\r\nline4\r\n");
    // Single line without trailing newline (1 line)
    repo.write("single.txt", "just one line");
    // Mixed line endings without trailing newline (3 lines)
    repo.write("mixed.txt", "one\r\ntwo\nthree");

    let statuses = GitReader::get_status(&repo.path_str()).expect("get_status");
    let crlf = statuses.iter().find(|s| s.path == "crlf.txt").unwrap();
    assert_eq!(crlf.additions, 4);

    let single = statuses.iter().find(|s| s.path == "single.txt").unwrap();
    assert_eq!(single.additions, 1);

    let mixed = statuses.iter().find(|s| s.path == "mixed.txt").unwrap();
    assert_eq!(mixed.additions, 3);
}

#[test]
fn stress_test_many_untracked_and_modified_files_with_concurrency() {
    use std::sync::Arc;
    use std::thread;

    let repo = Arc::new(TestRepo::init());
    repo.write("tracked_0.txt", "zero\n");
    repo.write("tracked_1.txt", "one\n");
    repo.commit_all("chore: initial");

    // Create 100 untracked files across nested subdirectories
    for i in 0..100 {
        let content = (0..10)
            .map(|l| format!("row {l} in file {i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        repo.write(&format!("nested/sub_{}/file_{}.rs", i % 10, i), &content);
    }

    // Modify existing tracked files (one staged, one unstaged)
    repo.write("tracked_0.txt", "zero\nstaged_line_1\nstaged_line_2\n");
    run_git(repo.dir.path(), &["add", "tracked_0.txt"]);
    repo.write(
        "tracked_1.txt",
        "one\nunstaged_line_1\nunstaged_line_2\nunstaged_line_3\n",
    );

    // Spawn 8 concurrent threads calling get_status simultaneously
    let mut handles = Vec::new();
    for _ in 0..8 {
        let repo_clone = Arc::clone(&repo);
        handles.push(thread::spawn(move || {
            let statuses = GitReader::get_status(&repo_clone.path_str()).expect("status");
            assert_eq!(statuses.len(), 102); // 100 untracked + 2 modified

            let t0 = statuses.iter().find(|s| s.path == "tracked_0.txt").unwrap();
            assert_eq!(t0.additions, 2);
            assert_eq!(t0.status_code, "M ");

            let t1 = statuses.iter().find(|s| s.path == "tracked_1.txt").unwrap();
            assert_eq!(t1.additions, 3);
            assert_eq!(t1.status_code, " M");

            let total_additions: usize = statuses.iter().map(|s| s.additions).sum();
            // 100 untracked * 10 lines + 2 staged + 3 unstaged = 1005 total additions!
            assert_eq!(total_additions, 1005);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}
