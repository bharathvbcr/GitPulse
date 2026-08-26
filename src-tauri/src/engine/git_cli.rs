use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);
/// Ceiling for network-bound operations (`clone`, `fetch`, `push`, remote
/// `ls-remote`) where multi-gigabyte transfers are legitimate. Local plumbing
/// keeps [`DEFAULT_TIMEOUT`].
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Grace window [`run_bounded`] grants pipe EOF after a timeout kill before
/// it deliberately detaches any still-blocked drain threads.
const DRAIN_JOIN_GRACE: Duration = Duration::from_secs(2);

/// Canonical work tree or bare repository resolved from a user-supplied path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedRepo {
    pub path: String,
    pub name: String,
    pub is_bare: bool,
}

/// Validates that `repo_path` is an absolute, readable Git work tree or bare repository.
///
/// A work tree is accepted when `.git` exists as a directory or a gitfile (linked worktrees).
/// A bare repo is accepted when `HEAD` and `objects` exist, or when
/// `git rev-parse --is-bare-repository` returns true.
/// Always returns the canonical path.
pub fn validate_repo(repo_path: &str) -> Result<PathBuf, String> {
    if repo_path.is_empty() || repo_path.contains('\0') || repo_path.chars().any(|c| c.is_control())
    {
        return Err("Invalid repository path".into());
    }
    let path = Path::new(repo_path);
    if !path.is_absolute() {
        return Err("Repository path must be absolute".into());
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Cannot access path '{}': {}", repo_path, e))?;
    if !canonical.is_dir() {
        return Err(format!("Not a directory: {}", canonical.display()));
    }
    if is_git_repository(&canonical) {
        return Ok(canonical);
    }
    Err(format!("Not a Git repository: {}", canonical.display()))
}

fn is_git_repository(canonical: &Path) -> bool {
    if canonical.join(".git").exists() {
        return true;
    }
    has_bare_layout(canonical) || rev_parse_is_bare(canonical)
}

fn has_bare_layout(path: &Path) -> bool {
    path.join("HEAD").is_file() && path.join("objects").is_dir()
}

fn rev_parse_is_bare(path: &Path) -> bool {
    match git_text(path, &["rev-parse", "--is-bare-repository"]) {
        Ok(text) => text.trim().eq_ignore_ascii_case("true"),
        Err(_) => false,
    }
}

/// Resolves `git rev-parse --git-dir` to an absolute canonical git directory.
pub fn resolve_git_dir(repo: &Path) -> Result<PathBuf, String> {
    let raw = git_text(repo, &["rev-parse", "--git-dir"])?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("git rev-parse --git-dir returned an empty path".into());
    }
    let git_dir = Path::new(trimmed);
    let absolute = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        repo.join(git_dir)
    };
    absolute.canonicalize().map_err(|e| {
        format!(
            "Cannot resolve git directory '{}': {}",
            absolute.display(),
            e
        )
    })
}

/// Resolves `git rev-parse --git-common-dir` to the shared canonical git directory,
/// ensuring all linked worktrees in the same repository map to the same root git directory.
pub fn resolve_git_common_dir(repo: &Path) -> Result<PathBuf, String> {
    let raw = match git_text(repo, &["rev-parse", "--git-common-dir"]) {
        Ok(text) => text,
        Err(_) => git_text(repo, &["rev-parse", "--git-dir"])?,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("git rev-parse --git-common-dir returned an empty path".into());
    }
    let git_dir = Path::new(trimmed);
    let absolute = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        repo.join(git_dir)
    };
    absolute.canonicalize().map_err(|e| {
        format!(
            "Cannot resolve common git directory '{}': {}",
            absolute.display(),
            e
        )
    })
}

/// Canonicalizes `repo_path` and reports whether it is a bare repository.
pub fn resolve_repo(repo_path: &str) -> Result<ResolvedRepo, String> {
    let canonical = validate_repo(repo_path)?;
    let is_bare = rev_parse_is_bare(&canonical)
        || (!canonical.join(".git").exists() && has_bare_layout(&canonical));
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    Ok(ResolvedRepo {
        path: canonical.to_string_lossy().into_owned(),
        name,
        is_bare,
    })
}

/// Walks from `path` (file or directory) up to the nearest work tree that contains `.git`.
///
/// Used for Finder "Open With", Dock drops, and in-window folder drops, which often
/// hand us a nested file rather than the repository root.
pub fn find_git_root(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let start = if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        path.to_path_buf()
    };
    let mut current = start.canonicalize().ok()?;
    loop {
        if current.join(".git").exists() || has_bare_layout(&current) {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Resolves a relative path inside a repository, rejecting absolute paths and `..` escapes.
pub fn sandbox_join(repo: &Path, file_path: &str) -> Result<PathBuf, String> {
    if file_path.is_empty() || file_path.contains('\0') {
        return Err("Invalid file path".into());
    }
    let rel = Path::new(file_path);
    if rel.is_absolute() {
        return Err("File path must be relative to the repository".into());
    }
    for component in rel.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err("File path escapes the repository".into());
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    let joined = repo.join(rel);
    Ok(joined)
}

/// Like [`sandbox_join`], but safe against symlinks that point outside the repo.
///
/// `sandbox_join` only validates the path lexically, so a symlink placed inside
/// the repository would silently redirect reads and writes past the repo
/// boundary. This helper performs the same lexical validation, then canonicalizes
/// the repository root (TempDir paths may live behind `/var` -> `/private/var`)
/// and walks the relative path one component at a time: every EXISTING prefix
/// is resolved through `symlink_metadata` + `canonicalize` and re-checked
/// against the canonical repo prefix, so intermediate symlinks — including
/// dangling ones whose target lies outside — are caught before any filesystem
/// use. Non-existent trailing components are appended lexically, which keeps
/// "create new nested directories" flows working unchanged.
pub fn sandbox_join_canonical(repo: &Path, file_path: &str) -> Result<PathBuf, String> {
    // Lexical validation (absolute/..//NUL rejection) is owned by sandbox_join.
    let _validated = sandbox_join(repo, file_path)?;
    let repo_canonical = repo
        .canonicalize()
        .map_err(|e| format!("Cannot resolve repository path '{}': {}", repo.display(), e))?;
    let mut current = repo_canonical.clone();
    for component in Path::new(file_path).components() {
        let name = match component {
            Component::Normal(name) => name,
            // sandbox_join already accepted only CurDir besides Normal; a `.`
            // component is a semantic no-op on a canonical base.
            Component::CurDir => continue,
            _ => continue,
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(_) => {
                // The component exists (file, dir, or symlink): resolve it and
                // re-verify containment. A dangling symlink lands here too —
                // the link itself exists — and fails canonicalize below.
                let resolved = current
                    .canonicalize()
                    .map_err(|e| format!("Cannot resolve '{}': {}", current.display(), e))?;
                if !resolved.starts_with(&repo_canonical) {
                    return Err(format!(
                        "File path escapes the repository via symlink: {}",
                        file_path
                    ));
                }
                current = resolved;
            }
            Err(_) => {
                // Does not exist yet: remaining components stay purely lexical,
                // which is safe because `..`/absolute/NUL were already rejected.
            }
        }
    }
    Ok(current)
}

pub fn sandbox_write(repo_path: &str, file_path: &str, content: &str) -> Result<(), String> {
    let repo = validate_repo(repo_path)?;
    let dest = sandbox_join_canonical(&repo, file_path)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create parent directories: {}", e))?;
    }
    std::fs::write(&dest, content).map_err(|e| format!("Failed to write file: {}", e))?;
    // TOCTOU hardening (post-hoc containment re-check): sandbox_join_canonical
    // verified every existing component before the write, but a symlink
    // swapped in between that check and fs::write would redirect the write
    // outside the repository. Full prevention needs O_NOFOLLOW via libc,
    // which this crate deliberately does not depend on, so we instead
    // re-canonicalize the written file — resolving any final-component or
    // parent-directory swap — and fail loudly if it left the repo. Residual
    // race, documented honestly: a swap AFTER this check goes undetected, and
    // the escaped bytes are already on disk; the window is narrowed, not closed.
    let written = std::fs::canonicalize(&dest)
        .map_err(|e| format!("Cannot verify written file '{}': {}", dest.display(), e))?;
    if !written.starts_with(&repo) {
        return Err(format!(
            "Written file '{}' escaped the repository (containment re-check failed)",
            written.display()
        ));
    }
    Ok(())
}

/// True for inherited environment names that can redirect git's config,
/// transport, or credential resolution away from what the user picked.
///
/// `GIT_CONFIG_PARAMETERS` is the shell-quoted config channel (`'alias.st=!sh
/// -c …'` outranks even repo-local config), so it is injected by definition;
/// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` are listed here too so the
/// classification stays in lockstep with the explicit strip list in
/// [`git_command`] — anything stripped must classify as injected.
fn is_injected_git_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("GIT_CONFIG_KEY_")
        || upper.starts_with("GIT_CONFIG_VALUE_")
        || upper.starts_with("GIT_CREDHELPER")
        || matches!(
            upper.as_str(),
            "GIT_CONFIG_COUNT"
                | "GIT_CONFIG_PARAMETERS"
                | "GIT_CONFIG_GLOBAL"
                | "GIT_CONFIG_SYSTEM"
                | "GIT_SSH_COMMAND"
                | "GIT_SSH_VARIANT"
                | "GIT_ASKPASS"
                | "GIT_EXTERNAL_DIFF"
                | "GIT_SEQUENCE_EDITOR"
                | "GIT_EXEC_PATH"
        )
}

