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
        Self::init_with(&[])
    }

    fn init_with(extra_init_args: &[&str]) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let mut args = vec!["init", "-b", "main"];
        args.extend_from_slice(extra_init_args);
        run_git(dir.path(), &args);
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

/// Regression (NUL-safe record framing): a commit subject containing a raw
/// 0x01 byte used to be able to split the `log` stream into bogus records
/// when 0x01 was the record terminator. Framing now terminates records with
/// %x00 (subjects may legally contain any byte except NUL), so a hostile
/// subject must never desync the commits that follow it.
#[test]
fn history_survives_0x01_byte_inside_commit_subject() {
    let repo = TestRepo::init();
    repo.write("src/lib.rs", "fn first() {}\n");
    repo.commit_all("feat: clean first");

    // Commit whose subject embeds the field separator byte. argv may carry
    // any byte except NUL, and git accepts it verbatim.
    let hostile = "feat: \u{1} embedded separator \u{1} end";
    let output = Command::new("git")
        .args(["commit", "--allow-empty", "-m", hostile])
        .current_dir(repo.dir.path())
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("spawn hostile commit");
    assert!(
        output.status.success(),
        "git must accept a 0x01-bearing subject: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A clean commit AFTER the hostile subject is the actual regression:
    // \x01 record framing used to swallow everything that followed.
    repo.write("src/lib.rs", "fn after() {}\n");
    repo.commit_all("feat: clean after");

    let path = repo.path_str();
    let history = GitReader::read_commit_history(&path, 10, None).expect("history");
    assert_eq!(
        history.len(),
        3,
        "a 0x01 inside one subject must not fabricate or swallow records: {history:?}"
    );
    assert_eq!(history[0].summary, "feat: clean after");
    assert_eq!(history[1].summary, hostile);
    assert!(history[1].id.len() == 40 || history[1].id.len() == 64);
    assert_eq!(history[2].summary, "feat: clean first");
    assert_ne!(history[0].id, history[1].id);
    assert_ne!(history[1].id, history[2].id);
}

/// Regression (\x01-safe field framing, live-git half): a 0x01 byte inside an
/// AUTHOR NAME used to shift every later positional field — the email came
/// back as a fragment of the name. The record's risky fields are parsed from
/// the right now, so the timestamp and email must survive intact.
#[test]
fn history_survives_0x01_byte_inside_author_name() {
    let repo = TestRepo::init();
    repo.write("src/lib.rs", "fn first() {}\n");
    repo.commit_all("feat: clean seed");

    let output = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "feat: hostile author"])
        .current_dir(repo.dir.path())
        .env("GIT_AUTHOR_NAME", "Ev\u{1}il Name")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("spawn hostile-author commit");
    assert!(
        output.status.success(),
        "git must accept a 0x01-bearing author name: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let path = repo.path_str();
    let history = GitReader::read_commit_history(&path, 10, None).expect("history");
    assert_eq!(history.len(), 2, "{history:?}");
    // Newest row is the hostile-author commit.
    let hostile = &history[0];
    // A 0x01 inside the author name may leave that name's leading fragment
    // glued to the summary text (bounded degradation, documented); the
    // structurally protected fields must still come back exact.
    assert!(
        hostile.summary.starts_with("feat: hostile author"),
        "summary must keep the subject text: {:?}",
        hostile.summary
    );
    assert!(
        hostile.timestamp > 0,
        "timestamp must be structurally immune to author-name corruption"
    );
    assert_eq!(
        hostile.author_email, "test@example.com",
        "email is the record's final field and must not shift"
    );
}

