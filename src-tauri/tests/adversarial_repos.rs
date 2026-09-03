//! Adversarial repository fixtures for the engine seam.
//!
//! Each test builds a real repository through `git` itself and drives the
//! public `GitReader`/`GitWriter` API against hostile shapes: missing trailing
//! newlines, binary blobs at the size cap, symlink loops, nested tags, empty
//! and orphan repositories, corrupted `.git` state, concurrent write storms,
//! and filesystem-event storms. Expectations were pinned against the current
//! implementation on this branch; where git's actual behavior diverges from
//! intuition it is called out in a comment.

use gitpulse_lib::analyzer::loc_counter::DiffChurn;
use gitpulse_lib::engine::git_cli::resolve_git_dir;
#[cfg(unix)]
use gitpulse_lib::engine::git_cli::sandbox_join_canonical;
use gitpulse_lib::engine::git_writer::validate_ref_name;
use gitpulse_lib::engine::{GitReader, GitWriter};
use std::fs::File;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn git_in(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            "user.email=gitpulse@test.local",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Repo with one seed commit on `main`.
fn init_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    git_in(dir.path(), &["init", "-q", "-b", "main"]);
    git_in(dir.path(), &["config", "user.name", "GitPulse"]);
    git_in(dir.path(), &["config", "user.email", "gitpulse@test.local"]);
    git_in(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").unwrap();
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

/// Repo with zero commits.
fn init_empty_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    git_in(dir.path(), &["init", "-q", "-b", "main"]);
    git_in(dir.path(), &["config", "user.name", "GitPulse"]);
    git_in(dir.path(), &["config", "user.email", "gitpulse@test.local"]);
    git_in(dir.path(), &["config", "commit.gpgsign", "false"]);
    dir
}

const FAKE_OID: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn no_trailing_newline_commit_round_trips_cleanly() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();

    std::fs::write(repo.path().join("x.rs"), "fn a() {}\nfn b()").unwrap(); // no \n
    GitWriter::commit_files(path, "one", &["x.rs".to_string()]).expect("commit one");
    std::fs::write(repo.path().join("x.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    GitWriter::commit_files(path, "two", &["x.rs".to_string()]).expect("commit two");
    let two = GitReader::head_id(path).expect("head_id");

    // The raw patch keeps the marker and never panics the parser seam.
    let diff = GitReader::get_commit_diff(path, &two)
        .expect("commit diff")
        .text;
    assert!(diff.contains("\\ No newline at end of file"));
    assert!(diff.contains("+fn b() {}"));

    let files = GitReader::get_commit_files(path, &two).expect("commit files");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "x.rs");
    assert_eq!((files[0].additions, files[0].deletions), (1, 1));

    // Unstaged and staged worktree diffs over the same shape stay clean.
    std::fs::write(repo.path().join("x.rs"), "fn a() {}\nfn c() {}\n").unwrap();
    let wt = GitReader::get_file_diff(path, "x.rs", false, false)
        .expect("worktree diff")
        .text;
    assert!(wt.contains("-fn b()"));
    GitWriter::stage_file(path, "x.rs").expect("stage");
    let idx = GitReader::get_file_diff(path, "x.rs", true, false)
        .expect("staged diff")
        .text;
    assert!(idx.contains("+fn c()"));

    // The shortstat grammar this project parses (loc_counter) reads real
    // output for the same range without losing fields.
    let out = Command::new("git")
        .args(["diff", "--shortstat", "HEAD~1", "HEAD"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout).to_string();
    let churn = DiffChurn::parse_shortstat(&stat);
    // one deletion ("fn b()") replaced by one addition ("fn b() {}")
    assert_eq!(
        (churn.files_changed, churn.additions, churn.deletions),
        (1, 1, 1)
    );
}

#[test]
fn binary_blob_and_oversize_cap_fail_closed_not_fatal() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();

    // Deterministic pseudo-random 5 MB payload (xorshift32), well under the
    // 64 MiB working-tree cap: the blob path must succeed and classify binary.
    let mut state: u32 = 0x9E3779B9;
    let mut bytes = vec![0u8; 5 * 1024 * 1024];
    for b in bytes.iter_mut() {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *b = (state & 0xFF) as u8;
    }
    // Guarantee a NUL byte so binary detection cannot miss.
    bytes[0] = 0;
    std::fs::write(repo.path().join("bin5m.dat"), &bytes).unwrap();
    GitWriter::commit_files(path, "binary", &["bin5m.dat".to_string()]).unwrap();

    let blob = GitReader::get_file_blob(path, "bin5m.dat", None).expect("blob read");
    assert!(blob.is_binary);
    assert!(blob.text.is_none());
    assert!(!blob.base64.unwrap().is_empty());

    let commit = GitReader::head_id(path).unwrap();
    let _diff = GitReader::get_commit_diff(path, &commit)
        .expect("diff")
        .text;
    let files = GitReader::get_commit_files(path, &commit).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "bin5m.dat");

    // Sparse 65 MiB file (> MAX_WORKING_TREE_BYTES): committing is fine
    // (zeros compress away), but the blob reader must refuse via stat BEFORE
    // reading — Err with the size-limit message, never an OOM or panic.
    let big = File::create(repo.path().join("big.bin")).unwrap();
    big.set_len(65 * 1024 * 1024).unwrap();
    drop(big);
    GitWriter::commit_files(path, "big", &["big.bin".to_string()])
        .expect("git handles sparse commits fine");
    let started = Instant::now();
    let err = GitReader::get_file_blob(path, "big.bin", None).unwrap_err();
    assert!(
        err.contains("size limit"),
        "expected size-cap error, got: {err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cap check must be stat-fast"
    );
}

