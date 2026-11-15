//! The turn-checkpoint hook calls, through the real hook entry points.

use super::*;
use crate::procguard::LockedSpawn;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn input(root: &Path) -> HookInput {
    HookInput {
        cwd: root.to_string_lossy().into_owned(),
        session_id: "sess-1".into(),
        last_assistant_message: "done\nsecond line".into(),
        ..HookInput::default()
    }
}

fn profile(enabled: Option<bool>) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    if let Some(enabled) = enabled {
        std::fs::write(
            dir.path().join("tools.json"),
            format!(r#"{{"version":1,"turn_checkpoints":{{"enabled":{enabled}}}}}"#),
        )
        .unwrap();
    }
    dir
}

fn probe() -> (
    Arc<AtomicBool>,
    impl FnOnce(
            PathBuf,
            crate::desktop::instance::Request,
            Duration,
        ) -> Result<serde_json::Value, String>
        + Send
        + 'static,
) {
    let reached = Arc::new(AtomicBool::new(false));
    let flag = reached.clone();
    (reached, move |_dir, request, _deadline| {
        flag.store(true, Ordering::SeqCst);
        assert!(matches!(
            request,
            crate::desktop::instance::Request::RecordTurnCheckpoint { .. }
        ));
        Ok(serde_json::json!({"outcome": "captured"}))
    })
}

#[test]
fn a_missing_or_disabled_setting_attempts_nothing() {
    let root = tempfile::TempDir::new().unwrap();
    for enabled in [None, Some(false)] {
        let dir = profile(enabled);
        let (reached, reach) = probe();
        let outcome = at_boundary_in(
            Some(dir.path().to_path_buf()),
            &root.path().to_string_lossy(),
            &input(root.path()),
            TurnEvent::TurnStart,
            Duration::from_millis(200),
            reach,
        );
        assert_eq!(outcome, Outcome::Disabled);
        assert!(
            !reached.load(Ordering::SeqCst),
            "no connection when {enabled:?}"
        );
    }
}

#[test]
fn an_unreadable_setting_file_is_off_not_on() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("tools.json"), "{ not json").unwrap();
    let (reached, reach) = probe();
    let outcome = at_boundary_in(
        Some(dir.path().to_path_buf()),
        &root.path().to_string_lossy(),
        &input(root.path()),
        TurnEvent::TurnEnd,
        Duration::from_millis(200),
        reach,
    );
    assert_eq!(outcome, Outcome::Disabled);
    assert!(!reached.load(Ordering::SeqCst));
}

#[test]
fn an_enabled_setting_sends_one_request_and_reports_the_answer() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = profile(Some(true));
    let (reached, reach) = probe();
    let outcome = at_boundary_in(
        Some(dir.path().to_path_buf()),
        &root.path().to_string_lossy(),
        &input(root.path()),
        TurnEvent::TurnEnd,
        Duration::from_millis(500),
        reach,
    );
    assert!(reached.load(Ordering::SeqCst));
    assert_eq!(
        outcome,
        Outcome::Answered {
            outcome: "captured".into(),
            reason: None
        }
    );
}

#[test]
fn an_owner_that_does_not_know_the_request_reads_as_not_captured() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = profile(Some(true));
    let outcome = at_boundary_in(
        Some(dir.path().to_path_buf()),
        &root.path().to_string_lossy(),
        &input(root.path()),
        TurnEvent::TurnStart,
        Duration::from_millis(500),
        |_, _, _| Err("malformed request: unknown variant `record_turn_checkpoint`".into()),
    );
    assert_eq!(
        outcome,
        Outcome::NotCaptured("not captured: app does not support checkpoints".into())
    );
}

