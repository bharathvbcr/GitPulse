//! Terminal keystrokes and resizes must not wait on anything that is not a
//! terminal.
//!
//! Every Tauri command that did blocking work used to share one pool, and the
//! git commands that hold its threads for seconds under load are the same pool
//! a keystroke went through; `cmd_terminal_resize` skipped the pool and blocked
//! an async-runtime worker on a std mutex instead. The frontend gives each of
//! those calls five seconds before it declares input dead and offers a restart
//! that kills the child, so a slow host cost people their agent sessions.
//!
//! These drive the real commands (`cmd_terminal_write`, `cmd_terminal_resize`)
//! against real PTYs while the pool tokio gives `spawn_blocking` is full. They
//! share one process whose thread and descriptor counts are part of the
//! measurement, so they run one at a time behind `SERIAL`.
#![cfg(unix)]
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use gitpulse_lib::commands::{cmd_terminal_resize, cmd_terminal_write};
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::terminal::{
    acknowledge_output, hold_master, kill_session, list_sessions, spawn_session, TerminalSessions,
    GEOMETRY_QUEUE_CAPACITY,
};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};
use tauri::{Listener, Manager};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Tokio's default `max_blocking_threads`, which is what Tauri's runtime uses.
const BLOCKING_POOL: usize = 512;

/// How long a keystroke or a resize may take with the blocking pool full.
/// Before the fix the answer was "until the pool drains"; this only has to be
/// far from that, and loose enough for a loaded CI host.
fn prompt() -> Duration {
    common::env::coverage_relaxed(Duration::from_secs(1))
}

struct TerminalCleanup(TerminalSessions);
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if let Err(error) = gitpulse_lib::terminal::shutdown_sessions(&self.0) {
            eprintln!("PTY test cleanup incomplete: {error}");
        }
    }
}

fn repo() -> tempfile::TempDir {
    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir.path())
        .status_locked()
        .unwrap()
        .success());
    common::trust_repo(dir.path());
    dir
}

fn wait_until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

fn open_descriptors() -> usize {
    std::fs::read_dir("/dev/fd")
        .or_else(|_| std::fs::read_dir("/proc/self/fd"))
        .expect("list descriptors")
        .count()
}

fn threads() -> usize {
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        return tasks.count();
    }
    // macOS: `ps -M` prints a header and one line per thread.
    let out = std::process::Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output_locked()
        .expect("ps -M");
    String::from_utf8_lossy(&out.stdout).lines().count() - 1
}