fn git_command(repo: Option<&Path>, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    // `core.quotepath=false` keeps non-ASCII paths as raw bytes in every
    // command's output (`status`, `diff --numstat`, `show`, ...). Without it,
    // porcelain text output arrives C-quoted ("\\346\\226\\207...") while `-z`
    // output emits raw bytes, so the same file matches under two different
    // spellings. Harmless for commands whose output has no paths at all.
    // Inherited CI-style environments can export GIT_* pointers that redirect
    // git's index, object database, alternates, common dir, namespace, or
    // config away from the repository the user actually picked. Strip them all
    // so a GUI-initiated git call always operates on the repo it was given.
    cmd.args(["-c", "core.quotepath=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_NAMESPACE")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        // The numbered-config channel (GIT_CONFIG_COUNT + GIT_CONFIG_KEY_n /
        // VALUE_n) and the shell-quoted GIT_CONFIG_PARAMETERS channel inject
        // arbitrary config without any of the names above — the latter
        // outranks even repo-local config, so an `alias.*=!sh -c …` planted
        // there would execute on the next GUI status call.
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_SEQUENCE_EDITOR")
        .env_remove("GIT_EXEC_PATH");
    for (name, _) in std::env::vars_os() {
        if is_injected_git_env(&name.to_string_lossy()) {
            cmd.env_remove(&name);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = repo {
        cmd.current_dir(dir);
    }
    cmd
}

/// Runs `git` in `repo` with a hard timeout and bounded stdout/stderr.
pub fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    git_timeout(Some(repo), args, DEFAULT_TIMEOUT, None)
}

pub fn git_text(repo: &Path, args: &[&str]) -> Result<String, String> {
    let bytes = git(repo, args)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Like [`git_text`], but an over-cap stream is data rather than an error:
/// returns what arrived plus the truncation flag. For callers whose input
/// tolerates a prefix (coverage family detection), where failing the whole
/// scan would be worse than reading part of the listing.
pub fn git_text_partial(repo: &Path, args: &[&str]) -> Result<(String, bool), String> {
    let (bytes, truncated) = git_run(Some(repo), args, DEFAULT_TIMEOUT, None)?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

pub fn git_global(args: &[&str]) -> Result<Vec<u8>, String> {
    git_timeout(None, args, DEFAULT_TIMEOUT, None)
}

pub fn git_with_stdin(repo: &Path, args: &[&str], stdin_bytes: &[u8]) -> Result<Vec<u8>, String> {
    git_timeout(Some(repo), args, DEFAULT_TIMEOUT, Some(stdin_bytes))
}

/// Runs `git` in `repo` with an explicit deadline instead of
/// [`DEFAULT_TIMEOUT`].
///
/// Use for network-bound work (`clone`, `fetch`, `push`) where a multi-gigabyte
/// transfer is legitimate and a short default cap would kill healthy traffic.
pub fn git_with_timeout(repo: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    git_timeout(Some(repo), args, timeout, None)
}

/// Like [`git_with_timeout`], but for repo-less global invocations.
pub fn git_global_with_timeout(args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    git_timeout(None, args, timeout, None)
}

/// [`git_with_timeout`] with UTF-8 (lossy) text output.
pub fn git_text_with_timeout(
    repo: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let bytes = git_with_timeout(repo, args, timeout)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Runs network-bound git plumbing in `repo` under [`NETWORK_TIMEOUT`].
pub fn git_text_network(repo: &Path, args: &[&str]) -> Result<String, String> {
    git_text_with_timeout(repo, args, NETWORK_TIMEOUT)
}

/// Captured stdout/stderr from a bounded external process.
///
/// Unlike `run_command`, a non-zero exit is not an error: tools such as
/// `npm audit` and `npm outdated` use the status code to mean "findings",
/// and the JSON the caller needs is still on stdout.
#[derive(Debug, Clone)]
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub status_code: i32,
}

impl CapturedOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Runs an external command with the same timeout and output caps as `git`.
pub fn run_command(program: &str, args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    run_command_in(program, args, timeout, None)
}

/// Like [`run_command`], with an optional working directory (used for `gh` in a repo).
pub fn run_command_in(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cwd: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let output = capture_command(program, args, cwd, timeout, &[])?;
    if output.success {
        return Ok(output.stdout);
    }
    let err = output.stderr_text();
    if err.is_empty() {
        return Err(format!(
            "{} failed with status {}",
            program, output.status_code
        ));
    }
    Err(err)
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
/// [`crate::harness::sidecar::resolve_binary`] with Go toolchain locations for
/// scanners that spawn `go` themselves; nonexistent entries are harmlessly skipped.
fn gui_launch_fallback_dirs(home: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = home {
        dirs.push(PathBuf::from(home).join(".local/bin"));
        // Standard GOBIN/GOPATH install locations (`go install ...` defaults).
        dirs.push(PathBuf::from("/usr/local/go/bin"));
        dirs.push(PathBuf::from(home).join("go/bin"));
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
/// - on Windows the bare name is tried before the `.exe`-suffixed one, and
///   never double-suffixed (`npm.cmd`, `gh.exe`).
fn find_in_dirs(program: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut names = vec![program.to_string()];
    if cfg!(windows) && !program.to_ascii_lowercase().ends_with(".exe") {
        names.push(format!("{program}.exe"));
    }
    dirs.iter().filter(|dir| dir.is_absolute()).find_map(|dir| {
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| is_executable_file(candidate))
    })
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
/// disallowed character); the caller then leaves the inherited PATH untouched
/// rather than degrading the child to an empty one.
fn extended_child_path(
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<std::ffi::OsString> {
    // Empty entries are dropped: POSIX reads one as "the current directory",
    // which would let whatever repo the user has open inject executables into
    // every spawned tool's lookup path.
    let mut entries: Vec<PathBuf> = path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|entry| !entry.as_os_str().is_empty())
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
    if program.contains('/') || program.contains('\\') {
        return program.to_string();
    }
    let mut dirs: Vec<PathBuf> = path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect();
    dirs.extend(gui_launch_fallback_dirs(home));
    find_in_dirs(program, &dirs)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
}

/// Shared lookup for subsystems that need to *find* an external tool without
/// spawning it (the harness sidecar's `manvi`, presence checks that want the
/// resolved path). Single owner of PATH + GUI-fallback resolution semantics;
/// bare names only — anything with a separator is not searched.
pub(crate) fn find_external_tool(program: &str) -> Option<String> {
    if program.is_empty() || program.contains('/') || program.contains('\\') {
        return None;
    }
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let mut dirs: Vec<PathBuf> = path_var
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect();
    dirs.extend(gui_launch_fallback_dirs(home.as_deref()));
    find_in_dirs(program, &dirs).map(|p| p.to_string_lossy().into_owned())
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
    let resolved = resolve_spawn_program_with(program, path_var, home);
    let mut cmd = Command::new(&resolved);
    // The same injected-GIT-* strip [`git_command`] applies: captured tools
    // shell out to git themselves (`gh pr checkout` fetches and merges in the
    // repo; npm/go/cargo read git config), and CI-style inherited pointers
    // (GIT_DIR, GIT_INDEX_FILE, GIT_CONFIG_PARAMETERS, ...) would redirect
    // them off the repository the caller picked. Applied before `extra_env`,
    // so a caller that explicitly passes one of these still wins.
    for (name, _) in std::env::vars_os() {
        if is_injected_git_env(&name.to_string_lossy()) {
            cmd.env_remove(&name);
        }
    }
    cmd.env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_NAMESPACE")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_SEQUENCE_EDITOR")
        .env_remove("GIT_EXEC_PATH")
        .env("GH_PROMPT_DISABLED", "1")
        // gh consults this before printing update notices; keep subprocess
        // output deterministic instead of interleaving a self-update banner.
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if !cfg!(windows) {
        // Applied before `extra_env` so an explicit caller-provided PATH wins.
        if let Some(child_path) = extended_child_path(path_var, home) {
            cmd.env("PATH", child_path);
        }
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd
}

/// Runs `program` with a hard timeout and bounded pipes.
///
/// `cwd` is optional. When set, it must already be a directory the caller has
/// judged (a validated repo, or a path `sandbox_join` produced). This helper
/// does not re-validate the path. Non-zero exits are returned, not raised.
pub fn capture_command(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
) -> Result<CapturedOutput, String> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    capture_command_with_env(
        program,
        args,
        cwd,
        timeout,
        extra_env,
        path_var.as_deref(),
        home.as_deref(),
    )
}

/// [`capture_command`] with injectable `PATH`/home lookups — the seam its
/// callers fill from the process environment by default. Tests use it to run
/// the exact production spawn path under a simulated GUI-minimal environment
/// without mutating process-global state.
pub(crate) fn capture_command_with_env(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Result<CapturedOutput, String> {
    let cmd = build_capture_command(program, args, cwd, extra_env, path_var, home);

    let out = run_bounded(cmd, program, timeout, None)?;
    if out.truncated {
        return Err(format!("{} output exceeded cap", program));
    }
    Ok(CapturedOutput {
        stdout: out.stdout,
        stderr: out.stderr,
        success: out.success,
        status_code: out.status_code,
    })
}

/// Tail of one byte stream as display text, cut on a char boundary so a
/// multibyte character split at the cap does not render as replacement
/// garbage. Shared by every surface that shows capped tool output.
pub fn byte_tail(bytes: &[u8], cap: usize) -> String {
    let start = bytes.len().saturating_sub(cap);
    let tail = &bytes[start..];
    let boundary = tail
        .iter()
        .position(|b| (*b & 0xC0) != 0x80)
        .unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[boundary..]).into_owned()
}

/// A finished run (any exit code) with capped per-stream tails.
#[derive(Debug)]
pub struct CapturedRun {
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub success: bool,
    pub status_code: i32,
    /// True when output hit [`MAX_OUTPUT_BYTES`] and was cut — the tails are
    /// what survived, and the flag says so rather than implying completeness.
    pub truncated: bool,
}

/// Outcome of [`run_captured`]. A timeout is an outcome, not an error: the
/// terminal surfaces "killed at N s" next to normal exits, and collapsing it
/// into a spawn-style error string would make the two indistinguishable.
#[derive(Debug)]
pub enum RunOutcome {
    Finished(CapturedRun),
    TimedOut(Duration),
}

/// Like [`capture_command`], but truncation keeps capped tails instead of
/// erroring away everything, and the timeout is reported as an outcome.
///
/// `tail_cap` bounds each returned tail in bytes; pass [`MAX_OUTPUT_BYTES`]
/// to keep everything up to the drain cap. Built for callers that show raw
/// tool output to a user (the terminal); `capture_command` stays the right
/// shape for JSON-scraping scanners.
pub fn run_captured(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
    tail_cap: usize,
) -> Result<RunOutcome, String> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let cmd = build_capture_command(
        program,
        args,
        cwd,
        extra_env,
        path_var.as_deref(),
        home.as_deref(),
    );
    // Spawn/wait failures stay errors. The timeout is detected from the error
    // `run_bounded` formats for exactly that case (see TIMEOUT_MARKER); the
    // regression test below pins this contract so a rewording cannot silently
    // turn timeouts into spawn errors again.
    match run_bounded(cmd, program, timeout, None) {
        Ok(out) => Ok(RunOutcome::Finished(CapturedRun {
            stdout_tail: byte_tail(&out.stdout, tail_cap),
            stderr_tail: byte_tail(&out.stderr, tail_cap),
            success: out.success,
            status_code: out.status_code,
            truncated: out.truncated || out.stdout.len() > tail_cap || out.stderr.len() > tail_cap,
        })),
        Err(e) if is_timeout_error(program, &e) => Ok(RunOutcome::TimedOut(timeout)),
        Err(e) => Err(e),
    }
}

/// The exact phrase [`run_bounded`] embeds when a deadline kills a child and
/// the only marker [`run_captured`] matches to classify a timeout. Formatter
/// and matcher both go through this constant, so the two sides of the
/// stringly-typed seam cannot drift apart silently; the regression test
/// additionally pins the whole behavior end to end.
const TIMEOUT_MARKER: &str = " timed out after ";

fn is_timeout_error(program: &str, err: &str) -> bool {
    err.starts_with(program) && err.contains(TIMEOUT_MARKER)
}

/// What [`run_bounded`] observed, before a caller shapes its own errors.
#[derive(Debug)]
pub(crate) struct BoundedRun {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub status_code: i32,
    /// True when stdout was cut off at [`MAX_OUTPUT_BYTES`].
    pub truncated: bool,
}

/// Shared engine behind `git_timeout` and `capture_command`: spawns `cmd`,
/// enforces `timeout`, and bounds stdout/stderr.
///
/// `label` names the process in spawn/timeout/wait errors (callers keep their
/// own wording for truncation). When `stdin_bytes` is set, the child gets a
/// piped stdin fed from a dedicated thread, so the deadline loop below stays
/// responsive even while megabytes are still being pushed into the child.
pub(crate) fn run_bounded(
    mut cmd: Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
) -> Result<BoundedRun, String> {
    if stdin_bytes.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn {}: {}", label, e))?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    // Drain results travel over channels rather than `JoinHandle::join()`:
    // join has no timeout, so a drain thread stuck behind an orphaned
    // grandchild holding a pipe write end would block this function forever.
    // [`collect_drained`] bounds that wait instead.
    let (stdout_tx, stdout_rx) = mpsc::channel();
    let (stderr_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = stdout_tx.send(drain_capped(stdout_pipe, MAX_OUTPUT_BYTES));
    });
    thread::spawn(move || {
        let _ = stderr_tx.send(drain_capped(
            stderr_pipe,
            MAX_OUTPUT_BYTES.min(4 * 1024 * 1024),
        ));
    });

    // Feed stdin from its own thread: a child that exits early (rejecting our
    // input) makes the write fail with EPIPE, which is not an error of ours —
    // the exit status decides. Dropping `stdin` at closure end is what sends
    // the child EOF.
    let stdin_handle = stdin_bytes.map(|bytes| {
        // The writer thread may outlive this stack frame. Own the bounded
        // payload rather than leaking a caller borrow into a `'static` task.
        let bytes = bytes.to_vec();
        let stdin = child.stdin.take();
        thread::spawn(move || {
            if let Some(mut stdin) = stdin {
                use std::io::Write;
                let _ = stdin.write_all(&bytes);
            }
        })
    });

    let start = Instant::now();
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    kill_process_tree(&mut child);
                    let _ = child.wait();
                    break Err(format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs()));
                }
                thread::sleep(Duration::from_millis(15));
            }
            Err(e) => break Err(format!("Failed to wait on {}: {}", label, e)),
        }
    };
    if let Some(handle) = stdin_handle {
        // After a kill the write end fails with EPIPE promptly; on a natural
        // exit the thread has already finished.
        let _ = handle.join();
    }
    match outcome {
        Ok(status) => {
            // Normal exit: EOF is imminent unless the child daemonized a
            // grandchild that inherited the pipes; then we take whatever was
            // buffered after the grace window instead of hanging forever.
            let (stdout, truncated) = collect_drained(&stdout_rx);
            let (stderr, _) = collect_drained(&stderr_rx);
            Ok(BoundedRun {
                stdout,
                stderr,
                success: status.success(),
                status_code: status.code().unwrap_or(-1),
                truncated,
            })
        }
        Err(e) => {
            // Timeout/wait failure: the pipes are dead weight now. Grant one
            // shared grace window for EOF (a tree-kill on Windows usually delivers
            // it), then detach any still-blocked drain threads — see
            // [`collect_drained`] for the documented residual leak.
            let deadline = Instant::now() + DRAIN_JOIN_GRACE;
            let _ = collect_drained_deadline(&stdout_rx, deadline);
            let _ = collect_drained_deadline(&stderr_rx, deadline);
            Err(e)
        }
    }
}

