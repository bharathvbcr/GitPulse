use gitpulse_lib::analyzer::CoverageScanner;
use gitpulse_lib::diff::PatchBuilder;
use gitpulse_lib::diff::{DiffLineType, FilePatch, UnifiedDiffHunk, UnifiedDiffLine};
use gitpulse_lib::engine::git_cli::{resolve_git_dir, sandbox_join, sandbox_write, validate_repo};
use gitpulse_lib::engine::{GitReader, GitWriter, RebaseActionKind, RebaseStep};
use gitpulse_lib::github::parse_github_remote_url;
use std::fs;
use std::path::{Path, PathBuf};
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
fn test_open_repo_history_status_and_details() {
    let repo = TestRepo::init();
    repo.write("src/lib.rs", "fn first() {}\n");
    repo.commit_all("feat: initial library");
    repo.write("src/lib.rs", "fn first() {}\nfn second() {}\n");
    repo.commit_all("feat: add second function");

    let path = repo.path_str();
    let history = GitReader::read_commit_history(&path, 50, None).expect("history");
    assert_eq!(history.len(), 2);
    assert!(history[0].summary.contains("second"));
    assert_eq!(history[0].author_name, "Test User");

    let branches = GitReader::list_branches(&path).expect("branches");
    assert!(branches
        .iter()
        .any(|b| b.is_current && (b.name == "main" || b.name == "master")));

    let files = GitReader::get_commit_files(&path, &history[0].id).expect("files");
    assert!(files.iter().any(|f| f.path.contains("lib.rs")));
    assert!(files.iter().any(|f| f.additions > 0));

    let details = GitReader::get_commit_details(&path, &history[0].id).expect("details");
    assert_eq!(details.summary, history[0].summary);
    assert_eq!(details.gpg_status, "N");

    let stats = GitReader::get_repo_language_stats(&path).expect("stats");
    let rust = stats
        .iter()
        .find(|s| s.language == "Rust")
        .expect("rust language");
    assert_eq!(rust.category, "programming");
    assert!(rust.file_count >= 1);

    let reflog = GitReader::get_reflog(&path, 20).expect("reflog");
    assert!(!reflog.is_empty());
}

#[test]
fn test_stage_commit_and_selective_patch() {
    let repo = TestRepo::init();
    repo.write("app.txt", "alpha\n");
    repo.commit_all("chore: seed");
    repo.write("app.txt", "alpha\nbeta\ngamma\n");

    let path = repo.path_str();
    GitWriter::stage_file(&path, "app.txt").expect("stage");
    let status = GitReader::get_status(&path).expect("status");
    assert!(status.iter().any(|s| s.path == "app.txt" && s.is_staged));

    GitWriter::unstage_file(&path, "app.txt").expect("unstage");
    GitWriter::stage_file(&path, "app.txt").expect("stage again");
    GitWriter::commit(&path, "feat: append lines", false).expect("commit");

    let history = GitReader::read_commit_history(&path, 10, None).unwrap();
    assert!(history[0].summary.starts_with("feat:"));
}

#[test]
fn test_patch_builder_applies_selected_line() {
    let repo = TestRepo::init();
    repo.write("src/main.rs", "fn main() {\n}\n");
    repo.commit_all("chore: start");
    repo.write(
        "src/main.rs",
        "fn main() {\n    println!(\"First\");\n    println!(\"Second\");\n}\n",
    );

    let file_patch = FilePatch {
        old_path: "src/main.rs".into(),
        new_path: "src/main.rs".into(),
        hunks: vec![UnifiedDiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 4,
            header: String::new(),
            lines: vec![
                UnifiedDiffLine {
                    line_type: DiffLineType::Context,
                    old_line_no: Some(1),
                    new_line_no: Some(1),
                    content: "fn main() {".into(),
                    is_selected: false,
                },
                UnifiedDiffLine {
                    line_type: DiffLineType::Addition,
                    old_line_no: None,
                    new_line_no: Some(2),
                    content: "    println!(\"First\");".into(),
                    is_selected: true,
                },
                UnifiedDiffLine {
                    line_type: DiffLineType::Addition,
                    old_line_no: None,
                    new_line_no: Some(3),
                    content: "    println!(\"Second\");".into(),
                    is_selected: false,
                },
                UnifiedDiffLine {
                    line_type: DiffLineType::Context,
                    old_line_no: Some(2),
                    new_line_no: Some(4),
                    content: "}".into(),
                    is_selected: false,
                },
            ],
        }],
    };

    let patch = PatchBuilder::build_selective_patch(&file_patch, true);
    GitWriter::apply_patch_to_index(&repo.path_str(), &patch).expect("apply patch");
    GitWriter::commit(&repo.path_str(), "feat: stage first println only", false).expect("commit");

    let head = GitReader::read_commit_history(&repo.path_str(), 1, None).unwrap();
    let blob =
        GitReader::get_file_blob(&repo.path_str(), "src/main.rs", Some(&head[0].id)).unwrap();
    let text = blob.text.unwrap();
    assert!(text.contains("First"));
    assert!(!text.contains("Second"));
}

