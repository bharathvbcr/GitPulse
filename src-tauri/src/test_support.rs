//! Fixtures shared by the crate's unit tests.
//!
//! `git_in` existed seven times over — in `engine::git_cli`, `engine::worktree`,
//! `harness`, `ingest`, `insights`, `ledger::bindings` and `watcher` — as
//! byte-identical bodies that differed only in whether they spelled `Path` and
//! `Command` in full. Seven copies of a fixture is seven places to update when
//! the identity pins or the failure message need to change, and nothing fails
//! when only six of them are.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;

static HARNESS_CLONE_SEQ: AtomicU64 = AtomicU64::new(0);

/// A private copy of this libtest image. Drop removes it after the child exits.
pub(crate) struct IsolatedHarnessGuard {
    path: PathBuf,
}

impl IsolatedHarnessGuard {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for IsolatedHarnessGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Re-executes a libtest filter from a copy of this harness.
///
/// `cargo llvm-cov --workspace` can unlink the running `current_exe()` while
/// later cases still need to spawn it. `Command::spawn` then fails with
/// `NotFound` even though this process is still mapped. Copying beside the
/// original keeps `@rpath` intact and gives the child a path cargo will not
/// replace. Keep the guard alive until the child exits.
#[cfg(test)]
pub(crate) fn isolated_libtest_command(filter: &str) -> (Command, IsolatedHarnessGuard) {
    let src = std::env::current_exe().expect("test executable");
    let stem = src
        .file_name()
        .expect("test executable name")
        .to_string_lossy();
    let path = src.with_file_name(format!(
        "{stem}-isolate-{}-{}",
        std::process::id(),
        HARNESS_CLONE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::copy(&src, &path).unwrap_or_else(|error| {
        panic!(
            "copy test harness for isolation spawn: {} -> {}: {error}",
            src.display(),
            path.display()
        );
    });
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path)
            .expect("cloned harness metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("cloned harness executable");
    }
    let mut command = Command::new(&path);
    command.args(["--exact", filter, "--nocapture"]);
    (command, IsolatedHarnessGuard { path })
}

/// Runs `git` in `dir` with the test identity pinned, and asserts it succeeded.
///
/// The `-c` flags are what make the fixture hermetic: a developer's global
/// `user.name`, `user.email` or `commit.gpgsign` must not decide whether the
/// suite passes, and signing in particular would block on a passphrase prompt.
///
/// The `cfg(test)` is redundant with this module's gated declaration in
/// `lib.rs`, and is kept because `tests/spawn_seam.rs` classifies source by
/// the attributes it can see in the file: without it, this fixture reads as
/// production code spawning outside the gated seam.
#[cfg(test)]
pub(crate) fn git_in(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            "user.email=gitpulse@test.local",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A fresh repository on `main`, for suites that only need somewhere to scan.
///
/// Identical bodies stood in `analyzer::coverage` and `analyzer::deps`; the
/// default-branch name is the kind of fact that gets corrected in one copy.
#[cfg(test)]
pub(crate) fn git_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(status.success());
    dir
}

/// Writes `content` to `dir/rel`, creating parent directories as needed.
#[cfg(test)]
pub(crate) fn write(dir: &Path, rel: &str, content: &str) {
    let dest = dir.join(rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(dest, content).unwrap();
}

#[cfg(test)]
mod tests {
    use super::isolated_libtest_command;

    #[test]
    fn isolated_harness_copy_is_a_file_beside_the_running_image() {
        let (_command, guard) = isolated_libtest_command("does-not-need-to-exist");
        let src = std::env::current_exe().expect("test executable");
        assert!(
            guard.path().is_file(),
            "cloned harness missing: {}",
            guard.path().display()
        );
        assert_ne!(guard.path(), src.as_path());
        assert_eq!(guard.path().parent(), src.parent());
    }
}
