//! No claim is silently lost. Every append that returned `Ok` has to be in the
//! store at the end, however many hook processes were writing, whether a
//! compaction or a repair ran meanwhile, and whatever else was in the file.
//!
//! Each child process is this test binary re-run with environment variables,
//! because the hook is one process per edit and the lock that matters is the
//! file lock between processes, not a mutex.

use std::fs;
use std::io::Write;
use std::path::Path;

use gitpulse_lib::insights::claims::{
    append_claim, append_claims, append_claims_repairing, read_store, ClaimSource, FileClaim,
    DEFAULT_CAP, DEFAULT_TTL_MS,
};
use gitpulse_lib::procguard::LockedSpawn;

const T0: u64 = 1_794_110_184_306;
const COMPACT_BYTES_PER_ROW: u64 = 512;
const WORKTREE: &str = "/Users/bharath/Code/devtools/GitPulse/.gitpulse/worktrees/b1106w9-claims";
const CHILDREN: usize = 6;
const APPENDS: usize = 120;
const BATCH_AT: usize = 60;
const BATCH_ROWS: usize = 64;
const TORN_FRAGMENT: &[u8] = b"{\"source\":\"observed\",\"sess";

fn observed(tag: &str, i: usize, ts: u64) -> FileClaim {
    FileClaim {
        source: ClaimSource::Observed,
        session_id: Some(format!("0b6c7f3e-1d2a-4c5b-9e8f-{tag}")),
        run_id: None,
        path: format!("src-tauri/src/insights/{tag}_{i:05}.rs"),
        worktree: WORKTREE.into(),
        ts_ms: ts,
        cwd: Some(WORKTREE.into()),
    }
}

fn declared(id: usize, k: usize, j: usize, pad: usize, ts: u64) -> FileClaim {
    FileClaim {
        source: ClaimSource::Declared,
        session_id: None,
        run_id: Some(format!("run-{id}-{k}")),
        path: format!("src/declared/{id}_{k}_{j:03}_{}.rs", "x".repeat(pad)),
        worktree: "/Users/bharath/Code/devtools/GitPulse/.gitpulse/worktrees/lane".into(),
        ts_ms: ts,
        cwd: None,
    }
}

fn fill_to_threshold(store: &Path) -> usize {
    let threshold = DEFAULT_CAP as u64 * COMPACT_BYTES_PER_ROW;
    let mut rows = 0usize;
    while fs::metadata(store).map(|m| m.len()).unwrap_or(0) + 400 < threshold {
        let claim = observed("fill", rows, T0 + rows as u64);
        append_claim(store, &claim, DEFAULT_CAP, DEFAULT_TTL_MS, T0).unwrap();
        rows += 1;
    }
    rows
}

fn child_main(store: &Path) {
    let id: usize = std::env::var("W9R4_CHILD_ID").unwrap().parse().unwrap();
    let pad: usize = std::env::var("W9R4_PAD").unwrap().parse().unwrap();
    let torn = std::env::var("W9R4_TORN").is_ok();
    let mut failed = 0;
    for k in 0..APPENDS {
        let ts = T0 + 1_000_000 + (id * 10_000 + k) as u64;
        if k == BATCH_AT {
            let batch: Vec<FileClaim> = (0..BATCH_ROWS)
                .map(|j| declared(id, k, j, pad, ts))
                .collect();
            if let Err(error) =
                append_claims_repairing(store, &batch, DEFAULT_CAP, DEFAULT_TTL_MS, ts)
            {
                failed += BATCH_ROWS;
                println!("W9R4_ERR=batch of {BATCH_ROWS}: {error}");
            }
            continue;
        }
        if torn && k % 40 == 7 {
            // A writer that crashed mid-row leaves its first half behind.
            let mut file = fs::OpenOptions::new().append(true).open(store).unwrap();
            file.write_all(TORN_FRAGMENT).unwrap();
        }
        let claim = observed(&format!("c{id}"), k, ts);
        if let Err(error) = append_claim(store, &claim, DEFAULT_CAP, DEFAULT_TTL_MS, ts) {
            failed += 1;
            println!("W9R4_ERR=append {k}: {error}");
        }
    }
    println!("W9R4_FAILED={failed}");
}

