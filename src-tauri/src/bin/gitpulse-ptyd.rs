//! `gitpulse-ptyd` — the process that holds GitPulse's terminals.
//!
//! The app starts it (in a session of its own) when "Keep terminals running
//! when GitPulse quits" is on, and talks to it over a private unix socket.
//! Because the PTY masters live here, a GitPulse that quits, crashes or is
//! killed takes no terminal with it; the next GitPulse finds them again. See
//! `docs/TERMINAL_HOST.md`.
//!
//! It is not meant to be run by hand, but it can be: `--dir` names the host
//! directory (socket, lock, receipts), and a host with no app and no sessions
//! for `--idle-exit-ms` exits on its own.

use std::time::Duration;

const USAGE: &str = "\
gitpulse-ptyd — GitPulse terminal host

USAGE:
    gitpulse-ptyd [--dir <PATH>] [--idle-exit-ms <MS>]

OPTIONS:
    --dir <PATH>          The host directory (default: GitPulse's config directory, pty-host)
    --idle-exit-ms <MS>   Exit after this long with no app and no sessions
                          (default 30000, min 1000, max 3600000)
    --version             Print the build and protocol, and exit
    --help                Print this message

EXIT:
    0  idle, or retired by a newer app
    1  could not start (directory, socket, receipts)
    2  the arguments could not be understood, or this platform has no host
    3  another host already serves this protocol in that directory
";

const MIN_IDLE_EXIT: Duration = Duration::from_secs(1);
const MAX_IDLE_EXIT: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, PartialEq)]
enum Parsed {
    Help,
    Version,
    Run {
        dir: Option<std::path::PathBuf>,
        idle_exit: Option<Duration>,
    },
    Error(String),
}

fn parse(argv: &[String]) -> Parsed {
    let mut dir = None;
    let mut idle_exit = None;
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Parsed::Help,
            "--version" => return Parsed::Version,
            "--dir" => match args.next() {
                Some(path) if std::path::Path::new(path).is_absolute() => {
                    dir = Some(std::path::PathBuf::from(path))
                }
                Some(path) => {
                    return Parsed::Error(format!("--dir must be an absolute path, not {path:?}"))
                }
                None => return Parsed::Error("--dir needs a path".into()),
            },
            "--idle-exit-ms" => match args.next().and_then(|ms| ms.parse::<u64>().ok()) {
                Some(ms) => {
                    let wanted = Duration::from_millis(ms);
                    if !(MIN_IDLE_EXIT..=MAX_IDLE_EXIT).contains(&wanted) {
                        return Parsed::Error(format!(
                            "--idle-exit-ms must be between {} and {}",
                            MIN_IDLE_EXIT.as_millis(),
                            MAX_IDLE_EXIT.as_millis()
                        ));
                    }
                    idle_exit = Some(wanted);
                }
                None => {
                    return Parsed::Error("--idle-exit-ms needs a number of milliseconds".into())
                }
            },
            other => return Parsed::Error(format!("unexpected argument {other:?}")),
        }
    }
    Parsed::Run { dir, idle_exit }
}

fn main() {
    // From the per-user spawn budget every GitPulse process shares, before any
    // spawn builds the gate.
    gitpulse_lib::engine::git_cli::run_process_with_shared_spawn_budget();
    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
    // Two descriptors per terminal, for every terminal the user has open.
    log::info!(target: "setup", "{}", gitpulse_lib::limits::raise_open_file_limit().describe());
    // A signal stops every session with the hangup grace and records each
    // exit (the shutdown hook `serve` registers), then exits.
    let signals = gitpulse_lib::procguard::install_signal_handlers();
    if signals.is_armed() {
        log::info!(target: "setup", "{}", signals.describe());
    } else {
        log::warn!(target: "setup", "{}", signals.describe());
    }
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse(&argv) {
        Parsed::Help => {
            print!("{USAGE}");
            0
        }
        Parsed::Version => {
            #[cfg(unix)]
            let protocol = gitpulse_lib::terminal::host::PROTOCOL.to_string();
            #[cfg(not(unix))]
            let protocol = "none".to_string();
            println!(
                "gitpulse-ptyd {} protocol {protocol}",
                env!("CARGO_PKG_VERSION")
            );
            0
        }
        Parsed::Error(reason) => {
            eprintln!("gitpulse-ptyd: {reason}; run gitpulse-ptyd --help for usage");
            2
        }
        Parsed::Run { dir, idle_exit } => run(dir, idle_exit),
    };
    gitpulse_lib::procguard::exit(code);
}

#[cfg(unix)]
fn run(dir: Option<std::path::PathBuf>, idle_exit: Option<Duration>) -> i32 {
    use gitpulse_lib::terminal::host;
    let Some(dir) = dir.or_else(host::default_dir) else {
        eprintln!("gitpulse-ptyd: the application config directory is unavailable; pass --dir");
        return host::server::exit_code::FAILED;
    };
    host::server::serve(host::server::ServeConfig {
        dir,
        idle_exit: idle_exit.unwrap_or(host::server::DEFAULT_IDLE_EXIT),
    })
}

#[cfg(not(unix))]
fn run(_dir: Option<std::path::PathBuf>, _idle_exit: Option<Duration>) -> i32 {
    eprintln!("gitpulse-ptyd: this platform has no terminal host; terminals stay in the app");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arguments_are_read_strictly() {
        assert_eq!(
            parse(&args(&[])),
            Parsed::Run {
                dir: None,
                idle_exit: None
            }
        );
        assert_eq!(
            parse(&args(&["--dir", "/x/y", "--idle-exit-ms", "2500"])),
            Parsed::Run {
                dir: Some("/x/y".into()),
                idle_exit: Some(Duration::from_millis(2500))
            }
        );
        assert_eq!(parse(&args(&["--help"])), Parsed::Help);
        assert_eq!(parse(&args(&["--version"])), Parsed::Version);
        for bad in [
            &["--dir"][..],
            &["--dir", "relative"],
            &["--idle-exit-ms", "5"],
            &["--idle-exit-ms", "999999999"],
            &["--idle-exit-ms", "soon"],
            &["--repo", "/x"],
        ] {
            assert!(matches!(parse(&args(bad)), Parsed::Error(_)), "{bad:?}");
        }
    }
}
