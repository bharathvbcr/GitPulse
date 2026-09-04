//! Adversarial and scale tests for the graph's ref scope.
//!
//! `graph_ref_scope.rs` proves the intended behaviour on a realistic topology.
//! This file tries to BREAK it: ref names chosen to defeat prefix matching,
//! namespaces at a scale a CI mirror reaches, histories deeper than the
//! probe's own ceiling, and the states (empty repo, unborn HEAD, mid-rebase)
//! where git's own answers change shape.
//!
//! The load-bearing test is `the_classifier_agrees_with_git`: rather than
//! asserting what we believe git does with `--branches --remotes --tags`, it
//! asks git and compares. Every rule encoded twice — once in a Rust prefix
//! test, once in a git option — is a rule that can drift.

use std::collections::{BTreeSet, HashSet};
use std::process::Command;

use gitpulse_lib::engine::GitReader;
use gitpulse_lib::graph::{
    hidden_ref_namespaces, hidden_ref_warning, history_rev_args, is_named_ref,
    list_ref_decorations, probe_hidden_history, HiddenHistory, RefKind, RefScope,
};
use tempfile::TempDir;

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn seed() -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "--initial-branch=main", "."]);
    std::fs::write(path.join("a.txt"), "seed\n").unwrap();
    git(path, &["add", "a.txt"]);
    git(path, &["commit", "-m", "seed"]);
    let repo = path
        .canonicalize()
        .expect("canonical repo path")
        .to_string_lossy()
        .into_owned();
    (dir, repo)
}

/// A commit reachable from nothing but the ref created for it, so each ref's
/// reachability is decided by that ref alone.
fn ref_at_own_commit(dir: &std::path::Path, base: &str, refname: &str, tag: &str) -> String {
    let oid = git(
        dir,
        &[
            "commit-tree",
            &format!("{base}^{{tree}}"),
            "-p",
            base,
            "-m",
            tag,
        ],
    );
    git(dir, &["update-ref", refname, &oid]);
    oid
}

/// Ref names chosen to defeat a naive prefix test, plus the namespaces real
/// tooling actually writes.
const ADVERSARIAL_REFS: &[&str] = &[
    // Genuinely named.
    "refs/heads/ok",
    "refs/heads/nested/deep/branch",
    "refs/heads/with.dots",
    "refs/heads/ünïcödé",
    "refs/remotes/origin/ok",
    "refs/remotes/origin/nested/deep",
    "refs/tags/v9.9.9",
    // NOT named: the prefix matches as a string but not as a path component.
    // `--branches` is `refs/heads/*`; `refs/headsfoo/x` is not a branch, and a
    // classifier using `starts_with("refs/heads")` says it is.
    "refs/headsfoo/sneaky",
    "refs/heads-extra/sneaky",
    "refs/remotesx/sneaky",
    "refs/remotes-mirror/sneaky",
    "refs/tagsy/sneaky",
    "refs/tags-archive/sneaky",
    // NOT named: machine-written namespaces seen in the wild.
    "refs/cmux/last-turn/abc",
    "refs/codex/turn-diffs/checkpoints/x/y",
    "refs/prefetch/remotes/origin/main",
    "refs/pull/17/head",
    "refs/archive/releases/v1",
    "refs/notes/commits",
    // NOT named: a single-segment ref, which has no namespace below it.
    "refs/stash",
];

