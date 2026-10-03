//! The process-wide admission default, in a process of its own: setting it in
//! the unit-test binary would reclassify every other test's spawns.

use gitpulse_lib::engine::git_cli::{current_admission_name, run_process_as_background};

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

/// The binaries whose work is optional must make the switch, and make it
/// before anything else in `main` can start a thread.
#[test]
fn the_agent_server_and_the_daemon_run_as_background() {
    for bin in ["src/bin/gitpulse-mcp.rs", "src/bin/gitpulsed.rs"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(bin);
        let source = std::fs::read_to_string(&path).unwrap();
        let main = source
            .find("fn main() {")
            .unwrap_or_else(|| panic!("{bin}: no main"));
        let body = &source[main..];
        let first_call = body
            .lines()
            .skip(1)
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("//"))
            .unwrap_or_default();
        assert_eq!(
            first_call, "gitpulse_lib::engine::git_cli::run_process_as_background();",
            "{bin}: the class switch must be the first statement of main"
        );
    }
    // The app and the hooks keep the default: the hook gates the user's own
    // agent tool calls, and the app is what the user is looking at.
    for bin in ["src/main.rs", "src/bin/gitpulse-hook.rs"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(bin);
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(!source.contains("run_process_as_background"), "{bin}");
    }
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
