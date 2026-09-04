//! `gitpulsed` — GitPulse's control plane without a window.
//!
//! # Why this exists
//!
//! Attribution catch-up — reading agent transcripts and the reflog and turning
//! them into durable, attributed ledger rows — only ran when the desktop app
//! opened a repository. An agent working for six hours with GitPulse closed
//! produced a transcript nothing ingested, so the ledger's answer to "who
//! changed this, and when" had a six-hour hole in it that nothing on screen
//! said was there. The record is supposed to be the thing that does not depend
//! on someone having the UI open.
//!
//! So this is a small, bounded loop: for each repository named on the command
//! line, run the same `ingest::catch_up` the app runs, on an interval, and say
//! what it did on stdout as NDJSON.
//!
//! # What it deliberately is not
//!
//! It does not serve requests. `gitpulse-mcp` already does that, over stdio,
//! and a second request surface answering the same questions from the same
//! store would be a second thing to keep in step. This process only *writes*
//! what nothing else was writing.
//!
//! It does not take a lease, check out a task, or write a file. Those belong
//! to DevCouncil and Manvi, and a background process that took a writer lease
//! would contend with the agent actually doing the work.
//!
//! # Interruption is safe, so there is no signal handler
//!
//! Every append is one SQLite transaction against a WAL database, so a process
//! killed mid-cycle leaves a consistent ledger. Catch-up is idempotent against
//! a watermark read back out of the ledger itself, so the next cycle re-reads
//! whatever the interrupted one did not finish and writes it exactly once.
//! Adding a signal-handling dependency would buy a tidier log line and nothing
//! else.

use gitpulse_lib::engine::git_cli::validate_repo;
use gitpulse_lib::{ingest, ledger};
use std::time::{Duration, Instant};

/// Floor on `--interval`.
///
/// Each cycle walks the reflog and the transcript corpus for every repository,
/// which is real IO — the corpus this was measured against is 1.8 GB. A
/// caller asking for a one-second loop gets this instead, and is told, rather
/// than being given a process that pins a core.
const MIN_INTERVAL: Duration = Duration::from_secs(15);

/// Ceiling on `--interval`, so a typo cannot park the daemon for a year.
const MAX_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

const DEFAULT_INTERVAL: Duration = Duration::from_secs(300);

/// Repositories one invocation will watch.
///
/// Each costs a reflog walk and a transcript scan per cycle. The cap is here
/// so the cycle time stays something a caller can reason about; past it the
/// invocation is refused rather than silently watching a prefix.
const MAX_REPOS: usize = 32;

const USAGE: &str = "\
gitpulsed — GitPulse attribution daemon

USAGE:
    gitpulsed [OPTIONS] <REPO>...

Runs transcript and reflog catch-up for each repository on an interval, so the
ledger keeps recording who changed what while the desktop app is closed.

ARGS:
    <REPO>...   Absolute paths to git repositories (at most 32)

OPTIONS:
    --interval <SECS>   Seconds between cycles (default 300, min 15, max 86400)
    --once              Run one cycle and exit
    --help              Print this message

