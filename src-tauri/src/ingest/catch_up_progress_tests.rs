//! Why the ledger's catch-up never finished on a repository with two dozen
//! worktrees: one five-second budget for the address, the reflogs and the
//! transcripts, retried every ten seconds for ever, from the reactive class.

use super::*;
use crate::engine::process::{spawn_log, Admission};
use crate::test_support::repo_with_worktrees;

fn plain(path: &std::path::Path) -> String {
    crate::engine::process::canonicalize_plain(path)
        .expect("canonical")
        .to_string_lossy()
        .into_owned()
}

/// Runs `f` with the transcript root pointed at an empty corpus.
fn with_empty_transcripts<T>(f: impl FnOnce() -> T) -> T {
    let corpus = tempfile::tempdir().expect("corpus");
    let serial = crate::test_support::env::env_serial();
    let _env =
        crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", corpus.path());
    f()
}

/// Every git child the gate started in `dirs`, with the class it was admitted
/// under.
fn classed_git_children(dirs: &[std::path::PathBuf]) -> Vec<(Vec<String>, Admission)> {
    dirs.iter()
        .flat_map(|dir| spawn_log::classed_spawns_in(dir))
        .collect()
}

/// A background catch-up must not spend the reactive budget the interface
/// reads from: the app asks for it on every tab switch, and a pass over a large
/// family starts dozens of children.
#[test]
fn a_catch_up_pass_starts_its_git_children_in_the_background_class() {
    let (main, _parent, linked) = repo_with_worktrees(2);
    let anchor = plain(main.path());
    let dirs: Vec<std::path::PathBuf> = std::iter::once(std::path::PathBuf::from(&anchor))
        .chain(linked.iter().cloned())
        .collect();

    let out = with_empty_transcripts(|| catch_up_coalesced(&anchor));
    assert!(out.error.is_empty(), "{out:?}");

    let children = classed_git_children(&dirs);
    let reflogs: Vec<_> = children
        .iter()
        .filter(|(argv, _)| argv.iter().any(|arg| arg == "reflog"))
        .collect();
    assert!(!reflogs.is_empty(), "the pass read no reflog: {children:?}");
    let not_background: Vec<_> = children
        .iter()
        .filter(|(_, class)| *class != Admission::Background)
        .collect();
    assert!(
        not_background.is_empty(),
        "the pass admitted children outside the background class: {not_background:?}"
    );
}

/// Git, delayed: a directory holding a `git` that sleeps `millis` and then runs
/// the real one. A host whose process starts cost over a second (security
/// scanning of every exec) behaves like this, and a fast fixture hides every
/// bug in how a pass spends its budget.
fn slow_git_dir(millis: u64) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let real = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("a git on PATH");
    let dir = tempfile::tempdir().expect("shim dir");
    let shim = dir.path().join("git");
    let seconds = millis as f64 / 1000.0;
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nsleep {seconds}\nexec \"{}\" \"$@\"\n",
            real.display()
        ),
    )
    .expect("write the shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    dir
}

/// A gate that answers nothing from the last pass, so passes run back to back.
fn eager_gate() -> flight::Gate {
    flight::Gate::new(flight::Intervals {
        complete: std::time::Duration::ZERO,
        incomplete: std::time::Duration::ZERO,
        incomplete_cap: std::time::Duration::ZERO,
    })
}

fn git_count(dirs: &[std::path::PathBuf], verb: &[&str]) -> usize {
    dirs.iter()
        .flat_map(|dir| spawn_log::spawns_in(dir))
        .filter(|argv| verb.iter().all(|word| argv.iter().any(|arg| arg == word)))
        .count()
}