/// Regression (revision parity for path filtering): the graph walk uses
/// `--all` or the user-selected revision, but the path filter's allow-set was
/// built from a plain HEAD-only walk. Commits on OTHER branches touching the
/// path were therefore retained OUT of the graph — whole lanes vanished.
#[test]
fn commits_touching_path_honors_all_and_selected_revision() {
    let repo = TestRepo::init();
    repo.write("shared.txt", "base\n");
    repo.commit_all("feat: base shared");
    let base_commit = GitReader::read_commit_history(&repo.path_str(), 1, None)
        .expect("base tip")
        .remove(0);

    run_git(repo.dir.path(), &["checkout", "-q", "-b", "feature"]);
    repo.write("shared.txt", "base\nfeature edit\n");
    repo.commit_all("feat: feature edits shared");
    let feature_edit = GitReader::read_commit_history(&repo.path_str(), 1, None)
        .expect("feature tip")
        .remove(0);
    // Back on main: the feature commit is invisible to HEAD.
    run_git(repo.dir.path(), &["checkout", "-q", "main"]);

    let path = repo.path_str();

    // Default semantics must match the graph walk (`--all`): branch commits
    // touching the path stay in the allow-set.
    let all = GitReader::commits_touching_path(&path, "shared.txt", 10, None).expect("--all walk");
    assert!(
        all.contains(&feature_edit.id),
        "--all walk must include other-branch commits touching the path: {all:?}"
    );
    assert!(all.contains(&base_commit.id));

    // An explicit revision keeps HEAD-only semantics available.
    let head_only =
        GitReader::commits_touching_path(&path, "shared.txt", 10, Some("HEAD")).expect("HEAD walk");
    assert!(
        !head_only.contains(&feature_edit.id),
        "explicit HEAD must stay a HEAD-only walk: {head_only:?}"
    );
    assert!(head_only.contains(&base_commit.id));
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
                    no_newline: false,
                },
                UnifiedDiffLine {
                    line_type: DiffLineType::Addition,
                    old_line_no: None,
                    new_line_no: Some(2),
                    content: "    println!(\"First\");".into(),
                    is_selected: true,
                    no_newline: false,
                },
                UnifiedDiffLine {
                    line_type: DiffLineType::Addition,
                    old_line_no: None,
                    new_line_no: Some(3),
                    content: "    println!(\"Second\");".into(),
                    is_selected: false,
                    no_newline: false,
                },
                UnifiedDiffLine {
                    line_type: DiffLineType::Context,
                    old_line_no: Some(2),
                    new_line_no: Some(4),
                    content: "}".into(),
                    is_selected: false,
                    no_newline: false,
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

/// Regression (M1+M11): in porcelain v1 `-z` output a rename record is
/// `XY <new>\0<old>\0`, and its second NUL field must be consumed or every
/// later entry desyncs. The unstaged edit staged AFTER the rename proves the
/// cursor survived the two-field record.
#[test]
fn test_get_status_rename_order_and_cursor_alignment() {
    let repo = TestRepo::init();
    repo.write("original.txt", "one\ntwo\n");
    repo.write("other.txt", "stable\n");
    repo.commit_all("chore: seed");

    run_git(repo.dir.path(), &["mv", "original.txt", "renamed.txt"]);
    repo.write("other.txt", "stable\nchanged\n");

    let status = GitReader::get_status(&repo.path_str()).expect("status");
    let renamed = status
        .iter()
        .find(|s| s.path == "renamed.txt")
        .expect("rename keyed on NEW path");
    assert_eq!(renamed.old_path.as_deref(), Some("original.txt"));
    assert!(renamed.status_code.starts_with('R'));
    assert!(renamed.is_staged);
    assert!(!status
        .iter()
        .any(|s| s.old_path.as_deref() == Some("renamed.txt")));

    let after = status
        .iter()
        .find(|s| s.path == "other.txt")
        .expect("record following the rename must still parse");
    assert!(!after.is_staged, "got: {after:?}");
    assert!(
        after.additions >= 1,
        "numstat churn must join on the parsed path: {after:?}"
    );
}

/// Regression (M1+M11, copy half): with `status.renames=copies` and a changed
/// source, a staged duplicate arrives as `C  <new>\0<orig>\0`; the parser must
/// consume both fields so the trailing `M` record stays aligned.
#[test]
fn test_get_status_copy_record_keeps_cursor_aligned() {
    let repo = TestRepo::init();
    repo.write("orig.txt", "same\nlines\n");
    repo.commit_all("chore: seed");
    run_git(repo.dir.path(), &["config", "status.renames", "copies"]);
    fs::copy(
        repo.dir.path().join("orig.txt"),
        repo.dir.path().join("dup.txt"),
    )
    .unwrap();
    repo.write("orig.txt", "same\nlines\nplus one\n");
    run_git(repo.dir.path(), &["add", "-A"]);

    let status = GitReader::get_status(&repo.path_str()).expect("status");
    let copy = status
        .iter()
        .find(|s| s.status_code.starts_with('C'))
        .expect("copy entry");
    assert_eq!(copy.path, "dup.txt");
    assert_eq!(copy.old_path.as_deref(), Some("orig.txt"));

    let modified = status
        .iter()
        .find(|s| s.path == "orig.txt")
        .expect("record following the copy pair must parse");
    assert_eq!(modified.status_code, "M ");
}

/// Regression (m2): blame header detection hardcoded SHA-1's 40-char oid and
/// rejected every record in a SHA-256 repository.
#[test]
fn test_get_file_blame_sha256_repo() {
    let repo = TestRepo::init_with(&["--object-format=sha256"]);
    repo.write("story.txt", "first line\nsecond line\n");
    repo.commit_all("feat: sha256 story");

    let blame = GitReader::get_file_blame(&repo.path_str(), "story.txt").expect("blame");
    assert_eq!(blame.len(), 2);
    for line in &blame {
        assert_eq!(
            line.commit_id.len(),
            64,
            "sha256 oids are 64 hex chars: {}",
            line.commit_id
        );
        assert!(line.commit_id.chars().all(|c| c.is_ascii_hexdigit()));
    }
    assert_eq!(blame[0].content, "first line");
    assert_eq!(blame[1].line_no, 2);
    assert_eq!(blame[0].author_name, "Test User");
}

/// Regression (item 5): glob metacharacters in a filename must not widen the
/// pathspec used by log/diff queries.
#[test]
fn test_glob_shaped_paths_match_literally() {
    let repo = TestRepo::init();
    repo.write("weird*.txt", "literal star\n");
    repo.commit_all("chore: literal star file");
    repo.write("weirdX.txt", "glob sibling\n");
    repo.commit_all("chore: sibling file");
    repo.write("weird*.txt", "literal star\nedited\n");

    let path = repo.path_str();

    // A widened glob would match both files' history; literal matches only
    // the star-named file's single commit.
    let touching =
        GitReader::commits_touching_path(&path, "weird*.txt", 10, None).expect("log pathspec");
    assert_eq!(
        touching.len(),
        1,
        "pathspec must stay literal: {touching:?}"
    );

    let diff = GitReader::get_file_diff(&path, "weird*.txt", false, false).expect("diff");
    assert!(
        diff.contains("edited"),
        "must diff the literal file: {diff}"
    );
    assert!(!diff.contains("weirdX"), "sibling file leaked into diff");

    // blame treats its <file> argument literally already; it must keep doing
    // so (and NOT receive pathspec magic, which git rejects there).
    let blame = GitReader::get_file_blame(&path, "weird*.txt").expect("blame");
    assert_eq!(blame.len(), 2);
    assert!(blame
        .iter()
        .all(|l| l.content.contains("literal star") || l.content.contains("edited")));
}

/// Regression (item 6): reader commands resolve paths through symlinks only
/// while staying inside the repository; a symlinked directory pointing out is
/// refused before git ever runs.
#[test]
fn test_reader_read_paths_refuse_symlink_escape() {
    let outside = TempDir::new().expect("outside tempdir");
    fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

    let repo = TestRepo::init();
    repo.write("keep.txt", "inside\n");
    repo.commit_all("chore: keep");
    std::os::unix::fs::symlink(outside.path(), repo.dir.path().join("leak")).unwrap();

    let path = repo.path_str();
    let err = GitReader::get_file_diff(&path, "leak/secret.txt", false, false)
        .expect_err("symlink escape must fail");
    assert!(err.contains("escapes the repository"), "got: {err}");
    assert!(GitReader::commits_touching_path(&path, "leak/secret.txt", 10, None).is_err());
    assert!(GitReader::get_file_blame(&path, "leak/secret.txt").is_err());
}

/// Regression (m3c): a failing `git show --numstat` must surface through
/// get_commit_details instead of reporting silently-empty changed_files.
#[test]
fn test_get_commit_details_surfaces_missing_blob_failure() {
    let repo = TestRepo::init();
    repo.write("data.bin", "payload\n");
    repo.commit_all("chore: payload");
    let path = repo.path_str();

    let output = Command::new("git")
        .args(["rev-parse", "HEAD:data.bin"])
        .current_dir(repo.dir.path())
        .output()
        .expect("rev-parse");
    assert!(output.status.success());
    let blob_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Remove the loose blob backing the committed file; metadata still parses
    // but the numstat walk cannot read the object any more.
    let object = repo
        .dir
        .path()
        .join(".git/objects")
        .join(&blob_sha[0..2])
        .join(&blob_sha[2..]);
    fs::remove_file(&object)
        .unwrap_or_else(|e| panic!("expected loose object at {}: {e}", object.display()));

    let head = GitReader::head_id(&path).expect("head id");
    let details = GitReader::get_commit_details(&path, &head)
        .expect_err("missing blob must fail the whole detail report");
    assert!(!details.is_empty());

    assert!(GitReader::get_commit_files(&path, &head).is_err());
}

// ---------------------------------------------------------------------------
// Audit fixes: destructive-delete guard, restack fork point, rebase step
// ancestry, reword body preservation, start-point revisions.
// ---------------------------------------------------------------------------

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn force_delete_refuses_default_branch() {
    let repo = TestRepo::init();
    repo.write("f.txt", "one\n");
    repo.commit_all("chore: first");
    // Detach so no worktree holds `main`: only the default-branch rule can
    // refuse the delete (native git would happily run `branch -D main` here).
    run_git(repo.dir.path(), &["checkout", "--detach"]);
    let path = repo.path_str();

    let err = GitWriter::delete_branch(&path, "main", true)
        .expect_err("force-deleting the default branch must be refused");
    assert!(
        err.contains("refusing to force-delete") && err.contains("default branch"),
        "error must name the default-branch refusal: {err}"
    );
    // The branch must still exist after the refusal.
    assert!(git_out(repo.dir.path(), &["rev-parse", "--verify", "main"]).len() == 40);
}

#[test]
fn force_delete_refuses_branch_checked_out_in_linked_worktree() {
    let repo = TestRepo::init();
    repo.write("f.txt", "one\n");
    repo.commit_all("chore: first");

    let work_parent = TempDir::new().expect("worktree parent");
    let work_path = work_parent.path().join("linked");
    run_git(
        repo.dir.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            work_path.to_str().unwrap(),
        ],
    );

    let path = repo.path_str();
    let err = GitWriter::delete_branch(&path, "feature", true)
        .expect_err("force-deleting a branch checked out in a worktree must be refused");
    assert!(
        err.contains("checked out"),
        "error must name the worktree conflict: {err}"
    );
    // Non-forced delete keeps git's native safety net and also fails.
    assert!(GitWriter::delete_branch(&path, "feature", false).is_err());
}

