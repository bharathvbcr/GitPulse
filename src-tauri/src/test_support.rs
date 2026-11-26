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
#[cfg(target_os = "macos")]
pub(crate) mod fsmonitor;

/// Converts a filesystem path to forward-slash POSIX notation so Git hooks and
/// `diff.<driver>.textconv` scripts execute identically on Unix and Git for
/// Windows's bundled MSYS2 `sh`.
#[cfg(test)]
pub(crate) fn sh_path(path: &Path) -> String {
    plain_git_path(path).to_string_lossy().replace('\\', "/")
}

/// Formats `path` as a single-quoted POSIX shell token (forward slashes,
/// verbatim Win32 prefixes stripped, embedded `'` escaped as `'\''`) so
/// spaces and apostrophes in temp paths never break `#!/bin/sh` fixtures.
#[cfg(test)]
pub(crate) fn sh_quote(path: &Path) -> String {
    format!("'{}'", sh_path(path).replace('\'', "'\\''"))
}

/// Writes an executable POSIX `#!/bin/sh` script at `path` (setting `0o755` on
/// Unix; Git for Windows invokes `.git/hooks/*` and `textconv` scripts through
/// its bundled `sh` without requiring a POSIX execute bit).
#[cfg(test)]
pub(crate) fn write_git_script(path: &Path, body: &str, executable: bool) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = executable;
}

/// Writes an executable PATH stub named `name` inside `bin_dir`: an executable
/// POSIX shell script on Unix, and a CRLF `.cmd` batch script on Windows (which
/// `engine::process::env::find_in_dirs` resolves before the bare name).
#[cfg(test)]
pub(crate) fn write_path_stub(
    bin_dir: &Path,
    name: &str,
    unix_sh: &str,
    windows_cmd: &str,
) -> PathBuf {
    std::fs::create_dir_all(bin_dir).unwrap();
    #[cfg(unix)]
    {
        let _ = windows_cmd;
        let path = bin_dir.join(name);
        write_git_script(&path, unix_sh, true);
        path
    }
    #[cfg(windows)]
    {
        let _ = unix_sh;
        let path = bin_dir.join(format!("{name}.cmd"));
        let crlf = windows_cmd.replace("\r\n", "\n").replace('\n', "\r\n");
        std::fs::write(&path, crlf).unwrap();
        path
    }
}

/// Prepends `dir` to the inherited `PATH` using the OS path separator (`:` on
/// Unix, `;` on Windows) so tests that inject a fake binary keep `git` and
/// system utilities reachable on every platform.
#[cfg(test)]
pub(crate) fn prepend_path_dir(dir: &Path) -> std::ffi::OsString {
    let mut entries = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(entries).expect("join PATH entries")
}

