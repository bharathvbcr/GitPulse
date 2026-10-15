//! A repository that moves keeps its ledger and keeps recording; a ledger that
//! belongs to another repository is still refused.
//!
//! The ledger records which repository it belongs to (root commits and
//! remotes) while it is at home, and the first transcript pass after a move
//! takes the old path for an earlier location of the same repository. Every
//! refusal below is one of the threats the foreign-row guard exists for: a
//! ledger carried in from another repository, a clone, a fork, a rewritten
//! history. See `ledger::anchor` for the rule.

use super::event_time_tests::{call, stamp, with_transcript_root, write_corpus};
use super::*;

use crate::ledger::ids;
use crate::procguard::LockedSpawn;
use crate::test_support::{git_in, trust_repo};
use std::path::Path;

/// A trusted checkout of one commit (`message`) at `parent/name`, canonical.
fn repo_at(parent: &Path, name: &str, message: &str) -> String {
    let path = parent.canonicalize().unwrap().join(name);
    std::fs::create_dir(&path).unwrap();
    git_in(&path, &["init", "-b", "main"]);
    std::fs::write(path.join("seed.txt"), message).unwrap();
    git_in(&path, &["add", "seed.txt"]);
    git_in(&path, &["commit", "-m", message]);
    path.to_string_lossy().into_owned()
}

/// A trusted clone of `upstream` at `parent/name`.
fn clone_at(parent: &Path, upstream: &str, name: &str) -> String {
    let path = parent.canonicalize().unwrap().join(name);
    git_in(parent, &["clone", "-q", upstream, path.to_str().unwrap()]);
    trust_repo(&path);
    path.to_string_lossy().into_owned()
}

/// A bare upstream with one commit.
fn upstream_in(parent: &Path) -> String {
    let seed = repo_at(parent, "seed", "root");
    let bare = parent.canonicalize().unwrap().join("upstream.git");
    git_in(
        parent,
        &["clone", "-q", "--bare", &seed, bare.to_str().unwrap()],
    );
    bare.to_string_lossy().into_owned()
}

/// `mv from to`, trusting the new location as the user must in the app.
fn mv(from: &str, to: &Path) -> String {
    std::fs::rename(from, to).unwrap();
    trust_repo(to);
    to.to_string_lossy().into_owned()
}

/// `cp -R from to` for a repository with its ledger, trusted at `to`.
fn cp_r(from: &str, to: &Path) -> String {
    let out = std::process::Command::new("cp")
        .args(["-R", from, to.to_str().unwrap()])
        .output_locked()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    trust_repo(to);
    to.to_string_lossy().into_owned()
}

/// Puts a snapshot of `from`'s ledger where `to`'s ledger would be, as copying
/// one repository's `.devcouncil` into another does.
fn carry_ledger(from: &str, to: &str) {
    let state = Path::new(to).join(".devcouncil");
    std::fs::create_dir_all(&state).unwrap();
    let target = state.join("ledger.sqlite");
    let _ = std::fs::remove_file(&target);
    let conn = rusqlite::Connection::open(ledger::ledger_path(from)).unwrap();
    conn.execute("VACUUM INTO ?1", [target.to_str().unwrap()])
        .unwrap();
}

/// A transcript corpus that grows one call at a time, like a live agent's.
struct Corpus {
    dir: tempfile::TempDir,
    lines: Vec<String>,
    steps: u64,
}

impl Corpus {
    fn new() -> Self {
        Corpus {
            dir: tempfile::tempdir().unwrap(),
            lines: Vec::new(),
            steps: 0,
        }
    }

    /// One more edit made from `cwd`, dated after every earlier one and
    /// after any baseline a pass could have frozen. Returns the file edited.
    fn edit(&mut self, cwd: &str) -> String {
        self.steps += 1;
        let file = format!("{cwd}/edit-{}.rs", self.steps);
        let at = stamp(ids::now_millis() + 2_000 * self.steps);
        self.lines.push(call(cwd, &at, &file));
        write_corpus(self.dir.path(), &self.lines);
        file
    }

