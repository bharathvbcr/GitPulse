use super::{judge, manvi_owner, native_owner, Verdict};
use crate::engine::git_cli::{git_global, git_text};
use crate::workbench::process_birth::{self, Liveness};
use crate::workbench::{query, Inner, WorkbenchState};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn run(state: &str, owner: &str, process: Option<(u32, &str)>) -> Value {
    let mut run = json!({"id":"run","state":state,"owner_id":owner});
    if let Some((pid, birth)) = process {
        run["process_id"] = json!(pid);
        run["process_start"] = json!(birth);
    }
    run
}

/// Panic if consulted: the branch under test must not need this answer.
fn no_child(_: u32, _: &str) -> Liveness {
    panic!("the agent process must not be consulted here")
}
fn no_owner(_: u32, _: u128) -> Liveness {
    panic!("the launching host must not be consulted here")
}

#[test]
fn owner_ids_parse_only_in_the_exact_native_shape() {
    assert_eq!(native_owner("native-42-1700-3"), Some((42, 1700)));
    for bad in [
        "native-0-1-1",
        "native-42-1700",
        "native-42-1700-3-4",
        "native--1700-3",
        "native-x-1700-3",
        "manvi-42-1700-3",
        "",
    ] {
        assert_eq!(native_owner(bad), None, "{bad}");
    }
}

#[test]
fn a_recorded_process_decides_alone_and_only_gone_releases() {
    let other = format!("native-{}-1-1", std::process::id().wrapping_add(1));
    let held = run("running", &other, Some((77, "birth")));
    let verdict = |child: Liveness| judge(&held, Some(false), move |_, _| child.clone(), no_owner);
    assert!(matches!(verdict(Liveness::Gone), Verdict::Release(r) if r.contains("77")));
    assert!(
        matches!(verdict(Liveness::Alive), Verdict::Keep(r) if r.contains("still running as pid 77"))
    );
    assert!(
        matches!(verdict(Liveness::Unknown("EPERM".into())), Verdict::Keep(r) if r.contains("Could not check") && r.contains("EPERM"))
    );
    // The same rule for a managed attempt, whose owner nothing here can parse.
    let managed = run("unresolved", "manvi-session", Some((78, "birth")));
    assert!(matches!(
        judge(&managed, Some(false), |_, _| Liveness::Gone, no_owner),
        Verdict::Release(_)
    ));
}

#[test]
fn without_a_process_only_a_provably_gone_owner_releases() {
    let pid = std::process::id().wrapping_add(1);
    let owner = format!("native-{pid}-5-1");
    let held = run("starting", &owner, None);
    let decide = |owner: Liveness| judge(&held, Some(false), no_child, move |_, _| owner.clone());
    assert!(matches!(decide(Liveness::Gone), Verdict::Release(_)));
    assert!(
        matches!(decide(Liveness::Alive), Verdict::Keep(r) if r.contains("Another running GitPulse"))
    );
    assert!(matches!(
        decide(Liveness::Unknown("denied".into())),
        Verdict::Keep(_)
    ));
    // Nothing to judge for a host this code cannot read.
    assert!(matches!(
        judge(
            &run("starting", "manvi-session", None),
            Some(false),
            no_child,
            no_owner
        ),
        Verdict::Keep(_)
    ));
}

#[test]
fn this_process_releases_its_own_final_unresolved_but_never_a_launch_in_flight() {
    let owner = format!("native-{}-5-1", std::process::id());
    let alive = |_, _| Liveness::Alive;
    assert!(matches!(
        judge(
            &run("unresolved", &owner, None),
            Some(false),
            no_child,
            alive
        ),
        Verdict::Release(_)
    ));
    assert!(matches!(
        judge(&run("starting", &owner, None), Some(false), no_child, alive),
        Verdict::Keep(r) if r.contains("still starting")
    ));
}

