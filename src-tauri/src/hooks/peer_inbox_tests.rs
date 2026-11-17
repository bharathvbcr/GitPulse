//! The `peer-inbox` hook takes a message off the mailbox only once the text
//! that delivers it is ready to print.
//!
//! The hook's work runs inside `PEER_INBOX_BUDGET` and prints nothing when the
//! budget is missed. A message consumed before that point has no bus copy when a
//! person sent it, so it would be gone with nothing to say so. These tests stop
//! the work at a checkpoint between reading the mailbox and building the text
//! (no sleep: the builder blocks on a channel the test releases), let the budget
//! run out, and then look at what is left.

use super::*;
use crate::agent_mailbox::{EnvelopeProvenance, Mailbox};
use crate::test_support::git_in;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::SystemTime;

const SESSION: &str = "5c0e7a42-91b3-4d68-a1f4-0b7c6d2e9f33";

fn trusted_repo() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-b", "feat/peer-inbox"]);
    (dir, home, repo)
}

fn boundary_input(repo: &std::path::Path) -> HookInput {
    HookInput {
        host: HostKind::Claude,
        hook_event_name: "PostToolUse".into(),
        session_id: SESSION.into(),
        cwd: repo.to_string_lossy().into_owned(),
        ..HookInput::default()
    }
}

fn delivered_text(output: &HookOutput) -> String {
    output
        .hook_specific_output
        .as_ref()
        .and_then(|specific| specific.additional_context.clone())
        .unwrap_or_default()
}

/// Waits until `held` is the only owner, i.e. until the worker thread that
/// captured a clone of it has finished and dropped its closure.
fn wait_for_worker<T>(held: &Arc<T>) {
    assert!(
        crate::test_support::wait_until(Duration::from_secs(20), || Arc::strong_count(held) == 1),
        "the abandoned worker never finished"
    );
}

#[test]
fn a_budget_missed_while_the_text_is_built_leaves_the_message_pending_and_the_next_call_delivers_it(
) {
    let serial = crate::test_support::env::env_serial();
    let (_dir, home, repo) = trusted_repo();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", &home)
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .remove(crate::alerts::bridge::SESSION_ENV)
        .remove(AGENT_KIND_ENV);
    let mailbox = Mailbox::at_default().unwrap();
    mailbox
        .deposit(
            SESSION,
            "human-1",
            "please rebase before you push",
            SystemTime::now(),
        )
        .unwrap();
    let input = boundary_input(&repo);

    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let worker = Arc::new(());
    let held = Arc::clone(&worker);
    let missed = peer_inbox_budgeted(
        mailbox.clone(),
        &input,
        Duration::from_millis(100),
        move || {
            let _held = &held;
            let _ = reached_tx.send(());
            // Parked here until the test lets the abandoned worker go on.
            let _ = release_rx.recv_timeout(Duration::from_secs(30));
        },
    );
    assert!(
        missed.is_silent(),
        "a missed budget prints nothing: {missed:?}"
    );
    reached_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the worker reached the point between reading the mailbox and building the text");
    release_tx.send(()).unwrap();
    wait_for_worker(&worker);

    assert_eq!(
        mailbox.pending(SESSION),
        1,
        "a message nobody was shown must still be pending"
    );
    let next = peer_inbox_in(&mailbox, &input);
    assert!(
        delivered_text(&next).contains("please rebase before you push"),
        "the next hook call delivers it: {next:?}"
    );
    assert_eq!(mailbox.pending(SESSION), 0);
}

/// Both callers wait here until each has read the mailbox, so neither can
/// have taken the message before the other looked.
struct Rendezvous {
    arrived: Mutex<usize>,
    both: Condvar,
}

impl Rendezvous {
    fn meet(&self) {
        let mut arrived = self.arrived.lock().unwrap();
        *arrived += 1;
        self.both.notify_all();
        let _ = self
            .both
            .wait_timeout_while(arrived, Duration::from_secs(5), |n| *n < 2)
            .unwrap();
    }
}

