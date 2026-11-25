//! Why the ledger's catch-up never finished on a repository with two dozen
//! worktrees: one five-second budget for the address, the reflogs and the
//! transcripts, retried every ten seconds for ever, from the reactive class.

use super::*;
use crate::engine::process::{spawn_log, Admission};
use crate::test_support::repo_with_worktrees;

/// Serializes the heavy multi-worktree catch-up tests in this module so they do
/// not saturate the shared `SpawnGate` background bucket concurrently while
/// leaving other test modules unblocked.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn plain(path: &std::path::Path) -> String {
    crate::engine::process::canonicalize_plain(path)
        .expect("canonical")
        .to_string_lossy()
        .into_owned()
}

/// `dir` in the spelling a pass spawns its git children under. The repository
/// address names every checkout by its canonical path (`\\?\C:\...` on
/// Windows), and the spawn log files a child under the directory it was given.
pub(super) fn as_spawned(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Runs `f` with the transcript root pointed at an empty corpus on this thread.
fn with_empty_transcripts<T>(f: impl FnOnce() -> T) -> T {
    let corpus = tempfile::tempdir().expect("corpus");
    test_transcript_root::with(corpus.path(), f)
}

/// Every git child the gate started in `dirs`, with the class it was admitted
/// under.
fn classed_git_children(dirs: &[std::path::PathBuf]) -> Vec<(Vec<String>, Admission)> {
    dirs.iter()
        .flat_map(|dir| spawn_log::classed_spawns_in(&as_spawned(dir)))
        .collect()
}

/// A background catch-up must not spend the reactive budget the interface
/// reads from: the app asks for it on every tab switch, and a pass over a large
/// family starts dozens of children.
#[test]
fn a_catch_up_pass_starts_its_git_children_in_the_background_class() {
    let _serial = serial();
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
/// bug in how a pass spends its budget. The shim is a `/bin/sh` script, so it
/// and the tests that run git through it are Unix only.
#[cfg(unix)]
fn slow_git_dir(millis: u64) -> tempfile::TempDir {
    slow_git_dir_for(millis, &[])
}

/// [`slow_git_dir`] for the git subcommands in `verbs` only (every one when
/// empty): a lookup that costs several of them can be made slow while the
/// reflog reads stay fast.
#[cfg(unix)]
fn slow_git_dir_for(millis: u64, verbs: &[&str]) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let real = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("a git on PATH");
    let dir = tempfile::tempdir().expect("shim dir");
    let shim = dir.path().join("git");
    let seconds = millis as f64 / 1000.0;
    let delay = if verbs.is_empty() {
        format!("sleep {seconds}\n")
    } else {
        let patterns: Vec<String> = verbs.iter().map(|verb| format!("*\" {verb} \"*")).collect();
        format!(
            "case \" $* \" in {}) sleep {seconds} ;; esac\n",
            patterns.join("|")
        )
    };
    std::fs::write(
        &shim,
        format!("#!/bin/sh\n{delay}exec \"{}\" \"$@\"\n", real.display()),
    )
    .expect("write the shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    dir
}

/// A gate that answers nothing from the last pass, so passes run back to back.
#[cfg(unix)]
fn eager_gate() -> flight::Gate {
    flight::Gate::new(flight::Intervals {
        complete: std::time::Duration::ZERO,
        incomplete: std::time::Duration::ZERO,
        incomplete_cap: std::time::Duration::ZERO,
    })
}

fn git_count(dirs: &[std::path::PathBuf], verb: &[&str]) -> usize {
    dirs.iter()
        .flat_map(|dir| spawn_log::spawns_in(&as_spawned(dir)))
        .filter(|argv| verb.iter().all(|word| argv.iter().any(|arg| arg == word)))
        .count()
}

/// The production report: 22 worktrees, a host that takes a good fraction of a
/// second to start any process, and a catch-up that stayed `truncated` for 39
/// hours. One pass cannot read every reflog in five seconds, so the passes
/// must add up: each resumes where the last stopped, the transcripts get their
/// own share of the budget rather than what the reflogs leave, and the pass
/// that reads the last reflog is complete.
#[cfg(unix)]
#[test]
fn passes_over_a_slow_git_and_twenty_worktrees_add_up_to_a_completed_catch_up() {
    let _serial = serial();
    let (main, _parent, linked) = repo_with_worktrees(20);
    let anchor = plain(main.path());
    let corpus = one_transcript_corpus(&anchor);

    let shim = slow_git_dir(150);
    let path = std::env::join_paths(
        std::iter::once(shim.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();

    let gate = eager_gate();
    let mut passes = Vec::new();
    test_transcript_root::with(corpus.path(), || {
        crate::engine::git_cli::with_test_git_path(&path, || {
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
        });
    });
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

/// A transcript root holding one session with one edit under `anchor`.
#[cfg(unix)]
fn one_transcript_corpus(anchor: &str) -> tempfile::TempDir {
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
    corpus
}

/// The default branch is found with several git processes (the config, the
/// remote's HEAD, the candidate branches). On a cold cache, on a host where
/// each process takes over a second to start, that lookup alone outlasts a
/// pass, and it ran before the reflogs' slice was cut: it spent the
/// transcripts' share, and no transcript was read while the cache was cold.
/// It now runs after the transcripts, the next passes read the branch it named,
/// and the catch-up completes.
#[cfg(unix)]
#[test]
fn a_slow_default_branch_lookup_does_not_spend_the_transcripts_share() {
    let _serial = serial();
    let (main, _parent, linked) = repo_with_worktrees(2);
    let anchor = plain(main.path());
    let corpus = one_transcript_corpus(&anchor);
    // Five lookups at this cost are well past the five-second pass; the reflog
    // reads and the address are not slowed.
    let shim = slow_git_dir_for(1500, &["config", "symbolic-ref"]);
    let path = std::env::join_paths(
        std::iter::once(shim.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();

    let gate = eager_gate();
    let mut passes = Vec::new();
    test_transcript_root::with(corpus.path(), || {
        crate::engine::git_cli::with_test_git_path(&path, || {
            let first = catch_up_through(&gate, &anchor);
            assert!(
                first.transcripts > 0,
                "the default-branch lookup spent the transcripts' share: {first:?}"
            );
            passes.push(first);
            for _ in 0..8 {
                let last = passes.last().unwrap();
                if !last.truncated && last.error.is_empty() {
                    break;
                }
                passes.push(catch_up_through(&gate, &anchor));
            }
        });
    });
    let last = passes.last().unwrap();
    assert!(
        !last.truncated && last.error.is_empty(),
        "{} passes did not complete the catch-up: {passes:#?}",
        passes.len()
    );
    let default_reads = git_count(
        &[std::path::PathBuf::from(&anchor)],
        &["reflog", "refs/heads/main"],
    );
    assert!(
        default_reads > 0,
        "the default branch's reflog was never read: {passes:#?}"
    );
    let _ = linked;
}

/// The repository's address is a `git worktree list` over every checkout, and
/// the family does not change between passes seconds apart. The engine
/// remembers an authenticated family for as long as the repository's metadata
/// names the same checkouts, so a second pass spawns no listing, and a
/// worktree added in between is seen by the next one.
#[test]
fn the_repository_address_asks_git_once_and_again_after_a_worktree_is_added() {
    let _serial = serial();
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
    let _serial = serial();
    let (main, _parent, _linked) = repo_with_worktrees(20);
    let anchor = plain(main.path());
    let out = with_empty_transcripts(|| catch_up_into_bounded(&anchor, &anchor, None));
    assert_eq!(out.reflog_refs_skipped, 0, "{out:?}");
    assert!(!out.truncated && out.error.is_empty(), "{out:?}");
}

/// A pass cut short after recording some refs must not record them again: the
/// watermark advances in the transaction that writes the rows, so the pass that
/// follows picks up only what the first never reached.
#[cfg(unix)]
#[test]
fn a_pass_cut_short_leaves_its_recorded_refs_recorded_once() {
    let _serial = serial();
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
    let first = crate::engine::git_cli::with_test_git_path(&path, || {
        let budget = Budget::within(std::time::Duration::from_millis(4000));
        ingest_reflogs(&anchor, &anchor, &family, MAX_REFLOG_WORKTREES, &budget)
    });
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

/// The spawn gate sheds Background work while the host is under pressure or
/// the spawn budget is low: git never starts, so the pass has read nothing and
/// learned nothing about the repository. That is a pass cut short, to be
/// retried, and not one that failed. The predicate is the one the binding
/// path applies (`commands::audit::is_partial`): truncated, a backlog counted,
/// no failed write, and every part of `error` a "catch-up truncated: " note.
#[test]
fn a_pass_whose_spawns_are_shed_ends_truncated_and_is_not_a_failure() {
    let _serial = serial();
    use crate::sysload::{GovernorInput, Level};
    let (main, _parent, linked) = repo_with_worktrees(3);
    let anchor = plain(main.path());
    // One clean pass first, so the ledger, the family and the default branch
    // are all warm and the shed lands on the reads of the next pass.
    with_empty_transcripts(|| {
        let warm = catch_up_into_bounded(&anchor, &anchor, None);
        assert!(!warm.truncated && warm.error.is_empty(), "{warm:?}");
    });
    for (index, worktree) in linked.iter().enumerate() {
        std::fs::write(worktree.join("note.txt"), index.to_string()).unwrap();
        crate::test_support::git_in(worktree, &["add", "."]);
        crate::test_support::git_in(worktree, &["commit", "-q", "-m", &format!("note {index}")]);
    }
    let shed = with_empty_transcripts(|| {
        let _pressed =
            crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
        crate::engine::process::with_background_processes(|| {
            catch_up_into_bounded(&anchor, &anchor, None)
        })
    });
    assert!(shed.truncated, "{shed:?}");
    assert_eq!(shed.failed_records, 0, "{shed:?}");
    assert!(
        shed.reflog_refs_skipped > 0,
        "no backlog was counted: {shed:?}"
    );
    assert!(
        !shed.finished(),
        "a shed pass must not read as complete: {shed:?}"
    );
    assert!(
        shed.error
            .split("; ")
            .all(|part| part.starts_with("catch-up truncated: ")),
        "a shed pass reported a failure, not a deferral: {:?}",
        shed.error
    );

    let next = with_empty_transcripts(|| catch_up_into_bounded(&anchor, &anchor, None));
    assert!(!next.truncated && next.error.is_empty(), "{next:?}");
    assert!(
        next.recorded >= 3,
        "the shed pass lost what the next one should record: {next:?}"
    );
}

/// The gate backs off a pass that is stuck and keeps the base interval for one
/// that is converging. A pass that fails for its own reasons (here a ledger
/// that refuses a row) over refs that have not moved reads the same refs every
/// time: that is not progress, however many reads it did cleanly.
#[test]
fn a_pass_that_keeps_failing_over_unchanged_refs_is_not_progress() {
    let _serial = serial();
    let (main, _parent, linked) = repo_with_worktrees(2);
    let anchor = plain(main.path());
    with_empty_transcripts(|| {
        let first =
            catch_up_into_bounded(&anchor, &anchor, Some(std::time::Duration::from_secs(60)));
        assert!(first.error.is_empty(), "{first:?}");
        std::fs::write(linked[0].join("boom.txt"), "x").unwrap();
        crate::test_support::git_in(&linked[0], &["add", "."]);
        crate::test_support::git_in(&linked[0], &["commit", "-q", "-m", "boom"]);
        let db = main.path().join(".devcouncil").join("ledger.sqlite");
        let conn = rusqlite::Connection::open(&db).expect("open the ledger file");
        conn.execute_batch(
            "CREATE TRIGGER inject_failure BEFORE INSERT ON events
             WHEN NEW.object LIKE '%boom%'
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .expect("inject the failure");
        for round in 0..2 {
            let out =
                catch_up_into_bounded(&anchor, &anchor, Some(std::time::Duration::from_secs(60)));
            assert!(out.failed_records > 0, "round {round}: {out:?}");
            assert!(
                !out.made_progress(),
                "round {round}: a pass stuck on a failed write counted as progress: {out:?}"
            );
        }
    });
}