#[test]
fn a_slow_app_is_abandoned_at_the_deadline_without_failing_the_hook() {
    let root = tempfile::TempDir::new().unwrap();
    let dir = profile(Some(true));
    let started = std::time::Instant::now();
    let outcome = at_boundary_in(
        Some(dir.path().to_path_buf()),
        &root.path().to_string_lossy(),
        &input(root.path()),
        TurnEvent::TurnStart,
        Duration::from_millis(100),
        |_, _, _| {
            std::thread::sleep(Duration::from_secs(3));
            Ok(serde_json::json!({"outcome": "captured"}))
        },
    );
    assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)));
    assert!(matches!(outcome, Outcome::NotCaptured(_)), "{outcome:?}");
}

#[test]
fn the_label_is_one_redacted_bounded_line() {
    let long = format!("{}\nsecond", "é".repeat(2000));
    let label = label_of(&long);
    assert!(label.len() <= MAX_LABEL_BYTES);
    assert!(!label.contains('\n'));
    assert_eq!(label_of("\n\n  \n"), "");
}

// ── the real entry points ────────────────────────────────────────────────

const SESSION: &str = "8a2f4b10-3c9e-4d12-8b77-112233445566";

/// A repository with one commit, an app owning a temporary profile that
/// answers turn checkpoints, and the settings file saying `enabled`.
struct Wired {
    _env: crate::test_support::env::EnvBinding<'static>,
    _home: tempfile::TempDir,
    _profile: tempfile::TempDir,
    _owner: crate::desktop::instance::Primary,
    _registered: Box<dyn std::any::Any>,
    _repo_dir: tempfile::TempDir,
    repo: PathBuf,
}

/// Not `test_support::git_in`: it trusts a repository the moment it is
/// initialised, and one case here needs a repository nobody approved.
fn git(dir: &Path, args: &[&str]) {
    use crate::procguard::LockedSpawn;
    let mut command = std::process::Command::new("git");
    crate::test_support::pin_git(&mut command);
    let out = command
        .args(args)
        .current_dir(dir)
        .output_locked()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn wired(enabled: bool, trusted: bool) -> Wired {
    let env = crate::test_support::env::bind_env_owned(crate::test_support::env::env_serial());
    let home = tempfile::tempdir().unwrap();
    let env = env
        .set("HOME", home.path())
        .set("XDG_CONFIG_HOME", home.path().join(".config"))
        .remove(crate::alerts::bridge::SESSION_ENV)
        .remove(super::super::AGENT_KIND_ENV);
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(repo_dir.path()).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    if trusted {
        crate::test_support::trust_repo(&repo);
    }
    let profile = tempfile::tempdir().unwrap();
    std::fs::write(
        profile.path().join("tools.json"),
        format!(r#"{{"version":1,"turn_checkpoints":{{"enabled":{enabled}}}}}"#),
    )
    .unwrap();
    let crate::desktop::instance::Claim::Primary(owner) =
        crate::desktop::instance::claim(profile.path(), Duration::from_secs(2))
    else {
        panic!("expected to own the mailbox");
    };
    owner.attach_turn_checkpointer(std::sync::Arc::new(
        crate::engine::turn_checkpoint::serve_turn_checkpoint,
    ));
    let registered: Box<dyn std::any::Any> =
        Box::new(super::super::test_apps::register(&repo, profile.path()));
    Wired {
        _env: env,
        _home: home,
        _profile: profile,
        _owner: owner,
        _registered: registered,
        _repo_dir: repo_dir,
        repo,
    }
}

impl Wired {
    fn input(&self, event: &str) -> HookInput {
        HookInput {
            hook_event_name: event.into(),
            session_id: SESSION.into(),
            cwd: self.repo.to_string_lossy().into_owned(),
            last_assistant_message: "finished the thing".into(),
            ..HookInput::default()
        }
    }

    fn checkpoints(&self) -> Vec<crate::engine::turn_checkpoint::TurnCheckpointSummary> {
        crate::engine::turn_checkpoint::list_turn_checkpoints(self.repo.to_str().unwrap(), 10)
            .map(|list| list.entries)
            .unwrap_or_default()
    }

    fn ledger_rows(&self) -> usize {
        crate::ledger::tail(self.repo.to_str().unwrap(), 0, 100)
            .map(|rows| {
                rows.iter()
                    .filter(|r| r.action == crate::ledger::TURN_CHECKPOINT_ACTION)
                    .count()
            })
            .unwrap_or(0)
    }
}

#[test]
fn a_prompt_submit_through_the_real_hook_takes_a_turn_start_checkpoint() {
    let w = wired(true, true);
    std::fs::write(w.repo.join("new.txt"), "n\n").unwrap();
    let _ = super::super::run_peer_inbox(&w.input("UserPromptSubmit"));
    let taken = w.checkpoints();
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].event.as_deref(), Some("turn_start"));
    assert_eq!(taken[0].session_id.as_deref(), Some(SESSION));
    assert_eq!(w.ledger_rows(), 1);
    // Any other event is not a turn boundary.
    let _ = super::super::run_peer_inbox(&w.input("PostToolUse"));
    assert_eq!(w.checkpoints().len(), 1);
}

