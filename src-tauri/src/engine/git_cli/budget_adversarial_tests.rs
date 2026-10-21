//! Adversarial review of lane L3 (ft-da85cd5599b713d7a7f5910da2133d70, budget
//! side): skipped denial writes must never mint, the probe must still find a
//! record that stopped taking writes, the nap-until-due waiter must stay fair,
//! bounded and cancellable, and the refusal tail must not break any reader.
//!
//! A child of `shared_budget` so it can drive the private `take_at`.

use super::*;
use crate::engine::git_cli::{
    is_deferred_under_load, refusal_message_at, with_admission, Admission, BucketLevel, Refusal,
    RefusingBucket, SpawnGate,
};
use crate::test_support::coverage_relaxed;

const SEC: u64 = 1_000_000_000;

/// xorshift64*; no dependency, seeded, deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn open_in(dir: &tempfile::TempDir, name: &str) -> SharedBudget {
    SharedBudget::open(&dir.path().join(name)).expect("open")
}

/// The pre-change decision: read, refill, take, and ALWAYS write. The oracle
/// the skipped-write path must never beat.
fn take_always_writing(
    h: &SharedBudget,
    floor: u32,
    burst: u32,
    per_sec: u32,
    now_ns: u64,
) -> Take {
    let locked = match h.lock() {
        Ok(l) => l,
        Err(r) => return Take::Unavailable(r),
    };
    let (mut state, _reset) = match h.read_state(&locked, burst, now_ns) {
        Ok(r) => r,
        Err(r) => return Take::Unavailable(r),
    };
    refill(&mut state, burst, per_sec, now_ns);
    let granted = state.tokens > floor;
    if granted {
        state.tokens -= 1;
    }
    match (granted, h.write_state(&locked, state)) {
        (true, Ok(())) => Take::Granted,
        (false, Ok(())) => Take::Denied,
        (true, Err(r)) => Take::Unavailable(r),
        (false, Err(r)) => Take::DeniedUnrecorded(r),
    }
}

struct ModelResult {
    lane: u64,
    oracle: u64,
    bound: u64,
    lane_grant_oracle_denied: u64,
    steps: u64,
}

/// Random ops over three handles per record, on an injected clock that steps
/// forward, stands still, jumps far forward and steps back, with foreign
/// undecodable blobs landing between takes. The lane's record and an
/// always-write oracle record see the identical op stream.
fn model(seed: u64, burst: u32, per_sec: u32, steps: u64) -> ModelResult {
    let mut rng = Rng(seed | 1);
    let dir = tempfile::tempdir().unwrap();
    let lane: Vec<_> = (0..3).map(|_| open_in(&dir, "lane")).collect();
    let oracle: Vec<_> = (0..3).map(|_| open_in(&dir, "oracle")).collect();
    let mut now = 1_000 * SEC;
    let mut positive_ns = 0u128;
    let (mut lane_grants, mut oracle_grants, mut diverge) = (0u64, 0u64, 0u64);
    for _ in 0..steps {
        let before = now;
        match rng.below(100) {
            0..=69 => now += rng.below(400_000_000),
            70..=84 => {}
            85..=94 => now += (1 + rng.below(30)) * SEC,
            _ => now = now.saturating_sub((1 + rng.below(20)) * SEC).max(SEC),
        }
        if now > before {
            positive_ns += u128::from(now - before);
        }
        match rng.below(100) {
            0..=84 => {
                let k = rng.below(3) as usize;
                let floor = [0, 0, 2, burst / 4][rng.below(4) as usize].min(burst);
                let a = lane[k].take_at(floor, burst, per_sec, now);
                let b = take_always_writing(&oracle[k], floor, burst, per_sec, now);
                lane_grants += u64::from(a == Take::Granted);
                oracle_grants += u64::from(b == Take::Granted);
                if a == Take::Granted && b != Take::Granted {
                    diverge += 1;
                }
            }
            _ => {
                let blob: Vec<u8> = match rng.below(4) {
                    0 => b"garbage".to_vec(),
                    1 => stamped(1, burst, now),
                    2 => {
                        let mut v = stamped(VERSION, burst, now);
                        v.push(0);
                        v
                    }
                    _ => vec![0xFF; RECORD_LEN],
                };
                std::fs::write(dir.path().join("lane"), &blob).unwrap();
                std::fs::write(dir.path().join("oracle"), &blob).unwrap();
            }
        }
    }
    let bound = u64::from(burst) + (positive_ns * u128::from(per_sec) / SEC as u128) as u64 + 1;
    ModelResult {
        lane: lane_grants,
        oracle: oracle_grants,
        bound,
        lane_grant_oracle_denied: diverge,
        steps,
    }
}