#[test]
fn normal_delete_of_merged_branch_still_succeeds() {
    let repo = TestRepo::init();
    repo.write("f.txt", "one\n");
    repo.commit_all("chore: first");
    run_git(repo.dir.path(), &["checkout", "-b", "feature"]);
    repo.write("g.txt", "g\n");
    repo.commit_all("feat: g");
    run_git(repo.dir.path(), &["checkout", "main"]);
    run_git(repo.dir.path(), &["merge", "--ff-only", "feature"]);

    GitWriter::delete_branch(&repo.path_str(), "feature", false)
        .expect("deleting a merged branch with -d must succeed");
}

#[test]
fn restack_after_parent_rewrite_replays_only_new_commits() {
    let repo = TestRepo::init();
    repo.write("base.txt", "b\n");
    repo.commit_all("root");
    repo.write("f.txt", "v1\n");
    repo.commit_all("commit one");
    run_git(repo.dir.path(), &["checkout", "-b", "feature"]);
    repo.write("g.txt", "g\n");
    repo.commit_all("commit two");

    // Rewrite the parent branch by amending its tip.
    run_git(repo.dir.path(), &["checkout", "main"]);
    repo.write("f.txt", "v1 changed\n");
    run_git(repo.dir.path(), &["add", "-A"]);
    run_git(
        repo.dir.path(),
        &["commit", "--amend", "-m", "commit one prime"],
    );

    GitWriter::restack(&repo.path_str(), "feature", "main")
        .expect("restack over a rewritten parent must succeed");

    let count = git_out(repo.dir.path(), &["rev-list", "--count", "main..feature"]);
    assert_eq!(
        count, "1",
        "only the child's own commit may sit on the rewritten parent"
    );
    let tip_parent = git_out(repo.dir.path(), &["rev-parse", "feature^"]);
    let main_tip = git_out(repo.dir.path(), &["rev-parse", "main"]);
    assert_eq!(tip_parent, main_tip, "feature must sit directly on main");
    let subjects = git_out(repo.dir.path(), &["log", "--format=%s", "main..feature"]);
    assert_eq!(
        subjects, "commit two",
        "no stale pre-image commit may remain"
    );
}

