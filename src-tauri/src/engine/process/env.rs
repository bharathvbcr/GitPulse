//! Subprocess environment pinning, scrubbing, allowlisting, and tool lookup.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Environment names git reads that can redirect it away from the repository
/// and settings the caller picked, or change what it prints. Every git child
/// has these removed ([`pin_git_environment`]), whichever builder spawned it.
///
/// - the repository pointers: where the index, objects, alternates, common dir,
///   namespace and shallow file live;
/// - the config channels, `GIT_CONFIG_PARAMETERS` being the shell-quoted one
///   (`'alias.st=!sh -c …'` outranks even repo-local config);
/// - transport and credential redirection: `GIT_SSH`, `GIT_SSH_COMMAND`,
///   `GIT_PROXY_COMMAND`, `GIT_ASKPASS` and friends;
/// - output and matching behaviour: `GIT_DIFF_OPTS` overrides `-U3`, and the
///   pathspec switches (`GIT_LITERAL_PATHSPECS`, `GIT_GLOB_PATHSPECS`,
///   `GIT_NOGLOB_PATHSPECS`, `GIT_ICASE_PATHSPECS`) retarget the `:(literal)`
///   pathspecs the writers rely on, so `stage f*.txt` could touch other files;
/// - `GIT_ATTR_SOURCE`, which swaps the attributes diffs and filters see.
pub(super) const STRIPPED_GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_SHALLOW_FILE",
    "GIT_ATTR_SOURCE",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_EXTERNAL_DIFF",
    "GIT_SEQUENCE_EDITOR",
    "GIT_EXEC_PATH",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_PROXY_COMMAND",
    "GIT_ASKPASS",
    "GIT_DIFF_OPTS",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// Families of names removed by prefix: the numbered config channel, credential
/// helper overrides, and every tracing switch (`GIT_TRACE`, `GIT_TRACE2*`,
/// `GIT_TRACE_PACKET`, ...), whose output floods stderr and pushes the fatal
/// line out of the few bytes of it that are kept.
pub(super) const STRIPPED_GIT_ENV_PREFIXES: &[&str] = &[
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "GIT_CREDHELPER",
    "GIT_TRACE",
];

/// True for inherited environment names [`pin_git_environment`] removes.
pub(crate) fn is_injected_git_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    STRIPPED_GIT_ENV.contains(&upper.as_str())
        || STRIPPED_GIT_ENV_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

/// The locale a git child runs under. Messages are pinned to English (the
/// parsers match git's words) through `LC_MESSAGES` and `LANGUAGE`, which
/// govern message catalogs and nothing else. `LC_ALL` is NOT set: it overrides
/// every other locale variable, so a hook, gpg or pinentry that git execs would
/// get the C locale (US-ASCII for Ruby) where the same command works from a
/// terminal. The inherited `LANG` and `LC_CTYPE` pass through; an inherited
/// `LC_ALL` is removed, and when it was the user's only encoding (neither `LANG`
/// nor `LC_CTYPE` set) its value moves into `LC_CTYPE` so the encoding survives.
/// Returns the `LC_CTYPE` to set, if any.
pub(crate) fn lc_ctype_from_inherited(
    lc_all: Option<&std::ffi::OsStr>,
    lang: Option<&std::ffi::OsStr>,
    lc_ctype: Option<&std::ffi::OsStr>,
) -> Option<std::ffi::OsString> {
    let present = |v: Option<&std::ffi::OsStr>| v.is_some_and(|v| !v.is_empty());
    match lc_all {
        Some(all) if present(lc_all) && !present(lang) && !present(lc_ctype) => {
            Some(all.to_os_string())
        }
        _ => None,
    }
}

