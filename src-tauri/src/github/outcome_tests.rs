use super::*;
use std::time::Duration;

fn run(id: u64, status: &str, conclusion: &str) -> WorkflowRunInfo {
    WorkflowRunInfo {
        id,
        name: "CI".into(),
        title: format!("run {id}"),
        status: status.into(),
        conclusion: conclusion.into(),
        head_branch: "gitpulse/fix-1234abcd".into(),
        url: format!("https://github.com/acme/app/actions/runs/{id}"),
        created_at: String::new(),
        started_at: String::new(),
        updated_at: String::new(),
        head_sha: "a".repeat(40),
        event: "push".into(),
        outcome: classify(status, conclusion),
    }
}

fn moving(id: u64) -> WorkflowRunInfo {
    run(id, "in_progress", "")
}

fn ended(id: u64, conclusion: &str) -> WorkflowRunInfo {
    run(id, "completed", conclusion)
}

#[test]
fn every_status_and_conclusion_gh_documents_has_one_outcome() {
    let in_flight = ["queued", "in_progress", "requested", "waiting", "pending"];
    for status in in_flight {
        assert_eq!(classify(status, ""), RunOutcome::InFlight, "{status}");
        // A conclusion on a moving run is not read: it cannot have ended.
        assert_eq!(
            classify(status, "success"),
            RunOutcome::InFlight,
            "{status}"
        );
    }
    let table = [
        ("success", RunOutcome::Passed),
        ("failure", RunOutcome::Failed),
        ("timed_out", RunOutcome::Failed),
        ("startup_failure", RunOutcome::Failed),
        ("cancelled", RunOutcome::Cancelled),
        ("action_required", RunOutcome::NeedsPerson),
        ("neutral", RunOutcome::Unknown),
        ("skipped", RunOutcome::Unknown),
        ("stale", RunOutcome::Unknown),
        ("", RunOutcome::Unknown),
        ("a_conclusion_from_the_future", RunOutcome::Unknown),
    ];
    for (conclusion, expected) in table {
        assert_eq!(
            classify("completed", conclusion),
            expected,
            "{conclusion:?}"
        );
    }
}

/// The cases file is also what the renderer's test fixtures build their runs
/// from (`src/lib/github/runOutcomeFixtures.ts`), so a fixture's outcome is this
/// classifier's answer and not a second opinion. It has to be right, and it has
/// to cover every word `gh run list --help` documents.
#[test]
fn the_shared_cases_are_this_classifiers_answers_and_cover_every_documented_word() {
    #[derive(serde::Deserialize)]
    struct Case {
        status: String,
        conclusion: String,
        outcome: RunOutcome,
    }
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("run_outcome_cases.json")).expect("cases parse");
    for case in &cases {
        assert_eq!(
            classify(&case.status, &case.conclusion),
            case.outcome,
            "{:?}/{:?}",
            case.status,
            case.conclusion
        );
    }
    let has = |status: &str, conclusion: &str| {
        cases
            .iter()
            .any(|case| case.status == status && case.conclusion == conclusion)
    };
    for status in ["queued", "in_progress", "requested", "waiting", "pending"] {
        assert!(has(status, ""), "{status} has no case");
    }
    for conclusion in [
        "action_required",
        "cancelled",
        "failure",
        "neutral",
        "skipped",
        "stale",
        "startup_failure",
        "success",
        "timed_out",
        "",
    ] {
        assert!(
            has("completed", conclusion),
            "completed/{conclusion} has no case"
        );
    }
}

#[test]
fn a_state_this_build_has_not_heard_of_is_never_a_verdict() {
    assert_eq!(classify("", ""), RunOutcome::Unknown);
    assert_eq!(classify("from_the_future", "failure"), RunOutcome::Unknown);
    assert_eq!(classify("COMPLETED", " Failure "), RunOutcome::Failed);
}

/// The rule the work view and the Remote panel used to disagree about: a run
/// held for a person is attention-worthy and is not a failure.
#[test]
fn action_required_needs_a_person_and_is_neither_failed_nor_a_verdict() {
    let outcome = classify("completed", "action_required");
    assert_eq!(outcome, RunOutcome::NeedsPerson);
    assert_ne!(outcome, RunOutcome::Failed);
    assert!(!outcome.is_verdict());
}