/// Collects a pipe-drain result, waiting at most [`DRAIN_JOIN_GRACE`] for EOF.
///
/// Residual leak, documented deliberately: when a timed-out command forked a
/// grandchild that inherited stdout/stderr, killing the direct child does not
/// close the pipe write ends and the drain thread stays blocked on read. This
/// crate has no libc dependency, so a portable Unix process-group kill
/// (`setsid`/`killpg` via `pre_exec`) is unavailable — Windows gets a real
/// tree kill through `taskkill /T`, but on Unix the orphan cannot be signalled
/// portably. The honest options were hanging forever or detaching; we detach.
/// The detached thread unblocks when the orphan exits (pipe EOF) or at app
/// shutdown, holds at most [`MAX_OUTPUT_BYTES`], and at most two exist per
/// timed-out command. When the grace expires, bytes read but not yet delivered
/// (no EOF seen) are discarded — acceptable because git and the other tools
/// routed through this engine do not fork pipe-holding descendants.
fn collect_drained(rx: &mpsc::Receiver<(Vec<u8>, bool)>) -> (Vec<u8>, bool) {
    rx.recv_timeout(DRAIN_JOIN_GRACE).unwrap_or_default()
}

fn collect_drained_deadline(
    rx: &mpsc::Receiver<(Vec<u8>, bool)>,
    deadline: Instant,
) -> (Vec<u8>, bool) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(remaining).unwrap_or_default()
}

