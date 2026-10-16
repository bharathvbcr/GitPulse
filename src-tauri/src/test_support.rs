//! Fixtures shared by the crate's unit tests.
//!
//! `git_in` existed seven times over — in `engine::git_cli`, `engine::worktree`,
//! `harness`, `ingest`, `insights`, `ledger::bindings` and `watcher` — as
//! byte-identical bodies that differed only in whether they spelled `Path` and
//! `Command` in full. Seven copies of a fixture is seven places to update when
//! the identity pins or the failure message need to change, and nothing fails
//! when only six of them are.

use crate::procguard::LockedSpawn;
pub(crate) mod env;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::time::Duration;
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
    static SWEPT: std::sync::Once = std::sync::Once::new();
    SWEPT.call_once(|| {
        if let Some(dir) = src.parent() {
            sweep_dead_isolates(dir, &stem, process_is_alive);
        }
    });
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

/// Removes copies a killed run left beside the harness. Each is a full copy of
/// the test image (tens to hundreds of MB), removed by its guard's `Drop`,
/// which a run that is killed (a timeout, a signal, a crash) never reaches; they
/// then accumulate in `target/` run after run. A copy is named for the process
/// that made it, so one whose process is gone is nobody's.
#[cfg(test)]
fn sweep_dead_isolates(dir: &Path, stem: &str, alive: impl Fn(u32) -> bool) -> usize {
    let prefix = format!("{stem}-isolate-");
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        let Some(pid) = rest
            .split('-')
            .next()
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if !alive(pid) && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether `pid` names a process that exists. A permission error still means it
/// does (the copy is then left alone), and a reused pid keeps a stale copy
/// until its new owner exits, which costs disk, never correctness.
#[cfg(all(test, unix))]
fn process_is_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() != Some(3) }
}

#[cfg(all(test, not(unix)))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
pub(crate) use env::coverage_relaxed;

