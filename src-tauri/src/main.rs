#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Usage text for `--help` / `-h`. Without this the flag falls through to
/// `gitpulse_lib::run()`, whose single-instance handoff raises the running
/// app's window. `--cleaner-due` is deliberately omitted: it is launchd
/// plumbing, not something a person types.
const USAGE: &str = "gitpulse: the GitPulse desktop app.\n\
\n\
Run it with no arguments to open the app; it takes none for normal use.\n\
The command-line tools are separate binaries: `gitpulse-mcp` (tools, describe,\n\
call, read) queries and drives GitPulse from a shell, and `gitpulse-hook` is the\n\
hook executable agent hosts invoke.\n";

/// Arguments that are answered before the app starts: `--help` and `-h`, as
/// the first argument only. Every other invocation, including none, launchd's
/// `--cleaner-due` and LaunchServices' `-psn_*`, returns `None` and behaves
/// as it always has.
fn early_exit(args: &[std::ffi::OsString]) -> Option<(String, i32)> {
    match args.first().and_then(|arg| arg.to_str()) {
        Some("--help" | "-h") => Some((USAGE.to_owned(), 0)),
        _ => None,
    }
}

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some((text, code)) = early_exit(&args) {
        print!("{text}");
        std::process::exit(code);
    }
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

#[cfg(test)]
mod tests {
    use super::early_exit;
    use std::ffi::OsString;

    fn args(rest: &[&str]) -> Vec<OsString> {
        rest.iter().map(OsString::from).collect()
    }

    #[test]
    fn help_flags_print_usage_and_exit_zero() {
        for flag in ["--help", "-h"] {
            let (text, code) = early_exit(&args(&[flag])).expect("help is handled early");
            assert_eq!(code, 0, "{flag}");
            assert!(text.contains("gitpulse-mcp"), "{flag}: {text}");
            assert!(text.contains("gitpulse-hook"), "{flag}: {text}");
        }
    }

    #[test]
    fn every_other_invocation_falls_through_to_the_app() {
        let cases: [&[&str]; 6] = [
            &[],
            &["--cleaner-due"],
            &["-psn_0_1"],
            &["--helpx"],
            &["help"],
            &["/tmp/some/file"],
        ];
        for case in cases {
            assert!(early_exit(&args(case)).is_none(), "{case:?}");
        }
    }

    #[test]
    fn help_is_recognised_only_as_the_first_argument() {
        assert!(early_exit(&args(&["--cleaner-due", "--help"])).is_none());
    }
}
