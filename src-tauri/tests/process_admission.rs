//! The process-wide admission default, in a process of its own: setting it in
//! the unit-test binary would reclassify every other test's spawns. The same
//! holds for the shared spawn budget opt-in, which the unit-test build cannot
//! even name.

mod common;

use common::{env, shipped_binaries};

use std::path::Path;

use gitpulse_lib::engine::git_cli::{
    current_admission_name, run_process_as_background, run_process_with_shared_spawn_budget,
};

#[test]
fn a_background_process_starts_every_thread_in_the_background_class() {
    // A thread that asked before the switch keeps the app's default...
    assert_eq!(
        std::thread::spawn(current_admission_name).join().unwrap(),
        "reactive"
    );
    run_process_as_background();
    // ...and every thread started after it — a request worker, a pool
    // thread behind a `par_iter` — begins shed-first.
    assert_eq!(
        std::thread::spawn(current_admission_name).join().unwrap(),
        "background"
    );
    let nested = std::thread::spawn(|| std::thread::spawn(current_admission_name).join().unwrap());
    assert_eq!(nested.join().unwrap(), "background");
}

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The process-wide switches at the top of `bin`'s `main`: the consecutive
/// `run_process_*` calls before its first other statement, comments skipped.
/// Being in this block is what "before anything else can start a thread or
/// spawn" means; order within it does not matter.
fn leading_switches(bin: &str) -> Vec<String> {
    let source = std::fs::read_to_string(manifest_dir().join(bin)).unwrap();
    let main = source
        .find("fn main() {")
        .unwrap_or_else(|| panic!("{bin}: no main"));
    source[main..]
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .take_while(|line| line.starts_with("gitpulse_lib::engine::git_cli::run_process_"))
        .map(str::to_string)
        .collect()
}

const BACKGROUND_SWITCH: &str = "gitpulse_lib::engine::git_cli::run_process_as_background();";
const SHARED_BUDGET_SWITCH: &str =
    "gitpulse_lib::engine::git_cli::run_process_with_shared_spawn_budget();";

/// The binaries whose work is optional must make the switch, and make it
/// before anything else in `main` can start a thread.
#[test]
fn the_agent_server_and_the_daemon_run_as_background() {
    let binaries = shipped_binaries();
    for bin in ["src/bin/gitpulse-mcp.rs", "src/bin/gitpulsed.rs"] {
        assert!(
            binaries.contains(bin),
            "{bin} is no longer a shipped binary"
        );
        assert!(
            leading_switches(bin).iter().any(|s| s == BACKGROUND_SWITCH),
            "{bin}: the class switch must open main, ahead of any other statement"
        );
    }
    // The app and the hooks keep the default: the hook gates the user's own
    // agent tool calls, and the app is what the user is looking at.
    for bin in ["src/main.rs", "src/bin/gitpulse-hook.rs"] {
        let source = std::fs::read_to_string(manifest_dir().join(bin)).unwrap();
        assert!(!source.contains("run_process_as_background"), "{bin}");
    }
}

/// The agent server's reads have a caller waiting on them, so its background
/// spawns wait a bounded time for budget instead of being shed; the daemon's
/// scheduled work has no one waiting and keeps shedding.
#[test]
fn the_agent_server_waits_for_background_budget_and_the_daemon_does_not() {
    const AWAIT_SWITCH: &str =
        "gitpulse_lib::engine::git_cli::run_process_awaiting_background_budget();";
    assert!(leading_switches("src/bin/gitpulse-mcp.rs")
        .iter()
        .any(|s| s == AWAIT_SWITCH));
    assert!(!leading_switches("src/bin/gitpulsed.rs")
        .iter()
        .any(|s| s == AWAIT_SWITCH));
}

/// Every shipped binary spends the per-user spawn budget, and opts in before
/// its first spawn could build the gate without it. A binary that forgot
/// would run on its own bucket, and N of them could start N times the rate
/// the shared record exists to cap. Derived, so a new binary is covered.
#[test]
fn every_shipped_binary_opts_into_the_shared_spawn_budget() {
    for bin in shipped_binaries() {
        assert!(
            leading_switches(&bin)
                .iter()
                .any(|s| s == SHARED_BUDGET_SWITCH),
            "{bin}: run_process_with_shared_spawn_budget() must open main, \
             ahead of any other statement"
        );
    }
}

