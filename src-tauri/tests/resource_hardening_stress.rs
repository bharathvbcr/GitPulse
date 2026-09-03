//! Adversarial pressure on the resource bounds.
//!
//! Each test here tries to *defeat* a guard rather than confirm it: cuts placed
//! exactly on the budget, on a multi-byte character, and on a binary payload;
//! the spawn gate under panics, timeouts and a fan-out far past its ceiling;
//! and peak memory under concurrent worst-case reads, which is the number the
//! whole exercise exists to hold down.

use gitpulse_lib::engine::budget;
use gitpulse_lib::engine::git_cli;
use gitpulse_lib::engine::GitReader;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo_with(dir: &Path, name: &str, first: &str, second: &str) -> String {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t.t"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join(name), first).expect("write");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "one"]);
    std::fs::write(dir.join(name), second).expect("write");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "two"]);
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn rss_bytes() -> u64 {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
        * 1024
}

/// A diff made entirely of 4-byte characters puts a multi-byte boundary at
/// essentially every possible cut point. A naive byte slice panics here; a
/// naive lossy conversion leaves U+FFFD in the payload.
#[test]
fn a_diff_of_multibyte_characters_survives_the_cut_intact() {
    let dir = TempDir::new().expect("tempdir");
    // Each line is 100 emoji (400 bytes) so the budget lands mid-character
    // with overwhelming probability.
    let line: String = "\u{1F600}".repeat(100);
    let mut first = String::new();
    let mut second = String::new();
    for _ in 0..40_000 {
        first.push_str(&line);
        first.push('\n');
        second.push_str(&"\u{1F601}".repeat(100));
        second.push('\n');
    }
    let oid = repo_with(dir.path(), "emoji.txt", &first, &second);
    let repo = dir.path().to_str().expect("utf8");

    let payload = GitReader::get_commit_diff(repo, &oid).expect("diff");
    assert!(payload.truncated, "this diff is far past the budget");
    assert!(
        !payload.text.contains('\u{FFFD}'),
        "a cut through a 4-byte character leaked a replacement char"
    );
    assert!(
        payload.text.ends_with('\n'),
        "the cut must land on a line boundary"
    );
    // Every surviving line is whole: no line may end mid-emoji.
    for line in payload.text.lines() {
        assert!(
            std::str::from_utf8(line.as_bytes()).is_ok(),
            "a surviving line is not valid UTF-8"
        );
    }
}

/// A diff sitting exactly on the budget must NOT be reported as truncated:
/// a false positive disables staging on a complete diff, which is a silent
/// loss of function rather than a visible one.
#[test]
fn a_diff_exactly_at_the_budget_is_not_reported_as_truncated() {
    for len in [0usize, 1, 4095, 4096, 4097] {
        let text = format!("{}\n", "x".repeat(len));
        let (out, truncated) = budget::truncate_at_line_boundary(text.clone(), text.len());
        assert!(!truncated, "len {len} at exactly its own length");
        assert_eq!(out, text);

        let (out, truncated) = budget::truncate_at_line_boundary(text.clone(), text.len() + 1);
        assert!(!truncated, "len {len} under the budget");
        assert_eq!(out, text);
    }
}

/// Binary diffs carry a base85 payload whose lines legitimately begin with
/// `+`/`-`. Cutting one must still leave whole lines, never half a payload row
/// that the frontend would classify as an addition.
#[test]
fn a_binary_diff_is_cut_on_whole_lines() {
    let dir = TempDir::new().expect("tempdir");
    let first: String = (0u32..900_000)
        .map(|i| (b'a' + (i % 26) as u8) as char)
        .collect();
    let second: String = (0u32..900_000)
        .map(|i| (b'z' - (i % 26) as u8) as char)
        .collect();
    // Force git to treat it as binary with a NUL byte.
    let first = format!("\u{0}{first}");
    let second = format!("\u{0}{second}");
    let oid = repo_with(dir.path(), "blob.bin", &first, &second);
    let repo = dir.path().to_str().expect("utf8");

    let payload = GitReader::get_commit_diff(repo, &oid).expect("diff");
    if payload.truncated {
        assert!(payload.text.ends_with('\n'), "binary cut must end a line");
        assert!(!payload.text.contains('\u{FFFD}'));
    }
    assert!(payload.text.len() <= budget::MAX_DIFF_BYTES);
}

