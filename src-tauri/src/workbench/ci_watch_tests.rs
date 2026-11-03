//! The CI watcher against a real store and a scripted GitHub: what it watches,
//! what it refuses to ask, what it records, and that a recorded failure is a
//! notice the native coordinator would post.

use super::*;
use crate::github::outcome::{self, Observer};
use crate::github::{GitHubRunsReport, WorkflowRunInfo};
use std::cell::{Cell, RefCell};

const BRANCH: &str = "gitpulse/fix-1234abcd";
const FAILED_TITLE: &str = "Coding agent branch failed CI";

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn ci_run(id: u64, status: &str, conclusion: &str, branch: &str) -> WorkflowRunInfo {
    WorkflowRunInfo {
        id,
        name: "CI".into(),
        title: format!("run {id}"),
        status: status.into(),
        conclusion: conclusion.into(),
        head_branch: branch.into(),
        url: format!("https://github.com/acme/app/actions/runs/{id}"),
        created_at: String::new(),
        started_at: String::new(),
        updated_at: String::new(),
        head_sha: sha('b'),
        event: "push".into(),
        outcome: crate::github::outcome::classify(status, conclusion),
    }
}

fn moving(id: u64) -> WorkflowRunInfo {
    ci_run(id, "in_progress", "", BRANCH)
}

fn ended(id: u64, conclusion: &str) -> WorkflowRunInfo {
    ci_run(id, "completed", conclusion, BRANCH)
}

fn listing(runs: Vec<WorkflowRunInfo>) -> GitHubRunsReport {
    GitHubRunsReport {
        available: true,
        checked: true,
        cli_present: true,
        owner: "acme".into(),
        repo: "app".into(),
        runs,
        truncated: false,
        error: None,
        went_red: Vec::new(),
    }
}

fn unreadable(why: &str) -> GitHubRunsReport {
    GitHubRunsReport {
        available: false,
        checked: false,
        cli_present: false,
        owner: "acme".into(),
        repo: "app".into(),
        runs: Vec::new(),
        truncated: false,
        error: Some(why.into()),
        went_red: Vec::new(),
    }
}

/// GitHub as the watcher sees it: a script of listings, the shared baseline
/// the real loader would fold each into, and a clock the test moves.
struct Fake {
    trusted: RefCell<Result<bool, String>>,
    /// What git says the checkout's remote is, and how often it was asked.
    remote: RefCell<Result<Option<String>, String>>,
    remote_asked: Cell<usize>,
    script: RefCell<VecDeque<GitHubRunsReport>>,
    last: RefCell<Option<GitHubRunsReport>>,
    observer: RefCell<Observer>,
    /// Finishes handed over again, as two pollers could.
    replay: RefCell<Vec<Finished>>,
    asked: Cell<usize>,
    wakes: Cell<usize>,
    clock: Cell<Duration>,
    epoch: Instant,
    cancelled: Cell<bool>,
}

impl Fake {
    fn new() -> Self {
        Self {
            trusted: RefCell::new(Ok(true)),
            remote: RefCell::new(Ok(Some(outcome::repository_key("acme", "app")))),
            remote_asked: Cell::new(0),
            script: RefCell::new(VecDeque::new()),
            last: RefCell::new(None),
            observer: RefCell::new(Observer::default()),
            replay: RefCell::new(Vec::new()),
            asked: Cell::new(0),
            wakes: Cell::new(0),
            clock: Cell::new(Duration::ZERO),
            epoch: Instant::now(),
            cancelled: Cell::new(false),
        }
    }
    fn say(&self, report: GitHubRunsReport) {
        self.script.borrow_mut().push_back(report);
    }
    fn advance(&self, by: Duration) {
        self.clock.set(self.clock.get() + by);
    }
}

impl Backend for Fake {
    fn trusted(&self, _: &str) -> Result<bool, String> {
        self.trusted.borrow().clone()
    }
    fn remote(&self, _: &str) -> Result<Option<String>, String> {
        self.remote_asked.set(self.remote_asked.get() + 1);
        self.remote.borrow().clone()
    }
    fn runs(&self, _: &str) -> GitHubRunsReport {
        self.asked.set(self.asked.get() + 1);
        let report = match self.script.borrow_mut().pop_front() {
            Some(next) => next,
            None => self.last.borrow().clone().expect("a listing was scripted"),
        };
        if report.checked {
            self.observer.borrow_mut().observe(
                &outcome::repository_key(&report.owner, &report.repo),
                &report.runs,
                report.truncated,
                self.now(),
            );
        }
        *self.last.borrow_mut() = Some(report.clone());
        report
    }
    fn take_finished(&self, max: usize) -> Vec<Finished> {
        let mut all = self.observer.borrow_mut().take_pending(max);
        all.extend(self.replay.borrow_mut().drain(..));
        all
    }
    fn dropped(&self) -> u64 {
        self.observer.borrow().dropped()
    }
    fn wake(&self) {
        self.wakes.set(self.wakes.get() + 1);
    }
    fn now(&self) -> Instant {
        self.epoch + self.clock.get()
    }
    fn unix_now(&self) -> i64 {
        intake::now_millis() / 1000
    }
    fn cancelled(&self) -> bool {
        self.cancelled.get()
    }
}

