//! The process-wide admission default, in a process of its own: setting it in
//! the unit-test binary would reclassify every other test's spawns. The same
//! holds for the shared spawn budget opt-in, which the unit-test build cannot
//! even name.

#[path = "../src/test_support/env.rs"]
mod env;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

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

/// Every shipped binary's entry file, relative to the crate, derived two
/// independent ways that must agree: the `[[bin]]` paths `Cargo.toml` builds,
/// and the files a binary can live in (`src/main.rs` plus `src/bin/*.rs`). A
/// `[[bin]]` with a custom path, or a new file in `src/bin/` that Cargo picks
/// up on its own, fails here instead of escaping the guards below.
fn shipped_binaries() -> BTreeSet<String> {
    let manifest = std::fs::read_to_string(manifest_dir().join("Cargo.toml")).unwrap();
    let mut declared = BTreeSet::new();
    let mut in_bin = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_bin = line == "[[bin]]";
        } else if in_bin {
            if let Some(path) = line
                .strip_prefix("path")
                .map(str::trim_start)
                .and_then(|rest| rest.strip_prefix('='))
            {
                declared.insert(path.trim().trim_matches('"').to_string());
            }
        }
    }
    let mut on_disk = BTreeSet::from(["src/main.rs".to_string()]);
    for entry in std::fs::read_dir(manifest_dir().join("src/bin")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path.file_name().unwrap().to_string_lossy();
            on_disk.insert(format!("src/bin/{name}"));
        }
    }
    assert_eq!(
        declared, on_disk,
        "Cargo.toml's [[bin]] paths and src/main.rs + src/bin/*.rs disagree"
    );
    assert!(
        declared.len() >= 3,
        "derived only {declared:?}: the walk or the parse is broken"
    );
    declared
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

static ENV_SERIAL: Mutex<()> = Mutex::new(());

/// The behaviour the guard above stands for: a process that opts in opens
/// the per-user record. Its counterpart, a process that does not and never
/// touches it, is `tests/spawn_budget_isolation.rs`. In this executable
/// because it holds the process-wide switches; no other test here spawns or
/// reads `HOME`, and one that did would race this one for the first spawn.
#[test]
fn a_process_that_opts_in_draws_from_the_per_user_spawn_budget() {
    let serial = ENV_SERIAL.lock().unwrap();
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
