use gitpulse_lib::analyzer::CoverageScanner;
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

#[test]
fn untracked_rust_is_detected() {
    let repo = TestRepo::init();
    repo.write("src/lib.rs", "pub fn ready() {}\n");
    let stats = GitReader::get_repo_language_stats(&repo.path_str()).expect("stats");
    let rust = stats
        .iter()
        .find(|s| s.language == "Rust")
        .expect("untracked .rs must appear");
    assert_eq!(rust.category, "programming");
    assert!(rust.file_count >= 1);
    assert!(rust.code_lines >= 1);
}

#[test]
fn tauri_layout_detects_rust_despite_lockfiles_and_frontend() {
    let repo = TestRepo::init();
    repo.write(
        "package-lock.json",
        &format!("{}\n", "{\n  \"name\": \"x\"\n}".repeat(400)),
    );
    repo.write("README.md", &"# docs\n".repeat(200));
    repo.write("src/App.svelte", "<script>let x = 1;</script>\n");
    repo.write("src/main.ts", "export const n = 1;\n");
    repo.write(
        "src-tauri/src/lib.rs",
        "pub fn native() {\n    let _x = 1;\n}\n",
    );
    repo.write(
        "src-tauri/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    repo.write(
        "src-tauri/Cargo.lock",
        "# This file is automatically @generated\n[[package]]\nname = \"foo\"\n",
    );
    repo.commit_all("chore: mixed tauri tree");

    let stats = GitReader::get_repo_language_stats(&repo.path_str()).expect("stats");
    assert!(
        stats
            .iter()
            .any(|s| s.language == "Rust" && s.category == "programming"),
        "rust missing from {:?}: {:?}",
        stats
            .iter()
            .map(|s| (s.language.as_str(), s.category.as_str(), s.code_lines))
            .collect::<Vec<_>>(),
        stats
    );
    assert!(
        !stats
            .iter()
            .any(|s| s.language == "Rust" && s.file_count == 0),
        "empty rust bucket: {stats:?}"
    );
    assert!(
        !stats
            .iter()
            .any(|s| s.language == "JSON" && s.code_lines > 1000),
        "lockfile JSON must not dominate: {stats:?}"
    );
}

#[test]
fn coverage_family_from_untracked_rust_and_cargo_toml() {
    let repo = TestRepo::init();
    repo.write(
        "Cargo.toml",
        "[package]\nname = \"x\"\nversion = \"0.0.1\"\n",
    );
    let report = CoverageScanner::scan(&repo.path_str()).expect("scan");
    assert!(
        report.families.iter().any(|f| f.family == "rust"),
        "Cargo.toml must seed rust: {:?}",
        report.families
    );
}

#[test]
fn rust_survives_many_earlier_prose_files() {
    let repo = TestRepo::init();
    for i in 0..80 {
        repo.write(&format!("docs/{i:03}.md"), "# note\n");
        repo.write(&format!("data/{i:03}.json"), "{\"k\":1}\n");
    }
    repo.write("zzz-backend/src/lib.rs", "pub fn late() { let y = 2; }\n");
    repo.commit_all("chore: drown then rust");
    let stats = GitReader::get_repo_language_stats(&repo.path_str()).expect("stats");
    assert!(
        stats.iter().any(|s| s.language == "Rust"),
        "late rust path dropped: {:?}",
        stats
            .iter()
            .map(|s| s.language.as_str())
            .collect::<Vec<_>>()
    );
}
