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
    serve_with(&farm.repo, &Vec::new(), memo, TTL, APP_BUDGET)
}

#[test]
fn a_clean_farm_is_answered_whole_and_a_repeat_costs_no_git_child() {
    let farm = farm(3, false);
    let memo = CollisionMemo::new(TTL);
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
        again.age_ms <= TTL.as_millis() as u64 + 500,
        "{}",
        again.age_ms
    );
}

#[test]
fn a_positive_collision_is_read_again_where_it_was_found() {
    let farm = farm(3, true);
    let memo = CollisionMemo::new(TTL);
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
    let mut facets = ask(&farm, &CollisionMemo::new(TTL));
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