struct World {
    _profile: tempfile::TempDir,
    store: Store,
}

fn world() -> World {
    let profile = tempfile::tempdir().unwrap();
    let store = Store::open(profile.path().join("workbench.sqlite")).unwrap();
    query(
        &store,
        "notifications.settings.put",
        &json!({"id":"profile","request_id":"on","expected_revision":1,"enabled":true}).to_string(),
    )
    .unwrap();
    World {
        _profile: profile,
        store,
    }
}

impl World {
    /// A repository with one task and one attempt prepared on `head_ref`.
    /// Returns the checkout so it stays alive for the test.
    fn attempt(&self, key: &str, head_ref: Option<&str>) -> tempfile::TempDir {
        let repo = crate::test_support::git_repo();
        let local = intake::resolve(repo.path().to_str().unwrap()).unwrap();
        let repository = intake::register(
            &self.store,
            &local,
            &format!("repo-{key}"),
            &format!("register-{key}"),
        )
        .unwrap()["repository"]
            .clone();
        let repository_id = repository["id"].as_str().unwrap().to_owned();
        query(
            &self.store,
            "items.put",
            &json!({"id":format!("task-{key}"),"request_id":format!("task-{key}"),"expected_revision":0,"title":format!("Task {key}"),"repository_ids":[repository_id],"primary_repository_id":repository_id}).to_string(),
        )
        .unwrap();
        let root = repo.path().canonicalize().unwrap();
        let git = root.join(".git");
        let mut input = json!({
            "id": format!("run-{key}"), "request_id": format!("prepare-{key}"), "expected_revision": 0,
            "task_id": format!("task-{key}"), "source_revision": 1, "repository_id": repository_id,
            "repository_revision": repository["revision"], "provider": "claude",
            "permission_mode": "ask", "cwd": root, "git_dir": git, "git_common_dir": git,
            "head_oid": sha('c'), "max_active_runs": 64,
        });
        if let Some(head_ref) = head_ref {
            input["head_ref"] = json!(format!("refs/heads/{head_ref}"));
        }
        query(&self.store, "runs.prepare", &input.to_string()).unwrap();
        repo
    }

