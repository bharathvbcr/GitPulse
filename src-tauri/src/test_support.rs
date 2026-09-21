//! Fixtures shared by the crate's unit tests.
//!
//! `git_in` existed seven times over — in `engine::git_cli`, `engine::worktree`,
//! `harness`, `ingest`, `insights`, `ledger::bindings` and `watcher` — as
//! byte-identical bodies that differed only in whether they spelled `Path` and
//! `Command` in full. Seven copies of a fixture is seven places to update when
//! the identity pins or the failure message need to change, and nothing fails
//! when only six of them are.

pub(crate) mod env;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tempfile::TempDir;

/// The sidecar serial is this crate's de-facto environment serial: the tests
/// that override `GITPULSE_*` binary paths, `GITPULSE_DEVCOUNCIL_ROOT`,
/// `GITPULSE_MANVI_ROOT` and `PATH` all take it, because the thing those
/// variables ultimately steer is which binary a spawn finds. Declaring it here
/// rather than in `env` keeps that module includable by `gitpulsed`, which is a
/// separate crate and cannot name this type.
impl env::EnvSerial for crate::harness::sidecar::SidecarTestGuard {}

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

/// Coverage instrumentation makes sub-second process and thread deadlines
/// flake. Production timeouts stay unchanged; only tests should call this.
#[cfg(test)]
pub(crate) fn coverage_relaxed(duration: Duration) -> Duration {
    if std::env::var_os("CARGO_LLVM_COV").is_some()
        || std::env::var_os("LLVM_PROFILE_FILE").is_some()
    {
        duration.saturating_mul(8).max(Duration::from_secs(5))
    } else {
        duration
    }
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
    if args.first() == Some(&"init") {
        trust_repo(dir);
    }
}

/// Explicit approval for an owned fixture through the production grant path.
#[cfg(test)]
pub(crate) fn trust_repo(dir: &Path) {
    let path = dir.to_str().expect("fixture path");
    let preview = crate::repository_trust::inspect(path).expect("inspect fixture");
    crate::repository_trust::grant(path, &preview.identity, false).expect("approve fixture");
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
    trust_repo(dir.path());
    dir
}

/// Reads one pid a spawned fixture announced on its stdout.
///
/// It is also the synchronisation point for everything after it: a pid on the
/// pipe proves the child reached its own `fork`, so a test that starts
/// measuring here measures the behaviour under test rather than how long this
/// host took to start a process.
#[cfg(all(test, unix))]
pub(crate) fn announced_pid(child: &mut std::process::Child) -> u32 {
    use std::io::BufRead;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut line = String::new();
    std::io::BufReader::new(stdout)
        .read_line(&mut line)
        .expect("read announced pid");
    line.trim().parse().expect("pid")
}

/// Process-level liveness, which is not the same question as
/// `procguard::sys::is_alive`: that one asks about a process *group*, and a
/// helper some child forked is not a group leader, so asking it about one
/// always answers "gone".
///
/// A zombie answers "alive" here, which is the honest answer — a pid that has
/// not been waited on is still in the table and still ours. Assertions about a
/// process being gone therefore go through [`wait_gone`].
#[cfg(all(test, unix))]
pub(crate) fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Waits for a pid to leave the process table, so an assertion never depends on
/// how quickly `init` reaps a reparented orphan.
#[cfg(all(test, unix))]
pub(crate) fn wait_gone(pid: u32, budget: Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    while std::time::Instant::now() < deadline {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
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
    use super::{coverage_relaxed, isolated_libtest_command};
    use std::time::Duration;

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

    #[test]
    fn coverage_relaxed_leaves_plain_runs_unchanged() {
        let budget = Duration::from_millis(400);
        let instrumented = std::env::var_os("CARGO_LLVM_COV").is_some()
            || std::env::var_os("LLVM_PROFILE_FILE").is_some();
        let relaxed = coverage_relaxed(budget);
        if instrumented {
            assert!(relaxed >= Duration::from_secs(5), "{relaxed:?}");
        } else {
            assert_eq!(relaxed, budget);
        }
    }
}