/// Every thread of `tauri::async_runtime`'s blocking pool, occupied until
/// dropped: what a burst of slow git commands does to the pool a terminal
/// command used to queue on.
struct SaturatedPool {
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl SaturatedPool {
    fn new() -> Self {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let running = Arc::new(AtomicUsize::new(0));
        for _ in 0..BLOCKING_POOL + 16 {
            let (release, running) = (release.clone(), running.clone());
            tauri::async_runtime::spawn_blocking(move || {
                running.fetch_add(1, Ordering::SeqCst);
                let (flag, changed) = &*release;
                let mut released = flag.lock().unwrap();
                while !*released {
                    released = changed.wait(released).unwrap();
                }
            });
        }
        let pool = Self { release };
        assert!(
            wait_until(Duration::from_secs(60), || running.load(Ordering::SeqCst)
                >= BLOCKING_POOL),
            "only {} of {BLOCKING_POOL} blocking threads started; the pool is larger than assumed",
            running.load(Ordering::SeqCst)
        );
        // Positive control: the pool really is full. A probe queued now cannot
        // start, so a command that finishes while it is waiting did not use it.
        let probe_ran = Arc::new(AtomicUsize::new(0));
        let ran = probe_ran.clone();
        tauri::async_runtime::spawn_blocking(move || {
            ran.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            probe_ran.load(Ordering::SeqCst),
            0,
            "positive control failed: a task queued on a full blocking pool ran"
        );
        pool
    }

    fn release(&self) {
        let (flag, changed) = &*self.release;
        *flag.lock().unwrap() = true;
        changed.notify_all();
    }
}

impl Drop for SaturatedPool {
    fn drop(&mut self) {
        self.release();
    }
}

/// A session that echoes nothing and hands back every byte it is given: the
/// tty is raw and `cat` copies stdin to stdout. `READY` is printed once the
/// tty is configured, so nothing is typed into a cooked line.
fn cat_args() -> Option<Vec<String>> {
    Some(vec![
        "-c".into(),
        "stty raw -echo; echo READY; exec cat".into(),
    ])
}

/// Everything each session printed, keyed by session id. Acknowledges as it
/// goes, like the view does.
fn collecting(
    app: &tauri::App<tauri::test::MockRuntime>,
    state: &TerminalSessions,
) -> Arc<Mutex<HashMap<String, Vec<u8>>>> {
    let output: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
    let captured = output.clone();
    let ack = state.clone();
    app.listen("terminal-output", move |event| {
        let data: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let bytes = STANDARD.decode(data["data_b64"].as_str().unwrap()).unwrap();
        let id = data["id"].as_str().unwrap();
        captured
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .extend(&bytes);
        let _ = acknowledge_output(&ack, id, bytes.len());
    });
    output
}

/// What `id` printed after its `READY` line, once there is one.
fn after_ready(output: &Mutex<HashMap<String, Vec<u8>>>, id: &str) -> Option<Vec<u8>> {
    let guard = output.lock().unwrap();
    let bytes = guard.get(id)?;
    let at = bytes.windows(6).position(|w| w == b"READY\n")?;
    Some(bytes[at + 6..].to_vec())
}

fn mock_app(state: &TerminalSessions) -> tauri::App<tauri::test::MockRuntime> {
    tauri::test::mock_builder()
        .manage(state.clone())
        .build(gitpulse_lib::context())
        .unwrap()
}

fn start_cat(
    app: &tauri::App<tauri::test::MockRuntime>,
    state: &TerminalSessions,
    output: &Mutex<HashMap<String, Vec<u8>>>,
    dir: &std::path::Path,
) -> String {
    start(app, state, output, dir, cat_args())
}

/// Like [`start_cat`], but `cat` writes what it is given to `file`.
///
/// Many sessions' output cannot be told apart by when it was heard: Tauri
/// hands an event to a listener from whichever thread finishes emitting, and
/// when two readers emit together one of the events is parked and delivered
/// later, after its session's next one. That reorders what a listener sees, not
/// what the PTY carried, so the bytes under test are read from a file instead.
fn start_cat_to_file(
    app: &tauri::App<tauri::test::MockRuntime>,
    state: &TerminalSessions,
    output: &Mutex<HashMap<String, Vec<u8>>>,
    dir: &std::path::Path,
    file: &std::path::Path,
) -> String {
    start(
        app,
        state,
        output,
        dir,
        Some(vec![
            "-c".into(),
            "stty raw -echo; echo READY; exec cat > \"$0\"".into(),
            file.to_str().unwrap().into(),
        ]),
    )
}

fn start(
    app: &tauri::App<tauri::test::MockRuntime>,
    state: &TerminalSessions,
    output: &Mutex<HashMap<String, Vec<u8>>>,
    dir: &std::path::Path,
    args: Option<Vec<String>>,
) -> String {
    let spawned = spawn_session(
        app.handle(),
        state,
        dir.to_str().unwrap(),
        24,
        80,
        Some("/bin/sh".into()),
        args,
        None,
    )
    .unwrap();
    assert!(
        wait_until(Duration::from_secs(20), || after_ready(output, &spawned.id)
            .is_some()),
        "the raw-mode cat session never became ready"
    );
    spawned.id
}

/// Runs `cmd_terminal_write` the way the IPC bridge does, on a thread of its
/// own, and reports how long it took, or `None` if it had not answered by
/// `within`.
fn timed_write(
    handle: &tauri::AppHandle<tauri::test::MockRuntime>,
    id: &str,
    data: &str,
    within: Duration,
) -> Option<(Result<(), String>, Duration)> {
    let (done, answered) = mpsc::channel();
    let (handle, id, data) = (handle.clone(), id.to_owned(), data.to_owned());
    std::thread::spawn(move || {
        let started = Instant::now();
        let state = handle.state::<TerminalSessions>();
        let result = tauri::async_runtime::block_on(cmd_terminal_write(state, id, data, None));
        let _ = done.send((result, started.elapsed()));
    });
    answered.recv_timeout(within).ok()
}

/// A keystroke is delivered while every blocking-pool thread is busy.
///
/// The defect: `cmd_terminal_write` went through `off_thread`, so with the pool
/// full (git commands holding every thread, as the log shows on a loaded host:
/// `cmd_terminal_write queue_ms=1165`) it waited for a git command to finish
/// before it touched the PTY.
#[test]
fn a_keystroke_is_delivered_while_the_blocking_pool_is_full() {
    let _serial = serial();
    let state = TerminalSessions::default();
    let app = mock_app(&state);
    let _cleanup = TerminalCleanup(state.clone());
    let output = collecting(&app, &state);
    let dir = repo();
    let id = start_cat(&app, &state, &output, dir.path());

    let pool = SaturatedPool::new();
    let answer = timed_write(app.handle(), &id, "typed-while-saturated;", prompt());
    pool.release();
    let (result, took) = answer.unwrap_or_else(|| {
        panic!(
            "cmd_terminal_write did not answer within {:?} with the blocking pool full",
            prompt()
        )
    });
    result.unwrap();
    println!("write took {took:?} with the blocking pool full");
    assert!(
        wait_until(Duration::from_secs(10), || after_ready(&output, &id)
            .is_some_and(|b| b == b"typed-while-saturated;")),
        "the keystrokes did not come back through cat: {:?}",
        after_ready(&output, &id).map(|b| String::from_utf8_lossy(&b).into_owned())
    );
}

/// A poll of `cmd_terminal_resize` returns instead of blocking its thread.
///
/// The defect: the command was an `async fn` that called `resize_session`
/// inline, so a busy master lock parked a Tauri async-runtime worker. The
/// future is polled once by hand on this thread, which is the worker's job;
/// if the poll itself waits for the lock, this thread waits with it.
#[test]
fn a_resize_does_not_block_the_thread_that_polls_it() {
    let _serial = serial();
    let state = TerminalSessions::default();
    let app = mock_app(&state);
    let _cleanup = TerminalCleanup(state.clone());
    let output = collecting(&app, &state);
    let dir = repo();
    let id = start_cat(&app, &state, &output, dir.path());

    let hold = hold_master(&state, &id).unwrap();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(800));
        hold.release();
    });
    let handle = app.handle().clone();
    let tauri_state = handle.state::<TerminalSessions>();
    let mut resize = Box::pin(cmd_terminal_resize(tauri_state, id.clone(), 30, 100));
    let mut cx = Context::from_waker(Waker::noop());
    let polled = Instant::now();
    let first = resize.as_mut().poll(&mut cx);
    let blocked_for = polled.elapsed();
    println!("first poll of cmd_terminal_resize took {blocked_for:?} (master lock held 800ms)");
    assert!(
        blocked_for < Duration::from_millis(400),
        "polling cmd_terminal_resize blocked its thread for {blocked_for:?} on the master lock"
    );
    assert!(
        first.is_pending(),
        "the resize cannot have finished while the master lock was held"
    );
    // Once the lock is let go the same future completes.
    tauri::async_runtime::block_on(resize).unwrap();
    releaser.join().unwrap();
    assert!(list_sessions(&state).iter().any(|s| s.id == id));
    kill_session(&state, &id).unwrap();
}