#[test]
fn an_outcome_nobody_computed_decodes_as_unknown() {
    let mut value = serde_json::to_value(ended(1, "success")).unwrap();
    value.as_object_mut().unwrap().remove("outcome");
    let decoded: WorkflowRunInfo = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.outcome, RunOutcome::Unknown);
}

#[test]
fn the_wire_names_are_snake_case() {
    let wire = serde_json::to_value(RunOutcome::NeedsPerson).unwrap();
    assert_eq!(wire, serde_json::json!("needs_person"));
}

fn finished_ids(observed: &Observed) -> Vec<u64> {
    observed.finished.iter().map(|r| r.id).collect()
}

#[test]
fn the_first_look_is_a_baseline_and_announces_nothing() {
    let runs = [ended(1, "failure"), moving(2), ended(3, "success")];
    let observed = diff(None, &runs, false);
    assert!(observed.finished.is_empty());
    assert!(!observed.capped);
}

#[test]
fn a_run_seen_moving_then_failed_is_one_finish() {
    let before = baseline_of(&[moving(7)]);
    let observed = diff(Some(&before), &[ended(7, "failure")], false);
    assert_eq!(finished_ids(&observed), vec![7]);
    assert_eq!(observed.finished[0].outcome, RunOutcome::Failed);
}

#[test]
fn a_run_first_seen_already_settled_is_history_not_a_finish() {
    let before = baseline_of(&[ended(1, "success")]);
    let observed = diff(
        Some(&before),
        &[ended(9, "failure"), ended(1, "success")],
        false,
    );
    assert!(observed.finished.is_empty());
}

#[test]
fn a_settled_run_staying_settled_is_not_announced_again() {
    let before = baseline_of(&[ended(7, "failure")]);
    assert!(diff(Some(&before), &[ended(7, "failure")], false)
        .finished
        .is_empty());
}

#[test]
fn cancelled_is_its_own_finish_and_not_a_failure() {
    let before = baseline_of(&[moving(4)]);
    let observed = diff(Some(&before), &[ended(4, "cancelled")], false);
    assert_eq!(observed.finished[0].outcome, RunOutcome::Cancelled);
}

#[test]
fn a_run_held_for_a_person_is_not_a_finish() {
    let before = baseline_of(&[moving(4)]);
    let observed = diff(Some(&before), &[ended(4, "action_required")], false);
    assert!(observed.finished.is_empty());
    let observed = diff(Some(&before), &[ended(4, "neutral")], false);
    assert!(observed.finished.is_empty());
}

#[test]
fn rows_are_matched_by_id_so_a_new_push_shifting_the_list_changes_nothing() {
    let before = baseline_of(&[moving(2), ended(1, "success")]);
    // A newer run arrives at the head; run 2 finishes. Index-wise every row moved.
    let observed = diff(
        Some(&before),
        &[moving(3), ended(2, "success"), ended(1, "success")],
        false,
    );
    assert_eq!(finished_ids(&observed), vec![2]);
}

#[test]
fn a_duplicate_id_is_believed_in_its_newest_row_and_counted_once() {
    let before = baseline_of(&[moving(5)]);
    let observed = diff(Some(&before), &[ended(5, "failure"), moving(5)], false);
    assert_eq!(finished_ids(&observed), vec![5]);
}

#[test]
fn a_vanished_run_says_nothing_when_the_list_is_complete() {
    let before = baseline_of(&[moving(8)]);
    let observed = diff(Some(&before), &[], false);
    assert!(observed.finished.is_empty());
    assert!(!observed.capped);
}

#[test]
fn a_capped_list_is_reported_as_capped_and_still_finds_what_it_can_see() {
    let before = baseline_of(&[moving(8), moving(9), ended(1, "success")]);
    let observed = diff(Some(&before), &[ended(9, "failure")], true);
    assert!(
        observed.capped,
        "run 8 fell off the cap; the listing says it is cut short"
    );
    assert_eq!(finished_ids(&observed), vec![9]);
}

#[test]
fn a_capped_first_look_is_reported_as_capped() {
    let observed = diff(None, &[moving(1)], true);
    assert!(observed.capped);
}

fn later(base: Instant, seconds: u64) -> Instant {
    base + Duration::from_secs(seconds)
}