fn stamped(version: u32, tokens: u32, at_ns: u64) -> Vec<u8> {
    let mut record = vec![0u8; RECORD_LEN];
    record[0..4].copy_from_slice(&MAGIC);
    record[4..8].copy_from_slice(&version.to_le_bytes());
    record[8..12].copy_from_slice(&tokens.to_le_bytes());
    record[16..24].copy_from_slice(&at_ns.to_le_bytes());
    record
}

/// ATTACK 1a. Model check: total grants stay within burst + rate * (sum of
/// forward clock movement), and never exceed what an always-writing oracle
/// grants on the same op stream.
#[test]
fn model_check_skipped_denial_writes_never_mint() {
    let mut worst_gap = 0i64;
    for (seed, burst, per_sec) in [
        (1u64, 8u32, 3u32),
        (2, 16, 7),
        (3, 16, 8),
        (4, 192, 8),
        (5, 4, 1),
        (6, 5, 1000),
        (7, 32, 3),
        (8, 12, 13),
        (9, 64, 1),
        (10, 2, 5),
    ] {
        for rep in 0..20u64 {
            let r = model(seed * 7919 + rep, burst, per_sec, 3_000);
            worst_gap = worst_gap.max(r.lane as i64 - r.oracle as i64);
            assert!(
                r.lane <= r.bound,
                "seed {seed}/{rep} burst {burst} rate {per_sec}: lane granted {} over bound {} ({} steps)",
                r.lane,
                r.bound,
                r.steps
            );
            assert!(
                r.lane <= r.oracle,
                "seed {seed}/{rep} burst {burst} rate {per_sec}: lane granted {} > always-write oracle {} \
                 ({} step(s) where lane granted and the oracle denied)",
                r.lane,
                r.oracle,
                r.lane_grant_oracle_denied
            );
        }
    }
    println!("MODEL: worst lane-minus-oracle grants over 200 runs = {worst_gap}");
}

/// ATTACK 1b. A reads and denies (no write); B (another fd) grants and writes;
/// A's later view equals what the oracle's would be.
#[test]
fn interleaved_silent_denial_then_foreign_grant_matches_always_write() {
    for per_sec in [1u32, 3, 7, 8] {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (open_in(&dir, "l"), open_in(&dir, "l"));
        let (oa, ob) = (open_in(&dir, "o"), open_in(&dir, "o"));
        let burst = 6;
        let t0 = 500 * SEC;
        for _ in 0..burst {
            assert_eq!(a.take_at(0, burst, per_sec, t0), Take::Granted);
            assert_eq!(oa.take_at(0, burst, per_sec, t0), Take::Granted);
        }
        // A's first denial is the probe write; let it pass, then silence.
        assert_eq!(a.take_at(0, burst, per_sec, t0), Take::Denied);
        assert_eq!(oa.take_at(0, burst, per_sec, t0), Take::Denied);
        let mut lane = 0;
        let mut oracle = 0;
        for i in 1..=200u64 {
            let now = t0 + i * 17_000_000;
            let (x, ox) = if i % 3 == 0 { (&b, &ob) } else { (&a, &oa) };
            lane += u32::from(x.take_at(0, burst, per_sec, now) == Take::Granted);
            oracle += u32::from(take_always_writing(ox, 0, burst, per_sec, now) == Take::Granted);
        }
        assert!(
            lane <= oracle,
            "rate {per_sec}: lane {lane} oracle {oracle}"
        );
    }
}