    fn logs(&self, key: &str) -> String {
        intake::get_item(&self.store, &format!("task-{key}"))
            .unwrap()
            .unwrap()["logs"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn inbox(&self) -> Vec<Value> {
        query(
            &self.store,
            "attention.list",
            &json!({"filter":"all","limit":50}).to_string(),
        )
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn native_titles(&self) -> Vec<String> {
        query(
            &self.store,
            "notifications.pending.list",
            &json!({"minute_of_day":720,"limit":3}).to_string(),
        )
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["title"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// A sweep, then enough time for the repository to be due again, idle or not.
fn sweep(watcher: &mut Watcher, world: &World, fake: &Fake) -> Report {
    let report = watcher.sweep(&world.store, fake);
    fake.advance(IDLE_POLL + Duration::from_secs(1));
    report
}

// --- what is watched --------------------------------------------------------

fn run_row(extra: Value) -> Value {
    let mut row = json!({
        "id": "r1", "task_id": "t1", "state": "running", "head_ref": "refs/heads/gitpulse/fix-1234abcd",
        "head_oid": sha('c'), "git_common_dir": "/work/app/.git", "cwd": "/work/app/.gitpulse/worktrees/fix-1234abcd",
        "expires_at": 0, "finished_at": null, "updated_at": 0,
    });
    for (key, value) in extra.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}

#[test]
fn a_live_attempt_branch_is_watched_from_the_repositorys_main_checkout() {
    let found = candidate_of(&run_row(json!({})), 1_000).unwrap();
    assert!(found.attempt);
    assert_eq!(found.branch.as_deref(), Some(BRANCH));
    assert_eq!(
        found.checkout, "/work/app",
        "the attempt's worktree may be gone when CI finishes; the main checkout is what is asked"
    );
}

#[test]
fn a_run_stays_watched_for_the_window_after_it_ended_and_not_beyond() {
    let ended_at = |at: i64| run_row(json!({"state":"exited","finished_at":at}));
    let now = 100_000;
    assert!(candidate_of(&ended_at(now - FINISHED_WINDOW_SECS), now).is_some());
    assert!(candidate_of(&ended_at(now - FINISHED_WINDOW_SECS - 1), now).is_none());
}

#[test]
fn a_prepared_run_is_live_only_until_it_expires() {
    let prepared =
        |expires: i64| run_row(json!({"state":"prepared","expires_at":expires,"updated_at":0}));
    assert!(candidate_of(&prepared(1_001), 1_000).is_some());
    assert!(candidate_of(&prepared(999), 1_000).is_none());
}

#[test]
fn a_run_on_another_branch_is_a_candidate_but_not_an_attempt() {
    let main = candidate_of(&run_row(json!({"head_ref":"refs/heads/main"})), 1_000).unwrap();
    assert!(!main.attempt);
    let detached = candidate_of(&run_row(json!({"head_ref":null})), 1_000).unwrap();
    assert!(!detached.attempt);
    assert_eq!(detached.branch, None);
}

#[test]
fn a_finished_ci_run_is_attributed_only_by_the_attempt_branch_it_ran_on() {
    let attempt = candidate_of(&run_row(json!({})), 1_000).unwrap();
    let mut other = candidate_of(
        &run_row(json!({"id":"r2","head_ref":"refs/heads/main","head_oid":sha('d')})),
        1_000,
    )
    .unwrap();
    other.attempt = false;
    let candidates = [attempt.clone(), other.clone()];

    let by_branch = ended(1, "failure");
    assert_eq!(matching(&candidates, &by_branch).unwrap().run_id, "r1");

    // `head_oid` is the commit an attempt STARTED from, not a tip it pushed, so
    // CI on that commit is the base branch's CI. It is nobody's result here,
    // whatever branch GitHub names for it, and even when it names none.
    for (id, branch, start) in [
        (2, "somebody-elses-branch", 'c'),
        (3, "main", 'c'),
        (4, "", 'c'),
        (5, "main", 'd'),
    ] {
        let mut on_base = ci_run(id, "completed", "failure", branch);
        on_base.head_sha = sha(start);
        assert!(
            matching(&candidates, &on_base).is_none(),
            "a run on {branch:?} at a starting commit is not an attempt's result"
        );
    }
    assert!(matching(&candidates, &ci_run(6, "completed", "failure", "")).is_none());
}

/// The sequence the by-commit rule got wrong: main is pushed, an agent is
/// launched at main's HEAD while main's CI runs, and main's CI goes red.
#[test]
fn main_ci_on_the_commit_an_attempt_started_from_is_not_reported_as_the_agents_failure() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut watcher = Watcher::default();
    let on_base = |status: &str, conclusion: &str| {
        let mut run = ci_run(31, status, conclusion, "main");
        run.head_sha = sha('c');
        run
    };
    fake.say(listing(vec![on_base("in_progress", "")]));
    sweep(&mut watcher, &world, &fake);
    fake.say(listing(vec![on_base("completed", "failure")]));
    let report = sweep(&mut watcher, &world, &fake);
    assert_eq!((report.recorded, report.unmatched), (0, 1), "{report:?}");
    assert!(world.inbox().is_empty());
    assert!(world.native_titles().is_empty());
    assert!(world.logs("a").is_empty());
}

#[test]
fn a_signal_never_names_a_branch_the_run_was_not_on() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let candidate =
        candidate_of(&run_row(json!({"id":"run-a","task_id":"task-a"})), 1_000).unwrap();

    let elsewhere = ci_run(41, "completed", "failure", "main");
    let refused = record(&world.store, &candidate, &elsewhere).unwrap();
    assert!(matches!(refused, Recorded::Unrecordable(_)), "{refused:?}");
    let unnamed = ci_run(42, "completed", "failure", "");
    let refused = record(&world.store, &candidate, &unnamed).unwrap();
    assert!(matches!(refused, Recorded::Unrecordable(_)), "{refused:?}");
    assert!(world.inbox().is_empty());
    assert!(world.logs("a").is_empty());

    let on_branch = ended(43, "failure");
    assert_eq!(
        record(&world.store, &candidate, &on_branch).unwrap(),
        Recorded::New
    );
    assert_eq!(world.inbox()[0]["signal"]["branch"], BRANCH);
}

#[test]
fn a_failing_repository_backs_off_to_a_ceiling_and_never_below_the_idle_poll() {
    let waits: Vec<_> = (1..=12).map(backoff).collect();
    assert!(waits.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(waits
        .iter()
        .all(|wait| *wait >= IDLE_POLL && *wait <= BACKOFF_MAX));
    assert_eq!(*waits.last().unwrap(), BACKOFF_MAX);
}

// --- the loop ---------------------------------------------------------------

#[test]
fn a_ci_failure_on_a_live_attempt_reaches_the_inbox_the_task_log_and_the_native_coordinator() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut watcher = Watcher::default();

    fake.say(listing(vec![moving(77)]));
    let first = sweep(&mut watcher, &world, &fake);
    assert_eq!((first.polled, first.recorded), (1, 0), "{first:?}");
    assert!(world.inbox().is_empty(), "a run in flight is not news");

    fake.say(listing(vec![ended(77, "failure")]));
    let second = sweep(&mut watcher, &world, &fake);
    assert_eq!(second.recorded, 1, "{second:?}");
    assert_eq!(fake.wakes.get(), 1, "the coordinator is told to look now");

    let inbox = world.inbox();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["kind"], "run_ci_failed");
    assert_eq!(inbox[0]["target_status"], "current");
    assert_eq!(inbox[0]["task_id"], "task-a");
    assert_eq!(inbox[0]["signal"]["branch"], BRANCH);
    assert_eq!(inbox[0]["signal"]["conclusion"], "failure");
    assert_eq!(inbox[0]["signal"]["external_id"], "77");

    // The row the native coordinator posts from: pending, by the store's own rule.
    assert_eq!(world.native_titles(), vec![FAILED_TITLE.to_owned()]);

    let logs = world.logs("a");
    for expected in [
        "--- CI failed on gitpulse/fix-1234abcd (",
        "workflow: CI",
        "conclusion: failure",
        "run: https://github.com/acme/app/actions/runs/77",
        &format!("head: {}", sha('b')),
    ] {
        assert!(logs.contains(expected), "{expected:?} missing from {logs}");
    }
}

#[test]
fn a_run_already_finished_when_first_seen_is_a_baseline_and_says_nothing() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut watcher = Watcher::default();
    fake.say(listing(vec![ended(5, "failure")]));
    let first = sweep(&mut watcher, &world, &fake);
    assert_eq!((first.polled, first.recorded), (1, 0));
    let again = sweep(&mut watcher, &world, &fake);
    assert_eq!(again.recorded, 0);
    assert!(world.inbox().is_empty());
    assert!(world.native_titles().is_empty());
    assert!(world.logs("a").is_empty());
}

#[test]
fn cancelled_and_passed_are_their_own_kinds_and_leave_no_failure_block() {
    for (conclusion, kind, title) in [
        (
            "cancelled",
            "run_ci_cancelled",
            "Coding agent branch CI was cancelled",
        ),
        ("success", "run_ci_passed", "Coding agent branch passed CI"),
    ] {
        let world = world();
        let _repo = world.attempt("a", Some(BRANCH));
        let fake = Fake::new();
        let mut watcher = Watcher::default();
        fake.say(listing(vec![moving(9)]));
        sweep(&mut watcher, &world, &fake);
        fake.say(listing(vec![ended(9, conclusion)]));
        assert_eq!(sweep(&mut watcher, &world, &fake).recorded, 1);
        assert_eq!(world.inbox()[0]["kind"], kind);
        assert_eq!(world.native_titles(), vec![title.to_owned()]);
        assert!(
            world.logs("a").is_empty(),
            "only a failure writes a log block"
        );
    }
}

#[test]
fn a_run_held_for_a_person_is_not_a_ci_signal() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut watcher = Watcher::default();
    fake.say(listing(vec![moving(9)]));
    sweep(&mut watcher, &world, &fake);
    fake.say(listing(vec![ended(9, "action_required")]));
    assert_eq!(sweep(&mut watcher, &world, &fake).recorded, 0);
    assert!(world.inbox().is_empty());
}

#[test]
fn a_repoll_that_hands_the_same_finish_again_records_and_logs_nothing_more() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut watcher = Watcher::default();
    fake.say(listing(vec![moving(77)]));
    sweep(&mut watcher, &world, &fake);
    fake.say(listing(vec![ended(77, "failure")]));
    assert_eq!(sweep(&mut watcher, &world, &fake).recorded, 1);
    let blocks = world.logs("a");

