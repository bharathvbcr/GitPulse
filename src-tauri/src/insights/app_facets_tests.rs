use super::super::collision_memo::TTL as MEMO_TTL;
use super::*;
use crate::engine::git_cli::spawn_log::spawns_in;
use crate::test_support::{git_in, git_repo};

/// A repository with one commit and `count` linked worktrees, each dirty in a
/// file of its own, and with `seed.txt` dirty in each too when `contended`.
/// Paths are canonical, which is what the spawn log keys on.
struct Farm {
    _repo: tempfile::TempDir,
    _linked: tempfile::TempDir,
    repo: String,
    dirs: Vec<std::path::PathBuf>,
}

fn farm(count: usize, contended: bool) -> Farm {
    let repo_dir = git_repo();
    let repo = repo_dir.path().canonicalize().expect("canonical repo");
    std::fs::write(repo.join("seed.txt"), "seed").expect("seed");
    git_in(&repo, &["add", "seed.txt"]);
    git_in(&repo, &["commit", "-m", "seed"]);
    let linked = tempfile::tempdir().expect("linked worktrees");
    let root = linked.path().canonicalize().expect("canonical root");
    let mut dirs = vec![repo.clone()];
    for n in 0..count {
        let wt = root.join(format!("wt-{n}"));
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
        std::fs::write(wt.join(format!("own-{n}.txt")), "mine").expect("dirty");
        if contended {
            std::fs::write(wt.join("seed.txt"), format!("edit {n}")).expect("contend");
        }
        dirs.push(wt);
    }
    Farm {
        _repo: repo_dir,
        _linked: linked,
        repo: repo.to_string_lossy().into_owned(),
        dirs,
    }
}

fn marks(farm: &Farm) -> Vec<usize> {
    farm.dirs.iter().map(|dir| spawns_in(dir).len()).collect()
}

/// `(worktree list, status)` git children started in the farm since `marks`.
fn spawned(farm: &Farm, marks: &[usize]) -> (usize, usize) {
    let argvs: Vec<Vec<String>> = farm
        .dirs
        .iter()
        .zip(marks)
        .flat_map(|(dir, mark)| spawns_in(dir)[*mark..].to_vec())
        .collect();
    let has = |argv: &[String], word: &str| argv.iter().any(|arg| arg == word);
    (
        argvs.iter().filter(|a| has(a, "worktree")).count(),
        argvs.iter().filter(|a| has(a, "status")).count(),
    )
}

fn ask(farm: &Farm, memo: &CollisionMemo) -> AppFacets {
    serve_with(&farm.repo, &Vec::new(), memo, APP_TTL, APP_BUDGET)
}

#[test]
fn a_clean_farm_is_answered_whole_and_a_repeat_costs_no_git_child() {
    let farm = farm(3, false);
    let memo = CollisionMemo::new(MEMO_TTL);
    let first = ask(&farm, &memo);
    assert!(first.not_scanned.is_empty(), "{:?}", first.not_scanned);
    let worktrees = first.worktrees.as_ref().expect("worktrees scanned");
    assert_eq!(worktrees.count, 4);
    assert!(first.agents.is_some() && first.changes.is_some());
    assert_eq!(
        first.collisions.as_ref().expect("scanned").scanned_worktrees,
        4
    );

    let mark = marks(&farm);
    let again = ask(&farm, &memo);
    assert_eq!(
        spawned(&farm, &mark),
        (0, 0),
        "the second request re-read what the first had just read"
    );
    assert!(
        again.age_ms <= APP_TTL.as_millis() as u64 + 500,
        "{}",
        again.age_ms
    );
}

#[test]
fn a_positive_collision_is_read_again_where_it_was_found() {
    let farm = farm(3, true);
    let memo = CollisionMemo::new(MEMO_TTL);
    let first = ask(&farm, &memo);
    let found = first.collisions.as_ref().expect("scanned");
    assert!(found.overlapping_files >= 1, "{found:?}");

    let mark = marks(&farm);
    let again = ask(&farm, &memo);
    let (_, statuses) = spawned(&farm, &mark);
    assert!(
        statuses >= 3,
        "a positive was served from memory: {statuses} status reads for 3 parties"
    );
    assert!(again.age_ms < 1500, "{}", again.age_ms);
}