    fn pass(&self, repo: &str) -> CatchUp {
        with_transcript_root(self.dir.path(), || ingest_transcripts_into(repo, repo))
    }
}

/// The transcript rows of a ledger: `(ulid, object, repo_path)`.
fn transcript_rows(repo: &str) -> Vec<(String, String, String)> {
    ledger::tail(repo, 0, 1000)
        .unwrap()
        .into_iter()
        .filter(|event| event.action.starts_with("session."))
        .map(|event| {
            (
                event.ulid,
                event.object.unwrap_or_default(),
                event.repo_path,
            )
        })
        .collect()
}

/// Rows of the ledger file itself (not through the guard) with `action`.
fn count_action(repo: &str, action: &str) -> i64 {
    let conn = rusqlite::Connection::open(ledger::ledger_path(repo)).unwrap();
    conn.query_row(
        "SELECT count(*) FROM events WHERE action = ?1",
        [action],
        |r| r.get(0),
    )
    .unwrap()
}

fn assert_refused(out: &CatchUp, because: &str) {
    assert_eq!(out.recorded, 0, "a refused pass records nothing: {out:?}");
    assert!(out.reanchored_from.is_empty(), "{out:?}");
    assert!(out.error.contains("repository_mismatch"), "{out:?}");
    assert!(out.error.contains(because), "wanted {because:?}: {out:?}");
}

#[test]
fn a_renamed_repository_keeps_recording_with_no_loss_and_no_duplicate() {
    let parent = tempfile::tempdir().unwrap();
    let old = repo_at(parent.path(), "before", "seed");
    ledger::bindings::bind(&old, &old, "T1").unwrap();
    let mut corpus = Corpus::new();
    let before_file = corpus.edit(&old);
    let out = corpus.pass(&old);
    assert_eq!((out.recorded, out.error.as_str()), (1, ""), "{out:?}");
    assert!(out.reanchored_from.is_empty(), "nothing moved yet: {out:?}");
    let recorded_before = transcript_rows(&old);
    assert_eq!(recorded_before.len(), 1);

    let new = mv(&old, &parent.path().canonicalize().unwrap().join("after"));
    let after_file = corpus.edit(&new);
    let out = corpus.pass(&new);
    assert_eq!(out.error, "", "{out:?}");
    assert_eq!(
        out.recorded, 1,
        "the call made after the move is recorded: {out:?}"
    );
    assert_eq!(
        out.reanchored_from,
        vec![old.clone()],
        "the move is reported: {out:?}"
    );

    let rows = transcript_rows(&new);
    assert_eq!(
        rows.len(),
        2,
        "one row before the move, one after: {rows:?}"
    );
    for file in [&before_file, &after_file] {
        let named = rows.iter().filter(|(_, object, _)| object == file).count();
        assert_eq!(named, 1, "{file} is neither lost nor duplicated: {rows:?}");
    }
    assert_eq!(
        rows[0].0, recorded_before[0].0,
        "the pre-move row keeps its identity"
    );
    assert!(
        rows.iter().all(|(_, _, repo)| *repo == new),
        "reads present the new path: {rows:?}"
    );
    // History stays honest about where it was written.
    let stored: Vec<String> = {
        let conn = rusqlite::Connection::open(ledger::ledger_path(&new)).unwrap();
        let mut stmt = conn
            .prepare("SELECT DISTINCT repo_path FROM events WHERE action LIKE 'session.%'")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        stored.len(),
        2,
        "rows keep the path they were written under: {stored:?}"
    );

    // Reported once: later passes, a new call included, say nothing more.
    let third_file = corpus.edit(&new);
    for expected in [1, 0, 0] {
        let out = corpus.pass(&new);
        assert_eq!(
            (out.recorded, out.error.as_str()),
            (expected, ""),
            "{out:?}"
        );
        assert!(out.reanchored_from.is_empty(), "reported again: {out:?}");
    }
    assert_eq!(count_action(&new, "ledger.reanchor"), 1);
    let rows = transcript_rows(&new);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows.iter().any(|(_, object, _)| *object == third_file));
}