/// Kills `child`, best-effort taking its whole process tree down on Windows.
fn kill_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        // `taskkill /T /F` walks the PID tree. Spawned directly by argv,
        // never through a shell, and best-effort only.
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// Detects transient git lock contention errors (e.g. background AI agents or terminal processes holding index.lock)
pub fn is_transient_git_lock_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    (lower.contains(".lock'") && lower.contains("file exists"))
        || lower.contains("another git process seems to be running")
        || (lower.contains("unable to create") && lower.contains(".lock"))
        || lower.contains("cannot lock ref")
        || (lower.contains("index.lock") && lower.contains("file exists"))
}

/// Backoff for attempts 1..=3: 50 + 150 + 300 ms = 500 ms, plus a tiny pid jitter
/// so concurrent waiters do not stampede the lock file together.
fn lock_retry_backoff_ms(attempt: usize) -> u64 {
    const STEPS: [u64; 3] = [50, 150, 300];
    let base = STEPS[(attempt.saturating_sub(1)).min(2)];
    base + (std::process::id() as u64 % 20)
}

/// Shared bounded-invocation loop behind [`git_timeout`] and
/// [`git_text_partial`]: spawns git with the lock-retry backoff and returns
/// its stdout plus whether stdout hit [`MAX_OUTPUT_BYTES`] and was cut.
/// A run that FAILED never flows through the data path — partial output
/// from a failed command is not trustworthy — so failures surface git's
/// own diagnosis (stderr first, then stdout, then the bare status) exactly
/// as before; only successful runs carry the truncation flag.
fn git_run(
    repo: Option<&Path>,
    args: &[&str],
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
) -> Result<(Vec<u8>, bool), String> {
    let sub = args.first().unwrap_or(&"");
    let label = format!("git {}", sub);
    let mut attempts = 0;
    const MAX_LOCK_RETRIES: usize = 3;

    loop {
        let cmd = git_command(repo, args);
        let out = run_bounded(cmd, &label, timeout, stdin_bytes)?;
        if out.success {
            return Ok((out.stdout, out.truncated));
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if !err.is_empty() {
            if attempts < MAX_LOCK_RETRIES && is_transient_git_lock_error(&err) {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(lock_retry_backoff_ms(attempts)));
                continue;
            }
            return Err(err);
        }
        // Some git failures report entirely on stdout — notably `commit`'s
        // "nothing added to commit" (exit 1, empty stderr). A bare status code
        // hides the one string callers match on to retry; surface the diagnosis.
        let stdout_text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !stdout_text.is_empty() {
            if attempts < MAX_LOCK_RETRIES && is_transient_git_lock_error(&stdout_text) {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(lock_retry_backoff_ms(attempts)));
                continue;
            }
            if stdout_text.len() > MAX_FAILURE_MESSAGE_BYTES {
                let cut = truncate_utf8_bytes(&stdout_text, MAX_FAILURE_MESSAGE_BYTES);
                return Err(format!("{cut}… (git {} output truncated)", sub));
            }
            return Err(stdout_text);
        }
        return Err(format!(
            "git {} failed with status {}",
            sub, out.status_code
        ));
    }
}

fn git_timeout(
    repo: Option<&Path>,
    args: &[&str],
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let sub = args.first().unwrap_or(&"");
    let (stdout, truncated) = git_run(repo, args, timeout, stdin_bytes)?;
    if truncated {
        return Err(format!(
            "git {} output exceeded {} MB",
            sub,
            MAX_OUTPUT_BYTES / (1024 * 1024)
        ));
    }
    Ok(stdout)
}

/// Upper bound on stdout text embedded in a failure message when git put its
/// diagnosis on stdout instead of stderr. Bounded so a chatty failure never
/// drags megabytes into an error string.
const MAX_FAILURE_MESSAGE_BYTES: usize = 2_000;

/// Truncates `s` to at most `max_bytes` on a UTF-8 character boundary.
fn truncate_utf8_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