    // The panel's poll and this one both saw the finish; the second delivery
    // reaches the store with a new request id and is a repeat.
    fake.replay.borrow_mut().push(Finished {
        repository: "acme/app".into(),
        run: ended(77, "failure"),
    });
    let repeat = sweep(&mut watcher, &world, &fake);
    assert_eq!(
        (repeat.recorded, repeat.already_recorded),
        (0, 1),
        "{repeat:?}"
    );
    assert_eq!(world.inbox().len(), 1);
    assert_eq!(world.native_titles().len(), 1);
    assert_eq!(world.logs("a"), blocks, "no second log block");
    assert_eq!(fake.wakes.get(), 1);
}

#[test]
fn a_newer_result_on_the_same_branch_replaces_the_older_notice() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut watcher = Watcher::default();
    fake.say(listing(vec![moving(1)]));
    sweep(&mut watcher, &world, &fake);
    fake.say(listing(vec![ended(1, "failure")]));
    sweep(&mut watcher, &world, &fake);
    fake.say(listing(vec![moving(2), ended(1, "failure")]));
    sweep(&mut watcher, &world, &fake);
    fake.say(listing(vec![ended(2, "success"), ended(1, "failure")]));
    let last = sweep(&mut watcher, &world, &fake);
    assert_eq!(last.recorded, 1, "{last:?} {:?}", world.inbox());
    assert_eq!(
        world.native_titles(),
        vec!["Coding agent branch passed CI".to_owned()],
        "the earlier failure is superseded and no longer pending"
    );
}

