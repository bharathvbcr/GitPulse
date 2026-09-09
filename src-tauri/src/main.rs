#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--cleaner-due")
    {
        gitpulse_lib::logging::init();
        let _signals = gitpulse_lib::procguard::install_signal_handlers();
        let limits = gitpulse_lib::limits::raise_open_file_limit();
        log::info!("{}", limits.describe());
        let outcome = gitpulse_lib::storage::hygiene::global::run_due_headless();
        if let Err(error) = &outcome {
            log::error!("Background cleanup: {error}");
        }
        gitpulse_lib::procguard::exit(if outcome.is_ok() { 0 } else { 1 });
    }
    gitpulse_lib::run();
    // The window closing is a normal return, but a `kill` or a logout is not:
    // exiting through `procguard` is what stops that return from overtaking a
    // shutdown sweep that is still killing this session's git subprocesses.
    gitpulse_lib::procguard::exit(0);
}
