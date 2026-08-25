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