/// The identity record is written once and only rewritten when the repository
/// grew a fact the record lacks; passes in between add nothing to the ledger.
#[test]
fn the_identity_record_is_written_once_and_refreshed_only_when_it_changes() {
    let parent = tempfile::tempdir().unwrap();
    let repo = repo_at(parent.path(), "repo", "seed");
    let mut corpus = Corpus::new();
    for _ in 0..3 {
        corpus.edit(&repo);
        assert_eq!(corpus.pass(&repo).recorded, 1);
    }
    assert_eq!(count_action(&repo, "ledger.anchor"), 1);
    git_in(
        Path::new(&repo),
        &[
            "remote",
            "add",
            "origin",
            "https://example.test/acme/repo.git",
        ],
    );
    ledger::forget_identity_state(&repo);
    corpus.edit(&repo);
    assert_eq!(corpus.pass(&repo).recorded, 1);
    assert_eq!(count_action(&repo, "ledger.anchor"), 2, "the new remote");
    ledger::forget_identity_state(&repo);
    corpus.edit(&repo);
    corpus.pass(&repo);
    assert_eq!(
        count_action(&repo, "ledger.anchor"),
        2,
        "nothing new to record"
    );
}

#[test]
fn a_ledger_from_a_different_repository_is_still_refused() {
    let parent = tempfile::tempdir().unwrap();
    let a = repo_at(parent.path(), "a", "alpha");
    let b = repo_at(parent.path(), "b", "beta");
    let mut corpus = Corpus::new();
    corpus.edit(&a);
    assert_eq!(corpus.pass(&a).recorded, 1);
    carry_ledger(&a, &b);
    // Even with the source gone, the roots differ: only the rule can refuse it.
    std::fs::remove_dir_all(&a).unwrap();
    corpus.edit(&b);
    let out = corpus.pass(&b);
    assert_refused(&out, "root commit");
    assert_eq!(count_action(&b, "session.edit"), 1, "nothing was added");
    assert_eq!(count_action(&b, "ledger.reanchor"), 0);
}

/// Threat: a copy of a clone's ledger, with the clone still there, is not a
/// move: it would attribute that clone's agent activity to this one.
#[test]
fn a_ledger_copied_from_a_live_clone_is_refused_until_the_source_is_gone() {
    let parent = tempfile::tempdir().unwrap();
    let upstream = upstream_in(parent.path());
    let first = clone_at(parent.path(), &upstream, "first");
    let second = clone_at(parent.path(), &upstream, "second");
    let mut corpus = Corpus::new();
    corpus.edit(&first);
    assert_eq!(corpus.pass(&first).recorded, 1);
    carry_ledger(&first, &second);

    corpus.edit(&second);
    let out = corpus.pass(&second);
    assert_refused(&out, "copy or another clone");
    assert_eq!(count_action(&second, "ledger.reanchor"), 0);

    // The honest residual, pinned: two clones of one upstream have the same
    // root commits and remotes, so once the source is gone the carried ledger
    // reads as a move. Nothing in a common dir tells the two apart.
    std::fs::remove_dir_all(&first).unwrap();
    ledger::forget_identity_state(&second);
    let out = corpus.pass(&second);
    assert_eq!(out.reanchored_from, vec![first], "{out:?}");
}

/// Threat: a fork shares the root commit and points `origin` elsewhere.
#[test]
fn a_ledger_carried_into_a_fork_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let upstream = upstream_in(parent.path());
    let original = clone_at(parent.path(), &upstream, "original");
    let fork = clone_at(parent.path(), &upstream, "fork");
    git_in(
        Path::new(&fork),
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.test/someone/fork.git",
        ],
    );
    let mut corpus = Corpus::new();
    corpus.edit(&original);
    assert_eq!(corpus.pass(&original).recorded, 1);
    carry_ledger(&original, &fork);
    std::fs::remove_dir_all(&original).unwrap();
    corpus.edit(&fork);
    assert_refused(&corpus.pass(&fork), "remotes");
}