/// Polls `call` once with no waker, as the runtime does the first time it sees
/// a command's future. Queueing happens in that poll.
fn poll_once<F: Future>(call: &mut std::pin::Pin<Box<F>>) -> Poll<F::Output> {
    call.as_mut().poll(&mut Context::from_waker(Waker::noop()))
}

/// A resize queue that is full answers at once, and everything that was
/// accepted still completes when the lock comes free.
///
/// The master lock is held, so the session's one geometry thread is stuck in
/// its (bounded) wait and every later resize queues behind it. Capacity is the
/// bound: the call past it is refused on the spot, never parked.
#[test]
fn a_full_resize_queue_is_refused_at_once_and_what_was_accepted_still_completes() {
    let _serial = serial();
    let state = TerminalSessions::default();
    let app = mock_app(&state);
    let _cleanup = TerminalCleanup(state.clone());
    let output = collecting(&app, &state);
    let dir = repo();
    let id = start_cat(&app, &state, &output, dir.path());
    let handle = app.handle().clone();

    let hold = hold_master(&state, &id).unwrap();
    let mut accepted = Vec::new();
    let mut refusals = Vec::new();
    let started = Instant::now();
    for n in 0..GEOMETRY_QUEUE_CAPACITY + 4 {
        let rows = 20 + n as u16;
        let mut call = Box::pin(cmd_terminal_resize(
            handle.state::<TerminalSessions>(),
            id.clone(),
            rows,
            90,
        ));
        match poll_once(&mut call) {
            Poll::Pending => accepted.push(call),
            Poll::Ready(result) => refusals.push(result),
        }
    }
    let queuing = started.elapsed();
    println!(
        "{} resizes accepted, {} refused, in {queuing:?}",
        accepted.len(),
        refusals.len()
    );
    assert!(
        queuing < prompt(),
        "queueing {} resizes took {queuing:?}: a full queue must refuse, not wait",
        GEOMETRY_QUEUE_CAPACITY + 4
    );
    // One may already be running; the rest wait, up to the capacity.
    assert!(
        accepted.len() <= GEOMETRY_QUEUE_CAPACITY + 1,
        "{}",
        accepted.len()
    );
    assert!(
        !refusals.is_empty(),
        "nothing was refused past the capacity"
    );
    for refusal in &refusals {
        let message = refusal.as_ref().unwrap_err();
        assert!(message.contains("queue is full"), "{message}");
    }

    // The input lane is a different thread: typing is not behind them.
    let (written, took) = timed_write(&handle, &id, "typed-behind-resizes;", prompt())
        .expect("a write waited behind resizes that were waiting for the master lock");
    written.unwrap();
    println!("write took {took:?} while the resize queue was full");

    hold.release();
    for call in accepted {
        tauri::async_runtime::block_on(call).unwrap();
    }
    // The lane is not wedged: a new resize goes straight through.
    tauri::async_runtime::block_on(cmd_terminal_resize(
        handle.state::<TerminalSessions>(),
        id.clone(),
        24,
        80,
    ))
    .unwrap();
    kill_session(&state, &id).unwrap();
}