/// ATTACK 1c. Forward then back, a denial in the gap: a stepped-back clock
/// re-anchors with a write, and the forward step's credit is not paid twice
/// beyond what the oracle pays.
#[test]
fn clock_forward_then_back_does_not_pay_more_than_always_write() {
    let (burst, per_sec) = (10u32, 2u32);
    let dir = tempfile::tempdir().unwrap();
    let (a, o) = (open_in(&dir, "l"), open_in(&dir, "o"));
    let t0 = 1_000 * SEC;
    let mut lane = 0u32;
    let mut oracle = 0u32;
    for _ in 0..burst {
        lane += u32::from(a.take_at(0, burst, per_sec, t0) == Take::Granted);
        oracle += u32::from(take_always_writing(&o, 0, burst, per_sec, t0) == Take::Granted);
    }
    // 1.2 s forward: 2 tokens' worth credited with floor 5 -> a denial (level 2).
    for (t, floor) in [
        (t0 + 1_200_000_000, 5),
        (t0 + 1_200_000_000, 5),
        (t0 - 3 * SEC, 5),
        (t0 - 3 * SEC + 400_000_000, 0),
        (t0 + 1_200_000_000, 0),
        (t0 + 1_200_000_000, 0),
        (t0 + 90 * SEC, 0),
        (t0 - 50 * SEC, 5),
        (t0 + 90 * SEC, 0),
    ] {
        lane += u32::from(a.take_at(floor, burst, per_sec, t) == Take::Granted);
        oracle += u32::from(take_always_writing(&o, floor, burst, per_sec, t) == Take::Granted);
    }
    assert!(lane <= oracle, "lane {lane} oracle {oracle}");
}

/// ATTACK 1d. Refill anchor at the edge of u64.
#[test]
fn anchor_near_u64_max_mints_nothing_and_does_not_panic() {
    for anchor in [
        u64::MAX,
        u64::MAX - 1,
        u64::MAX - 999_999,
        u64::MAX - SEC,
        u64::MAX / 2,
    ] {
        for now in [0u64, 1, anchor.saturating_sub(1), anchor, u64::MAX] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(FILE_NAME);
            let mut rec = [0u8; RECORD_LEN];
            rec[0..4].copy_from_slice(&MAGIC);
            rec[4..8].copy_from_slice(&VERSION.to_le_bytes());
            rec[8..12].copy_from_slice(&0u32.to_le_bytes());
            rec[16..24].copy_from_slice(&anchor.to_le_bytes());
            std::fs::write(&path, rec).unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            let h = SharedBudget::open(&path).unwrap();
            let mut grants = 0;
            for k in 0..50u64 {
                grants += u32::from(
                    h.take_at(0, 8, 8, now.saturating_add(k * 1_000_000)) == Take::Granted,
                );
            }
            // Only forward time since the anchor (or since `now`, once a
            // stepped-back clock re-anchors there) can be credited.
            let end = now.saturating_add(49_000_000);
            let from = if now >= anchor { anchor } else { now };
            let allowed = u32::try_from((end - from) / 1_000_000 * 8 / 1000)
                .unwrap_or(u32::MAX)
                .saturating_add(1);
            assert!(
                grants <= allowed.min(8),
                "anchor {anchor} now {now}: {grants} grants, allowed {allowed}"
            );
        }
    }
}

/// ATTACK 1e. A foreign undecodable blob lands between a silent denial and
/// the next take: still empty, not a gift; a foreign blob of another version
/// between two foreign writers does not let the silent record resurface.
#[test]
fn foreign_blob_after_silent_denial_resets_empty_and_stays_empty() {
    let dir = tempfile::tempdir().unwrap();
    let h = open_in(&dir, "l");
    let (burst, per_sec) = (8u32, 2u32);
    let t0 = 100 * SEC;
    for _ in 0..burst {
        assert_eq!(h.take_at(0, burst, per_sec, t0), Take::Granted);
    }
    for i in 0..30u64 {
        assert_eq!(
            h.take_at(0, burst, per_sec, t0 + i * 10_000_000),
            Take::Denied
        );
    }
    let path = dir.path().join("l");
    for blob in [
        stamped(1, burst, t0),
        b"xx".to_vec(),
        vec![0u8; 31],
        stamped(3, 99, t0),
    ] {
        std::fs::write(&path, &blob).unwrap();
        let now = t0 + 400_000_000;
        let out = h.take_at(0, burst, per_sec, now);
        assert!(
            matches!(out, Take::Denied | Take::Unavailable(_)),
            "{blob:?}: {out:?}"
        );
        assert!(!matches!(out, Take::Granted), "{blob:?} minted");
    }
}

