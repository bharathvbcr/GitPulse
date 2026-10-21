//! Adversarial checks on `push_planned`'s argv allowlist and the force-push
//! capability guard.

use super::*;
use crate::test_support::{git_in, git_repo, trust_repo};

struct Pair {
    origin: tempfile::TempDir,
    work: tempfile::TempDir,
}

/// `work` has one commit on `main`, already pushed to the bare `origin`.
fn pair() -> Pair {
    let origin = tempfile::TempDir::new().unwrap();
    git_in(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
    let work = git_repo();
    git_in(work.path(), &["commit", "--allow-empty", "-q", "-m", "A"]);
    git_in(
        work.path(),
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );
    git_in(work.path(), &["push", "-q", "origin", "main"]);
    Pair { origin, work }
}

fn path(dir: &tempfile::TempDir) -> String {
    dir.path().to_str().unwrap().to_string()
}

fn tip(dir: &Path) -> String {
    git_text(dir, &["rev-parse", "refs/heads/main"])
        .unwrap()
        .trim()
        .to_string()
}

fn run(pair: &Pair, args: &[&str]) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    GitWriter::push_planned(&path(&pair.work), &args)
}

/// Gives `origin` a commit the work tree does not have, and `work` one origin
/// does not have; returns origin's tip.
fn diverge(pair: &Pair) -> String {
    let other = tempfile::TempDir::new().unwrap();
    git_in(
        other.path(),
        &["clone", "-q", pair.origin.path().to_str().unwrap(), "."],
    );
    git_in(other.path(), &["commit", "--allow-empty", "-q", "-m", "B"]);
    git_in(other.path(), &["push", "-q", "origin", "main"]);
    git_in(
        pair.work.path(),
        &["commit", "--allow-empty", "-q", "-m", "C"],
    );
    trust_repo(pair.origin.path());
    tip(pair.origin.path())
}

#[test]
fn adv_l1_push_planned_refuses_every_flag_shape_outside_the_pair() {
    let pair = pair();
    let before = tip(pair.origin.path());
    let pairflags = ["--force-with-lease", "--force-if-includes"];
    let cases: Vec<Vec<&str>> = vec![
        vec!["push", "--force-if-includes", "origin", "main"],
        vec!["push", "--no-force-if-includes", "origin", "main"],
        vec![
            "push",
            "--force-with-lease=main:abc",
            "--force-if-includes",
            "origin",
        ],
        vec!["push", "--force-with-lease=main", "origin", "main"],
        vec![
            "push",
            "--force-with-lease",
            "--force-with-lease",
            "--force-if-includes",
            "origin",
        ],
        vec![
            "push",
            "--force-with-lease",
            "--force-if-includes",
            "--force-if-includes",
            "origin",
        ],
        vec![
            "push",
            "--force-with-lease",
            "--force-if-includes=x",
            "origin",
        ],
        vec!["push", pairflags[0], pairflags[1], "--force", "origin"],
        vec!["push", "--mirror", "origin"],
        vec!["push", "--delete", "origin", "main"],
        vec!["push", "-d", "origin", "main"],
        vec!["push", "--tags", "origin"],
        vec!["push", "--all", "origin"],
        vec!["push", "--prune", "origin"],
        vec!["push", "origin", "--mirror"],
        vec!["push", "origin", "--force"],
        vec!["push", "origin", "-f"],
        vec!["push", "origin", "main", "--delete"],
        vec!["push", "origin", ":main"],
        vec!["push", "origin", "HEAD:main"],
        vec!["push", "origin", "+HEAD:main"],
        vec!["push", "+main"],
        vec!["push", ":main"],
        vec![
            "push",
            "--force-with-lease",
            "--force-if-includes",
            "-u",
            "--",
            "origin",
        ],
        vec!["push", "origin", "--receive-pack=/bin/true"],
        vec!["push", "--exec=/bin/true", "origin"],
        vec!["push", "--", "origin"],
        vec!["push", ""],
        vec!["fetch"],
    ];
    for case in &cases {
        assert!(run(&pair, case).is_err(), "{case:?} must be refused");
    }
    assert_eq!(tip(pair.origin.path()), before);
}