#[test]
fn test_sandbox_rejects_path_escape() {
    let repo = TestRepo::init();
    repo.write("ok.txt", "ok\n");
    repo.commit_all("chore: ok");
    let path = repo.path_str();
    let repo_path = PathBuf::from(&path);
    assert!(sandbox_join(&repo_path, "../secret").is_err());
    assert!(sandbox_write(&path, "../outside.txt", "nope").is_err());
    sandbox_write(&path, "ok.txt", "updated\n").expect("write inside repo");
    validate_repo(&path).expect("valid repo");
}

#[test]
fn test_branch_create_checkout_and_stack() {
    let repo = TestRepo::init();
    repo.write("a.txt", "root\n");
    repo.commit_all("chore: root");
    let path = repo.path_str();

    GitWriter::create_branch(&path, "feat-auth", None).expect("create");
    GitWriter::checkout_branch(&path, "feat-auth").expect("checkout");
    repo.write("a.txt", "auth\n");
    repo.commit_all("feat: auth");

    GitWriter::create_branch(&path, "feat-oauth", None).expect("create oauth");
    GitWriter::checkout_branch(&path, "feat-oauth").expect("checkout oauth");
    repo.write("a.txt", "oauth\n");
    repo.commit_all("feat: oauth");

    let branches = GitReader::list_branches(&path).unwrap();
    assert!(branches
        .iter()
        .any(|b| b.name == "feat-oauth" && b.is_current));
}

#[test]
fn test_branch_line_changes_rename_and_revision_history() {
    let repo = TestRepo::init();
    repo.write("app.txt", "one\n");
    repo.commit_all("chore: root");
    let path = repo.path_str();

    GitWriter::create_branch(&path, "feat/lines", None).expect("create");
    GitWriter::checkout_branch(&path, "feat/lines").expect("checkout");
    repo.write("app.txt", "one\ntwo\nthree\n");
    repo.commit_all("feat: add lines");

    GitWriter::checkout_branch(&path, "main").expect("back to main");
    repo.write("main-only.txt", "only on main\n");
    repo.commit_all("chore: main only");

    let branches = GitReader::list_branches(&path).expect("branches");
    let main = branches.iter().find(|b| b.name == "main").expect("main");
    assert!(main.is_default);

    // Churn left the listing's critical path by design: line counts are zero
    // on BranchInfo and arrive through branch_stats instead.
    let feature = branches
        .iter()
        .find(|b| b.name == "feat/lines")
        .expect("feature");
    assert_eq!(
        (feature.additions, feature.deletions, feature.files_changed),
        (0, 0, 0),
        "list_branches must not block on churn subprocesses"
    );
    assert!(feature.commits_ahead_of_base >= 1);
    assert_eq!(feature.compared_to.as_deref(), Some("main"));

    let stats = GitReader::branch_stats(&path).expect("branch stats");
    assert_eq!(stats.compared_to, "main");
    let feature_stats = stats
        .updates
        .iter()
        .find(|u| u.name == "feat/lines")
        .expect("feat/lines stats");
    assert!(
        feature_stats.additions >= 2,
        "expected committed line additions on feat/lines, got {}",
        feature_stats.additions
    );
    assert!(feature_stats.commits_ahead_of_base >= 1);

    GitWriter::rename_branch(&path, "feat/lines", "feat/renamed").expect("rename");
    let renamed = GitReader::list_branches(&path).expect("after rename");
    assert!(renamed.iter().any(|b| b.name == "feat/renamed"));
    assert!(!renamed.iter().any(|b| b.name == "feat/lines"));

    let feature_history =
        GitReader::read_commit_history(&path, 20, Some("feat/renamed")).expect("feature history");
    assert!(
        feature_history
            .iter()
            .all(|c| !c.summary.contains("main only")),
        "branch-filtered history must not include commits unique to main"
    );
    let all_history = GitReader::read_commit_history(&path, 20, None).expect("all history");
    assert!(all_history.iter().any(|c| c.summary.contains("main only")));

    let range = GitReader::get_range_diff(&path, "main", "feat/renamed").expect("range diff");
    assert!(range.contains("two") || range.contains("three") || range.contains("app.txt"));
}