#[test]
fn a_terminal_this_process_still_holds_is_never_reconciled() {
    for state in ["starting", "running", "unresolved"] {
        assert!(matches!(
            judge(&run(state, "native-1-1-1", Some((1, "b"))), Some(true), no_child, no_owner),
            Verdict::Keep(r) if r.contains("still holds")
        ));
    }
    // Without the registry, this process's own runs are left alone too.
    let own = format!("native-{}-5-1", std::process::id());
    assert!(matches!(
        judge(
            &run("running", &own, Some((1, "b"))),
            None,
            no_child,
            no_owner
        ),
        Verdict::Keep(_)
    ));
}

#[test]
fn states_that_do_not_hold_a_checkout_are_left_as_they_are() {
    for state in ["prepared", "exited", "failed", "cancelled", ""] {
        assert!(matches!(
            judge(
                &run(state, "native-1-1-1", Some((1, "b"))),
                Some(false),
                no_child,
                no_owner
            ),
            Verdict::Keep(_)
        ));
    }
}

// --- Against the real store and real processes -------------------------------

fn host(path: &Path) -> WorkbenchState {
    WorkbenchState(Arc::new(Inner {
        path: Some(path.into()),
        ..Inner::default()
    }))
}

fn repo(root: &Path) {
    git_global(&["init", root.to_str().unwrap()]).unwrap();
    crate::test_support::trust_repo(root);
    git_text(
        root,
        &[
            "-c",
            "user.name=Workbench Test",
            "-c",
            "user.email=workbench@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    )
    .unwrap();
}

fn seed(state: &WorkbenchState, root: &Path) {
    state
        .register(root.to_str().unwrap(), "repo", "register")
        .unwrap();
    state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Ship it","description":"Exact saved instructions","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
}

fn prepare(
    state: &WorkbenchState,
    root: &Path,
    id: &str,
) -> Result<Value, crate::workbench::WorkbenchError> {
    state.request(
        "runs.prepare_terminal",
        &json!({"id":id,"request_id":format!("prepare-{id}"),"task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"claude","permission_mode":"ask","repo_path":root}).to_string(),
    )
}

/// What a crashed host leaves: claimed and started under an owner whose
/// process is `owner`, recording `child` as the agent.
fn strand(state: &WorkbenchState, id: &str, owner: &str, child: Option<(u32, String)>) {
    let store = |method: &str, input: Value| {
        state
            .with_store(|store| query(store, method, &input.to_string()))
            .unwrap()
    };
    store(
        "runs.claim",
        json!({"id":id,"request_id":format!("claim-{id}"),"expected_revision":1,"owner_id":owner,"session_id":format!("session-{id}")}),
    );
    if let Some((pid, birth)) = child {
        store(
            "runs.started",
            json!({"id":id,"request_id":format!("started-{id}"),"expected_revision":2,"owner_id":owner,"session_id":format!("session-{id}"),"process_id":pid,"process_start":birth}),
        );
    }
}

/// A process that has started and been reaped: its PID names nothing now
/// (or something born after it), which is exactly a crashed agent.
///
/// The `cfg(test)` here and on the test below repeats the gated declaration
/// of this whole file in `reconcile.rs`, and is kept because
/// `tests/spawn_seam.rs` classifies source by the attributes it can see in
/// the file: without it, these fixture processes read as production code
/// spawning outside the gated seam.
#[cfg(test)]
fn reaped() -> (u32, String) {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "read line"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let birth = process_birth::read(pid).unwrap();
    drop(child.stdin.take());
    child.wait().unwrap();
    (pid, birth)
}

#[cfg(unix)]
#[test]
fn a_crashed_session_no_longer_blocks_its_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    repo(&root);
    let state = host(&dir.path().join("profile.sqlite"));
    seed(&state, &root);
    prepare(&state, &root, "crashed").unwrap();
    let (dead, birth) = reaped();
    // Owner and agent both gone, as after a force-quit.
    strand(
        &state,
        "crashed",
        &format!("native-{dead}-1-1"),
        Some((dead, birth)),
    );
    // Before this change the next launch failed forever. Now the preparation
    // itself finds the stranded attempt, releases it and goes ahead.
    let next = prepare(&state, &root, "next").unwrap();
    assert_eq!(next["item"]["state"], "prepared");
    let old = state.request("runs.get", r#"{"id":"crashed"}"#).unwrap();
    assert_eq!(old["item"]["state"], "exited");
    assert_eq!(old["item"]["outcome_uncertain"], true);
    assert!(old["item"]["exit_code"].is_null());
    assert!(old["item"]["reason"]
        .as_str()
        .unwrap()
        .starts_with("Reconciled: agent process"));
    // And the person is told: the release raises the inbox notice.
    let inbox = state
        .request("attention.list", r#"{"filter":"all"}"#)
        .unwrap();
    assert!(inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["target_id"] == "crashed" && a["kind"] == "run_unresolved"));
}

#[cfg(test)]
#[cfg(unix)]
#[test]
fn a_live_agent_keeps_its_checkout_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    repo(&root);
    let state = host(&dir.path().join("profile.sqlite"));
    seed(&state, &root);
    prepare(&state, &root, "live").unwrap();
    let mut agent = std::process::Command::new("sh")
        .args(["-c", "read line"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = agent.id();
    let birth = process_birth::read(pid).unwrap();
    let (gone_owner, _) = reaped();
    strand(
        &state,
        "live",
        &format!("native-{gone_owner}-1-1"),
        Some((pid, birth)),
    );
    let refused = prepare(&state, &root, "second").unwrap_err();
    assert_eq!(refused.code, "checkout_busy");
    let answer = state.request("runs.release", r#"{"id":"live"}"#).unwrap();
    assert_eq!(answer["released"], false);
    assert!(answer["reason"]
        .as_str()
        .unwrap()
        .contains(&format!("pid {pid}")));
    assert_eq!(
        state.request("runs.get", r#"{"id":"live"}"#).unwrap()["item"]["state"],
        "running"
    );
    drop(agent.stdin.take());
    agent.wait().unwrap();
    // The moment it is gone, the explicit action releases it.
    let answer = state.request("runs.release", r#"{"id":"live"}"#).unwrap();
    assert_eq!(answer["released"], true);
    assert_eq!(answer["item"]["state"], "exited");
    prepare(&state, &root, "second").unwrap();
}

#[cfg(unix)]
#[test]
fn a_sweep_releases_only_the_attempts_with_proof_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    repo(&root);
    // Three worktrees so three attempts can be held at once.
    let mut checkouts = vec![root.clone()];
    for name in ["a", "b"] {
        let path = dir.path().join(name);
        git_text(
            &root,
            &["worktree", "add", "--detach", path.to_str().unwrap()],
        )
        .unwrap();
        crate::test_support::trust_repo(&path);
        checkouts.push(path);
    }
    let state = host(&dir.path().join("profile.sqlite"));
    seed(&state, &root);
    for (i, path) in checkouts.iter().enumerate() {
        prepare(&state, path, &format!("r{i}")).unwrap();
    }
    let (dead, birth) = reaped();
    let me = std::process::id();
    // r0: everything gone. r1: this process launched it and it is still
    // starting. r2: launched by a dead owner, no process recorded.
    strand(
        &state,
        "r0",
        &format!("native-{dead}-1-1"),
        Some((dead, birth)),
    );
    strand(&state, "r1", &format!("native-{me}-{}-1", now()), None);
    strand(&state, "r2", &format!("native-{dead}-1-2"), None);
    assert_eq!(state.reconcile_stale_runs().unwrap(), 2);
    let states: Vec<Value> = ["r0", "r1", "r2"]
        .iter()
        .map(|id| {
            state
                .request("runs.get", &json!({"id":id}).to_string())
                .unwrap()["item"]["state"]
                .clone()
        })
        .collect();
    assert_eq!(states, ["exited", "starting", "exited"]);
    assert_eq!(state.reconcile_stale_runs().unwrap(), 0);
}

#[test]
fn the_renderer_cannot_reconcile_directly_or_release_without_a_valid_id() {
    let dir = tempfile::tempdir().unwrap();
    let state = host(&dir.path().join("profile.sqlite"));
    let direct = state
        .request(
            "runs.reconcile",
            r#"{"id":"run","request_id":"r","expected_revision":2,"owner_id":"a","prior_owner_id":"b","reason":"trust me"}"#,
        )
        .unwrap_err();
    assert_eq!(direct.code, "host_only");
    for input in [
        r#"{"id":""}"#,
        r#"{"id":"../x"}"#,
        r#"{"id":"x","extra":1}"#,
        "[]",
    ] {
        assert_eq!(
            state.request("runs.release", input).unwrap_err().code,
            "invalid_input",
            "{input}"
        );
    }
    assert!(!dir.path().join("profile.sqlite").exists() || state.reconcile_stale_runs().is_ok());
}

#[test]
fn a_profile_that_does_not_exist_is_not_created_by_a_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.sqlite");
    assert_eq!(host(&path).reconcile_stale_runs().unwrap(), 0);
    assert!(!path.exists());
}

#[test]
fn manvi_owner_ids_parse_only_in_their_exact_shape() {
    assert_eq!(manvi_owner("manvi-42-1700-ab12cd"), Some((42, 1700)));
    for bad in [
        "manvi-0-1700-ab",
        "manvi-42-1700",
        "manvi-42-1700-",
        "manvi-42-1700-zz",
        "manvi-42-1700-ab-cd",
        "manvi--1700-ab",
        &format!("manvi-42-1700-{}", "a".repeat(65)),
        "native-42-1700-3",
        // What Manvi recorded before 18fc2c6: nothing in it can be checked.
        "0123456789abcdef0123456789abcdef",
        "",
    ] {
        assert_eq!(manvi_owner(bad), None, "{bad}");
    }
}

/// A managed attempt claimed by a Manvi that died before activation has no
/// provider pid. Its owner names that Manvi, so the Manvi is the evidence:
/// only a provably gone one releases the checkout.
#[test]
fn an_unactivated_managed_attempt_is_released_only_when_its_manvi_is_gone() {
    let held = run("starting", "manvi-4242-1700-ab12cd", None);
    let decide = |manvi: Liveness| {
        judge(&held, Some(false), no_child, move |pid, stamp| {
            assert_eq!((pid, stamp), (4242, 1700), "asked about the wrong process");
            manvi.clone()
        })
    };
    assert!(
        matches!(decide(Liveness::Gone), Verdict::Release(r) if r.contains("Manvi process 4242"))
    );
    assert!(
        matches!(decide(Liveness::Alive), Verdict::Keep(r) if r.contains("Manvi (pid 4242) is still running"))
    );
    assert!(matches!(
        decide(Liveness::Unknown("denied".into())),
        Verdict::Keep(r) if r.contains("Could not check") && r.contains("denied")
    ));
    // A recorded provider process still decides alone.
    let activated = run("running", "manvi-4242-1700-ab12cd", Some((79, "birth")));
    assert!(matches!(
        judge(&activated, Some(false), |_, _| Liveness::Alive, no_owner),
        Verdict::Keep(r) if r.contains("pid 79")
    ));
    // An owner from before Manvi named itself stays unprovable.
    assert!(matches!(
        judge(
            &run("starting", "0123456789abcdef0123456789abcdef", None),
            Some(false),
            no_child,
            no_owner
        ),
        Verdict::Keep(_)
    ));
}

/// Against the real store and a real reaped pid: the sweep releases the
/// attempt whose Manvi is gone and keeps the one whose Manvi is this live
/// process, started before its stamp.
#[cfg(unix)]
#[test]
fn a_sweep_releases_an_attempt_whose_manvi_died_before_activation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    repo(&root);
    let other = dir.path().join("other");
    git_text(
        &root,
        &["worktree", "add", "--detach", other.to_str().unwrap()],
    )
    .unwrap();
    crate::test_support::trust_repo(&other);
    let state = host(&dir.path().join("profile.sqlite"));
    seed(&state, &root);
    prepare(&state, &root, "dead-manvi").unwrap();
    prepare(&state, &other, "live-manvi").unwrap();
    let (dead, _) = reaped();
    strand(&state, "dead-manvi", &format!("manvi-{dead}-1-ab12"), None);
    strand(
        &state,
        "live-manvi",
        &format!("manvi-{}-{}-cd34", std::process::id(), now()),
        None,
    );
    assert_eq!(state.reconcile_stale_runs().unwrap(), 1);
    let item = |id: &str| {
        state
            .request("runs.get", &json!({"id":id}).to_string())
            .unwrap()["item"]
            .clone()
    };
    assert_eq!(item("dead-manvi")["state"], "exited");
    assert!(item("dead-manvi")["reason"]
        .as_str()
        .unwrap()
        .contains("Manvi process"));
    assert_eq!(item("live-manvi")["state"], "starting");
    // The released checkout takes a new attempt; nothing else moved.
    prepare(&state, &root, "after").unwrap();
    assert_eq!(state.reconcile_stale_runs().unwrap(), 0);
}

/// What a managed attempt looks like in the store before activation: kind
/// `managed`, claimed at `claimed` (the store's seconds), no process.
fn managed(state: &str, owner: &str, claimed: u64) -> Value {
    json!({"id":"run","kind":"managed","state":state,"owner_id":owner,"claimed_at":claimed})
}

const LEGACY: &str = "0123456789abcdef0123456789abcdef";

/// The defect: an attempt claimed by a Manvi from before 18fc2c6 records an
/// owner naming no process, so when that Manvi died before activation the
/// attempt held its checkout until someone pressed Release. What Manvi does
/// with an unactivated attempt — any build of it — is the proof instead.
#[test]
fn an_unactivated_managed_attempt_is_released_by_what_manvi_guarantees() {
    use super::{judge_at, MANAGED_ACTIVATION_GRACE_SECS as GRACE};
    let claimed = 1_800_000_000;
    let at = |run: &Value, now: u64| judge_at(run, Some(false), no_child, no_owner, now);

    // An opaque owner: kept, with the time it will be released, until the
    // grace has passed; then released. Its Manvi is never asked about.
    let legacy = managed("starting", LEGACY, claimed);
    for waited in [0, 60, GRACE - 1] {
        assert!(
            matches!(at(&legacy, claimed + waited), Verdict::Keep(r) if r.contains("older Manvi") && r.contains("10 minutes")),
            "{waited}"
        );
    }
    for waited in [GRACE, GRACE + 1, 30 * 24 * 3600] {
        assert!(
            matches!(at(&legacy, claimed + waited), Verdict::Release(r) if r.contains("never activated")),
            "{waited}"
        );
    }
    // A clock that reads earlier than the claim never releases.
    assert!(matches!(at(&legacy, claimed - 3600), Verdict::Keep(_)));
    assert!(matches!(at(&legacy, 0), Verdict::Keep(_)));

    // Manvi stops the provider before recording an unactivated attempt
    // unresolved, so that releases at once, whatever claimed it and however
    // alive that Manvi still is.
    for owner in [LEGACY, "manvi-4242-1700-ab12cd"] {
        assert!(
            matches!(at(&managed("unresolved", owner, claimed), claimed), Verdict::Release(r) if r.contains("unresolved")),
            "{owner}"
        );
    }

    // A Manvi that names itself and is alive past the grace: its own final
    // write failed after it had stopped the provider. Released.
    let named = managed("starting", "manvi-4242-1700-ab12cd", claimed);
    assert!(matches!(
        judge_at(
            &named,
            Some(false),
            no_child,
            |_, _| Liveness::Alive,
            claimed + GRACE
        ),
        Verdict::Release(_)
    ));
    // Within the grace it is still its Manvi's to decide.
    assert!(matches!(
        judge_at(&named, Some(false), no_child, |_, _| Liveness::Alive, claimed + 60),
        Verdict::Keep(r) if r.contains("still running")
    ));
    assert!(matches!(
        judge_at(&named, Some(false), no_child, |_, _| Liveness::Gone, claimed + 60),
        Verdict::Release(r) if r.contains("Manvi process 4242")
    ));

    // Activated: the recorded provider decides alone, past any grace.
    let mut activated = managed("running", LEGACY, claimed);
    activated["process_id"] = json!(79);
    activated["process_start"] = json!("birth");
    assert!(matches!(
        judge_at(&activated, Some(false), |_, _| Liveness::Alive, no_owner, claimed + GRACE * 10),
        Verdict::Keep(r) if r.contains("pid 79")
    ));

    // The rule is the managed lane's. A terminal attempt with the same owner
    // shape and age is not released by it.
    let mut terminal = managed("starting", LEGACY, claimed);
    terminal["kind"] = json!("external_terminal");
    assert!(matches!(
        at(&terminal, claimed + GRACE * 10),
        Verdict::Keep(_)
    ));
    // Nor is one whose claim time is missing or not a number.
    for bad in [json!(null), json!("1800000000"), json!(-1)] {
        let mut run = managed("starting", LEGACY, claimed);
        run["claimed_at"] = bad.clone();
        assert!(
            matches!(at(&run, claimed + GRACE * 10), Verdict::Keep(_)),
            "{bad}"
        );
    }
}

/// Against the real store: a managed claim records `claimed_at` in the
/// seconds the grace is measured in, and an unactivated attempt its Manvi
/// recorded unresolved is released by a sweep while its Manvi — this live
/// process — still runs.
#[test]
fn a_sweep_releases_managed_attempts_by_their_recorded_claim_and_outcome() {
    use super::{judge_at, MANAGED_ACTIVATION_GRACE_SECS as GRACE};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    repo(&root);
    let state = host(&dir.path().join("profile.sqlite"));
    seed(&state, &root);
    let store = |method: &str, input: Value| {
        state
            .with_store(|store| query(store, method, &input.to_string()))
            .unwrap()
    };
    state
        .request(
            "runs.prepare_managed",
            &json!({"id":"managed","request_id":"prepare-managed","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"claude","permission_mode":"ask","repo_path":root}).to_string(),
        )
        .unwrap();
    let owner = format!("manvi-{}-{}-ab12", std::process::id(), now());
    store(
        "runs.claim",
        json!({"id":"managed","request_id":"claim","expected_revision":1,"kind":"managed","owner_id":owner,"session_id":"session"}),
    );
    let claimed = state.request("runs.get", r#"{"id":"managed"}"#).unwrap()["item"].clone();
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let at = claimed["claimed_at"].as_u64().expect("no claim time");
    assert!(
        at.abs_diff(secs) < 120,
        "claimed_at {at} is not in seconds (now {secs})"
    );
    assert!(matches!(
        judge_at(
            &claimed,
            Some(false),
            no_child,
            |_, _| Liveness::Alive,
            at + GRACE
        ),
        Verdict::Release(_)
    ));
    // Within the grace, with its Manvi alive, a sweep keeps it.
    assert_eq!(state.reconcile_stale_runs().unwrap(), 0);

    store(
        "runs.finish",
        json!({"id":"managed","request_id":"finish","expected_revision":2,"owner_id":owner,"session_id":"session","outcome":"unresolved","reason":"managed preparation ended before activation","provider_state":"interrupted"}),
    );
    assert_eq!(state.reconcile_stale_runs().unwrap(), 1);
    let item = state.request("runs.get", r#"{"id":"managed"}"#).unwrap()["item"].clone();
    assert_eq!(item["state"], "exited", "{item}");
    // The checkout takes a new attempt.
    prepare(&state, &root, "after").unwrap();
    assert_eq!(state.reconcile_stale_runs().unwrap(), 0);
}
