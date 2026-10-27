//! W6-L4 adversarial review of `decode_hex` (ft-181d43a9): refusing non-ASCII
//! must not change any valid-HEAD outcome, and every caller that reaches
//! `decode_hex` before `is_hex_hash` must survive hostile text. All of these
//! are expected to PASS on the reviewed commit (they are the attacks that held).

mod common;
use common::{run_git, trust_repo};
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::storage::commit_graph::{inspect, CommitGraphState};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

/// Every test starts here, so this is also where the spawn rate cap is lifted
/// for the process.
fn serial() -> MutexGuard<'static, ()> {
    common::unmetered_spawns();
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

fn graph_repo(sha256: bool) -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    let mut args = vec!["init", "-q", "-b", "main"];
    if sha256 {
        args.push("--object-format=sha256");
    }
    let init = std::process::Command::new("git")
        .args(&args)
        .current_dir(dir.path())
        .output_locked()
        .expect("spawn git");
    if !init.status.success() {
        return None;
    }
    trust_repo(dir.path());
    for (k, v) in [
        ("commit.gpgsign", "false"),
        ("gc.auto", "0"),
        ("user.name", "T"),
        ("user.email", "t@example.com"),
    ] {
        run_git(dir.path(), &["config", k, v]);
    }
    for name in ["a.txt", "b.txt"] {
        std::fs::write(dir.path().join(name), name).unwrap();
        run_git(dir.path(), &["add", "-A"]);
        run_git(dir.path(), &["commit", "-q", "-m", name]);
    }
    run_git(
        dir.path(),
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    Some(dir)
}

fn state_with_head(dir: &Path, head: &str) -> CommitGraphState {
    let git_dir = dir.join(".git");
    std::fs::write(git_dir.join("HEAD"), head).unwrap();
    inspect(dir, &git_dir, &git_dir).state
}

#[test]
fn valid_heads_resolve_identically_in_either_case_for_sha1_and_sha256() {
    let _g = serial();
    for sha256 in [false, true] {
        let Some(dir) = graph_repo(sha256) else {
            eprintln!("git lacks sha256 support; case skipped");
            continue;
        };
        let oid = std::fs::read_to_string(dir.path().join(".git/refs/heads/main"))
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(oid.len(), if sha256 { 64 } else { 40 });
        let symbolic = state_with_head(dir.path(), "ref: refs/heads/main\n");
        assert_eq!(symbolic, CommitGraphState::Present, "sha256={sha256}");
        for head in [
            format!("{oid}\n"),
            format!("{}\n", oid.to_uppercase()),
            oid.clone(),
        ] {
            assert_eq!(
                state_with_head(dir.path(), &head),
                CommitGraphState::Present,
                "sha256={sha256} head={head:?}"
            );
        }
        // An unborn-looking all-zero id of the right length is a well-formed id
        // naming no commit: it must not be called Present.
        let zero = "0".repeat(oid.len());
        assert_ne!(
            state_with_head(dir.path(), &zero),
            CommitGraphState::Present
        );
    }
}

/// `resolve_head` calls `decode_hex` BEFORE `is_hex_hash` at the loose-ref
/// site (commit_graph.rs:466) and in packed-refs; hostile text there must be
/// refused, not panic.
#[test]
fn non_ascii_loose_and_packed_refs_and_chain_files_do_not_panic() {
    let _g = serial();
    let dir = graph_repo(false).unwrap();
    let git_dir = dir.path().join(".git");
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    for body in ["日日\n", "aé\n", "é\n", "ab\u{301}c\n"] {
        std::fs::write(git_dir.join("refs/heads/main"), body).unwrap();
        let state = inspect(dir.path(), &git_dir, &git_dir).state;
        assert!(
            matches!(state, CommitGraphState::Unknown),
            "loose ref {body:?}: {state:?}"
        );
    }
    std::fs::remove_file(git_dir.join("refs/heads/main")).unwrap();
    for line in [
        "日日 refs/heads/main\n",
        &format!("{}é refs/heads/main\n", "a".repeat(39)),
    ] {
        std::fs::write(git_dir.join("packed-refs"), line).unwrap();
        let _ = inspect(dir.path(), &git_dir, &git_dir);
    }
    let chain = git_dir.join("objects/info/commit-graphs");
    std::fs::create_dir_all(&chain).unwrap();
    for body in ["日日\n", &format!("{}é\n", "a".repeat(39)), "é\n"] {
        std::fs::write(chain.join("commit-graph-chain"), body).unwrap();
        let _ = inspect(dir.path(), &git_dir, &git_dir);
    }
}
