//! Parity and edge-case oracle comparing GitPulse's in-process `gix` status
//! (`engine::gix_status::porcelain_v1_z_for_test`) against
//! `git -c core.fsmonitor=false -c core.quotepath=off status --porcelain=v1 -u -z`.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use gitpulse_lib::engine::git_reader::GitReader;
use gitpulse_lib::engine::gix_status::porcelain_v1_z_for_test;
use gitpulse_lib::procguard::LockedSpawn;

/// One status row: `(path, old_path, index_letter, worktree_letter)`.
type Row = (String, Option<String>, char, char);

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Parity")
        .env("GIT_AUTHOR_EMAIL", "parity@example.com")
        .env("GIT_COMMITTER_NAME", "Parity")
        .env("GIT_COMMITTER_EMAIL", "parity@example.com")
        .output_locked()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git_status(cwd: &Path) -> Vec<Row> {
    parse_porcelain_z(&git_status_bytes(cwd))
}

fn git_status_bytes(cwd: &Path) -> Vec<u8> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotepath=off",
            "status",
            "--porcelain=v1",
            "-u",
            "-z",
        ])
        .current_dir(cwd)
        .output_locked()
        .expect("spawn git status");
    assert!(
        output.status.success(),
        "git status failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn gix_rows_at(cwd: &Path) -> Vec<Row> {
    let text = porcelain_v1_z_for_test(cwd)
        .expect("gix status must not error")
        .expect("parity fixture must stay on gix without fallback");
    parse_porcelain_z(text.as_bytes())
}

fn parse_porcelain_z(bytes: &[u8]) -> Vec<Row> {
    let mut rows = rows_in_order(bytes);
    rows.sort();
    rows
}

/// The rows as the bytes list them, unsorted: order is part of the output.
fn rows_in_order(bytes: &[u8]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut fields = bytes.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(record) = fields.next() {
        if record.len() < 4 {
            continue;
        }
        let (x, y) = (record[0] as char, record[1] as char);
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        let old_path = if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            fields
                .next()
                .map(|raw| String::from_utf8_lossy(raw).into_owned())
        } else {
            None
        };
        rows.push((path, old_path, x, y));
    }
    rows
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