/// The one place the environment of a git child is decided. Both spawn
/// builders ([`git_command_with_env`] for git itself and
/// [`build_capture_command_in`] for tools that shell out to git, such as `gh`)
/// call it, so they cannot drift apart: a setting added here reaches both.
///
/// Prompts and pagers are off, messages are English (the parsers match git's
/// words; see [`lc_ctype_from_inherited`] for why `LC_ALL` is not the
/// mechanism; one edge of that policy: a parent with `LC_ALL=C.UTF-8` and
/// `LANG=C` keeps `LANG=C` and loses the UTF-8 `LC_ALL`, because `LANG` counts
/// as the user's stated encoding), optional locks are skipped so a read never contends with a
/// writer, every name in [`STRIPPED_GIT_ENV`] and [`STRIPPED_GIT_ENV_PREFIXES`]
/// is removed, and the editors are pinned to `true`.
///
/// Editors are neutralized AFTER the strip, which would otherwise remove
/// `GIT_SEQUENCE_EDITOR` right back out again. Removal alone is not enough:
/// with none set, git falls back to `core.editor` and then to vi, and any
/// subcommand that wants a message (`rebase --continue`, `am --continue`,
/// `merge --continue`, a bare `commit`) launches it against a null stdin and
/// blocks until the command timeout — surfacing as "git timed out" rather than
/// a result. `true` exits 0 immediately, so git accepts the message it
/// prepared. Every message-bearing call here already passes `-m` or
/// `--no-edit`, so no existing caller's behaviour changes.
pub(crate) fn pin_git_environment(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_MESSAGES", "C")
        .env("LANGUAGE", "C");
    let inherited = |name: &str| std::env::var_os(name);
    if let Some(ctype) = lc_ctype_from_inherited(
        inherited("LC_ALL").as_deref(),
        inherited("LANG").as_deref(),
        inherited("LC_CTYPE").as_deref(),
    ) {
        cmd.env("LC_CTYPE", ctype);
    }
    cmd.env_remove("LC_ALL");
    strip_injected_git_env(cmd);
    cmd.env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true");
}

/// Removes every name in [`STRIPPED_GIT_ENV`] and [`STRIPPED_GIT_ENV_PREFIXES`]
/// from the child's environment: the explicit names always (so a name set after
/// this call but before the spawn is still removed), and the prefix families by
/// scanning what the process holds now.
///
/// The one list of what must not reach a git child, shared by
/// [`pin_git_environment`] and the test fixtures' `test_support::pin_git`, which
/// strip more on top of it (identities, the ceiling, templates) rather than
/// keep a second copy of this one.
pub(crate) fn strip_injected_git_env(cmd: &mut Command) {
    for name in STRIPPED_GIT_ENV {
        cmd.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        if is_injected_git_env(&name.to_string_lossy()) {
            cmd.env_remove(&name);
        }
    }
}

/// Fallback directories searched when a bare tool name is missing from the
/// inherited `PATH`, and appended to the child's PATH so nested lookups (a
/// shebang script's interpreter, `govulncheck` shelling out to `go`) resolve
/// the same way.
///
/// GUI launches on macOS (Finder, Dock, `open`) hand the app a minimal PATH
/// (`/usr/bin:/bin:/usr/sbin:/sbin`) that omits Homebrew and user-local bin
/// directories, so `Command::new("gh")` failed to resolve even though the CLI
/// was installed — every GitHub view then reported "`gh` is not installed".
/// Terminal launches never saw this. Superset of the convention mirrored from
/// [`crate::harness::sidecar::resolve_binary`] with Go and Rust toolchain
/// locations for scanners that spawn `go` or `cargo` themselves; nonexistent
/// entries are harmlessly skipped.
///
/// `CARGO_HOME` is deliberately not consulted for the Rust entry. A custom
/// value is set in a shell profile, and a GUI launch inherits no shell
/// profile — so in the launch this whole mechanism exists for, reading it
/// would find nothing. In the launches where it *is* set, PATH was inherited
/// too and resolution never reaches this list. `~/.cargo/bin` is rustup's
/// default and the only spelling reachable here.
pub(super) fn gui_launch_fallback_dirs(home: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = home {
        dirs.push(PathBuf::from(home).join(".local/bin"));
        // Grok Build installs the real binary under `~/.grok/bin` and may
        // only symlink `grok` into `~/.local/bin`. A GUI PATH that sees
        // neither Homebrew nor that symlink still has to find it.
        dirs.push(PathBuf::from(home).join(".grok/bin"));
    }
    // Fixed system path, so it does not depend on knowing `home` — gating it
    // on that withheld a still-valid directory from launchd/daemon contexts,
    // which are exactly the ones with no inherited PATH. Kept in its original
    // position so no existing precedence between these dirs shifts.
    dirs.push(PathBuf::from("/usr/local/go/bin"));
    if let Some(home) = home {
        // Standard GOBIN/GOPATH install location (`go install ...` default).
        dirs.push(PathBuf::from(home).join("go/bin"));
        // rustup's install root: `cargo`, `rustc`, `rustup` and every
        // `cargo-*` subcommand binary (`cargo-audit`, `cargo-llvm-cov`) live
        // here and nowhere a GUI-launch PATH can see.
        dirs.push(PathBuf::from(home).join(".cargo/bin"));
    }
    dirs
}

