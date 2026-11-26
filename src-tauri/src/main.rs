#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Usage text for `--help` / `-h`. Without this the flag falls through to
/// `waylode_lib::run()`, whose single-instance handoff raises the running
/// app's window. `--cleaner-due` is deliberately omitted: it is launchd
/// plumbing, not something a person types.
const USAGE: &str = "waylode: the Waylode desktop app.\n\
\n\
Run it with no arguments to open the app; it takes none for normal use.\n\
The command-line tools are separate binaries: `waylode-mcp` (tools, describe,\n\
call, read) queries and drives Waylode from a shell, and `waylode-hook` is the\n\
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

/// Writes `text` and flushes. The result is dropped on purpose: a reader that
/// has already gone away (EPIPE) must not turn `--help` into a panic, since
/// Rust ignores SIGPIPE and `print!` would abort with exit 101.
fn write_usage(out: &mut impl std::io::Write, text: &str) {
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

fn main() {
    // The app spends the per-user spawn budget its agent servers and hooks
    // share. First, ahead of the headless cleanup branch below, which spawns
    // git too and never reaches `run()`. Opting in spawns nothing, so
    // `--help` below can still answer without ever starting the app.
    waylode_lib::engine::process::run_process_with_shared_spawn_budget();
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some((text, code)) = early_exit(&args) {
        write_usage(&mut std::io::stdout().lock(), &text);
        std::process::exit(code);
    }
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--cleaner-due")
    {
        // launchd runs this every 60 s and it usually finds nothing due. A
        // wake that logs nothing leaves the app's log as it was; a raised
        // limit is routine, and only one that fell short is worth a line.
        waylode_lib::logging::init_for_headless_tick();
        let _signals = waylode_lib::procguard::install_signal_handlers();
        let limits = waylode_lib::limits::raise_open_file_limit();
        if !limits.is_sufficient() {
            log::warn!("{}", limits.describe());
        }
        let outcome = waylode_lib::storage::hygiene::global::run_due_headless();
        if let Err(error) = &outcome {
            log::error!("Background cleanup: {error}", error = error.message());
        }
        waylode_lib::procguard::exit(if outcome.is_ok() { 0 } else { 1 });
    }
    waylode_lib::run();
    // The window closing is a normal return, but a `kill` or a logout is not:
    // exiting through `procguard` is what stops that return from overtaking a
    // shutdown sweep that is still killing this session's git subprocesses.
    waylode_lib::procguard::exit(0);
}

#[cfg(test)]
mod tests {
    use super::write_usage;

    struct BrokenPipe;

    impl std::io::Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn closed_reader_does_not_panic_and_open_reader_gets_the_text() {
        write_usage(&mut BrokenPipe, "usage");
        let mut sink = Vec::new();
        write_usage(&mut sink, "usage");
        assert_eq!(sink, b"usage");
    }

    use super::{early_exit, USAGE};
    use std::ffi::OsString;

    fn args(rest: &[&str]) -> Vec<OsString> {
        rest.iter().map(OsString::from).collect()
    }

    #[test]
    fn help_flags_print_usage_and_exit_zero() {
        for flag in ["--help", "-h"] {
            let (text, code) = early_exit(&args(&[flag])).expect("help is handled early");
            assert_eq!(code, 0, "{flag}");
            assert!(text.contains("waylode-mcp"), "{flag}: {text}");
            assert!(text.contains("waylode-hook"), "{flag}: {text}");
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

    #[cfg(unix)]
    #[test]
    fn non_utf8_first_argument_falls_through_without_panicking() {
        use std::os::unix::ffi::OsStringExt;
        for bytes in [
            vec![0xff],
            vec![b'-', b'-', b'h', b'e', b'l', b'p', 0xff],
            vec![0xc0, 0x80],
        ] {
            assert!(
                early_exit(&[OsString::from_vec(bytes.clone())]).is_none(),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn help_with_trailing_arguments_still_prints_usage() {
        // Pinned: only the first argument is read, so `--help extra` is help.
        for case in [
            &["--help", "extra"][..],
            &["-h", "--cleaner-due"],
            &["--help", "--help"],
        ] {
            let (_, code) = early_exit(&args(case)).expect("first argument decides");
            assert_eq!(code, 0, "{case:?}");
        }
    }

    #[test]
    fn near_miss_spellings_fall_through_to_the_app() {
        // Exact, case-sensitive match: none of these is help.
        for case in [
            &["-h="][..],
            &["--help="],
            &["--HELP"],
            &["-H"],
            &["-help"],
            &["--", "--help"],
            &[""],
            &[" --help"],
            &["--help "],
        ] {
            assert!(early_exit(&args(case)).is_none(), "{case:?}");
        }
    }

    #[test]
    fn usage_text_is_plain_ascii_lines_naming_real_tools() {
        assert!(USAGE.is_ascii());
        assert!(USAGE.ends_with('\n'));
        for name in ["waylode-mcp", "waylode-hook"] {
            assert!(USAGE.contains(name), "{name}");
        }
        // Every one-shot command the text names is in waylode-mcp's CLI_COMMANDS.
        let mcp = include_str!("bin/waylode-mcp.rs");
        assert!(mcp
            .contains(r#"const CLI_COMMANDS: [&str; 4] = ["tools", "describe", "call", "read"];"#));
        for command in ["tools", "describe", "call", "read"] {
            assert!(USAGE.contains(command), "{command}");
        }
        assert!(!USAGE.contains("--cleaner-due"));
    }
}