#[test]
fn the_observer_announces_a_finish_once_however_many_pollers_look() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    assert!(observer
        .observe("acme/app", &[moving(1)], false, t0)
        .is_some());
    let first = observer
        .observe("acme/app", &[ended(1, "failure")], false, later(t0, 1))
        .unwrap();
    assert_eq!(finished_ids(&first), vec![1]);
    // The panel and the watcher both poll: the second look finds nothing new.
    let second = observer
        .observe("acme/app", &[ended(1, "failure")], false, later(t0, 2))
        .unwrap();
    assert!(second.finished.is_empty());
    assert_eq!(
        observer.take_pending(10).len(),
        1,
        "queued for the watcher once"
    );
}

#[test]
fn a_listing_older_than_the_baseline_is_ignored() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    observer.observe("acme/app", &[moving(1)], false, t0);
    observer.observe("acme/app", &[ended(1, "failure")], false, later(t0, 5));
    observer.take_pending(10);
    // A slow call, requested before the finish was seen, lands afterwards.
    let stale = observer.observe("acme/app", &[moving(1)], false, later(t0, 2));
    assert!(
        stale.is_none(),
        "an older listing must not rewind the baseline"
    );
    let again = observer
        .observe("acme/app", &[ended(1, "failure")], false, later(t0, 6))
        .unwrap();
    assert!(
        again.finished.is_empty(),
        "the finish was not announced twice"
    );
    assert!(observer.take_pending(10).is_empty());
}

#[test]
fn repositories_do_not_share_a_baseline() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    observer.observe("acme/app", &[moving(1)], false, t0);
    observer.observe("acme/other", &[ended(1, "failure")], false, later(t0, 1));
    assert!(observer.take_pending(10).is_empty());
}

#[test]
fn the_baselines_kept_are_bounded_and_the_stalest_repository_goes() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    for n in 0..(MAX_REPOSITORIES + 5) {
        observer.observe(&format!("o/r{n}"), &[moving(1)], false, later(t0, n as u64));
    }
    assert_eq!(observer.repositories.len(), MAX_REPOSITORIES);
    assert!(!observer.repositories.contains_key("o/r0"));
    assert!(observer
        .repositories
        .contains_key(&format!("o/r{}", MAX_REPOSITORIES + 4)));
}

#[test]
fn the_pending_queue_is_bounded_and_counts_what_it_dropped() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    let moving_runs: Vec<_> = (0..20).map(moving).collect();
    let settled: Vec<_> = (0..20).map(|id| ended(id, "failure")).collect();
    let mut at = 0;
    // Each repository is a separate baseline, so 20 finishes per round.
    for n in 0..(MAX_PENDING / 20 + 2) {
        let key = format!("o/r{n}");
        observer.observe(&key, &moving_runs, false, later(t0, at));
        at += 1;
        observer.observe(&key, &settled, false, later(t0, at));
        at += 1;
    }
    assert_eq!(observer.pending.len(), MAX_PENDING);
    assert!(
        observer.dropped() > 0,
        "a dropped CI result must be counted"
    );
}

#[test]
fn went_red_reports_only_failures_and_nothing_for_an_unchecked_report() {
    let t0 = Instant::now();
    let report = |runs: Vec<WorkflowRunInfo>, checked: bool| GitHubRunsReport {
        available: checked,
        checked,
        cli_present: true,
        owner: "acme".into(),
        repo: "went-red-fixture".into(),
        runs,
        truncated: false,
        error: None,
        went_red: Vec::new(),
    };
    assert!(went_red(
        &report(vec![moving(1), moving(2), moving(3), moving(4)], true),
        t0
    )
    .is_empty());
    let after = report(
        vec![
            ended(1, "failure"),
            ended(2, "cancelled"),
            ended(3, "action_required"),
            ended(4, "success"),
        ],
        true,
    );
    let red = went_red(&after, later(t0, 1));
    assert_eq!(red.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1]);
    // A poll that could not run learns nothing and moves nothing.
    assert!(went_red(&report(Vec::new(), false), later(t0, 2)).is_empty());
}

fn repo_report(repo: &str, runs: Vec<WorkflowRunInfo>) -> GitHubRunsReport {
    GitHubRunsReport {
        available: true,
        checked: true,
        cli_present: true,
        owner: "acme".into(),
        repo: repo.into(),
        runs,
        truncated: false,
        error: None,
        went_red: Vec::new(),
    }
}