/// Calls whose futures are dropped (a webview that went away mid-command)
/// leave the lane serving: their jobs are skipped or finish unheard, and the
/// next call is answered.
#[test]
fn dropped_calls_do_not_wedge_a_lane() {
    let _serial = serial();
    let state = TerminalSessions::default();
    let app = mock_app(&state);
    let _cleanup = TerminalCleanup(state.clone());
    let output = collecting(&app, &state);
    let dir = repo();
    let id = start_cat(&app, &state, &output, dir.path());
    let handle = app.handle().clone();

    let hold = hold_master(&state, &id).unwrap();
    for n in 0..8u16 {
        let mut call = Box::pin(cmd_terminal_resize(
            handle.state::<TerminalSessions>(),
            id.clone(),
            30 + n,
            100,
        ));
        assert!(poll_once(&mut call).is_pending());
        drop(call);
    }
    hold.release();
    let (written, took) = timed_write(&handle, &id, "after-cancel;", prompt()).expect("write");
    written.unwrap();
    println!("write after 8 cancelled resizes took {took:?}");
    let started = Instant::now();
    tauri::async_runtime::block_on(cmd_terminal_resize(
        handle.state::<TerminalSessions>(),
        id.clone(),
        24,
        80,
    ))
    .unwrap();
    assert!(
        started.elapsed() < prompt(),
        "the lane was still draining cancelled resizes after {:?}",
        started.elapsed()
    );
    kill_session(&state, &id).unwrap();
}

/// A write to a session that is gone is refused with the old message, without
/// starting any thread.
#[test]
fn a_write_or_resize_to_a_missing_session_is_refused_by_name() {
    let _serial = serial();
    let state = TerminalSessions::default();
    let app = mock_app(&state);
    let handle = app.handle().clone();
    let write = tauri::async_runtime::block_on(cmd_terminal_write(
        handle.state::<TerminalSessions>(),
        "term-nobody".into(),
        "x".into(),
        None,
    ))
    .unwrap_err();
    assert!(write.contains("not found"), "{write}");
    let resize = tauri::async_runtime::block_on(cmd_terminal_resize(
        handle.state::<TerminalSessions>(),
        "term-nobody".into(),
        24,
        80,
    ))
    .unwrap_err();
    assert!(resize.contains("not found"), "{resize}");
}

/// A child that never reads its input, so a large write spins on a full tty
/// for its two-second budget, and that exits by itself after `exit_after`.
fn deaf_args(exit_after: &str) -> Option<Vec<String>> {
    Some(vec![
        "-c".into(),
        format!("stty raw -echo; echo READY; sleep {exit_after}"),
    ])
}

/// How a session can end under a write.
#[derive(Clone, Copy, Debug)]
enum Ending {
    /// The user's Close.
    Kill,
    /// Application exit.
    Shutdown,
    /// The program exits on its own (an agent finishing, `exit` typed).
    SelfExit,
}