/// The Remote panel's poll and the watcher's share one baseline, so whichever
/// looks first consumes the finish. The watcher must still record it, even for
/// a repository it has not itself asked yet (a new attempt branch, or more
/// repositories due than one sweep asks).
#[test]
fn a_finish_the_panel_saw_first_is_recorded_before_the_watcher_has_asked_that_repository() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    fake.say(listing(vec![moving(77)]));
    fake.runs("the panel's own poll");
    fake.advance(ACTIVE_POLL);
    fake.say(listing(vec![ended(77, "failure")]));
    fake.runs("the panel's own poll");
    fake.advance(ACTIVE_POLL);

    let mut watcher = Watcher::default();
    for _ in 0..MAX_CALLS {
        watcher.calls.push_back(fake.now());
    }
    let report = watcher.sweep(&world.store, &fake);
    assert_eq!(
        report.polled, 0,
        "the watcher asked nothing itself: {report:?}"
    );
    assert_eq!(report.recorded, 1, "{report:?}");
    assert_eq!(world.inbox()[0]["kind"], "run_ci_failed");
    assert_eq!(world.native_titles(), vec![FAILED_TITLE.to_owned()]);
}

/// The reverse order: the watcher's poll crosses in-flight to failed, and the
/// Remote panel looks afterwards. The panel's banner is its own news, not
/// whatever the watcher left over.
#[test]
fn the_panel_still_hears_of_a_red_finish_the_watcher_saw_first() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut watcher = Watcher::default();
    let key = outcome::repository_key("acme", "app");
    let panel = |runs: Vec<WorkflowRunInfo>, fake: &Fake| {
        fake.observer
            .borrow_mut()
            .panel_look(&key, &runs, false, fake.now())
            .iter()
            .map(|run| run.id)
            .collect::<Vec<_>>()
    };
    assert!(panel(vec![moving(77)], &fake).is_empty());
    fake.advance(Duration::from_secs(1));

    fake.say(listing(vec![moving(77)]));
    sweep(&mut watcher, &world, &fake);
    fake.say(listing(vec![ended(77, "failure")]));
    assert_eq!(sweep(&mut watcher, &world, &fake).recorded, 1);
    assert_eq!(world.inbox()[0]["kind"], "run_ci_failed");

    assert_eq!(
        panel(vec![ended(77, "failure")], &fake),
        vec![77],
        "the watcher recorded it, and the panel is told too"
    );
    assert!(panel(vec![ended(77, "failure")], &fake).is_empty());
}

/// A repository has no GitHub remote: there is nothing to read, which is a
/// fact about the repository and not a fault. Finding that out costs a `git`
/// process, so it is not repeated every sweep.
#[test]
fn a_repository_with_no_github_remote_is_its_own_state_and_is_looked_up_rarely() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    *fake.remote.borrow_mut() = Ok(None);
    let mut watcher = Watcher::default();
    for _ in 0..10 {
        watcher.sweep(&world.store, &fake);
        fake.advance(Duration::from_secs(5));
    }
    assert_eq!(
        fake.remote_asked.get(),
        1,
        "ten sweeps in under an idle period read the remote once"
    );
    assert_eq!(fake.asked.get(), 0, "there is no GitHub to ask");
    let status = watcher.repos.values().next().unwrap().status.clone();
    assert_eq!(status.state, WatchState::NoRemote);
    assert!(
        status.reason.contains("no GitHub remote"),
        "{}",
        status.reason
    );
    assert_eq!(watcher.fault(), None, "not being on GitHub is not a fault");

    fake.advance(IDLE_POLL);
    watcher.sweep(&world.store, &fake);
    assert_eq!(
        fake.remote_asked.get(),
        2,
        "and again after the idle period"
    );

    // The remote appears: it is found at the next look and the repository is asked.
    *fake.remote.borrow_mut() = Ok(Some(outcome::repository_key("acme", "app")));
    fake.say(listing(vec![moving(1)]));
    fake.advance(IDLE_POLL + Duration::from_secs(1));
    assert_eq!(watcher.sweep(&world.store, &fake).polled, 1);
    assert_eq!(
        watcher.repos.values().next().unwrap().status.state,
        WatchState::Watching
    );
}