/// The behaviour the guard above stands for: a process that opts in opens
/// the per-user record. Its counterpart, a process that does not and never
/// touches it, is `tests/spawn_budget_isolation.rs`. In this executable
/// because it holds the process-wide switches; no other test here spawns or
/// reads `HOME`, and one that did would race this one for the first spawn.
#[test]
fn a_process_that_opts_in_draws_from_the_per_user_spawn_budget() {
    let serial = env::env_serial();
    let home = tempfile::tempdir().unwrap();
    let _env = env::bind_env(&serial)
        .set("HOME", home.path())
        .set("XDG_CONFIG_HOME", home.path().join(".config"))
        .set("APPDATA", home.path().join("AppData"));
    let config = gitpulse_lib::tool_config::default_config_dir().expect("config dir");
    assert!(
        config.starts_with(home.path()),
        "the premise failed: the record would not land in the temp home ({})",
        config.display()
    );
    run_process_with_shared_spawn_budget();
    gitpulse_lib::engine::git_cli::git_global(&["--version"]).expect("git runs");
    let record = std::fs::read_dir(&config)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        record,
        1,
        "an opted-in process did not open the per-user record under {}",
        config.display()
    );
}

/// The unlimited-rate switch exists for integration-test processes. A shipped
/// binary that called it would run with no spawn rate cap and no shared
/// budget, which is the storm the gate exists to stop.
#[test]
fn no_shipped_source_lifts_the_spawn_rate_cap() {
    fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(files.len() > 100, "walked only {} sources", files.len());
    let call = "run_process_with_unlimited_spawn_rate();";
    let definition = "pub fn run_process_with_unlimited_spawn_rate() {";
    let mut callers = Vec::new();
    let mut definitions = 0;
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        definitions += source.matches(definition).count();
        if source.contains(call) {
            callers.push(path.display().to_string());
        }
    }
    assert_eq!(
        definitions, 1,
        "the switch must still exist for this guard to mean anything"
    );
    assert!(
        callers.is_empty(),
        "shipped sources lift the spawn rate cap: {callers:?}"
    );
}