/// Whose `PATH` a spawn was handed, which decides whether the
/// [`gui_launch_fallback_dirs`] may widen it.
///
/// The fallback dirs repair an *inherited* PATH that a GUI launch left
/// minimal. A PATH a caller chose on purpose — a test simulating a host with
/// a tool absent — is the whole answer, and widening it made the choice
/// meaningless: a scan told "cargo-deny is not on this PATH" still found
/// `/opt/homebrew/bin/cargo-deny` on any host that had one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathSource {
    /// Read from the process environment: searched first, then the fallback
    /// dirs, and the child's PATH is extended the same way.
    Inherited,
    /// Supplied by the caller: searched alone, and handed to the child as is.
    Explicit,
}

/// Directories a bare program name is searched in, in precedence order.
///
/// Single owner of that list for resolution ([`resolve_spawn_program_in`],
/// [`find_external_tool_with`]). Empty entries are dropped: POSIX reads one as
/// "the current directory".
fn tool_search_dirs(
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect();
    if source == PathSource::Inherited {
        dirs.extend(gui_launch_fallback_dirs(home));
    }
    dirs
}

/// First entry of `dirs` holding a spawnpable file named `program`.
///
/// Deliberately stricter than an `is_file()` scan so resolution matches what
/// the OS PATH walk would have done:
/// - relative directory entries are ignored. POSIX reads an empty entry as
///   "the current directory", and a relative candidate would be resolved by
///   the spawn against the child's post-`chdir` working directory (`cwd`
///   argument), silently searching the wrong tree;
/// - on Unix the candidate must carry an execute bit — `execvp` skips
///   non-executable matches rather than failing the whole lookup, and picking
///   one would turn "tool found" into a PermissionDenied spawn error;
/// - broken symlinks and directories named like the tool are skipped;
/// - on Windows the suffixed spellings are tried BEFORE the bare name, in the
///   order the OS itself uses, and a name that already carries one of those
///   suffixes is never double-suffixed.
pub(crate) fn find_in_dirs(program: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let names = spawn_candidate_names(program);
    dirs.iter().filter(|dir| dir.is_absolute()).find_map(|dir| {
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| is_executable_file(candidate))
    })
}

/// Apple's `/usr/bin/git` is not git. It is the `xcrun` shim, which works out
/// the active developer directory and then execs the real binary under it, on
/// every call. `benches/process_spawn.rs` times `git rev-parse HEAD` with the
/// child environment set here, interleaved: on an M-series host the shim's
/// p50 was 7.9 ms, Homebrew's git 4.2 ms (-47%) and the developer-tools
/// binary the shim execs 3.3 ms (-59%). Homebrew's is preferred anyway: it is
/// the git a user installed on purpose, and usually newer. Every git read
/// GitPulse makes is a spawn, so the shim roughly doubled the cost of each.
/// SIP keeps `/usr/bin` read-only, so this path is always the shim.
#[cfg(target_os = "macos")]
pub(crate) const XCRUN_GIT_SHIM: &str = "/usr/bin/git";

/// Package-manager directories whose `git` is preferred over the shim:
/// Homebrew on Apple silicon, then Homebrew on Intel and other `/usr/local`
/// installs. Deliberately not every GUI fallback directory — a stray `git` in
/// `~/.cargo/bin` or `~/go/bin` is not one the user chose as their git.
#[cfg(target_os = "macos")]
pub(crate) const REAL_GIT_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// The developer directories `xcrun` itself would consult, most specific
/// first: `DEVELOPER_DIR`, then the `xcode-select` choice (a symlink, so no
/// spawn is needed to read it), then the Command Line Tools default.
#[cfg(target_os = "macos")]
pub(crate) fn developer_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("DEVELOPER_DIR").filter(|dir| !dir.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(dir) = std::fs::read_link("/var/db/xcode_select_link") {
        dirs.push(dir);
    }
    dirs.push(PathBuf::from("/Library/Developer/CommandLineTools"));
    dirs
}