/// The invariant that makes the scope trustworthy: what Rust calls a named
/// ref is exactly what git walks under the named scope.
///
/// Encoded twice — a Rust prefix test and a set of `git log` options — these
/// two rules can disagree, and when they do the failure is silent in the worst
/// direction: a ref the walk skips but the classifier calls "named" is history
/// that is neither drawn NOR reported. So this asks git rather than asserting
/// from memory.
#[test]
fn the_classifier_agrees_with_git() {
    let (dir, repo) = seed();
    let base = git(dir.path(), &["rev-parse", "HEAD"]);

    let mut owner: Vec<(&str, String)> = Vec::new();
    for refname in ADVERSARIAL_REFS {
        let oid = ref_at_own_commit(dir.path(), &base, refname, refname);
        owner.push((refname, oid));
    }

    // Ask git which commits the named scope actually reaches.
    let mut args = vec!["rev-list"];
    args.extend_from_slice(history_rev_args(RefScope::Named));
    let reached: HashSet<String> = git(dir.path(), &args)
        .lines()
        .map(|l| l.trim().to_string())
        .collect();

    let mut disagreements = Vec::new();
    for (refname, oid) in &owner {
        let git_says_named = reached.contains(oid);
        let we_say_named = is_named_ref(refname);
        if git_says_named != we_say_named {
            disagreements.push(format!(
                "{refname}: git walks it = {git_says_named}, is_named_ref = {we_say_named}"
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "the classifier and git disagree about {} ref(s):\n  {}",
        disagreements.len(),
        disagreements.join("\n  ")
    );

    // And the same list must decide the labels: a ref git walks under the
    // named scope has a decoration; one it skips does not.
    let decorations = list_ref_decorations(&repo, RefScope::Named)
        .expect("named decorations")
        .decorations;
    let labelled: HashSet<&str> = decorations.iter().map(|d| d.commit_id.as_str()).collect();
    for (refname, oid) in &owner {
        if is_named_ref(refname) {
            assert!(
                labelled.contains(oid.as_str()),
                "{refname} is walked under the named scope but carries no label"
            );
        } else {
            assert!(
                !labelled.contains(oid.as_str()),
                "{refname} is not walked under the named scope yet was labelled"
            );
        }
    }
}

/// Every commit a scope's walk reaches must be reachable from a ref that same
/// scope labels. Stated over the ADVERSARIAL_REFS set, in both scopes.
#[test]
fn no_scope_walks_history_it_cannot_label() {
    let (dir, repo) = seed();
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    for refname in ADVERSARIAL_REFS {
        ref_at_own_commit(dir.path(), &base, refname, refname);
    }

    for scope in [RefScope::Named, RefScope::All] {
        let decorations = list_ref_decorations(&repo, scope)
            .expect("decorations")
            .decorations;

        // A ref outside branches, remotes and tags must never be labelled as
        // one. The kind is not cosmetic: it decides the chip a reader sees,
        // and calling `refs/headsfoo/x` a local branch invites branch actions
        // against something that is not a branch.
        for name in [
            "headsfoo/sneaky",
            "remotesx/sneaky",
            "tagsy/sneaky",
            "cmux/last-turn/abc",
        ] {
            if let Some(d) = decorations.iter().find(|d| d.name == name) {
                assert_eq!(
                    d.kind,
                    RefKind::Other,
                    "{scope:?}: {name} was labelled {:?}, not Other",
                    d.kind
                );
                assert!(!d.is_head, "{scope:?}: {name} must never be marked HEAD");
            }
        }
        let labelled: HashSet<&str> = decorations.iter().map(|d| d.commit_id.as_str()).collect();
        let head = git(dir.path(), &["rev-parse", "HEAD"]);

        // Tips, not the whole walk: an interior commit is explained by the tip
        // above it, but a TIP with no label is a lane that starts from nowhere.
        let mut args = vec!["rev-list", "--no-walk"];
        args.extend_from_slice(history_rev_args(scope));
        for tip in git(dir.path(), &args).lines() {
            let tip = tip.trim();
            if tip.is_empty() {
                continue;
            }
            assert!(
                labelled.contains(tip) || tip == head,
                "{scope:?}: tip {tip} is walked but nothing labels it"
            );
        }
    }
}

/// The report must name a namespace whenever it claims commits are hidden.
/// A count with an empty list is a sentence that trails off — and it is
/// exactly what a misclassifying `is_named_ref` produces.
#[test]
fn a_hidden_commit_is_always_attributed_to_a_namespace() {
    let (dir, repo) = seed();
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    // Names a naive prefix test calls "named" while git does not walk them.
    for refname in [
        "refs/headsfoo/sneaky",
        "refs/remotesx/sneaky",
        "refs/tagsy/sneaky",
    ] {
        ref_at_own_commit(dir.path(), &base, refname, refname);
    }

    let hidden = probe_hidden_history(&repo).expect("hidden probe");
    assert_eq!(
        hidden.commits, 3,
        "three commits are reachable from nowhere else"
    );
    assert!(
        !hidden.namespaces.is_empty(),
        "3 commits are hidden and no namespace was named for any of them"
    );
    let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
    assert!(
        warning.contains("refs/headsfoo"),
        "the namespace holding the hidden commits is not named: {warning}"
    );
}

/// A bounded list must name the namespaces that matter. Alphabetical order
/// would report six empty namespaces and hide the one holding ten thousand
/// refs — technically bounded, practically useless.
#[test]
fn the_report_names_the_largest_namespaces_first() {
    let mut names: Vec<String> = Vec::new();
    // `refs/zzz` is last alphabetically and by far the largest.
    for i in 0..40 {
        names.push(format!("refs/zzz/ref{i:03}"));
    }
    for ns in 0..10 {
        names.push(format!("refs/aa{ns:02}/only"));
    }
    let hidden = HiddenHistory {
        commits: 41,
        capped: false,
        namespaces: hidden_ref_namespaces(names.iter().map(String::as_str)),
    };
    let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
    assert!(
        warning.contains("refs/zzz"),
        "the biggest namespace was dropped from a bounded list: {warning}"
    );
}

/// A single-segment ref is its own namespace and has nothing under it.
/// Printing it as `refs/stash/*` describes a directory that does not exist.
#[test]
fn a_single_segment_ref_is_not_described_as_a_directory() {
    let hidden = HiddenHistory {
        commits: 2,
        capped: false,
        namespaces: hidden_ref_namespaces(["refs/stash"]),
    };
    let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
    assert!(warning.contains("refs/stash"), "{warning}");
    assert!(
        !warning.contains("refs/stash/*"),
        "refs/stash has no children to stand for: {warning}"
    );
}

/// Six figures of `refs/pull/*` is an ordinary CI mirror. Under the scope that
/// walks them, the decoration list must stay bounded — and must SAY it was
/// bounded, or a truncated label set is indistinguishable from a complete one.
#[test]
fn a_ref_mirror_cannot_produce_an_unbounded_decoration_list() {
    let (dir, repo) = seed();
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    // One `update-ref --stdin` batch: 5,000 processes would be the test's own
    // bottleneck, not the code's.
    let mut batch = String::new();
    for i in 0..5_000 {
        batch.push_str(&format!("create refs/pull/{i}/head {base}\n"));
    }
    let mut child = Command::new("git")
        .args(["update-ref", "--stdin"])
        .current_dir(dir.path())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("update-ref --stdin");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(batch.as_bytes())
            .expect("write batch");
    }
    assert!(child.wait().expect("update-ref").success());

    let listing = list_ref_decorations(&repo, RefScope::All).expect("all-scope decorations");
    assert!(
        listing.decorations.len() <= 1_000,
        "an all-scope listing returned {} decorations; a CI mirror would ship six figures \
         of them over IPC on every graph load",
        listing.decorations.len()
    );
    assert_eq!(
        listing.other_dropped,
        5_000 - gitpulse_lib::graph::REFS_OTHER_CAP,
        "the cap dropped refs and must report how many"
    );
    let note = listing
        .truncation_warning()
        .expect("a capped listing must say it was capped");
    assert!(
        note.contains("outside branches, remotes and tags"),
        "{note}"
    );
    // The rows those refs point at are still drawn — the cap costs labels,
    // never history. Saying otherwise would send the reader hunting for
    // commits that are on screen.
    assert!(note.contains("still drawn"), "{note}");
}

/// Deeper hidden history than the probe will walk. The count must read as a
/// floor, never as an exact number that happens to equal the ceiling.
#[test]
fn a_probe_that_hits_its_ceiling_reports_a_floor() {
    let hidden = HiddenHistory {
        commits: 5_000,
        capped: true,
        namespaces: hidden_ref_namespaces(["refs/cmux/a"]),
    };
    let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
    assert!(
        warning.contains("or more"),
        "a capped count must not read as exact: {warning}"
    );
}

/// The states where git's own answers change shape. None may error, and none
/// may report hidden history that is not there.
#[test]
fn degenerate_repositories_probe_cleanly() {
    // Empty: no commits, no refs at all.
    let empty = tempfile::tempdir().expect("tempdir");
    git(empty.path(), &["init", "--initial-branch=main", "."]);
    let empty_repo = empty
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let hidden = probe_hidden_history(&empty_repo).expect("empty repo probes cleanly");
    assert_eq!(hidden, HiddenHistory::default());
    assert_eq!(hidden_ref_warning(&hidden), None);
    assert!(list_ref_decorations(&empty_repo, RefScope::All)
        .expect("empty repo lists cleanly")
        .decorations
        .is_empty());

    // Unborn HEAD on an orphan branch, with real history on `main`.
    let (dir, repo) = seed();
    git(dir.path(), &["checkout", "-q", "--orphan", "fresh"]);
    let hidden = probe_hidden_history(&repo).expect("orphan HEAD probes cleanly");
    assert_eq!(
        hidden,
        HiddenHistory::default(),
        "an orphan branch hides nothing — main still names every commit"
    );
    let rows = GitReader::read_commit_history_paged(&repo, 0, 50, None, None, RefScope::Named)
        .expect("orphan history");
    assert_eq!(rows.len(), 1, "main's commit must still be walked");
}

/// A commit named by BOTH a branch and a custom ref is not hidden: it is
/// drawn. Reporting it would tell the user their complete graph is missing
/// something, which is the same lie as saying nothing when it really is.
#[test]
fn a_custom_ref_pointing_at_drawn_history_reports_nothing() {
    let (dir, repo) = seed();
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    git(dir.path(), &["update-ref", "refs/archive/v1", &base]);
    git(dir.path(), &["update-ref", "refs/cmux/last-turn/x", &base]);

    let hidden = probe_hidden_history(&repo).expect("hidden probe");
    assert_eq!(hidden.commits, 0, "every commit is on main and is drawn");
    assert_eq!(
        hidden_ref_warning(&hidden),
        None,
        "a complete graph must not be reported as incomplete"
    );
}

/// Namespace grouping over hostile input: no panic, no misattribution.
#[test]
fn the_census_survives_malformed_ref_names() {
    let census = hidden_ref_namespaces([
        "",
        "refs",
        "refs/",
        "refs//doubled",
        "HEAD",
        "not-a-ref",
        "refs/ok/x",
        "  refs/spaced/x  ",
    ]);
    // Only the two well-formed non-named refs are attributable.
    let keys: BTreeSet<&str> = census.keys().map(String::as_str).collect();
    assert!(keys.contains("refs/ok"), "{keys:?}");
    assert!(keys.contains("refs/spaced"), "{keys:?}");
    assert!(
        !keys.contains(""),
        "an empty name must not become a namespace"
    );
}

/// HEAD detached onto a commit that only a hidden ref names.
///
/// The commit IS drawn — the named scope walks HEAD — so reporting it as
/// hidden would tell the reader that the row under their cursor is missing.
/// The probe excludes HEAD for the same reason the walk includes it.
#[test]
fn a_head_detached_onto_a_hidden_ref_is_drawn_and_not_reported() {
    let (dir, repo) = seed();
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    let checkpoint = ref_at_own_commit(dir.path(), &base, "refs/cmux/last-turn/x", "checkpoint");
    git(dir.path(), &["checkout", "-q", "--detach", &checkpoint]);

    let rows = GitReader::read_commit_history_paged(&repo, 0, 50, None, None, RefScope::Named)
        .expect("history");
    assert!(
        rows.iter().any(|c| c.id == checkpoint),
        "the commit HEAD is sitting on must be drawn"
    );

    let hidden = probe_hidden_history(&repo).expect("hidden probe");
    assert_eq!(
        hidden.commits, 0,
        "the only commit outside a branch is the one HEAD names, and it is drawn"
    );
    assert_eq!(hidden_ref_warning(&hidden), None);
}

/// The scope crosses IPC as a serde string. A renamed variant would keep both
/// sides compiling while the comparison silently stopped matching, and a
/// malformed value must be REFUSED rather than quietly becoming a scope the
/// user did not pick.
#[test]
fn the_scope_round_trips_over_the_wire_and_refuses_nonsense() {
    assert_eq!(
        serde_json::to_string(&RefScope::Named).unwrap(),
        "\"named\""
    );
    assert_eq!(serde_json::to_string(&RefScope::All).unwrap(), "\"all\"");
    assert_eq!(
        serde_json::from_str::<RefScope>("\"named\"").unwrap(),
        RefScope::Named
    );
    assert_eq!(
        serde_json::from_str::<RefScope>("\"all\"").unwrap(),
        RefScope::All
    );
    for bad in [
        "\"Named\"",
        "\"ALL\"",
        "\"\"",
        "\"everything\"",
        "0",
        "true",
        "null",
    ] {
        assert!(
            serde_json::from_str::<RefScope>(bad).is_err(),
            "{bad} was accepted as a ref scope"
        );
    }
    // Absent is the named set, never the wider one.
    assert_eq!(RefScope::default(), RefScope::Named);
}

/// Scale: a repository carrying a thousand harness checkpoints, walked and
/// solved end to end through the production pipeline.
///
/// The named scope must stay linear in the history a person actually wrote,
/// no matter how much machine-written history sits beside it — that is the
/// whole point — and the all-refs scope must still complete and stay
/// internally consistent rather than degenerating.
#[test]
fn a_thousand_checkpoints_do_not_reach_the_named_graph() {
    let (dir, repo) = seed();
    let path = dir.path();
    for i in 0..30 {
        std::fs::write(path.join("a.txt"), format!("line {i}\n")).unwrap();
        git(path, &["add", "a.txt"]);
        git(path, &["commit", "-m", &format!("real work {i}")]);
    }
    let base = git(path, &["rev-parse", "HEAD~10"]);
    // 1,000 checkpoint pairs written in one batch: 2,000 commits and 1,000
    // refs, all forked from the same commit — the shape that produced 34
    // simultaneous anonymous lanes on a real repository.
    let mut batch = String::new();
    for i in 0..1_000 {
        let index = git(
            path,
            &[
                "commit-tree",
                &format!("{base}^{{tree}}"),
                "-p",
                &base,
                "-m",
                &format!("index on main: {i}"),
            ],
        );
        let turn = git(
            path,
            &[
                "commit-tree",
                &format!("{base}^{{tree}}"),
                "-p",
                &base,
                "-p",
                &index,
                "-m",
                &format!("On main: cmux last turn baseline {i}"),
            ],
        );
        batch.push_str(&format!("create refs/cmux/last-turn/{turn} {turn}\n"));
    }
    let mut child = Command::new("git")
        .args(["update-ref", "--stdin"])
        .current_dir(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("update-ref --stdin");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(batch.as_bytes())
            .expect("write batch");
    }
    assert!(child.wait().expect("update-ref").success());

    let named = GitReader::read_commit_history_paged(&repo, 0, 5_000, None, None, RefScope::Named)
        .expect("named history");
    assert_eq!(
        named.len(),
        31,
        "the named graph must hold the 31 commits a person wrote and nothing else"
    );

    let hidden = probe_hidden_history(&repo).expect("hidden probe");
    assert_eq!(
        hidden.commits, 2_000,
        "1,000 checkpoints hide two commits each"
    );
    assert_eq!(hidden.namespaces.get("refs/cmux"), Some(&1_000));
    let warning = hidden_ref_warning(&hidden).expect("hidden commits must be reported");
    assert!(warning.contains("2000 commit(s)"), "{warning}");
    assert!(warning.contains("refs/cmux (1000 refs)"), "{warning}");

    // The escape hatch still resolves, and its label set is capped rather
    // than shipping a thousand chips.
    let listing = list_ref_decorations(&repo, RefScope::All).expect("all-scope decorations");
    assert!(
        listing.decorations.len() <= 1_000,
        "{}",
        listing.decorations.len()
    );
    assert!(listing.other_dropped > 0);
    assert!(listing.truncation_warning().is_some());
}