/// Threat: a repository whose root commit was rewritten is another history.
#[test]
fn a_repository_whose_root_was_rewritten_after_the_move_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let old = repo_at(parent.path(), "old", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&old);
    assert_eq!(corpus.pass(&old).recorded, 1);
    let new = mv(&old, &parent.path().canonicalize().unwrap().join("new"));
    git_in(
        Path::new(&new),
        &["commit", "--amend", "-m", "rewritten root"],
    );
    corpus.edit(&new);
    assert_refused(&corpus.pass(&new), "root commit");
}

/// A repository with no commit has no root to identify it by: refused when it
/// holds a ledger of another path, and unharmed when the ledger is its own.
#[test]
fn a_repository_with_no_commit_cannot_take_over_a_ledger() {
    let parent = tempfile::tempdir().unwrap();
    let source = repo_at(parent.path(), "source", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&source);
    assert_eq!(corpus.pass(&source).recorded, 1);

    let empty = parent.path().canonicalize().unwrap().join("empty");
    std::fs::create_dir(&empty).unwrap();
    git_in(&empty, &["init", "-b", "main"]);
    let empty = empty.to_string_lossy().into_owned();
    corpus.edit(&empty);
    let own = corpus.pass(&empty);
    assert_eq!(
        (own.recorded, own.error.as_str()),
        (1, ""),
        "its own ledger works: {own:?}"
    );
    assert_eq!(
        count_action(&empty, "ledger.anchor"),
        0,
        "no root, nothing to record"
    );

    let carried = parent.path().canonicalize().unwrap().join("carried");
    std::fs::create_dir(&carried).unwrap();
    git_in(&carried, &["init", "-b", "main"]);
    let carried = carried.to_string_lossy().into_owned();
    carry_ledger(&source, &carried);
    std::fs::remove_dir_all(&source).unwrap();
    corpus.edit(&carried);
    assert_refused(&corpus.pass(&carried), "identity could not be read");
}

/// A ledger that never recorded who it belongs to (written before this, or
/// never given a pass) cannot be told from a carried one, so a move leaves it
/// refused. Fail closed, and say why.
#[test]
fn a_ledger_that_recorded_no_identity_is_refused_after_a_move() {
    let parent = tempfile::tempdir().unwrap();
    let old = repo_at(parent.path(), "old", "seed");
    ledger::bindings::bind(&old, &old, "T1").unwrap();
    let new = mv(&old, &parent.path().canonicalize().unwrap().join("new"));
    let mut corpus = Corpus::new();
    corpus.edit(&new);
    assert_refused(&corpus.pass(&new), "records no repository identity");
}

/// After a legitimate re-anchor the guard accepts only the listed earlier
/// locations: a row naming any other path refuses reads again, and nothing
/// but a pass that passes the whole rule takes it over.
#[test]
fn a_row_naming_an_unlisted_path_after_a_reanchor_is_foreign() {
    let parent = tempfile::tempdir().unwrap();
    let old = repo_at(parent.path(), "old", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&old);
    corpus.pass(&old);
    let new = mv(&old, &parent.path().canonicalize().unwrap().join("new"));
    corpus.edit(&new);
    assert_eq!(corpus.pass(&new).reanchored_from, vec![old]);
    assert!(ledger::tail(&new, 0, 1000).is_ok());

    let conn = rusqlite::Connection::open(ledger::ledger_path(&new)).unwrap();
    conn.execute(
        "INSERT INTO events (ulid, ts_utc, repo_path, actor_kind, action, outcome)
         VALUES ('01HZZZZZZZZZZZZZZZZZZZZZZZ', '2026-01-01T00:00:00.000Z', '/elsewhere',
                 'human', 'git.commit', 'ok')",
        [],
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        ledger::tail(&new, 0, 1000).unwrap_err().code,
        "repository_mismatch"
    );
    assert!(ledger::bindings::resolve(&new, &new).is_err());
}