/// ATTACK 3. A long-lived handle whose record stops taking writes mid-life is
/// reported `DeniedUnrecorded` within DENIAL_PROBE of the failure, by the
/// injected clock.
#[test]
fn probe_reports_a_record_that_went_read_only_mid_life_within_ten_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let h = open_in(&dir, "l");
    let (burst, per_sec) = (4u32, 1u32);
    let t0 = 1_000 * SEC;
    for _ in 0..burst {
        assert_eq!(h.take_at(0, burst, per_sec, t0), Take::Granted);
    }
    assert_eq!(h.take_at(1, burst, per_sec, t0), Take::Denied, "probe");
    h.fail_writes(true);
    let mut first_reported = None;
    for ms in (50..=12_000u64).step_by(50) {
        let now = t0 + ms * 1_000_000;
        // Floor 4 == burst, so the level never crosses it: always a denial.
        if matches!(h.take_at(4, burst, per_sec, now), Take::DeniedUnrecorded(_)) {
            first_reported = Some(ms);
            break;
        }
    }
    let ms = first_reported.expect("a read-only record was never reported");
    println!("PROBE: first DeniedUnrecorded {ms} ms after the failure");
    assert!(ms <= 10_000, "reported only after {ms} ms");
    // Once reported it stays reported (the failed probe did not refresh it).
    let now = t0 + (ms + 50) * 1_000_000;
    assert!(matches!(
        h.take_at(4, burst, per_sec, now),
        Take::DeniedUnrecorded(_)
    ));
}

/// ATTACK 3b. The probe timer is the record clock handed to `take_at`, not a
/// real `Instant`: stepping that clock BACK past the stamp re-probes at once.
#[test]
fn probe_follows_the_injected_clock_and_reprobes_after_a_step_back() {
    let dir = tempfile::tempdir().unwrap();
    let h = open_in(&dir, "l");
    let t0 = 1_000 * SEC;
    for _ in 0..4 {
        let _ = h.take_at(0, 4, 1, t0);
    }
    assert_eq!(h.take_at(4, 4, 1, t0), Take::Denied);
    let w = h.writes();
    assert_eq!(h.take_at(4, 4, 1, t0 + SEC), Take::Denied);
    assert_eq!(h.writes(), w, "fresh probe: no write");
    assert_eq!(h.take_at(4, 4, 1, t0 - SEC), Take::Denied);
    assert_eq!(h.writes(), w + 1, "stepped back: re-probe");
}

// ---- gate-level attacks (real clock; limits scaled) ----------------------

fn drained_gate(rate: u32, burst: u32) -> (tempfile::TempDir, &'static SpawnGate) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let gate: &'static SpawnGate = Box::leak(Box::new(
        SpawnGate::with_rate(32, burst, rate).sharing(Ok(path.clone())),
    ));
    let other = SharedBudget::open(&path).unwrap();
    while other.take(0, burst, rate) == Take::Granted {}
    (dir, gate)
}

/// ATTACK 2. Four waiters re-entering `acquire` against a drained record that
/// refills at 8/s. Prints the grant distribution; every waiter must get some.
#[test]
fn four_waiters_each_get_tokens_under_the_nap_schedule() {
    let (_dir, gate) = drained_gate(8, 4);
    let window = Duration::from_secs(10);
    let end = Instant::now() + window;
    let per: Vec<(u64, u64)> = std::thread::scope(|scope| {
        let ws: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(move || {
                    let (mut ok, mut refused) = (0u64, 0u64);
                    while Instant::now() < end {
                        let deadline = (Instant::now() + Duration::from_millis(1_500)).min(end);
                        match with_admission(Admission::Reactive, || {
                            gate.acquire_judged(deadline, &|| false)
                        }) {
                            Ok(_permit) => ok += 1,
                            Err(_) => refused += 1,
                        }
                    }
                    (ok, refused)
                })
            })
            .collect();
        ws.into_iter().map(|w| w.join().unwrap()).collect()
    });
    let total: u64 = per.iter().map(|p| p.0).sum();
    println!("FAIRNESS: per waiter (grants, refusals) over 10 s at 8/s: {per:?}, total {total}");
    assert!(per.iter().all(|p| p.0 >= 1), "a waiter starved: {per:?}");
    let min = per.iter().map(|p| p.0).min().unwrap();
    let max = per.iter().map(|p| p.0).max().unwrap();
    assert!(
        max <= min.saturating_mul(4).max(8),
        "grants are lopsided: {per:?}"
    );
    assert!(
        total >= 40,
        "the record's 8/s refill was underused: {total}"
    );
}