#[test]
fn test_github_url_parser_integration_shape() {
    let parsed = parse_github_remote_url("https://github.com/acme/gitpulse.git").unwrap();
    assert_eq!(parsed.slug(), "acme/gitpulse");
}

#[test]
fn test_discover_github_origin_remote() {
    let repo = TestRepo::init();
    repo.write("README.md", "pulse\n");
    repo.commit_all("chore: readme");
    run_git(
        repo.dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/gitpulse.git",
        ],
    );
    let discovered = gitpulse_lib::github::discover_github_remote(&repo.path_str())
        .expect("discover")
        .expect("remote");
    assert_eq!(discovered.slug(), "acme/gitpulse");
}

#[test]
fn test_clone_into_parent_directory() {
    let src = TestRepo::init();
    src.write("hello.txt", "cloned\n");
    src.commit_all("chore: seed clone");
    let parent = TempDir::new().expect("parent");
    let cloned =
        GitWriter::clone_repo(&src.path_str(), parent.path().to_str().unwrap()).expect("clone");
    assert!(Path::new(&cloned).join(".git").exists());
    let history = GitReader::read_commit_history(&cloned, 10, None).expect("history");
    assert_eq!(history.len(), 1);
}

#[test]
fn test_merge_conflict_parse_and_resolve() {
    let repo = TestRepo::init();
    repo.write("app.txt", "base\n");
    repo.commit_all("chore: base");
    let path = repo.path_str();

    GitWriter::create_branch(&path, "theirs", None).unwrap();
    GitWriter::checkout_branch(&path, "theirs").unwrap();
    repo.write("app.txt", "theirs change\n");
    repo.commit_all("feat: theirs");

    GitWriter::checkout_branch(&path, "main").unwrap();
    repo.write("app.txt", "ours change\n");
    repo.commit_all("feat: ours");

    let merge = GitWriter::merge_branch(&path, "theirs", false);
    assert!(
        merge.is_err()
            || GitReader::get_status(&path)
                .unwrap()
                .iter()
                .any(|s| s.is_conflicted)
    );

    let content = GitReader::get_file_content(&path, "app.txt", None).unwrap();
    let doc = gitpulse_lib::diff::ConflictResolver::parse("app.txt", &content);
    assert!(doc.total_conflicts >= 1);

    let mut resolved = doc.clone();
    for seg in &mut resolved.segments {
        if let gitpulse_lib::diff::FileSegment::Conflict(chunk) = seg {
            chunk.resolution = gitpulse_lib::diff::ConflictResolutionChoice::AcceptOurs;
        }
    }
    let text = gitpulse_lib::diff::ConflictResolver::render_resolved(&resolved).unwrap();
    assert!(text.contains("ours change"));
}

#[test]
fn test_image_blob_and_head_revision() {
    let repo = TestRepo::init();
    let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
    let dest = repo.dir.path().join("logo.png");
    std::fs::write(dest, png).unwrap();
    repo.commit_all("chore: add logo");
    let blob = GitReader::get_file_blob(&repo.path_str(), "logo.png", Some("HEAD")).unwrap();
    assert!(blob.is_image);
    assert!(blob.base64.is_some());
}