#[test]
fn moving_back_and_moving_twice_are_each_reported_once() {
    let parent = tempfile::tempdir().unwrap();
    let base = parent.path().canonicalize().unwrap();
    let one = repo_at(parent.path(), "one", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&one);
    assert_eq!(corpus.pass(&one).recorded, 1);

    let two = mv(&one, &base.join("two"));
    corpus.edit(&two);
    let out = corpus.pass(&two);
    assert_eq!(
        (out.recorded, out.reanchored_from.clone()),
        (1, vec![one.clone()]),
        "{out:?}"
    );

    let three = mv(&two, &base.join("three"));
    corpus.edit(&three);
    let out = corpus.pass(&three);
    assert_eq!(out.recorded, 1, "{out:?}");
    let mut listed = out.reanchored_from.clone();
    listed.sort();
    let mut expected = vec![one.clone(), two.clone()];
    expected.sort();
    assert_eq!(listed, expected, "{out:?}");

    // Back to where it began, in the same process that saw it there.
    let back = mv(&three, &base.join("one"));
    assert_eq!(back, one);
    corpus.edit(&back);
    let out = corpus.pass(&back);
    assert_eq!((out.recorded, out.error.as_str()), (1, ""), "{out:?}");
    let mut listed = out.reanchored_from.clone();
    listed.sort();
    let mut expected = vec![three, two];
    expected.sort();
    assert_eq!(listed, expected, "{out:?}");

    let rows = transcript_rows(&back);
    let mut objects: Vec<&str> = rows.iter().map(|(_, o, _)| o.as_str()).collect();
    objects.sort_unstable();
    objects.dedup();
    assert_eq!(
        rows.len(),
        4,
        "four calls, four rows, no duplicate: {rows:?}"
    );
    assert_eq!(objects.len(), 4);
    assert_eq!(count_action(&back, "ledger.reanchor"), 3);
}

#[test]
fn a_symlink_left_at_the_old_path_does_not_make_it_look_occupied() {
    let parent = tempfile::tempdir().unwrap();
    let base = parent.path().canonicalize().unwrap();
    let old = repo_at(parent.path(), "old", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&old);
    corpus.pass(&old);
    let new = mv(&old, &base.join("new"));
    std::os::unix::fs::symlink(&new, &old).unwrap();
    corpus.edit(&old);
    // The pass is asked for through the symlink and keys on the real path.
    let out = corpus.pass(&old);
    assert_eq!(out.error, "", "{out:?}");
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.reanchored_from, vec![old], "{out:?}");
}

/// `cp -R` with the original still in place: the original keeps its ledger and
/// keeps recording, the copy is refused and left exactly as it was.
#[test]
fn a_copy_beside_a_live_original_is_refused_and_the_original_is_unaffected() {
    let parent = tempfile::tempdir().unwrap();
    let original = repo_at(parent.path(), "original", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&original);
    assert_eq!(corpus.pass(&original).recorded, 1);
    let copy = cp_r(
        &original,
        &parent.path().canonicalize().unwrap().join("copy"),
    );
    let before = count_action(&copy, "session.edit");

    corpus.edit(&copy);
    assert_refused(&corpus.pass(&copy), "copy or another clone");
    assert_eq!(
        count_action(&copy, "session.edit"),
        before,
        "the copy gained nothing"
    );
    assert_eq!(count_action(&copy, "ledger.reanchor"), 0);

    corpus.edit(&original);
    let out = corpus.pass(&original);
    assert_eq!((out.recorded, out.error.as_str()), (1, ""), "{out:?}");
    assert!(out.reanchored_from.is_empty());
    assert_eq!(count_action(&original, "session.edit"), before + 1);
}

/// Remotes corroborate and only contradict: one added after the move is fine,
/// all replaced is a fork.
#[test]
fn a_remote_added_after_the_move_is_fine_and_all_replaced_is_not() {
    for (replace, accepted) in [(false, true), (true, false)] {
        let parent = tempfile::tempdir().unwrap();
        let old = repo_at(parent.path(), "old", "seed");
        git_in(
            Path::new(&old),
            &[
                "remote",
                "add",
                "origin",
                "https://example.test/acme/repo.git",
            ],
        );
        let mut corpus = Corpus::new();
        corpus.edit(&old);
        assert_eq!(corpus.pass(&old).recorded, 1);
        let new = mv(&old, &parent.path().canonicalize().unwrap().join("new"));
        let new_path = Path::new(&new);
        if replace {
            git_in(new_path, &["remote", "remove", "origin"]);
        }
        git_in(
            new_path,
            &[
                "remote",
                "add",
                "other",
                "https://example.test/else/where.git",
            ],
        );
        corpus.edit(&new);
        let out = corpus.pass(&new);
        if accepted {
            assert_eq!(out.reanchored_from, vec![old], "{out:?}");
            assert_eq!(out.recorded, 1, "{out:?}");
        } else {
            assert_refused(&out, "remotes");
        }
    }
}