#[test]
fn exotic_path_survives_status_numstat_diff_blame_end_to_end() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    let rel = "sp ace/uniﬁé😀.txt";

    std::fs::create_dir_all(repo.path().join("sp ace")).unwrap();
    std::fs::write(repo.path().join(rel), "salut 😀\n").unwrap();
    GitWriter::stage_file(path, rel).expect("stage exotic path");
    GitWriter::commit_files(path, "exotic seed", &[rel.to_string()]).expect("commit");
    std::fs::write(repo.path().join(rel), "salut 😀\nà bientôt\n").unwrap();

    let statuses = GitReader::get_status(path).expect("status");
    let entry = statuses
        .iter()
        .find(|s| s.path == rel)
        .expect("raw UTF-8 path must round-trip porcelain -z");
    assert!(!entry.is_staged);
    assert_eq!((entry.additions, entry.deletions), (1, 0));
    assert_eq!(entry.old_path, None);

    let diff = GitReader::get_file_diff(path, rel, false, false)
        .expect("file diff")
        .text;
    assert!(diff.contains("@@"), "diff must contain a hunk: {diff}");

    let blame = GitReader::get_file_blame(path, rel).expect("blame");
    // CHARACTERIZED: git blames the WORKTREE file, so the committed line plus
    // the uncommitted append both appear (the latter attributed to
    // "Not Committed Yet").
    assert_eq!(blame.len(), 2);
    assert_eq!(blame[0].content, "salut 😀");
    assert_eq!(blame[1].content, "à bientôt");
}

#[cfg(unix)]
#[test]
fn symlink_loop_is_rejected_cleanly_everywhere() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    std::os::unix::fs::symlink("loop", repo.path().join("loop")).expect("self-symlink");

    // The canonical sandbox join must Err (ELOOP or containment check) —
    // never loop forever, never panic.
    let started = Instant::now();
    let result = sandbox_join_canonical(repo.path(), "loop/file.txt");
    assert!(result.is_err(), "symlink loop must not resolve");
    assert!(started.elapsed() < Duration::from_secs(5));

    // Every reader that funnels through it fails loudly instead of panicking.
    assert!(GitReader::get_file_blame(path, "loop/f.txt").is_err());
    assert!(GitReader::get_file_diff(path, "loop/f.txt", false, false).is_err());
    assert!(GitReader::get_file_blob(path, "loop/f.txt", None).is_err());

    // A healthy path right next to the loop still works.
    std::fs::write(repo.path().join("ok.txt"), "fine\n").unwrap();
    assert!(sandbox_join_canonical(repo.path(), "ok.txt").is_ok());
}

#[test]
fn ref_name_validation_rejects_hostile_branch_names_without_side_effects() {
    assert!(validate_ref_name("weird..name--dashes").is_err()); // '..' traversal
    assert!(validate_ref_name("-dash").is_err()); // option injection

    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    let before = GitReader::list_branches(path).unwrap().len();
    assert!(GitWriter::create_branch(path, "weird..name--dashes", None).is_err());
    assert!(GitWriter::create_branch(path, "-dash", None).is_err());
    assert_eq!(GitReader::list_branches(path).unwrap().len(), before);
}