#[test]
fn an_untrusted_or_unrelated_repository_is_refused_not_answered_empty() {
    let farm = farm(1, false);
    crate::repository_trust::revoke(&farm.repo).expect("revoke trust");
    assert!(serve(&farm.repo).is_err(), "an untrusted repository was read");

    let elsewhere = tempfile::tempdir().expect("not a repository");
    assert!(
        serve(&elsewhere.path().to_string_lossy()).is_err(),
        "a directory that is no repository was read"
    );
    assert!(serve("relative/path").is_err());
}

#[test]
fn a_facet_the_app_could_not_read_is_absent_with_its_reason_and_refuses_the_snapshot() {
    let farm = farm(1, false);
    let mut facets = ask(&farm, &CollisionMemo::new(MEMO_TTL));
    facets.changes = None;
    facets.not_scanned.push(NotScanned {
        facet: "changes".into(),
        reason: "the status read failed".into(),
    });
    let refused = snapshot_from_app(&farm.repo, Instant::now(), facets)
        .expect_err("a snapshot was assembled around a gap");
    assert!(
        refused.contains("changes (the status read failed)"),
        "{refused}"
    );
}

/// Two live sessions in the main checkout, which has uncommitted work, and
/// nothing contended: the shape of this repository on a busy day.
fn busy_main(farm: &Farm) -> live_sessions::Observation {
    std::fs::write(Path::new(&farm.repo).join("scratch.txt"), "wip").expect("dirty main");
    let session = |pid: u32| crate::insights::LiveSession {
        kind: "claude".into(),
        pid,
        entrypoint: "cli".into(),
        status: "busy".into(),
        cwd: farm.repo.clone(),
        tree: None,
        run_id: None,
        parent_pid: None,
        session_id: Some(format!("s{pid}")),
        proc_start: None,
        registry_cwd: None,
    };
    vec![live_sessions::KindObservation {
        kind: "claude".into(),
        ok: true,
        live: vec![session(1), session(2)],
        ..Default::default()
    }]
}

#[test]
fn a_busy_main_checkout_is_not_a_positive_collision_to_read_again() {
    let farm = farm(2, false);
    let observation = busy_main(&farm);
    let memo = CollisionMemo::new(MEMO_TTL);
    let first = serve_with(&farm.repo, &observation, &memo, APP_TTL, APP_BUDGET);
    let risk = first.collisions.as_ref().expect("scanned");
    assert_eq!(risk.overlapping_files, 0, "nothing is contended: {risk:?}");
    assert!(
        risk.shared_worktree_files >= 1,
        "the shape is two sessions in a dirty main checkout: {risk:?}"
    );

    let mark = marks(&farm);
    let again = serve_with(&farm.repo, &observation, &memo, APP_TTL, APP_BUDGET);
    let (listings, statuses) = spawned(&farm, &mark);
    eprintln!("handler git children on a busy main checkout, second ask: worktree list {listings}, status {statuses}");
    assert_eq!(
        (listings, statuses),
        (0, 0),
        "a checkout that only holds two sessions was read again"
    );
    assert!(again.age_ms <= APP_TTL.as_millis() as u64 + 500);
}

/// The age an answer can have is the promise: a read or a listing older than
/// the TTL is read again, one inside it is not.
#[test]
fn a_read_older_than_the_promised_age_is_read_again_and_a_younger_one_is_not() {
    let farm = farm(2, false);
    let ttl = Duration::from_millis(1500);
    let memo = CollisionMemo::new(MEMO_TTL);
    let serve = || serve_with(&farm.repo, &Vec::new(), &memo, ttl, APP_BUDGET);
    serve();

    let mark = marks(&farm);
    let young = serve();
    assert_eq!(spawned(&farm, &mark), (0, 0), "a young read was read again");
    assert!(young.age_ms < ttl.as_millis() as u64 + 500, "{}", young.age_ms);

    std::thread::sleep(ttl + Duration::from_millis(300));
    let mark = marks(&farm);
    let old = serve();
    let (listings, statuses) = spawned(&farm, &mark);
    assert!(
        listings >= 1 && statuses >= 3,
        "an old answer was served from memory: {listings} listings, {statuses} statuses"
    );
    assert!(old.age_ms < ttl.as_millis() as u64, "{}", old.age_ms);
}