/// A linked worktree of a moved repository records into the moved ledger.
#[test]
fn a_linked_worktree_of_a_moved_repository_records_into_the_taken_over_ledger() {
    let parent = tempfile::tempdir().unwrap();
    let base = parent.path().canonicalize().unwrap();
    let old = repo_at(parent.path(), "old", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&old);
    assert_eq!(corpus.pass(&old).recorded, 1);
    let new = mv(&old, &base.join("new"));
    let linked = base.join("linked");
    git_in(
        Path::new(&new),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            linked.to_str().unwrap(),
        ],
    );
    trust_repo(&linked);
    let linked = crate::engine::git_cli::canonicalize_plain(&linked).unwrap();
    let linked = linked.to_string_lossy().into_owned();
    let file = corpus.edit(&linked);
    let out = with_transcript_root(corpus.dir.path(), || ingest_transcripts_into(&new, &linked));
    assert_eq!(out.error, "", "{out:?}");
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.reanchored_from, vec![old], "{out:?}");
    let rows = ledger::tail(&new, 0, 1000).unwrap();
    let row = rows
        .iter()
        .find(|e| e.object.as_deref() == Some(file.as_str()))
        .unwrap();
    assert_eq!(row.worktree_path.as_deref(), Some(linked.as_str()));
}

/// Several passes meet the first open after the move together: one takes the
/// ledger over and reports it, nobody records a call twice.
#[test]
fn concurrent_first_passes_after_a_move_reanchor_once() {
    let parent = tempfile::tempdir().unwrap();
    let old = repo_at(parent.path(), "old", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&old);
    assert_eq!(corpus.pass(&old).recorded, 1);
    let new = mv(&old, &parent.path().canonicalize().unwrap().join("new"));
    let file = corpus.edit(&new);

    let outs: Vec<CatchUp> = with_transcript_root(corpus.dir.path(), || {
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        ingest_transcripts_into(&new, &new)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    });
    let reports: usize = outs.iter().map(|o| o.reanchored_from.len()).sum();
    assert_eq!(reports, 1, "{outs:?}");
    assert!(outs.iter().all(|o| o.error.is_empty()), "{outs:?}");
    let rows = transcript_rows(&new);
    assert_eq!(
        rows.iter().filter(|(_, o, _)| *o == file).count(),
        1,
        "{rows:?}"
    );
    assert_eq!(count_action(&new, "ledger.reanchor"), 1);
}

/// A shallow trusted clone (depth 1) of a two-commit repository.
fn shallow_clone_at(parent: &Path, name: &str) -> String {
    let source = repo_at(parent, &format!("{name}-src"), "one");
    std::fs::write(Path::new(&source).join("two.txt"), "two").unwrap();
    git_in(Path::new(&source), &["add", "two.txt"]);
    git_in(Path::new(&source), &["commit", "-m", "two"]);
    let path = parent.canonicalize().unwrap().join(name);
    git_in(
        parent,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{source}"),
            path.to_str().unwrap(),
        ],
    );
    trust_repo(&path);
    path.to_string_lossy().into_owned()
}

