//! A shipped binary a test starts must log into a directory the test owns.
//!
//! The binaries pick their log file from `GITPULSE_LOG_DIR`, and fall back to
//! the user's real log directory. `hook_protocol_stress` set neither, so every
//! `cargo test` appended a debug-build hook's debug output, and its
//! dependencies' narration, to `~/Library/Logs/GitPulse/gitpulse-hook.log`:
//! about 2,600 debug lines in one log, mixed in with the real hook's own.
//!
//! The check is per file, not per spawn: a file that starts a binary must name
//! the variable somewhere. That is cheap and catches a new suite that forgets
//! it entirely, which is how this happened.

use std::path::Path;

#[test]
fn every_suite_that_starts_a_shipped_binary_isolates_its_log_directory() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let this_file = Path::new(file!())
        .file_name()
        .expect("this file's name")
        .to_owned();
    let mut starting = Vec::new();
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&tests).expect("read tests/") {
        let path = entry.expect("tests/ entry").path();
        if path.extension().is_none_or(|ext| ext != "rs")
            || path.file_name() == Some(this_file.as_os_str())
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read suite");
        if !text.contains("CARGO_BIN_EXE_") {
            continue;
        }
        starting.push(path.display().to_string());
        if !text.contains("LOG_DIR") {
            offenders.push(path.display().to_string());
        }
    }
    // The suites that start binaries today; fewer found means this check
    // stopped looking, not that they went away.
    assert!(starting.len() >= 7, "found only {starting:?}");
    assert!(
        offenders.is_empty(),
        "these suites start a shipped binary without GITPULSE_LOG_DIR \
         (logging::LOG_DIR_ENV), so it writes into the user's real log: {offenders:?}"
    );
}