/// Runs `cmd` to completion inside `budget` through the engine's bounded
/// runner (process group killed at the deadline, output capped) and panics
/// naming `label` when it does not finish: a fixture's helper process that
/// wedges fails the test with a message instead of parking the whole suite.
///
/// The command is run exactly as built: binary, environment and working
/// directory are the caller's. Only stdin is closed. The exit status is left
/// to the caller.
#[cfg(test)]
pub(crate) fn run_within(
    cmd: Command,
    label: &str,
    budget: std::time::Duration,
) -> crate::engine::git_cli::BoundedRun {
    crate::engine::git_cli::run_bounded(cmd, label, budget, None)
        .and_then(|run| run.require_complete(label))
        .unwrap_or_else(|why| panic!("{label} did not finish within {budget:?}: {why}"))
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
    // Persisting a fixture identity is how contract@gitpulse.test became the
    // author of real commits. `-c` on one command is hermetic; `git config
    // user.*` is not, and this checkout refuses to store it.
    if args.first() == Some(&"config")
        && args.get(1).is_some_and(|key| key.starts_with("user."))
        && dir.join(".githooks/pre-commit").is_file()
    {
        panic!(
            "refusing to persist {} in {}; this checkout rejects fixture identities",
            args[1],
            dir.display()
        );
    }
    let mut command = Command::new("git");
    command
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Spawned under the inheritance lock like every production child: this
    // runs on test threads beside code whose pipes are not yet close-on-exec,
    // and a child forked in that window keeps their ends for its lifetime.
    let child = command.spawn_locked().expect("spawn git");
    let output = child.wait_with_output().expect("wait for git");
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

/// A main checkout (one commit of `seed.txt` on `main`) with `linked`
/// worktrees, `wt-00`.. on branches `agent/00`.., under one parent directory.
/// Each linked path is trusted and returned in the spelling git lists it in
/// (plain canonical form), which is also the key the spawn log files its
/// children under.
#[cfg(test)]
pub(crate) fn repo_with_worktrees(linked: usize) -> (TempDir, TempDir, Vec<PathBuf>) {
    let main = TempDir::new().unwrap();
    git_in(main.path(), &["init", "-b", "main"]);
    fs::write(main.path().join("seed.txt"), "seed").unwrap();
    git_in(main.path(), &["add", "."]);
    git_in(main.path(), &["commit", "-m", "init"]);
    let parent = TempDir::new().unwrap();
    let paths = (0..linked)
        .map(|i| {
            let path = parent.path().join(format!("wt-{i:02}"));
            let branch = format!("agent/{i:02}");
            git_in(
                main.path(),
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    &branch,
                    path.to_str().unwrap(),
                ],
            );
            trust_repo(&path);
            crate::engine::git_cli::canonicalize_plain(&path).unwrap()
        })
        .collect();
    (main, parent, paths)
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
        .status_locked()
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
    // `pid as i32` would turn u32::MAX into -1, which `kill` reads as "every
    // process"; a pid that does not fit a `pid_t` names nothing.
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 delivers nothing; it only probes for existence.
    unsafe { libc::kill(pid, 0) == 0 }
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

/// Commits `message` with author and committer dated `epoch`.
///
/// `git_in` cannot carry environment, and a history test that lets git pick
/// "now" for every commit can only ever assert on one bucket.
#[cfg(test)]
pub(crate) fn commit_at(dir: &Path, message: &str, epoch: i64, email: &str) {
    let stamp = format!("@{epoch} +0000");
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            &format!("user.email={email}"),
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ])
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_EMAIL", email)
        .current_dir(dir)
        .output_locked()
        .expect("spawn git commit");
    assert!(
        output.status.success(),
        "commit {message} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Writes one object of `kind` holding `content` and returns its id.
#[cfg(test)]
pub(crate) fn hash_object(dir: &Path, kind: &str, content: &[u8]) -> String {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(["hash-object", "-t", kind, "-w", "--stdin"])
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn git hash-object");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(content)
        .expect("write object");
    let out = child.wait_with_output().expect("wait for hash-object");
    assert!(
        out.status.success(),
        "hash-object {kind} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("object id")
        .trim()
        .to_string()
}

/// Writes a commit object by hand, so a fixture can carry headers no key could
/// produce (`gpgsig`, `mergetag`) without a signing program. `extra_headers`
/// is raw text placed after `committer`, each line newline-terminated, with
/// continuation lines already indented. Does not move any ref.
#[cfg(test)]
pub(crate) fn raw_commit(
    dir: &Path,
    parents: &[&str],
    extra_headers: &str,
    message: &str,
    epoch: i64,
) -> String {
    let tree = hash_object(dir, "tree", b"");
    let mut body = format!("tree {tree}\n");
    for parent in parents {
        body.push_str(&format!("parent {parent}\n"));
    }
    let who = format!("T <t@test.local> {epoch} +0000");
    body.push_str(&format!("author {who}\ncommitter {who}\n"));
    body.push_str(extra_headers);
    body.push_str(&format!("\n{message}\n"));
    hash_object(dir, "commit", body.as_bytes())
}

/// An armored signature header of `armor` ("PGP SIGNATURE", "SSH SIGNATURE",
/// "SIGNED MESSAGE") for `header` ("gpgsig" or "gpgsig-sha256"): the shape git
/// writes, with a continuation line per armor line. The body is not a real
/// signature; nothing here verifies it.
#[cfg(test)]
pub(crate) fn fake_signature_header(header: &str, armor: &str) -> String {
    format!(
        "{header} -----BEGIN {armor}-----\n \n bm90IGEgcmVhbCBzaWduYXR1cmU=\n -----END {armor}-----\n"
    )
}

/// A signing program that records every call, drains its input, prints
/// `output` and exits: the verifier stand-in for "was a verifier started at
/// all". Returns the directory holding the script and the file it appends one
/// line to per call; the directory is its own, so the stub never dirties the
/// repository under test.
#[cfg(all(test, unix))]
pub(crate) fn counting_verifier(output: &str) -> (TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().expect("stub dir");
    let script = dir.path().join("verifier");
    fs::write(
        &script,
        format!("#!/bin/sh\necho x >> \"$(dirname \"$0\")/count\"\ncat >/dev/null\n{output}\n"),
    )
    .expect("write verifier stub");
    let mut permissions = fs::metadata(&script).expect("stub metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("stub executable");
    (dir, script)
}

/// Points `gpg.program`, `gpg.ssh.program` and `gpg.x509.program` of `repo`
/// at `script`, so whichever format a commit claims, its verifier is the stub.
#[cfg(all(test, unix))]
pub(crate) fn use_verifier(repo: &Path, script: &Path) {
    for key in ["gpg.program", "gpg.ssh.program", "gpg.x509.program"] {
        git_in(repo, &["config", key, script.to_str().expect("stub path")]);
    }
}

/// Times the stub in `dir` has been called.
#[cfg(all(test, unix))]
pub(crate) fn verifier_spawns(dir: &TempDir) -> usize {
    fs::read_to_string(dir.path().join("count")).map_or(0, |count| count.lines().count())
}

#[cfg(test)]
mod tests {
    use super::{coverage_relaxed, isolated_libtest_command, run_within};
    use std::time::Duration;

    /// A helper process that never exits fails the test with its label and the
    /// budget, and the runner kills it instead of waiting for it.
    #[cfg(unix)]
    #[test]
    #[should_panic(expected = "wedged-helper did not finish within 1s")]
    fn run_within_fails_a_helper_that_outlives_its_budget() {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("600");
        run_within(cmd, "wedged-helper", Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn run_within_returns_the_output_of_a_helper_that_finishes() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "printf out; printf err >&2; exit 3"]);
        let run = run_within(
            cmd,
            "quick-helper",
            coverage_relaxed(Duration::from_secs(30)),
        );
        assert_eq!(
            (run.stdout.as_slice(), run.stderr.as_slice()),
            (&b"out"[..], &b"err"[..])
        );
        assert_eq!((run.success, run.status_code), (false, 3));
    }

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

    /// A fixture git spawns under the inheritance lock, so it cannot inherit
    /// a pipe another test thread has not yet made close-on-exec. Checked by
    /// holding the lock: the spawn must wait for it.
    #[test]
    fn fixture_git_waits_for_the_inheritance_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            crate::procguard::with_inheritance_lock(|| {
                held_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(300));
            })
        });
        held_rx.recv().unwrap();
        let started = std::time::Instant::now();
        super::git_in(dir.path(), &["--version"]);
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "spawned while the lock was held ({:?})",
            started.elapsed()
        );
        holder.join().unwrap();
    }

    /// The defect: a killed run left its harness copies in `target/` for good.
    #[test]
    fn copies_left_by_dead_processes_are_swept_and_live_ones_are_not() {
        use super::{fs, sweep_dead_isolates};
        let dir = tempfile::tempdir().unwrap();
        let name = |pid: u32, seq: u32| format!("gitpulse_lib-abc-isolate-{pid}-{seq}");
        let ours = std::process::id();
        for file in [
            name(4_000_001, 0),
            name(4_000_001, 1),
            name(ours, 0),
            // Not ours to touch: another stem, and no pid at all.
            "other_lib-abc-isolate-4000001-0".to_owned(),
            "gitpulse_lib-abc-isolate-notapid-0".to_owned(),
            "gitpulse_lib-abc".to_owned(),
        ] {
            fs::write(dir.path().join(file), b"image").unwrap();
        }
        let removed = sweep_dead_isolates(dir.path(), "gitpulse_lib-abc", |pid| pid == ours);
        assert_eq!(removed, 2);
        let left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        for kept in [
            name(ours, 0),
            "other_lib-abc-isolate-4000001-0".to_owned(),
            "gitpulse_lib-abc-isolate-notapid-0".to_owned(),
            "gitpulse_lib-abc".to_owned(),
        ] {
            assert!(left.contains(&kept), "{kept} was removed: {left:?}");
        }
        assert_eq!(left.len(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn this_process_is_alive_and_an_unused_pid_is_not() {
        use super::process_is_alive;
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(4_000_001));
    }

    #[test]
    fn coverage_relaxed_never_tightens_and_only_loosens_for_a_reason() {
        let budget = Duration::from_millis(400);
        let instrumented = std::env::var_os("CARGO_LLVM_COV").is_some()
            || std::env::var_os("LLVM_PROFILE_FILE").is_some();
        let relaxed = coverage_relaxed(budget);
        assert!(relaxed >= budget, "{relaxed:?}");
        if instrumented {
            assert!(relaxed >= Duration::from_secs(5), "{relaxed:?}");
        } else if super::env::host_load().is_none_or(|(load, cpus)| load < cpus as f64 * 0.8) {
            // A host with a core free for every runnable task, by a margin the
            // slow-moving average cannot cross between the two samples.
            assert_eq!(relaxed, budget);
        }
    }
}

/// The hermetic environment's canary: the places GitPulse keeps state, and the
/// git configuration every fixture repository inherits, must all resolve under
/// this process's own root and never under the developer's home. A resolver
/// added later that reads some other variable gets its own line here, which is
/// the point: the failure names the one path that escaped.
#[cfg(test)]
mod hermetic_canary {
    use super::env::{env_serial, hermetic};
    use crate::procguard::LockedSpawn;
    use std::path::{Path, PathBuf};

    fn assert_under_root(what: &str, path: Option<PathBuf>) {
        let root = hermetic::root();
        let path = path.unwrap_or_else(|| panic!("{what} did not resolve at all"));
        assert!(
            path.starts_with(&root),
            "{what} resolved to {}, outside this test process's hermetic root {}; a test that \
             reads it is reading (and may be writing) the developer's real state",
            path.display(),
            root.display()
        );
    }

    #[test]
    fn every_per_user_state_directory_resolves_under_the_per_process_root() {
        // Another test may have a scoped override bound; read between them.
        let _serial = env_serial();
        assert_under_root(
            "tool_config::default_config_dir",
            crate::tool_config::default_config_dir(),
        );
        assert_under_root(
            "tool_config::default_cache_dir",
            crate::tool_config::default_cache_dir(),
        );
        assert_under_root(
            "workbench::conversation::claude_home",
            crate::workbench::conversation::claude_home(),
        );
        assert_under_root("ingest::transcript_root", crate::ingest::transcript_root());
        assert_under_root(
            "repository_trust::persistent_root",
            crate::repository_trust::persistent_root().ok(),
        );
    }

    #[test]
    fn this_process_is_not_running_in_the_developers_home() {
        let _serial = env_serial();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        assert!(
            home.as_deref()
                .is_some_and(|home| home.starts_with(hermetic::root())),
            "HOME is {home:?}, not under {}",
            hermetic::root().display()
        );
        if let Some(real) = hermetic::real_home() {
            assert!(
                !home.as_deref().is_some_and(|home| home.starts_with(&real)),
                "HOME still points into the developer's {}",
                real.display()
            );
        }
    }

    /// A developer's `core.hooksPath`, `init.defaultBranch` or `commit.gpgsign`
    /// applies inside every fixture repository unless git is told where its
    /// global and system configuration are. Asked of git itself rather than
    /// the environment, because the claim is about what git reads.
    #[test]
    fn git_reads_no_configuration_outside_the_fixture() {
        let _serial = env_serial();
        assert_eq!(
            std::env::var("GIT_CONFIG_NOSYSTEM").as_deref(),
            Ok("1"),
            "system git configuration is not disabled"
        );
        let fixture = PathBuf::from(
            std::env::var_os("GIT_CONFIG_GLOBAL").expect("GIT_CONFIG_GLOBAL must be set"),
        );
        assert!(
            fixture.starts_with(hermetic::root()),
            "GIT_CONFIG_GLOBAL is {}",
            fixture.display()
        );
        let outside = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("git")
            .args(["config", "--list", "--show-origin"])
            .current_dir(outside.path())
            .output_locked()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git config --list: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let listing = String::from_utf8_lossy(&output.stdout);
        let foreign: Vec<&str> = listing
            .lines()
            .filter(|line| !origin_is_under(line, hermetic::root().as_path()))
            .collect();
        assert!(
            foreign.is_empty(),
            "git reads configuration from outside the hermetic root: {foreign:?}"
        );
    }

    fn origin_is_under(line: &str, root: &Path) -> bool {
        line.strip_prefix("file:")
            .is_some_and(|rest| Path::new(rest).starts_with(root))
    }
}

/// Every lock a test takes before touching the environment is the same lock.
///
/// `GITPULSE_TOOL_CONFIG` was bound under the tool-config lock while
/// `devmap::cli::resolve_binary` read the very same file under the sidecar
/// serial, so a reader holding the one it believed was *the* serial saw another
/// test's binding half-installed. Asserted through the reader's own lock rather
/// than by naming the locks, because the claim is about what a reader observes.
#[cfg(test)]
mod one_environment_lock {
    use super::env::{bind_env, env_serial};
    use crate::tool_config::TOOL_CONFIG_ENV;
    use std::sync::mpsc;
    use std::time::Duration;

    const WRITER_VALUE: &str = "/writer/tools.json";

    #[test]
    fn a_reader_under_the_sidecar_serial_never_observes_a_concurrently_bound_tool_config() {
        let (bound_tx, bound_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let writer = std::thread::spawn(move || {
            let serial = env_serial();
            let _env = bind_env(&serial).set(TOOL_CONFIG_ENV, WRITER_VALUE);
            bound_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        bound_rx.recv().unwrap();

        let (seen_tx, seen_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _sidecar = crate::harness::sidecar::test_serial();
            seen_tx.send(std::env::var_os(TOOL_CONFIG_ENV)).unwrap();
        });
        // Long enough that a reader which is not excluded has certainly run;
        // short enough to be invisible. If the reader is excluded it is still
        // blocked here, which is the point.
        std::thread::sleep(Duration::from_millis(150));
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        reader.join().unwrap();

        let seen = seen_rx.recv().unwrap();
        assert_eq!(
            seen, None,
            "a reader holding the sidecar serial observed another test's TOOL_CONFIG_ENV binding; \
             the two serials are not one lock"
        );
    }

    #[test]
    fn the_lock_is_reentrant_on_its_owner_and_releases_on_the_last_drop() {
        let outer = env_serial();
        let inner = env_serial();
        drop(inner);
        let (tx, rx) = mpsc::channel();
        let other = std::thread::spawn(move || {
            let _guard = env_serial();
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "another thread took the lock while the owner still held it"
        );
        drop(outer);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the lock was never released after the last guard dropped");
        other.join().unwrap();
    }
}

/// The disk-write guard: a test process that writes without bound is stopped,
/// loudly, instead of finishing green an hour later having burned the SSD.
#[cfg(test)]
mod diskio_guard {
    use super::env::diskio::{self, Limits};
    use std::io::Write;
    use std::time::Duration;

    const MIB: u64 = 1 << 20;

    fn limits(rate_mib: u64, window_s: u64, total_mib: u64) -> Limits {
        Limits {
            rate: rate_mib * MIB,
            window: Duration::from_secs(window_s),
            total: total_mib * MIB,
        }
    }

    fn at(seconds: u64, mib: u64) -> (Duration, u64) {
        (Duration::from_secs(seconds), mib * MIB)
    }

    #[test]
    fn a_sustained_rate_over_the_window_breaks_the_budget() {
        // 101 MB/s, the rate of the 549.8 GiB report, against a 40 MiB/s limit.
        let samples: Vec<_> = (0..=40).map(|s| at(s, s * 100)).collect();
        let why = diskio::verdict(&samples, &limits(40, 30, 1 << 20)).expect("over budget");
        assert!(why.contains("MiB/s"), "{why}");
    }

    #[test]
    fn a_short_burst_inside_the_window_is_not_a_runaway() {
        // 2 GiB in one second, then quiet: 2 GiB over 30 s is ~68 MiB/s, so use
        // a limit that the *average* respects while the instant rate does not.
        let mut samples = vec![at(0, 0), at(1, 2048)];
        samples.extend((2..=31).map(|s| at(s, 2048)));
        assert_eq!(diskio::verdict(&samples, &limits(80, 30, 1 << 20)), None);
    }

    #[test]
    fn a_young_process_has_no_rate_to_judge_yet() {
        let samples = [at(0, 0), at(5, 5_000)];
        assert_eq!(diskio::verdict(&samples, &limits(40, 30, 1 << 20)), None);
    }

    #[test]
    fn the_total_budget_applies_whatever_the_rate() {
        let samples = [at(0, 0), at(600, 9_000)];
        let why = diskio::verdict(&samples, &limits(1 << 20, 30, 8_192)).expect("over total");
        assert!(why.contains("budget"), "{why}");
        assert_eq!(diskio::verdict(&[], &limits(40, 30, 8_192)), None);
    }

    #[test]
    fn the_budget_setting_can_tighten_or_disable_but_never_silently_disable() {
        assert!(diskio::limits(None).is_some());
        assert_eq!(diskio::limits(Some("0")), None);
        assert_eq!(diskio::limits(Some("64")).map(|l| l.total), Some(64 * MIB));
        assert!(
            diskio::limits(Some("lots")).is_some(),
            "an unparseable budget must leave the guard on"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_os_counter_sees_what_this_process_writes() {
        let before = diskio::bytes_written().expect("this platform has a write counter");
        let dir = tempfile::tempdir().unwrap();
        let mut file = std::fs::File::create(dir.path().join("payload")).unwrap();
        file.write_all(&vec![0xA5u8; 16 * MIB as usize]).unwrap();
        file.sync_all().unwrap();
        let after = diskio::bytes_written().expect("counter still readable");
        assert!(
            after >= before + 8 * MIB,
            "wrote 16 MiB and synced it, but the counter moved {} bytes",
            after.saturating_sub(before)
        );
    }

    /// Runs only as the child of the test below: writes until something stops it.
    #[test]
    #[ignore = "child of a_runaway_writer_is_aborted_by_the_watchdog"]
    fn child_runaway_writer() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = std::fs::File::create(dir.path().join("runaway")).unwrap();
        let chunk = vec![0x5Au8; 8 * MIB as usize];
        for _ in 0..24 {
            file.write_all(&chunk).unwrap();
            file.sync_all().unwrap();
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    #[cfg(unix)]
    fn run_child(budget: &str) -> std::process::Output {
        use crate::procguard::LockedSpawn;
        let (mut command, _guard) =
            super::isolated_libtest_command("test_support::diskio_guard::child_runaway_writer");
        command
            .arg("--ignored")
            .env(diskio::BUDGET_ENV, budget)
            .stdin(std::process::Stdio::null());
        command.output_locked().expect("run child test")
    }

    /// The guard itself: a child allowed 64 MiB that writes 192 MiB must be
    /// stopped by the watchdog; the same child with the guard off must finish,
    /// which is what shows the stop was the guard's and not the writer's.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_runaway_writer_is_aborted_by_the_watchdog() {
        let guarded = run_child("64");
        let stderr = String::from_utf8_lossy(&guarded.stderr);
        assert!(
            !guarded.status.success() && stderr.contains("DISK WRITE BUDGET EXCEEDED"),
            "the watchdog did not stop a writer over its budget: {:?}\n{stderr}",
            guarded.status
        );

        let unguarded = run_child("0");
        assert!(
            unguarded.status.success(),
            "with the guard off the child must complete (it is the control): {}\n{}",
            String::from_utf8_lossy(&unguarded.stdout),
            String::from_utf8_lossy(&unguarded.stderr)
        );
    }
}