/// Threat (pinned, not endorsed): a different repository that merged this
/// one's whole history has every root this ledger recorded, so once the source
/// is gone the carried ledger reads as a move. The rule says "recorded roots
/// are still roots": it cannot tell a merged-in history from a root added by an
/// orphan branch, and refusing the first refuses the second.
#[test]
fn a_repository_that_merged_the_source_history_takes_the_carried_ledger() {
    let parent = tempfile::tempdir().unwrap();
    let a = repo_at(parent.path(), "a", "alpha");
    let mut corpus = Corpus::new();
    corpus.edit(&a);
    assert_eq!(corpus.pass(&a).recorded, 1);

    let b = parent.path().canonicalize().unwrap().join("b");
    std::fs::create_dir(&b).unwrap();
    git_in(&b, &["init", "-b", "main"]);
    std::fs::write(b.join("other.txt"), "b").unwrap();
    git_in(&b, &["add", "other.txt"]);
    git_in(&b, &["commit", "-m", "beta"]);
    git_in(&b, &["fetch", "-q", &a, "main:from-a"]);
    git_in(
        &b,
        &[
            "merge",
            "-q",
            "--allow-unrelated-histories",
            "-m",
            "fold",
            "from-a",
        ],
    );
    trust_repo(&b);
    let b = b.to_string_lossy().into_owned();
    carry_ledger(&a, &b);
    std::fs::remove_dir_all(&a).unwrap();
    corpus.edit(&b);
    let out = corpus.pass(&b);
    assert_eq!(out.reanchored_from, vec![a], "{out:?}");
}

/// A remote carrying a credential in any spelling is never written to the
/// ledger, in the identity records or anywhere else.
#[test]
fn no_remote_credential_reaches_the_ledger() {
    let parent = tempfile::tempdir().unwrap();
    let repo = repo_at(parent.path(), "repo", "seed");
    let dir = Path::new(&repo);
    git_in(
        dir,
        &[
            "remote",
            "add",
            "origin",
            "https://user:SECRETtok@host.test/a/b.git?token=SECRETtok#SECRETtok",
        ],
    );
    git_in(
        dir,
        &["remote", "add", "up.stream", "SECRETtok@host2.test:a/b.git"],
    );
    git_in(
        dir,
        &[
            "remote",
            "add",
            "v6",
            "ssh://git:SECRETtok@[::1]:2222/a/b.git",
        ],
    );
    git_in(
        dir,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "https://SECRETtok@push.test/a",
        ],
    );
    git_in(
        dir,
        &[
            "config",
            "url.https://SECRETtok@rewrite.test/.insteadOf",
            "rw:",
        ],
    );
    let mut corpus = Corpus::new();
    corpus.edit(&repo);
    assert_eq!(corpus.pass(&repo).recorded, 1);
    let conn = rusqlite::Connection::open(ledger::ledger_path(&repo)).unwrap();
    let mut stmt = conn.prepare("SELECT * FROM events").unwrap();
    let width = stmt.column_count();
    let mut rows = stmt.query([]).unwrap();
    let mut anchors = 0;
    while let Some(row) = rows.next().unwrap() {
        for column in 0..width {
            let text = match row.get_ref(column).unwrap() {
                rusqlite::types::ValueRef::Text(bytes) => {
                    String::from_utf8_lossy(bytes).to_string()
                }
                _ => continue,
            };
            assert!(
                !text.to_lowercase().contains("secrettok"),
                "column {column} holds a credential: {text}"
            );
        }
        anchors += 1;
    }
    assert!(anchors >= 2, "the anchor row and the transcript row exist");
}

/// Remotes with a credential or query in them are compared as recorded, so a
/// moved repository whose remote carries one is still taken over, and a fork
/// behind a different such remote is still not.
#[test]
fn a_remote_with_a_credential_in_it_still_identifies_a_moved_repository() {
    let parent = tempfile::tempdir().unwrap();
    let repo = repo_at(parent.path(), "repo", "seed");
    let remote = "https://user:SECRETtok@host.test/a/b.git?token=SECRETtok#frag";
    git_in(Path::new(&repo), &["remote", "add", "origin", remote]);
    let mut corpus = Corpus::new();
    corpus.edit(&repo);
    assert_eq!(corpus.pass(&repo).recorded, 1);
    let moved = mv(&repo, &parent.path().canonicalize().unwrap().join("moved"));
    corpus.edit(&moved);
    let out = corpus.pass(&moved);
    assert_eq!((out.recorded, out.error.as_str()), (1, ""), "{out:?}");
    assert_eq!(out.reanchored_from, vec![repo.clone()], "{out:?}");

    let other = repo_at(parent.path(), "other", "seed");
    git_in(
        Path::new(&other),
        &[
            "remote",
            "add",
            "origin",
            "https://host.test/a/b.git?token=SECRETtok",
        ],
    );
    corpus.edit(&other);
    assert_eq!(corpus.pass(&other).recorded, 1);
    let moved = mv(
        &other,
        &parent.path().canonicalize().unwrap().join("other-moved"),
    );
    git_in(
        Path::new(&moved),
        &[
            "remote",
            "set-url",
            "origin",
            "https://host.test/a/other.git?token=SECRETtok",
        ],
    );
    corpus.edit(&moved);
    assert_refused(&corpus.pass(&moved), "remotes");
}