#[test]
fn rebase_sequence_refuses_foreign_commit_without_moving_branch() {
    let repo = TestRepo::init();
    repo.write("base.txt", "b\n");
    repo.commit_all("root");
    run_git(repo.dir.path(), &["checkout", "-b", "side"]);
    repo.write("side.txt", "s\n");
    repo.commit_all("side commit");
    let foreign = git_out(repo.dir.path(), &["rev-parse", "side"]);
    run_git(repo.dir.path(), &["checkout", "main"]);
    repo.write("main2.txt", "m\n");
    repo.commit_all("main work");
    let onto = git_out(repo.dir.path(), &["rev-parse", "HEAD~1"]);
    let main_before = git_out(repo.dir.path(), &["rev-parse", "main"]);

    let err = GitWriter::execute_rebase_sequence(
        &repo.path_str(),
        &onto,
        &[RebaseStep {
            commit_id: foreign.clone(),
            action: RebaseActionKind::Pick,
        }],
    )
    .expect_err("a commit from another branch must not transplant");
    assert!(
        err.contains(&format!("{:.7}", &foreign[..7])) || err.contains(&foreign),
        "error must name the offending step: {err}"
    );
    assert_eq!(
        git_out(repo.dir.path(), &["rev-parse", "main"]),
        main_before,
        "branch must be untouched after refusal"
    );
    assert_eq!(
        git_out(repo.dir.path(), &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
}

#[test]
fn rebase_reword_preserves_commit_body() {
    let repo = TestRepo::init();
    repo.write("base.txt", "b\n");
    repo.commit_all("root");
    repo.write("d.txt", "d\n");
    run_git(repo.dir.path(), &["add", "-A"]);
    run_git(
        repo.dir.path(),
        &["commit", "-m", "subject line", "-m", "body text here"],
    );
    let target = git_out(repo.dir.path(), &["rev-parse", "HEAD"]);
    let root = git_out(repo.dir.path(), &["rev-parse", "HEAD~1"]);

    GitWriter::execute_rebase_sequence(
        &repo.path_str(),
        &root,
        &[RebaseStep {
            commit_id: target,
            action: RebaseActionKind::Reword("new subject".into()),
        }],
    )
    .expect("reword sequence must succeed");

    let message = git_out(repo.dir.path(), &["log", "-1", "--format=%B"]);
    assert_eq!(
        message, "new subject\n\nbody text here",
        "reword replaces only the subject; body must survive"
    );
}

#[test]
fn create_branch_accepts_head_ancestor_start_point() {
    let repo = TestRepo::init();
    repo.write("f.txt", "one\n");
    repo.commit_all("first");
    let first = git_out(repo.dir.path(), &["rev-parse", "HEAD"]);
    repo.write("g.txt", "two\n");
    repo.commit_all("second");

    GitWriter::create_branch(&repo.path_str(), "backdated", Some("HEAD~1"))
        .expect("revision-style start points must be accepted");
    assert_eq!(
        git_out(repo.dir.path(), &["rev-parse", "backdated"]),
        first,
        "branch must point at the resolved start point"
    );
}

/// `--numstat -z` round-trip through real git: rename entries must surface
/// under their post-image path with status R, and non-ASCII filenames must
/// arrive raw (never C-quoted) because NUL framing needs no escaping.
#[test]
fn commit_files_parses_renames_and_unicode_paths_from_real_git() {
    let repo = TestRepo::init();
    let unicode_old = "dir/naïve-файл.txt";
    let unicode_new = "dir/renamed-ünïcode.txt";
    repo.write(unicode_old, "content\n");
    repo.write("plain.txt", "one\n");
    repo.commit_all("chore: seed");

    run_git(repo.dir.path(), &["mv", unicode_old, unicode_new]);
    repo.write("plain.txt", "two\n");
    // Binary content so numstat emits "-" counts.
    std::fs::write(repo.dir.path().join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
    repo.commit_all("feat: rename and edit");

    let head = git_out(repo.dir.path(), &["rev-parse", "HEAD"]);
    let files = GitReader::get_commit_files(&repo.path_str(), &head).expect("commit files");

    let renamed = files
        .iter()
        .find(|f| f.path == unicode_new)
        .expect("rename must be reported under its exact post-image path");
    assert_eq!(renamed.status_code, "R");
    let plain = files
        .iter()
        .find(|f| f.path == "plain.txt")
        .expect("edited file must be listed");
    assert_eq!(plain.status_code, "M");
    assert!(plain.additions > 0 && plain.deletions > 0);
    let binary = files
        .iter()
        .find(|f| f.path == "blob.bin")
        .expect("binary file must be listed");
    assert_eq!(binary.status_code, "B");
    assert_eq!((binary.additions, binary.deletions), (0, 0));
}

#[test]
fn get_commit_file_diff_returns_only_the_requested_path() {
    let repo = TestRepo::init();
    repo.write("keep.txt", "keep-old\n");
    repo.write("other.txt", "other-old\n");
    repo.commit_all("chore: seed");
    repo.write("keep.txt", "keep-new\n");
    repo.write("other.txt", "other-new\n");
    repo.commit_all("feat: edit both");

    let head = git_out(repo.dir.path(), &["rev-parse", "HEAD"]);
    let one = GitReader::get_commit_file_diff(&repo.path_str(), &head, "keep.txt")
        .expect("path-scoped commit diff");
    assert!(one.contains("keep-new") || one.contains("+keep-new"));
    assert!(
        !one.contains("other-new") && !one.contains("other-old"),
        "scoped diff must not carry the sibling file: {one}"
    );
}

#[test]
fn empty_selective_patch_is_rejected_before_git_apply() {
    let file_patch = FilePatch {
        old_path: "app.txt".into(),
        new_path: "app.txt".into(),
        hunks: vec![UnifiedDiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: String::new(),
            lines: vec![UnifiedDiffLine {
                line_type: DiffLineType::Addition,
                old_line_no: None,
                new_line_no: Some(1),
                content: "nope".into(),
                is_selected: false,
                no_newline: false,
            }],
        }],
    };
    let err = PatchBuilder::validate_file_patch(&file_patch).expect_err("empty selection");
    assert!(err.to_lowercase().contains("no lines selected"));
}

#[test]
fn github_ssh_url_with_explicit_port_strips_the_port() {
    let parsed = parse_github_remote_url("ssh://git@github.com:22/acme/gitpulse.git").unwrap();
    assert_eq!(parsed.host, "github.com");
    assert_eq!(parsed.owner, "acme");
    assert_eq!(parsed.name, "gitpulse");
    assert_eq!(parsed.slug(), "acme/gitpulse");
}

#[test]
fn rebase_rejects_squash_as_the_first_step() {
    let repo = TestRepo::init();
    repo.write("a.txt", "root\n");
    repo.commit_all("chore: root");
    repo.write("b.txt", "feature\n");
    repo.commit_all("feat: feature");
    let path = repo.path_str();
    let head = git_out(repo.dir.path(), &["rev-parse", "HEAD"]);
    let onto = git_out(repo.dir.path(), &["rev-parse", "HEAD~1"]);
    let err = GitWriter::execute_rebase_sequence(
        &path,
        &onto,
        &[RebaseStep {
            commit_id: head,
            action: RebaseActionKind::Squash,
        }],
    )
    .expect_err("squash with no previous commit must be refused");
    assert!(
        err.to_lowercase().contains("squash"),
        "error must name the illegal first action, got: {err}"
    );
}

#[test]
fn selective_staging_new_file_via_dev_null() {
    let repo = TestRepo::init();
    repo.write("seed.txt", "seed\n");
    repo.commit_all("chore: seed");
    let file_patch = FilePatch {
        old_path: "/dev/null".into(),
        new_path: "fresh.txt".into(),
        hunks: vec![UnifiedDiffHunk {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: 1,
            header: String::new(),
            lines: vec![UnifiedDiffLine {
                line_type: DiffLineType::Addition,
                old_line_no: None,
                new_line_no: Some(1),
                content: "hello from patch".into(),
                is_selected: true,
                no_newline: false,
            }],
        }],
    };
    PatchBuilder::validate_file_patch(&file_patch).expect("new-file /dev/null must validate");
    let patch = PatchBuilder::build_selective_patch(&file_patch, true);
    GitWriter::apply_patch_to_index(&repo.path_str(), &patch).expect("apply new-file patch");
    let status = git_out(repo.dir.path(), &["status", "--porcelain"]);
    assert!(
        status.contains("fresh.txt"),
        "new file must be staged, status was: {status}"
    );
}

#[test]
fn linked_worktree_mutations_serialize_without_lock_failures() {
    let repo = TestRepo::init();
    repo.write("f.txt", "one\n");
    repo.commit_all("chore: first");
    let work_parent = TempDir::new().expect("worktree parent");
    let work_path = work_parent.path().join("linked");
    run_git(
        repo.dir.path(),
        &[
            "worktree",
            "add",
            "-b",
            "wt-main",
            work_path.to_str().unwrap(),
        ],
    );

    let main_path = repo.path_str();
    let wt_path = work_path.to_string_lossy().into_owned();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let b_main = barrier.clone();
    let b_wt = barrier.clone();
    let h_main = std::thread::spawn(move || {
        b_main.wait();
        GitWriter::create_branch(&main_path, "from-main", None)
    });
    let h_wt = std::thread::spawn(move || {
        b_wt.wait();
        GitWriter::create_branch(&wt_path, "from-wt", None)
    });
    h_main
        .join()
        .expect("main thread")
        .expect("create branch from main checkout");
    h_wt.join()
        .expect("worktree thread")
        .expect("create branch from linked worktree");
    let branches = git_out(repo.dir.path(), &["branch", "--list"]);
    assert!(branches.contains("from-main"), "got: {branches}");
    assert!(branches.contains("from-wt"), "got: {branches}");
}

// ---------------------------------------------------------------------------
// Stack page: restack hardening + deep-history ancestry walk
// ---------------------------------------------------------------------------

/// The targeted first-parent walk must stop AT a known tip (inclusive) and
/// return oldest-first, so consecutive pairs are parent edges the hierarchy
/// engine can consume directly.
#[test]
fn first_parent_chain_stops_at_known_tip_oldest_first() {
    let repo = TestRepo::init();
    repo.write("base.txt", "b\n");
    repo.commit_all("root");
    let root = git_out(repo.dir.path(), &["rev-parse", "HEAD"]);
    repo.write("mid.txt", "m\n");
    repo.commit_all("mid");
    repo.write("top.txt", "t\n");
    repo.commit_all("top");
    let tip = git_out(repo.dir.path(), &["rev-parse", "HEAD"]);

    let stop: std::collections::HashSet<String> = [root.clone()].into_iter().collect();
    let chain = GitReader::first_parent_chain(&repo.path_str(), &tip, &stop, 100)
        .expect("walk succeeds")
        .expect("stop id is reachable");
    // Oldest-first: root (the stop id) -> mid -> tip.
    let mid = git_out(repo.dir.path(), &["rev-parse", "HEAD~"]);
    assert_eq!(chain, vec![root.clone(), mid, tip.clone()]);

    // A stop set that never matches exhausts the cap and reports None —
    // "no discoverable base", never a fabricated root.
    let miss: std::collections::HashSet<String> = ["deadbeef".to_string()].into_iter().collect();
    let unresolved = GitReader::first_parent_chain(&repo.path_str(), &tip, &miss, 100).unwrap();
    assert!(unresolved.is_none());
}

/// Regression (audit dirty-tree): restacking with uncommitted changes used to
/// hand `git rebase` a dirty tree; now it is refused before any state moves.
#[test]
fn restack_refuses_dirty_worktree_without_moving_refs() {
    let repo = TestRepo::init();
    repo.write("f.txt", "one\n");
    repo.commit_all("base");
    run_git(repo.dir.path(), &["checkout", "-b", "feature"]);
    repo.write("g.txt", "g\n");
    repo.commit_all("feature work");
    let feature_before = git_out(repo.dir.path(), &["rev-parse", "feature"]);

    repo.write("dirty.txt", "uncommitted\n");

    let err = GitWriter::restack(&repo.path_str(), "feature", "main")
        .expect_err("dirty tree must be refused");
    assert!(
        err.contains("uncommitted"),
        "error must name the real cause: {err}"
    );
    let feature_after = git_out(repo.dir.path(), &["rev-parse", "feature"]);
    assert_eq!(feature_before, feature_after, "refs must not move");
}

/// A conflicting rebase must roll back cleanly: no half-applied state, HEAD
/// back where the user was, branch ref untouched, error says what happened.
#[test]
fn restack_conflict_aborts_and_restores_original_checkout() {
    let repo = TestRepo::init();
    repo.write("shared.txt", "base\n");
    repo.commit_all("base");
    run_git(repo.dir.path(), &["checkout", "-b", "feature"]);
    repo.write("shared.txt", "feature version\n");
    repo.commit_all("feature edit");
    let feature_before = git_out(repo.dir.path(), &["rev-parse", "feature"]);
    run_git(repo.dir.path(), &["checkout", "main"]);
    repo.write("other.txt", "main advance\n");
    repo.commit_all("main work");
    // main's new commit conflicts with feature's edit of shared.txt
    repo.write("shared.txt", "main version\n");
    repo.commit_all("main conflicts");
    let main_tip = git_out(repo.dir.path(), &["rev-parse", "HEAD"]);

    let err = GitWriter::restack(&repo.path_str(), "feature", "main")
        .expect_err("conflicting restack must fail");
    assert!(
        err.contains("rolled back"),
        "error must promise rollback: {err}"
    );
    // No rebase may linger mid-flight.
    assert!(
        !repo.dir.path().join(".git").join("rebase-merge").exists(),
        "rebase-merge state must be gone"
    );
    // The user was on main when they clicked restack; they must still be.
    let head_branch = git_out(repo.dir.path(), &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(head_branch, "main");
    // And feature must point exactly where it did before.
    let feature_after = git_out(repo.dir.path(), &["rev-parse", "feature"]);
    assert_eq!(feature_before, feature_after);
    // Sanity: the conflicting base really was main's new tip.
    assert_ne!(main_tip, feature_before);
}

/// Success path: rebase leaves `branch` checked out by default, which would
/// silently switch the user's working copy; the original checkout must come
/// back.
#[test]
fn restack_success_restores_previous_checkout() {
    let repo = TestRepo::init();
    repo.write("f.txt", "v1\n");
    repo.commit_all("base");
    run_git(repo.dir.path(), &["checkout", "-b", "feature"]);
    repo.write("g.txt", "g\n");
    repo.commit_all("feature work");
    run_git(repo.dir.path(), &["checkout", "main"]);
    repo.write("f.txt", "v1 changed\n");
    repo.commit_all("rewrite base content");

    GitWriter::restack(&repo.path_str(), "feature", "main")
        .expect("non-conflicting restack must succeed");

    // The user was on main; rebase would have left them on feature.
    let current = git_out(repo.dir.path(), &["branch", "--show-current"]);
    assert_eq!(current, "main", "original checkout must be restored");

    // And the child really moved onto the rewritten parent.
    let count = git_out(repo.dir.path(), &["rev-list", "--count", "main..feature"]);
    assert_eq!(count, "1");
}

/// An in-progress rebase must be detected and refused instead of corrupted.
#[test]
fn restack_refuses_when_a_rebase_is_already_in_progress() {
    let repo = TestRepo::init();
    repo.write("f.txt", "one\n");
    repo.commit_all("base");
    run_git(repo.dir.path(), &["checkout", "-b", "feature"]);
    repo.write("f.txt", "feature version\n");
    repo.write("g.txt", "g\n");
    repo.commit_all("feature work");
    run_git(repo.dir.path(), &["checkout", "main"]);
    repo.write("f.txt", "conflicting one\n");
    repo.commit_all("conflict on f");

    // Manufacture a mid-rebase state that the client did not create: main
    // replayed onto feature collides on f.txt.
    run_git_expect_failure(repo.dir.path(), &["rebase", "feature"]);
    assert!(repo.dir.path().join(".git").join("rebase-merge").exists());

    let err = GitWriter::restack(&repo.path_str(), "feature", "main")
        .expect_err("must refuse while another rebase is in flight");
    assert!(
        err.contains("already in progress"),
        "error must name the blocker: {err}"
    );

    // Clean up for the temp dir assertion hygiene.
    run_git(repo.dir.path(), &["rebase", "--abort"]);
}

fn run_git_expect_failure(cwd: &Path, args: &[&str]) {
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
        !output.status.success(),
        "git {args:?} was expected to fail but succeeded"
    );
}
