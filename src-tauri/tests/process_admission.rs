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
