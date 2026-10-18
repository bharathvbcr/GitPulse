//! Adversarial tests for `run_process_awaiting_background_budget` (ft-f815889e):
//! the bounded wait of an agent's MCP read, its reserve, its cancellation, the
//! write churn it adds to the shared record, and what it does to a Reactive
//! caller. Tests that pin a break found in review are `#[ignore]`d with the
//! reason; the measurement tests print their numbers (run with `--nocapture`).

use super::{
    shared_budget, with_admission, with_background_budget_wait, with_background_processes,
    Admission, Refusal, SpawnGate,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn far() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

/// The gate's bucket cannot refill for the length of the test.
fn freeze(gate: &SpawnGate) {
    gate.state.lock().unwrap().tokens_at = Some(Instant::now() + Duration::from_secs(86_400));
}

/// A gate sharing a fresh record in `dir`, with burst 8 (reserve 2) and a
/// refill of one token a second, and the record drained to zero by a second
/// handle, as a fleet of hook processes does.
fn drained_shared_gate(dir: &std::path::Path) -> (&'static SpawnGate, shared_budget::SharedBudget) {
    let path = dir.join("spawn-budget.v2");
    let gate = Box::leak(Box::new(
        SpawnGate::with_rate(16, 8, 1).sharing(Ok(path.clone())),
    ));
    assert!(gate.shared.is_some(), "{:?}", gate.shared_note);
    let other = shared_budget::SharedBudget::open(&path).expect("second handle");
    let mut drained = 0;
    while other.take(0, 8, 1) == shared_budget::Take::Granted {
        drained += 1;
        assert!(drained <= 8, "burst is 8");
    }
    (gate, other)
}

fn wait_as_mcp<T>(body: impl FnOnce() -> T) -> T {
    with_background_budget_wait(true, || with_background_processes(body))
}

/// Shared record at or below the reserve for the whole wait: the waiter is
/// refused at about 2 s, takes nothing from the gate, and the record is never
/// above the reserve (so `take(floor = reserve)` never had a token to give).
/// (Held 2.x s rather than 3 s: at 1 token/s the record reaches reserve + 1
/// at 3 s, which would legitimately admit.)
#[test]
fn a_waiting_reader_is_refused_at_the_wait_budget_and_takes_nothing_at_the_reserve() {
    let dir = tempfile::tempdir().unwrap();
    let (gate, other) = drained_shared_gate(dir.path());
    let began = Instant::now();
    let refusal = wait_as_mcp(|| gate.acquire_until(far(), &|| false).err());
    let waited = began.elapsed();
    assert!(
        matches!(refusal, Some(Refusal::Refused { .. })),
        "got {refusal:?}"
    );
    assert!(
        waited >= Duration::from_millis(1_900) && waited < Duration::from_millis(3_000),
        "refused after {waited:?}, expected about 2 s"
    );
    let counters = gate.counters()[Admission::Background.index()];
    assert_eq!(counters.admitted, 0);
    assert_eq!(
        gate.state.lock().unwrap().tokens,
        8,
        "the local bucket was debited by a refused waiter"
    );
    assert_eq!(
        other.take(2, 8, 1),
        shared_budget::Take::Denied,
        "the record rose above the reserve, so the wait window exceeded 3 tokens"
    );
}

/// Cancellation during the wait returns promptly, not at the 2 s budget.
#[test]
fn cancellation_ends_the_wait_within_a_poll() {
    let dir = tempfile::tempdir().unwrap();
    let (gate, _other) = drained_shared_gate(dir.path());
    let flag = AtomicBool::new(false);
    let began = Instant::now();
    let refusal = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            flag.store(true, Ordering::SeqCst);
        });
        wait_as_mcp(|| {
            gate.acquire_until(far(), &|| flag.load(Ordering::SeqCst))
                .err()
        })
    });
    assert_eq!(refusal, Some(Refusal::Cancelled));
    assert!(
        began.elapsed() < Duration::from_millis(900),
        "cancel took {:?}",
        began.elapsed()
    );
}

/// The caller's own (shorter) deadline bounds the wait: a 400 ms deadline is
/// not stretched to the 2 s wait budget.
#[test]
fn the_callers_deadline_bounds_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let (gate, _other) = drained_shared_gate(dir.path());
    let began = Instant::now();
    let refusal = wait_as_mcp(|| {
        gate.acquire_until(Instant::now() + Duration::from_millis(400), &|| false)
            .err()
    });
    assert!(refusal.is_some());
    assert!(
        began.elapsed() < Duration::from_millis(900),
        "{:?}",
        began.elapsed()
    );
}

