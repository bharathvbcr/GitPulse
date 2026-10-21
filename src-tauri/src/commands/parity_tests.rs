//! The judged line is the run line, for every writer behind a `cmd_*` that
//! builds its own argv: create/delete/rename branch, create/delete tag,
//! publish release (tag, then push) and the two selective patch commands.
//!
//! Each command is driven for real. A fake sidecar stands in for the harness,
//! so the gate answers and its ledger row records the exact argv it was asked
//! to judge (`argv_json`). The spawn log records what git was then started
//! with. The test asserts the two agree. It needs no knowledge of the
//! builders: a writer that stops running what its command judged (a `--force`
//! added, a `--` inserted, a different subcommand) fails here.
//!
//! Commands hop to a pool thread, so the thread-local `spawn_labels_during`
//! cannot see their children; the per-directory spawn log can, and is keyed by
//! the checkout.

use super::*;
use crate::diff::{DiffLineType, FilePatch, UnifiedDiffHunk, UnifiedDiffLine};
use crate::harness::sidecar;
use crate::procguard::LockedSpawn;
use std::path::Path;

/// Replies to every policy request with `@REPLY@` after the hello handshake.
#[cfg(unix)]
const FAKE_GATE: &str = r#"#!/bin/sh
reply() {
  id=$(printf '%s' "$1" | sed -n 's/^{"id":"\([0-9]*\)".*/\1/p')
  printf '{"id":"%s","ok":true,"result":%s}\n' "$id" "$2"
}
IFS= read -r line || exit 1
reply "$line" '{"protocol":1,"ops":["hello","policy.check.command","policy.check.file"],"posture":"host"}'
while IFS= read -r line; do
  reply "$line" '@REPLY@'
done
"#;

#[cfg(unix)]
const ALLOW: &str = r#"{"action":"allow","rule":"","severity":"","reason":"","target":"","task_id":"","demoted":"","degraded":[]}"#;
#[cfg(unix)]
const DENY: &str = r#"{"action":"deny","rule":"parity.denied","severity":"hard","reason":"refused by the parity fixture","target":"","task_id":"","demoted":"","degraded":[]}"#;

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output_locked()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    crate::engine::git_cli::git_text(dir, args).expect("git read")
}

/// A checkout with one commit and a local bare `origin` it tracks, answered by
/// a fake gate. Field order is drop order: the binding goes before the lock.
#[cfg(unix)]
struct Fixture {
    _binding: sidecar::TestBinaryBinding,
    _serial: sidecar::SidecarTestGuard,
    _gate: tempfile::TempDir,
    _remote: tempfile::TempDir,
    dir: tempfile::TempDir,
    repo: String,
}