/// Writes an executable stand-in for `gitpulse-hook` at `path` that answers
/// `--version` the way a build with lease mode does, for tests of the
/// `lease-bin` links, which only link to a binary that says it is a lease
/// wrapper. Written to a temporary name and renamed into place, so nothing
/// runs a half-written file.
#[cfg(unix)]
pub(crate) fn fake_lease_hook(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let body = "#!/bin/sh\n\
        if [ \"$1\" = --version ]; then\n\
        printf 'gitpulse-hook 0.0.0-test\\nsubcommands: collision-guard, command-gate, lease\\n'\n\
        fi\n";
    let tmp = path.with_extension("tmp-hook");
    std::fs::write(&tmp, body).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::time::Duration;
use tempfile::TempDir;

static HARNESS_CLONE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Stamps every generation in a hand-built DevMap fixture store with this
/// build's resolver epoch, the way `Store::save_generation` stamps a real one.
///
/// A fixture that writes payloads under `current_payload_identity` claims a
/// store the current devmap wrote. Since the store began reading
/// `analysis_json.resolver_epoch` (`Store::latest_generation_resolver_stamp`),
/// a generation without the key reads as written before the stamp existed, so
/// status reports it not fresh and affected-tests fails closed with "stored
/// extraction payload is obsolete". Fixtures that wrote the identity and not
/// the stamp failed five tests on the re-vendor that brought the check in,
/// although it changed nothing they test. One owner, so the next field a
/// current store must carry is added once.
pub(crate) fn stamp_current_resolver_epoch(conn: &rusqlite::Connection) {
    let stamped = conn
        .execute(
            "UPDATE generations SET analysis_json = json_set(analysis_json, '$.resolver_epoch', ?1)",
            [devmap_resolve::resolver::RESOLUTION_REUSE_EPOCH],
        )
        .expect("stamp the fixture's generations with the current resolver epoch");
    assert!(stamped > 0, "the fixture store has no generation to stamp");
}

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

/// Re-executes a libtest filter from a second name for this harness.
///
/// `cargo llvm-cov --workspace` can unlink the running `current_exe()` while
/// later cases still need to spawn it. `Command::spawn` then fails with
/// `NotFound` even though this process is still mapped. Linking beside the
/// original keeps `@rpath` intact and gives the child a path cargo will not
/// replace, without writing the image again (a copy is the fallback where
/// linking fails). Keep the guard alive until the child exits.
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
    // A hard link names the same file under a second name, which is all the
    // isolation needs (`cargo llvm-cov` unlinks the original's *name*; the file
    // lives on under this one) and writes no bytes. A copy writes the whole
    // test image, hundreds of MB, every call: free on macOS (APFS clones it) but
    // 410 MiB a call on Linux, where a serial run of the `devmap::live` tests
    // alone averaged 164 MiB/s and was aborted by the disk-write watchdog. The
    // copy remains the fallback for a filesystem that cannot link.
    if fs::hard_link(&src, &path).is_err() {
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
pub(crate) use env::{coverage_relaxed, wait_until};

/// Runs `cmd` to completion inside `budget` through the engine's bounded
/// runner (process group killed at the deadline, output capped) and panics
/// naming `label` when it does not finish: a fixture's helper process that
/// wedges fails the test with a message instead of parking the whole suite.
///
/// The command is run exactly as built: binary, environment and working
/// directory are the caller's. Only stdin is closed. The exit status is left
/// to the caller.
///
/// A budget past the runner's ceiling ([`crate::engine::process::NETWORK_TIMEOUT`])
/// is held to it: [`coverage_relaxed`] scales budgets by instrumentation and
/// host load, and an unclamped 60 s reached 1,920 s on a loaded instrumented
/// run, which the runner refuses without starting the helper.
#[cfg(all(test, unix))]
pub(crate) fn run_within(
    cmd: Command,
    label: &str,
    budget: std::time::Duration,
) -> crate::engine::process::BoundedRun {
    let budget = budget.min(crate::engine::process::NETWORK_TIMEOUT);
    crate::engine::process::run_bounded(cmd, label, budget, None)
        .and_then(|run| run.require_complete(label))
        .unwrap_or_else(|why| {
            panic!(
                "{label} did not finish within {budget:?}: {why}",
                why = why.message()
            )
        })
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
    let (dir, output) = pinned_git(dir, args);
    assert!(
        output.status.success(),
        "git {args:?} failed with {}",
        output.status
    );
    if args.first() == Some(&"init") {
        pin_repo_local_identity(&dir);
        trust_repo(&dir);
    }
}

/// Runs a merge (or any command that may stop on a conflict) in `dir` the way
/// [`git_in`] runs everything, and says whether it conflicted: `false` when it
/// succeeded, `true` when it failed and left unmerged entries in the index.
///
/// Any other failure panics with git's own words. A fixture that only checked
/// that its merge failed read every refusal as a conflict: on a runner with
/// no identity, `git merge` stops at "Committer identity unknown" before it
/// merges anything, and the tests went on to report "no longer conflicted",
/// a rollback that had nothing to roll back, or a stage count of 1, instead
/// of the reason.
#[cfg(test)]
pub(crate) fn git_merge_in(dir: &Path, args: &[&str]) -> bool {
    let (dir, output) = pinned_git(dir, args);
    if output.status.success() {
        return false;
    }
    let (_, unmerged) = pinned_git(&dir, &["ls-files", "--unmerged"]);
    assert!(
        unmerged.status.success() && !unmerged.stdout.is_empty(),
        "git {args:?} failed with {} without leaving a conflict: {}{}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim(),
        String::from_utf8_lossy(&output.stdout).trim()
    );
    true
}

/// The command [`git_in`] and [`git_merge_in`] run, and what it printed.
#[cfg(test)]
fn pinned_git(dir: &Path, args: &[&str]) -> (PathBuf, std::process::Output) {
    // Persisting a fixture identity is how contract@gitpulse.test became the
    // author of real commits. `-c` on one command is hermetic; `git config
    // user.*` is not, and this checkout refuses to store it.
    if args.first() == Some(&"config")
        && args.get(1).is_some_and(|key| key.starts_with("user."))
        && dir.join(".githooks/pre-commit").is_file()
    {
        panic!("refusing to persist a fixture identity; this checkout rejects it");
    }
    let dir = plain_git_path(dir);
    let plain_args: Vec<String> = args.iter().map(|arg| plain_git_arg(arg)).collect();
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
        .args(&plain_args)
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // The pinned config and home, named outright: a neighbour's scoped rebind
    // of `HOME` or `GIT_CONFIG_GLOBAL` must not reach a fixture command.
    pin_git(&mut command);
    // Spawned under the inheritance lock like every production child: this
    // runs on test threads beside code whose pipes are not yet close-on-exec,
    // and a child forked in that window keeps their ends for its lifetime.
    let child = command.spawn_locked().expect("spawn git");
    let output = child.wait_with_output().expect("wait for git");
    (dir, output)
}

/// Writes hermetic local identity/signing settings into a test repo's
/// `.git/config` so production `git_cli` writes (which strip
/// `GIT_CONFIG_GLOBAL`) can commit on runners whose passwd GECOS field is empty.
#[cfg(test)]
fn pin_repo_local_identity(dir: &Path) {
    let dir = plain_git_path(dir);
    if dir.join(".githooks/pre-commit").is_file() {
        return;
    }
    for (key, value) in [
        ("user.name", "GitPulse"),
        ("user.email", "gitpulse@test.local"),
        ("commit.gpgsign", "false"),
    ] {
        let mut cmd = Command::new("git");
        pin_git(&mut cmd);
        cmd.args(["config", "--local", key, value])
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = cmd.output_locked().expect("spawn git config");
        assert!(
            output.status.success(),
            "fixture git config failed with {}",
            output.status
        );
    }
}

/// The hermetic config directory inside this test process's isolated `HOME`,
/// unaffected by transient `HOME` rebinds on sibling threads.
#[cfg(test)]
pub(crate) fn hermetic_config_dir() -> PathBuf {
    let home = env::hermetic::home();
    #[cfg(target_os = "macos")]
    {
        home.join("Library")
            .join("Application Support")
            .join("GitPulse")
    }
    #[cfg(target_os = "windows")]
    {
        home.join("AppData").join("Roaming").join("GitPulse")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".config").join("gitpulse")
    }
}

/// Sets the modification time of a file or directory.
///
/// `File::open(dir).set_times(..)` is `Access is denied` on Windows twice over:
/// opening a directory needs `FILE_FLAG_BACKUP_SEMANTICS`, and `set_times`
/// needs write-attribute access, which a read-only open does not carry. This
/// asks for exactly those two things, and is the plain open elsewhere.
#[cfg(test)]
pub(crate) fn set_modified(path: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        options.write(true);
        if path.is_dir() {
            options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        }
    }
    #[cfg(not(windows))]
    options.read(true);
    options
        .open(path)?
        .set_times(fs::FileTimes::new().set_modified(when))
}