OUTPUT:
    One NDJSON object per repository per cycle on stdout:
      {\"repo\":\"…\",\"cycle\":1,\"recorded\":3,\"transcripts\":2,\"skipped_lines\":0,
       \"reflog_entries\":7,\"recording\":true,\"error\":\"\",\"elapsed_ms\":42}

    `recording` is the ledger's own status. A cycle that recorded nothing
    because the ledger could not be opened is not the same as one that
    recorded nothing because nothing happened, and both appear here.

EXIT:
    0  every cycle completed (or --once completed)
    2  the arguments could not be understood
";

#[derive(Debug, PartialEq)]
struct Config {
    repos: Vec<String>,
    interval: Duration,
    once: bool,
}

/// What parsing the command line produced.
#[derive(Debug, PartialEq)]
enum Parsed {
    Run(Config),
    Help,
    /// Refused, with the reason. Never a silently-corrected value: an interval
    /// clamped without saying so is a daemon running at a cadence its operator
    /// did not choose and cannot see.
    Error(String),
}

fn parse(argv: &[String]) -> Parsed {
    let mut repos = Vec::new();
    let mut interval = DEFAULT_INTERVAL;
    let mut once = false;
    let mut i = 0;

    while i < argv.len() {
        match argv[i].as_str() {
            "--help" | "-h" => return Parsed::Help,
            "--once" => once = true,
            "--interval" => {
                let Some(raw) = argv.get(i + 1) else {
                    return Parsed::Error("--interval needs a number of seconds".into());
                };
                let Ok(secs) = raw.parse::<u64>() else {
                    return Parsed::Error(format!(
                        "--interval: {raw:?} is not a number of seconds"
                    ));
                };
                let requested = Duration::from_secs(secs);
                if requested < MIN_INTERVAL {
                    return Parsed::Error(format!(
                        "--interval {secs}s is below the {}s floor; each cycle walks every \
                         repository's reflog and transcript corpus",
                        MIN_INTERVAL.as_secs()
                    ));
                }
                if requested > MAX_INTERVAL {
                    return Parsed::Error(format!(
                        "--interval {secs}s is above the {}s ceiling",
                        MAX_INTERVAL.as_secs()
                    ));
                }
                interval = requested;
                i += 1;
            }
            other if other.starts_with('-') => {
                return Parsed::Error(format!("unknown option {other:?}"));
            }
            path => repos.push(path.to_string()),
        }
        i += 1;
    }

    if repos.is_empty() {
        // Never a default set. Guessing which repositories to watch — from a
        // recent-repos file, say — would have this process quietly reading
        // transcripts for repositories its operator did not name.
        return Parsed::Error("name at least one repository to watch".into());
    }
    if repos.len() > MAX_REPOS {
        return Parsed::Error(format!(
            "{} repositories given; the limit is {MAX_REPOS}",
            repos.len()
        ));
    }
    if let Some(relative) = repos
        .iter()
        .find(|p| !std::path::Path::new(p).is_absolute())
    {
        return Parsed::Error(format!(
            "{relative:?} is not an absolute path; a daemon has no useful working directory"
        ));
    }

    Parsed::Run(Config {
        repos,
        interval,
        once,
    })
}

/// One repository's result for one cycle.
#[derive(Debug, serde::Serialize)]
struct CycleReport {
    repo: String,
    cycle: u64,
    recorded: i64,
    transcripts: i64,
    skipped_lines: i64,
    reflog_entries: i64,
    /// The ledger's own status. A cycle that recorded nothing because the
    /// ledger could not be opened must never read like one that recorded
    /// nothing because nothing happened.
    recording: bool,
    /// Empty when the cycle completed; otherwise what stopped it. Carries the
    /// ledger's error when the ledger is the thing that failed.
    error: String,
    elapsed_ms: u64,
}

fn run_cycle(repo: &str, cycle: u64) -> CycleReport {
    let started = Instant::now();

    // Validated before anything is opened. `ledger::status` *creates*
    // `.devcouncil/ledger.sqlite` as a side effect of answering, so calling it
    // on a path that is not a repository leaves a ledger in a directory that
    // will never have anything to record — and then reports `recording: true`
    // for it. The daemon takes paths straight from an operator's command line,
    // so unlike the app it cannot assume they were already checked.
    let address = validate_repo(repo).and_then(|canonical| {
        gitpulse_lib::ledger::bindings::repository_address(&canonical.to_string_lossy())
            .map_err(|error| error.to_string())
    });
    let address = match address {
        Ok(address) => address,
        Err(reason) => {
            return CycleReport {
                repo: repo.to_string(),
                cycle,
                recorded: 0,
                transcripts: 0,
                skipped_lines: 0,
                reflog_entries: 0,
                recording: false,
                error: reason,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            };
        }
    };

    let status = ledger::status(&address.anchor);
    let caught = ingest::catch_up_into(&address.anchor, &address.worktree);
    CycleReport {
        repo: repo.to_string(),
        cycle,
        recorded: caught.recorded,
        transcripts: caught.transcripts,
        skipped_lines: caught.skipped_lines,
        reflog_entries: caught.reflog_entries,
        recording: status.recording,
        error: if caught.error.is_empty() {
            status.error
        } else {
            caught.error
        },
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

fn main() {
    // The daemon runs unattended for hours, which is precisely when nobody is
    // watching stderr. Without this it had no durable record and no panic
    // hook at all: a crash mid-catch-up left the ledger hole it exists to
    // fill, and nothing anywhere that said why.
    gitpulse_lib::logging::init();
    gitpulse_lib::logging::install_panic_hook();
    // Unattended catch-up walks every repository it was given, so it is the
    // binary most likely to run out of descriptors and the least likely to
    // have anyone watching when it does.
    log::info!(target: "setup", "{}", gitpulse_lib::limits::raise_open_file_limit().describe());
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse(&argv) {
        Parsed::Help => {
            print!("{USAGE}");
        }
        Parsed::Error(reason) => {
            eprintln!("gitpulsed: {reason}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
        Parsed::Run(config) => {
            let mut cycle = 0u64;
            loop {
                cycle += 1;
                for repo in &config.repos {
                    let report = run_cycle(repo, cycle);
                    match serde_json::to_string(&report) {
                        Ok(line) => println!("{line}"),
                        // Serialising our own struct cannot fail, but a panic
                        // here would take down a daemon over a log line.
                        Err(e) => eprintln!("gitpulsed: could not report cycle: {e}"),
                    }
                }
                use std::io::Write;
                let _ = std::io::stdout().flush();
                if config.once {
                    return;
                }
                std::thread::sleep(config.interval);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// An absolute repository path for the host platform.
    ///
    /// `/a` is absolute on Unix but merely *drive-relative* on Windows, where
    /// it resolves against whichever drive the process happens to be on --
    /// exactly the working-directory dependence `parse` exists to refuse. A
    /// fixture that needs a path `parse` will accept therefore has to name one
    /// per platform instead of sharing a Unix literal.
    #[cfg(windows)]
    fn abs(tail: &str) -> String {
        format!("C:\\{}", tail.replace('/', "\\"))
    }
    #[cfg(not(windows))]
    fn abs(tail: &str) -> String {
        format!("/{tail}")
    }

    /// Serialises the tests that override the transcript root.
    ///
    /// The override is an environment variable, which is process-global: two
    /// tests setting and clearing it in parallel let one of them run against
    /// the developer's real `~/.claude/projects`.
    static ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_transcript_root<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let _guard = ROOT_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("GITPULSE_TRANSCRIPT_ROOT", root);
        let out = f();
        std::env::remove_var("GITPULSE_TRANSCRIPT_ROOT");
        out
    }

    #[test]
    fn parses_repositories_and_defaults() {
        let (a, b) = (abs("a"), abs("b"));
        let Parsed::Run(c) = parse(&args(&[&a, &b])) else {
            panic!("expected a run");
        };
        assert_eq!(c.repos, vec![a, b]);
        assert_eq!(c.interval, DEFAULT_INTERVAL);
        assert!(!c.once);
    }

    #[test]
    fn accepts_once_and_an_interval_in_any_order() {
        let a = abs("a");
        let Parsed::Run(c) = parse(&args(&["--once", &a, "--interval", "60"])) else {
            panic!("expected a run");
        };
        assert!(c.once);
        assert_eq!(c.interval, Duration::from_secs(60));
        assert_eq!(c.repos, vec![a]);
    }

    /// An interval quietly clamped is a daemon running at a cadence its
    /// operator did not choose and cannot see. Refusing is the honest answer.
    #[test]
    fn an_out_of_range_interval_is_refused_not_clamped() {
        let a = abs("a");
        let Parsed::Error(low) = parse(&args(&[&a, "--interval", "1"])) else {
            panic!("expected a refusal");
        };
        assert!(low.contains("floor"), "{low}");
        assert!(low.contains("15"), "{low}");

        let Parsed::Error(high) = parse(&args(&[&a, "--interval", "999999999"])) else {
            panic!("expected a refusal");
        };
        assert!(high.contains("ceiling"), "{high}");

        // And the floor itself is accepted, so the boundary is usable.
        assert!(matches!(
            parse(&args(&[&a, "--interval", "15"])),
            Parsed::Run(_)
        ));
    }

    #[test]
    fn a_missing_or_unparseable_interval_is_named() {
        assert!(matches!(
            parse(&args(&[&abs("a"), "--interval"])),
            Parsed::Error(_)
        ));
        let Parsed::Error(e) = parse(&args(&[&abs("a"), "--interval", "soon"])) else {
            panic!("expected a refusal");
        };
        assert!(e.contains("soon"), "{e}");
    }

    /// Guessing a repository set — from a recent-repos file, say — would have
    /// this process reading transcripts for repositories nobody named.
    #[test]
    fn no_repository_means_refusal_never_a_default_set() {
        let Parsed::Error(e) = parse(&args(&["--once"])) else {
            panic!("expected a refusal");
        };
        assert!(e.contains("at least one repository"), "{e}");
    }

    /// The fixtures above only mean what they say if `abs` really yields an
    /// absolute path on the host running them. A Unix literal silently stops
    /// being one on Windows, which is how four of these tests failed there
    /// while reading as ordinary parse bugs.
    #[test]
    fn the_absolute_fixture_is_absolute_on_this_host() {
        for tail in ["a", "b", "repo/0"] {
            let path = abs(tail);
            assert!(
                std::path::Path::new(&path).is_absolute(),
                "abs({tail:?}) produced {path:?}, which this platform does not \
                 consider absolute"
            );
        }
        assert_eq!(
            std::path::Path::new("/a").is_absolute(),
            cfg!(not(windows)),
            "a Unix path literal is absolute only off Windows; fixtures that \
             need `parse` to accept a path must go through abs()"
        );
    }

    #[test]
    fn a_relative_path_is_refused_because_a_daemon_has_no_cwd_worth_using() {
        let Parsed::Error(e) = parse(&args(&["./repo"])) else {
            panic!("expected a refusal");
        };
        assert!(e.contains("absolute"), "{e}");
    }

    #[test]
    fn too_many_repositories_is_refused_rather_than_truncated() {
        // Watching a silent prefix of what was asked for is the shape of every
        // bug this codebase keeps finding: a capped sample presented as
        // complete coverage.
        let many: Vec<String> = (0..MAX_REPOS + 1)
            .map(|i| abs(&format!("repo/{i}")))
            .collect();
        let Parsed::Error(e) = parse(&many) else {
            panic!("expected a refusal");
        };
        assert!(e.contains(&format!("{MAX_REPOS}")), "{e}");

        let at_limit: Vec<String> = (0..MAX_REPOS).map(|i| abs(&format!("repo/{i}"))).collect();
        assert!(matches!(parse(&at_limit), Parsed::Run(_)));
    }

    #[test]
    fn unknown_options_are_refused_rather_than_treated_as_paths() {
        // Without this, `gitpulsed --interva1 60 /repo` would watch a
        // repository called "--interva1" and one called "60".
        let Parsed::Error(e) = parse(&args(&["--interva1", "60", "/repo"])) else {
            panic!("expected a refusal");
        };
        assert!(e.contains("--interva1"), "{e}");
    }

    #[test]
    fn help_is_a_first_class_answer_and_documents_every_option() {
        assert_eq!(parse(&args(&["--help"])), Parsed::Help);
        assert_eq!(parse(&args(&[&abs("a"), "-h"])), Parsed::Help);
        for flag in ["--interval", "--once", "--help"] {
            assert!(USAGE.contains(flag), "usage does not document {flag}");
        }
        assert!(USAGE.contains(&format!("{}", MIN_INTERVAL.as_secs())));
        assert!(USAGE.contains(&format!("{MAX_REPOS}")));
    }

    /// A cycle over a directory that is not a repository must report why,
    /// never a clean zero — and must not leave a ledger behind in it.
    ///
    /// This failed when first written: `run_cycle` called `ledger::status`
    /// first, which creates `.devcouncil/ledger.sqlite` as a side effect of
    /// answering and then reported `recording: true` for a directory with no
    /// git in it at all.
    #[test]
    fn a_cycle_on_a_non_repository_reports_rather_than_reading_as_quiet() {
        let dir = tempfile::tempdir().expect("tempdir");
        let report = run_cycle(dir.path().to_str().expect("utf8"), 1);
        assert_eq!(report.recorded, 0);
        assert!(!report.recording, "there is no ledger here");
        assert!(
            report.error.contains("Not a Git repository"),
            "a cycle that could not run must say so, not report a quiet zero: {:?}",
            report.error
        );
        assert!(
            !dir.path().join(".devcouncil").exists(),
            "a rejected path must not be left with a ledger in it"
        );
    }

    /// A real repository, so the happy path is exercised rather than assumed.
    ///
    /// The transcript root is pointed at an empty directory: without that,
    /// catch-up walks the developer's real `~/.claude/projects`, which is a
    /// 1.8 GB scan and makes the result depend on what is on the machine.
    #[test]
    fn a_cycle_on_a_real_repository_records_and_reports_recording() {
        let corpus = tempfile::tempdir().expect("tempdir");
        let repo = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "T"]);
        git(&["config", "user.email", "t@e.com"]);
        git(&["commit", "--allow-empty", "-m", "c0"]);

        let report = with_transcript_root(corpus.path(), || {
            run_cycle(repo.path().to_str().expect("utf8"), 1)
        });
        assert_eq!(report.error, "", "a clean repository has nothing to report");
        assert!(report.recording, "the ledger opened");
        assert!(
            report.reflog_entries >= 1,
            "the initial commit is in the reflog, got {}",
            report.reflog_entries
        );
        assert!(report.recorded >= 1, "and it reached the ledger");

        // Idempotent: the watermark is read back out of the ledger, so a second
        // cycle re-reads the same reflog and writes nothing.
        let again = with_transcript_root(corpus.path(), || {
            run_cycle(repo.path().to_str().expect("utf8"), 2)
        });
        assert_eq!(again.recorded, 0, "a second cycle must not duplicate rows");
        assert!(again.recording);
    }

    #[test]
    fn a_daemon_cycle_consolidates_pre_family_sibling_history_once() {
        let corpus = tempfile::tempdir().expect("transcript corpus");
        let repo = tempfile::tempdir().expect("repository");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=GitPulse",
                    "-c",
                    "user.email=gitpulse@test.local",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(repo.path())
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["commit", "--allow-empty", "-m", "seed"]);
        let parent = tempfile::tempdir().expect("worktree parent");
        let linked = parent.path().join("linked");
        git(&[
            "worktree",
            "add",
            "--detach",
            linked.to_str().expect("utf8 linked path"),
        ]);
        let linked = linked.canonicalize().expect("canonical linked worktree");
        gitpulse_lib::ledger::append(gitpulse_lib::ledger::Draft {
            repo_path: linked.to_string_lossy().into_owned(),
            worktree_path: Some(linked.to_string_lossy().into_owned()),
            action: "git.commit".to_string(),
            actor_kind: Some(gitpulse_lib::ledger::ActorKind::Human),
            outcome: Some(gitpulse_lib::ledger::Outcome::Ok),
            ..Default::default()
        })
        .expect("legacy sibling row");
        let legacy_ulid = gitpulse_lib::ledger::tail(&linked.to_string_lossy(), 0, 10)
            .expect("legacy ledger")
            .remove(0)
            .ulid;

        let main = repo.path().canonicalize().expect("canonical repository");
        let first = with_transcript_root(corpus.path(), || {
            run_cycle(main.to_str().expect("utf8 repository"), 1)
        });
        assert!(first.recording, "{first:?}");
        let second = with_transcript_root(corpus.path(), || {
            run_cycle(main.to_str().expect("utf8 repository"), 2)
        });
        assert!(second.recording, "{second:?}");
        let rows =
            gitpulse_lib::ledger::tail(&main.to_string_lossy(), 0, 1000).expect("family ledger");
        let imported: Vec<_> = rows
            .iter()
            .filter(|event| event.ulid == legacy_ulid)
            .collect();
        assert_eq!(
            imported.len(),
            1,
            "daemon retries must not duplicate import"
        );
        assert_eq!(
            imported[0].worktree_path.as_deref(),
            Some(gitpulse_lib::ledger::redact::text(&linked.to_string_lossy()).as_str())
        );
    }

    #[test]
    fn a_cycle_report_serialises_every_field_the_usage_promises() {
        let dir = tempfile::tempdir().expect("tempdir");
        let json =
            serde_json::to_string(&run_cycle(dir.path().to_str().unwrap(), 7)).expect("serialise");
        for field in [
            "repo",
            "cycle",
            "recorded",
            "transcripts",
            "skipped_lines",
            "reflog_entries",
            "recording",
            "error",
            "elapsed_ms",
        ] {
            assert!(json.contains(&format!("\"{field}\"")), "missing {field}");
            assert!(
                USAGE.contains(field),
                "the usage text does not document {field}"
            );
        }
    }
}
