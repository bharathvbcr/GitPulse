//! Adversarial probes of the ref listings (list_branches, list_tags) against odd refs.
//! Public API only, so the same file runs on main and on the lane for an A/B.
mod common;

use gitpulse_lib::engine::GitReader;
use gitpulse_lib::procguard::LockedSpawn;
use std::path::Path;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .output_locked()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if args.first() == Some(&"init") {
        common::trust_repo(dir);
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn repo() -> (TempDir, String) {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "a@b.c"]);
    git(dir.path(), &["config", "user.name", "A"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("f"), "x").unwrap();
    git(dir.path(), &["add", "f"]);
    git(dir.path(), &["commit", "-q", "-m", "seed"]);
    git(dir.path(), &["switch", "-q", "-c", "good"]);
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "g"]);
    git(dir.path(), &["switch", "-q", "main"]);
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "m2"]);
    let path = dir.path().to_str().unwrap().to_string();
    (dir, path)
}

/// Writes `packed-refs` with raw bytes (no filesystem path limits or UTF-8 rules).
fn pack(dir: &Path, rows: &[(&str, Vec<u8>)]) {
    let mut bytes = b"# pack-refs with: peeled fully-peeled \n".to_vec();
    for (oid, name) in rows {
        bytes.extend_from_slice(oid.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(name);
        bytes.push(b'\n');
    }
    std::fs::write(dir.join(".git").join("packed-refs"), bytes).unwrap();
}

fn names(path: &str) -> Result<Vec<String>, gitpulse_lib::ipc_error::CommandError> {
    GitReader::list_branches(path).map(|b| b.into_iter().map(|b| b.name).collect())
}

fn assert_survives(path: &str, expect_extra: &[&str]) {
    let got = names(path).unwrap_or_else(|e| panic!("list_branches failed: {e}", e = e.message()));
    for n in ["main", "good"].iter().chain(expect_extra) {
        assert!(got.iter().any(|g| g == n), "{n} missing from {got:?}");
    }
}

#[test]
fn l4_odd_but_legal_ref_names_are_listed() {
    let (dir, path) = repo();
    let oid = git(dir.path(), &["rev-parse", "good"]);
    for name in ["-dash", "a%00b", "pct%(refname)", "semi;colon", "sp ace"] {
        // update-ref accepts these where `git branch` refuses a leading dash.
        let full = format!("refs/heads/{name}");
        let o = StdCommand::new("git")
            .args(["update-ref", &full, &oid])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        eprintln!("update-ref {name:?} -> {}", o.status.success());
    }
    let listed = names(&path).unwrap();
    eprintln!("LISTED {listed:?}");
    for n in ["-dash", "a%00b", "pct%(refname)", "semi;colon"] {
        assert!(listed.iter().any(|g| g == n), "{n} dropped: {listed:?}");
    }
}

#[test]
fn l4_a_4kib_ref_name_does_not_break_the_listing() {
    let (dir, path) = repo();
    let oid = git(dir.path(), &["rev-parse", "good"]);
    let long = format!("refs/heads/{}", "x".repeat(4000));
    pack(dir.path(), &[(oid.as_str(), long.clone().into_bytes())]);
    assert_survives(&path, &[]);
    let got = names(&path).unwrap();
    assert!(got.iter().any(|g| g.len() == 4000), "long name dropped");
}

#[test]
fn l4_a_non_utf8_ref_name_does_not_break_the_listing() {
    let (dir, path) = repo();
    let oid = git(dir.path(), &["rev-parse", "good"]);
    let mut name = b"refs/heads/bad-".to_vec();
    name.extend_from_slice(&[0xff, 0xfe, b'x']);
    pack(dir.path(), &[(oid.as_str(), name)]);
    let r = names(&path);
    eprintln!("NONUTF8 list_branches -> {r:?}");
    let got =
        r.unwrap_or_else(|e| panic!("one odd ref broke the whole listing: {e}", e = e.message()));
    assert!(got.iter().any(|g| g == "main"));
}

#[test]
fn l4_refs_pointing_at_a_tree_or_a_blob_do_not_break_the_listing() {
    let (dir, path) = repo();
    let tree = git(dir.path(), &["rev-parse", "main^{tree}"]);
    let blob = git(dir.path(), &["rev-parse", "main:f"]);
    let good = git(dir.path(), &["rev-parse", "good"]);
    let main = git(dir.path(), &["rev-parse", "main"]);
    pack(
        dir.path(),
        &[
            (good.as_str(), b"refs/heads/good".to_vec()),
            (main.as_str(), b"refs/heads/main".to_vec()),
            (tree.as_str(), b"refs/heads/treeref".to_vec()),
            (blob.as_str(), b"refs/remotes/origin/blobref".to_vec()),
        ],
    );
    let r = GitReader::list_branches(&path);
    eprintln!(
        "TREE/BLOB list_branches -> {:?}",
        r.as_ref().map(|b| b
            .iter()
            .map(|b| (
                b.name.clone(),
                b.commits_ahead_of_base,
                b.compared_to.clone()
            ))
            .collect::<Vec<_>>())
    );
    let listed = r.unwrap_or_else(|e| {
        panic!(
            "a non-commit ref broke the whole listing: {e}",
            e = e.message()
        )
    });
    for n in ["main", "good"] {
        assert!(listed.iter().any(|b| b.name == n), "{n} missing");
    }
}

#[test]
fn l4_tags_at_a_tree_or_a_blob_do_not_break_the_tag_listing() {
    let (dir, path) = repo();
    let tree = git(dir.path(), &["rev-parse", "main^{tree}"]);
    let blob = git(dir.path(), &["rev-parse", "main:f"]);
    let main = git(dir.path(), &["rev-parse", "main"]);
    git(dir.path(), &["tag", "ok", main.as_str()]);
    pack(
        dir.path(),
        &[
            (main.as_str(), b"refs/heads/main".to_vec()),
            (main.as_str(), b"refs/tags/ok".to_vec()),
            (tree.as_str(), b"refs/tags/treetag".to_vec()),
            (blob.as_str(), b"refs/tags/blobtag".to_vec()),
        ],
    );
    let r = GitReader::list_tags(&path);
    eprintln!(
        "TAGS list_tags -> {:?}",
        r.as_ref().map(|t| t
            .tags
            .iter()
            .map(|t| (t.name.clone(), t.compared_to.clone()))
            .collect::<Vec<_>>())
    );
    let listed =
        r.unwrap_or_else(|e| panic!("a non-commit tag broke the listing: {e}", e = e.message()));
    assert!(listed.tags.iter().any(|t| t.name == "ok"));
}

#[test]
fn l4_a_tab_and_unicode_in_subject_and_author_keep_rows_aligned() {
    let (dir, path) = repo();
    git(dir.path(), &["switch", "-q", "-c", "tabby"]);
    let o = StdCommand::new("git")
        .args([
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "sub\tject \u{1F600} 100%",
        ])
        .env("GIT_AUTHOR_NAME", "N\tame \u{e9}")
        .current_dir(dir.path())
        .output_locked()
        .unwrap();
    assert!(o.status.success());
    git(dir.path(), &["switch", "-q", "main"]);
    assert_survives(&path, &["tabby"]);
    let tabby = GitReader::list_branches(&path)
        .unwrap()
        .into_iter()
        .find(|b| b.name == "tabby")
        .unwrap();
    assert_eq!(tabby.commits_ahead_of_base, 1);
}
