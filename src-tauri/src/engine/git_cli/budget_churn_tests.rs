//! The spawn budget's observability and churn (ft-da85cd5599b713d7a7f5910da2133d70):
//! what one edit-time hook invocation spends by call site (a characterization
//! of the current spend; the cut lands with the hooks half), what a refusal
//! says about the bucket that refused, and how often denied waiters rewrite the
//! shared record. The measurement tests print their numbers (`--nocapture`).

use super::{
    refusal_message, refusal_message_at, shared_budget, spawn_log, subcommand, with_admission,
    with_background_processes, Admission, BucketLevel, Refusal, RefusingBucket, RunError,
    SpawnGate,
};
use crate::test_support::coverage_relaxed;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

fn level(bucket: RefusingBucket, tokens: u32, floor: u32, burst: u32) -> BucketLevel {
    BucketLevel {
        bucket,
        tokens,
        floor,
        burst,
    }
}

/// A repository with `siblings` linked worktrees, each dirty in a file of its
/// own, and the files the edit is of: the ordinary state of a machine running
/// parallel agents.
struct Farm {
    _repo: tempfile::TempDir,
    _siblings: tempfile::TempDir,
    repo: String,
    dirs: Vec<std::path::PathBuf>,
}

fn farm(siblings: usize) -> Farm {
    use crate::test_support::git_in;
    let repo_dir = tempfile::tempdir().expect("repository");
    git_in(repo_dir.path(), &["init", "-b", "main"]);
    std::fs::write(repo_dir.path().join("seed.txt"), "seed").expect("seed");
    git_in(repo_dir.path(), &["add", "seed.txt"]);
    git_in(repo_dir.path(), &["commit", "-m", "seed"]);
    let repo = repo_dir
        .path()
        .canonicalize()
        .expect("canonical repository");
    let sibling_root = tempfile::tempdir().expect("sibling worktrees");
    let root = sibling_root.path().canonicalize().expect("canonical root");
    let mut dirs = vec![repo.clone()];
    for n in 0..siblings {
        let wt = root.join(format!("sibling-{n}"));
        git_in(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                &format!("b{n}"),
                &wt.to_string_lossy(),
            ],
        );
        std::fs::write(wt.join(format!("own-{n}.txt")), "mine").expect("dirty a sibling");
        dirs.push(wt);
    }
    Farm {
        repo: repo.to_string_lossy().into_owned(),
        _repo: repo_dir,
        _siblings: sibling_root,
        dirs,
    }
}

/// AC1, characterization. One PreToolUse Edit through the production
/// `collision-guard` (what `gitpulse-hook collision-guard` dispatches) against
/// a real repository with two linked worktrees, and every git child it
/// started, by where it ran and what it ran. Each child is one token of the
/// shared spawn budget.
///
/// The count is PINNED at what the code spends today. The cut (answer from the
/// app over its socket, or cache the scan) lands with the hooks half of the
/// card, behind ft-42081f16, and will lower this number deliberately.
#[cfg(unix)]
#[test]
fn one_edit_hook_invocation_spends_this_many_git_spawns_by_call_site() {
    let farm = farm(2);
    let marks: Vec<usize> = farm
        .dirs
        .iter()
        .map(|dir| spawn_log::spawns_in(dir).len())
        .collect();
    let output = crate::hooks::run_collision_guard(&crate::hooks::HookInput {
        hook_event_name: "PreToolUse".to_string(),
        tool_name: "Edit".to_string(),
        cwd: farm.repo.clone(),
        file_path: format!("{}/seed.txt", farm.repo),
        ..Default::default()
    });
    let mut table: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut total = 0;
    for (dir, mark) in farm.dirs.iter().zip(&marks) {
        let place = if dir == &farm.dirs[0] {
            "main checkout".to_string()
        } else {
            format!("sibling {}", dir.file_name().unwrap().to_string_lossy())
        };
        for argv in &spawn_log::spawns_in(dir)[*mark..] {
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            *table
                .entry((place.clone(), subcommand(&args).to_string()))
                .or_default() += 1;
            total += 1;
        }
    }
    println!("EDIT HOOK SPAWNS (3 worktrees, 1 PreToolUse Edit): {total} git children");
    println!("{:<18} {:<14} spawns", "where", "git command");
    for ((place, command), count) in &table {
        println!("{place:<18} {command:<14} {count}");
    }
    assert!(!output.is_silent() || total > 0, "the guard did not run");
    assert_eq!(
        total, 4,
        "the hook's spend changed: {table:?}; if this is the hooks-half cut, lower the pin"
    );
}