/// The watcher's 30 s poll and the panel's live poll race. Whoever crosses
/// in-flight to failed first used to consume the finish, so the panel never
/// raised the banner for a failure the watcher saw first.
#[test]
fn a_red_finish_the_watcher_saw_first_is_still_the_panels_news_exactly_once() {
    let t0 = Instant::now();
    let key = "acme/panel-after-watcher";
    assert!(went_red(&repo_report("panel-after-watcher", vec![moving(1)]), t0).is_empty());
    with_observer(|observer| {
        observer.observe(key, &[moving(1)], false, later(t0, 1));
        observer.observe(key, &[ended(1, "failure")], false, later(t0, 2));
    });
    let red = went_red(
        &repo_report("panel-after-watcher", vec![ended(1, "failure")]),
        later(t0, 3),
    );
    assert_eq!(red.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1]);
    let again = went_red(
        &repo_report("panel-after-watcher", vec![ended(1, "failure")]),
        later(t0, 4),
    );
    assert!(again.is_empty(), "the panel is told once");
}

fn ids(runs: &[WorkflowRunInfo]) -> Vec<u64> {
    runs.iter().map(|run| run.id).collect()
}

#[test]
fn the_panel_and_the_watcher_each_get_a_finish_neither_using_up_the_others() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    assert!(observer
        .panel_look("acme/app", &[moving(1)], false, t0)
        .is_empty());
    observer.observe("acme/app", &[ended(1, "failure")], false, later(t0, 1));
    assert_eq!(
        observer.take_pending(10).len(),
        1,
        "the watcher drains its queue"
    );
    let news = observer.panel_look("acme/app", &[ended(1, "failure")], false, later(t0, 2));
    assert_eq!(
        ids(&news),
        vec![1],
        "and the panel's cursor is untouched by it"
    );
    assert!(observer
        .panel_look("acme/app", &[ended(1, "failure")], false, later(t0, 3))
        .is_empty());
    assert!(observer.take_pending(10).is_empty());
}

#[test]
fn the_panels_first_look_announces_nothing_even_when_the_watcher_saw_runs_move_before_it() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    observer.observe("acme/app", &[moving(1), moving(2)], false, t0);
    observer.observe(
        "acme/app",
        &[ended(1, "failure"), moving(2)],
        false,
        later(t0, 1),
    );
    // The panel opens later. The watcher saw both runs move, but the panel
    // never did: run 1 is history, and so is run 2 finishing in the very
    // listing the panel first looks at, because it was not watching before it.
    let first = observer.panel_look(
        "acme/app",
        &[ended(1, "failure"), ended(2, "failure")],
        false,
        later(t0, 2),
    );
    assert!(first.is_empty(), "{:?}", ids(&first));
    assert!(observer
        .panel_look(
            "acme/app",
            &[ended(1, "failure"), ended(2, "failure")],
            false,
            later(t0, 3)
        )
        .is_empty());
}

/// The replay the process-wide red log allowed: the panel polls only while its
/// own listing has something moving, so a red the watcher logged while the
/// panel was not polling used to be announced on the panel's next live poll.
#[test]
fn a_red_the_panel_never_saw_moving_is_not_announced_when_the_panel_polls_again() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    assert!(observer
        .panel_look("acme/app", &[moving(1)], false, t0)
        .is_empty());
    // Run 2 starts and fails while the panel is not polling; only the watcher
    // follows it.
    observer.observe("acme/app", &[moving(2)], false, later(t0, 1));
    observer.observe("acme/app", &[ended(2, "failure")], false, later(t0, 2));
    // Run 1 goes green; the panel polls again and looks at both.
    let news = observer.panel_look(
        "acme/app",
        &[ended(2, "failure"), ended(1, "success")],
        false,
        later(t0, 3),
    );
    assert!(
        news.is_empty(),
        "run 2 was never seen moving by the panel: {:?}",
        ids(&news)
    );
}

#[test]
fn the_panels_baseline_is_its_own_not_the_watchers() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    observer.panel_look("acme/app", &[moving(1)], false, t0);
    // The watcher saw run 1 settle red and drained it.
    observer.observe("acme/app", &[ended(1, "failure")], false, later(t0, 1));
    observer.take_pending(10);
    // The panel still remembers run 1 moving, so it is told, once.
    assert_eq!(
        ids(&observer.panel_look("acme/app", &[ended(1, "failure")], false, later(t0, 2))),
        vec![1]
    );
    assert!(observer
        .panel_look("acme/app", &[ended(1, "failure")], false, later(t0, 3))
        .is_empty());
}