#[test]
fn a_remote_that_cannot_be_read_is_not_re_read_every_sweep_and_does_not_stop_the_ask() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    *fake.remote.borrow_mut() = Err("git is busy".into());
    // A listing that names no repository, so nothing else fills the remote in.
    let mut nameless = unreadable("gh failed");
    nameless.owner = String::new();
    nameless.repo = String::new();
    fake.say(nameless);
    let mut watcher = Watcher::default();
    for _ in 0..6 {
        watcher.sweep(&world.store, &fake);
        fake.advance(Duration::from_secs(5));
    }
    assert_eq!(fake.remote_asked.get(), 1);
    assert_eq!(
        fake.asked.get(),
        1,
        "the listing is still asked, and says more"
    );
    assert_eq!(
        watcher.repos.values().next().unwrap().status.state,
        WatchState::Unavailable
    );
}

#[test]
fn the_remote_of_an_untrusted_checkout_is_not_read_from_git() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    *fake.trusted.borrow_mut() = Ok(false);
    let mut watcher = Watcher::default();
    watcher.sweep(&world.store, &fake);
    assert_eq!(fake.remote_asked.get(), 0);
}

#[test]
fn a_finish_in_a_repository_that_is_not_trusted_is_never_recorded() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    *fake.trusted.borrow_mut() = Ok(false);
    fake.say(listing(vec![moving(77)]));
    fake.runs("the panel's own poll");
    fake.advance(ACTIVE_POLL);
    fake.say(listing(vec![ended(77, "failure")]));
    fake.runs("the panel's own poll");
    fake.advance(ACTIVE_POLL);
    let mut watcher = Watcher::default();
    let report = watcher.sweep(&world.store, &fake);
    assert_eq!((report.polled, report.recorded), (0, 0), "{report:?}");
    assert!(world.inbox().is_empty());
    assert!(world.logs("a").is_empty());
}

/// The run table is read on a slower beat than the sweep: it is up to 600
/// run bodies, and nothing it holds changes second by second.
#[test]
fn the_watch_set_is_refreshed_on_a_slower_beat_than_the_sweep() {
    let world = world();
    let _first = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    fake.say(listing(vec![moving(1)]));
    let mut watcher = Watcher::default();
    watcher.sweep(&world.store, &fake);
    assert_eq!(watcher.repos.len(), 1);

    let _second = world.attempt("b", Some("gitpulse/fix-87654321"));
    fake.advance(Duration::from_secs(5));
    watcher.sweep(&world.store, &fake);
    assert_eq!(
        watcher.repos.len(),
        1,
        "a sweep five seconds on does not re-read the run table"
    );

    fake.advance(Duration::from_secs(31));
    watcher.sweep(&world.store, &fake);
    assert_eq!(
        watcher.repos.len(),
        2,
        "the next refresh finds the new attempt"
    );
}

#[test]
fn an_untrusted_repository_is_never_asked_and_says_why() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    *fake.trusted.borrow_mut() = Ok(false);
    fake.say(listing(vec![moving(1)]));
    let mut watcher = Watcher::default();
    let report = sweep(&mut watcher, &world, &fake);
    assert_eq!(report.polled, 0);
    assert_eq!(
        fake.asked.get(),
        0,
        "GitHub is not asked about an untrusted repository"
    );
    let state = &watcher.repos.values().next().unwrap().status;
    assert_eq!(state.state, WatchState::Untrusted);
    assert!(state.reason.contains("not trusted"), "{}", state.reason);
    assert_eq!(
        watcher.fault(),
        None,
        "declining to trust a repository is a choice, not a fault for Diagnostics"
    );
}

#[test]
fn trust_that_cannot_be_read_is_not_reported_as_untrusted_or_as_gh_trouble() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    *fake.trusted.borrow_mut() = Err("the trust store is unreadable".into());
    let mut watcher = Watcher::default();
    sweep(&mut watcher, &world, &fake);
    assert_eq!(fake.asked.get(), 0);
    let state = &watcher.repos.values().next().unwrap().status;
    assert_eq!(state.state, WatchState::TrustUnknown);
    assert!(state.reason.contains("unreadable"));
}

