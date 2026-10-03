//! An integration-test process never touches the user's per-user spawn budget.
//!
//! Every `tests/*.rs` crate links the library without `cfg(test)`, so it gets
//! the production spawn gate. That gate used to open the per-user record the
//! running app and every agent's `gitpulse-mcp` share, so each crate that
//! spawned git spent their budget and was deferred by them. Only the shipped
//! binaries opt in now; this process never does, and must stay off the record
//! by construction rather than by remembering a switch.
//!
//! Its own executable: the gate is built once, by the first spawn in the
//! process, from the environment of that moment. A second test here that
//! spawned or read `HOME` would race this one for that first spawn. The
//! positive case, a process that opts in, is in `tests/process_admission.rs`.

#[path = "../src/test_support/env.rs"]
mod env;

use std::path::Path;
use std::sync::Mutex;

static ENV_SERIAL: Mutex<()> = Mutex::new(());

fn files_under(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(&path, out);
        }
        out.push(path);
    }
}

#[test]
fn an_integration_process_never_opens_the_per_user_spawn_budget() {
    let serial = ENV_SERIAL.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    // Every variable the per-user config directory is resolved from, on
    // every platform, so the record would land in `home` wherever it lands.
    let _env = env::bind_env(&serial)
        .set("HOME", home.path())
        .set("XDG_CONFIG_HOME", home.path().join(".config"))
        .set("APPDATA", home.path().join("AppData"));
    let config = gitpulse_lib::tool_config::default_config_dir()
        .expect("a config directory resolves from the overridden environment");
    assert!(
        config.starts_with(home.path()),
        "the premise failed: the record would not land in the temp home ({})",
        config.display()
    );

    // The first spawn in this process builds the production gate.
    let version = gitpulse_lib::engine::git_cli::git_global(&["--version"]).expect("git runs");
    assert!(String::from_utf8_lossy(&version).starts_with("git version"));

    let mut created = Vec::new();
    files_under(home.path(), &mut created);
    assert!(
        created.is_empty(),
        "a test process opened the per-user spawn budget: {created:?}"
    );

    // Opting in after the gate exists cannot take effect, and must say so
    // rather than leave a shipped binary silently on its own budget.
    let late = std::panic::catch_unwind(
        gitpulse_lib::engine::git_cli::run_process_with_shared_spawn_budget,
    );
    assert!(late.is_err(), "a late opt-in was accepted");
    gitpulse_lib::engine::git_cli::git_global(&["--version"]).expect("git still runs");
    let mut after = Vec::new();
    files_under(home.path(), &mut after);
    assert!(after.is_empty(), "the late opt-in opened it: {after:?}");
}