#[test]
fn test_validate_repo_accepts_normal_and_bare() {
    let repo = TestRepo::init();
    let canonical = validate_repo(&repo.path_str()).expect("normal repo");
    assert!(canonical.join(".git").exists());

    let git_dir = resolve_git_dir(&canonical).expect("git dir");
    assert!(git_dir.ends_with(".git"));
    assert!(git_dir.is_dir());

    let bare = TempDir::new().expect("bare tempdir");
    run_git(bare.path(), &["init", "--bare"]);
    let bare_canonical = validate_repo(&bare.path().to_string_lossy()).expect("bare repo");
    assert!(bare_canonical.join("HEAD").is_file());
    assert!(bare_canonical.join("objects").is_dir());
    assert!(!bare_canonical.join(".git").exists());
    let bare_git_dir = resolve_git_dir(&bare_canonical).expect("bare git dir");
    assert_eq!(bare_git_dir, bare_canonical);
}

#[test]
fn test_rebase_restores_branch_and_can_drop_a_commit() {
    let repo = TestRepo::init();
    repo.write("a.txt", "root\n");
    repo.commit_all("chore: root");
    let path = repo.path_str();

    GitWriter::create_branch(&path, "feat-rebase", None).unwrap();
    GitWriter::checkout_branch(&path, "feat-rebase").unwrap();
    repo.write("b.txt", "bravo\n");
    repo.commit_all("feat: bravo");
    repo.write("c.txt", "charlie\n");
    repo.commit_all("feat: charlie");
    repo.write("d.txt", "delta\n");
    repo.commit_all("feat: delta");

    let history = GitReader::read_commit_history(&path, 20, Some("feat-rebase")).unwrap();
    let bravo = history
        .iter()
        .find(|c| c.summary.contains("bravo"))
        .expect("bravo")
        .id
        .clone();
    let charlie = history
        .iter()
        .find(|c| c.summary.contains("charlie"))
        .expect("charlie")
        .id
        .clone();
    let delta = history
        .iter()
        .find(|c| c.summary.contains("delta"))
        .expect("delta")
        .id
        .clone();

    GitWriter::execute_rebase_sequence(
        &path,
        "main",
        &[
            RebaseStep {
                commit_id: bravo,
                action: RebaseActionKind::Pick,
            },
            RebaseStep {
                commit_id: charlie,
                action: RebaseActionKind::Drop,
            },
            RebaseStep {
                commit_id: delta,
                action: RebaseActionKind::Pick,
            },
        ],
    )
    .expect("rebase");

    let branches = GitReader::list_branches(&path).unwrap();
    let current = branches.iter().find(|b| b.is_current).expect("current");
    assert_eq!(current.name, "feat-rebase");

    let after = GitReader::read_commit_history(&path, 20, Some("feat-rebase")).unwrap();
    assert!(after.iter().any(|c| c.summary.contains("bravo")));
    assert!(after.iter().any(|c| c.summary.contains("delta")));
    assert!(!after.iter().any(|c| c.summary.contains("charlie")));

    let empty = GitWriter::execute_rebase_sequence(&path, "main", &[]);
    assert!(empty.is_err());
}

#[test]
fn test_rebase_failed_step_restores_original_branch() {
    let repo = TestRepo::init();
    repo.write("a.txt", "root\n");
    repo.commit_all("chore: root");
    let path = repo.path_str();
    GitWriter::create_branch(&path, "feat-keep", None).unwrap();
    GitWriter::checkout_branch(&path, "feat-keep").unwrap();

    let err = GitWriter::execute_rebase_sequence(
        &path,
        "main",
        &[RebaseStep {
            commit_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            action: RebaseActionKind::Pick,
        }],
    );
    assert!(err.is_err());
    let branches = GitReader::list_branches(&path).unwrap();
    assert!(branches
        .iter()
        .any(|b| b.is_current && b.name == "feat-keep"));
}

#[test]
fn test_coverage_scan_of_rust_lcov_in_opened_repo() {
    let repo = TestRepo::init();
    repo.write("src/lib.rs", "pub fn x() {}\n");
    repo.write(
        "lcov.info",
        "SF:src/lib.rs\nDA:1,1\nDA:2,0\nend_of_record\n",
    );
    let report = CoverageScanner::scan(&repo.path_str()).expect("scan");
    assert!(report
        .families
        .iter()
        .any(|f| f.family == "rust" && f.found));
    assert!(report.files.iter().any(|f| f.path == "src/lib.rs"));
    assert_eq!(report.overall.lines_found, 2);
    assert_eq!(report.overall.lines_hit, 1);
}