#[test]
fn gh_that_is_unavailable_is_reported_and_retried_later_not_read_as_no_ci() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    fake.say(unreadable("gh is not authenticated; run `gh auth login`"));
    let mut watcher = Watcher::default();
    let first = watcher.sweep(&world.store, &fake);
    assert_eq!(first.polled, 1);
    let status = watcher.repos.values().next().unwrap().status.clone();
    assert_eq!(status.state, WatchState::Unavailable);
    assert!(
        status.reason.contains("not authenticated"),
        "{}",
        status.reason
    );
    assert!(
        watcher
            .fault()
            .is_some_and(|fault| fault.contains("not authenticated")),
        "Diagnostics is told, with the reason"
    );
    assert!(world.inbox().is_empty());

    // Inside the back-off it is not asked again; after it, it is.
    fake.advance(ACTIVE_POLL + Duration::from_secs(1));
    assert_eq!(watcher.sweep(&world.store, &fake).polled, 0);
    fake.advance(backoff(1));
    fake.say(listing(vec![moving(1)]));
    assert_eq!(watcher.sweep(&world.store, &fake).polled, 1);
    assert_eq!(
        watcher.repos.values().next().unwrap().status.state,
        WatchState::Watching
    );
    assert_eq!(
        watcher.fault(),
        None,
        "the fault clears once GitHub answers"
    );
}

#[test]
fn a_capped_listing_is_reported_as_capped() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut capped = listing(vec![moving(1)]);
    capped.truncated = true;
    fake.say(capped);
    let mut watcher = Watcher::default();
    sweep(&mut watcher, &world, &fake);
    assert!(watcher.repos.values().next().unwrap().status.capped);
}

/// `capped` was set and nothing read it, so a repository whose listing outgrew
/// the page was only visible to a debugger. It is a warning when it turns on,
/// once, and not on every poll while it stays on.
#[test]
fn a_listing_that_turns_capped_is_reported_once_when_it_does() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut capped = listing(vec![moving(1)]);
    capped.truncated = true;
    let whole = listing(vec![moving(1)]);
    let mut watcher = Watcher::default();

    fake.say(whole.clone());
    assert_eq!(sweep(&mut watcher, &world, &fake).newly_capped, 0);
    fake.say(capped.clone());
    assert_eq!(
        sweep(&mut watcher, &world, &fake).newly_capped,
        1,
        "complete, then capped, is the transition"
    );
    fake.say(capped.clone());
    assert_eq!(
        sweep(&mut watcher, &world, &fake).newly_capped,
        0,
        "staying capped is not announced again"
    );
    fake.say(whole);
    assert_eq!(sweep(&mut watcher, &world, &fake).newly_capped, 0);
    assert!(!watcher.repos.values().next().unwrap().status.capped);
    fake.say(capped);
    assert_eq!(
        sweep(&mut watcher, &world, &fake).newly_capped,
        1,
        "capped again after a complete listing is a new transition"
    );
}

#[test]
fn a_repository_with_no_attempt_branch_is_never_asked() {
    let world = world();
    let _repo = world.attempt("a", Some("main"));
    let fake = Fake::new();
    fake.say(listing(vec![moving(1)]));
    let mut watcher = Watcher::default();
    assert_eq!(sweep(&mut watcher, &world, &fake).polled, 0);
    assert_eq!(fake.asked.get(), 0);
}

#[test]
fn only_a_bounded_number_of_repositories_are_asked_per_sweep() {
    let world = world();
    let repos: Vec<_> = (0..POLLS_PER_SWEEP + 2)
        .map(|n| world.attempt(&format!("r{n}"), Some(&format!("gitpulse/fix-{n:08}"))))
        .collect();
    let fake = Fake::new();
    fake.say(listing(vec![moving(1)]));
    let mut watcher = Watcher::default();
    assert_eq!(watcher.sweep(&world.store, &fake).polled, POLLS_PER_SWEEP);
    assert_eq!(fake.asked.get(), POLLS_PER_SWEEP);
    drop(repos);
}

#[test]
fn the_call_budget_caps_gh_calls_within_a_window_and_is_reported_as_throttled() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    fake.say(listing(vec![moving(1)]));
    let mut watcher = Watcher::default();
    for _ in 0..MAX_CALLS {
        watcher.calls.push_back(fake.now());
    }
    let report = watcher.sweep(&world.store, &fake);
    assert_eq!(report.polled, 0);
    assert_eq!(fake.asked.get(), 0);
    assert_eq!(
        watcher.repos.values().next().unwrap().status.state,
        WatchState::Throttled
    );
    // Once the window has passed the budget is whole again.
    fake.advance(CALL_WINDOW + Duration::from_secs(1));
    assert_eq!(watcher.sweep(&world.store, &fake).polled, 1);
    assert!(watcher.calls.len() <= MAX_CALLS);
}

#[test]
fn a_host_that_is_shutting_down_stops_the_sweep_before_it_asks_or_writes() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    fake.cancelled.set(true);
    let mut watcher = Watcher::default();
    assert_eq!(watcher.sweep(&world.store, &fake), Report::default());
    assert_eq!(fake.asked.get(), 0);
}