/// Threat to availability: unshallowing a clone that did not move changes its
/// roots. The ledger is at home, so nothing is refused, and the record follows.
#[test]
fn unshallowing_a_repository_that_did_not_move_keeps_recording() {
    let parent = tempfile::tempdir().unwrap();
    let repo = shallow_clone_at(parent.path(), "shallow");
    let mut corpus = Corpus::new();
    corpus.edit(&repo);
    assert_eq!(corpus.pass(&repo).recorded, 1);
    git_in(Path::new(&repo), &["fetch", "-q", "--unshallow"]);
    ledger::forget_identity_state(&repo);
    corpus.edit(&repo);
    let out = corpus.pass(&repo);
    assert_eq!((out.recorded, out.error.as_str()), (1, ""), "{out:?}");
    assert_eq!(count_action(&repo, "ledger.anchor"), 2, "the roots changed");
}

/// A history rewrite that changes the root, with no move, is not a reason to
/// refuse the repository its own ledger.
#[test]
fn a_rewritten_root_without_a_move_keeps_recording() {
    let parent = tempfile::tempdir().unwrap();
    let repo = repo_at(parent.path(), "repo", "seed");
    let mut corpus = Corpus::new();
    corpus.edit(&repo);
    assert_eq!(corpus.pass(&repo).recorded, 1);
    git_in(
        Path::new(&repo),
        &["commit", "--amend", "-m", "rewritten root"],
    );
    ledger::forget_identity_state(&repo);
    corpus.edit(&repo);
    let out = corpus.pass(&repo);
    assert_eq!((out.recorded, out.error.as_str()), (1, ""), "{out:?}");
    // And the rewritten identity is the one a later move is judged against.
    let moved = mv(&repo, &parent.path().canonicalize().unwrap().join("moved"));
    corpus.edit(&moved);
    let out = corpus.pass(&moved);
    assert_eq!(out.reanchored_from, vec![repo], "{out:?}");
}

/// A shallow repository that moves is the same repository; one unshallowed in
/// the same breath has other roots than the ledger recorded and is refused
/// (fail closed: the roots are what identify it).
#[test]
fn a_moved_shallow_repository_is_taken_over_unless_it_was_unshallowed_too() {
    let parent = tempfile::tempdir().unwrap();
    let mut corpus = Corpus::new();

    let plain = shallow_clone_at(parent.path(), "plain");
    corpus.edit(&plain);
    assert_eq!(corpus.pass(&plain).recorded, 1);
    let moved = mv(
        &plain,
        &parent.path().canonicalize().unwrap().join("plain-moved"),
    );
    corpus.edit(&moved);
    let out = corpus.pass(&moved);
    assert_eq!(out.reanchored_from, vec![plain], "{out:?}");

    let deep = shallow_clone_at(parent.path(), "deep");
    corpus.edit(&deep);
    assert_eq!(corpus.pass(&deep).recorded, 1);
    let moved = mv(
        &deep,
        &parent.path().canonicalize().unwrap().join("deep-moved"),
    );
    git_in(Path::new(&moved), &["fetch", "-q", "--unshallow"]);
    corpus.edit(&moved);
    assert_refused(&corpus.pass(&moved), "root commit");
}
