//! Parity oracle: gix's in-process status against `git status --porcelain=v1 -u -z`.
//!
//! Every case builds a throwaway repository with plain `git`, runs both
//! readers on it and compares sorted `(path, index letter, worktree letter)`
//! rows. A case either must match, or pins the exact divergence found
//! (`Expect::Diverges`); pinning means a gix upgrade that fixes (or worsens) a
//! divergence fails this test loudly instead of passing silently.
//!
//! `cargo test --manifest-path crates/git-status-parity/Cargo.toml -- --nocapture`
//! prints the parity table the verdict in docs/PERFORMANCE.md quotes.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Once;

use git_status_parity::{git, git_status, gix_status_at, hermetic_environment, Row, Untracked};

static HERMETIC: Once = Once::new();

fn tmp_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("parity");
    fs::create_dir_all(&root).expect("create parity root");
    HERMETIC.call_once(|| hermetic_environment(&root));
    root
}

/// A scratch directory removed on drop, unique per process and case.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = tmp_root().join(format!("{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(repo: &Path, rel: &str, text: &str) {
    let path = repo.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, text).expect("write");
}

fn body(tag: &str) -> String {
    (0..30)
        .map(|i| format!("{tag} line {i} of the file\n"))
        .collect()
}

/// A repository with one commit holding a.txt, b.txt, c.txt, dir/d.txt, dir/e.txt.
fn init(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("mkdir repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    for name in ["a.txt", "b.txt", "c.txt", "dir/d.txt", "dir/e.txt"] {
        write(&repo, name, &body(name));
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    repo
}

fn r(path: &str, x: char, y: char) -> Row {
    (path.to_string(), x, y)
}

enum Expect {
    /// gix must equal git.
    Parity,
    /// The pinned divergence: git's rows and gix's rows.
    Diverges { git: Vec<Row>, gix: Vec<Row> },
}

struct Case {
    name: &'static str,
    /// Builds the fixture under the scratch dir and returns where to run status.
    setup: fn(&Path) -> PathBuf,
    expect: Expect,
}

fn modified(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "a.txt", &body("changed"));
    repo
}
fn touched_unchanged(root: &Path) -> PathBuf {
    let repo = init(root);
    // Same bytes, new mtime: the stat differs, the content does not.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(&repo, "a.txt", &body("a.txt"));
    repo
}
fn added_staged(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "new.txt", &body("new"));
    git(&repo, &["add", "new.txt"]);
    repo
}
fn added_staged_then_modified(root: &Path) -> PathBuf {
    let repo = added_staged(root);
    write(&repo, "new.txt", &body("newer"));
    repo
}
fn staged_modified_then_modified(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "a.txt", &body("one"));
    git(&repo, &["add", "a.txt"]);
    write(&repo, "a.txt", &body("two"));
    repo
}
fn deleted_worktree(root: &Path) -> PathBuf {
    let repo = init(root);
    fs::remove_file(repo.join("b.txt")).expect("rm");
    repo
}
fn deleted_staged(root: &Path) -> PathBuf {
    let repo = init(root);
    git(&repo, &["rm", "-q", "b.txt"]);
    repo
}
fn deleted_staged_recreated(root: &Path) -> PathBuf {
    let repo = init(root);
    git(&repo, &["rm", "-q", "--cached", "b.txt"]);
    repo
}
fn renamed_staged(root: &Path) -> PathBuf {
    let repo = init(root);
    git(&repo, &["mv", "c.txt", "c-renamed.txt"]);
    repo
}
fn renamed_staged_modified(root: &Path) -> PathBuf {
    let repo = renamed_staged(root);
    let mut text = body("c.txt");
    text.push_str("extra\n");
    write(&repo, "c-renamed.txt", &text);
    repo
}
fn untracked_file(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "loose.txt", "x\n");
    repo
}
fn untracked_dir(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "newdir/one.txt", "1\n");
    write(&repo, "newdir/deep/two.txt", "2\n");
    repo
}
fn untracked_empty_dir(root: &Path) -> PathBuf {
    let repo = init(root);
    fs::create_dir_all(repo.join("emptydir")).expect("mkdir");
    repo
}
fn ignored(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, ".gitignore", "*.log\nbuild/\n");
    git(&repo, &["add", ".gitignore"]);
    write(&repo, "x.log", "x\n");
    write(&repo, "build/out.bin", "x\n");
    write(&repo, "keep.txt", "x\n");
    repo
}
fn nested_gitignore(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "dir/.gitignore", "*.tmp\n!keep.tmp\n/only-here\n");
    git(&repo, &["add", "dir/.gitignore"]);
    write(&repo, "dir/a.tmp", "x\n");
    write(&repo, "dir/keep.tmp", "x\n");
    write(&repo, "dir/only-here", "x\n");
    write(&repo, "only-here", "x\n");
    write(&repo, "a.tmp", "x\n");
    repo
}
fn info_exclude(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, ".git/info/exclude", ".devcouncil/\n");
    write(&repo, ".devcouncil/state.db", "x\n");
    write(&repo, "other.txt", "x\n");
    repo
}
fn submodule_base(root: &Path) -> PathBuf {
    let sub = root.join("subsrc");
    fs::create_dir_all(&sub).expect("mkdir sub");
    git(&sub, &["init", "-q", "-b", "main"]);
    git(&sub, &["config", "commit.gpgsign", "false"]);
    write(&sub, "s.txt", &body("s"));
    git(&sub, &["add", "-A"]);
    git(&sub, &["commit", "-q", "-m", "sub"]);
    let repo = init(root);
    git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().expect("utf8"),
            "sub",
        ],
    );
    git(&repo, &["commit", "-q", "-m", "add sub"]);
    repo
}
fn submodule_clean(root: &Path) -> PathBuf {
    submodule_base(root)
}
fn submodule_new_commit(root: &Path) -> PathBuf {
    let repo = submodule_base(root);
    let sub = repo.join("sub");
    git(&sub, &["config", "commit.gpgsign", "false"]);
    write(&sub, "s2.txt", "x\n");
    git(&sub, &["add", "s2.txt"]);
    git(&sub, &["commit", "-q", "-m", "more"]);
    repo
}
fn submodule_modified_content(root: &Path) -> PathBuf {
    let repo = submodule_base(root);
    write(&repo.join("sub"), "s.txt", &body("dirty"));
    repo
}
fn submodule_untracked_content(root: &Path) -> PathBuf {
    let repo = submodule_base(root);
    write(&repo.join("sub"), "untracked.txt", "x\n");
    repo
}
fn sparse_cone(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "inside/i.txt", &body("inside"));
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "inside"]);
    git(&repo, &["sparse-checkout", "init", "--cone"]);
    git(&repo, &["sparse-checkout", "set", "inside"]);
    assert!(
        !repo.join("dir/d.txt").exists(),
        "dir/ must be outside the cone"
    );
    write(&repo, "inside/i.txt", &body("changed in cone"));
    write(&repo, "inside/new.txt", "x\n");
    repo
}
fn symlink_added(root: &Path) -> PathBuf {
    let repo = init(root);
    symlink("a.txt", repo.join("link")).expect("symlink");
    git(&repo, &["add", "link"]);
    repo
}
fn symlink_retargeted(root: &Path) -> PathBuf {
    let repo = symlink_added(root);
    git(&repo, &["commit", "-q", "-m", "link"]);
    fs::remove_file(repo.join("link")).expect("rm link");
    symlink("b.txt", repo.join("link")).expect("symlink");
    repo
}
fn symlink_dangling_untracked(root: &Path) -> PathBuf {
    let repo = init(root);
    symlink("does-not-exist", repo.join("dangling")).expect("symlink");
    repo
}
fn file_became_symlink(root: &Path) -> PathBuf {
    let repo = init(root);
    fs::remove_file(repo.join("b.txt")).expect("rm");
    symlink("a.txt", repo.join("b.txt")).expect("symlink");
    repo
}
fn case_rename_staged(root: &Path) -> PathBuf {
    let repo = init(root);
    git(&repo, &["mv", "c.txt", "C.txt"]);
    repo
}
fn case_rename_on_disk_only(root: &Path) -> PathBuf {
    let repo = init(root);
    fs::rename(repo.join("c.txt"), repo.join("C.txt")).expect("rename");
    repo
}
/// Written CRLF, added (the index holds LF), committed, then rewritten with the
/// same CRLF bytes a second later: stat-dirty but content-clean after the
/// filter, so both readers must hash through the clean filter and report nothing.
fn crlf_autocrlf_true_clean(root: &Path) -> PathBuf {
    let repo = init(root);
    git(&repo, &["config", "core.autocrlf", "true"]);
    write(&repo, "lf.txt", "one\r\ntwo\r\nthree\r\n");
    git(&repo, &["add", "lf.txt"]);
    git(&repo, &["commit", "-q", "-m", "crlf"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(&repo, "lf.txt", "one\r\ntwo\r\nthree\r\n");
    repo
}
fn crlf_gitattributes_text_auto_clean(root: &Path) -> PathBuf {
    let repo = init(root);
    write(
        &repo,
        ".gitattributes",
        "* text=auto\n*.bat text eol=crlf\n",
    );
    write(&repo, "t.txt", "one\r\ntwo\r\nthree\r\n");
    write(&repo, "t.bat", "echo\r\nexit\r\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "attrs"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(&repo, "t.txt", "one\r\ntwo\r\nthree\r\n");
    write(&repo, "t.bat", "echo\r\nexit\r\n");
    repo
}
fn crlf_real_change(root: &Path) -> PathBuf {
    let repo = crlf_autocrlf_true_clean(root);
    write(&repo, "lf.txt", "one\r\nTWO\r\nthree\r\n");
    repo
}
/// LF in the index, CRLF in the worktree, never re-added. Plain git reports
/// this as modified; the reader must agree with it, not with what one expects.
fn crlf_lf_index_crlf_worktree(root: &Path) -> PathBuf {
    let repo = init(root);
    git(&repo, &["config", "core.autocrlf", "true"]);
    write(&repo, "lf.txt", "one\ntwo\nthree\n");
    git(&repo, &["add", "lf.txt"]);
    git(&repo, &["commit", "-q", "-m", "lf"]);
    write(&repo, "lf.txt", "one\r\ntwo\r\nthree\r\n");
    repo
}
fn crlf_without_normalisation_is_a_change(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "lf.txt", "one\ntwo\n");
    git(&repo, &["add", "lf.txt"]);
    git(&repo, &["commit", "-q", "-m", "lf"]);
    write(&repo, "lf.txt", "one\r\ntwo\r\n");
    repo
}
fn exec_bit(root: &Path) -> PathBuf {
    let repo = init(root);
    let path = repo.join("a.txt");
    let mut perm = fs::metadata(&path).expect("meta").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod");
    repo
}
fn exec_bit_staged(root: &Path) -> PathBuf {
    let repo = exec_bit(root);
    git(&repo, &["add", "a.txt"]);
    repo
}
fn intent_to_add(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "ita.txt", &body("ita"));
    git(&repo, &["add", "-N", "ita.txt"]);
    repo
}
fn intent_to_add_modified_then_staged_elsewhere(root: &Path) -> PathBuf {
    let repo = intent_to_add(root);
    write(&repo, "b.txt", &body("changed"));
    git(&repo, &["add", "b.txt"]);
    repo
}
fn linked_worktree(root: &Path) -> PathBuf {
    let repo = init(root);
    let wt = root.join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            wt.to_str().expect("utf8"),
        ],
    );
    write(&wt, "a.txt", &body("dirty in linked"));
    write(&wt, "only-linked.txt", "x\n");
    // The main checkout is dirty differently; the linked one must not see it.
    write(&repo, "b.txt", &body("dirty in main"));
    wt
}
fn clean(root: &Path) -> PathBuf {
    init(root)
}
fn merge_conflict(root: &Path) -> PathBuf {
    let repo = init(root);
    git(&repo, &["checkout", "-q", "-b", "other"]);
    write(&repo, "a.txt", &body("other"));
    git(&repo, &["commit", "-q", "-am", "other"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "a.txt", &body("main"));
    git(&repo, &["commit", "-q", "-am", "main"]);
    let output = std::process::Command::new("git")
        .args(["merge", "other"])
        .current_dir(&repo)
        .output()
        .expect("merge");
    assert!(!output.status.success(), "the merge must conflict");
    repo
}

fn cases() -> Vec<Case> {
    use Expect::Parity;
    let c = |name, setup: fn(&Path) -> PathBuf, expect| Case {
        name,
        setup,
        expect,
    };
    vec![
        c("clean", clean, Parity),
        c("modified", modified, Parity),
        c("touched_unchanged (stale stat)", touched_unchanged, Parity),
        c("added_staged", added_staged, Parity),
        c(
            "added_staged_then_modified",
            added_staged_then_modified,
            Parity,
        ),
        c(
            "staged_modified_then_modified",
            staged_modified_then_modified,
            Parity,
        ),
        c("deleted_worktree", deleted_worktree, Parity),
        c("deleted_staged", deleted_staged, Parity),
        c("deleted_staged_recreated", deleted_staged_recreated, Parity),
        c("renamed_staged", renamed_staged, Parity),
        c("renamed_staged_modified", renamed_staged_modified, Parity),
        c("untracked_file", untracked_file, Parity),
        c("untracked_dir", untracked_dir, Parity),
        c("untracked_empty_dir", untracked_empty_dir, Parity),
        c("ignored", ignored, Parity),
        c("nested_gitignore", nested_gitignore, Parity),
        c("info_exclude", info_exclude, Parity),
        c("submodule_clean", submodule_clean, Parity),
        c("submodule_new_commit", submodule_new_commit, Parity),
        c(
            "submodule_modified_content",
            submodule_modified_content,
            Parity,
        ),
        c(
            "submodule_untracked_content",
            submodule_untracked_content,
            Parity,
        ),
        c("sparse_cone", sparse_cone, Parity),
        c("symlink_added", symlink_added, Parity),
        c("symlink_retargeted", symlink_retargeted, Parity),
        c(
            "symlink_dangling_untracked",
            symlink_dangling_untracked,
            Parity,
        ),
        c("file_became_symlink", file_became_symlink, Parity),
        c("case_rename_staged", case_rename_staged, Parity),
        c("case_rename_on_disk_only", case_rename_on_disk_only, Parity),
        c("crlf_autocrlf_true_clean", crlf_autocrlf_true_clean, Parity),
        c(
            "crlf_gitattributes_text_auto_clean",
            crlf_gitattributes_text_auto_clean,
            Parity,
        ),
        c("crlf_real_change", crlf_real_change, Parity),
        c(
            "crlf_lf_index_crlf_worktree",
            crlf_lf_index_crlf_worktree,
            Parity,
        ),
        c(
            "crlf_without_normalisation_is_a_change",
            crlf_without_normalisation_is_a_change,
            Parity,
        ),
        c("exec_bit", exec_bit, Parity),
        c("exec_bit_staged", exec_bit_staged, Parity),
        c("intent_to_add", intent_to_add, Parity),
        c(
            "intent_to_add_plus_staged_modify",
            intent_to_add_modified_then_staged_elsewhere,
            Parity,
        ),
        c("linked_worktree", linked_worktree, Parity),
        c(
            "merge_conflict (both modified)",
            merge_conflict,
            Expect::Diverges {
                git: vec![r("a.txt", 'U', 'U')],
                gix: vec![r("a.txt", ' ', 'U')],
            },
        ),
    ]
}

fn fmt(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "(clean)".into();
    }
    rows.iter()
        .map(|(p, x, y)| format!("{x}{y} {p}"))
        .collect::<Vec<_>>()
        .join("; ")
}