/// Six processes, 120 appends each (one of them a 64-row declared batch), on a
/// store at its compaction threshold, so compactions and repairs run while
/// everything else is appending. The reviewer's run lost whole batches and cut
/// a row's start off, every append having returned `Ok`.
///
/// Without injected torn tails every claim must be present and no line
/// skipped. With them, a fragment may swallow at most the one row appended
/// straight after it, so at most one claim per fragment is missing, and the
/// reader counts what it skipped.
fn scenario(name: &str, pad: usize, torn: bool) {
    if let Ok(store) = std::env::var("W9R4_CHILD_STORE") {
        child_main(Path::new(&store));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("gitpulse").join("file-claims.jsonl");
    let filled = fill_to_threshold(&store);
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..CHILDREN)
        .map(|id| {
            let mut command = std::process::Command::new(&exe);
            command
                .args([name, "--exact", "--nocapture", "--test-threads=1"])
                .env("W9R4_CHILD_STORE", &store)
                .env("W9R4_CHILD_ID", id.to_string())
                .env("W9R4_PAD", pad.to_string())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            if torn {
                command.env("W9R4_TORN", "1");
            }
            command.spawn_locked().unwrap()
        })
        .collect();
    let mut failed = 0usize;
    for child in children {
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        for line in text
            .lines()
            .filter_map(|line| line.split("W9R4_ERR=").nth(1))
        {
            eprintln!("{name}: child append error: {line}");
        }
        let at = text
            .find("W9R4_FAILED=")
            .unwrap_or_else(|| panic!("a child printed no count: {text}"));
        let digits: String = text[at + 12..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        failed += digits.parse::<usize>().unwrap();
    }
    let read = read_store(&store, T0 + 2_000_000, DEFAULT_TTL_MS, DEFAULT_CAP).unwrap();
    let present: std::collections::HashSet<&str> =
        read.rows.iter().map(|row| row.path.as_str()).collect();
    let mut missing = Vec::new();
    for id in 0..CHILDREN {
        for k in 0..APPENDS {
            if k == BATCH_AT {
                for j in 0..BATCH_ROWS {
                    let path = declared(id, k, j, pad, 0).path;
                    if !present.contains(path.as_str()) {
                        missing.push(format!("declared child {id} row {j}"));
                    }
                }
                continue;
            }
            let path = format!("src-tauri/src/insights/c{id}_{k:05}.rs");
            if !present.contains(path.as_str()) {
                missing.push(format!("observed child {id} append {k}"));
            }
        }
    }
    let fragments = if torn { CHILDREN * 3 } else { 0 };
    eprintln!(
        "{name}: fill {filled}, append errors {failed}, rows {}, skipped {}, dropped {}, \
         missing {} (allowed {fragments}), size {} B",
        read.rows.len(),
        read.skipped,
        read.dropped,
        missing.len(),
        fs::metadata(&store).unwrap().len()
    );
    assert_eq!(failed, 0, "an append returned an error");
    assert!(
        missing.len() <= fragments,
        "{} claims were lost with every append Ok (allowed {fragments}): {:?}",
        missing.len(),
        &missing[..missing.len().min(8)]
    );
    if !torn {
        assert_eq!(
            read.skipped, 0,
            "a line was cut or torn with no torn tail injected"
        );
    }
}

#[cfg(unix)]
#[test]
fn no_claim_is_lost_under_compaction_and_repair_with_long_rows() {
    scenario(
        "no_claim_is_lost_under_compaction_and_repair_with_long_rows",
        1700,
        false,
    );
}

#[cfg(unix)]
#[test]
fn no_claim_is_lost_under_compaction_and_repair_with_short_rows() {
    scenario(
        "no_claim_is_lost_under_compaction_and_repair_with_short_rows",
        100,
        false,
    );
}

#[cfg(unix)]
#[test]
fn a_crashed_writers_fragment_costs_at_most_the_row_after_it_with_long_rows() {
    scenario(
        "a_crashed_writers_fragment_costs_at_most_the_row_after_it_with_long_rows",
        1700,
        true,
    );
}

#[cfg(unix)]
#[test]
fn a_crashed_writers_fragment_costs_at_most_the_row_after_it_with_short_rows() {
    scenario(
        "a_crashed_writers_fragment_costs_at_most_the_row_after_it_with_short_rows",
        100,
        true,
    );
}

/// Lock-free readers while shared-lock appenders write ~10 KB buffers: they may
/// see a cut last row (a write in flight, which the reader counts), but never
/// NUL bytes or a terminated line that does not parse.
#[cfg(unix)]
#[test]
fn a_lock_free_read_never_sees_nul_bytes_or_a_terminated_bad_line() {
    let name = "a_lock_free_read_never_sees_nul_bytes_or_a_terminated_bad_line";
    let big_cap = 1_000_000usize;
    if let Ok(store) = std::env::var("W9R4P_CHILD_STORE") {
        let id: usize = std::env::var("W9R4P_CHILD_ID").unwrap().parse().unwrap();
        for k in 0..100usize {
            let ts = T0 + k as u64;
            let batch: Vec<FileClaim> = (0..16)
                .map(|j| {
                    let mut claim = observed(&format!("p{id}"), k * 16 + j, ts);
                    claim.path = format!("{}{}", claim.path, "z".repeat(600));
                    claim
                })
                .collect();
            append_claims(Path::new(&store), &batch, big_cap, DEFAULT_TTL_MS, ts).unwrap();
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("gitpulse").join("file-claims.jsonl");
    append_claim(
        &store,
        &observed("seed", 0, T0),
        big_cap,
        DEFAULT_TTL_MS,
        T0,
    )
    .unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut children: Vec<_> = (0..4usize)
        .map(|id| {
            std::process::Command::new(&exe)
                .args([name, "--exact", "--nocapture", "--test-threads=1"])
                .env("W9R4P_CHILD_STORE", &store)
                .env("W9R4P_CHILD_ID", id.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn_locked()
                .unwrap()
        })
        .collect();
    let (mut reads, mut nul, mut bad_terminated) = (0usize, 0usize, 0usize);
    loop {
        let done = children
            .iter_mut()
            .all(|child| child.try_wait().unwrap().is_some());
        let bytes = fs::read(&store).unwrap();
        reads += 1;
        nul += usize::from(bytes.contains(&0u8));
        let segments: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
        let complete = segments.len().saturating_sub(1);
        bad_terminated += usize::from(
            segments
                .iter()
                .take(complete)
                .any(|s| !s.is_empty() && serde_json::from_slice::<FileClaim>(s).is_err()),
        );
        if done {
            break;
        }
    }
    let last = read_store(&store, T0, DEFAULT_TTL_MS, big_cap).unwrap();
    eprintln!(
        "{reads} lock-free reads: {nul} with NUL bytes, {bad_terminated} with a bad terminated line; \
         final {} rows, skipped {}",
        last.rows.len(),
        last.skipped
    );
    assert_eq!((nul, bad_terminated), (0, 0));
    assert_eq!(last.skipped, 0, "{last:?}");
    assert_eq!(last.rows.len(), 1 + 4 * 100 * 16);
}

/// A read creates nothing, and with a degraded store present it leaves every
/// byte and every file as it was.
#[test]
fn a_read_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("gitpulse").join("file-claims.jsonl");
    let read = read_store(&store, T0, DEFAULT_TTL_MS, DEFAULT_CAP).unwrap();
    assert!(read.rows.is_empty());
    assert!(
        !dir.path().join("gitpulse").exists(),
        "a read created the store directory"
    );
    append_claim(
        &store,
        &observed("r", 1, T0),
        DEFAULT_CAP,
        DEFAULT_TTL_MS,
        T0,
    )
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&store)
        .unwrap()
        .write_all(b"garbage\n")
        .unwrap();
    let listing = || {
        let mut names: Vec<String> = fs::read_dir(store.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let (bytes, files) = (fs::read(&store).unwrap(), listing());
    let read = read_store(&store, T0, DEFAULT_TTL_MS, DEFAULT_CAP).unwrap();
    assert_eq!(read.skipped, 1);
    assert_eq!(fs::read(&store).unwrap(), bytes);
    assert_eq!(listing(), files);
}

/// A torn line is closed and removed by the next append in a quiet store.
#[test]
fn a_torn_line_heals_on_the_next_append() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("gitpulse").join("file-claims.jsonl");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(&store, TORN_FRAGMENT).unwrap();
    append_claim(
        &store,
        &observed("q", 1, T0),
        DEFAULT_CAP,
        DEFAULT_TTL_MS,
        T0,
    )
    .unwrap();
    let read = read_store(&store, T0, DEFAULT_TTL_MS, DEFAULT_CAP).unwrap();
    assert_eq!((read.rows.len(), read.skipped), (1, 0), "{read:?}");
}

/// A compaction that has to drop live claims, to stay inside the cap or its
/// size, says so: the next read counts them in `dropped` instead of the loss
/// being a log line, so kept rows plus counted drops are every live claim that
/// was appended. Rows of about 300 bytes fill the cap; rows of about 600 reach
/// the size limit first, so live claims inside the cap are dropped too.
#[test]
fn a_compaction_that_drops_live_claims_is_counted_by_the_next_read() {
    for (pad, fills_the_cap) in [(0usize, true), (300usize, false)] {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("gitpulse").join("file-claims.jsonl");
        let mut compacted_at = None;
        for i in 0..12_000usize {
            let ts = T0 + i as u64;
            let mut claim = observed("d", i, ts);
            claim.path = format!("{}{}", claim.path, "y".repeat(pad));
            let before = fs::metadata(&store).map(|m| m.len()).unwrap_or(0);
            append_claim(&store, &claim, DEFAULT_CAP, DEFAULT_TTL_MS, ts).unwrap();
            if fs::metadata(&store).unwrap().len() < before {
                compacted_at = Some((i + 1, ts));
                break;
            }
        }
        let (appended, ts) = compacted_at.expect("a compaction ran");
        let read = read_store(&store, ts, DEFAULT_TTL_MS, DEFAULT_CAP).unwrap();
        assert_eq!(read.skipped, 0, "pad {pad}: {read:?}");
        assert_eq!(
            read.rows.len() + read.dropped,
            appended,
            "pad {pad}: claims are unaccounted for: {} kept, {} counted dropped, {appended} appended",
            read.rows.len(),
            read.dropped
        );
        assert!(read.dropped > 0, "pad {pad}: the drops were not counted");
        assert!(read
            .degraded()
            .is_some_and(|text| text.contains("not kept")));
        if fills_the_cap {
            assert_eq!(read.rows.len(), DEFAULT_CAP, "pad {pad}");
        } else {
            assert!(
                read.rows.len() < DEFAULT_CAP,
                "pad {pad}: the size limit dropped nothing inside the cap"
            );
        }
    }
}