/// ATTACK 2b. One impatient hog re-enters instantly against three patient
/// single-shot waiters with a 1.5 s deadline each: do the patient ones win?
#[test]
fn patient_waiters_are_not_starved_by_an_instant_reentrant() {
    let (_dir, gate) = drained_gate(8, 4);
    let window = Duration::from_secs(8);
    let end = Instant::now() + window;
    let (hog, patient): (u64, Vec<(u64, u64)>) = std::thread::scope(|scope| {
        let hog = scope.spawn(move || {
            let mut ok = 0u64;
            while Instant::now() < end {
                let d = Instant::now() + Duration::from_millis(30);
                ok += u64::from(
                    with_admission(Admission::Reactive, || gate.acquire_judged(d, &|| false))
                        .is_ok(),
                );
            }
            ok
        });
        let ps: Vec<_> = (0..3)
            .map(|_| {
                scope.spawn(move || {
                    let (mut ok, mut refused) = (0u64, 0u64);
                    while Instant::now() < end {
                        let d = Instant::now() + Duration::from_millis(1_500);
                        match with_admission(Admission::Reactive, || {
                            gate.acquire_judged(d, &|| false)
                        }) {
                            Ok(_p) => ok += 1,
                            Err(_) => refused += 1,
                        }
                    }
                    (ok, refused)
                })
            })
            .collect();
        (
            hog.join().unwrap(),
            ps.into_iter().map(|p| p.join().unwrap()).collect(),
        )
    });
    println!("HOG: hog grants {hog}; patient (grants, refusals) {patient:?}");
    assert!(
        patient.iter().all(|p| p.0 >= 1),
        "patient starved: hog {hog}, {patient:?}"
    );
}

/// ATTACK 2c. A waiter whose LOCAL bucket is the empty one, after an earlier
/// shared denial left `next_shared_ask` in the past, naps `MIN_TOKEN_NAP`
/// (1 ms) per pass instead of until a token is due: a busy loop that locks the
/// gate and runs the caller's cancel check ~1000x a second.
#[test]
fn a_local_bucket_waiter_does_not_spin_on_a_stale_shared_ask_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let gate = SpawnGate::with_rate(8, 8, 4).sharing(Ok(path.clone()));
    let other = SharedBudget::open(&path).unwrap();
    while other.take(0, 8, 4) == Take::Granted {}
    let polls = std::sync::atomic::AtomicU64::new(0);
    let began = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            // After the first shared denial (immediate), empty the LOCAL
            // bucket and freeze its refill.
            std::thread::sleep(Duration::from_millis(60));
            let mut s = gate.state.lock().unwrap();
            s.tokens = 0;
            s.tokens_at = Some(Instant::now() + Duration::from_secs(86_400));
        });
        let r = with_admission(Admission::Reactive, || {
            gate.acquire_judged(began + Duration::from_millis(1_200), &|| {
                polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            })
        });
        assert!(r.is_err());
    });
    let n = polls.load(std::sync::atomic::Ordering::Relaxed);
    println!("SPIN: cancel check ran {n} times in 1.2 s with an empty local bucket");
    // Capped nap is 50 ms: ~24 passes. Allow generous slack.
    assert!(n <= 40, "waiter spun: {n} passes in 1.2 s");
}

