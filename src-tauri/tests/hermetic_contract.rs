//! The hermetic test environment is only as good as its coverage: a suite that
//! does not run in it reads the developer's git config, `~/.claude`, trust
//! store and `tools.json`, and a raw `set_var` is a process-global write nothing
//! restores. Both are properties of the *source*, so they are asserted over the
//! source, the way `tests/spawn_seam.rs` asserts the spawn seam.

mod common;

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn manifest(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(sub)
}

/// Every integration crate links the library without `cfg(test)`, so none of
/// them gets the hermetic environment from the library. Each one has to bring
/// it: by declaring `mod common;` (which includes it) or by including
/// `test_support/env.rs` itself.
#[test]
fn every_integration_test_crate_runs_in_the_hermetic_environment() {
    let mut files = Vec::new();
    let tests = manifest("tests");
    for entry in std::fs::read_dir(&tests).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    assert!(files.len() > 40, "found only {} test crates", files.len());
    let outside: Vec<String> = files
        .iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path).unwrap();
            let declares_common = text
                .lines()
                .any(|line| line.trim() == "mod common;" || line.trim() == "pub mod common;");
            let includes_env = text.lines().any(|line| {
                line.trim().starts_with("#[path =") && line.contains("test_support/env.rs")
            });
            !declares_common && !includes_env
        })
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        outside.is_empty(),
        "these test crates neither declare `mod common;` nor include \
         `src/test_support/env.rs`, so they run against the developer's home: {outside:?}"
    );
}

/// `set_var` and `remove_var` write one slot shared by every thread, and a test
/// that fails between the write and its undo leaves the value for every later
/// test. `test_support/env.rs` owns the only sanctioned form (`bind_env`, which
/// serializes and restores on drop).
#[test]
fn no_test_mutates_the_environment_outside_the_env_owner() {
    let mut files = Vec::new();
    rust_files(&manifest("src"), &mut files);
    rust_files(&manifest("tests"), &mut files);
    let owner = manifest("src/test_support/env.rs");
    let this = manifest("tests/hermetic_contract.rs");
    let mut offenders = Vec::new();
    for path in files {
        if path == owner || path == this {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (index, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("set_var(") || code.contains("remove_var(") {
                offenders.push(format!("{}:{}: {}", path.display(), index + 1, code.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "raw environment mutation outside src/test_support/env.rs; use \
         `env_serial()` + `bind_env(..).set(..)` so it is serialized and restored:\n{}",
        offenders.join("\n")
    );
}

/// The integration-test side of `src/test_support.rs::hermetic_canary`: the two
/// state directories the library exposes resolve under this process's root, and
/// git reads no configuration from outside it.
#[test]
fn integration_tests_resolve_state_under_the_per_process_root() {
    let _serial = common::env::env_serial();
    let root = common::env::hermetic::root();
    for (what, path) in [
        (
            "default_config_dir",
            gitpulse_lib::tool_config::default_config_dir(),
        ),
        (
            "default_cache_dir",
            gitpulse_lib::tool_config::default_cache_dir(),
        ),
    ] {
        let path = path.unwrap_or_else(|| panic!("{what} did not resolve"));
        assert!(
            path.starts_with(&root),
            "{what} resolved to {}, outside {}",
            path.display(),
            root.display()
        );
    }
    assert_eq!(std::env::var("GIT_CONFIG_NOSYSTEM").as_deref(), Ok("1"));
    assert!(
        PathBuf::from(std::env::var_os("GIT_CONFIG_GLOBAL").unwrap()).starts_with(&root),
        "GIT_CONFIG_GLOBAL points outside the hermetic root"
    );
}

/// A wall-clock limit asserted as a bare `Duration::from_millis(400)` is a limit
/// nobody scaled for an instrumented or loaded runner, and when it includes a
/// process spawn it fails on contention rather than on a bug. Every such
/// assertion goes through `coverage_relaxed`, the one helper, so the scaling
/// rule lives in one place.
#[test]
fn no_wall_clock_assertion_bypasses_the_scaling_helper() {
    let mut files = Vec::new();
    rust_files(&manifest("src"), &mut files);
    rust_files(&manifest("tests"), &mut files);
    let owner = manifest("src/test_support/env.rs");
    let this = manifest("tests/hermetic_contract.rs");
    let bin = manifest("src/bin");
    let mut offenders = Vec::new();
    for path in files {
        // `src/bin/*` are separate crates with no access to the helper.
        if path == owner || path == this || path.starts_with(&bin) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (index, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains(".elapsed() <")
                && code.contains("Duration::from_")
                && !code.contains("coverage_relaxed")
            {
                offenders.push(format!("{}:{}: {}", path.display(), index + 1, code));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "wall-clock limits must be written `coverage_relaxed(Duration::from_..)`:\n{}",
        offenders.join("\n")
    );
}