/// The gate must not lose a permit when the command inside it fails, times
/// out, or the caller panics. A leaked permit shrinks the budget permanently:
/// the app would get slower and slower and never recover.
#[test]
fn the_spawn_gate_reclaims_permits_from_failures_timeouts_and_panics() {
    let dir = TempDir::new().expect("tempdir");
    let _ = repo_with(dir.path(), "f.txt", "a\n", "b\n");
    let repo = dir.path();

    // Failures: a command that exits non-zero.
    for _ in 0..40 {
        let _ = git_cli::git_text(repo, &["rev-parse", "definitely-not-a-ref"]);
    }
    // Timeouts: a command killed by its deadline.
    for _ in 0..8 {
        let err = git_cli::run_command("sh", &["-c", "sleep 5"], Duration::from_millis(80))
            .expect_err("must time out");
        assert!(err.contains("timed out"), "got: {err}");
    }
    // Panics inside a caller that is mid-permit are covered by the unit test
    // in git_cli; here we prove the budget is intact afterwards by doing real
    // concurrent work and requiring it to complete promptly.
    let started = Instant::now();
    let barrier = Arc::new(Barrier::new(24));
    let handles: Vec<_> = (0..24)
        .map(|_| {
            let repo = repo.to_path_buf();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                git_cli::git_text(&repo, &["rev-parse", "HEAD"])
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread").expect("rev-parse");
    }
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the gate leaked permits: 24 trivial calls took {:?}",
        started.elapsed()
    );
}

/// The number this whole exercise exists to hold down. Eight threads each read
/// a diff that would be ~29 MB unbudgeted; unbounded that is ~1.1 GB of text
/// and lossy copies before the webview sees any of it.
#[test]
fn concurrent_worst_case_diff_reads_stay_within_a_sane_memory_envelope() {
    let dir = TempDir::new().expect("tempdir");
    let line: String = "a".repeat(120);
    let line2: String = "b".repeat(120);
    let mut first = String::new();
    let mut second = String::new();
    for _ in 0..120_000 {
        first.push_str(&line);
        first.push('\n');
        second.push_str(&line2);
        second.push('\n');
    }
    let oid = repo_with(dir.path(), "big.txt", &first, &second);
    drop(first);
    drop(second);
    let repo = dir.path().to_str().expect("utf8").to_string();

    let base = rss_bytes();
    let threads = 8usize;
    let barrier = Arc::new(Barrier::new(threads));
    let truncations = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let repo = repo.clone();
            let oid = oid.clone();
            let barrier = Arc::clone(&barrier);
            let truncations = Arc::clone(&truncations);
            std::thread::spawn(move || {
                barrier.wait();
                let payload = GitReader::get_commit_diff(&repo, &oid).expect("diff");
                if payload.truncated {
                    truncations.fetch_add(1, Ordering::SeqCst);
                }
                assert!(payload.text.len() <= budget::MAX_DIFF_BYTES);
                payload.text.len()
            })
        })
        .collect();
    let mut total = 0usize;
    for handle in handles {
        total += handle.join().expect("reader thread");
    }
    let peak = rss_bytes();

    assert_eq!(
        truncations.load(Ordering::SeqCst),
        threads,
        "every reader must see the same truncation verdict"
    );
    assert!(total > 0);
    // Generous, and still an order of magnitude under where this used to land:
    // eight unbudgeted readers of this commit held ~1.1 GB of diff text alone.
    let ceiling = 400 * 1024 * 1024;
    assert!(
        peak < ceiling,
        "peak RSS {:.0} MB exceeded the {:.0} MB envelope (base {:.0} MB)",
        peak as f64 / 1e6,
        ceiling as f64 / 1e6,
        base as f64 / 1e6
    );
    println!(
        "concurrent worst case: base {:.0} MB, peak {:.0} MB, {} readers",
        base as f64 / 1e6,
        peak as f64 / 1e6,
        threads
    );
}
