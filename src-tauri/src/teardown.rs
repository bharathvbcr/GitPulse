//! The one teardown every way out of the app runs.
//!
//! Quitting from the menu, the Dock or a logout ends in `RunEvent::Exit`; a
//! `kill` or a hangup ends in procguard's signal watcher. Before this, the
//! first stopped the terminals and never swept procguard's children, and the
//! second swept the children and never stopped the terminals — so a `git
//! fetch` outlived a Cmd-Q, and a SIGTERM left every task run "running" with
//! the exit code lost. Both now call [`run`].

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::terminal::{self, TerminalSessions};

/// How long terminal shutdown may take before the sweep goes ahead without it.
/// A PTY close waits out the hangup grace and a reap, all sessions at once;
/// this is only the bound on a wedged one.
pub const TERMINAL_DEADLINE: Duration = Duration::from_secs(8);

/// Stops everything this process started, once. A second caller (a signal
/// during a normal quit) waits for the first rather than racing it.
pub fn run(terminals: &TerminalSessions) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        steps(terminals, TERMINAL_DEADLINE, || {
            let sweep = crate::procguard::reap_all(crate::procguard::CHILD_GRACE);
            if sweep.is_complete() {
                log::info!(target: "shutdown", "{}", sweep.describe());
            } else {
                log::warn!(target: "shutdown", "{}", sweep.describe());
            }
        });
    });
}

/// The teardown with the procguard sweep injected, so a test can exercise the
/// order without sweeping every child the test process owns.
pub(crate) fn steps(terminals: &TerminalSessions, deadline: Duration, sweep: impl FnOnce()) {
    // First: a launch from here on must wait for this process to be gone and
    // take over, not hand its request to one that is leaving.
    #[cfg(target_os = "macos")]
    crate::desktop::launch::shutdown();
    // Before anything that can stall, so a hung terminal shutdown cannot lose it.
    crate::logging::performance::flush();
    stop_terminals(terminals, deadline);
    // The sidecar is asked to finish (it closes its runs) before the sweep
    // would kill it outright; the call gives up on its own after ~1.2 s.
    crate::harness::sidecar::shutdown();
    sweep();
}

fn stop_terminals(terminals: &TerminalSessions, deadline: Duration) {
    let (done, finished) = mpsc::channel();
    let terminals = terminals.clone();
    let spawned = thread::Builder::new()
        .name("terminal-shutdown".into())
        .spawn(move || {
            let _ = done.send(terminal::shutdown_sessions(&terminals));
        });
    match spawned {
        Ok(_) => match finished.recv_timeout(deadline) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                log::warn!(target: "terminal", "terminal shutdown incomplete: {error}")
            }
            Err(_) => {
                log::warn!(target: "terminal", "terminal shutdown did not finish in {deadline:?}")
            }
        },
        Err(error) => log::warn!(target: "terminal", "terminal shutdown did not start: {error}"),
    }
}
