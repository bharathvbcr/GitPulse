use super::*;

fn ms(n: u64) -> Outcome {
    Outcome::Sample(Duration::from_millis(n))
}

/// A window that has seen `samples` milliseconds, one probe per cadence, and
/// the instant just after the last of them.
fn window_of(samples: &[u64]) -> (Window, Instant) {
    let mut window = Window::default();
    let mut at = Instant::now();
    for sample in samples {
        window.push(at, ms(*sample));
        at += CADENCE;
    }
    (window, at)
}

fn value(reading: Reading<ExecLatency>) -> ExecLatency {
    match reading {
        Reading::Value(latency) => latency,
        other => panic!("expected a value, got {other:?}"),
    }
}

#[test]
fn the_median_comes_from_the_injected_timings() {
    let (window, now) = window_of(&[100, 400, 200, 300, 250]);
    let latency = value(window.reading(now));
    assert_eq!(latency.median_ms, 250, "{latency:?}");
    assert_eq!(latency.n, 5);
    assert!(!latency.slow);
}

#[test]
fn an_even_count_takes_the_mean_of_the_middle_two() {
    let (window, now) = window_of(&[100, 200, 300, 400]);
    assert_eq!(value(window.reading(now)).median_ms, 250);
}

#[test]
fn only_the_last_window_of_probes_counts() {
    let mut samples = vec![5_000; WINDOW];
    samples.extend([10; WINDOW]);
    let (window, now) = window_of(&samples);
    let latency = value(window.reading(now));
    assert_eq!(latency.median_ms, 10, "old slow probes left the window");
    assert_eq!(latency.n as usize, WINDOW);
}

#[test]
fn a_probe_that_has_not_run_is_not_yet_and_never_fast() {
    let window = Window::default();
    assert_eq!(
        window.reading(Instant::now()),
        Reading::Unavailable(Why::NotYet)
    );
}

#[test]
fn too_few_samples_are_not_a_median() {
    let (window, now) = window_of(&[900; MIN_SAMPLES - 1]);
    assert_eq!(window.reading(now), Reading::Unavailable(Why::NotYet));
}

#[test]
fn failed_probes_are_unknown_never_fast() {
    let at = Instant::now();
    let mut window = Window::default();
    window.push(at, Outcome::Failed);
    assert_eq!(
        window.reading(at),
        Reading::Unavailable(Why::Failed),
        "a probe that could not run is a failed check, not a quiet one"
    );
    // Two samples and a failure still do not make three samples.
    window.push(at, ms(5));
    window.push(at, ms(5));
    assert_eq!(window.reading(at), Reading::Unavailable(Why::Failed));
}

#[test]
fn a_run_of_failures_outweighs_older_fast_samples() {
    let (mut window, mut at) = window_of(&[5; WINDOW]);
    assert!(matches!(window.reading(at), Reading::Value(_)));
    for _ in 0..FAILURE_RUN {
        window.push(at, Outcome::Failed);
        at += CADENCE;
    }
    assert_eq!(
        window.reading(at),
        Reading::Unavailable(Why::Failed),
        "the machine can no longer be said to start processes quickly"
    );
    // One probe that lands again ends the run.
    window.push(at, ms(5));
    assert!(matches!(window.reading(at), Reading::Value(_)));
}

#[test]
fn a_window_whose_newest_outcome_is_old_is_unavailable() {
    let (window, now) = window_of(&[5; WINDOW]);
    let newest = now - CADENCE;
    assert!(matches!(window.reading(newest), Reading::Value(_)));
    assert!(matches!(
        window.reading(newest + STALE_AFTER),
        Reading::Value(_)
    ));
    assert_eq!(
        window.reading(newest + STALE_AFTER + Duration::from_millis(1)),
        Reading::Unavailable(Why::Failed),
        "a probe thread that stopped cannot leave a fast reading standing"
    );
}

#[test]
fn a_probe_that_hit_its_deadline_is_the_slowest_sample_not_no_signal() {
    let (window, now) = window_of(&[DEADLINE.as_millis() as u64; MIN_SAMPLES]);
    let latency = value(window.reading(now));
    assert_eq!(latency.median_ms, 10_000);
    assert!(latency.slow);
}

#[test]
fn the_latch_sets_at_the_enter_line_and_clears_only_below_the_exit_line() {
    let (mut window, mut at) = window_of(&[ENTER_MS; MIN_SAMPLES]);
    assert!(
        value(window.reading(at)).slow,
        "a median at {ENTER_MS} ms is slow"
    );
    // Recovering to between the two lines is still slow.
    for _ in 0..WINDOW {
        window.push(at, ms(EXIT_MS));
        at += CADENCE;
    }
    let held = value(window.reading(at));
    assert_eq!(held.median_ms, EXIT_MS);
    assert!(
        held.slow,
        "{EXIT_MS} ms is not below the exit line: {held:?}"
    );
    for _ in 0..WINDOW {
        window.push(at, ms(EXIT_MS - 1));
        at += CADENCE;
    }
    assert!(
        !value(window.reading(at)).slow,
        "below the exit line clears"
    );
}

#[test]
fn a_median_hovering_at_the_line_does_not_flap() {
    let mut window = Window::default();
    let mut at = Instant::now();
    let mut transitions = 0;
    let mut last = false;
    for i in 0..60u64 {
        window.push(
            at,
            ms(if i % 2 == 0 {
                ENTER_MS - 20
            } else {
                ENTER_MS + 20
            }),
        );
        at += CADENCE;
        if let Reading::Value(latency) = window.reading(at) {
            if latency.slow != last {
                transitions += 1;
                last = latency.slow;
            }
        }
    }
    assert!(
        transitions <= 1,
        "the verdict followed the median across the line {transitions} times"
    );
}

#[test]
fn reading_a_window_again_never_moves_the_latch() {
    let (window, now) = window_of(&[ENTER_MS + 100; MIN_SAMPLES]);
    for _ in 0..5 {
        assert!(value(window.reading(now)).slow);
    }
}

#[test]
fn probe_once_records_what_the_injected_probe_returns() {
    let window = Mutex::new(Window::default());
    let clock = Instant::now();
    let mut results = vec![ms(40), Outcome::Failed, ms(60), ms(80)].into_iter();
    for _ in 0..4 {
        probe_once(&window, &|| clock, &mut || {
            results.next().expect("four probes")
        });
    }
    let latency = value(window.lock().unwrap().reading(clock));
    assert_eq!(latency.n, 3, "the failed probe is not a sample");
    assert_eq!(latency.median_ms, 60);
}

#[test]
fn the_latency_serializes_with_its_median_count_and_flag() {
    let json = serde_json::to_value(ExecLatency {
        median_ms: 1_360,
        n: 12,
        slow: true,
    })
    .unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "median_ms": 1360, "n": 12, "slow": true })
    );
}