/// Integration suites that name the library but do not lift the spawn rate cap,
/// and why each one need not: it measures the gate, runs in a child process of
/// its own (its own gate), or calls only library code that spawns nothing
/// through it. A suite that calls a module that does spawn through
/// `git_cli`'s gate (`engine`, `graph`, `repo_op`, `worktree`, `stash`,
/// `submodules`, `analyzer::{coverage, deps}`, `storage`, `desktop::
/// cmd_resolve_git_root`, `harness`, `devmap`, `ops`, `ingest`, `ledger`,
/// `terminal` sessions and `run_terminal`) belongs in neither this list nor
/// outside the switch. (`repository_trust::inspect` and `grant` do not: they
/// read the filesystem through `validate_repo_path` and `find_git_root`, and
/// spawn nothing; checked in `repository_trust.rs` above its test module.)
///
/// What the lint below can and cannot see. It is lexical: it strips comments,
/// string literals and raw strings from each top-level `tests/*.rs` file, then
/// looks for a statement that lifts the cap. It therefore catches a lift that
/// exists only in a comment, a block comment or a string. It does NOT catch
/// anything structural, and none of these is half-solved here:
///   - a `#[path]` module, or a `common` helper, that reaches the library while
///     the suite file never names the crate (the file is skipped);
///   - a lift in one fixture only, with a second test reaching the gate
///     without it (the file is read once, not per test);
///   - a lift in dead code, or in a macro body that is never expanded;
///   - a local function that shadows the lifting wrapper's name and does
///     nothing.
///
/// Each needs the call graph, which `devmap` has and this test does not.
/// The run-time answer is the instrumented gate: patch the library's spawn gate
/// (`git_cli`'s `SpawnGate::acquire_judged`, where `rate_limited` is decided) to
/// append the test executable's name, the thread's name and the admission class
/// (and a backtrace, to name the call) to a log whenever a spawn is made under
/// the production rate by a process whose executable sits in `target/debug/deps`
/// (an integration-test crate). A suite in that log either has an entry point
/// that reaches the gate before its lift, or sits in the list below on a false
/// reason. That is how `terminal_pty_stress` (14 spawns from
/// `session-spawn-ledger`) and the `bind` calls of the hook suites were found.
const UNMETERED_ALLOWLIST: &[(&str, &str)] = &[
    ("adv_notify_process.rs", "subprocess-only: drives gitpulse-hook; the library supplies constants and LockedSpawn"),
    ("adv_w3c_process.rs", "subprocess-only, measured: drives gitpulse-hook; the library supplies alerts::bridge env names, logging::LOG_DIR_ENV and LockedSpawn, and with the gate instrumented the suite made 0 metered spawns (the control, mcp_background_wait run beside it, made over a hundred; the count varies by run)"),
    ("adv_w6l4_ascii_oracle.rs", "no git spawn: pure parsers (github::parse_github_remote_url, tasks::scan_briefs)"),
    ("agent_notify_socket.rs", "subprocess-only: the socket protocol against built binaries; alerts::bridge parsers"),
    ("ai_transport_integration.rs", "no git spawn: ai::http URL parsing"),
    ("alerts_worker_throughput.rs", "no git spawn: the alerts worker behind a stub host"),
    ("cli_io_process.rs", "subprocess-only: drives built binaries; hooks and mcp parsers"),
    ("codeintel_real_store.rs", "no git spawn: codeintel reads a store; validate_repo only reads the filesystem"),
    ("command_rendering_integration.rs", "no git spawn: harness::render_command renders text"),
    ("conflict_hardening.rs", "no git spawn: diff::ConflictResolver transforms text"),
    ("desktop_menu_integration.rs", "no in-process git: menu events are emitted to the webview; its git is test-side"),
    ("github_integration.rs", "no spawn: builds `gh` argv without running it"),
    ("gitpulsed_single_owner.rs", "subprocess-only: drives the daemon binary; daemon_lock::running reads a lock"),
    ("gtk_runtime.rs", "no git spawn: desktop runtime probing"),
    ("hook_deadline_reap.rs", "subprocess-only: drives gitpulse-hook"),
    ("hook_gate_app.rs", "no git spawn: desktop::instance claim and hooks::serve_gate resolve paths through find_git_root, which reads the filesystem"),
    ("hook_gate_bench.rs", "no git spawn: desktop::instance claim and hooks::serve_gate resolve paths through find_git_root, which reads the filesystem"),
    ("host_load_soak.rs", "no git spawn: samples host load (sysload)"),
    ("lane_solver_fuzz.rs", "no git spawn: graph::LaneSolver over synthetic commits"),
    ("lane_solver_stress.rs", "no git spawn: graph::LaneSolver over synthetic commits"),
    ("lane_stability.rs", "no git spawn: graph::LaneSolver over synthetic commits"),
    ("language_classification_integration.rs", "no git spawn: analyzer::LanguageDetector and LocCounter classify text"),
    ("local_ai_live.rs", "no git spawn: ai against a local model server"),
    ("logging_process.rs", "subprocess-only: the logging module in processes of their own"),
    ("mcp_background_wait.rs", "measures the metered gate: background spawns waiting for budget"),
    ("mcp_stdio.rs", "subprocess-only: drives gitpulse-mcp over stdio"),
    ("mcp_transport_stress.rs", "subprocess-only, measured: drives gitpulse-mcp; its in-process calls are repository_trust::inspect/grant (filesystem) and ledger::record, and with the gate instrumented the suite made 0 metered spawns"),
    ("native_event_unwind.rs", "subprocess-only: native event-loop fault injection in a child process"),
    ("native_menu_main_thread.rs", "no git spawn: builds desktop menus"),
    ("native_quit_guard.rs", "no git spawn: quit guard over empty terminal sessions"),
    ("native_status_material.rs", "no git spawn: desktop window material"),
    ("native_watcher_faults.rs", "child-process gate: RepoFileWatcher::watch runs in a re-executed child, which has a gate of its own"),
    ("notify_stdin_bench.rs", "subprocess-only: times the hook binary; alerts::bridge env names"),
    ("notify_stdin_process.rs", "subprocess-only: drives the hook binary; alerts::bridge env names"),
    ("process_admission.rs", "measures the process-wide admission switches and the per-user budget; lifting the cap would change what it measures"),
    ("process_admission_stress.rs", "measures the admission gate under load; lifting the cap would change what it measures"),
    ("process_spawn_timing.rs", "measures the timing contract of the production spawn seam"),
    ("procguard_signals.rs", "subprocess-only: signals against child processes; procguard only"),
    ("signal_guard_hook.rs", "subprocess-only: drives gitpulse-hook"),
    ("signal_guard_live.rs", "no git spawn: hooks::signal_guard reads the process table"),
    ("signal_teardown.rs", "subprocess-only: signal teardown of child processes; procguard only"),
    ("spawn_budget_isolation.rs", "builds the gate once from its first spawn and must stay off the shared record; lifting the cap would defeat it"),
    ("updates_integration.rs", "no spawn: updates::{evaluate, latest_stable_tag} parse text handed to them"),
];

