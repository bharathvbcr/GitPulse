use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);
/// Ceiling for network-bound operations (`clone`, `fetch`, `push`, remote
/// `ls-remote`) where multi-gigabyte transfers are legitimate. Local plumbing
/// keeps [`DEFAULT_TIMEOUT`].
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

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
    std::fs::write(&dest, content).map_err(|e| format!("Failed to write file: {}", e))
}

/// True for inherited environment names that can redirect git's config,
/// transport, or credential resolution away from what the user picked.
fn is_injected_git_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("GIT_CONFIG_KEY_")
        || upper.starts_with("GIT_CONFIG_VALUE_")
        || upper.starts_with("GIT_CREDHELPER")
        || matches!(
            upper.as_str(),
            "GIT_CONFIG_COUNT" | "GIT_SSH_COMMAND" | "GIT_SSH_VARIANT" | "GIT_ASKPASS"
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
        // VALUE_n) injects arbitrary config without any of the names above.
        .env_remove("GIT_CONFIG_COUNT");
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
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

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

/// What [`run_bounded`] observed, before a caller shapes its own errors.
struct BoundedRun {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    success: bool,
    status_code: i32,
    /// True when stdout was cut off at [`MAX_OUTPUT_BYTES`].
    truncated: bool,
}

/// Shared engine behind `git_timeout` and `capture_command`: spawns `cmd`,
/// enforces `timeout`, and bounds stdout/stderr.
///
/// `label` names the process in spawn/timeout/wait errors (callers keep their
/// own wording for truncation). When `stdin_bytes` is set, the child gets a
/// piped stdin fed from a dedicated thread, so the deadline loop below stays
/// responsive even while megabytes are still being pushed into the child.
fn run_bounded(
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

    let stdout_handle = thread::spawn(move || drain_capped(stdout_pipe, MAX_OUTPUT_BYTES));
    let stderr_handle =
        thread::spawn(move || drain_capped(stderr_pipe, MAX_OUTPUT_BYTES.min(4 * 1024 * 1024)));

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
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!("{} timed out after {}s", label, timeout.as_secs()));
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
    let status = outcome?;
    let (stdout, truncated) = stdout_handle.join().unwrap_or_default();
    let (stderr, _) = stderr_handle.join().unwrap_or_default();
    Ok(BoundedRun {
        stdout,
        stderr,
        success: status.success(),
        status_code: status.code().unwrap_or(-1),
        truncated,
    })
}

fn git_timeout(
    repo: Option<&Path>,
    args: &[&str],
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let sub = args.first().unwrap_or(&"");
    let label = format!("git {}", sub);
    let cmd = git_command(repo, args);
    let out = run_bounded(cmd, &label, timeout, stdin_bytes)?;
    if out.truncated {
        return Err(format!(
            "git {} output exceeded {} MB",
            sub,
            MAX_OUTPUT_BYTES / (1024 * 1024)
        ));
    }
    if out.success {
        return Ok(out.stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !err.is_empty() {
        return Err(err);
    }
    // Some git failures report entirely on stdout — notably `commit`'s
    // "nothing added to commit" (exit 1, empty stderr). A bare status code
    // hides the one string callers match on to retry; surface the diagnosis.
    let stdout_text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !stdout_text.is_empty() {
        if stdout_text.chars().count() > MAX_FAILURE_MESSAGE_CHARS {
            let cut: String = stdout_text
                .chars()
                .take(MAX_FAILURE_MESSAGE_CHARS)
                .collect();
            return Err(format!("{cut}… (git {} output truncated)", sub));
        }
        return Err(stdout_text);
    }
    Err(format!(
        "git {} failed with status {}",
        sub, out.status_code
    ))
}

/// Upper bound on stdout text embedded in a failure message when git put its
/// diagnosis on stdout instead of stderr. Bounded so a chatty failure never
/// drags megabytes into an error string.
const MAX_FAILURE_MESSAGE_CHARS: usize = 2_000;

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
    let mut ahead = 0;
    let mut behind = 0;
    if let Some(rest) = track.split("ahead ").nth(1) {
        ahead = rest
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0);
    }
    if let Some(rest) = track.split("behind ").nth(1) {
        behind = rest
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0);
    }
    (ahead, behind)
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
    fn test_parse_ahead_behind() {
        assert_eq!(parse_ahead_behind("[ahead 3, behind 1]"), (3, 1));
        assert_eq!(parse_ahead_behind("[ahead 12]"), (12, 0));
        assert_eq!(parse_ahead_behind("[behind 4]"), (0, 4));
        assert_eq!(parse_ahead_behind("[gone]"), (0, 0));
        assert_eq!(parse_ahead_behind(""), (0, 0));
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
            "GIT_SSH_COMMAND",
            "GIT_SSH_VARIANT",
            "GIT_ASKPASS",
            "GIT_CREDHELPER",
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

    fn init_plain_git_repo(dir: &Path) {
        let output = std::process::Command::new("git")
            .arg("init")
            .current_dir(dir)
            .output()
            .expect("spawn git init");
        assert!(output.status.success());
    }
}