/// The production report: 22 worktrees, a host that takes a good fraction of a
/// second to start any process, and a catch-up that stayed `truncated` for 39
/// hours. One pass cannot read every reflog in five seconds, so the passes
/// must add up: each resumes where the last stopped, the transcripts get their
/// own share of the budget rather than what the reflogs leave, and the pass
/// that reads the last reflog is complete.
#[test]
fn passes_over_a_slow_git_and_twenty_worktrees_add_up_to_a_completed_catch_up() {
    let (main, _parent, linked) = repo_with_worktrees(20);
    let anchor = plain(main.path());
    let corpus = tempfile::tempdir().expect("corpus");
    let transcript = corpus.path().join("slug").join("S1.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        serde_json::json!({
            "type": "assistant",
            "sessionId": "S1",
            "timestamp": "2026-09-01T12:00:01.000Z",
            "version": "2.1.241",
            "cwd": anchor,
            "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": format!("{anchor}/a.rs") } }
            ]}
        })
        .to_string()
            + "\n",
    )
    .unwrap();

    let shim = slow_git_dir(150);
    let path = std::env::join_paths(
        std::iter::once(shim.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("GITPULSE_TRANSCRIPT_ROOT", corpus.path())
        .set("PATH", &path);

    let gate = eager_gate();
    let mut passes = Vec::new();
    for _ in 0..6 {
        let out = catch_up_through(&gate, &anchor);
        let done = !out.truncated && out.error.is_empty();
        if passes.is_empty() {
            assert!(
                out.transcripts > 0,
                "the first pass left the transcripts to what the reflogs left over: {out:?}"
            );
        }
        passes.push(out);
        if done {
            break;
        }
    }
    let last = passes.last().unwrap();
    assert!(
        !last.truncated && last.error.is_empty(),
        "{} passes did not complete the catch-up: {passes:#?}",
        passes.len()
    );
    let recorded: Vec<_> = ledger::tail(&anchor, 0, 1000)
        .unwrap()
        .into_iter()
        .filter(|event| transcript::ACTIONS.contains(&event.action.as_str()))
        .collect();
    assert_eq!(recorded.len(), 1, "the transcript was never ingested");
    let _ = linked;
}

/// The repository's address is a `git worktree list` over every checkout, and
/// the family does not change between passes seconds apart. The engine
/// remembers an authenticated family for as long as the repository's metadata
/// names the same checkouts, so a second pass spawns no listing, and a
/// worktree added in between is seen by the next one.
#[test]
fn the_repository_address_asks_git_once_and_again_after_a_worktree_is_added() {
    let (main, parent, _linked) = repo_with_worktrees(3);
    let anchor = plain(main.path());
    let dirs = [std::path::PathBuf::from(&anchor)];
    let listings = || git_count(&dirs, &["worktree", "list"]);

    let before = listings();
    ledger::bindings::repository_address(&anchor).expect("first address");
    ledger::bindings::repository_address(&anchor).expect("second address");
    assert_eq!(
        listings() - before,
        1,
        "two addresses asked git for the worktree list more than once"
    );

    let added = parent.path().join("wt-new");
    crate::test_support::git_in(
        main.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/new",
            added.to_str().unwrap(),
        ],
    );
    ledger::bindings::repository_address(&anchor).expect("address after the add");
    assert_eq!(
        listings() - before,
        2,
        "a new worktree did not make the address resolve again"
    );
}

/// A family past the per-pass reflog bound could never be read whole: every
/// pass reported the members past the bound as skipped, so no pass was ever
/// complete, however fast git was.
#[test]
fn a_twenty_worktree_family_is_read_whole_by_one_unhurried_pass() {
    let (main, _parent, _linked) = repo_with_worktrees(20);
    let anchor = plain(main.path());
    let out = with_empty_transcripts(|| catch_up_into_bounded(&anchor, &anchor, None));
    assert_eq!(out.reflog_refs_skipped, 0, "{out:?}");
    assert!(!out.truncated && out.error.is_empty(), "{out:?}");
}

/// A pass cut short after recording some refs must not record them again: the
/// watermark advances in the transaction that writes the rows, so the pass that
/// follows picks up only what the first never reached.
#[test]
fn a_pass_cut_short_leaves_its_recorded_refs_recorded_once() {
    let (main, _parent, linked) = repo_with_worktrees(12);
    let anchor = plain(main.path());
    for (index, worktree) in linked.iter().enumerate() {
        std::fs::write(worktree.join("note.txt"), index.to_string()).unwrap();
        crate::test_support::git_in(worktree, &["add", "."]);
        crate::test_support::git_in(worktree, &["commit", "-q", "-m", &format!("note {index}")]);
    }
    let siblings: Vec<String> = linked
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let family = Ok(siblings);

    let shim = slow_git_dir(150);
    let path = std::env::join_paths(
        std::iter::once(shim.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let first = {
        let serial = crate::test_support::env::env_serial();
        let _env = crate::test_support::env::bind_env(&serial).set("PATH", &path);
        let budget = Budget::within(std::time::Duration::from_millis(4000));
        ingest_reflogs(&anchor, &anchor, &family, MAX_REFLOG_WORKTREES, &budget)
    };
    assert!(
        first.truncated && first.reflog_refs_skipped > 0,
        "{first:?}"
    );
    assert!(
        first.recorded > 0,
        "the cut-short pass recorded nothing: {first:?}"
    );

    let second = ingest_reflogs(
        &anchor,
        &anchor,
        &family,
        MAX_REFLOG_WORKTREES,
        &Budget::unbounded(),
    );
    assert!(!second.truncated, "{second:?}");
    let rows = super::reflog_replay_tests::reflog_rows(&anchor).len();
    assert_eq!(
        usize::try_from(first.recorded + second.recorded).unwrap(),
        rows,
        "a ref was recorded twice or not at all: {first:?} {second:?}"
    );
}