#[test]
fn a_result_for_a_deleted_task_is_retried_a_bounded_number_of_times_then_given_up() {
    let world = world();
    let _repo = world.attempt("a", Some(BRANCH));
    let fake = Fake::new();
    let mut watcher = Watcher::default();
    fake.say(listing(vec![moving(1)]));
    sweep(&mut watcher, &world, &fake);
    let item = intake::get_item(&world.store, "task-a").unwrap().unwrap();
    query(
        &world.store,
        "items.delete",
        &json!({"id":"task-a","request_id":"gone","expected_revision":item["revision"],"even_if_running":true}).to_string(),
    )
    .unwrap();
    fake.say(listing(vec![ended(1, "failure")]));
    let mut failed = 0;
    for _ in 0..(MAX_ATTEMPTS as usize + 2) {
        failed += sweep(&mut watcher, &world, &fake).failed;
    }
    assert_eq!(failed, 1, "given up once, not retried forever");
    assert!(watcher.retry.is_empty());
}

#[test]
fn the_failure_block_names_the_run_and_never_carries_output_or_control_characters() {
    let mut run = ended(5, "failure");
    run.name = "CI\nINJECTED: yes".into();
    let block = failure_block("gitpulse/x\r", &run.name, &run, 0);
    assert_eq!(
        block.matches('\n').count(),
        5,
        "six lines, no injected ones: {block:?}"
    );
    assert!(block.contains("workflow: CIINJECTED: yes"));
    assert!(!block.contains('\r'));
}

// --- CI:local ---------------------------------------------------------------

fn host_of(world: &World) -> WorkbenchState {
    WorkbenchState::for_profile(&world._profile.path().join("workbench.sqlite"))
}

#[test]
fn a_checkout_is_an_attempts_worktree_when_a_run_began_there_on_this_branch() {
    let world = world();
    let repo = world.attempt("a", Some(BRANCH));
    let other = world.attempt("b", Some("gitpulse/fix-87654321"));
    let found = attempt_in(&world.store, repo.path(), BRANCH)
        .unwrap()
        .unwrap();
    assert_eq!(
        found,
        Attempt {
            task_id: "task-a".into(),
            run_id: "run-a".into(),
            branch: BRANCH.into()
        }
    );
    // The branch must be the one the run recorded; the same checkout on another
    // branch, a branch that is not an attempt's, and a different checkout, are not it.
    assert_eq!(
        attempt_in(&world.store, repo.path(), "gitpulse/fix-87654321").unwrap(),
        None
    );
    assert_eq!(attempt_in(&world.store, repo.path(), "main").unwrap(), None);
    assert_eq!(
        attempt_in(&world.store, other.path(), BRANCH).unwrap(),
        None
    );
}

#[test]
fn a_local_ci_result_in_an_attempt_worktree_reaches_the_task_once() {
    let world = world();
    let repo = world.attempt("a", Some(BRANCH));
    crate::test_support::git_in(repo.path(), &["checkout", "-q", "-b", BRANCH]);
    let host = host_of(&world);
    let attempt = attempt_of(&host, repo.path().to_str().unwrap())
        .unwrap()
        .expect("the checkout is the attempt's worktree");
    assert_eq!(attempt.task_id, "task-a");

    assert!(record_local(&host, &attempt, "ci_failed", "failure", &sha('e')).unwrap());
    let inbox = world.inbox();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["kind"], "run_ci_failed");
    assert_eq!(inbox[0]["signal"]["source"], "ci_local");
    assert_eq!(inbox[0]["signal"]["workflow"], "ci.local");
    assert_eq!(world.native_titles(), vec![FAILED_TITLE.to_owned()]);
    // The same commit with the same verdict is the same result.
    assert!(!record_local(&host, &attempt, "ci_failed", "failure", &sha('e')).unwrap());
    assert_eq!(world.inbox().len(), 1);
}

#[test]
fn a_plain_checkout_or_missing_profile_is_not_an_attempt_and_creates_nothing() {
    let world = world();
    let repo = world.attempt("a", Some(BRANCH));
    // `main` is checked out; the run was prepared for an attempt branch.
    let host = host_of(&world);
    assert_eq!(
        attempt_of(&host, repo.path().to_str().unwrap()).unwrap(),
        None
    );
    let nowhere = tempfile::tempdir().unwrap();
    let absent = WorkbenchState::for_profile(&nowhere.path().join("absent.sqlite"));
    crate::test_support::git_in(repo.path(), &["checkout", "-q", "-b", BRANCH]);
    assert_eq!(
        attempt_of(&absent, repo.path().to_str().unwrap()).unwrap(),
        None
    );
    assert!(
        !nowhere.path().join("absent.sqlite").exists(),
        "looking for an attempt must not create a task profile"
    );
}