#[test]
fn two_hook_processes_racing_on_one_session_deliver_a_message_once() {
    let serial = crate::test_support::env::env_serial();
    let (_dir, home, repo) = trusted_repo();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", &home)
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .remove(crate::alerts::bridge::SESSION_ENV)
        .remove(AGENT_KIND_ENV);
    let mailbox = Mailbox::at_default().unwrap();
    mailbox
        .deposit(
            SESSION,
            "human-1",
            "one message, two readers",
            SystemTime::now(),
        )
        .unwrap();
    let input = boundary_input(&repo);
    let rendezvous = Arc::new(Rendezvous {
        arrived: Mutex::new(0),
        both: Condvar::new(),
    });

    let outputs: Vec<HookOutput> = (0..2)
        .map(|_| {
            let mailbox = mailbox.clone();
            let input = input.clone();
            let rendezvous = Arc::clone(&rendezvous);
            thread::spawn(move || {
                peer_inbox_budgeted(mailbox, &input, Duration::from_secs(30), move || {
                    rendezvous.meet();
                })
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();

    let delivered = AtomicUsize::new(0);
    for output in &outputs {
        if delivered_text(output).contains("one message, two readers") {
            delivered.fetch_add(1, Ordering::SeqCst);
        }
    }
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        1,
        "exactly one reader delivers it: {outputs:?}"
    );
    assert_eq!(mailbox.pending(SESSION), 0);
}

#[test]
fn a_peer_message_is_marked_read_on_the_bus_only_when_it_is_delivered() {
    let serial = crate::test_support::env::env_serial();
    let (_dir, home, repo) = trusted_repo();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("HOME", &home)
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .remove(crate::alerts::bridge::SESSION_ENV)
        .remove(AGENT_KIND_ENV);
    let mailbox = Mailbox::at_default().unwrap();
    let repo_root = repo.to_string_lossy().into_owned();
    let posted = crate::engine::agent_bus::post_message(
        &repo_root,
        crate::engine::agent_bus::MessageOrigin::Agent,
        "claude:other",
        Some("claude:peer-inbox-test"),
        "note",
        "peer says hello",
        Some("main"),
    )
    .unwrap();
    let msg_id = posted.msg_id.clone().expect("the bus names its messages");
    mailbox
        .deposit_with_provenance(
            SESSION,
            &msg_id,
            "peer says hello",
            EnvelopeProvenance::PeerAgent {
                msg_id: msg_id.clone(),
                from_peer: "claude:other".into(),
                provider: "claude".into(),
                branch: "main".into(),
                thread_key: String::new(),
                hop: 1,
                max_hops: 6,
                truncated_in_bus: false,
                repo_root: repo_root.clone(),
            },
            SystemTime::now(),
        )
        .unwrap();
    let read_by = || {
        crate::engine::agent_bus::get_message_by_id(&repo_root, &msg_id)
            .unwrap()
            .expect("the bus still holds the message")
            .read_by
    };
    let input = boundary_input(&repo);

    // A budget missed while the text is built: nothing is delivered, so the
    // sender must not be told it was read.
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let worker = Arc::new(());
    let held = Arc::clone(&worker);
    let missed = peer_inbox_budgeted(
        mailbox.clone(),
        &input,
        Duration::from_millis(100),
        move || {
            let _held = &held;
            let _ = reached_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(30));
        },
    );
    assert!(missed.is_silent());
    reached_rx.recv_timeout(Duration::from_secs(20)).unwrap();
    release_tx.send(()).unwrap();
    wait_for_worker(&worker);
    assert_eq!(mailbox.pending(SESSION), 1);
    assert!(read_by().is_empty(), "undelivered, so unacknowledged");

    let delivered = peer_inbox_in(&mailbox, &input);
    assert!(
        delivered_text(&delivered).contains("peer says hello"),
        "{delivered:?}"
    );
    assert!(!read_by().is_empty(), "delivered, so acknowledged");
}