/// `arg` in the spelling git takes on its command line.
///
/// Fixtures spell directories with `canonicalize()`, which on Windows answers
/// `\\?\C:\...`, and hand them to git as a worktree target, a clone source or a
/// remote URL (bare or as `file://`). Git refuses that form -- `could not
/// create leading directories ... Invalid argument`, `fatal: Cannot resolve` --
/// so the prefix is dropped here, once, for every fixture command. Any other
/// argument, and a path with no ordinary spelling, is returned unchanged.
/// Compiled on every host so the rewrite is tested where the suite is run.
#[cfg(test)]
pub(crate) fn plain_git_arg(arg: &str) -> String {
    use crate::engine::process::simplified_windows_path;
    if let Some(path) = arg.strip_prefix("file://") {
        return simplified_windows_path(path)
            .map_or_else(|| arg.to_string(), |p| format!("file://{p}"));
    }
    simplified_windows_path(arg).unwrap_or_else(|| arg.to_string())
}

/// What git reads from the environment besides [`STRIPPED_GIT_ENV`], that a
/// fixture command must not inherit: the ceiling and filesystem rules that
/// decide whether the fixture repository is found, the identities and dates a
/// commit is stamped with, the defaults a new repository is made with, and the
/// pagers, editors and protocol rules. From the names the `git` binary on this
/// host holds (`strings $(git --exec-path)/git`; `git help environment` has no
/// manual here), minus the ones production already strips. A name set by a
/// neighbour after [`pin_git`] scans the process is still removed, because each
/// is removed by name; [`pin_git`] also sweeps every inherited `GIT_*` for the
/// names this host's git does not list but a newer one may.
///
/// [`STRIPPED_GIT_ENV`]: crate::engine::process
#[cfg(test)]
const FIXTURE_STRIPPED_GIT_ENV: &[&str] = &[
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_AUTHOR_IDENT",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_COMMITTER_IDENT",
    "GIT_DEFAULT_BRANCH",
    "GIT_DEFAULT_HASH",
    "GIT_DEFAULT_REF_FORMAT",
    "GIT_REFERENCE_BACKEND",
    "GIT_TEMPLATE_DIR",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_GRAFT_FILE",
    "GIT_NOTES_REF",
    "GIT_NOTES_DISPLAY_REF",
    "GIT_NOTES_REWRITE_REF",
    "GIT_NOTES_REWRITE_MODE",
    "GIT_REFLOG_ACTION",
    "GIT_INDEX_VERSION",
    "GIT_ALLOW_PROTOCOL",
    "GIT_PROTOCOL",
    "GIT_PROTOCOL_FROM_USER",
    "GIT_ATTR_GLOBAL",
    "GIT_ATTR_SYSTEM",
    "GIT_ATTR_NOSYSTEM",
    "GIT_EDITOR",
    "GIT_PAGER",
    "GIT_MERGE_AUTOEDIT",
    "GIT_MERGE_VERBOSITY",
    "GIT_REF_PARANOIA",
    "GIT_QUARANTINE_PATH",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_DIFF_TOOL",
    "GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE",
    "GIT_SHELL_PATH",
    "GIT_COMMIT_GRAPH_PARANOIA",
    "GIT_DISABLE_UNTRACKED_CACHE",
    "GIT_FORCE_UNTRACKED_CACHE",
    "GIT_NO_LAZY_FETCH",
];

/// Gives a fixture's `git` child the hermetic git environment, whatever a
/// neighbouring test has rebound in the process.
///
/// A neighbour that rebinds a variable holds the environment lock, which a
/// fixture helper does not take, so the helper reads the neighbour's value:
/// its config, its `GIT_DIR`, its identity, a config file it has just deleted
/// ("unknown error occurred while reading the configuration files"). Only the
/// product call under test should see a rebound environment. So nothing git
/// reads is inherited: every name production strips
/// ([`strip_injected_git_env`]), the fixture-only names in
/// [`FIXTURE_STRIPPED_GIT_ENV`] and any other inherited `GIT_*` are removed on
/// the `Command`, never in the process, and the config and home are named
/// outright: the pinned global config, no system config, and the hermetic
/// `HOME`, `XDG_CONFIG_HOME` and `USERPROFILE`.
///
/// Call it before adding the command's own environment: a variable the caller
/// sets afterwards (an author date, a template) is the caller's choice and
/// survives.
///
/// [`strip_injected_git_env`]: crate::engine::process::strip_injected_git_env
#[cfg(test)]
pub(crate) fn pin_git(command: &mut Command) -> &mut Command {
    crate::engine::process::strip_injected_git_env(command);
    for name in FIXTURE_STRIPPED_GIT_ENV {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(&name);
        }
    }
    let home = env::hermetic::home();
    command
        .env("GIT_CONFIG_GLOBAL", env::hermetic::pinned_git_config())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("USERPROFILE", &home)
        .env("HOME", home)
}