/// `resolved`, unless it is the `xcrun` shim and a real git is installed:
/// then a package-manager git from `real_git_dirs`, else the binary the shim
/// would have exec'd from the first of `developer_dirs` that holds one. A
/// git found earlier on `PATH` than `/usr/bin` is the user's choice and is
/// returned untouched; so is the shim when no real git exists, because then
/// the shim is what offers to install the developer tools.
#[cfg(target_os = "macos")]
pub(crate) fn past_xcrun_git_shim(
    resolved: PathBuf,
    real_git_dirs: &[PathBuf],
    developer_dirs: &[PathBuf],
) -> PathBuf {
    let shim = Path::new(XCRUN_GIT_SHIM);
    if resolved != shim {
        return resolved;
    }
    let packaged = real_git_dirs.iter().map(|dir| dir.join("git"));
    // `DEVELOPER_DIR` may name the `Xcode.app` bundle as well as its
    // `Contents/Developer`; `xcrun` accepts both, so both are tried.
    let developer = developer_dirs.iter().flat_map(|dir| {
        [
            dir.join("usr/bin/git"),
            dir.join("Contents/Developer/usr/bin/git"),
        ]
    });
    // A candidate that is a link back to the shim would cost exactly what the
    // shim does, so it is judged by where it leads, not by its own name.
    packaged
        .chain(developer)
        .filter(|candidate| candidate.is_absolute() && is_executable_file(candidate))
        .find(|candidate| std::fs::canonicalize(candidate).is_ok_and(|target| target != shim))
        .unwrap_or(resolved)
}

/// Executable suffixes tried on Windows, in the order `CreateProcess`'
/// own `PATHEXT` walk uses them.
///
/// A fixed list rather than the environment's `PATHEXT`: the default value of
/// that variable also carries `.VBS`, `.JS`, `.WSF` and friends, which are
/// Windows Script Host inputs. Resolving a tool name to one of those would
/// hand an interpreter a script this process never meant to run, and no tool
/// GitPulse spawns ships as one.
#[cfg(windows)]
const WINDOWS_EXECUTABLE_SUFFIXES: &[&str] = &[".com", ".exe", ".bat", ".cmd"];

/// Names to try for `program` in one directory, most specific first.
///
/// On Windows the bare name is tried LAST, not first. Node installs both
/// `npm.cmd` and an extension-less `npm` — a shell script for Git Bash — in
/// the same directory, and `is_executable_file` counts any regular file there.
/// Trying the bare name first therefore resolved the shell script and handed
/// it to `CreateProcess`, which fails with "%1 is not a valid Win32
/// application" (os error 193). The OS PATH walk would have found `npm.cmd`.
///
/// SECURITY: this makes `.bat`/`.cmd` resolvable, so a spawn can now reach a
/// batch file. It does not introduce shell interpretation: `std::process`
/// detects a batch target, routes it through `cmd.exe` itself, and quotes the
/// arguments with cmd's own rules — returning `InvalidInput` for an argument
/// it cannot safely escape rather than emitting a mis-quoted command line
/// (the CVE-2024-24576 fix, present since 1.77.2). Building a `cmd /c` line
/// here by hand is what would have added that surface.
pub(super) fn spawn_candidate_names(program: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        let lower = program.to_ascii_lowercase();
        if WINDOWS_EXECUTABLE_SUFFIXES
            .iter()
            .any(|suffix| lower.ends_with(suffix))
        {
            return vec![program.to_string()];
        }
        let mut names: Vec<String> = WINDOWS_EXECUTABLE_SUFFIXES
            .iter()
            .map(|suffix| format!("{program}{suffix}"))
            .collect();
        names.push(program.to_string());
        names
    }
    #[cfg(not(windows))]
    {
        vec![program.to_string()]
    }
}