#[test]
fn nested_tag_peels_without_panic() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    std::fs::write(repo.path().join("f.txt"), "l1\nl2\n").unwrap();
    GitWriter::commit_files(path, "content", &["f.txt".to_string()]).unwrap();
    GitWriter::create_tag(path, "v1", None, Some("first")).unwrap();

    // Tag the TAG: pass v1's tag object id as the start point. (create_tag
    // runs the start point through validate_oid, so a symbolic "v1" is
    // rejected by design; the object id produces a true nested tag.)
    let v1 = GitReader::list_tags(path)
        .unwrap()
        .tags
        .into_iter()
        .find(|t| t.name == "v1")
        .unwrap();
    GitWriter::create_tag(path, "v2", Some(&v1.commit_id), Some("points at v1"))
        .expect("nested tag creation");

    let tags = GitReader::list_tags(path).expect("list tags");
    assert!(!tags.truncated);
    assert_eq!(tags.tags.len(), 2);
    let names: Vec<_> = tags.tags.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"v1") && names.contains(&"v2"));
    for tag in &tags.tags {
        assert_eq!(tag.commit_id.len(), 40);
    }

    // Reads that peel through the outer tag all succeed.
    let head = GitReader::head_id(path).unwrap();
    assert!(GitReader::get_commit_diff(path, &head).is_ok());
    let files = GitReader::get_commit_files(path, &head).unwrap();
    assert_eq!(files.len(), 1);
    assert!(GitReader::get_file_blame(path, "f.txt").is_ok());
}

#[test]
fn empty_repo_reader_contract() {
    let repo = init_empty_repo();
    let path = repo.path().to_str().unwrap();

    // CHARACTERIZED: modern git exits 0 for `log --all` with no refs, so the
    // uncapped walk reports Ok(empty) rather than Err.
    assert!(GitReader::read_commit_history(path, 50, None)
        .expect("--all history on an empty repo is Ok(vec![])")
        .is_empty());
    // An explicit dead revision fails cleanly.
    assert!(GitReader::read_commit_history(path, 50, Some("HEAD")).is_err());
    assert!(GitReader::head_id(path).is_err());

    // Status/diff need no commits and answer normally.
    assert!(GitReader::get_status(path)
        .expect("status works pre-first-commit")
        .is_empty());
    assert_eq!(
        GitReader::get_file_diff(path, "seed.txt", false, false)
            .expect("empty diff")
            .text,
        ""
    );
    assert!(GitReader::list_tags(path)
        .expect("no tags yet")
        .tags
        .is_empty());

    // Everything anchored to objects/refs errors without panicking.
    assert!(GitReader::get_file_blame(path, "seed.txt").is_err());
    assert!(GitReader::get_commit_files(path, FAKE_OID).is_err());
    assert!(GitReader::get_commit_diff(path, FAKE_OID).is_err());
    assert!(GitReader::get_reflog(path, 10).is_err());
}

#[test]
fn orphan_head_reader_contract() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    git_in(repo.path(), &["checkout", "-q", "--orphan", "fresh"]);

    // Same shape as the empty repo for HEAD-anchored reads: unborn branch.
    // But --orphan leaves `main` (and every other ref) intact, so unlike the
    // truly-empty repo, an --all walk still finds the seed commit.
    let history =
        GitReader::read_commit_history(path, 50, None).expect("orphan --all history stays Ok");
    assert_eq!(history.len(), 1); // seed commit, reachable via refs/heads/main
    assert!(GitReader::read_commit_history(path, 50, Some("HEAD")).is_err());
    assert!(GitReader::head_id(path).is_err());
    assert!(
        GitReader::get_status(path).is_ok(),
        "status survives unborn HEAD"
    );
    assert!(GitReader::get_file_blame(path, "seed.txt").is_err());
    assert!(GitReader::get_commit_files(path, FAKE_OID).is_err());
}

#[test]
fn corrupted_head_errors_cleanly_never_panics() {
    let repo = init_repo();
    let path = repo.path().to_str().unwrap();
    std::fs::write(repo.path().join(".git/HEAD"), "").unwrap();

    // validate_repo still accepts the directory (.git exists), so every
    // command reaches git and must surface Err — not a panic, not fake data.
    assert!(GitReader::get_status(path).is_err());
    assert!(GitReader::read_commit_history(path, 50, None).is_err());
    assert!(GitReader::get_file_diff(path, "seed.txt", false, false).is_err());
    assert!(GitReader::get_file_blame(path, "seed.txt").is_err());
    assert!(GitReader::get_commit_files(path, FAKE_OID).is_err());
    assert!(GitReader::get_commit_diff(path, FAKE_OID).is_err());
    assert!(GitReader::head_id(path).is_err());
    assert!(GitReader::list_tags(path).is_err());
    assert!(GitReader::get_reflog(path, 10).is_err());
    assert!(GitWriter::stage_file(path, "seed.txt").is_err());
}