#[cfg(test)]
/// Git for Windows does not treat a `\\?\` verbatim path as a repository it
/// just created in that directory: `git init` can succeed with that spelling
/// as the working directory, and the next `git` given the same spelling as an
/// argument reports "Not a Git repository" or "unknown error occurred while
/// reading the configuration files". The Win32 extended prefix is a spelling,
/// not a different directory.
pub(crate) fn plain_git_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };
    if let Some(unc) = rest.strip_prefix(r"UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    PathBuf::from(rest)
}

/// `path` canonicalized and spelled the way git and Node print a Windows path
/// (`C:/Users/...`): no `\\?\` prefix, `/` for every separator. A fixture
/// that appends `/name` to a root as text then holds one spelling throughout,
/// where `canonicalize` plus a `/` join is `\\?\C:\...\/name`, a name Windows
/// refuses (os error 123). Identical to `canonicalize` off Windows.
#[cfg(test)]
pub(crate) fn canonical_slash_path(path: &Path) -> std::io::Result<PathBuf> {
    let plain = crate::engine::process::canonicalize_plain(path)?;
    if cfg!(windows) {
        return Ok(PathBuf::from(plain.to_string_lossy().replace('\\', "/")));
    }
    Ok(plain)
}

/// Binds the devmap binary the product itself resolves (`GITPULSE_DEVMAP_BIN`,
/// saved config, the app bin dir, then `PATH`, with `.exe` and `USERPROFILE`
/// handled on Windows) for the life of the returned guard. The tests that need
/// a real index build used to probe `$HOME/.local/bin` first: that has no
/// `.exe`, is dead on Windows, and could prefer a stale copy over the one the
/// product would run. Panics when no devmap is installed.
#[cfg(test)]
pub(crate) fn bind_real_devmap() -> crate::devmap::cli::TestBinaryBinding {
    crate::devmap::cli::bind_resolved_test_binary()
}

/// An absolute path that names no directory. `unix` is written the Unix way
/// (`/gone/a`); on Windows it gains a drive (`C:\gone\a`), because `/gone` has
/// no root there and is refused as a relative path.
#[cfg(test)]
pub(crate) fn absent_absolute_path(unix: &str) -> String {
    if cfg!(windows) {
        return format!("C:{}", unix.replace('/', "\\"));
    }
    unix.to_string()
}

/// Whether this platform's file system can hold a file at `rel`, a repository
/// path spelled with `/`. Everywhere but Windows it can hold anything a git
/// path can. Windows refuses a component with `< > : " \ | ? *` or a control
/// character (a `\` there is a separator, so the name would not be one
/// component), one that ends in a dot or a space, and a reserved device name
/// (`CON`, `NUL`, `COM1`, ... with or without an extension). That is not a
/// GitPulse bug, so a fixture that exercises such a name asserts the platform's
/// outcome instead of creating it.
#[cfg(test)]
pub(crate) fn platform_can_hold(rel: &str) -> bool {
    if !cfg!(windows) {
        return true;
    }
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    rel.split('/').all(|part| {
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        !part.is_empty()
            && !part.ends_with(['.', ' '])
            && !part
                .chars()
                .any(|c| c.is_control() || "<>:\"\\|?*".contains(c))
            && !RESERVED.contains(&stem.as_str())
    })
}

#[cfg(test)]
mod pin_adversary_tests;

#[cfg(test)]
mod config_value_tests;

#[cfg(all(test, target_os = "macos"))]
mod fsmonitor_tests;

#[cfg(test)]
mod fs_time_tests;

#[cfg(test)]
mod plain_git_arg_tests {
    use super::plain_git_arg;

    #[test]
    fn a_verbatim_path_is_plain_for_git_as_a_bare_argument_and_as_a_file_url() {
        assert_eq!(
            plain_git_arg(r"\\?\C:\Users\runneradmin\Temp\wt"),
            r"C:\Users\runneradmin\Temp\wt"
        );
        assert_eq!(
            plain_git_arg(r"file://\\?\C:\Users\runneradmin\Temp\src"),
            r"file://C:\Users\runneradmin\Temp\src"
        );
        assert_eq!(plain_git_arg(r"\\?\UNC\srv\share\r"), r"\\srv\share\r");
    }

    #[test]
    fn anything_else_is_left_alone() {
        for arg in [
            "HEAD:refs/heads/x",
            "-b",
            "/tmp/repo",
            r"C:\plain\path",
            r"\\?\Volume{1234}\x",
            "file:///tmp/x",
        ] {
            assert_eq!(plain_git_arg(arg), arg);
        }
    }
}

