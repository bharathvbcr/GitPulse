//! The hooks' side of turn checkpoints (`engine::turn_checkpoint`): at the two
//! turn boundaries, ask the running app to take one.
//!
//! This process only decides to ask and only sends a request over the
//! UID-authenticated mailbox; the app writes the ref and the ledger row. It
//! asks only when `turn_checkpoints.enabled` is set in `tools.json`
//! ([`crate::tool_config::TurnCheckpointSettings`]), and when it is not, this
//! returns before any connection, process or ledger row.
//!
//! It never fails the hook, never waits past [`TURN_CHECKPOINT_APP_DEADLINE`],
//! and never prints to the agent: a turn boundary happens every turn, and a
//! warning each time is noise the person cannot act on. What happened is the
//! returned [`Outcome`] (logged at debug) and, for every attempt that reaches
//! the app, the app's ledger row.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::HookInput;
use crate::engine::turn_checkpoint::TurnEvent;

/// How long a hook waits for the app's answer. Short: the agent is waiting. A
/// capture that outlasts it finishes in the app and is recorded there.
pub(super) const TURN_CHECKPOINT_APP_DEADLINE: Duration = Duration::from_millis(1000);

/// The longest label sent to the app.
#[cfg(target_os = "macos")]
const MAX_LABEL_BYTES: usize = 1000;

/// What one boundary call came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The setting is off (or unreadable): nothing was attempted.
    Disabled,
    /// The app answered: `captured`, `unchanged`, `coalesced`, `skipped` or
    /// `failed`, with its reason.
    Answered {
        outcome: String,
        reason: Option<String>,
    },
    /// The request did not reach a checkpoint: no app, no deadline met, an
    /// older app, an unsupported platform. The reason says which.
    NotCaptured(String),
}

/// The turn is starting (`UserPromptSubmit`).
pub(super) fn at_turn_start(input: &HookInput) -> Outcome {
    at_boundary(input, TurnEvent::TurnStart)
}

/// The turn is really ending: a `Stop` that did not continue.
pub(super) fn at_turn_end(input: &HookInput) -> Outcome {
    at_boundary(input, TurnEvent::TurnEnd)
}

fn at_boundary(input: &HookInput, event: TurnEvent) -> Outcome {
    let Some(root) = super::find_git_root(Path::new(&input.cwd)) else {
        return Outcome::Disabled;
    };
    // An untrusted repository is never asked about: no request, no ledger row.
    if crate::repository_trust::require(&root).is_err() {
        return Outcome::Disabled;
    }
    let root = root.to_string_lossy().into_owned();
    let outcome = at_boundary_in(
        profile_dir(&root),
        &root,
        input,
        event,
        TURN_CHECKPOINT_APP_DEADLINE,
        reach_app,
    );
    log::debug!(target: "hooks", "turn checkpoint ({}): {outcome:?}", event.as_str());
    outcome
}

#[cfg(target_os = "macos")]
fn profile_dir(root: &str) -> Option<PathBuf> {
    super::scan_profile(root)
}

#[cfg(not(target_os = "macos"))]
fn profile_dir(_root: &str) -> Option<PathBuf> {
    crate::tool_config::default_config_dir()
}

/// The boundary call with its profile directory, deadline and transport
/// passed in, so a test can see that a disabled setting never reaches `reach`.
fn at_boundary_in<R>(
    dir: Option<PathBuf>,
    root: &str,
    input: &HookInput,
    event: TurnEvent,
    deadline: Duration,
    reach: R,
) -> Outcome
where
    R: FnOnce(
            PathBuf,
            crate::desktop::instance::Request,
            Duration,
        ) -> Result<serde_json::Value, String>
        + Send
        + 'static,
{
    let Some(dir) = dir else {
        return Outcome::Disabled;
    };
    if !crate::tool_config::turn_checkpoint_settings_in(&dir).enabled {
        return Outcome::Disabled;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (root, input, event, deadline, reach);
        Outcome::NotCaptured("not captured: the app-side recorder is unavailable off macOS".into())
    }
    #[cfg(target_os = "macos")]
    {
        let label = (event == TurnEvent::TurnEnd).then(|| label_of(&input.last_assistant_message));
        let request = crate::desktop::instance::Request::RecordTurnCheckpoint {
            id: super::mailbox_id("turn"),
            cwd: root.to_string(),
            session_id: (!input.session_id.trim().is_empty()).then(|| input.session_id.clone()),
            event: event.as_str().to_string(),
            label: label.filter(|l| !l.is_empty()),
        };
        let answer = super::within_budget(deadline + Duration::from_millis(100), move || {
            reach(dir, request, deadline)
        });
        match answer {
            Some(Ok(reply)) => answered(&reply),
            Some(Err(why)) => Outcome::NotCaptured(not_captured_reason(&why)),
            None => Outcome::NotCaptured("not captured: the app missed its deadline".into()),
        }
    }
}

#[cfg(target_os = "macos")]
fn reach_app(
    dir: PathBuf,
    request: crate::desktop::instance::Request,
    deadline: Duration,
) -> Result<serde_json::Value, String> {
    crate::desktop::instance::reach_owner(&dir, deadline)
        .ok_or_else(|| "no GitPulse app is answering".to_string())?
        .record_turn_checkpoint(&request, deadline)
}

#[cfg(not(target_os = "macos"))]
fn reach_app(
    _dir: PathBuf,
    _request: crate::desktop::instance::Request,
    _deadline: Duration,
) -> Result<serde_json::Value, String> {
    Err("the app-side recorder is unavailable off macOS".into())
}

/// One redacted, control-free line of the agent's last words, bounded.
#[cfg(target_os = "macos")]
fn label_of(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let clean: String = crate::ledger::redact::text(line)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut end = clean.len().min(MAX_LABEL_BYTES);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    clean[..end].to_string()
}

#[cfg(target_os = "macos")]
fn answered(reply: &serde_json::Value) -> Outcome {
    match reply.get("outcome").and_then(serde_json::Value::as_str) {
        Some(outcome) => Outcome::Answered {
            outcome: outcome.to_string(),
            reason: reply
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        },
        None => Outcome::NotCaptured("not captured: the app's answer named no outcome".into()),
    }
}

/// An owner that predates the request refuses its `kind` as a malformed
/// frame; say that, rather than a parser's wording.
#[cfg(target_os = "macos")]
fn not_captured_reason(why: &str) -> String {
    if why.contains("unknown variant") {
        "not captured: app does not support checkpoints".to_string()
    } else {
        format!("not captured: {why}")
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
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
        assert!(started.elapsed() < Duration::from_secs(2));
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
                .output()
                .unwrap();
            assert!(
                refs.stdout.is_empty(),
                "enabled={enabled} trusted={trusted}"
            );
            assert_eq!(w.ledger_rows(), 0);
        }
    }
}