#[test]
fn eight_thread_stage_commit_hammer_finishes_without_deadlock() {
    let repo = init_repo();
    let path = std::sync::Arc::new(repo.path().to_str().unwrap().to_string());
    const THREADS: usize = 8;
    const ROUNDS: usize = 4;

    // commit_files stages+commits under ONE mutation-lock acquisition, the
    // documented programmatic seam for exactly this concurrency.
    let barrier = Arc::new(Barrier::new(THREADS));
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let path = path.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            let root = Path::new(&*path);
            let mut results = Vec::with_capacity(ROUNDS);
            for r in 0..ROUNDS {
                let rel = format!("hammer/t{t}/r{r}.txt");
                std::fs::create_dir_all(root.join(format!("hammer/t{t}"))).unwrap();
                std::fs::write(root.join(&rel), format!("{t}:{r}\n")).unwrap();
                results.push(GitWriter::commit_files(
                    &path,
                    &format!("hammer {t}-{r}"),
                    &[rel],
                ));
            }
            results
        }));
    }

    let started = Instant::now();
    let mut committed = 0usize;
    for handle in handles {
        let results = handle.join().expect("worker thread must not panic");
        for result in results {
            match result {
                Ok(_) => committed += 1,
                Err(err) => panic!("unexpected hammer failure: {err}"),
            }
        }
    }
    let elapsed = started.elapsed();
    assert_eq!(
        committed,
        THREADS * ROUNDS,
        "every atomic stage+commit lands"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "hammer deadlocked or starved: {elapsed:?}"
    );

    // Repo stays coherent afterwards.
    let history = GitReader::read_commit_history(&path, 100, None).unwrap();
    assert!(history.len() >= THREADS * ROUNDS);
    assert!(GitReader::get_status(&path).is_ok());
}

#[test]
fn watcher_storm_keeps_session_responsive() {
    let repo = init_repo();
    let path = repo.path();
    let git_dir = resolve_git_dir(path).expect("resolve git dir");

    let watcher = gitpulse_lib::watcher::RepoFileWatcher::watch_repo(&git_dir, Some(path), None)
        .expect("watcher starts");
    let receiver = &watcher.receiver;
    // Give the OS backend time to install its watches before the storm.
    std::thread::sleep(Duration::from_millis(300));

    // Warmup handshake: prove the backend stream is live before any timing
    // assertion. macOS FSEvents can start late or stall under load; without
    // this, that startup race reads as a wedge.
    std::fs::write(path.join(".gitpulse-warmup"), "warm").unwrap();
    let _ = receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("watcher must deliver a warmup event once live");
    while receiver.try_recv().is_ok() {}

    // 200 rapid create/delete pairs at the watched (non-recursive) root.
    // Drain opportunistically between chunks so the queue cannot wedge, the
    // way run_watch_loop drains with try_iter.
    for i in 0..200 {
        let p = path.join(format!("storm_{i}.txt"));
        std::fs::write(&p, "x").unwrap();
        let _ = std::fs::remove_file(&p);
        if i % 40 == 39 {
            while let Ok(_ev) = receiver.recv_timeout(Duration::from_millis(1)) {}
        }
    }

    // After the storm the session must still deliver: one new file surfaces
    // within the debounce budget (< 5s rule; FSEvents adds latency).
    std::fs::write(path.join("sentinel_after_storm.txt"), "done").unwrap();
    let started = Instant::now();
    let mut saw_event_after_sentinel = false;
    while started.elapsed() < Duration::from_secs(10) {
        match receiver.recv_timeout(Duration::from_millis(500)) {
            Ok(_) => {
                saw_event_after_sentinel = true;
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("watcher channel disconnected during storm recovery")
            }
        }
    }
    assert!(
        saw_event_after_sentinel,
        "watcher wedged: no event within 10s of post-storm activity"
    );
    drop(watcher); // Drop impl flips the stop flag; must not hang.
}