#[cfg(test)]
mod plain_path_tests {
    use super::plain_git_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_verbatim_win32_prefix_is_stripped_for_git() {
        assert_eq!(
            plain_git_path(Path::new(
                r"\\?\C:\Users\runneradmin\AppData\Local\Temp\.tmpabc"
            )),
            PathBuf::from(r"C:\Users\runneradmin\AppData\Local\Temp\.tmpabc")
        );
        assert_eq!(
            plain_git_path(Path::new(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
        assert_eq!(
            plain_git_path(Path::new("/tmp/repo")),
            PathBuf::from("/tmp/repo")
        );
    }

    #[test]
    fn sh_path_and_sh_quote_handle_verbatim_unc_spaces_and_apostrophes() {
        assert_eq!(
            super::sh_path(Path::new(r"\\?\C:\Users\runner admin\Temp\it's\hook")),
            "C:/Users/runner admin/Temp/it's/hook"
        );
        assert_eq!(
            super::sh_path(Path::new(r"\\?\UNC\server\share\repo")),
            "//server/share/repo"
        );
        assert_eq!(
            super::sh_quote(Path::new(r"\\?\C:\Temp\it's a test\out.txt")),
            r"'C:/Temp/it'\''s a test/out.txt'"
        );
    }

    #[test]
    fn prepend_path_dir_places_dir_first_using_platform_separator() {
        let dir = tempfile::tempdir().unwrap();
        let joined = super::prepend_path_dir(dir.path());
        let mut iter = std::env::split_paths(&joined);
        assert_eq!(iter.next().as_deref(), Some(dir.path()));
    }

    #[test]
    fn write_path_stub_writes_expected_platform_stub() {
        let dir = tempfile::tempdir().unwrap();
        let stub = super::write_path_stub(
            dir.path(),
            "demo-tool",
            "#!/bin/sh\necho ok\n",
            "@echo off\necho ok\n",
        );
        assert!(stub.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(stub.file_name().and_then(|n| n.to_str()), Some("demo-tool"));
            let mode = std::fs::metadata(&stub).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0);
        }
        #[cfg(windows)]
        {
            assert_eq!(
                stub.file_name().and_then(|n| n.to_str()),
                Some("demo-tool.cmd")
            );
            let body = std::fs::read_to_string(&stub).unwrap();
            assert!(
                body.contains("\r\n"),
                "Windows .cmd must use CRLF: {body:?}"
            );
        }
    }
}

/// Explicit approval for an owned fixture through the production grant path.
#[cfg(test)]
pub(crate) fn trust_repo(dir: &Path) {
    let plain = plain_git_path(dir);
    let path = plain.to_str().expect("fixture path");
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
            crate::engine::process::canonicalize_plain(&path).unwrap()
        })
        .collect();
    (main, parent, paths)
}

/// A repository parked between the two commits of a multi-commit
/// `cherry-pick` (or `revert`, when `revert`): the first commit conflicted and
/// was resolved with a plain `git commit`, so `CHERRY_PICK_HEAD` /
/// `REVERT_HEAD` is gone while `.git/sequencer/` still holds the second step
/// and `git status` still says the operation is in progress.
#[cfg(test)]
pub(crate) fn repo_parked_in_a_sequence(revert: bool) -> TempDir {
    let dir = git_repo();
    let repo = dir.path();
    let write = |name: &str, body: &str| fs::write(repo.join(name), body).expect("write fixture");
    let commit = |message: &str| {
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-q", "-m", message]);
    };
    write("f.txt", "base\n");
    commit("base");
    let ids = if revert {
        write("f.txt", "one\n");
        commit("one");
        write("f.txt", "two\n");
        commit("two");
        write("g.txt", "three\n");
        commit("three");
        ["HEAD~2", "HEAD"]
    } else {
        git_in(repo, &["switch", "-q", "-c", "side"]);
        write("f.txt", "side\n");
        commit("side one");
        write("g.txt", "side two\n");
        commit("side two");
        git_in(repo, &["switch", "-q", "main"]);
        write("f.txt", "main\n");
        commit("main moves f");
        ["side~1", "side"]
    };
    let mut command = Command::new("git");
    pin_git(&mut command);
    command
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            "user.email=gitpulse@test.local",
            "-c",
            "commit.gpgsign=false",
            if revert { "revert" } else { "cherry-pick" },
            "--no-edit",
        ])
        .args(ids)
        .current_dir(repo)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let first = command.spawn_locked().expect("spawn git");
    let first = first.wait_with_output().expect("wait for git");
    assert!(
        !first.status.success(),
        "the first step was meant to conflict: {}",
        String::from_utf8_lossy(&first.stdout)
    );
    write("f.txt", "resolved\n");
    git_in(repo, &["add", "f.txt"]);
    git_in(repo, &["commit", "-q", "--no-edit"]);
    assert!(
        repo.join(".git/sequencer/todo").is_file(),
        "the fixture must leave a sequencer behind"
    );
    dir
}

/// A fresh repository on `main`, for suites that only need somewhere to scan.
///
/// Identical bodies stood in `analyzer::coverage` and `analyzer::deps`; the
/// default-branch name is the kind of fact that gets corrected in one copy.
#[cfg(test)]
pub(crate) fn git_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let mut cmd = Command::new("git");
    pin_git(&mut cmd);
    let status = cmd
        .args(["init", "-b", "main"])
        .current_dir(plain_git_path(dir.path()))
        .status_locked()
        .expect("git init");
    assert!(status.success());
    pin_repo_local_identity(dir.path());
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

