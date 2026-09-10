//! GitPulse hook executable.
//!
//! One binary, dispatched by its first argument, reading the host's hook JSON
//! on stdin and writing hook JSON on stdout. Every decision lives in
//! `gitpulse_lib::hooks` so the contract is unit-tested rather than only
//! exercised by spawning this process.
//!
//! Two rules shape this file:
//!
//! * **stdout is the protocol channel.** Nothing but the hook JSON goes down
//!   it. Diagnostics go to stderr, where a hook that exits 0 has them recorded
//!   in the host's debug log and shown to nobody else. The git and harness
//!   subprocesses underneath already pipe their own stdout
//!   (`engine::git_cli::git_command`, `harness::sidecar`), so none of them can
//!   leak into ours.
//! * **Always exit 0.** Exit 2 blocks the tool call whatever the JSON says. A
//!   hook that crashed, timed out or could not read its input must never be the
//!   reason a user's edit is refused, so every failure path here degrades to
//!   "no decision" plus a stderr line. The single exception is a hook the host
//!   *itself* killed: `procguard::exit` reports that signal, which is not exit
//!   2 and so still blocks nothing, and which the host already knows about
//!   because it sent it.

use std::io;

use gitpulse_lib::hooks::{self, SUBCOMMANDS};

fn main() {
    decide();
    // Not a bare return: a hook that finished while a caught signal was still
    // being swept would exit out from under it, leaving the git or harness
    // subprocess the sweep was killing.
    gitpulse_lib::procguard::exit(0);
}

fn decide() {
    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
    // A host that decides a hook has taken too long kills it, and the git or
    // harness subprocess a check was in the middle of would otherwise carry
    // on unattached. The cost is one pipe and one thread, well under the
    // startup this file is written to protect. Not logged at startup like the
    // servers do: stderr is the host's debug channel and a line on every
    // matching tool call would drown the ones that matter.
    let _ = gitpulse_lib::procguard::install_signal_handlers();

    let Some(subcommand) = std::env::args().nth(1) else {
        log::warn!(target: "hook",
            "gitpulse-hook: no subcommand given; expected one of {}",
            SUBCOMMANDS.join(", ")
        );
        return;
    };

    if !SUBCOMMANDS.contains(&subcommand.as_str()) {
        log::warn!(target: "hook", "unknown hook subcommand {subcommand:?}; no decision");
        return;
    }

    // An unparseable payload is reported on stderr and dropped. Guessing at a
    // decision from input we could not read would be the worst of both worlds:
    // neither a check that ran nor an admission that one did not. Exiting 0
    // with no stdout leaves the host's own permission flow exactly as it was.
    let input = match hooks::read_input(io::stdin()) {
        Ok(input) => input,
        Err(error) => {
            log::warn!(target: "hook", "gitpulse-hook {subcommand}: {error}");
            return;
        }
    };

    match hooks::dispatch(&subcommand, &input) {
        Ok(output) => {
            if let Some(json) = output.render() {
                let result = gitpulse_lib::output::BoundedOutput::new(
                    io::stdout(),
                    "gitpulse-hook-stdout",
                    hooks::MAX_INPUT_BYTES,
                    std::time::Duration::from_secs(1),
                )
                .and_then(|output| output.write(format!("{json}\n").as_bytes()));
                if let Err(error) = result {
                    log::warn!(target: "hook", "could not deliver hook stdout: {error}; no retry");
                }
            }
        }
        Err(error) => log::warn!(target: "hook", "gitpulse-hook: {error}"),
    }
}