/// A hung read cannot hold the handler: past its wait every facet is
/// answered not scanned, and the number of abandoned readings is bounded.
#[test]
fn a_read_that_cannot_finish_in_time_is_abandoned_and_every_facet_is_not_scanned() {
    // Its own count: the app's is shared with every test that asks it.
    static MINE: AtomicUsize = AtomicUsize::new(0);
    let reading = || {
        std::thread::sleep(Duration::from_millis(900));
        unscanned("/r".into(), "stand-in")
    };
    let started = Instant::now();
    let gave_up = serve_bounded_in(&MINE, "/r".into(), Duration::from_millis(100), reading)
        .expect("an abandoned read is still an answer");
    assert!(started.elapsed() < Duration::from_millis(700), "waited for the read");
    assert!(gave_up.worktrees.is_none() && gave_up.collisions.is_none());
    assert!(gave_up.deadline_expired);
    assert_eq!(gave_up.not_scanned.len(), 4);
    assert!(gave_up.not_scanned[0].reason.contains("did not finish"));

    // Past the bound, new asks are refused until the abandoned readings end.
    for _ in 1..MAX_ABANDONED {
        serve_bounded_in(&MINE, "/r".into(), Duration::from_millis(50), reading).expect("answered");
    }
    let refused = serve_bounded_in(&MINE, "/r".into(), Duration::from_millis(50), reading);
    assert!(refused.unwrap_err().contains("still running"));
    let ended = Instant::now();
    while MINE.load(Ordering::Acquire) > 0 {
        assert!(
            ended.elapsed() < Duration::from_secs(10),
            "abandoned reads never ended"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let after = serve_bounded_in(&MINE, "/r".into(), Duration::from_secs(2), || {
        unscanned("/r".into(), "ok")
    });
    assert!(after.is_ok());
}

/// Every read is of the canonical path validation returned: asked through a
/// link, the app reads (and names) the real repository.
#[test]
fn a_repository_asked_for_through_a_link_is_read_at_its_canonical_path() {
    let farm = farm(1, false);
    let links = tempfile::tempdir().expect("link dir");
    let link = links.path().join("alias");
    std::os::unix::fs::symlink(&farm.repo, &link).expect("symlink");
    let mark = marks(&farm);
    let answer = serve(&link.to_string_lossy()).expect("trusted through the link");
    assert_eq!(answer.repo, farm.repo);
    let reads: usize = farm
        .dirs
        .iter()
        .zip(&mark)
        .map(|(dir, mark)| spawns_in(dir).len() - mark)
        .sum();
    assert!(reads > 0, "no read was made in the repository itself");
}

/// The app runs an agent's read in the class the agent's own scan runs in
/// (`gitpulse-mcp` is a background process), not at the UI's priority.
#[test]
fn the_apps_reads_for_an_mcp_process_run_in_the_background_class() {
    use crate::engine::git_cli::spawn_log::classed_spawns_in;
    let farm = farm(2, false);
    let answer = serve(&farm.repo).expect("the app reads");
    assert!(answer.worktrees.is_some());
    let classes: Vec<_> = farm
        .dirs
        .iter()
        .flat_map(|dir| classed_spawns_in(dir))
        .filter(|(argv, _)| argv.iter().any(|arg| arg == "status" || arg == "worktree"))
        .collect();
    assert!(!classes.is_empty(), "no read was recorded");
    for (argv, class) in classes {
        assert_eq!(
            class,
            crate::engine::git_cli::Admission::Background,
            "{argv:?} ran at the UI's priority"
        );
    }
}

/// An answer that is partial is not one the asker may build a snapshot on.
#[test]
fn a_partial_answer_is_not_a_snapshot() {
    let farm = farm(1, false);
    let whole = ask(&farm, &CollisionMemo::new(MEMO_TTL));
    assert!(snapshot_from_app(&farm.repo, Instant::now(), whole.clone()).is_ok());
    let edits: [fn(&mut AppFacets); 4] = [
        |f| f.deadline_expired = true,
        |f| f.collisions.as_mut().expect("scanned").unscanned_worktrees = 2,
        |f| f.collisions.as_mut().expect("scanned").failed_worktrees = 1,
        |f| f.worktrees.as_mut().expect("scanned").dirty_unknown = 1,
    ];
    for edit in edits {
        let mut partial = whole.clone();
        edit(&mut partial);
        let refused = snapshot_from_app(&farm.repo, Instant::now(), partial)
            .expect_err("a partial answer was believed");
        assert!(refused.contains("partial"), "{refused}");
    }
}