fn drain_capped<R: Read>(pipe: Option<R>, max_bytes: usize) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let mut truncated = false;
    let Some(mut pipe) = pipe else {
        return (buf, false);
    };
    let mut tmp = [0u8; 16_384];
    loop {
        match pipe.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() < max_bytes {
                    let room = max_bytes - buf.len();
                    let take = n.min(room);
                    buf.extend_from_slice(&tmp[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (buf, truncated)
}

pub fn upstream_is_gone(track: &str) -> bool {
    track.contains("gone")
}

/// Parses `git rev-list --left-right --count A...B` (`behind\\tahead`).
pub fn parse_left_right_count(raw: &str) -> (usize, usize) {
    let line = raw.trim();
    let mut parts = line.split(['\t', ' ']).filter(|s| !s.is_empty());
    let left = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let right = parts.next().unwrap_or("0").parse().unwrap_or(0);
    (left, right)
}

pub fn parse_ahead_behind(track: &str) -> (usize, usize) {
    fn digits_after(haystack: &str, marker: &str) -> usize {
        let Some(idx) = haystack.find(marker) else {
            return 0;
        };
        let rest = &haystack.as_bytes()[idx + marker.len()..];
        let mut n = 0usize;
        for &b in rest {
            if b.is_ascii_digit() {
                n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
            } else {
                break;
            }
        }
        n
    }
    (
        digits_after(track, "ahead "),
        digits_after(track, "behind "),
    )
}

pub fn repo_name_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    trimmed
        .rsplit('/')
        .next()
        .and_then(|s| s.rsplit(':').next())
        .filter(|s| !s.is_empty())
        .unwrap_or("repo")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_rejects_parent_dir() {
        let repo = Path::new("/tmp/example-repo");
        assert!(sandbox_join(repo, "../secret").is_err());
        assert!(sandbox_join(repo, "/etc/passwd").is_err());
        assert!(sandbox_join(repo, "src/main.rs").is_ok());
    }

    #[test]
    fn transient_lock_errors_match_real_git_wording() {
        assert!(is_transient_git_lock_error(
            "fatal: Unable to create '/tmp/repo/.git/index.lock': File exists.\n\nAnother git process seems to be running in this repository"
        ));
        assert!(is_transient_git_lock_error(
            "fatal: cannot lock ref 'refs/heads/main': Unable to create '/tmp/repo/.git/refs/heads/main.lock': File exists."
        ));
        assert!(!is_transient_git_lock_error(
            "nothing to commit, working tree clean"
        ));
        assert!(!is_transient_git_lock_error(
            "pathspec 'ghost.txt' did not match any files"
        ));
        assert!(lock_retry_backoff_ms(1) >= 50);
        assert!(lock_retry_backoff_ms(2) >= 150);
        assert!(lock_retry_backoff_ms(3) >= 300);
    }

    #[test]
    fn test_parse_ahead_behind() {
        assert_eq!(parse_ahead_behind("[ahead 3, behind 1]"), (3, 1));
        assert_eq!(parse_ahead_behind("[ahead 12]"), (12, 0));
        assert_eq!(parse_ahead_behind("[behind 4]"), (0, 4));
        assert_eq!(parse_ahead_behind("[gone]"), (0, 0));
        assert_eq!(parse_ahead_behind(""), (0, 0));
        assert_eq!(parse_ahead_behind("[ahead 0, behind 0]"), (0, 0));
        assert_eq!(parse_ahead_behind("[ahead 1234, behind 99]"), (1234, 99));
        // Digits must be consumed without an intermediate String.
        assert_eq!(parse_ahead_behind("ahead 7 behind 8 extra"), (7, 8));
    }

    #[test]
    fn test_upstream_is_gone() {
        assert!(upstream_is_gone("[gone]"));
        assert!(upstream_is_gone("[ahead 1, gone]"));
        assert!(!upstream_is_gone("[ahead 3, behind 1]"));
        assert!(!upstream_is_gone(""));
    }

    #[test]
    fn test_parse_left_right_count() {
        assert_eq!(parse_left_right_count("2\t5"), (2, 5));
        assert_eq!(parse_left_right_count("0\t0\n"), (0, 0));
        assert_eq!(parse_left_right_count("12 3"), (12, 3));
        assert_eq!(parse_left_right_count(""), (0, 0));
    }

    #[test]
    fn test_repo_name_from_url() {
        assert_eq!(
            repo_name_from_url("https://github.com/acme/gitpulse.git"),
            "gitpulse"
        );
        assert_eq!(
            repo_name_from_url("git@github.com:acme/gitpulse.git"),
            "gitpulse"
        );
    }

    #[test]
    fn test_validate_repo_rejects_empty() {
        assert!(validate_repo("").is_err());
        assert!(validate_repo("relative/path").is_err());
        assert!(validate_repo("/tmp/foo\0bar").is_err());
        assert!(validate_repo("/tmp/foo\nbar").is_err());
        assert!(validate_repo("/definitely/missing-gitpulse-validate-repo").is_err());
    }

    #[test]
    fn run_captured_reports_a_finished_run_with_tails() {
        let outcome = run_captured(
            "git",
            &["--version"],
            None,
            DEFAULT_TIMEOUT,
            &[],
            MAX_OUTPUT_BYTES,
        )
        .unwrap();
        match outcome {
            RunOutcome::Finished(run) => {
                assert!(run.success);
                assert_eq!(run.status_code, 0);
                assert!(run.stdout_tail.contains("git version"));
                assert!(!run.truncated);
            }
            RunOutcome::TimedOut(_) => panic!("git --version must not time out"),
        }
    }

    /// Pins the timeout contract of [`run_captured`]: a deadline kill is an
    /// outcome (`TimedOut`), never laundered into a spawn-style error. If
    /// `run_bounded`'s message wording changes, this fails and the detector
    /// in `run_captured` has to be updated with it.
    #[cfg(unix)]
    #[test]
    fn run_captured_reports_timeout_as_an_outcome() {
        let outcome = run_captured(
            "sleep",
            &["5"],
            None,
            Duration::from_secs(1),
            &[],
            MAX_OUTPUT_BYTES,
        )
        .unwrap();
        assert!(
            matches!(outcome, RunOutcome::TimedOut(d) if d.as_secs() == 1),
            "expected TimedOut(1s), got {outcome:?}"
        );
    }

    #[test]
    fn byte_tail_keeps_the_end_and_stays_char_safe() {
        assert_eq!(byte_tail(b"hello world", 5), "world");
        // A multibyte sequence split at the cap must not leak replacement
        // characters at the head of the tail.
        let prefix = "é".repeat(5000); // 2 bytes each
        assert!(!byte_tail(prefix.as_bytes(), 9999).starts_with('\u{FFFD}'));
    }

    /// The formatter in `run_bounded` and the matcher behind `run_captured`
    /// share TIMEOUT_MARKER; this pins the round trip so neither side can
    /// drift without breaking here first, even on a platform where the
    /// end-to-end timeout test above does not run. Production always formats
    /// with `label == program`, and the starts_with guard rejects errors
    /// from any other program.
    #[test]
    fn timeout_marker_round_trips_between_formatter_and_matcher() {
        let formatted = format!("sleep{TIMEOUT_MARKER}3s");
        assert!(is_timeout_error("sleep", &formatted));
        // Same contract when the program is spelled as a path: the label is
        // then the full path too, so the prefix still matches.
        let path_formatted = format!("/usr/bin/sleep{TIMEOUT_MARKER}3s");
        assert!(is_timeout_error("/usr/bin/sleep", &path_formatted));
        // A spawn failure with unrelated text must not classify as a timeout…
        assert!(!is_timeout_error(
            "sleep",
            "Failed to spawn sleep: No such file"
        ));
        // …and another program's timeout must not match either.
        assert!(!is_timeout_error("git", &formatted));
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
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

    fn init_linked_worktree() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
        let main = init_test_repo(false);
        git_in(main.path(), &["commit", "--allow-empty", "-m", "init"]);
        let work_parent = tempfile::TempDir::new().unwrap();
        let work_path = work_parent.path().join("linked");
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(["worktree", "add", "-b", "gitpulse-link"])
            .arg(&work_path)
            .current_dir(main.path())
            .output()
            .expect("spawn git worktree");
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            work_path.join(".git").is_file(),
            "linked worktree must use a gitfile"
        );
        (main, work_parent, work_path)
    }

    fn init_test_repo(bare: bool) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let mut cmd = std::process::Command::new("git");
        cmd.arg("init");
        if bare {
            cmd.arg("--bare");
        } else {
            cmd.args(["-b", "main"]);
        }
        let output = cmd.current_dir(dir.path()).output().expect("spawn git");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        dir
    }

    #[test]
    fn test_validate_repo_accepts_normal_repo() {
        let dir = init_test_repo(false);
        let canonical = validate_repo(&dir.path().to_string_lossy()).expect("normal repo");
        assert!(canonical.is_absolute());
        assert!(canonical.join(".git").exists());
    }

    #[test]
    fn test_validate_repo_accepts_gitfile() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(".git"), "gitdir: /tmp/fake.git\n").unwrap();
        validate_repo(&dir.path().to_string_lossy()).expect("gitfile worktree");
    }

    #[test]
    fn test_validate_repo_rejects_file_and_incomplete_bare_layout() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "blob").unwrap();
        assert!(validate_repo(&file.to_string_lossy()).is_err());

        let head_only = tempfile::TempDir::new().unwrap();
        std::fs::write(head_only.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(
            validate_repo(&head_only.path().to_string_lossy()).is_err(),
            "HEAD without objects is not a bare repo"
        );

        let objects_only = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(objects_only.path().join("objects")).unwrap();
        assert!(
            validate_repo(&objects_only.path().to_string_lossy()).is_err(),
            "objects without HEAD is not a bare repo"
        );
    }

    #[test]
    fn test_resolve_repo_normal_is_not_bare() {
        let dir = init_test_repo(false);
        let resolved = resolve_repo(&dir.path().to_string_lossy()).expect("resolve");
        assert!(!resolved.is_bare);
        assert_eq!(
            resolved.path,
            dir.path().canonicalize().unwrap().to_string_lossy()
        );
        assert!(!resolved.name.is_empty());
    }

    #[test]
    fn test_validate_repo_accepts_bare_repo() {
        let dir = init_test_repo(true);
        let canonical = validate_repo(&dir.path().to_string_lossy()).expect("bare repo");
        assert!(canonical.join("HEAD").is_file());
        assert!(canonical.join("objects").is_dir());
        assert!(!canonical.join(".git").exists());
        let resolved = resolve_repo(&dir.path().to_string_lossy()).expect("resolve");
        assert!(resolved.is_bare);
        assert_eq!(resolved.path, canonical.to_string_lossy());
        assert!(!resolved.name.is_empty());
    }

    #[test]
    fn test_validate_repo_rejects_non_repo_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(validate_repo(&dir.path().to_string_lossy()).is_err());
    }

    #[test]
    fn test_resolve_git_dir_normal_repo() {
        let dir = init_test_repo(false);
        let canonical = validate_repo(&dir.path().to_string_lossy()).unwrap();
        let git_dir = resolve_git_dir(&canonical).expect("git dir");
        assert!(git_dir.ends_with(".git"));
        assert!(git_dir.is_dir());
    }

    #[test]
    fn test_resolve_git_dir_bare_repo() {
        let dir = init_test_repo(true);
        let canonical = validate_repo(&dir.path().to_string_lossy()).unwrap();
        let git_dir = resolve_git_dir(&canonical).expect("git dir");
        assert_eq!(git_dir, canonical);
    }

    #[test]
    fn test_gitfile_worktree_resolves() {
        let (_main, _work_parent, work_path) = init_linked_worktree();
        let raw = work_path.to_string_lossy().into_owned();
        let canonical = validate_repo(&raw).expect("validate gitfile worktree");
        assert!(canonical.join(".git").is_file());

        let resolved = resolve_repo(&raw).expect("resolve gitfile worktree");
        assert!(!resolved.is_bare);
        assert_eq!(resolved.path, canonical.to_string_lossy());
        assert_eq!(resolved.name, "linked");

        let git_dir = resolve_git_dir(&canonical).expect("gitfile git dir");
        assert!(git_dir.is_dir());
        assert_ne!(git_dir, canonical);
        let git_dir_str = git_dir.to_string_lossy();
        assert!(
            git_dir_str.contains("worktrees"),
            "gitfile worktree git-dir should be the worktrees entry, got {git_dir_str}"
        );

        let nested = canonical.join("src").join("lib.rs");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "fn main() {}").unwrap();
        let found = find_git_root(&nested).expect("nested file in gitfile worktree");
        assert_eq!(found, canonical);
    }

    #[test]
    fn test_find_git_root_from_nested_file() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("src").join("lib.rs");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "fn main() {}").unwrap();

        let found = find_git_root(&nested).expect("nested file should resolve to repo");
        assert_eq!(found, dir.path().canonicalize().unwrap());
        assert_eq!(
            find_git_root(dir.path()).unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_git_root_accepts_bare_repo() {
        let dir = init_test_repo(true);
        let found = find_git_root(dir.path()).expect("bare repo");
        assert_eq!(found, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_find_git_root_rejects_non_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "no git here").unwrap();
        assert!(find_git_root(dir.path()).is_none());
        assert!(find_git_root(&dir.path().join("readme.txt")).is_none());
        assert!(find_git_root(Path::new("")).is_none());
    }

    #[test]
    fn capture_command_keeps_stdout_on_nonzero() {
        let output = capture_command(
            "sh",
            &["-c", "echo findings; echo err >&2; exit 1"],
            None,
            Duration::from_secs(5),
            &[],
        )
        .expect("spawn sh");
        assert!(!output.success);
        assert_eq!(output.status_code, 1);
        assert!(output.stdout_text().contains("findings"));
        assert!(output.stderr_text().contains("err"));
    }

    /// Regression: the stdin payload must be fed from its own thread while
    /// stdout/stderr drain concurrently. A child that emits 256 KiB on stdout
    /// BEFORE reading stdin fills both 64 KiB pipe buffers, and a parent that
    /// writes its >128 KiB payload inline first would block in `write_all`
    /// forever — before the deadline loop ever runs. The watchdog here is the
    /// proof: pre-fix this call never returns and the channel times out.
    #[test]
    fn stdin_write_cannot_deadlock_behind_a_chatty_child() {
        let payload = vec![b'x'; 192 * 1024];
        let (tx, rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        thread::spawn(move || {
            // Writes 256 KiB to stdout first, then consumes all of stdin.
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "head -c 262144 /dev/zero; cat > /dev/null"]);
            let result = run_bounded(cmd, "sh", Duration::from_secs(60), Some(&payload));
            let _ = tx.send(result);
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(out)) => {
                assert_eq!(out.stdout.len(), 262_144);
                assert!(out.success);
            }
            Ok(other) => panic!("expected success, got: {:?}", other.map(|o| o.success)),
            Err(_) => panic!(
                "stdin write deadlocked behind child stdout; no result within 10s \
                 (write_all blocked before the deadline loop could start)"
            ),
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "bounded run should finish promptly once pipes drain"
        );
    }

    #[test]
    fn run_command_still_fails_on_nonzero() {
        let err = run_command(
            "sh",
            &["-c", "echo boom >&2; exit 2"],
            Duration::from_secs(5),
        )
        .expect_err("nonzero");
        assert!(err.contains("boom"));
    }

    /// Regression: a GUI launch (Finder/Dock/ScholarLM) hands the app a minimal
    /// PATH without `/opt/homebrew/bin`, so bare names like `gh` failed to
    /// resolve and every GitHub view reported the CLI as "not installed". With
    /// an empty PATH the resolver must fall back to the conventional install
    /// directories (`~/.local/bin` here, standing in for Homebrew's).
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_finds_tool_in_fallback_dir_when_path_is_empty() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("gitpulse-fake-tool");
        std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let resolved = resolve_spawn_program_with(
            "gitpulse-fake-tool",
            Some(std::ffi::OsStr::new("")),
            Some(home.path().as_os_str()),
        );
        assert_eq!(
            Path::new(&resolved),
            &tool,
            "bare name must resolve through the fallback dir on an empty PATH"
        );
    }

    /// PATH order wins: a name present on the inherited PATH must not be
    /// shadowed by a fallback-directory copy. Both candidates are executable
    /// so the strict `find_in_dirs` scan considers them at all.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_prefers_path_over_fallback_dirs() {
        let path_dir = tempfile::TempDir::new().unwrap();
        let home_dir = tempfile::TempDir::new().unwrap();
        for dir in [path_dir.path(), &home_dir.path().join(".local/bin")] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("gitpulse-shadowed"), "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(
                dir.join("gitpulse-shadowed"),
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
        }
        let resolved = resolve_spawn_program_with(
            "gitpulse-shadowed",
            Some(path_dir.path().as_os_str()),
            Some(home_dir.path().as_os_str()),
        );
        assert_eq!(
            Path::new(&resolved),
            path_dir.path().join("gitpulse-shadowed")
        );
    }

    /// A non-executable file must not win the lookup: `execvp` skips it and
    /// keeps searching, so an executable copy later in the search order has to
    /// be picked instead of failing the eventual spawn with PermissionDenied.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_skips_non_executable_candidate_for_later_match() {
        let first = tempfile::TempDir::new().unwrap();
        let second = tempfile::TempDir::new().unwrap();
        std::fs::write(first.path().join("gitpulse-exec-probe"), "data, not code").unwrap();
        let good = second.path().join("gitpulse-exec-probe");
        std::fs::write(&good, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&good, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let path_var = std::env::join_paths([first.path(), second.path()]).expect("join dirs");
        let resolved = resolve_spawn_program_with("gitpulse-exec-probe", Some(&path_var), None);
        assert_eq!(
            Path::new(&resolved),
            &good,
            "executable copy in a later dir must beat a non-executable earlier one"
        );
    }

    /// Empty PATH entries mean CWD (POSIX) and relative entries resolve
    /// against whatever cwd the child ends up with — both must be ignored by
    /// the resolver even when they really contain the tool.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_ignores_empty_and_relative_path_entries() {
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let rel = Cleanup(PathBuf::from("gitpulse-rel-probe-dir"));
        std::fs::create_dir_all(&rel.0).unwrap();
        std::fs::write(rel.0.join("gitpulse-rel-probe-tool"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            rel.0.join("gitpulse-rel-probe-tool"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        for hostile in ["", "::", "gitpulse-rel-probe-dir"] {
            let resolved = resolve_spawn_program_with(
                "gitpulse-rel-probe-tool",
                Some(std::ffi::OsStr::new(hostile)),
                None,
            );
            assert_eq!(
                resolved, "gitpulse-rel-probe-tool",
                "entry {hostile:?} must not resolve a relative candidate"
            );
        }
    }

    /// Broken symlinks and directories that merely share the tool's name are
    /// not spawnpable candidates.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_skips_broken_symlink_and_directory_named_like_tool() {
        let dir = tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink(
            "/definitely/does/not/exist",
            dir.path().join("gitpulse-dangling"),
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("gitpulse-dirname")).unwrap();

        for name in ["gitpulse-dangling", "gitpulse-dirname"] {
            let resolved = resolve_spawn_program_with(name, Some(dir.path().as_os_str()), None);
            assert_eq!(
                resolved, name,
                "{name} must be skipped rather than selected as a candidate"
            );
        }
    }

    /// The child PATH never carries CWD-searching empty entries, and a fully
    /// absent PATH is seeded with the Unix default set instead of leaving
    /// children with only Homebrew/fallback dirs.
    #[cfg(unix)]
    #[test]
    fn extended_child_path_drops_cwd_entries_and_seeds_default_when_unset() {
        let home = tempfile::TempDir::new().unwrap();
        let colon_joined = extended_child_path(
            Some(std::ffi::OsStr::new("::")),
            Some(home.path().as_os_str()),
        )
        .expect("join");
        let entries: Vec<PathBuf> = std::env::split_paths(&colon_joined).collect();
        assert!(
            entries.iter().all(|e| !e.as_os_str().is_empty()),
            "empty CWD entries must be dropped: {entries:?}"
        );
        assert_eq!(
            entries.first(),
            Some(&PathBuf::from("/opt/homebrew/bin")),
            "with nothing inherited, fallbacks start immediately"
        );

        let unset = extended_child_path(None, Some(home.path().as_os_str())).expect("join");
        let seeded: Vec<PathBuf> = std::env::split_paths(&unset).collect();
        assert_eq!(
            &seeded[..2],
            [Path::new("/usr/bin"), Path::new("/bin")],
            "unset PATH must seed the Unix default search set"
        );
    }

    /// The extended child PATH appends only the fallback dirs that are not
    /// already present, preserving inherited order and precedence.
    #[cfg(unix)]
    #[test]
    fn extended_child_path_appends_missing_fallback_dirs_only() {
        let home = tempfile::TempDir::new().unwrap();
        let extended = extended_child_path(
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(home.path().as_os_str()),
        )
        .expect("join must succeed for plain entries");
        let entries: Vec<PathBuf> = std::env::split_paths(&extended).collect();
        assert_eq!(entries[0], PathBuf::from("/usr/bin"));
        assert_eq!(entries[1], PathBuf::from("/bin"));
        for fallback in [
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            home.path().join(".local/bin"),
            // Go toolchain locations so govulncheck's own `go` spawn resolves.
            PathBuf::from("/usr/local/go/bin"),
            home.path().join("go/bin"),
        ] {
            assert!(
                entries.contains(&fallback),
                "fallback dir must be appended: {} in {entries:?}",
                fallback.display()
            );
        }

        // Already-present entries are never duplicated.
        let deduped = extended_child_path(
            Some(std::ffi::OsStr::new("/opt/homebrew/bin")),
            Some(home.path().as_os_str()),
        )
        .expect("join must succeed");
        let count = std::env::split_paths(&deduped)
            .filter(|p| p == Path::new("/opt/homebrew/bin"))
            .count();
        assert_eq!(count, 1, "must not duplicate an existing entry");
    }

    /// An explicit caller-provided PATH (via `extra_env`) outranks the child
    /// PATH extension — the extension only fills an absent decision.
    #[test]
    fn extra_env_path_overrides_child_path_extension() {
        let home = tempfile::TempDir::new().unwrap();
        let cmd = build_capture_command(
            "sh",
            &[],
            None,
            &[("PATH", "/caller/chosen")],
            Some(std::ffi::OsStr::new("")),
            Some(home.path().as_os_str()),
        );
        let path_values: Vec<String> = cmd
            .get_envs()
            .filter(|(k, _)| *k == std::ffi::OsStr::new("PATH"))
            .filter_map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(path_values, vec!["/caller/chosen".to_string()]);
    }

    /// Regression (GUI launch + shebang scripts): resolving the top-level
    /// program through the fallback dirs is not enough. `/opt/homebrew/bin/npm`
    /// is a symlink to a `#!/usr/bin/env node` script; under a GUI-minimal
    /// inherited PATH the spawn's own `env` interpreter lookup fails (exit 127,
    /// "env: node: No such file or directory"), which reads downstream as
    /// "npm is not installed". The child must see the fallback dirs on its own
    /// PATH too.
    #[cfg(unix)]
    #[test]
    fn shebang_tool_in_fallback_dir_finds_interpreter_through_child_path() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let interpreter = bin.join("gitpulse-fake-interp");
        std::fs::write(&interpreter, "#!/bin/sh\necho INTERP_OK\n").unwrap();
        std::fs::set_permissions(
            &interpreter,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let tool = bin.join("gitpulse-fake-shebang-tool");
        std::fs::write(&tool, "#!/usr/bin/env gitpulse-fake-interp\n").unwrap();
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        // An empty inherited PATH stands in for a Finder/Dock launch: the tool
        // resolves via ~/.local/bin and its interpreter must resolve the same
        // way inside the child.
        let cmd = build_capture_command(
            "gitpulse-fake-shebang-tool",
            &[],
            None,
            &[],
            Some(std::ffi::OsStr::new("")),
            Some(home.path().as_os_str()),
        );
        let out = run_bounded(
            cmd,
            "gitpulse-fake-shebang-tool",
            Duration::from_secs(5),
            None,
        )
        .expect("spawn");
        assert!(
            out.success,
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("INTERP_OK"),
            "stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    /// Absolute paths pass through untouched, and a name found nowhere comes
    /// back unchanged so the spawn error keeps naming the requested tool.
    #[test]
    fn spawn_resolution_passes_through_separators_and_total_misses() {
        assert_eq!(
            resolve_spawn_program_with("/bin/sh", Some(std::ffi::OsStr::new("")), None),
            "/bin/sh"
        );
        assert_eq!(
            resolve_spawn_program_with(
                "gitpulse-no-such-tool",
                Some(std::ffi::OsStr::new("")),
                None
            ),
            "gitpulse-no-such-tool"
        );
    }

    /// The failure contract is preserved: an unresolvable program still fails
    /// `capture_command` with the original tool name in the message.
    #[test]
    fn capture_command_error_still_names_unresolvable_tool() {
        let err = capture_command(
            "gitpulse-no-such-tool-xyz",
            &[],
            None,
            Duration::from_secs(5),
            &[],
        )
        .expect_err("unresolvable tool must error");
        assert!(
            err.contains("gitpulse-no-such-tool-xyz"),
            "error must keep the bare tool name, got: {err}"
        );
    }

    #[test]
    fn git_command_strips_repo_redirect_env() {
        use std::collections::HashSet;
        let cmd = git_command(Some(Path::new("/tmp")), &["status"]);
        let removed: HashSet<String> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_CONFIG",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_EXTERNAL_DIFF",
            "GIT_SEQUENCE_EDITOR",
            "GIT_EXEC_PATH",
        ] {
            assert!(removed.contains(key), "git_command must remove {key}");
        }
    }

    /// Regression: without `-c core.quotepath=false`, `diff --numstat` quotes
    /// non-ASCII paths ("\\346...") while porcelain `-z` emits raw bytes, so
    /// the same file matches under two spellings. The config must ride every
    /// invocation assembled by `git_command`.
    #[test]
    fn git_command_disables_path_quoting() {
        let cmd = git_command(Some(Path::new("/tmp")), &["status"]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.len() >= 2 && args[0] == "-c" && args[1] == "core.quotepath=false",
            "expected leading -c core.quotepath=false args, got {args:?}"
        );
    }

    #[test]
    fn network_timeout_exceeds_default_and_is_30_minutes() {
        assert_eq!(NETWORK_TIMEOUT, Duration::from_secs(30 * 60));
        assert!(NETWORK_TIMEOUT > DEFAULT_TIMEOUT);
    }

    /// A call that outlives its deadline must fail with the bounded runner's
    /// timeout wording (the same engine `git_with_timeout` drives), not hang.
    #[test]
    fn short_timeout_kills_slow_command_with_timeout_error() {
        let started = Instant::now();
        let outcome = run_bounded(
            {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 30"]);
                cmd
            },
            "sh sleep 30",
            Duration::from_millis(300),
            None,
        );
        let Err(err) = outcome else {
            panic!("slow command must hit the deadline");
        };
        assert!(err.contains("timed out"), "got: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "kill must be prompt, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn capture_command_surfaces_timeout_wording() {
        let err = capture_command(
            "sh",
            &["-c", "sleep 30"],
            None,
            Duration::from_millis(250),
            &[],
        )
        .expect_err("must time out");
        assert!(err.contains("timed out after"), "got: {err}");
    }

    /// Regression (audit A4): the numbered-config channel injects arbitrary
    /// git config without any of the fixed names; SSH/askpass overrides can
    /// hijack transport and credentials. All must be classified as injected.
    #[test]
    fn numbered_config_and_transport_overrides_are_injected_env() {
        for name in [
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_KEY_17",
            "git_config_key_3",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_SSH_COMMAND",
            "GIT_SSH_VARIANT",
            "GIT_ASKPASS",
            "GIT_CREDHELPER",
            "GIT_EXTERNAL_DIFF",
            "GIT_SEQUENCE_EDITOR",
            "GIT_EXEC_PATH",
        ] {
            assert!(is_injected_git_env(name), "{name} must be stripped");
        }
        for safe in [
            "GIT_AUTHOR_NAME",
            "GIT_PAGER",
            "EDITOR",
            "PATH",
            "GIT_TRACE",
        ] {
            assert!(!is_injected_git_env(safe), "{safe} must survive");
        }
    }

    /// Regression (M4): GIT_CONFIG_PARAMETERS must be marked stripped on the
    /// spawned command. `env_remove` records the removal whether or not the
    /// parent environment carries the variable, so this needs no process-global
    /// mutation — which would race every other test that reads or spawns with
    /// the real environment in parallel.
    #[test]
    fn planted_git_config_parameters_is_removed_from_spawn_env() {
        let key = "GIT_CONFIG_PARAMETERS";
        let cmd = git_command(Some(Path::new("/tmp")), &["status"]);
        let removed = cmd
            .get_envs()
            .any(|(k, v)| k == std::ffi::OsStr::new(key) && v.is_none());
        assert!(
            removed,
            "planted GIT_CONFIG_PARAMETERS must be stripped from the spawned env"
        );
    }

    /// Captured external tools (`gh pr checkout` shells to git; npm/go/cargo
    /// read git config) inherit no injected-GIT-* redirection either, while an
    /// explicitly passed `extra_env` value still wins over the strip.
    #[test]
    fn capture_children_strip_injected_git_env_but_extra_env_wins() {
        let explicit_git_dir = "/explicit/caller-chosen";
        let cmd = build_capture_command(
            "gh",
            &["--version"],
            None,
            &[("GIT_DIR", explicit_git_dir)],
            None,
            None,
        );
        let value_of = |key: &str| -> Option<Option<std::ffi::OsString>> {
            cmd.get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(key))
                .map(|(_, v)| v.map(|v| v.to_os_string()))
        };
        for key in [
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_CONFIG",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_EXTERNAL_DIFF",
            "GIT_SEQUENCE_EDITOR",
            "GIT_EXEC_PATH",
        ] {
            assert_eq!(
                value_of(key),
                Some(None),
                "{key} must be stripped from captured-tool children"
            );
        }
        assert_eq!(
            value_of("GIT_DIR").and_then(|v| v.map(|s| s.to_string_lossy().into_owned())),
            Some(explicit_git_dir.to_string()),
            "caller-supplied extra_env must override the strip"
        );
        // gh-specific prompt suppression travels with every captured child.
        assert_eq!(
            value_of("GH_PROMPT_DISABLED").map(|v| v.is_some()),
            Some(true)
        );
    }

    /// Regression (M3): a child that exits successfully while a daemonized
    /// grandchild keeps the stdout/stderr write ends open used to block the
    /// unconditional drain joins forever. The run must return promptly with
    /// the child's exit status.
    #[cfg(unix)]
    #[test]
    fn run_bounded_success_does_not_hang_on_grandchild_holding_pipes() {
        let started = Instant::now();
        let mut cmd = Command::new("sh");
        // `sh` backgrounds `sleep 30` (which inherits both pipe write ends)
        // and then itself exits 0 immediately.
        cmd.args(["-c", "sleep 30 & exit 0"]);
        let out = run_bounded(cmd, "sh", Duration::from_secs(5), None).expect("run");
        assert!(out.success);
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "drain collection must be grace-bounded, took {:?}",
            started.elapsed()
        );
    }

    /// Regression (M3): a timed-out child whose backgrounded grandchild keeps
    /// the pipes open must still yield the timeout error within the grace
    /// window instead of leaking unbounded blocking work into the caller.
    #[cfg(unix)]
    #[test]
    fn run_bounded_timeout_returns_despite_grandchild_holding_pipes() {
        let started = Instant::now();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & sleep 30"]);
        let err = run_bounded(cmd, "sh", Duration::from_secs(1), None).expect_err("timeout");
        assert!(err.contains("timed out"), "got: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "timeout handling must stay bounded (grace is 2s per pipe), took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn sandbox_join_canonical_resolves_and_stays_inside() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "fn main() {}").unwrap();
        let repo = dir.path().canonicalize().unwrap();

        // Existing nested file resolves through to the canonical location.
        let resolved = sandbox_join_canonical(&repo, "src/main.rs").expect("existing nested file");
        assert_eq!(resolved, repo.join("src").join("main.rs"));

        // Non-existent trailing components stay lexical so new files can be
        // created in new nested directories.
        let fresh = sandbox_join_canonical(&repo, "deep/new/dir/file.txt").expect("new file");
        assert_eq!(
            fresh,
            repo.join("deep").join("new").join("dir").join("file.txt")
        );

        // Lexical escapes are still rejected before any filesystem work.
        assert!(sandbox_join_canonical(&repo, "../outside").is_err());
        assert!(sandbox_join_canonical(&repo, "/etc/passwd").is_err());
    }

    #[test]
    fn sandbox_join_canonical_refuses_symlink_escape() {
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

        let dir = tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), dir.path().join("leak"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("dir-leak")).unwrap();
        let repo = dir.path().canonicalize().unwrap();

        let err = sandbox_join_canonical(&repo, "leak").expect_err("file symlink escape");
        assert!(err.contains("escapes the repository"), "got: {err}");
        let err = sandbox_join_canonical(&repo, "dir-leak/payload.txt")
            .expect_err("directory symlink escape");
        assert!(err.contains("escapes the repository"), "got: {err}");

        // A dangling symlink pointing outside must also be refused, not passed
        // through as a merely "non-existent" trailing component.
        std::os::unix::fs::symlink(
            outside.path().join("does-not-exist.txt"),
            dir.path().join("dangling"),
        )
        .unwrap();
        assert!(sandbox_join_canonical(&repo, "dangling").is_err());
        // ...and a symlink that stays inside the repo is fine.
        std::fs::write(dir.path().join("inside.txt"), "ok").unwrap();
        std::os::unix::fs::symlink(dir.path().join("inside.txt"), dir.path().join("alias"))
            .unwrap();
        assert_eq!(
            sandbox_join_canonical(&repo, "alias").expect("internal symlink"),
            repo.join("inside.txt")
        );
    }

    #[test]
    fn sandbox_write_survives_non_canonical_repo_path() {
        // On macOS TempDir lives under /var -> /private/var; validate_repo
        // canonicalizes, and sandbox_join_canonical canonicalizes both sides
        // again, so prefix comparison never mixes the two spellings.
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let raw = dir.path().to_string_lossy().into_owned();
        sandbox_write(&raw, "notes/todo.md", "- item").expect("write via raw path");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes").join("todo.md")).unwrap(),
            "- item"
        );
    }

    /// Regression for the symlinked-directory escape: `create_dir_all` +
    /// `fs::write` used to follow a repo-internal symlink and land the file
    /// outside the repository. The canonicalizing join must refuse it while
    /// ordinary writes keep working.
    #[test]
    fn sandbox_write_refuses_symlinked_directory_escape() {
        let outside = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        let raw = dir.path().to_string_lossy().into_owned();
        let err = sandbox_write(&raw, "link/sub.txt", "escaped")
            .expect_err("write through a directory symlink must fail");
        assert!(err.contains("escapes"), "got: {err}");
        assert!(
            !outside.path().join("sub.txt").exists(),
            "nothing may be written through the link"
        );

        // A plain path still writes normally.
        sandbox_write(&raw, "real/sub.txt", "kept inside").expect("direct write");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("real").join("sub.txt")).unwrap(),
            "kept inside"
        );
    }

    #[test]
    fn truncate_utf8_bytes_cuts_on_a_character_boundary() {
        let s = "éééé"; // each é is 2 bytes
        assert_eq!(truncate_utf8_bytes(s, 100), s);
        assert_eq!(truncate_utf8_bytes(s, 3).len(), 2);
        assert!(truncate_utf8_bytes(s, 3).is_char_boundary(truncate_utf8_bytes(s, 3).len()));
        assert!(!truncate_utf8_bytes(s, 3).contains('\u{FFFD}'));
    }

    fn init_plain_git_repo(dir: &Path) {
        let output = std::process::Command::new("git")
            .arg("init")
            .current_dir(dir)
            .output()
            .expect("spawn git init");
        assert!(output.status.success());
    }
}