/// `remove_dir_all` that waits out a Windows sharing violation (os error 32),
/// access denied (5) or non-empty directory (145): a handle another thread or
/// process closed a moment ago (a scanner, an exiting child) still pins the
/// entry, and a delete that races it fails though nothing is wrong. Other
/// failures, and a pin that outlasts the bound (about 5 s), are returned.
/// Where a directory deletes at once this is exactly `remove_dir_all`.
#[cfg(test)]
pub(crate) fn remove_dir_all_retrying(path: &Path) -> std::io::Result<()> {
    let mut last = None;
    for _ in 0..50u32 {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 145)) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("fifty attempts all failed"))
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
    #[cfg(unix)]
    use super::run_within;
    use super::{coverage_relaxed, isolated_libtest_command};
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

    /// A budget relaxed past the runner's ceiling (60 s x 8 instrumented x 4 at
    /// the load cap is 1,920 s) still runs the helper. Passed through as is,
    /// the runner refused it at once: "deadline exceeds 1800s".
    #[cfg(unix)]
    #[test]
    fn run_within_runs_a_helper_whose_budget_is_past_the_runners_ceiling() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "printf ran"]);
        let past = crate::engine::process::NETWORK_TIMEOUT + Duration::from_secs(120);
        let run = run_within(cmd, "relaxed-helper", past);
        assert_eq!(run.stdout.as_slice(), &b"ran"[..]);
        assert!(run.success);
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

    /// The harness image is hundreds of MB. A copy of it per call wrote 410 MiB
    /// each on Linux (APFS clones it for free, which hid this on macOS), so the
    /// serial Ubuntu run was aborted by the disk-write watchdog. The isolated
    /// name must be a second name for the same file, not a second file.
    #[cfg(unix)]
    #[test]
    fn isolated_harness_is_a_second_name_for_the_image_not_a_copy_of_it() {
        use std::os::unix::fs::MetadataExt;
        let (_command, guard) = isolated_libtest_command("does-not-need-to-exist");
        let src = std::env::current_exe().expect("test executable");
        let (image, isolated) = (
            std::fs::metadata(&src).expect("image metadata"),
            std::fs::metadata(guard.path()).expect("isolated metadata"),
        );
        assert_eq!(
            (image.dev(), image.ino()),
            (isolated.dev(), isolated.ino()),
            "the isolated harness is a separate file ({} bytes written)",
            isolated.len()
        );
        assert!(isolated.nlink() >= 2, "no second name for the image");
        assert_ne!(
            isolated.mode() & 0o111,
            0,
            "the isolated harness cannot run"
        );
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

    /// A neighbour's scoped rebind of `GIT_CONFIG_GLOBAL` or `HOME` must not
    /// reach a fixture command: it holds the environment lock, which `git_in`
    /// does not take, so a fixture running beside it read the neighbour's
    /// config, or one the neighbour had just deleted ("unknown error occurred
    /// while reading the configuration files"). The lock is held here so the
    /// rebind is deterministic, not a race.
    #[test]
    fn git_in_reads_the_pinned_config_whatever_a_neighbour_has_rebound() {
        use crate::test_support::env::{bind_env, env_serial, write_git_config};
        let serial = env_serial();
        let neighbour = tempfile::tempdir().unwrap();

        // The config another test pointed `GIT_CONFIG_GLOBAL` at.
        let rebound = neighbour.path().join("rebound.gitconfig");
        write_git_config(&rebound, "[init]\n\tdefaultBranch = leaked\n").unwrap();
        let repo = tempfile::tempdir().unwrap();
        {
            let _env = bind_env(&serial).set("GIT_CONFIG_GLOBAL", &rebound);
            super::git_in(repo.path(), &["init", "-q"]);
        }
        let head = std::fs::read_to_string(repo.path().join(".git/HEAD")).unwrap();
        assert!(
            !head.contains("leaked"),
            "git_in read the rebound config: {head}"
        );

        // A fake home whose `.gitconfig` git cannot parse, with no
        // `GIT_CONFIG_GLOBAL` left to override it.
        std::fs::write(neighbour.path().join(".gitconfig"), "[broken\n").unwrap();
        let second = tempfile::tempdir().unwrap();
        let _env = bind_env(&serial)
            .remove("GIT_CONFIG_GLOBAL")
            .set("HOME", neighbour.path())
            .set("XDG_CONFIG_HOME", neighbour.path());
        super::git_in(second.path(), &["init", "-q"]);
    }

    use super::{git_in, git_merge_in, git_repo, write};
    use tempfile::TempDir;

    /// Two branches that edit one line, ready for `merge side`.
    fn diverged(conflicting: bool) -> TempDir {
        let dir = git_repo();
        write(dir.path(), "f.txt", "base\n");
        git_in(dir.path(), &["add", "f.txt"]);
        git_in(dir.path(), &["commit", "-qm", "base"]);
        git_in(dir.path(), &["checkout", "-qb", "side"]);
        write(dir.path(), "f.txt", "side\n");
        git_in(dir.path(), &["commit", "-qam", "side"]);
        git_in(dir.path(), &["checkout", "-q", "main"]);
        let (path, text) = if conflicting {
            ("f.txt", "main\n")
        } else {
            ("g.txt", "main\n")
        };
        write(dir.path(), path, text);
        git_in(dir.path(), &["add", path]);
        git_in(dir.path(), &["commit", "-qm", "main"]);
        dir
    }

    #[test]
    fn git_merge_in_tells_a_conflict_from_a_clean_merge() {
        let clean = diverged(false);
        assert!(!git_merge_in(clean.path(), &["merge", "--no-edit", "side"]));
        let conflict = diverged(true);
        assert!(git_merge_in(
            conflict.path(),
            &["merge", "--no-edit", "side"]
        ));
        assert!(conflict.path().join(".git/MERGE_HEAD").exists());
    }

    /// A merge that fails without merging anything is not a conflict: the
    /// fixture stops there, with git's reason, rather than letting the test
    /// read whatever state the refusal left.
    #[test]
    #[should_panic(expected = "without leaving a conflict")]
    fn git_merge_in_refuses_a_failure_that_left_no_conflict() {
        let dir = diverged(true);
        git_merge_in(dir.path(), &["merge", "--no-edit", "no-such-branch"]);
    }

    /// The code under test commits with whatever identity git resolves, and
    /// the hermetic home pins one, so that resolution never reaches the
    /// host's account. Without it, commits by the code under test failed on
    /// the Linux CI runner (`empty ident name`) and in containers (`unable to
    /// auto-detect email address`) but passed where the account had a name.
    /// Asked through `GitWriter::identity`, whose git reads `$HOME/.gitconfig`
    /// rather than `GIT_CONFIG_GLOBAL`, so the home copy is the one checked.
    #[test]
    fn a_fresh_repository_resolves_the_hermetic_identity_not_the_hosts() {
        let repo = tempfile::tempdir().unwrap();
        super::git_in(repo.path(), &["init", "-q"]);
        let identity =
            crate::engine::git_writer::GitWriter::identity(repo.path().to_str().unwrap()).unwrap();
        assert_eq!(identity.name.as_deref(), Some("GitPulse"), "{identity:?}");
        assert_eq!(
            identity.email.as_deref(),
            Some("gitpulse@test.local"),
            "{identity:?}"
        );
    }

    /// `GIT_WORK_TREE` moves the work tree git reads files from: with a
    /// neighbour's value inherited, `add f.txt` looks for `f.txt` in the
    /// neighbour's directory and the fixture's file is never staged.
    #[test]
    fn git_in_does_not_follow_a_neighbours_work_tree() {
        use crate::test_support::env::{bind_env, env_serial};
        let serial = env_serial();
        let neighbour = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        super::git_in(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("f.txt"), "x").unwrap();
        {
            let _env = bind_env(&serial).set("GIT_WORK_TREE", neighbour.path());
            super::git_in(repo.path(), &["add", "f.txt"]);
        }
        let index = std::fs::read(repo.path().join(".git/index")).unwrap();
        assert!(
            index.windows(5).any(|window| window == b"f.txt"),
            "f.txt was not staged: git_in followed the neighbour's GIT_WORK_TREE"
        );
    }

    /// The defect: a killed run left its harness copies in `target/` for good.
    #[test]
    fn copies_left_by_dead_processes_are_swept_and_live_ones_are_not() {
        use super::{fs, sweep_dead_isolates};
        let dir = tempfile::tempdir().unwrap();
        let name = |pid: u32, seq: u32| format!("waylode_lib-abc-isolate-{pid}-{seq}");
        let ours = std::process::id();
        for file in [
            name(4_000_001, 0),
            name(4_000_001, 1),
            name(ours, 0),
            // Not ours to touch: another stem, and no pid at all.
            "other_lib-abc-isolate-4000001-0".to_owned(),
            "waylode_lib-abc-isolate-notapid-0".to_owned(),
            "waylode_lib-abc".to_owned(),
        ] {
            fs::write(dir.path().join(file), b"image").unwrap();
        }
        let removed = sweep_dead_isolates(dir.path(), "waylode_lib-abc", |pid| pid == ours);
        assert_eq!(removed, 2);
        let left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        for kept in [
            name(ours, 0),
            "other_lib-abc-isolate-4000001-0".to_owned(),
            "waylode_lib-abc-isolate-notapid-0".to_owned(),
            "waylode_lib-abc".to_owned(),
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
        // Every transcript source's root, each by its own name: a source
        // added later is listed here without anyone remembering to.
        for (source, root) in crate::ingest::source_roots() {
            assert_under_root(&format!("ingest transcript root of {source}"), root);
        }
        assert_under_root(
            "repository_trust::persistent_root",
            crate::repository_trust::persistent_root().ok(),
        );
    }

    /// The variables that would point a transcript source at the developer's own
    /// agent history are gone from the test process: a root that resolves "under
    /// the hermetic home" proves nothing if one of them is set.
    #[test]
    fn no_variable_names_the_developers_agent_history() {
        let _serial = env_serial();
        for key in [
            "GITPULSE_TRANSCRIPT_ROOT",
            "GITPULSE_CLAUDE_CONFIG_DIRS",
            "GITPULSE_CODEX_TRANSCRIPT_ROOT",
            "CODEX_HOME",
            "ANTIGRAVITY_HOME",
        ] {
            assert!(
                std::env::var_os(key).is_none(),
                "{key} is set in the test process; a test reading it reads real transcripts"
            );
        }
    }

    /// Nothing GitPulse, Manvi, lappi or portless could have exported into the
    /// developer's shell survives into the test process: `MANVI_STATE_DIR`,
    /// `GITPULSE_SESSION_ID`, `LAPPI_SOCKET` and the rest are what a GitPulse
    /// terminal injects, and a test that reads one passes or fails by where it
    /// was launched. Only the hermetic markers and named opt-in test drivers
    /// (see `hermetic::is_kept_test_knob`) remain.
    #[test]
    fn no_inherited_override_variable_survives_the_constructor() {
        let _serial = env_serial();
        let leaked: Vec<String> = std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| hermetic::is_scrubbed(name))
            .collect();
        assert!(
            leaked.is_empty(),
            "these inherited variables reached the test process: {leaked:?}"
        );
    }

    /// `temp_dir()` callers write under the hermetic root, not the real temp
    /// directory.
    #[test]
    fn the_temp_directory_is_under_the_per_process_root() {
        let _serial = env_serial();
        assert_under_root("std::env::temp_dir", Some(std::env::temp_dir()));
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
        assert!(
            !home.as_deref().is_some_and(hermetic::reaches_real_home),
            "HOME still points into the developer's {:?}",
            hermetic::real_home()
        );
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

    /// The Windows runner's lines, spelled with a backslash git must quote on
    /// any platform: read raw, a quoted origin named no file under the root.
    #[test]
    fn a_quoted_origin_is_read_as_the_path_it_quotes() {
        let root = Path::new(r"/tmp/gph-1\2");
        assert!(origin_is_under(
            "file:\"/tmp/gph-1\\\\2/gitconfig\"\tgc.auto=0",
            root
        ));
        assert!(origin_is_under(
            "file:/tmp/gph-1\\2/gitconfig\tgc.auto=0",
            root
        ));
        assert!(!origin_is_under(
            "file:\"/Users/dev/.gitconfig\"\tcore.hooksPath=/x",
            root
        ));
        // An unterminated quote names nothing, so it counts as foreign.
        assert!(!origin_is_under("file:\"/tmp/gph-1\\\\2/gitconfig", root));
        assert!(!origin_is_under("command line:\tgc.auto=0", root));
    }

    /// `file:<path>\t<key>=<value>`. git quotes the path C-style when it holds
    /// a backslash, so on Windows the origin is `file:"C:\\Users\\…\\gitconfig"`
    /// and, read raw, named no file under any root.
    fn origin_is_under(line: &str, root: &Path) -> bool {
        let Some(rest) = line.strip_prefix("file:") else {
            return false;
        };
        let origin = match rest.strip_prefix('"') {
            Some(quoted) => match quoted.find("\"\t") {
                Some(end) => quoted[..end].replace("\\\\", "\\").replace("\\\"", "\""),
                None => return false,
            },
            None => rest.split('\t').next().unwrap_or(rest).to_string(),
        };
        Path::new(&origin).starts_with(root)
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
    fn the_measured_suite_rate_stays_under_the_ceiling() {
        // 89 MiB/s for 30 s is the coverage job's abort (llvm-cov, one thread).
        // 84 MiB/s is the 8-thread Ubuntu suite. Both must be inside the ceiling;
        // twice that rate must still abort.
        let mib: u64 = 1024 * 1024;
        let limits = diskio::limits(None).expect("the guard is on by default");
        let healthy = [
            (Duration::ZERO, 0),
            (Duration::from_secs(30), 89 * mib * 30),
        ];
        assert!(
            diskio::verdict(&healthy, &limits).is_none(),
            "the coverage suite's 89 MiB/s was treated as a runaway"
        );
        let runaway = [
            (Duration::ZERO, 0),
            (Duration::from_secs(30), 200 * mib * 30),
        ];
        assert!(diskio::verdict(&runaway, &limits).is_some());
    }

    #[test]
    fn the_measured_suite_total_stays_under_the_budget() {
        // The whole lib suite is one process. Measured 2026-11-23: about
        // 10.2 GiB for the coverage suite (scaled from where runs 45268885253
        // and 45270424352 crossed the old 8 GiB), 9623 MiB for its tests
        // measured alone, and over 8196 MiB for the Ubuntu suite (run
        // 45266451066). Each must fit, written slowly enough that only the
        // total is judged; twice the coverage suite must still abort.
        let limits = diskio::limits(None).expect("the guard is on by default");
        for (what, mib) in [
            ("the coverage suite", 10_445),
            ("the tests measured alone", 9_623),
            ("the Ubuntu suite at its old abort", 8_196),
        ] {
            let healthy = [at(0, 0), at(2_400, mib)];
            assert!(
                diskio::verdict(&healthy, &limits).is_none(),
                "{what}'s {mib} MiB was treated as a runaway"
            );
        }
        let twice = [at(0, 0), at(4_800, 2 * 10_445)];
        let why = diskio::verdict(&twice, &limits).expect("twice the suite aborts");
        assert!(why.contains("budget"), "{why}");
        assert!(
            !why.contains("MiB/s"),
            "the total, not the rate, aborts it: {why}"
        );
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

    /// The counter is this process's own, as the budget says: what a child it
    /// started and waited for writes is the child's, held to the child's own
    /// watchdog. Linux's `/proc/self/io` folds every reaped child into its
    /// parent, so a suite that spawns git and itself was charged for all of it
    /// and aborted at 40 MiB/s on CI (fec1e008). The threshold is half the
    /// child's write because tests beside this one write too.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_os_counter_does_not_charge_this_process_for_its_children() {
        use crate::procguard::LockedSpawn;
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("child-payload");
        let before = diskio::bytes_written().expect("this platform has a write counter");
        let status = std::process::Command::new("/bin/dd")
            .arg("if=/dev/zero")
            .arg(format!("of={}", payload.display()))
            .args(["bs=1048576", "count=128"])
            .stderr(std::process::Stdio::null())
            .status_locked()
            .expect("run dd");
        let after = diskio::bytes_written().expect("counter still readable");
        assert!(status.success(), "dd failed: {status:?}");
        assert_eq!(std::fs::metadata(&payload).unwrap().len(), 128 * MIB);
        assert!(
            after.saturating_sub(before) < 64 * MIB,
            "a child's 128 MiB was charged to this process: the counter moved {} bytes",
            after.saturating_sub(before)
        );
    }

    /// The running total is printed once per 512 MiB passed, at the multiple
    /// reached, and never for a sample that passed none.
    #[test]
    fn milestones_are_printed_once_per_multiple_passed() {
        let step = diskio::MILESTONE_EVERY;
        assert_eq!(diskio::milestone_passed(0, step - 1), None);
        assert_eq!(diskio::milestone_passed(0, step), Some(step));
        assert_eq!(diskio::milestone_passed(step, step + MIB), None);
        assert_eq!(
            diskio::milestone_passed(step + MIB, 3 * step + 1),
            Some(3 * step)
        );
        assert_eq!(diskio::milestone_passed(3 * step, 2 * step), None);
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