/// The statements in `source` that lift the spawn rate cap for the process:
/// the library call itself, or the `unmetered_spawns()` wrapper of it, in the
/// code of `source` (see [`strip_non_code`]).
fn lifts_the_cap(source: &str) -> bool {
    strip_non_code(source).lines().map(str::trim).any(|line| {
        line.ends_with("run_process_with_unlimited_spawn_rate();")
            || line.ends_with("unmetered_spawns();")
    })
}

/// `source` with every comment (`//`, and nested `/* */`), string literal,
/// byte string and raw string (`r#"..."#`) blanked out, newlines kept. What
/// remains is what the compiler would read as code, so a mention of the lift in
/// prose or in quoted text no longer looks like a call. Char literals are
/// skipped so a `'"'` does not open a string; a lifetime (`'a`) is left alone.
fn strip_non_code(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
        } else if (c == 'r' || (c == 'b' && next == Some('r')))
            && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'))
            && raw_string_hashes(&chars, i).is_some()
        {
            let (hashes, open) = raw_string_hashes(&chars, i).unwrap();
            i = open;
            while i < chars.len() {
                let closes =
                    chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#'));
                if closes {
                    i += 1 + hashes;
                    break;
                }
                out.push(blank(chars[i]));
                i += 1;
            }
        } else if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                if i < chars.len() {
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
            i += 1;
        } else if c == '\'' {
            let escaped = next == Some('\\');
            let plain = chars.get(i + 2) == Some(&'\'');
            if escaped {
                i += 2;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if plain {
                i += 3;
            } else {
                out.push(c);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// For `r#..#"` or `br#..#"` starting at `at`: the number of `#`s and the index
/// just after the opening quote.
fn raw_string_hashes(chars: &[char], at: usize) -> Option<(usize, usize)> {
    let mut i = at + if chars[at] == 'b' { 2 } else { 1 };
    let mut hashes = 0;
    while chars.get(i) == Some(&'#') {
        hashes += 1;
        i += 1;
    }
    (chars.get(i) == Some(&'"')).then_some((hashes, i + 1))
}

/// Every integration suite that calls the library in-process lifts the spawn
/// rate cap, or is listed above with the reason it need not. Each of these
/// crates links the library without `cfg(test)`, so it runs under the production
/// spawn rate: a loaded parallel run drains the 192-token burst and defers a
/// `GitReader` call mid-test, for a reason that has nothing to do with the code
/// under test. Derived from the directory, so a new suite is held to it.
#[test]
fn every_integration_suite_that_calls_the_library_lifts_the_spawn_rate_cap_or_says_why_not() {
    let tests = manifest_dir().join("tests");
    let mut unswitched = Vec::new();
    let mut contradicted = Vec::new();
    let mut checked = 0;
    for entry in std::fs::read_dir(&tests).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "rs") || !path.is_file() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let source = std::fs::read_to_string(&path).unwrap();
        if !strip_non_code(&source).contains("gitpulse_lib") {
            continue;
        }
        checked += 1;
        let lifted = lifts_the_cap(&source);
        let listed = UNMETERED_ALLOWLIST
            .iter()
            .any(|(listed, _)| *listed == name);
        match (lifted, listed) {
            (false, false) => unswitched.push(name),
            (true, true) => contradicted.push(name),
            _ => {}
        }
    }
    assert!(checked > 60, "walked only {checked} suites");
    assert!(
        unswitched.is_empty(),
        "these suites call the library without lifting the spawn rate cap and are not in \
         UNMETERED_ALLOWLIST: {unswitched:?}\nCall `common::unmetered_spawns()` first in each \
         fixture entry point, or list the suite with its reason"
    );
    assert!(
        contradicted.is_empty(),
        "these suites lift the cap and are also in UNMETERED_ALLOWLIST, so one of the two is \
         stale: {contradicted:?}"
    );
}

/// The allowlist cannot rot: every entry names a suite that exists and calls
/// the library, carries a reason, and appears once.
#[test]
fn the_unmetered_allowlist_names_real_suites_with_reasons() {
    let mut seen = std::collections::BTreeSet::new();
    for (name, reason) in UNMETERED_ALLOWLIST {
        assert!(seen.insert(*name), "{name} is listed twice");
        assert!(!reason.trim().is_empty(), "{name} has no reason");
        let source = std::fs::read_to_string(manifest_dir().join("tests").join(name))
            .unwrap_or_else(|e| panic!("{name} is listed but cannot be read: {e}"));
        assert!(
            strip_non_code(&source).contains("gitpulse_lib"),
            "{name} no longer calls the library, so it needs no entry"
        );
    }
}

// The lint's matcher against the evasions an adversary found. Each fixture is
// the body of a suite; the lift is real in `lifted`, and only named in the rest.

#[test]
fn a_real_lift_is_seen_however_it_is_spelled() {
    assert!(lifts_the_cap(
        "fn f() {\n    common::unmetered_spawns();\n}\n"
    ));
    assert!(lifts_the_cap(
        "fn f() {\n    gitpulse_lib::engine::git_cli::run_process_with_unlimited_spawn_rate();\n}\n"
    ));
    assert!(!lifts_the_cap("fn f() {\n    let _ = 1;\n}\n"));
}

#[test]
fn a_lift_named_in_a_line_comment_is_not_a_lift() {
    assert!(!lifts_the_cap(
        "// common::unmetered_spawns();\nfn f() {}\n"
    ));
    assert!(!lifts_the_cap(
        "/// Call `common::unmetered_spawns();`\nfn f() {}\n"
    ));
}

#[test]
fn a_lift_inside_a_block_comment_is_not_a_lift() {
    assert!(!lifts_the_cap(
        "/*\n    common::unmetered_spawns();\n*/\nfn f() {}\n"
    ));
    assert!(!lifts_the_cap(
        "/* outer /* inner */\ncommon::unmetered_spawns();\n*/\nfn f() {}\n"
    ));
    // The code after a closed comment is still read.
    assert!(lifts_the_cap(
        "/* note */\nfn f() {\n    common::unmetered_spawns();\n}\n"
    ));
}

#[test]
fn a_lift_that_is_a_line_of_a_string_or_raw_string_is_not_a_lift() {
    assert!(!lifts_the_cap(
        "const S: &str = r#\"\ncommon::unmetered_spawns();\n\"#;\n"
    ));
    assert!(!lifts_the_cap(
        "const S: &str = r##\"\n\"# not the end\ncommon::unmetered_spawns();\n\"##;\n"
    ));
    assert!(!lifts_the_cap(
        "const S: &str = \"\\\ncommon::unmetered_spawns();\n\";\n"
    ));
    assert!(!lifts_the_cap(
        "const B: &[u8] = br#\"\ncommon::unmetered_spawns();\n\"#;\n"
    ));
}

#[test]
fn stripping_keeps_the_code_around_quotes_and_lifetimes_in_sync() {
    // An escaped quote, a char literal holding a quote, and a lifetime must not
    // leave the matcher inside a string it never left.
    assert!(lifts_the_cap(
        "fn f<'a>(s: &'a str) {\n    let _ = (\"\\\"\", '\"', '\\'');\n    common::unmetered_spawns();\n}\n"
    ));
}