/// ATTACK 5. A cancelled waiter returns within the 50 ms cap plus slack; a
/// deadline shorter than the next token returns at that deadline, not later.
#[test]
fn cancel_mid_nap_returns_within_the_poll_cap_and_short_deadlines_hold() {
    let (_dir, gate) = drained_gate(1, 4);
    let flag = std::sync::atomic::AtomicBool::new(false);
    let mut lat = Vec::new();
    for _ in 0..3 {
        flag.store(false, std::sync::atomic::Ordering::SeqCst);
        let set_at = std::sync::Mutex::new(None::<Instant>);
        let (res, done) = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(120));
                *set_at.lock().unwrap() = Some(Instant::now());
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            let r = with_admission(Admission::Reactive, || {
                let t = Instant::now();
                let r = gate.acquire_judged(t + Duration::from_secs(30), &|| {
                    flag.load(std::sync::atomic::Ordering::SeqCst)
                });
                r
            });
            (r.err().map(|e| e.0), Instant::now())
        });
        assert_eq!(res, Some(Refusal::Cancelled), "iteration {}", lat.len());
        println!(
            "CANCEL iter {}: {:?}",
            lat.len(),
            done.duration_since(set_at.lock().unwrap().unwrap())
        );
        lat.push(done.duration_since(set_at.lock().unwrap().unwrap()));
    }
    let worst = lat.iter().max().copied().unwrap();
    println!("CANCEL: latencies {lat:?}");
    assert!(
        worst < coverage_relaxed(host_wake_50ms() + Duration::from_millis(40)),
        "cancel took {worst:?}"
    );

    // rate 1/s: next token due in ~1 s; deadline 120 ms.
    let (_dir2, gate) = drained_gate(1, 4);
    let began = Instant::now();
    let r = with_admission(Admission::Reactive, || {
        gate.acquire_judged(began + Duration::from_millis(120), &|| false)
    });
    let took = began.elapsed();
    assert!(
        matches!(r, Err((Refusal::Refused { .. }, Some(_)))),
        "{:?}",
        r.err()
    );
    println!("DEADLINE: refused after {took:?} (deadline 120 ms)");
    assert!(
        took < coverage_relaxed(
            Duration::from_millis(120) + host_wake_50ms() + Duration::from_millis(40)
        ),
        "slept past the deadline: {took:?}"
    );
    assert!(
        took >= Duration::from_millis(110),
        "returned early: {took:?}"
    );
}

/// The worst of six real 50 ms `Condvar::wait_timeout`s here, independent of
/// the gate: a loaded host oversleeps a 50 ms wait several times over, and the
/// poll cap can only be as tight as the host's timer.
fn host_wake_50ms() -> Duration {
    let m = std::sync::Mutex::new(());
    let cv = std::sync::Condvar::new();
    let worst = (0..6)
        .map(|_| {
            let g = m.lock().unwrap();
            let t = Instant::now();
            let _ = cv.wait_timeout(g, Duration::from_millis(50)).unwrap();
            t.elapsed()
        })
        .max()
        .unwrap();
    println!("CONTROL: worst of 6 x wait_timeout(50ms) = {worst:?}");
    worst
}

/// ATTACK 4. The tail on every variant is one line, no quote, no backslash, no
/// control character; still a deferral to the Rust classifier; the number
/// after the first `s:` still parses.
#[test]
fn refusal_tail_is_single_line_json_safe_and_classified() {
    let level = Some(BucketLevel {
        bucket: RefusingBucket::Shared,
        tokens: u32::MAX,
        floor: u32::MAX,
        burst: u32::MAX,
    });
    for refusal in [
        Refusal::Refused {
            waited: Duration::from_millis(2_013),
        },
        Refusal::Shed,
        Refusal::Pressure { reactive: true },
        Refusal::Pressure { reactive: false },
        Refusal::Cancelled,
        Refusal::TimedOut {
            deadline: Duration::from_secs(3),
        },
    ] {
        let message = refusal_message_at("git status", refusal, level);
        assert!(
            !message.contains(['\n', '\r', '"', '\\', '\t']),
            "{message}"
        );
        assert!(message.chars().all(|c| !c.is_control()), "{message}");
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(json.len(), message.len() + 2, "needed escaping: {message}");
        println!("TEXT {refusal:?}: {message}");
        if matches!(
            refusal,
            Refusal::Refused { .. } | Refusal::Shed | Refusal::Pressure { .. }
        ) {
            assert!(is_deferred_under_load(&message), "{message}");
        }
    }
    // Pressure and Cancelled/TimedOut never carry the tail.
    for refusal in [
        Refusal::Pressure { reactive: true },
        Refusal::Cancelled,
        Refusal::TimedOut {
            deadline: Duration::from_secs(1),
        },
    ] {
        assert!(!refusal_message_at("git status", refusal, level).contains("bucket:"));
    }
}