#[test]
fn gix_status_matches_git_status_u_z() {
    let mut table = String::from("| case | result | git | gix |\n| --- | --- | --- | --- |\n");
    let mut failures = Vec::new();
    for case in cases() {
        let scratch = Scratch::new(case.name.split(' ').next().expect("name"));
        let cwd = (case.setup)(&scratch.0);
        let git_rows = git_status(&cwd).expect("git status");
        let gix_rows = gix_status_at(&cwd, Untracked::Files);
        let gix_rows = match gix_rows {
            Ok(rows) => rows,
            Err(error) => {
                table.push_str(&format!(
                    "| {} | gix error | {} | {error} |\n",
                    case.name,
                    fmt(&git_rows)
                ));
                failures.push(format!("{}: gix failed: {error}", case.name));
                continue;
            }
        };
        let equal = git_rows == gix_rows;
        let verdict = match (&case.expect, equal) {
            (Expect::Parity, true) => "match",
            (Expect::Parity, false) => {
                failures.push(format!(
                    "{}: unexpected divergence\n  git: {}\n  gix: {}",
                    case.name,
                    fmt(&git_rows),
                    fmt(&gix_rows)
                ));
                "DIVERGES (unpinned)"
            }
            (Expect::Diverges { git: g, gix: x }, false) if *g == git_rows && *x == gix_rows => {
                "diverges (pinned)"
            }
            (Expect::Diverges { .. }, _) => {
                failures.push(format!(
                    "{}: pinned divergence changed\n  git: {}\n  gix: {}",
                    case.name,
                    fmt(&git_rows),
                    fmt(&gix_rows)
                ));
                "DIVERGENCE CHANGED"
            }
        };
        table.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            case.name,
            verdict,
            fmt(&git_rows),
            fmt(&gix_rows)
        ));
    }
    println!("{table}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Without a directory walk, gix must report exactly git's tracked rows and no
/// untracked ones (the cheap mode GitPulse could use where only counts of
/// tracked changes matter).
#[test]
fn untracked_off_equals_git_minus_untracked() {
    let scratch = Scratch::new("untracked-off");
    let cwd = untracked_dir(&scratch.0);
    write(&cwd, "a.txt", &body("changed"));
    let expected: Vec<Row> = git_status(&cwd)
        .expect("git")
        .into_iter()
        .filter(|(_, x, _)| *x != '?')
        .collect();
    let got = gix_status_at(&cwd, Untracked::Off).expect("gix");
    assert_eq!(got, expected);
    assert_eq!(got, vec![r("a.txt", ' ', 'M')]);
}