/// AC3. A refusal names the bucket that refused and the level it saw, and
/// keeps the sentence every classifier matches on.
#[test]
fn a_refusal_names_the_refusing_bucket_and_its_level() {
    // The shared record is drained; this process's own bucket is full.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spawn-budget.v2");
    let gate = SpawnGate::with_rate(8, 8, 1).sharing(Ok(path.clone()));
    let other = shared_budget::SharedBudget::open(&path).expect("second handle");
    while other.take(0, 8, 1) == shared_budget::Take::Granted {}
    let (refusal, judged) = with_admission(Admission::Reactive, || {
        gate.acquire_judged(Instant::now() + Duration::from_millis(200), &|| false)
    })
    .err()
    .expect("refused");
    assert!(matches!(refusal, Refusal::Refused { .. }), "{refusal:?}");
    assert_eq!(judged, Some(level(RefusingBucket::Shared, 0, 0, 8)));
    let message = refusal_message_at("git status", refusal, judged);
    assert!(
        message.ends_with(
            "the git spawn rate limit admitted nothing sooner \
             (shared bucket: 0 token(s), needs more than 0, burst 8)"
        ),
        "{message}"
    );
    assert!(super::is_deferred_under_load(&message), "{message}");
    assert_eq!(RunError::classify(&message), RunError::Deferred);

    // Background work, shed at once by this process's own bucket: the floor is
    // the reserve kept for reactive reads, and the level is below it.
    let local = SpawnGate::with_rate(8, 16, 1);
    local.state.lock().unwrap().tokens = 3;
    local.state.lock().unwrap().tokens_at = Some(Instant::now() + Duration::from_secs(86_400));
    let (refusal, judged) = with_background_processes(|| {
        local.acquire_judged(Instant::now() + Duration::from_secs(5), &|| false)
    })
    .err()
    .expect("shed");
    assert_eq!(refusal, Refusal::Shed);
    assert_eq!(judged, Some(level(RefusingBucket::Local, 3, 4, 16)));
    let message = refusal_message_at("git worktree", refusal, judged);
    assert_eq!(
        message,
        "git worktree deferred under load after 0.000s: background work is shed while the git \
         spawn rate budget is low (local bucket: 3 token(s), needs more than 4, burst 16)"
    );
    assert!(super::is_deferred_under_load(&message), "{message}");
    assert!(super::is_deferred_under_load(&format!(
        "[deferred_worktree] {message}"
    )));
    assert_eq!(RunError::classify(&message), RunError::Deferred);
}

/// The wording callers that cannot know the level construct is unchanged: the
/// text before the level is the text every test, classifier and TS reader
/// already matches.
#[test]
fn a_refusal_without_a_level_reads_exactly_as_before() {
    let waited = Duration::from_millis(2_013);
    assert_eq!(
        refusal_message("git status", Refusal::Refused { waited }),
        "git status deferred under load after 2.013s: the git spawn rate limit admitted nothing sooner"
    );
    assert_eq!(
        refusal_message("git status", Refusal::Shed),
        "git status deferred under load after 0.000s: background work is shed while the git \
         spawn rate budget is low"
    );
}

/// AC4, through the gate. Four threads keep asking a drained shared record for
/// a token for two seconds, the way four agents' hooks do. The record changes
/// only when a token is credited, so it is rewritten at most as often as the
/// refill rate grants one, not once per denied ask.
#[test]
fn four_denied_waiters_rewrite_the_shared_record_no_faster_than_it_refills() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spawn-budget.v2");
    let rate = 4u32;
    let gate: &'static SpawnGate = Box::leak(Box::new(
        SpawnGate::with_rate(16, 16, rate).sharing(Ok(path.clone())),
    ));
    let other = shared_budget::SharedBudget::open(&path).expect("second handle");
    while other.take(0, 16, rate) == shared_budget::Take::Granted {}
    let window = Duration::from_secs(2);
    let started = Instant::now();
    let end = started + window;
    let grants: u64 = std::thread::scope(|scope| {
        let waiters: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(move || {
                    let mut grants = 0u64;
                    while Instant::now() < end {
                        let got = with_admission(Admission::Reactive, || {
                            gate.acquire_judged(end, &|| false).is_ok()
                        });
                        grants += u64::from(got);
                    }
                    grants
                })
            })
            .collect();
        waiters.into_iter().map(|w| w.join().unwrap()).sum()
    });
    let secs = started.elapsed().as_secs_f64();
    let writes = gate.shared.as_ref().unwrap().writes();
    println!(
        "WAITER WRITES: 4 waiters x {secs:.2} s on a drained record at {rate}/s -> {writes} record writes \
         ({:.1}/s), {grants} grants",
        writes as f64 / secs
    );
    // One write per grant, and one probe per handle that proves the record
    // still takes writes; the slack covers a grant on the window's edges.
    let bound = (f64::from(rate) * secs).ceil() as u64 + 3;
    assert!(
        writes <= bound,
        "{writes} writes in {secs:.2} s at {rate}/s refill (bound {bound})"
    );
}

/// The wait for a token is bounded by the deadline and by cancellation even
/// though the waiter now sleeps until the token is due.
#[test]
fn a_waiter_asleep_for_a_token_still_ends_at_its_deadline_and_on_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spawn-budget.v2");
    let gate = SpawnGate::with_rate(8, 8, 1).sharing(Ok(path.clone()));
    let other = shared_budget::SharedBudget::open(&path).expect("second handle");
    while other.take(0, 8, 1) == shared_budget::Take::Granted {}
    let began = Instant::now();
    let refusal = with_admission(Admission::Reactive, || {
        gate.acquire_until(began + Duration::from_millis(150), &|| false)
    })
    .err();
    assert!(
        matches!(refusal, Some(Refusal::Refused { .. })),
        "{refusal:?}"
    );
    assert!(
        began.elapsed() < coverage_relaxed(Duration::from_secs(1)),
        "{:?}",
        began.elapsed()
    );

    let flag = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let began = Instant::now();
        let refusal = with_admission(Admission::Reactive, || {
            gate.acquire_until(began + Duration::from_secs(30), &|| {
                flag.load(std::sync::atomic::Ordering::SeqCst)
            })
        })
        .err();
        assert_eq!(refusal, Some(Refusal::Cancelled));
        assert!(
            began.elapsed() < coverage_relaxed(Duration::from_secs(1)),
            "{:?}",
            began.elapsed()
        );
    });
}