fn init(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("mkdir repo");
    common::run_git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    for name in ["a.txt", "b.txt", "c.txt", "dir/d.txt", "dir/e.txt"] {
        write(&repo, name, &body(name));
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    repo
}

struct Case {
    name: &'static str,
    setup: fn(&Path) -> PathBuf,
}

fn clean(root: &Path) -> PathBuf {
    init(root)
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
/// An untracked path that sorts before a tracked change: git lists the
/// tracked change first.
fn untracked_sorting_before_a_change(root: &Path) -> PathBuf {
    let repo = init(root);
    write(&repo, "a.txt", &body("changed"));
    write(&repo, "dir/e.txt", &body("changed"));
    git(&repo, &["add", "dir/e.txt"]);
    write(&repo, ".cache/ledger.sqlite", "x\n");
    write(&repo, "0first.txt", "x\n");
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
    common::run_git(&sub, &["init", "-q", "-b", "main"]);
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
fn file_became_symlink_staged(root: &Path) -> PathBuf {
    let repo = file_became_symlink(root);
    git(&repo, &["add", "b.txt"]);
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
    common::trust_repo(&wt);
    write(&wt, "a.txt", &body("dirty in linked"));
    write(&wt, "only-linked.txt", "x\n");
    // The main checkout is dirty differently; the linked one must not see it.
    write(&repo, "b.txt", &body("dirty in main"));
    wt
}
fn unborn_head(root: &Path) -> PathBuf {
    let repo = root.join("unborn");
    fs::create_dir_all(&repo).expect("mkdir");
    common::run_git(&repo, &["init", "-q", "-b", "main"]);
    write(&repo, "staged.txt", "staged\n");
    git(&repo, &["add", "staged.txt"]);
    write(&repo, "loose.txt", "loose\n");
    repo
}
fn merge_conflict_uu(root: &Path) -> PathBuf {
    let repo = init(root);
    git(&repo, &["checkout", "-q", "-b", "other"]);
    write(&repo, "a.txt", &body("other"));
    git(&repo, &["commit", "-q", "-am", "other"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "a.txt", &body("main"));
    git(&repo, &["commit", "-q", "-am", "main"]);
    let output = Command::new("git")
        .args(["merge", "other"])
        .current_dir(&repo)
        .output_locked()
        .expect("merge");
    assert!(!output.status.success(), "the merge must conflict");
    repo
}
fn merge_conflict_aa_du_ud(root: &Path) -> PathBuf {
    let repo = init(root);
    // AA: both branches add `added.txt` with different contents.
    // DU: main deletes `a.txt`, other modifies `a.txt`.
    // UD: main modifies `b.txt`, other deletes `b.txt`.
    git(&repo, &["checkout", "-q", "-b", "other"]);
    write(&repo, "added.txt", &body("other-added"));
    write(&repo, "a.txt", &body("other-modified"));
    git(&repo, &["rm", "-q", "b.txt"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "other"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "added.txt", &body("main-added"));
    write(&repo, "b.txt", &body("main-modified"));
    git(&repo, &["rm", "-q", "a.txt"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "main"]);
    let output = Command::new("git")
        .args(["merge", "other"])
        .current_dir(&repo)
        .output_locked()
        .expect("merge");
    assert!(!output.status.success(), "the merge must conflict");
    repo
}
fn conflict_dd_au_ua_via_stages(root: &Path) -> PathBuf {
    let repo = init(root);
    let oid_a = git(&repo, &["rev-parse", "HEAD:a.txt"]).trim().to_string();
    let oid_b = git(&repo, &["rev-parse", "HEAD:b.txt"]).trim().to_string();
    let oid_c = git(&repo, &["rev-parse", "HEAD:c.txt"]).trim().to_string();
    git(&repo, &["rm", "-q", "-f", "a.txt", "b.txt", "c.txt"]);
    // Build exact stage patterns:
    //   a.txt: stage 1 only -> DD (both deleted)
    //   b.txt: stage 2 only -> AU (added by us)
    //   c.txt: stage 3 only -> UA (added by them)
    let spec = format!(
        "100644 {oid_a} 1\ta.txt\n\
         100644 {oid_b} 2\tb.txt\n\
         100644 {oid_c} 3\tc.txt\n"
    );
    let mut child = Command::new("git")
        .args(["update-index", "--index-info"])
        .current_dir(&repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn update-index");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(spec.as_bytes())
        .expect("write spec");
    let out = child.wait_with_output().expect("wait update-index");
    assert!(
        out.status.success(),
        "update-index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    repo
}

fn cases() -> Vec<Case> {
    let c = |name, setup: fn(&Path) -> PathBuf| Case { name, setup };
    vec![
        c("clean", clean),
        c("modified", modified),
        c("touched_unchanged (stale stat)", touched_unchanged),
        c("added_staged", added_staged),
        c("added_staged_then_modified", added_staged_then_modified),
        c(
            "staged_modified_then_modified",
            staged_modified_then_modified,
        ),
        c("deleted_worktree", deleted_worktree),
        c("deleted_staged", deleted_staged),
        c("deleted_staged_recreated", deleted_staged_recreated),
        c("renamed_staged", renamed_staged),
        c("renamed_staged_modified", renamed_staged_modified),
        c("untracked_file", untracked_file),
        c("untracked_dir", untracked_dir),
        c("untracked_empty_dir", untracked_empty_dir),
        c(
            "untracked_sorting_before_a_change",
            untracked_sorting_before_a_change,
        ),
        c("ignored", ignored),
        c("nested_gitignore", nested_gitignore),
        c("info_exclude", info_exclude),
        c("submodule_clean", submodule_clean),
        c("submodule_new_commit", submodule_new_commit),
        c("submodule_modified_content", submodule_modified_content),
        c("submodule_untracked_content", submodule_untracked_content),
        c("sparse_cone", sparse_cone),
        c("symlink_added", symlink_added),
        c("symlink_retargeted", symlink_retargeted),
        c("symlink_dangling_untracked", symlink_dangling_untracked),
        c("file_became_symlink", file_became_symlink),
        c("file_became_symlink_staged", file_became_symlink_staged),
        c("case_rename_staged", case_rename_staged),
        c("case_rename_on_disk_only", case_rename_on_disk_only),
        c("crlf_autocrlf_true_clean", crlf_autocrlf_true_clean),
        c(
            "crlf_gitattributes_text_auto_clean",
            crlf_gitattributes_text_auto_clean,
        ),
        c("crlf_real_change", crlf_real_change),
        c("crlf_lf_index_crlf_worktree", crlf_lf_index_crlf_worktree),
        c(
            "crlf_without_normalisation_is_a_change",
            crlf_without_normalisation_is_a_change,
        ),
        c("exec_bit", exec_bit),
        c("exec_bit_staged", exec_bit_staged),
        c("intent_to_add", intent_to_add),
        c(
            "intent_to_add_plus_staged_modify",
            intent_to_add_modified_then_staged_elsewhere,
        ),
        c("linked_worktree", linked_worktree),
        c("unborn_head", unborn_head),
        c("merge_conflict_uu", merge_conflict_uu),
        c("merge_conflict_aa_du_ud", merge_conflict_aa_du_ud),
        c("conflict_dd_au_ua_via_stages", conflict_dd_au_ua_via_stages),
    ]
}

fn fmt(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "(clean)".into();
    }
    rows.iter()
        .map(|(p, old, x, y)| match old {
            Some(old) => format!("{x}{y} {old} -> {p}"),
            None => format!("{x}{y} {p}"),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[test]
fn gix_status_matches_git_status_u_z_across_all_cases() {
    let mut failures = Vec::new();
    for case in cases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = (case.setup)(dir.path());
        let expected = git_status(&cwd);
        let actual = gix_rows_at(&cwd);
        if expected != actual {
            failures.push(format!(
                "{}:\n  git: {}\n  gix: {}",
                case.name,
                fmt(&expected),
                fmt(&actual)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "gix status diverged from git:\n{}",
        failures.join("\n")
    );
}

/// Order is part of the output: status lists keep the order they parse, so
/// gix must list rows as git does (tracked changes by path, then untracked
/// paths), not merely the same set.
#[test]
fn gix_status_lists_rows_in_gits_order_across_all_cases() {
    let mut failures = Vec::new();
    for case in cases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = (case.setup)(dir.path());
        let expected = rows_in_order(&git_status_bytes(&cwd));
        let actual = rows_in_order(
            porcelain_v1_z_for_test(&cwd)
                .expect("gix status must not error")
                .expect("parity fixture must stay on gix without fallback")
                .as_bytes(),
        );
        if expected != actual {
            failures.push(format!(
                "{}:\n  git: {}\n  gix: {}",
                case.name,
                fmt(&expected),
                fmt(&actual)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "gix status listed rows in another order than git:\n{}",
        failures.join("\n")
    );
}

#[test]
fn fsmonitor_hook_is_never_executed_by_gix_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = init(dir.path());
    write(&repo, "loose.txt", "dirty\n");
    let hook = repo.join(".git/fsmonitor-probe.sh");
    fs::write(
        &hook,
        "#!/bin/sh\n: > fsmonitor-executed\nprintf 'probe-token\\0'\n",
    )
    .expect("hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).expect("chmod");
    git(
        &repo,
        &[
            "config",
            "core.fsmonitor",
            hook.to_str().expect("hook path"),
        ],
    );

    let gix_rows = gix_rows_at(&repo);
    assert!(
        !repo.join("fsmonitor-executed").exists(),
        "gix executed core.fsmonitor hook"
    );
    assert_eq!(gix_rows, git_status(&repo));
    assert!(
        !repo.join("fsmonitor-executed").exists(),
        "git_status must keep core.fsmonitor disabled"
    );
}

#[test]
fn unsupported_features_fall_back_cleanly_through_get_status() {
    common::unmetered_spawns();

    // Local external filter falls back from gix to git_cli and agrees with git.
    let filter_dir = tempfile::tempdir().expect("tempdir");
    let filter_repo = init(filter_dir.path());
    git(
        &filter_repo,
        &["config", "--local", "filter.custom.clean", "sed s/a/A/g"],
    );
    write(&filter_repo, ".gitattributes", "*.txt filter=custom\n");
    write(&filter_repo, "a.txt", "edited\n");
    assert_eq!(
        porcelain_v1_z_for_test(&filter_repo).expect("probe"),
        None,
        "repo-local filter.* must trigger Fallback"
    );
    let statuses = GitReader::get_status(filter_repo.to_str().expect("utf8"))
        .expect("GitReader::get_status must succeed via fallback");
    assert!(statuses.iter().any(|s| s.path == "a.txt"));
    assert!(statuses.iter().any(|s| s.path == ".gitattributes"));

    // Physical split index (`update-index --split-index`) without `core.splitIndex=true`
    // is dissolved natively by gix and agrees with git.
    let split_native_dir = tempfile::tempdir().expect("tempdir");
    let split_native_repo = init(split_native_dir.path());
    git(&split_native_repo, &["update-index", "--split-index"]);
    write(&split_native_repo, "b.txt", "edited in split index\n");
    assert_eq!(
        gix_rows_at(&split_native_repo),
        git_status(&split_native_repo)
    );

    // Explicit `core.splitIndex=true` falls back from gix to git_cli and agrees with git.
    let split_dir = tempfile::tempdir().expect("tempdir");
    let split_repo = init(split_dir.path());
    git(
        &split_repo,
        &["config", "--local", "core.splitIndex", "true"],
    );
    git(&split_repo, &["update-index", "--split-index"]);
    write(&split_repo, "b.txt", "edited in split index\n");
    assert_eq!(
        porcelain_v1_z_for_test(&split_repo).expect("probe"),
        None,
        "core.splitIndex=true must trigger Fallback"
    );
    let split_statuses = GitReader::get_status(split_repo.to_str().expect("utf8"))
        .expect("GitReader::get_status must succeed via fallback on split index");
    assert!(split_statuses.iter().any(|s| s.path == "b.txt"));

    // Unregistered gitlink (nested repo committed via `git add .` without `.gitmodules`)
    // falls back from gix to git_cli and reports the dirty gitlink via `GitReader::get_status`.
    let bare_gl_dir = tempfile::tempdir().expect("tempdir");
    let bare_gl_repo = init(bare_gl_dir.path());
    let sub = bare_gl_repo.join("sub");
    fs::create_dir(&sub).expect("mkdir sub");
    common::run_git(&sub, &["init", "-q", "-b", "main"]);
    git(&sub, &["config", "commit.gpgsign", "false"]);
    write(&sub, "f.txt", "one\n");
    git(&sub, &["add", "f.txt"]);
    git(&sub, &["commit", "-q", "-m", "sub one"]);
    git(&bare_gl_repo, &["add", "."]);
    git(&bare_gl_repo, &["commit", "-q", "-m", "add gitlink"]);
    write(&sub, "f.txt", "two\n");
    git(&sub, &["commit", "-q", "-am", "sub two"]);
    assert_eq!(
        porcelain_v1_z_for_test(&bare_gl_repo).expect("probe"),
        None,
        "unregistered gitlink must trigger Fallback"
    );
    let gl_statuses = GitReader::get_status(bare_gl_repo.to_str().expect("utf8"))
        .expect("GitReader::get_status must succeed via fallback on unregistered gitlink");
    assert!(gl_statuses.iter().any(|s| s.path == "sub"));
}
