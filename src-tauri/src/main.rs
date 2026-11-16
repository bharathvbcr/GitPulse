#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The app spends the per-user spawn budget its agent servers and hooks
    // share. First, ahead of the headless cleanup branch below, which spawns
    // git too and never reaches `run()`.
    gitpulse_lib::engine::process::run_process_with_shared_spawn_budget();
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--cleaner-due")
    {
        // launchd runs this every 60 s and it usually finds nothing due. A
        // wake that logs nothing leaves the app's log as it was; a raised
        // limit is routine, and only one that fell short is worth a line.
        gitpulse_lib::logging::init_for_headless_tick();
        let _signals = gitpulse_lib::procguard::install_signal_handlers();
        let limits = gitpulse_lib::limits::raise_open_file_limit();
        if !limits.is_sufficient() {
            log::warn!("{}", limits.describe());
        }
        let outcome = gitpulse_lib::storage::hygiene::global::run_due_headless();
        if let Err(error) = &outcome {
            log::error!("Background cleanup: {error}", error = error.message());
        }
        gitpulse_lib::procguard::exit(if outcome.is_ok() { 0 } else { 1 });
    }
    gitpulse_lib::run();
    // The window closing is a normal return, but a `kill` or a logout is not:
    // exiting through `procguard` is what stops that return from overtaking a
    // shutdown sweep that is still killing this session's git subprocesses.
    gitpulse_lib::procguard::exit(0);
}