#[cfg(unix)]
impl Fixture {
    fn new(reply: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let serial = sidecar::test_serial();
        let gate = tempfile::tempdir().expect("gate dir");
        let script = gate.path().join("fake-gate");
        std::fs::write(&script, FAKE_GATE.replace("@REPLY@", reply)).expect("write gate");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let binding = sidecar::bind_test_binary(&serial, script.to_string_lossy());

        let remote = tempfile::tempdir().expect("remote dir");
        git(remote.path(), &["init", "--bare", "--initial-branch=main"]);
        let dir = tempfile::tempdir().expect("repo dir");
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test User"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        crate::test_support::trust_repo(dir.path());
        // The gate's ledger lives in the checkout; it is not the user's work.
        std::fs::write(dir.path().join(".git/info/exclude"), ".devcouncil/\n").unwrap();
        std::fs::write(dir.path().join("app.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "app.txt"]);
        git(dir.path(), &["commit", "-m", "feat: seed"]);
        git(
            dir.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(dir.path(), &["push", "-u", "origin", "main"]);

        let repo = crate::engine::git_cli::canonicalize_plain(dir.path())
            .expect("canonical checkout")
            .to_string_lossy()
            .into_owned();
        Self {
            _binding: binding,
            _serial: serial,
            _gate: gate,
            _remote: remote,
            dir,
            repo,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Runs `command` to completion and reports what the gate was asked to
    /// judge and what git was started with while it ran.
    fn observe<T>(
        &self,
        command: impl std::future::Future<Output = Result<T, IpcError>>,
    ) -> (Result<T, IpcError>, Observed) {
        let cursor = crate::ledger::latest_cursor(&self.repo).expect("ledger cursor");
        let spawned_before =
            crate::engine::git_cli::spawn_log::spawns_in(Path::new(&self.repo)).len();
        let result = tauri::async_runtime::block_on(command);
        let judged = crate::ledger::tail(&self.repo, cursor, 1_000)
            .expect("ledger tail")
            .into_iter()
            // The gate's rows. The branch-delete journal also writes the line it
            // ran, with the tip it removed as `before_ref`.
            .filter(|row| row.before_ref.is_none())
            .filter_map(|row| row.argv_json)
            .map(|json| serde_json::from_str::<Vec<String>>(&json).expect("argv_json is an argv"))
            .collect();
        let spawned = crate::engine::git_cli::spawn_log::spawns_in(Path::new(&self.repo))
            [spawned_before..]
            .iter()
            .map(|argv| without_pins(argv))
            .collect();
        (result, Observed { judged, spawned })
    }
}

/// What one command asked the gate and what it then ran.
struct Observed {
    judged: Vec<Vec<String>>,
    spawned: Vec<Vec<String>>,
}

/// The pinned `-c key=value` pairs git is started with ahead of the argv.
fn without_pins(argv: &[String]) -> Vec<String> {
    let mut rest = argv;
    while let [flag, _, tail @ ..] = rest {
        if flag != "-c" {
            break;
        }
        rest = tail;
    }
    rest.to_vec()
}

impl Observed {
    /// Every judged line ran exactly as judged: of the children started with
    /// that line's subcommand, there is exactly one and it is that line.
    fn assert_run_as_judged(&self, label: &str, judged_lines: usize) {
        assert_eq!(
            self.judged.len(),
            judged_lines,
            "{label}: the gate judged {:#?}",
            self.judged
        );
        for judged in &self.judged {
            assert_eq!(judged.first().map(String::as_str), Some("git"), "{label}");
            let run_line = &judged[1..];
            let same_verb: Vec<&Vec<String>> = self
                .spawned
                .iter()
                .filter(|argv| argv.first() == run_line.first())
                .collect();
            assert_eq!(
                same_verb,
                vec![&run_line.to_vec()],
                "{label}: judged `{}` but git ran {same_verb:#?}\nall spawns: {:#?}",
                judged.join(" "),
                self.spawned
            );
        }
    }

    /// Nothing that changes the repository was started.
    fn assert_nothing_mutated(&self, label: &str) {
        let mutating = ["branch", "tag", "push", "apply"];
        let started: Vec<&Vec<String>> = self
            .spawned
            .iter()
            .filter(|argv| {
                argv.first()
                    .is_some_and(|verb| mutating.contains(&verb.as_str()))
            })
            .collect();
        assert!(
            started.is_empty(),
            "{label}: a refused command still ran {started:#?}"
        );
    }
}

/// The command was refused by the gate, not by anything else.
fn assert_blocked<T: std::fmt::Debug>(result: Result<T, IpcError>) {
    let error = result.expect_err("the gate said Blocked, so the command must fail");
    assert!(
        error.message.starts_with("Blocked by"),
        "refused for another reason: {error}"
    );
}

fn one_line_patch() -> FilePatch {
    FilePatch {
        old_path: "app.txt".into(),
        new_path: "app.txt".into(),
        hunks: vec![UnifiedDiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 2,
            header: String::new(),
            lines: vec![
                UnifiedDiffLine {
                    line_type: DiffLineType::Context,
                    old_line_no: Some(1),
                    new_line_no: Some(1),
                    content: "one".into(),
                    is_selected: false,
                    no_newline: false,
                },
                UnifiedDiffLine {
                    line_type: DiffLineType::Addition,
                    old_line_no: None,
                    new_line_no: Some(2),
                    content: "two".into(),
                    is_selected: true,
                    no_newline: false,
                },
            ],
        }],
    }
}

/// One test for every writer: the line each command judged is the line its
/// writer ran. Mutating any writer to run something else makes it fail.
#[cfg(unix)]
#[test]
fn every_judged_writer_runs_the_line_its_command_judged() {
    let fx = Fixture::new(ALLOW);
    let repo = fx.repo.clone();
    let head = git_out(fx.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let (result, seen) = fx.observe(cmd_create_branch(repo.clone(), "feat/a".into(), None));
    result.expect("create branch");
    seen.assert_run_as_judged("create branch", 1);

    let (result, seen) = fx.observe(cmd_create_branch(
        repo.clone(),
        "feat/b".into(),
        Some("HEAD".into()),
    ));
    result.expect("create branch at a start point");
    seen.assert_run_as_judged("create branch at a start point", 1);

    let (result, seen) = fx.observe(cmd_rename_branch(
        repo.clone(),
        "feat/a".into(),
        "feat/c".into(),
    ));
    result.expect("rename branch");
    seen.assert_run_as_judged("rename branch", 1);

    let (result, seen) = fx.observe(cmd_delete_branch(repo.clone(), "feat/c".into(), false));
    result.expect("delete branch");
    seen.assert_run_as_judged("delete branch -d", 1);

    let (result, seen) = fx.observe(cmd_delete_branch(repo.clone(), "feat/b".into(), true));
    result.expect("force delete branch");
    seen.assert_run_as_judged("delete branch -D", 1);

    let (result, seen) = fx.observe(cmd_create_tag(
        repo.clone(),
        "light".into(),
        Some(head.clone()),
        None,
    ));
    result.expect("lightweight tag");
    seen.assert_run_as_judged("create lightweight tag", 1);

    let (result, seen) = fx.observe(cmd_create_tag(
        repo.clone(),
        "noted".into(),
        None,
        Some("a message".into()),
    ));
    result.expect("annotated tag");
    seen.assert_run_as_judged("create annotated tag", 1);

    let (result, seen) = fx.observe(cmd_delete_tag(repo.clone(), "light".into()));
    result.expect("delete tag");
    seen.assert_run_as_judged("delete tag", 1);

    let (result, seen) = fx.observe(cmd_publish_release(
        repo.clone(),
        "v1.0.0".into(),
        "Release v1.0.0".into(),
    ));
    result.expect("publish release");
    // The tag, then the push: both are judged before either runs.
    seen.assert_run_as_judged("publish release", 2);

    std::fs::write(fx.path().join("app.txt"), "one\ntwo\n").unwrap();
    let (result, seen) = fx.observe(cmd_stage_selective_patch(repo.clone(), one_line_patch()));
    result.expect("stage a selected line");
    seen.assert_run_as_judged("stage selective patch", 1);

    let (result, seen) = fx.observe(cmd_unstage_selective_patch(repo.clone(), one_line_patch()));
    result.expect("unstage a selected line");
    seen.assert_run_as_judged("unstage selective patch", 1);
}

/// A gate that says Blocked stops each command family before anything runs.
#[cfg(unix)]
#[test]
fn a_blocked_ref_command_starts_no_mutating_child() {
    let fx = Fixture::new(DENY);
    let repo = fx.repo.clone();
    git(fx.path(), &["branch", "keep"]);
    git(fx.path(), &["tag", "keep-tag"]);
    let head = git_out(fx.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let (result, seen) = fx.observe(cmd_create_branch(repo.clone(), "feat/x".into(), None));
    assert_blocked(result);
    seen.assert_nothing_mutated("create branch");

    let (result, seen) = fx.observe(cmd_delete_branch(repo.clone(), "keep".into(), true));
    assert_blocked(result);
    seen.assert_nothing_mutated("delete branch");

    let (result, seen) = fx.observe(cmd_rename_branch(
        repo.clone(),
        "keep".into(),
        "moved".into(),
    ));
    assert_blocked(result);
    seen.assert_nothing_mutated("rename branch");

    let (result, seen) = fx.observe(cmd_create_tag(repo.clone(), "t".into(), Some(head), None));
    assert_blocked(result);
    seen.assert_nothing_mutated("create tag");

    let (result, seen) = fx.observe(cmd_delete_tag(repo.clone(), "keep-tag".into()));
    assert_blocked(result);
    seen.assert_nothing_mutated("delete tag");

    let (result, seen) = fx.observe(cmd_publish_release(
        repo.clone(),
        "v2.0.0".into(),
        "Release v2.0.0".into(),
    ));
    assert_blocked(result);
    seen.assert_nothing_mutated("publish release");

    assert_eq!(
        git_out(fx.path(), &["branch", "--format=%(refname:short)"])
            .lines()
            .collect::<Vec<_>>(),
        vec!["keep", "main"]
    );
    assert_eq!(
        git_out(fx.path(), &["tag", "--list"])
            .lines()
            .collect::<Vec<_>>(),
        vec!["keep-tag"]
    );
}

#[cfg(unix)]
#[test]
fn a_blocked_selective_patch_starts_no_mutating_child() {
    let fx = Fixture::new(DENY);
    std::fs::write(fx.path().join("app.txt"), "one\ntwo\n").unwrap();

    let (result, seen) = fx.observe(cmd_stage_selective_patch(fx.repo.clone(), one_line_patch()));
    assert_blocked(result);
    seen.assert_nothing_mutated("stage selective patch");

    let (result, seen) = fx.observe(cmd_unstage_selective_patch(
        fx.repo.clone(),
        one_line_patch(),
    ));
    assert_blocked(result);
    seen.assert_nothing_mutated("unstage selective patch");

    assert!(
        git_out(fx.path(), &["diff", "--cached", "--name-only"])
            .trim()
            .is_empty(),
        "nothing reached the index"
    );
}

// ---- adversarial review additions (b17/adc5dca5-adv) ----

/// Edge inputs through every builder-backed command: whatever the gate judged
/// and the command then did must agree. An accepted input is run exactly as
/// judged; a refused one starts no mutating child.
#[cfg(unix)]
#[test]
fn adv_edge_inputs_keep_judged_equal_to_run() {
    let fx = Fixture::new(ALLOW);
    let repo = fx.repo.clone();
    let mut accepted = 0usize;
    let mut refused = 0usize;
    let mut tally = |label: &str, result: Result<(), IpcError>, seen: &Observed| match result {
        Ok(()) => {
            accepted += 1;
            // A mutating spawn must exist; judged == run is asserted per line.
            seen.assert_run_as_judged(label, 1);
            assert!(
                !seen.spawned.is_empty(),
                "{label}: accepted but spawned nothing"
            );
        }
        Err(_) => {
            refused += 1;
            seen.assert_nothing_mutated(label);
        }
    };

    for name in [
        "-x",
        "--force",
        "refs/heads/pfx",
        "ünï/çode",
        "a..b",
        "a~1",
        "-D",
    ] {
        let (r, s) = fx.observe(cmd_create_branch(repo.clone(), name.into(), None));
        tally(&format!("create branch {name:?}"), r.map(|_| ()), &s);
    }
    for start in ["", "-x", "--orphan", "HEAD~0", "main"] {
        let (r, s) = fx.observe(cmd_create_branch(
            repo.clone(),
            "sp".into(),
            Some(start.into()),
        ));
        tally(&format!("create branch start {start:?}"), r.map(|_| ()), &s);
        // best effort cleanup so the next start point can create it again
        let _ = tauri::async_runtime::block_on(cmd_delete_branch(repo.clone(), "sp".into(), true));
    }
    for name in [
        "-x",
        "--force",
        "v-é",
        "a..b",
        "a~1",
        "x:y",
        "tag with space",
    ] {
        let (r, s) = fx.observe(cmd_create_tag(repo.clone(), name.into(), None, None));
        tally(&format!("create tag {name:?}"), r.map(|_| ()), &s);
        let (r, s) = fx.observe(cmd_delete_tag(repo.clone(), name.into()));
        tally(&format!("delete tag {name:?}"), r.map(|_| ()), &s);
    }
    for name in ["-f", "*", "a..b", "-d"] {
        let (r, s) = fx.observe(cmd_delete_tag(repo.clone(), name.into()));
        tally(&format!("delete tag {name:?}"), r.map(|_| ()), &s);
    }
    for name in ["-x", "--force", "nope"] {
        let (r, s) = fx.observe(cmd_delete_branch(repo.clone(), name.into(), true));
        tally(&format!("delete branch {name:?}"), r.map(|_| ()), &s);
    }
    let (r, s) = fx.observe(cmd_rename_branch(repo.clone(), "-x".into(), "y".into()));
    tally("rename from flag", r.map(|_| ()), &s);
    let (r, s) = fx.observe(cmd_rename_branch(
        repo.clone(),
        "main".into(),
        "--force".into(),
    ));
    tally("rename to flag", r.map(|_| ()), &s);

    // Vacuity guard: both outcomes must have been exercised.
    assert!(accepted >= 6, "only {accepted} edge inputs were accepted");
    assert!(refused >= 15, "only {refused} edge inputs were refused");
}

/// The expectation the review brief states: a ref name the writer refuses is
/// never put in front of the gate. Today every cmd_* builds and judges the
/// line BEFORE the writer validates, so the gate sees (and its ledger records
/// an `allow` for) `git tag -d -f`, `git branch --force` and the like.
#[cfg(unix)]
#[test]
fn adv_a_name_the_writer_refuses_is_never_judged() {
    let fx = Fixture::new(ALLOW);
    let repo = fx.repo.clone();
    let (r, s) = fx.observe(cmd_delete_tag(repo.clone(), "-f".into()));
    assert!(r.is_err());
    assert!(
        s.judged.is_empty(),
        "the gate was asked to judge {:?}",
        s.judged
    );
    let (r, s) = fx.observe(cmd_create_branch(repo, "--force".into(), None));
    assert!(r.is_err());
    assert!(
        s.judged.is_empty(),
        "the gate was asked to judge {:?}",
        s.judged
    );
}

/// The commit-graph write (Owner Decision 3): the line the Storage panel previewed
/// is the line the gate judged and the line git was started with, a gate refusal
/// runs nothing, and a second click on the now-fresh graph is refused.
#[cfg(unix)]
#[test]
fn the_commit_graph_write_runs_the_previewed_and_judged_line() {
    let fx = Fixture::new(DENY);
    let repo = fx.repo.clone();
    let preview = GitWriter::commit_graph_write_plan(&repo).expect("a write is offered");
    let (result, seen) = fx.observe(cmd_commit_graph_write(repo.clone(), preview.argv.clone()));
    assert_blocked(result);
    seen.assert_nothing_mutated("denied commit-graph write");
    assert!(
        !seen
            .spawned
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("commit-graph")),
        "a denied write still spawned {:#?}",
        seen.spawned
    );
    assert!(!fx.path().join(".git/objects/info/commit-graph").exists());

    drop(fx);
    let fx = Fixture::new(ALLOW);
    let repo = fx.repo.clone();
    let preview = GitWriter::commit_graph_write_plan(&repo).expect("a write is offered");
    let (result, seen) = fx.observe(cmd_commit_graph_write(repo.clone(), preview.argv.clone()));
    let written = result.expect("the write runs").output;
    assert!(!written.cancelled);
    // The gate row and the writer's own journal row carry the same line.
    seen.assert_run_as_judged("commit-graph write", 2);
    assert!(seen.judged.iter().all(|judged| *judged == preview.argv));
    assert!(fx.path().join(".git/objects/info/commit-graph").exists());

    let again = tauri::async_runtime::block_on(cmd_commit_graph_write(repo, preview.argv));
    assert!(again.is_err(), "a fresh graph offers no second write");
}