/// True when `path` is a regular, executable file (symlinks followed; broken
/// links, directories and non-executable files are false).
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Windows counterpart: any regular file counts (extension handling is the
/// caller's job via [`find_in_dirs`]' name list).
#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Longest inherited `PATH` [`extended_child_path`] will still append to.
///
/// `argv` and `envp` share one buffer and `execve` fails the whole spawn with
/// `E2BIG` once it is full — 1 MiB on this project's macOS hosts, measured by
/// the test that pins this rather than taken from a manual. A PATH anywhere
/// near that is pathological, but appending to it is how a helpful change
/// becomes the reason a spawn that used to work stops working, and the failure
/// arrives as "Failed to spawn git: Argument list too long" with nothing
/// pointing back here. Below this ceiling the handful of short fallback dirs
/// cannot move the total meaningfully; at or above it the child keeps exactly
/// the environment it would have inherited without us.
pub(super) const MAX_INHERITED_PATH_BYTES: usize = 64 * 1024;

/// Child-side `PATH` value: inherited entries first (precedence preserved),
/// then the [`gui_launch_fallback_dirs`] that are not already present.
///
/// Resolving the top-level program is not enough. Shebang scripts re-resolve
/// their interpreter through `env` against the CHILD's PATH, and tools such as
/// `npm` (`#!/usr/bin/env node`), `composer` (`php`) and `bundler-audit`
/// (`ruby`) live exactly where a GUI-minimal PATH cannot see. Without this,
/// resolving `/opt/homebrew/bin/npm` succeeds and the spawn still dies with
/// "env: node: No such file or directory" (exit 127) — which reads downstream
/// as "npm is not installed".
///
/// Returns `None` when the joined value cannot be built (an entry with a
/// disallowed character) or when the inherited value already sits at
/// [`MAX_INHERITED_PATH_BYTES`]; the caller then leaves the inherited PATH
/// untouched rather than degrading the child to an empty one — or to one the
/// kernel refuses to exec.
pub(crate) fn extended_child_path(
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<std::ffi::OsString> {
    if path_var.is_some_and(|value| value.len() >= MAX_INHERITED_PATH_BYTES) {
        return None;
    }
    // Only absolute entries survive. An empty entry is POSIX for "the current
    // directory", and `.`, `bin` or `./node_modules/.bin` resolve against the
    // child's cwd, which for git helpers and sidecars is the repository the
    // user opened. Keeping any of them would let an untrusted clone put its
    // own `git-*`, `node` or `npm` ahead of the real ones.
    let mut entries: Vec<PathBuf> = path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|entry| entry.is_absolute())
        .collect();
    if path_var.is_none() && cfg!(unix) {
        // PATH entirely absent (daemon/launchd-style contexts): seed the Unix
        // default search set (confstr `_CS_PATH`) so appending fallbacks does
        // not leave children with nothing but Homebrew dirs.
        entries.push(PathBuf::from("/usr/bin"));
        entries.push(PathBuf::from("/bin"));
    }
    for dir in gui_launch_fallback_dirs(home) {
        if !entries.contains(&dir) {
            entries.push(dir);
        }
    }
    std::env::join_paths(entries).ok()
}

/// Give a hand-built child the same `PATH` [`build_capture_command`] gives its
/// own, unless the caller already chose one.
///
/// Resolving a tool and *running* it are two different PATH questions, and
/// until this existed only the first was answered. [`find_external_tool`]
/// searches the [`gui_launch_fallback_dirs`] on top of `PATH`, so a
/// GUI-launched GitPulse finds `~/.local/bin/devmap` and then spawns it with
/// launchd's `/usr/bin:/bin:/usr/sbin:/sbin` — a PATH that cannot see the
/// binary we just resolved out of it.
///
/// That is not a cosmetic difference for a tool whose job is to inspect the
/// machine. `devmap doctor` resolves the bare `devmap` command named by host
/// MCP configs against its own `PATH`; handed the minimal one it finds
/// nothing, records `exists: false`, and reports "host MCP config names a
/// devmap path that is not a file: devmap — this is not version skew".
/// GitPulse then renders that as an installation fault. It is not one: it is
/// the PATH we chose, described back to us as the user's broken install — a
/// check that could not run, naming a cause it did not have.
///
/// Two conditions keep this from changing anything else:
///
/// - **The caller wins.** A `PATH` already set on the command (the terminal's
///   own environment, [`build_capture_command`], the harness sidecar) is left
///   exactly as it is; this only fills an absent one.
/// - **The program must already be a path.** Rust falls back from
///   `posix_spawn` to `fork` when `PATH` is overridden for a *bare* program
///   name (`env_saw_path() && !program_is_path()` in `std`), and forking a
///   large GUI process is both slow and the shape behind the concurrent-spawn
///   descriptor races this module already guards. Every production spawn that
///   reaches here carries a resolved path; bare names arrive through
///   [`build_capture_command`] or [`git_command_with_env`], which resolve the
///   program *and* set `PATH` themselves, so the first condition already
///   excludes them.
///
/// Non-Windows only, for the reason [`build_capture_command`] gives: the
/// Windows environment block is case-insensitive, so setting `"PATH"` through
/// `.env()` can collide with an inherited `"Path"` and leave the child holding
/// two different search paths.
pub(crate) fn default_child_path(cmd: &mut Command) {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    default_child_path_with_env(cmd, path_var.as_deref(), home.as_deref());
}

/// [`default_child_path`] with the environment passed in, mirroring the
/// [`git_command`] / [`git_command_with_env`] split so the three guards can be
/// tested without writing to the process environment out from under every
/// other test in the binary.
pub(super) fn default_child_path_with_env(
    cmd: &mut Command,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) {
    if cfg!(windows) {
        return;
    }
    // `components()` rather than a `/` scan: it is the same question `std`
    // asks (`program_is_path`), and it answers it identically for "./devmap"
    // and for a `Path`-typed program built without a literal separator.
    if Path::new(cmd.get_program()).components().count() <= 1 {
        return;
    }
    if cmd
        .get_envs()
        .any(|(key, _)| key == std::ffi::OsStr::new("PATH"))
    {
        return;
    }
    if let Some(child_path) = extended_child_path(path_var, home) {
        cmd.env("PATH", child_path);
    }
}

/// Resolves the `program` argument of [`capture_command`] to a spawner-ready
/// form.
///
/// A name containing a path separator is honored verbatim. A bare name is
/// searched in `path_var` first (preserving normal PATH precedence), then in
/// the [`gui_launch_fallback_dirs`]. Found anywhere, its path is returned;
/// found nowhere, the bare name passes through unchanged so the existing
/// "Failed to spawn …" error keeps naming the tool the caller asked for.
///
/// Crate-visible so the dependency scanner can quote the resolved location of
/// a tool that exists but fails to run, under the same injectable seams its
/// spawns use.
pub(crate) fn resolve_spawn_program_with(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> String {
    resolve_spawn_program_in(program, path_var, home, PathSource::Inherited)
}

/// [`resolve_spawn_program_with`] for a `path_var` of known [`PathSource`]:
/// an explicit one is searched alone.
pub(crate) fn resolve_spawn_program_in(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
) -> String {
    if program.contains('/') || program.contains('\\') {
        return program.to_string();
    }
    find_in_dirs(program, &tool_search_dirs(path_var, home, source))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
}

/// The non-`PATH` directories [`find_external_tool`] searches, for messages
/// that have to tell a user where a missing tool was looked for.
///
/// Derived from the list the lookup itself uses rather than spelled out at the
/// message site: a hand-written list silently goes stale the moment a
/// directory is added here, and a "we looked in X" that omits where we
/// actually looked is a wrong answer, not a terse one.
pub(crate) fn external_tool_fallback_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    gui_launch_fallback_dirs(home.as_deref())
}

/// Shared lookup for subsystems that need to *find* an external tool without
/// spawning it (the harness sidecar's `manvi`, presence checks that want the
/// resolved path). Single owner of PATH + GUI-fallback resolution semantics;
/// bare names only — anything with a separator is not searched.
pub(crate) fn find_external_tool(program: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    find_external_tool_with(program, path_var.as_deref(), home.as_deref())
}

/// [`find_external_tool`] with the environment passed in, so a caller can
/// reproduce a GUI launch's PATH without writing to the process environment.
pub fn find_external_tool_with(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<String> {
    if program.is_empty() || program.contains('/') || program.contains('\\') {
        return None;
    }
    find_in_dirs(
        program,
        &tool_search_dirs(path_var, home, PathSource::Inherited),
    )
    .map(|p| p.to_string_lossy().into_owned())
}

/// Windows' legacy path limit. Beyond it a path needs the `\\?\` prefix to be
/// addressable at all, so a result that long keeps the prefix rather than
/// becoming a spelling Windows cannot open.
#[cfg(windows)]
const WINDOWS_LEGACY_PATH_LIMIT: usize = 260;

/// A `\\?\`-prefixed Windows path rewritten in ordinary form, or `None` when
/// the prefix has to stay.
///
/// Split out as string logic so the rules can be tested on any host: the
/// `Prefix` parse this mirrors only happens on Windows, and these are exactly
/// the cases that were wrong in production.
///
/// Compiled off Windows only for those tests: production has no use for it
/// there, and an ungated copy would be dead code under `-D warnings`.
#[cfg(any(windows, test))]
pub(crate) fn simplified_windows_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix(r"\\?\")?;
    let simplified = if let Some(unc) = rest.strip_prefix(r"UNC\") {
        // \\?\UNC\server\share -> \\server\share
        format!(r"\\{unc}")
    } else {
        // \\?\C:\dir -> C:\dir. Anything that is not a drive-letter root
        // (a device path such as \\?\Volume{...}) has no ordinary spelling.
        let bytes = rest.as_bytes();
        let is_drive_root = bytes.first().is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(1) == Some(&b':')
            && bytes.get(2) == Some(&b'\\');
        if !is_drive_root {
            return None;
        }
        rest.to_string()
    };
    #[cfg(windows)]
    if simplified.len() > WINDOWS_LEGACY_PATH_LIMIT {
        return None;
    }
    Some(simplified)
}

/// `std::fs::canonicalize` in a spelling external tools accept.
///
/// On Windows `canonicalize` always answers with a verbatim `\\?\` path -- it
/// asks the OS for the final name by handle -- and git refuses to use one as a
/// working tree:
///
///   fatal: could not create work tree dir '\\?\C:\...\healthy': Invalid argument
///
/// The prefix also leaks into user-facing text ("Already cloned at \\?\C:\..."),
/// so every caller that hands a resolved path to a child process or to a
/// message wants this rather than `canonicalize`.
///
/// SECURITY: this does not weaken any containment check built on it. The
/// symlink resolution has already happened inside `canonicalize`; removing the
/// prefix does not change which file the path denotes, and callers that
/// compare a child against a parent still compare two paths produced the same
/// way. What it gives up is the extended-length namespace, which is why a
/// result that no longer fits the legacy limit keeps its prefix instead of
/// becoming a path Windows would refuse to open.
///
/// On every other platform this is `canonicalize`, unchanged.
pub(crate) fn canonicalize_plain(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    Ok(match canonical.to_str() {
        Some(text) => PathBuf::from(plain_path_text(text)),
        None => canonical,
    })
}

/// `path` in the spelling git takes as a command-line argument.
///
/// [`canonicalize_plain`] for a path that is already in hand as text: the
/// repository admission and trust identity spell every checkout `\\?\C:\...`
/// (the sandbox containment checks depend on that), and git refuses that form
/// as an argument -- `git worktree add '\\?\C:\...\wt'` dies with `could not
/// create leading directories ... Invalid argument`. A path with no ordinary
/// spelling (beyond the legacy limit, a device path) is returned unchanged.
///
/// The same string reaches the command gate and the spawn, so both judge one
/// spelling. Identity off Windows, where a backslash is a filename character.
pub(crate) fn plain_path_text(path: &str) -> String {
    #[cfg(windows)]
    if let Some(simplified) = simplified_windows_path(path) {
        return simplified;
    }
    path.to_owned()
}

/// Assembles the `capture_command` child with injectable `PATH`/home lookups,
/// mirroring the seam [`resolve_spawn_program_with`] gives the resolver.
///
/// Bare names are resolved up front: the child inherits our environment, but
/// `Command` performs its PATH walk with it, so a GUI-minimal PATH cannot be
/// patched from inside the child. The resolved script's own interpreter lookup
/// (`env node` inside npm) walks the same inherited PATH, so the fallback dirs
/// are appended to the child's PATH too — non-Windows only, where setting
/// "PATH" via `.env()` cannot collide with the case-insensitive "Path"
/// variable in the Windows environment block.
pub(crate) fn build_capture_command(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Command {
    build_capture_command_in(
        program,
        args,
        cwd,
        extra_env,
        path_var,
        home,
        PathSource::Inherited,
    )
}

/// [`build_capture_command`] for a `path_var` of known [`PathSource`]. An
/// explicit PATH is both the resolution search path and, verbatim apart from
/// dropped empty entries, the child's PATH — so nothing the child re-resolves
/// (a shebang interpreter, a nested tool) reaches past it either.
pub(crate) fn build_capture_command_in(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
) -> Command {
    let resolved = resolve_spawn_program_in(program, path_var, home, source);
    let mut cmd = Command::new(&resolved);
    // The same environment [`git_command`] gives git ([`pin_git_environment`]):
    // captured tools shell out to git themselves (`gh pr checkout` fetches and
    // merges in the repo; npm/go/cargo read git config), and CI-style inherited
    // pointers (GIT_DIR, GIT_INDEX_FILE, GIT_CONFIG_PARAMETERS, ...) would
    // redirect them off the repository the caller picked, a localized message
    // would defeat the parsers, and a credential manager could prompt. Applied
    // before `extra_env`, so a caller that explicitly passes one of these
    // still wins.
    pin_git_environment(&mut cmd);
    cmd.env("GH_PROMPT_DISABLED", "1")
        // gh consults this before printing update notices; keep subprocess
        // output deterministic instead of interleaving a self-update banner.
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if !cfg!(windows) {
        // Applied before `extra_env` so an explicit caller-provided PATH wins.
        match source {
            PathSource::Inherited => {
                if let Some(child_path) = extended_child_path(path_var, home) {
                    cmd.env("PATH", child_path);
                }
            }
            // Never left to inherit: the process PATH is exactly what an
            // explicit one replaces.
            PathSource::Explicit => {
                match path_var.and_then(|value| {
                    std::env::join_paths(tool_search_dirs(Some(value), home, source)).ok()
                }) {
                    Some(child_path) => cmd.env("PATH", child_path),
                    None => cmd.env_remove("PATH"),
                };
            }
        }
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd
}

/// Names a scrubbed child may inherit (compared upper-cased). An allowlist, not
/// a denylist: a token variable nobody has heard of yet is dropped by default.
/// Proxy and CA variables are the user's own transport settings, not
/// credentials a repository's config can ask npm to expand into a request.
const SCRUBBED_ENV_ALLOWLIST: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "USER",
    "LOGNAME",
    "LANG",
    "LANGUAGE",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "BUN_INSTALL",
    "VOLTA_HOME",
    "ASDF_DIR",
    "ASDF_DATA_DIR",
    "NVM_DIR",
    "NVM_BIN",
    "FNM_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMDATA",
    "HOMEDRIVE",
    "HOMEPATH",
];

fn scrubbed_env_allows(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "PATH" || upper.starts_with("LC_") || SCRUBBED_ENV_ALLOWLIST.contains(&upper.as_str())
}

/// The variable a child reads its home directory from: `USERPROFILE` on
/// Windows, `HOME` elsewhere.
pub(super) const HOME_ENV_VAR: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

/// Replaces `cmd`'s environment with the allowlisted part of `inherited` plus
/// everything the builder set explicitly (PATH, the git pins, `extra_env`).
/// A variable the builder explicitly removed stays removed. `also_allowed`
/// names further inherited variables to keep (the ones the user's own npm
/// config references). This form keeps only the fixed allowlist; production
/// goes through [`restrict_env_to_allowlist_plus`].
#[cfg(test)]
pub(super) fn restrict_env_to_allowlist<I>(
    cmd: &mut Command,
    inherited: I,
    home: Option<&std::ffi::OsStr>,
) where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    restrict_env_to_allowlist_plus(cmd, inherited, home, &[]);
}

/// [`restrict_env_to_allowlist`] that also keeps the inherited variables named
/// in `also_allowed`.
pub(super) fn restrict_env_to_allowlist_plus<I>(
    cmd: &mut Command,
    inherited: I,
    home: Option<&std::ffi::OsStr>,
    also_allowed: &[String],
) where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    let mut explicit = Vec::new();
    let mut removed = Vec::new();
    for (key, value) in cmd.get_envs() {
        match value {
            Some(value) => explicit.push((key.to_os_string(), value.to_os_string())),
            None => removed.push(key.to_os_string()),
        }
    }
    cmd.env_clear();
    for (key, value) in inherited {
        let allowed = key.to_str().is_some_and(|name| {
            scrubbed_env_allows(name) || also_allowed.iter().any(|n| n == name)
        });
        if allowed && !removed.contains(&key) {
            cmd.env(key, value);
        }
    }
    if let Some(home) = home {
        cmd.env(HOME_ENV_VAR, home);
    }
    for (key, value) in explicit {
        cmd.env(key, value);
    }
}