#[test]
fn a_stop_that_ends_the_turn_takes_a_turn_end_checkpoint_and_one_that_continues_does_not() {
    use crate::agent_mailbox::{EnvelopeProvenance, Mailbox};
    let w = wired(true, true);
    std::fs::write(w.repo.join("edited.txt"), "e\n").unwrap();
    let mailbox = Mailbox::at_default().unwrap();
    let stop = HookInput {
        host: super::super::HostKind::Antigravity,
        termination_reason: "NO_TOOL_CALL".into(),
        ..w.input("Stop")
    };

    // A queued message continues the turn: it has not ended, so no checkpoint.
    mailbox
        .deposit_with_provenance(
            SESSION,
            "msg-1",
            "peer says hi",
            EnvelopeProvenance::PeerAgent {
                msg_id: "msg-1".into(),
                from_peer: "claude:peer-1".into(),
                provider: "claude".into(),
                branch: "main".into(),
                thread_key: "t".into(),
                hop: 1,
                max_hops: 6,
                truncated_in_bus: false,
                repo_root: w.repo.to_string_lossy().into_owned(),
            },
            std::time::SystemTime::now(),
        )
        .unwrap();
    assert!(super::super::stop_continue_in(&mailbox, &stop).is_some());
    assert!(
        w.checkpoints().is_empty(),
        "a continuing stop is not a turn end"
    );
    assert_eq!(w.ledger_rows(), 0);

    // Nothing left to continue with: the turn ends here.
    assert!(super::super::stop_continue_in(&mailbox, &stop).is_none());
    let taken = w.checkpoints();
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].event.as_deref(), Some("turn_end"));
    assert_eq!(taken[0].label, "finished the thing");
    assert_eq!(w.ledger_rows(), 1);
}

#[test]
fn a_claude_stop_through_deliver_takes_a_turn_end_checkpoint_without_peer_opt_in() {
    let w = wired(true, true);
    std::fs::write(w.repo.join("edited.txt"), "e\n").unwrap();
    let out = super::super::deliver::run(&HookInput {
        host: super::super::HostKind::Claude,
        ..w.input("Stop")
    })
    .unwrap();
    assert!(out.is_silent());
    let taken = w.checkpoints();
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].event.as_deref(), Some("turn_end"));
}

#[test]
fn a_disabled_setting_or_an_untrusted_repository_leaves_no_ref_and_no_ledger_row() {
    for (enabled, trusted) in [(false, true), (true, false)] {
        let w = wired(enabled, trusted);
        let _ = super::super::run_peer_inbox(&w.input("UserPromptSubmit"));
        assert_eq!(
            at_turn_end(&w.input("Stop")),
            Outcome::Disabled,
            "enabled={enabled} trusted={trusted}"
        );
        let refs = std::process::Command::new("git")
            .args(["for-each-ref", "refs/gitpulse"])
            .current_dir(&w.repo)
            .output_locked()
            .unwrap();
        assert!(
            refs.stdout.is_empty(),
            "enabled={enabled} trusted={trusted}"
        );
        assert_eq!(w.ledger_rows(), 0);
    }
}