/// Waiters hold neither the gate mutex nor a slot while they wait: with four
/// of them parked at the reserve, a Reactive caller (slots free) is admitted at
/// once, and all four are bounded at the wait budget (no pile-up past it).
#[test]
fn parked_waiters_do_not_hold_the_gate_and_all_end_at_the_budget() {
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::with_rate(16, 8, 1)));
    for _ in 0..6 {
        drop(with_admission(Admission::Reactive, || gate.acquire(far())).expect("drain"));
    }
    freeze(gate);
    let began = Instant::now();
    let waiters: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(move || wait_as_mcp(|| gate.acquire_until(far(), &|| false).err()))
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    let reactive = with_admission(Admission::Reactive, || {
        gate.acquire_until(Instant::now() + Duration::from_millis(500), &|| false)
    });
    assert!(
        reactive.is_ok(),
        "reactive was refused: {:?}",
        reactive.err()
    );
    assert!(
        t.elapsed() < Duration::from_millis(150),
        "{:?}",
        t.elapsed()
    );
    drop(reactive);
    for waiter in waiters {
        assert!(matches!(
            waiter.join().unwrap(),
            Some(Refusal::Refused { .. })
        ));
    }
    assert!(began.elapsed() < Duration::from_millis(3_000));
}

/// Break: a background waiter that is only short of TOKENS still counts in
/// `waiting_background`, which makes the gate withhold the last free slot from
/// Reactive (`eligibility`: `waiting_background == 0 || background > 0 ||
/// in_flight < limit - 1`). The waiter cannot use that slot, so a hook with a
/// token and a free slot is refused until the waiter gives up. Before the
/// change the shed waiter left at once.
#[test]
fn a_token_starved_waiter_does_not_block_a_reactive_read_that_has_a_slot_and_a_token() {
    // limit 2: one Reactive permit in flight is limit - 1.
    let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::with_rate(2, 8, 1)));
    let held = with_admission(Admission::Reactive, || gate.acquire(far())).expect("p1");
    for _ in 0..5 {
        drop(with_admission(Admission::Reactive, || gate.acquire(far())).expect("drain"));
    }
    freeze(gate);
    assert_eq!(gate.state.lock().unwrap().tokens, 2);

    // Control: no waiter, Reactive (1 free slot, 2 tokens) is admitted.
    let control = with_admission(Admission::Reactive, || {
        gate.acquire_until(Instant::now() + Duration::from_millis(300), &|| false)
    });
    assert!(control.is_ok(), "control: {:?}", control.err());
    drop(control);

    let waiter =
        std::thread::spawn(move || wait_as_mcp(|| gate.acquire_until(far(), &|| false).err()));
    std::thread::sleep(Duration::from_millis(200));
    let reactive = with_admission(Admission::Reactive, || {
        gate.acquire_until(Instant::now() + Duration::from_millis(500), &|| false)
    });
    assert!(
        reactive.is_ok(),
        "reactive with a free slot and a token was refused while an MCP read waited: {:?}",
        reactive.err()
    );
    drop(reactive);
    drop(held);
    let _ = waiter.join();
}

/// Records, with the record's writes failing so each denial is counted, how
/// many times `n` waiters ask the shared record while parked for the full wait
/// budget. Every denied ask is a locked read plus a rewrite of the record.
fn asks_by_parked_waiters(n: usize) -> u64 {
    let dir = tempfile::tempdir().unwrap();
    let (gate, _other) = drained_shared_gate(dir.path());
    gate.shared.as_ref().unwrap().fail_writes(true);
    let waiters: Vec<_> = (0..n)
        .map(|_| {
            std::thread::spawn(move || wait_as_mcp(|| gate.acquire_until(far(), &|| false).err()))
        })
        .collect();
    for waiter in waiters {
        waiter.join().unwrap();
    }
    let asks = gate.state.lock().unwrap().shared_unrecorded_denials;
    asks
}

/// Measurement: record writes per second added by four waiting readers.
#[test]
fn measure_shared_record_writes_of_four_waiting_readers() {
    let asks = asks_by_parked_waiters(4);
    println!(
        "WAITER CHURN: 4 waiters x 2 s -> {asks} shared-record rewrites ({:.0}/s); shed baseline is 1 per spawn",
        asks as f64 / 2.0
    );
    assert!(asks > 0);
}

/// Break: every waiting reader rewrites the shared record on every 50 ms poll,
/// so churn grows with the number of waiters (and with processes) instead of
/// staying at the refill rate. The record can only change once per refill
/// interval (125 ms at 8/s), so a waiter has no reason to re-ask faster.
#[test]
fn waiting_readers_ask_the_shared_record_no_faster_than_it_refills() {
    let asks = asks_by_parked_waiters(4);
    // One ask per refill interval per process (production: 8/s) plus slack.
    assert!(
        asks <= 4 * 16 / 4 + 4,
        "{asks} asks in a 2 s wait by 4 waiters"
    );
}