#[test]
fn a_panel_baseline_older_than_the_live_poll_session_is_dropped() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    observer.panel_look("acme/app", &[moving(1)], false, t0);
    observer.observe("acme/app", &[ended(1, "failure")], false, later(t0, 1));
    // An hour on, the panel remounts and looks: what it saw moving then is not
    // what it is watching now.
    assert!(PANEL_SEEN_TTL < Duration::from_secs(3600));
    assert!(
        observer
            .panel_look("acme/app", &[ended(1, "failure")], false, later(t0, 3600))
            .is_empty(),
        "a run that was in flight an hour ago is history"
    );
    // That look re-baselined the panel: the next run it watches is news.
    observer.panel_look(
        "acme/app",
        &[moving(2), ended(1, "failure")],
        false,
        later(t0, 3601),
    );
    assert_eq!(
        ids(&observer.panel_look(
            "acme/app",
            &[ended(2, "failure"), ended(1, "failure")],
            false,
            later(t0, 3602)
        )),
        vec![2]
    );
}

#[test]
fn a_panel_baseline_exactly_at_the_session_ceiling_is_still_believed() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    observer.panel_look("acme/app", &[moving(1)], false, t0);
    let news = observer.panel_look(
        "acme/app",
        &[ended(1, "failure")],
        false,
        t0 + PANEL_SEEN_TTL,
    );
    assert_eq!(ids(&news), vec![1]);
}

/// The ceiling is the renderer's `DEFAULT_POLL_LIMITS.maxDurationMs`. There is
/// no constant to share across the language boundary, so this reads it.
#[test]
fn the_panel_baseline_ceiling_is_the_live_poll_session_ceiling() {
    let schedule = include_str!("../../../src/lib/delivery/pollSchedule.ts");
    let line = schedule
        .lines()
        .find(|line| line.trim_start().starts_with("maxDurationMs: 15"))
        .expect("pollSchedule.ts no longer declares a 15 minute default maxDurationMs");
    assert_eq!(
        line.trim(),
        "maxDurationMs: 15 * 60_000,",
        "update PANEL_SEEN_TTL in outcome.rs with it"
    );
    assert_eq!(PANEL_SEEN_TTL, Duration::from_secs(15 * 60));
}

#[test]
fn only_a_red_finish_is_news_to_the_panel_and_it_is_per_repository() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    observer.panel_look("acme/app", &[moving(1), moving(2)], false, t0);
    observer.panel_look("acme/other", &[moving(1)], false, t0);
    observer.observe(
        "acme/app",
        &[ended(1, "cancelled"), ended(2, "success")],
        false,
        later(t0, 1),
    );
    observer.observe("acme/other", &[ended(1, "failure")], false, later(t0, 1));
    assert!(observer
        .panel_look(
            "acme/app",
            &[ended(1, "cancelled"), ended(2, "success")],
            false,
            later(t0, 2)
        )
        .is_empty());
    assert_eq!(
        ids(&observer.panel_look("acme/other", &[ended(1, "failure")], false, later(t0, 2))),
        vec![1]
    );
}

/// The panel's baseline is one listing, so a flood of finishes cannot grow it,
/// and every red the panel saw move is still its news.
#[test]
fn every_red_the_panel_saw_moving_is_news_however_many_there_are() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    let total = 40_u64;
    let moving_runs: Vec<_> = (1..=total).map(moving).collect();
    let red_runs: Vec<_> = (1..=total).map(|id| ended(id, "failure")).collect();
    observer.panel_look("acme/app", &moving_runs, false, t0);
    observer.observe("acme/app", &red_runs, false, later(t0, 1));
    let news = observer.panel_look("acme/app", &red_runs, false, later(t0, 2));
    assert_eq!(news.len() as u64, total);
}

/// A slow panel request made before the watcher saw the finish lands late. It
/// is ignored like any stale listing: it delivers nothing and leaves the
/// panel's baseline alone, so the next fresh listing still finds the run the
/// panel was waiting on.
#[test]
fn a_stale_listing_delivers_nothing_and_leaves_the_panels_baseline_alone() {
    let mut observer = Observer::default();
    let t0 = Instant::now();
    observer.panel_look("acme/app", &[moving(1)], false, t0);
    observer.observe("acme/app", &[ended(1, "failure")], false, later(t0, 5));
    let stale = observer.panel_look("acme/app", &[moving(1)], false, later(t0, 2));
    assert!(stale.is_empty());
    let news = observer.panel_look("acme/app", &[ended(1, "failure")], false, later(t0, 6));
    assert_eq!(ids(&news), vec![1], "told once, by the next fresh listing");
}