/// The reverse order, the pair split by the remote, and `-u` twice are all
/// accepted: they are the same flags git would see, so not a bypass.
#[test]
fn adv_l1_push_planned_accepts_harmless_reorderings() {
    let pair = pair();
    for case in [
        vec![
            "push",
            "--force-if-includes",
            "--force-with-lease",
            "origin",
            "main",
        ],
        vec![
            "push",
            "--force-with-lease",
            "origin",
            "--force-if-includes",
            "main",
        ],
        vec!["push", "-u", "-u", "origin", "main"],
    ] {
        run(&pair, &case).unwrap_or_else(|err| panic!("{case:?}: {err}"));
    }
}

/// Lookalike dashes and flags in the branch slot never reach git as options.
#[test]
fn adv_l1_unicode_dashes_and_branch_slot_flags_do_not_become_options() {
    let pair = pair();
    let before = tip(pair.origin.path());
    for flag in [
        "\u{2014}force",
        "\u{2013}force-with-lease",
        "\u{2212}f",
        "\u{ff0d}f",
    ] {
        assert!(
            run(&pair, &["push", "origin", flag]).is_err(),
            "{flag:?} is a refspec git cannot resolve, not an option"
        );
        assert!(
            run(&pair, &["push", flag, "main"]).is_err(),
            "{flag:?} as remote"
        );
    }
    for flag in ["--force", "-f", "--mirror", "--delete", "-d"] {
        assert!(GitWriter::plan_push(&path(&pair.work), Some("origin"), Some(flag), true).is_err());
        assert!(GitWriter::plan_push(&path(&pair.work), Some(flag), Some("main"), true).is_err());
        assert!(GitWriter::plan_push(&path(&pair.work), None, Some(flag), false).is_err());
    }
    assert_eq!(tip(pair.origin.path()), before);
}

/// `+refspec` is git's per-ref force. `validate_ref_name` allows `+`, and the
/// remote slot is only checked for the FIRST non-flag word, so `origin +main`
/// is a force push with no lease. Predates the lane.
#[test]
fn adv_l1_plus_refspec_in_the_branch_slot_is_a_force_push_without_a_lease() {
    let pair = pair();
    let theirs = diverge(&pair);
    for refspec in ["+main", ":main", "a:b", "main:main", "+HEAD:main"] {
        let result = run(&pair, &["push", "origin", refspec]);
        let after = tip(pair.origin.path());
        assert!(
            result.is_err() && after == theirs,
            "`push origin {refspec}` overwrote origin/main ({theirs} -> {after}): {result:?}"
        );
    }
}

/// Same hole through the planner a caller reaches: `branch = "+main"`.
#[test]
fn adv_l1_plan_push_refuses_a_plus_branch() {
    let pair = pair();
    for branch in ["+main", ":main", "a:b"] {
        for remote in [Some("origin"), None] {
            let planned = GitWriter::plan_push(&path(&pair.work), remote, Some(branch), false);
            let message = planned.expect_err(branch);
            // `+` is the one refspec shape the ref-name rules allow, so it is
            // the one that carries the refspec explanation; `:` is refused
            // earlier as a character git forbids in ref names.
            if branch.starts_with('+') {
                assert!(message.contains("refspec"), "{branch}: {message}");
            }
        }
    }
}

/// An old git is told to update; a git that could not be probed is told the
/// version could not be checked, and a shed probe keeps its deferral shape so
/// the retry classifier reads it as `deferred_under_load`.
#[test]
fn adv_l1_the_force_refusal_cannot_tell_an_old_git_from_an_unprobed_one() {
    use crate::engine::git_cli::{is_deferred_under_load, GitPreflight, GitPreflightStatus};
    let answer = |status, message: &str| GitPreflight {
        status,
        version: None,
        message: Some(message.to_string()),
    };
    let old =
        require_force_if_includes(&answer(GitPreflightStatus::Outdated, "too old")).unwrap_err();
    assert!(old.contains("2.30") && old.contains("Update Git"), "{old}");
    assert!(!old.contains("could not be checked"), "{old}");

    for status in [GitPreflightStatus::Broken, GitPreflightStatus::Missing] {
        let unknown = require_force_if_includes(&answer(status, "xcrun: error")).unwrap_err();
        assert!(unknown.contains("could not be checked"), "{unknown}");
        assert!(!unknown.contains("older"), "{unknown}");
        assert!(!is_deferred_under_load(&unknown), "{unknown}");
    }

    let shed = "git version deferred under load after 1.500s: too many git processes";
    let deferred = require_force_if_includes(&answer(
        GitPreflightStatus::Unchecked,
        &format!("Git could not be checked yet: {shed}"),
    ))
    .unwrap_err();
    assert!(is_deferred_under_load(&deferred), "{deferred}");
}