/// A write in flight and writes queued behind it on the same input lane, when
/// the session ends, are each answered with a message the frontend reads as
/// "the session is gone" (`reads_as_session_gone`, the rule `sessionIsGone`
/// applies), never as a dropped chunk of a live session — which the frontend
/// reports as "Some input was not delivered and was dropped".
///
/// The child never reads, so the first (64 KiB) write is spinning on a full
/// tty when the session ends and the others are queued behind it on the lane.
#[test]
fn writes_queued_or_in_flight_when_a_session_ends_are_answered_as_gone() {
    let _serial = serial();
    for ending in [Ending::Kill, Ending::Shutdown, Ending::SelfExit] {
        let state = TerminalSessions::default();
        let app = mock_app(&state);
        let _cleanup = TerminalCleanup(state.clone());
        let output = collecting(&app, &state);
        let dir = repo();
        let exit_after = match ending {
            Ending::SelfExit => "1",
            _ => "600",
        };
        let id = start(&app, &state, &output, dir.path(), deaf_args(exit_after));
        let handle = app.handle().clone();
        let big = "x".repeat(60 * 1024);
        let mut answers = Vec::new();
        for n in 0..4 {
            let (done, answered) = mpsc::channel();
            let (handle, id) = (handle.clone(), id.clone());
            let data = if n == 0 {
                big.clone()
            } else {
                format!("queued-{n};")
            };
            std::thread::spawn(move || {
                let state = handle.state::<TerminalSessions>();
                let result =
                    tauri::async_runtime::block_on(cmd_terminal_write(state, id, data, None));
                let _ = done.send(result);
            });
            // In order on the lane: each call queues before the next starts.
            std::thread::sleep(Duration::from_millis(if n == 0 { 300 } else { 30 }));
            answers.push(answered);
        }
        // A close must not wait out the stuck write's two seconds, nor the
        // writes queued behind it: it is bounded by its own cleanup wait.
        let closing = Instant::now();
        match ending {
            Ending::Kill => kill_session(&state, &id).unwrap(),
            Ending::Shutdown => gitpulse_lib::terminal::shutdown_sessions(&state).unwrap(),
            Ending::SelfExit => {}
        }
        let closed_in = closing.elapsed();
        println!("{ending:?}: closed in {closed_in:?} under a stuck write");
        assert!(
            closed_in < common::env::coverage_relaxed(Duration::from_secs(5)),
            "{ending:?}: closing took {closed_in:?} with a write stuck on the lane"
        );
        let mut seen = Vec::new();
        for (n, answered) in answers.into_iter().enumerate() {
            let result = answered
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("{ending:?}: write {n} never answered"));
            println!("{ending:?}: write {n} -> {result:?}");
            seen.push((n, result));
        }
        // The first write cannot have finished: nothing read its 60 KiB.
        for (n, result) in &seen {
            if let Err(message) = result {
                assert!(
                    gitpulse_lib::terminal::reads_as_session_gone(message),
                    "{ending:?}: write {n} was answered {message:?}, which the frontend \
                     reads as a dropped chunk of a live session, not a session that ended"
                );
            }
        }
        assert!(
            seen[0].1.is_err(),
            "{ending:?}: a 60 KiB write to a child that never reads cannot have succeeded"
        );
    }
}

/// Counts one worker down when it ends, however it ends.
struct Decrement<'a>(&'a AtomicUsize);
impl Drop for Decrement<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn token(session: usize, n: usize) -> String {
    format!("[{session}:{n}]")
}

