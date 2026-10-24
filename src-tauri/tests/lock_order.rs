//! One global order for the two test locks: `ref_cache::retention()` first,
//! the environment lock (`env_serial`, `bind_env*`, `unhurried`,
//! `eager_unhurried`, `UseStore`) second. `unhurried` and `UseStore` take the
//! environment lock for their whole body, so a test that already holds it and
//! then asks for `retention()` deadlocks against one that holds `retention()`
//! and asks for the environment lock. This lints the one place the order can
//! be seen: within a single function, retention() must not follow an env lock.

mod common;

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn takes_env_lock(line: &str) -> bool {
    let code = line.trim_start();
    if code.starts_with("//") || code.starts_with("use ") {
        return false;
    }
    [
        "env_serial()",
        "bind_env_owned(",
        "bind_env(",
        "UseStore::file(",
        "BackupStore::new(",
    ]
    .iter()
    .any(|needle| code.contains(needle))
        || (code.contains("unhurried(") && !code.contains("fn "))
}

#[test]
fn retention_is_never_taken_after_the_environment_lock_in_one_function() {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(
        files.len() > 100,
        "the walk found only {} files",
        files.len()
    );
    let mut inversions = Vec::new();
    let mut retention_calls = 0usize;
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read source");
        let mut env_line: Option<usize> = None;
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            let starts_fn = code.starts_with("fn ")
                || code.starts_with("pub fn ")
                || code.starts_with("pub(crate) fn ");
            if starts_fn {
                env_line = None;
            }
            if code.contains("retention()") && !code.starts_with("//") && !starts_fn {
                retention_calls += 1;
                if let Some(first) = env_line {
                    inversions.push(format!(
                        "{}:{} takes retention() after the environment lock at line {}",
                        file.display(),
                        number + 1,
                        first + 1
                    ));
                }
            }
            if env_line.is_none() && takes_env_lock(line) {
                env_line = Some(number);
            }
        }
    }
    assert!(retention_calls > 50, "only {retention_calls} calls seen");
    assert!(
        inversions.is_empty(),
        "lock-order inversions:\n{}",
        inversions.join("\n")
    );
}