/// 32 live sessions, 200 writes and 200 resizes each from 64 threads, with the
/// blocking pool full the whole time: every byte arrives, in order, per
/// session; every call is answered; no queue overflows; and when the sessions
/// are closed the process has the descriptors and threads it started with.
#[test]
fn thirty_two_sessions_keep_every_byte_in_order_with_the_blocking_pool_full() {
    const SESSIONS: usize = 32;
    const ROUNDS: usize = 200;
    let _serial = serial();
    let state = TerminalSessions::default();
    let app = mock_app(&state);
    let _cleanup = TerminalCleanup(state.clone());
    state.set_session_limit(SESSIONS + 8);
    let output = collecting(&app, &state);
    let dir = repo();
    let handle = app.handle().clone();

    // Process-wide lazy state (ledger, alert hub, pools) is not a leak, so it is
    // paid for before the baseline.
    let warm = start_cat(&app, &state, &output, dir.path());
    tauri::async_runtime::block_on(cmd_terminal_write(
        handle.state::<TerminalSessions>(),
        warm.clone(),
        "warm".into(),
        None,
    ))
    .unwrap();
    kill_session(&state, &warm).unwrap();
    assert!(wait_until(Duration::from_secs(10), || list_sessions(
        &state
    )
    .is_empty()));

    let pool = SaturatedPool::new();
    std::thread::sleep(Duration::from_millis(500));
    let (fds_before, threads_before) = (open_descriptors(), threads());

    let files: Vec<std::path::PathBuf> = (0..SESSIONS)
        .map(|index| dir.path().join(format!("session-{index}.out")))
        .collect();
    let ids: Vec<String> = files
        .iter()
        .map(|file| start_cat_to_file(&app, &state, &output, dir.path(), file))
        .collect();
    assert_eq!(list_sessions(&state).len(), SESSIONS);

    let slowest = Mutex::new(Duration::ZERO);
    let failures: Mutex<Vec<String>> = Mutex::default();
    // Calls that wait for the blocking pool would wait for ever here, because
    // it is released only afterwards: the watchdog lets go of it after a minute
    // so a regression fails this test instead of hanging the suite.
    let unfinished = AtomicUsize::new(2 * SESSIONS);
    let gave_up = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while unfinished.load(Ordering::SeqCst) > 0 {
                if Instant::now() >= deadline {
                    gave_up.store(true, Ordering::SeqCst);
                    pool.release();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        for (index, id) in ids.iter().enumerate() {
            let (handle, slowest, failures, unfinished) =
                (&handle, &slowest, &failures, &unfinished);
            scope.spawn(move || {
                let _done = Decrement(unfinished);
                for n in 0..ROUNDS {
                    let started = Instant::now();
                    // Half the sessions type through the binary path.
                    let binary = (index % 2 == 1).then_some(true);
                    let result = tauri::async_runtime::block_on(cmd_terminal_write(
                        handle.state::<TerminalSessions>(),
                        id.clone(),
                        token(index, n),
                        binary,
                    ));
                    let took = started.elapsed();
                    let mut slowest = slowest.lock().unwrap();
                    *slowest = (*slowest).max(took);
                    if let Err(error) = result {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("write {index}:{n}: {error}"));
                    }
                }
            });
            scope.spawn(move || {
                let _done = Decrement(unfinished);
                for n in 0..ROUNDS {
                    let result = tauri::async_runtime::block_on(cmd_terminal_resize(
                        handle.state::<TerminalSessions>(),
                        id.clone(),
                        10 + (n % 30) as u16,
                        40 + (n % 80) as u16,
                    ));
                    if let Err(error) = result {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("resize {index}:{n}: {error}"));
                    }
                }
            });
        }
    });
    assert!(
        !gave_up.load(Ordering::SeqCst),
        "terminal commands were still waiting after a minute with the blocking pool full"
    );
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} calls failed, first: {:?}",
        failures.len(),
        failures.first()
    );
    let slowest = slowest.into_inner().unwrap();
    println!("slowest of {} writes: {slowest:?}", SESSIONS * ROUNDS);
    assert!(
        slowest < Duration::from_secs(5),
        "a write took {slowest:?} with the blocking pool full; the frontend gives up at 5s"
    );

    for (index, file) in files.iter().enumerate() {
        let expected: Vec<u8> = (0..ROUNDS)
            .flat_map(|n| token(index, n).into_bytes())
            .collect();
        let read = || std::fs::read(file).unwrap_or_default();
        assert!(
            wait_until(Duration::from_secs(30), || read().len() >= expected.len()),
            "session {index}: only {} of {} bytes reached cat",
            read().len(),
            expected.len()
        );
        // A moment longer for anything extra (a duplicate) to show up.
        std::thread::sleep(Duration::from_millis(5));
        let got = read();
        assert!(
            got == expected,
            "session {index}: bytes reordered, lost or duplicated\n got: {}\nwant: {}",
            String::from_utf8_lossy(&got[..got.len().min(200)]),
            String::from_utf8_lossy(&expected[..200])
        );
    }

    std::thread::scope(|scope| {
        for id in &ids {
            let state = &state;
            scope.spawn(move || kill_session(state, id).unwrap());
        }
    });
    assert!(
        wait_until(Duration::from_secs(30), || list_sessions(&state).is_empty()),
        "sessions left after every close returned"
    );
    // Lane and reader threads end just after the entry leaves the map.
    let slack = 6;
    assert!(
        wait_until(Duration::from_secs(20), || {
            open_descriptors() <= fds_before + slack && threads() <= threads_before + slack
        }),
        "descriptors {fds_before} -> {}, threads {threads_before} -> {} after {SESSIONS} sessions",
        open_descriptors(),
        threads()
    );
    drop(pool);
}

mod common;
