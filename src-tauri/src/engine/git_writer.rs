use crate::engine::git_cli::{
    git_captured, git_global_observed, git_text, git_text_network, git_with_stdin,
    git_with_timeout, is_deferred_under_load, is_slot_wait_timeout, resolve_git_common_dir,
    sandbox_join, validate_repo, with_background_processes, BoundedRun, OutputStream,
    ProcessObserver, NETWORK_TIMEOUT,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) fn repo_mutation_lock(canon: &Path) -> Arc<Mutex<()>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let key = resolve_git_common_dir(canon).unwrap_or_else(|_| canon.to_path_buf());
    let mut map = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.len() >= 64 {
        map.retain(|_, arc| Arc::strong_count(arc) > 1);
    }
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Upper bound on commits replayed by one cherry-pick or revert call.
///
/// A replay is a sequence of merges; an unbounded list from a "select all"
/// click would park the repository in a sequencer thousands of steps deep with
/// no realistic way back out.
const MAX_REPLAY_COMMITS: usize = 200;

/// Validate selection bounds before planning policy checks or Git commands.
const MAX_INDEX_PATHS: usize = 20_000;
const MAX_INDEX_PATH_BYTES: usize = 4 * 1024 * 1024;
/// Leave room for Git's executable and quoting within Windows' command limit.
const MAX_INDEX_ARGV_BYTES: usize = 12 * 1024;
const MAX_INDEX_CHUNK_PATHS: usize = 128;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexAction {
    Stage,
    Unstage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StashSaveOptions {
    pub include_untracked: bool,
    pub keep_index: bool,
    /// Stash only these repository-relative paths. Empty stashes everything.
    /// Each is passed as a `:(literal)` pathspec, so `*` is a file name.
    pub paths: Vec<String>,
}

impl Default for StashSaveOptions {
    fn default() -> Self {
        Self {
            include_untracked: true,
            keep_index: false,
            paths: Vec::new(),
        }
    }
}

impl StashSaveOptions {
    /// The argv, program included, that the gate judges and the writer runs.
    pub fn argv(&self, message: Option<&str>) -> Vec<String> {
        let mut argv: Vec<String> = vec!["git".into(), "stash".into(), "push".into()];
        if self.include_untracked {
            argv.push("-u".into());
        }
        if self.keep_index {
            argv.push("--keep-index".into());
        }
        if let Some(message) = message {
            argv.extend(["-m".into(), message.into()]);
        }
        if !self.paths.is_empty() {
            argv.push("--".into());
            let mut seen = HashSet::new();
            argv.extend(
                self.paths
                    .iter()
                    .filter(|path| seen.insert(path.as_str()))
                    .map(|path| format!(":(literal){path}")),
            );
        }
        argv
    }

    /// Checks the selected paths stay inside `repo` and fit one command line.
    /// Stashing a selection runs as one `stash push`, so it cannot be split
    /// into chunks the way staging is.
    fn validate_paths(&self, repo: &Path) -> Result<(), String> {
        if self.paths.is_empty() {
            return Ok(());
        }
        let literal = literal_paths(repo, &self.paths)?;
        let bytes: usize = literal.iter().map(|path| path.len() + 1).sum();
        if bytes > MAX_INDEX_ARGV_BYTES {
            return Err(format!(
                "The {} selected paths are too long to stash in one command; \
                 stash fewer files or the whole working tree",
                literal.len()
            ));
        }
        Ok(())
    }
}

/// How [`GitWriter::merge_branch`] joins a branch into the checked-out one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeMode {
    /// Fast-forward when possible, otherwise a merge commit: git's default.
    #[default]
    Default,
    /// Only move the branch forward; refuse when a merge commit is needed.
    FfOnly,
    /// Always record a merge commit, even when a fast-forward was possible.
    NoFf,
    /// Stage the branch's combined changes and commit them as one commit
    /// with no merge parent.
    Squash,
}

/// Options for [`GitWriter::clone_repo_with`]. The default is a plain clone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CloneOptions {
    /// Check out this branch (or tag) instead of the remote's default.
    pub branch: Option<String>,
    /// Shallow clone: only the most recent `depth` commits.
    pub depth: Option<u32>,
    /// Also clone and check out every submodule.
    pub recurse_submodules: bool,
}

/// Deepest history a shallow clone may ask for; deeper is a full clone.
pub const MAX_CLONE_DEPTH: u32 = 1_000_000;

impl CloneOptions {
    fn validate(&self) -> Result<(), String> {
        if let Some(branch) = &self.branch {
            validate_ref_name(branch)?;
        }
        if let Some(depth) = self.depth {
            if depth == 0 || depth > MAX_CLONE_DEPTH {
                return Err(format!(
                    "Clone depth must be between 1 and {MAX_CLONE_DEPTH}"
                ));
            }
        }
        Ok(())
    }

    /// `clone` and its options, up to and including the `--` before the URL.
    fn argv(&self) -> Vec<String> {
        let mut argv = vec!["clone".to_string(), "--progress".to_string()];
        if let Some(branch) = &self.branch {
            argv.extend(["--branch".to_string(), branch.clone()]);
        }
        if let Some(depth) = self.depth {
            argv.extend(["--depth".to_string(), depth.to_string()]);
        }
        if self.recurse_submodules {
            argv.push("--recurse-submodules".to_string());
        }
        argv.push("--".to_string());
        argv
    }
}

/// One step of a clone as git reports it: `Receiving objects` at 45%.
/// `percent` is `None` for a step git does not count (`Cloning into …`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneProgress {
    pub phase: String,
    pub percent: Option<u8>,
}

impl CloneProgress {
    /// Reads one line of git's progress (`LC_ALL=C`, so the phrases are
    /// stable). `None` for anything that is not a progress report.
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        let line = line.strip_prefix("remote:").map_or(line, str::trim);
        if line.is_empty()
            || ["fatal:", "error:", "warning:", "hint:"]
                .iter()
                .any(|prefix| line.starts_with(prefix))
        {
            return None;
        }
        let Some((phase, rest)) = line.split_once(':') else {
            return line.starts_with("Cloning into").then(|| Self {
                phase: "Cloning".into(),
                percent: None,
            });
        };
        let phase = phase.trim();
        if phase.is_empty()
            || phase.len() > 64
            || !phase.chars().all(|c| c.is_ascii_alphabetic() || c == ' ')
        {
            return None;
        }
        let percent = rest
            .split_once('%')
            .and_then(|(number, _)| number.trim().parse::<u8>().ok().filter(|n| *n <= 100));
        Some(Self {
            phase: phase.to_string(),
            percent,
        })
    }
}

/// Splits git's stderr into progress lines (git rewrites one line with `\r`)
/// and hands each change of phase or percentage to `sink`.
struct CloneProgressObserver<'a> {
    pending: Vec<u8>,
    last: Option<CloneProgress>,
    sink: &'a mut dyn FnMut(&CloneProgress),
}

impl CloneProgressObserver<'_> {
    /// A partial line longer than this is not progress; drop it.
    const MAX_PENDING: usize = 4096;
}

impl ProcessObserver for CloneProgressObserver<'_> {
    fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
        if !matches!(stream, OutputStream::Stderr) {
            return;
        }
        for &byte in bytes {
            if byte == b'\r' || byte == b'\n' {
                let line = String::from_utf8_lossy(&self.pending).into_owned();
                self.pending.clear();
                if let Some(progress) = CloneProgress::parse(&line) {
                    if self.last.as_ref() != Some(&progress) {
                        (self.sink)(&progress);
                        self.last = Some(progress);
                    }
                }
            } else if self.pending.len() < Self::MAX_PENDING {
                self.pending.push(byte);
            }
        }
    }
}

/// Git's diagnosis of a failed clone, without the progress lines that fill
/// its stderr: what is left is the `fatal:` and its context.
fn clone_failure(run: &BoundedRun) -> String {
    let stderr = String::from_utf8_lossy(&run.stderr);
    let lines: Vec<&str> = stderr
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !CloneProgress::parse(line)
                    .is_some_and(|p| p.percent.is_some() || line.ends_with(", done."))
        })
        .collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    if tail.is_empty() {
        format!("git clone failed with status {}", run.status_code)
    } else if tail.len() > 2000 {
        let mut cut = tail.len() - 2000;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        format!("…{}", &tail[cut..])
    } else {
        tail
    }
}

/// What [`GitWriter::auto_fetch`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum AutoFetchOutcome {
    Fetched,
    /// Nothing ran. Not a failure: the next tick tries again.
    Skipped {
        reason: String,
    },
}

/// The argv, program included, of an automatic fetch. No `--prune`: an
/// unattended run only adds remote-tracking refs, never removes one.
pub const AUTO_FETCH_ARGV: [&str; 4] = ["git", "fetch", "--all", "--quiet"];

/// An automatic fetch holds the repository's mutation lock while it runs,
/// so it gets a short deadline: a slow network must not keep a user's
/// commit waiting behind a fetch nobody asked for just now.
pub const AUTO_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// A squash merge that did not complete. The distinction matters to the
/// caller: before the commit nothing is staged; after it, the squash is.
#[derive(Debug)]
pub(crate) enum SquashFailure {
    /// The merge or the gate refused; the index is as it was.
    Merge(String),
    /// The changes are staged but the commit failed (a refusing hook, an
    /// identity problem). The message says how to finish or undo it.
    Commit(String),
}

impl SquashFailure {
    pub(crate) fn into_message(self) -> String {
        match self {
            SquashFailure::Merge(message) | SquashFailure::Commit(message) => message,
        }
    }
}

impl IndexAction {
    fn argv(self) -> Vec<String> {
        match self {
            Self::Stage => vec!["git".into(), "add".into(), "--".into()],
            // Path-limited reset only changes the index and also works on an
            // unborn branch. `restore --staged` requires an existing HEAD.
            Self::Unstage => vec!["git".into(), "reset".into(), "--quiet".into(), "--".into()],
        }
    }
}

fn literal_paths(repo: &Path, files: &[String]) -> Result<Vec<String>, String> {
    if files.is_empty() || files.len() > MAX_INDEX_PATHS {
        return Err(format!("Select between 1 and {MAX_INDEX_PATHS} paths"));
    }
    let mut bytes = 0usize;
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for file in files {
        sandbox_join(repo, file)?;
        if file.len() > 4096 {
            return Err("A selected path exceeds 4096 bytes".into());
        }
        bytes = bytes.saturating_add(file.len() + 16);
        if bytes > MAX_INDEX_PATH_BYTES {
            return Err("Selected paths exceed the 4 MiB request limit".into());
        }
        if seen.insert(file) {
            paths.push(format!(":(literal){file}"));
        }
    }
    Ok(paths)
}

/// What [`GitWriter::move_path`] decided to run, handed to its gate before
/// anything moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MovePlan {
    /// The source has tracked content: `git mv`, program name included.
    Git { argv: Vec<String> },
    /// Nothing at the source is tracked, so no git command applies; a
    /// filesystem rename, judged as a delete of `from` and a write of `to`.
    Untracked { from: String, to: String },
}

/// What [`GitWriter::delete_path`] removed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteOutcome {
    pub path: String,
    /// Index entries removed with `git rm` (restorable from HEAD).
    pub tracked_removed: usize,
    /// Untracked files removed with `git clean` (not recoverable from git).
    pub untracked_removed: usize,
    /// True when something is still on disk at `path`: ignored files or a
    /// nested repository, which this never deletes.
    pub left_behind: bool,
}

/// How much of the working state a reset discards.
///
/// An enum rather than a passthrough string: no caller can invent a mode, and
/// the write gate is handed a line whose destructiveness it can rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResetMode {
    /// Move the branch; keep the index and the working tree.
    Soft,
    /// Move the branch and reset the index; keep the working tree.
    Mixed,
    /// Move the branch and reset the index, keeping local changes that do not
    /// collide. Refuses rather than overwriting.
    Keep,
    /// Move the branch and overwrite the index AND the working tree.
    /// Uncommitted work is destroyed and is not recoverable from git.
    Hard,
}

impl ResetMode {
    pub fn flag(self) -> &'static str {
        match self {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Keep => "--keep",
            ResetMode::Hard => "--hard",
        }
    }

    /// True for the mode that destroys uncommitted work. The UI uses this to
    /// decide whether an extra confirmation is owed; it is derived here so the
    /// answer cannot drift from the flag it describes.
    pub fn discards_working_tree(self) -> bool {
        matches!(self, ResetMode::Hard)
    }
}

/// Where [`GitWriter::set_identity`] writes `user.name` / `user.email`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityScope {
    /// This repository's `.git/config` only.
    Repo,
    /// The user's global config, for every repository on this machine.
    Global,
}

impl IdentityScope {
    fn flag(self) -> &'static str {
        match self {
            IdentityScope::Repo => "--local",
            IdentityScope::Global => "--global",
        }
    }
}

/// The identity git would record a commit under, as far as configuration
/// and the environment say. `None` is "not set anywhere git reads".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl GitIdentity {
    pub fn is_complete(&self) -> bool {
        self.name.is_some() && self.email.is_some()
    }
}

/// Opening phrase of the error a commit returns when no identity is set.
/// The UI matches it to offer setting one; nothing else produces it.
pub const IDENTITY_MISSING: &str = "Git does not know who you are";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RebaseActionKind {
    Pick,
    Squash,
    Fixup,
    Drop,
    Reword(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebaseStep {
    pub commit_id: String,
    pub action: RebaseActionKind,
}

pub struct GitWriter;

impl GitWriter {
    pub fn stage_file(repo_path: &str, file_path: &str) -> Result<(), String> {
        Self::change_index_with(repo_path, &[file_path.into()], IndexAction::Stage, |_| {
            Ok(())
        })
        .map(|_| ())
    }

    pub fn unstage_file(repo_path: &str, file_path: &str) -> Result<(), String> {
        Self::change_index_with(repo_path, &[file_path.into()], IndexAction::Unstage, |_| {
            Ok(())
        })
        .map(|_| ())
    }

    /// One native request and lock for a selection. All paths and all policy
    /// verdicts are checked before the first write. Bounded argv chunks avoid
    /// platform command-length limits. A later Git failure names the completed
    /// prefix; it is never presented as an atomic rollback or as success.
    pub fn change_index_with<J, V>(
        repo_path: &str,
        files: &[String],
        action: IndexAction,
        mut judge: J,
    ) -> Result<(Vec<V>, usize), String>
    where
        J: FnMut(&[&str]) -> Result<V, String>,
    {
        let repo = validate_repo(repo_path)?;
        let paths = literal_paths(&repo, files)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prefix = action.argv();
        let mut plans = Vec::new();
        let mut argv = prefix.clone();
        let mut bytes = 128usize;
        for path in &paths {
            // At most two bytes of escaping/UTF-16 storage per UTF-8 byte.
            let cost = path.len() * 2 + 4;
            if argv.len() > prefix.len()
                && (bytes + cost > MAX_INDEX_ARGV_BYTES
                    || argv.len() - prefix.len() >= MAX_INDEX_CHUNK_PATHS)
            {
                plans.push(argv);
                argv = prefix.clone();
                bytes = 128;
            }
            bytes += cost;
            argv.push(path.clone());
        }
        plans.push(argv);
        let mut verdicts = Vec::with_capacity(plans.len());
        for plan in &plans {
            let args: Vec<&str> = plan.iter().map(String::as_str).collect();
            verdicts.push(judge(&args)?);
        }
        let mut completed = 0;
        for plan in &plans {
            let args: Vec<&str> = plan.iter().skip(1).map(String::as_str).collect();
            git_text(&repo, &args).map_err(|error| format!(
                "Index update stopped after {completed} of {} selected paths: {error}. Refresh to review the current index.", paths.len()
            ))?;
            completed += plan.len() - prefix.len();
        }
        Ok((verdicts, completed))
    }

    /// Moves or renames a file or directory, tracked or not, under one
    /// mutation-lock acquisition.
    ///
    /// The one rename path: the file tree and the doc-vault rename both come
    /// here. Whether git knows the source is decided *inside* the lock and
    /// handed to `gate` as a [`MovePlan`] before anything moves, so the line
    /// judged is the operation that runs. A source with tracked content moves
    /// with `git mv`, which stages the rename (a directory moves whole,
    /// untracked files inside it included); a purely untracked source has no
    /// git command and moves with a filesystem rename. `git mv` takes path
    /// arguments, not pathspecs, so `:(literal)` cannot be used here the way
    /// `git add` can.
    pub fn move_path(
        repo_path: &str,
        from: &str,
        to: &str,
        gate: impl FnOnce(&MovePlan) -> Result<(), String>,
    ) -> Result<MovePlan, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let from = from.trim_end_matches('/');
        let to = to.trim_end_matches('/');
        if from.is_empty() || to.is_empty() {
            return Err("Source and destination must name a path inside the repository".into());
        }
        if from == to {
            return Err("Source and destination are the same path".into());
        }
        if to.starts_with(&format!("{from}/")) {
            return Err(format!("Cannot move {from} into itself"));
        }
        let from_abs = crate::engine::git_cli::sandbox_join_canonical(&repo, from)?;
        let to_abs = crate::engine::git_cli::sandbox_join_canonical(&repo, to)?;
        if std::fs::symlink_metadata(&from_abs).is_err() {
            return Err(format!("{from} does not exist"));
        }
        if std::fs::symlink_metadata(&to_abs).is_ok() {
            return Err(format!("destination already exists: {to}"));
        }
        let plan = if Self::tracked_count(&repo, from)? > 0 {
            MovePlan::Git {
                argv: ["git", "mv", "--", from, to].map(String::from).to_vec(),
            }
        } else {
            MovePlan::Untracked {
                from: from.to_string(),
                to: to.to_string(),
            }
        };
        gate(&plan)?;
        if let Some(parent) = to_abs.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create destination parent: {e}"))?;
        }
        match &plan {
            MovePlan::Git { argv } => {
                let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
                git_text(&repo, &args)?;
            }
            MovePlan::Untracked { .. } => {
                std::fs::rename(&from_abs, &to_abs)
                    .map_err(|e| format!("Failed to move {from} to {to}: {e}"))?;
            }
        }
        Ok(plan)
    }

    /// Deletes a file or directory from the working tree, under one
    /// mutation-lock acquisition.
    ///
    /// Tracked content goes with `git rm -r`, which stages the deletion (so a
    /// commit can still restore it) and refuses — removing nothing — when a
    /// file has changes the index or HEAD does not hold. Untracked content
    /// goes with `git clean -f -d`, which leaves ignored files and nested
    /// repositories alone; whatever is left is reported, not swept with a
    /// recursive filesystem delete. Every command is handed to `gate` before
    /// the first one runs.
    pub fn delete_path(
        repo_path: &str,
        path: &str,
        gate: impl FnOnce(&[Vec<String>]) -> Result<(), String>,
    ) -> Result<DeleteOutcome, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = path.trim_end_matches('/');
        if path.is_empty() || path == "." {
            return Err("Refusing to delete the repository root".into());
        }
        let abs = crate::engine::git_cli::sandbox_join_canonical(&repo, path)?;
        if std::fs::symlink_metadata(&abs).is_err() {
            return Err(format!("{path} does not exist"));
        }
        let spec = literal_paths(&repo, &[path.to_string()])?.remove(0);
        let tracked = Self::tracked_count(&repo, path)?;
        let untracked = Self::untracked_count(&repo, &spec)?;
        let mut plan: Vec<Vec<String>> = Vec::new();
        if tracked > 0 {
            plan.push(
                ["git", "rm", "-r", "-q", "--", spec.as_str()]
                    .map(String::from)
                    .to_vec(),
            );
        }
        if untracked > 0 {
            plan.push(
                ["git", "clean", "-f", "-d", "-q", "--", spec.as_str()]
                    .map(String::from)
                    .to_vec(),
            );
        }
        if plan.is_empty() {
            return Err(format!(
                "{path} holds only ignored files or nested repositories; GitPulse does not delete those"
            ));
        }
        gate(&plan)?;
        for (index, argv) in plan.iter().enumerate() {
            let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
            git_text(&repo, &args).map_err(|error| {
                if index == 0 {
                    error
                } else {
                    format!("The tracked files were removed, but removing the untracked ones failed: {error}")
                }
            })?;
        }
        Ok(DeleteOutcome {
            path: path.to_string(),
            tracked_removed: tracked,
            untracked_removed: untracked,
            left_behind: std::fs::symlink_metadata(&abs).is_ok(),
        })
    }

    /// Index entries at or under `path` (a file, or a directory's contents).
    fn tracked_count(repo: &Path, path: &str) -> Result<usize, String> {
        let spec = literal_paths(repo, &[path.to_string()])?.remove(0);
        let listed = git_text(repo, &["ls-files", "-z", "--", spec.as_str()])?;
        Ok(listed.split('\0').filter(|entry| !entry.is_empty()).count())
    }

    /// Untracked, non-ignored files at or under `spec` — exactly what
    /// `git clean -f -d` (without `-x`) would remove.
    fn untracked_count(repo: &Path, spec: &str) -> Result<usize, String> {
        let listed = git_text(
            repo,
            &[
                "ls-files",
                "-z",
                "--others",
                "--exclude-standard",
                "--",
                spec,
            ],
        )?;
        Ok(listed.split('\0').filter(|entry| !entry.is_empty()).count())
    }

    /// True when `email` is an address that exists so a fixture can commit in a
    /// throwaway repository. Reserved DNS names (`.test`, `.invalid`,
    /// `example.com`) and the local fixture addresses this repo actually uses.
    ///
    /// Keep the rule in lockstep with `.githooks/pre-commit`. A checkout of
    /// this project refuses these on commit; a fixture repository does not,
    /// because it has no `.githooks/pre-commit`.
    pub(crate) fn is_fixture_author_email(email: &str) -> bool {
        let email = email.trim().to_ascii_lowercase();
        if email.is_empty() || email.len() > 320 {
            return false;
        }
        let Some((local, domain)) = email.rsplit_once('@') else {
            return false;
        };
        if local.is_empty() || domain.is_empty() {
            return false;
        }
        domain == "test"
            || domain.ends_with(".test")
            || domain == "invalid"
            || domain.ends_with(".invalid")
            || domain == "example"
            || domain.ends_with(".example")
            || domain == "example.com"
            || domain == "example.org"
            || domain == "example.net"
            || matches!(
                email.as_str(),
                "gitpulse@test.local" | "test@gitpulse.local" | "t@t" | "t@e.com" | "test@test.com"
            )
    }

    fn email_from_ident(ident: &str) -> Result<String, String> {
        let start = ident
            .rfind('<')
            .ok_or_else(|| format!("could not read an email from git identity: {ident}"))?;
        let rest = &ident[start + 1..];
        let end = rest
            .find('>')
            .ok_or_else(|| format!("could not read an email from git identity: {ident}"))?;
        let email = rest[..end].trim();
        if email.is_empty() || !email.contains('@') {
            return Err(format!(
                "could not read an email from git identity: {ident}"
            ));
        }
        Ok(email.to_string())
    }

    fn fixture_identity_refusal(role: &str, email: &str) -> String {
        format!(
            "refusing to record the {role} as <{email}>. \
That address is a test fixture. This checkout's git identity was overwritten with it, \
so the commit would be attributed to the fixture. \
Unset the local override with `git config --local --unset-all user.name` and \
`git config --local --unset-all user.email`, and unset GIT_AUTHOR_* / GIT_COMMITTER_* if they are set."
        )
    }

    fn refuse_email(role: &str, email: &str) -> Result<(), String> {
        if Self::is_fixture_author_email(email) {
            Err(Self::fixture_identity_refusal(role, email.trim()))
        } else {
            Ok(())
        }
    }

    /// This checkout ships `.githooks/pre-commit` specifically so a fixture
    /// identity cannot be recorded. Other repositories, including the temp
    /// repos the suite commits in, do not carry that file and are left alone.
    fn guards_fixture_identity(repo: &Path) -> bool {
        repo.join(".githooks/pre-commit").is_file()
    }

    fn refuse_fixture_identity(repo: &Path, amend: bool) -> Result<(), String> {
        if !Self::guards_fixture_identity(repo) {
            return Ok(());
        }
        // An amend keeps the existing author unless --reset-author is passed,
        // and this writer never passes it. `git var` outside the hook reports
        // the configured identity, not the author the amend will preserve.
        if amend {
            let existing = git_text(repo, &["log", "-1", "--format=%ae"])?;
            Self::refuse_email("author", existing.trim())?;
        }
        let author = git_text(repo, &["var", "GIT_AUTHOR_IDENT"])?;
        let committer = git_text(repo, &["var", "GIT_COMMITTER_IDENT"])?;
        Self::refuse_email("author", &Self::email_from_ident(author.trim())?)?;
        Self::refuse_email("committer", &Self::email_from_ident(committer.trim())?)?;
        Ok(())
    }

    /// Reads `name` from git's effective configuration: `Some` when set,
    /// `None` when git says it is unset (exit 1), an error when the read
    /// itself failed, so an unreadable config never passes as "unset".
    fn config_value(repo: &Path, name: &str) -> Result<Option<String>, String> {
        let run = git_captured(repo, &["config", "--get", name])?;
        match run.status_code {
            0 => {
                let value = String::from_utf8_lossy(&run.stdout).trim().to_string();
                Ok((!value.is_empty()).then_some(value))
            }
            1 => Ok(None),
            code => Err(format!(
                "Could not read {name} from git config (exit {code}): {}",
                String::from_utf8_lossy(&run.stderr).trim()
            )),
        }
    }

    /// The identity a commit in `repo` would be recorded under. The
    /// environment counts only when it covers author and committer both,
    /// which is what git needs to record a commit without configuration.
    pub fn identity(repo_path: &str) -> Result<GitIdentity, String> {
        let repo = validate_repo(repo_path)?;
        Self::identity_in(&repo)
    }

    fn identity_in(repo: &Path) -> Result<GitIdentity, String> {
        Self::identity_with(repo, &|name| std::env::var(name).ok())
    }

    /// [`Self::identity_in`] with the environment passed in: git's child
    /// inherits this process's `GIT_AUTHOR_*` / `GIT_COMMITTER_*` / `EMAIL`.
    fn identity_with(
        repo: &Path,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<GitIdentity, String> {
        let env = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());
        let env_pair =
            |author: &str, committer: &str| env(author).filter(|_| env(committer).is_some());
        let name = match Self::config_value(repo, "user.name")? {
            Some(name) => Some(name),
            None => env_pair("GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"),
        };
        let email = match Self::config_value(repo, "user.email")? {
            Some(email) => Some(email),
            None => env_pair("GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL").or_else(|| env("EMAIL")),
        };
        Ok(GitIdentity { name, email })
    }

    /// Refuses before git runs when no identity is configured. Git would
    /// otherwise either fail with its own multi-line advice or, on a machine
    /// with a resolvable hostname, quietly record `user@host.local`.
    fn require_identity(repo: &Path) -> Result<(), String> {
        let identity = Self::identity_in(repo)?;
        let missing: Vec<&str> = [
            ("user.name", identity.name.is_none()),
            ("user.email", identity.email.is_none()),
        ]
        .into_iter()
        .filter_map(|(key, absent)| absent.then_some(key))
        .collect();
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "{IDENTITY_MISSING}: {} {} not set. Set your name and email for this repository \
             or for every repository, then commit again.",
            missing.join(" and "),
            if missing.len() == 1 { "is" } else { "are" }
        ))
    }

    /// The argv pair [`Self::set_identity`] runs, program included. Shared
    /// with the command gate so the judged lines are the lines that run.
    pub fn identity_argv(scope: IdentityScope, name: &str, email: &str) -> [Vec<String>; 2] {
        let line = |key: &str, value: &str| {
            vec![
                "git".to_string(),
                "config".to_string(),
                scope.flag().to_string(),
                key.to_string(),
                value.to_string(),
            ]
        };
        [line("user.name", name), line("user.email", email)]
    }

    /// Validates an identity before it is written to any config file.
    pub fn validate_identity(name: &str, email: &str) -> Result<(), String> {
        let name = name.trim();
        let email = email.trim();
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("Enter a name of 1 to 256 printable characters".into());
        }
        if name.contains(['<', '>']) {
            return Err("A name cannot contain < or >".into());
        }
        let valid_email = email.len() <= 320
            && !email
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '<' || c == '>')
            && email
                .rsplit_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty());
        if !valid_email {
            return Err("Enter an email address such as you@example.org".into());
        }
        Ok(())
    }

    /// Writes `user.name` and `user.email` to the repository's or the user's
    /// global config. Values are trimmed; nothing is written unless both are
    /// valid.
    pub fn set_identity(
        repo_path: &str,
        name: &str,
        email: &str,
        scope: IdentityScope,
    ) -> Result<GitIdentity, String> {
        let repo = validate_repo(repo_path)?;
        Self::validate_identity(name, email)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for argv in Self::identity_argv(scope, name.trim(), email.trim()) {
            let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
            git_text(&repo, &args)?;
        }
        Self::identity_in(&repo)
    }

    pub fn commit(repo_path: &str, message: &str, amend: bool) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if message.trim().is_empty() && !(amend && message.is_empty()) {
            return Err("Commit message must not be empty".into());
        }
        Self::require_identity(&repo)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::commit_inner(&repo, message, amend)
    }

    fn commit_inner(repo: &Path, message: &str, amend: bool) -> Result<String, String> {
        Self::refuse_fixture_identity(repo, amend)?;
        let mut args = vec!["commit"];
        if amend && message.is_empty() {
            args.push("--amend");
            args.push("--no-edit");
        } else {
            args.push("-m");
            args.push(message);
            if amend {
                args.push("--amend");
            }
        }
        git_text(repo, &args)
    }

    /// The add argv [`GitWriter::quick_commit`] runs. The command layer must
    /// judge `git` plus these args, never a paraphrased `-A` / pathspec form.
    pub(crate) const QUICK_COMMIT_ADD_ARGV: &'static [&'static str] = &["add", "--all"];

    /// Stage every tracked change and untracked non-ignored file, then commit,
    /// under one mutation-lock acquisition.
    ///
    /// Interactive "quick commit" uses this rather than `stageAll` + `commit()`,
    /// which spans two lock acquisitions and can absorb a concurrent writer's
    /// index. Unmerged paths refuse before `add` so a conflicted tree is never
    /// silently marked resolved. Ignored untracked files stay untracked —
    /// `git add --all` does not force them.
    pub fn quick_commit(repo_path: &str, message: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if message.trim().is_empty() {
            return Err("Commit message must not be empty".into());
        }
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::quick_commit_inner(&repo, message)
    }

    fn quick_commit_inner(repo: &Path, message: &str) -> Result<String, String> {
        Self::require_identity(repo)?;
        Self::refuse_fixture_identity(repo, false)?;
        let unmerged = git_text(repo, &["ls-files", "--unmerged"])?;
        if !unmerged.trim().is_empty() {
            return Err("Resolve merge conflicts before committing.".into());
        }
        git_text(repo, Self::QUICK_COMMIT_ADD_ARGV)?;
        Self::commit_inner(repo, message, false)
    }

    /// Commit exactly `files`, preserving unrelated staged and unstaged work,
    /// under a single mutation-lock acquisition.
    ///
    /// `stage_file()` followed by `commit()` spans two lock acquisitions, so a
    /// concurrent writer can commit the shared index in between and absorb the
    /// staged bytes into its own commit. Programmatic callers (automation,
    /// agents, batch tooling) need stage+commit to be one indivisible
    /// mutation; this is that primitive. Interactive flows that intentionally
    /// commit whatever the user staged keep using `stage_file()` + `commit()`.
    pub fn commit_files(
        repo_path: &str,
        message: &str,
        files: &[String],
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if message.trim().is_empty() {
            return Err("Commit message must not be empty".into());
        }
        let paths = literal_paths(&repo, files)?;
        Self::require_identity(&repo)?;
        Self::refuse_fixture_identity(&repo, false)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::refuse_if_parked(&repo, "commit selected files")?;
        let mut input = Vec::new();
        for path in &paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        git_with_stdin(
            &repo,
            &["add", "--pathspec-from-file=-", "--pathspec-file-nul"],
            &input,
        )?;
        git_with_stdin(
            &repo,
            &[
                "commit",
                "--only",
                "-m",
                message,
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            &input,
        )
        .map(|output| String::from_utf8_lossy(&output).into_owned())
    }

    pub fn checkout_branch(repo_path: &str, branch_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        // Two strategies: `switch --guess` covers creating a local
        // branch from a remote twin (git >= 2.23), revision-only `checkout` covers
        // everything older. The former middle attempt (`checkout --guess`)
        // added a third executable spelling without covering any state the
        // other two miss — and every extra strategy is another argv the
        // policy gate must judge identically.
        let attempts: [&[&str]; 2] = [
            &["switch", "--guess", branch_name],
            &["checkout", branch_name, "--"],
        ];
        let mut first_err = None;
        for attempt in attempts {
            match git_text(&repo, attempt) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        Err(first_err.expect("at least one attempt recorded"))
    }

    pub fn create_branch(
        repo_path: &str,
        branch_name: &str,
        start_point: Option<&str>,
    ) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        let mut args = vec!["branch", branch_name];
        if let Some(sp) = start_point {
            // Start points are revisions, not refs-to-create: HEAD~1 and raw
            // oids are legal here. Reflog/peel grammar stays excluded by
            // validate_oid_or_revision on purpose.
            validate_oid_or_revision(sp)?;
            args.push(sp);
        }
        git_text(&repo, args.as_slice())?;
        Ok(())
    }

    /// Deletes a local branch after capturing its tip SHA.
    ///
    /// The tip is resolved **before** `git branch -d` / `-D` removes the ref,
    /// then written to the ledger as `before_ref` so either delete door
    /// (sidebar toast or ops cleanup) can restore with [`Self::create_branch`]
    /// after the in-memory Undo toast is gone. Returns that tip on success.
    pub fn delete_branch(
        repo_path: &str,
        branch_name: &str,
        force: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        if force {
            // `-d` leaves these to git's own safety nets; `-D` bypasses them,
            // so the checks must be re-done server side.
            if is_default_branch(&repo, branch_name) {
                return Err(format!(
                    "refusing to force-delete '{branch_name}': it resolves to the repository's default branch"
                ));
            }
            if is_checked_out_in_any_worktree(&repo, branch_name)? {
                return Err(format!(
                    "refusing to force-delete '{branch_name}': it is checked out in a linked worktree"
                ));
            }
        }
        // Capture before the ref disappears — after `-d`/`-D` there is nothing
        // to rev-parse, and the Undo toast is not a durable store.
        let tip = git_text(&repo, &["rev-parse", &format!("refs/heads/{branch_name}")])?
            .trim()
            .to_string();
        validate_oid(&tip)?;
        let flag = if force { "-D" } else { "-d" };
        git_text(&repo, &["branch", flag, branch_name])?;
        // Journal only after a successful delete so a refused `-d` never claims
        // the branch is gone. The tip itself was resolved before the ref left.
        record_deleted_branch_tip(repo_path, branch_name, &tip, force);
        Ok(tip)
    }

    pub fn rename_branch(repo_path: &str, old_name: &str, new_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(old_name)?;
        validate_ref_name(new_name)?;
        git_text(&repo, &["branch", "-m", old_name, new_name])?;
        Ok(())
    }

    pub fn apply_patch_to_index(repo_path: &str, patch_content: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        git_with_stdin(
            &repo,
            &["apply", "--cached", "--unidiff-zero", "--recount", "-"],
            patch_content.as_bytes(),
        )?;
        Ok(())
    }

    pub fn execute_rebase_sequence(
        repo_path: &str,
        onto_commit: &str,
        steps: &[RebaseStep],
    ) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_oid_or_revision(onto_commit)?;
        if steps.is_empty() {
            return Err("Rebase sequence is empty".into());
        }
        if let Some(first) = steps.first() {
            if matches!(
                first.action,
                RebaseActionKind::Squash | RebaseActionKind::Fixup
            ) {
                return Err(format!(
                    "Cannot '{}' commit {} without a previous commit to combine into",
                    match first.action {
                        RebaseActionKind::Squash => "squash",
                        RebaseActionKind::Fixup => "fixup",
                        _ => unreachable!(),
                    },
                    first.commit_id
                ));
            }
        }
        for step in steps {
            validate_oid_or_revision(&step.commit_id)?;
        }

        let dirty = git_text(&repo, &["status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(
                "Working tree has uncommitted changes; commit or stash before rebasing".into(),
            );
        }

        let original_head = git_text(&repo, &["rev-parse", "HEAD"])?.trim().to_string();
        // A step whose commit is not reachable from the HEAD being rebased
        // would transplant a foreign commit onto the new base; refuse before
        // any state is touched.
        for step in steps {
            let is_ancestor = git_text(
                &repo,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &step.commit_id,
                    &original_head,
                ],
            );
            if is_ancestor.is_err() {
                return Err(format!(
                    "Rebase step {} is not an ancestor of HEAD; refusing to transplant foreign commits",
                    step.commit_id
                ));
            }
        }
        let original_branch = git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Recovery must never swallow its own failure: the checkout outcome is
        // captured so a locked index or full disk cannot hide that HEAD was
        // left detached mid-rebase.
        let restore = |repo: &std::path::Path| -> Result<(), String> {
            if let Some(ref branch) = original_branch {
                git_text(repo, &["checkout", "-f", branch]).map(|_| ())
            } else {
                git_text(repo, &["checkout", "-f", &original_head]).map(|_| ())
            }
        };

        git_text(&repo, &["checkout", "--detach", onto_commit])?;

        let result = (|| -> Result<(), String> {
            for step in steps {
                match &step.action {
                    RebaseActionKind::Pick => {
                        git_text(&repo, &["cherry-pick", &step.commit_id]).map_err(|e| {
                            format!("Rebase step failed (pick {}): {}", step.commit_id, e)
                        })?;
                    }
                    RebaseActionKind::Squash => {
                        git_text(&repo, &["cherry-pick", "-n", &step.commit_id]).map_err(|e| {
                            format!("Rebase step failed (squash {}): {}", step.commit_id, e)
                        })?;
                        Self::commit_inner(&repo, "", true)?;
                    }
                    RebaseActionKind::Fixup => {
                        git_text(&repo, &["cherry-pick", "-n", &step.commit_id]).map_err(|e| {
                            format!("Rebase step failed (fixup {}): {}", step.commit_id, e)
                        })?;
                        git_text(&repo, &["commit", "--amend", "--no-edit"])?;
                    }
                    RebaseActionKind::Drop => {}
                    RebaseActionKind::Reword(new_msg) => {
                        git_text(&repo, &["cherry-pick", &step.commit_id]).map_err(|e| {
                            format!("Rebase step failed (reword {}): {}", step.commit_id, e)
                        })?;
                        // Amending with only the new subject would destroy the
                        // commit body; rewrite the subject line in place.
                        let original =
                            git_text(&repo, &["log", "-1", "--format=%B", &step.commit_id])?;
                        let message = reworded_message(&original, new_msg);
                        Self::commit_inner(&repo, &message, true)?;
                    }
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            // git_text yields stdout on success; normalize to () so the
            // composer only ever sees recovery outcomes.
            let abort_result = git_text(&repo, &["cherry-pick", "--abort"]).map(|_| ());
            if let Err(abort_err) = &abort_result {
                log::warn!(
                    target: "engine",
                    "rebase recovery: cherry-pick --abort failed in {}: {abort_err}",
                    repo.display()
                );
            }
            let restore_result = restore(&repo);
            if let Err(restore_err) = &restore_result {
                log::warn!(
                    target: "engine",
                    "rebase recovery: checkout -f restore failed in {}: {restore_err}",
                    repo.display()
                );
            }
            return Err(combine_step_failure_with_recovery(
                &e,
                Some(&abort_result),
                &restore_result,
                original_branch.as_deref(),
                &original_head,
            ));
        }

        if let Some(ref branch) = original_branch {
            if let Err(e) = git_text(&repo, &["branch", "-f", branch, "HEAD"]) {
                let restore_result = restore(&repo);
                if let Err(restore_err) = &restore_result {
                    log::warn!(
                        target: "engine",
                        "rebase recovery: checkout -f restore after failed 'branch -f' in {}: {restore_err}",
                        repo.display()
                    );
                }
                // No cherry-pick was mid-flight here, so abort_result is None.
                return Err(combine_step_failure_with_recovery(
                    &e,
                    None,
                    &restore_result,
                    original_branch.as_deref(),
                    &original_head,
                ));
            }
            // The rebase is durably applied once `branch -f` succeeded; only
            // the working-tree checkout remains. Retry once, and if it still
            // fails never claim total failure — name the true end-state so
            // the user knows the branch DID move.
            if let Err(checkout_err) = git_text(&repo, &["checkout", branch]) {
                if git_text(&repo, &["checkout", branch]).is_err() {
                    return Err(format!(
                        "rebase was applied to '{branch}', but working-tree checkout failed: \
                         {checkout_err}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// A fetch nobody clicked: from the opt-in timer. Runs as background
    /// work, so the spawn gate sheds it under load instead of queueing it
    /// ahead of the user's own commands, and skips rather than waits when
    /// another git operation holds the repository.
    pub fn auto_fetch(repo_path: &str) -> Result<AutoFetchOutcome, String> {
        let repo = validate_repo(repo_path)?;
        let skipped = |reason: &str| {
            Ok(AutoFetchOutcome::Skipped {
                reason: reason.to_string(),
            })
        };
        let repo_lock = repo_mutation_lock(&repo);
        let _guard = match repo_lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return skipped("another git operation is running in this repository")
            }
        };
        match with_background_processes(|| {
            git_with_timeout(&repo, &AUTO_FETCH_ARGV[1..], AUTO_FETCH_TIMEOUT)
        }) {
            Ok(_) => Ok(AutoFetchOutcome::Fetched),
            Err(error) if is_deferred_under_load(&error) || is_slot_wait_timeout(&error) => {
                skipped("the app is busy; deferred to the next interval")
            }
            Err(error) => Err(error),
        }
    }

    pub fn fetch(repo_path: &str, remote: Option<&str>) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(r) = remote {
            validate_ref_name(r)?;
            git_text_network(&repo, &["fetch", r])
        } else {
            git_text_network(&repo, &["fetch", "--all", "--prune"])
        }
    }

    /// Git's arguments (no program) for a pull. Shared by the gate and
    /// [`Self::pull`]. `rebase`: `Some(true)` rebases local commits onto the
    /// upstream, `Some(false)` merges, `None` leaves it to `pull.rebase`.
    /// A branch is only named after a remote; git reads a lone word as the
    /// remote, so a branch without one is dropped rather than misread.
    pub fn pull_argv<'a>(
        remote: Option<&'a str>,
        branch: Option<&'a str>,
        rebase: Option<bool>,
    ) -> Vec<&'a str> {
        let mut argv = vec!["pull"];
        match rebase {
            Some(true) => argv.push("--rebase"),
            Some(false) => argv.push("--no-rebase"),
            None => {}
        }
        if let Some(remote) = remote {
            argv.push(remote);
            argv.extend(branch);
        }
        argv
    }

    pub fn pull(
        repo_path: &str,
        remote: Option<&str>,
        branch: Option<&str>,
        rebase: Option<bool>,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        for name in remote.into_iter().chain(branch) {
            validate_ref_name(name)?;
        }
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        git_text_network(&repo, &Self::pull_argv(remote, branch, rebase))
    }

    /// `pull.rebase` as configured for `repo` (`true`, `false`, `merges`,
    /// `interactive`), or `None` when unset — git then merges.
    pub fn pull_rebase_config(repo_path: &str) -> Result<Option<String>, String> {
        let repo = validate_repo(repo_path)?;
        Self::config_value(&repo, "pull.rebase")
    }

    /// Arguments after `git` for a push. The command gate judges this exact
    /// argv; [`Self::push_planned`] executes it, so a missing upstream cannot
    /// be judged as a bare `git push` and then run as `git push -u …`.
    pub fn plan_push(
        repo_path: &str,
        remote: Option<&str>,
        branch: Option<&str>,
        force: bool,
    ) -> Result<Vec<String>, String> {
        let repo = validate_repo(repo_path)?;
        plan_push_argv(&repo, remote, branch, force)
    }

    pub fn push(
        repo_path: &str,
        remote: Option<&str>,
        branch: Option<&str>,
        force: bool,
    ) -> Result<String, String> {
        let args = Self::plan_push(repo_path, remote, branch, force)?;
        Self::push_planned(repo_path, &args)
    }

    /// Runs a previously planned `push` argv. Refuses anything other than
    /// `--force-with-lease`, `-u`, and validated ref names so a caller cannot
    /// smuggle `--force` past the lease-only contract.
    pub fn push_planned(repo_path: &str, args: &[String]) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if args.first().map(String::as_str) != Some("push") {
            return Err("internal: planned push argv must start with push".into());
        }
        for arg in args.iter().skip(1) {
            if arg == "--force-with-lease" || arg == "-u" {
                continue;
            }
            validate_ref_name(arg)?;
        }
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        git_text_network(&repo, &refs)
    }

    /// Pushes exactly one tag ref. Using a fully-qualified refspec avoids an
    /// ambiguous branch/tag name from publishing the wrong object.
    pub fn push_tag(repo_path: &str, remote: &str, tag: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(remote)?;
        validate_ref_name(tag)?;
        let refspec = format!("refs/tags/{tag}");
        git_text_network(&repo, &["push", remote, &refspec])
    }

    /// Git's arguments (no program) for the merge step of `mode`. Shared by
    /// the command gate and [`Self::merge_branch`], so the judged line is the
    /// line that runs. A squash is `merge --squash`, followed by the commit
    /// [`Self::squash_commit_argv`] describes when anything was staged.
    pub fn merge_argv(branch: &str, mode: MergeMode) -> Vec<&str> {
        match mode {
            MergeMode::Default => vec!["merge", "--no-edit", branch],
            MergeMode::FfOnly => vec!["merge", "--ff-only", "--no-edit", branch],
            MergeMode::NoFf => vec!["merge", "--no-ff", "--no-edit", branch],
            MergeMode::Squash => vec!["merge", "--squash", branch],
        }
    }

    /// The commit that records a squash of `branch`, message included.
    pub fn squash_commit_argv(branch: &str) -> [String; 3] {
        [
            "commit".into(),
            "-m".into(),
            format!("Merge branch '{branch}' (squashed)"),
        ]
    }

    /// Merges `branch_name` into the checked-out branch. `gate` is asked
    /// about each git command, as git's arguments, before it runs; a squash
    /// asks twice, for the merge and then for its commit.
    pub fn merge_branch(
        repo_path: &str,
        branch_name: &str,
        mode: MergeMode,
        gate: &mut dyn FnMut(&[&str]) -> Result<(), String>,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        validate_ref_name(branch_name)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if mode == MergeMode::Squash {
            return Self::squash_merge_locked(&repo, branch_name, gate)
                .map(|committed| {
                    if committed {
                        format!("Squashed {branch_name} into one commit")
                    } else {
                        format!("Nothing to squash: {branch_name} is already merged")
                    }
                })
                .map_err(SquashFailure::into_message);
        }
        let argv = Self::merge_argv(branch_name, mode);
        gate(&argv)?;
        git_text(&repo, &argv)
    }

    /// Squash-merges `branch` into the checkout at `repo` and commits it.
    /// `Ok(false)` when the merge staged nothing: the branch's changes are
    /// already there, which is not a failure. Caller MUST hold the repository
    /// mutation lock. The one squash path: worktree teardown uses it too.
    pub(crate) fn squash_merge_locked(
        repo: &Path,
        branch: &str,
        gate: &mut dyn FnMut(&[&str]) -> Result<(), String>,
    ) -> Result<bool, SquashFailure> {
        // Checked first: once the merge has staged the squash, a missing
        // identity would leave it stranded in the index.
        Self::require_identity(repo).map_err(SquashFailure::Merge)?;
        let merge = Self::merge_argv(branch, MergeMode::Squash);
        gate(&merge).map_err(SquashFailure::Merge)?;
        git_text(repo, &merge).map_err(SquashFailure::Merge)?;
        // `git merge` refuses to start over staged changes, so whatever is
        // staged now is the squash.
        let staged =
            git_text(repo, &["diff", "--cached", "--name-only"]).map_err(SquashFailure::Merge)?;
        if staged.trim().is_empty() {
            return Ok(false);
        }
        let commit = Self::squash_commit_argv(branch);
        let commit: Vec<&str> = commit.iter().map(String::as_str).collect();
        let staged_note = |e: String| {
            SquashFailure::Commit(format!(
                "The squashed changes of {branch} are staged but could not be committed: {e}. \
                 Commit the staged changes, or undo them with `git reset --merge`."
            ))
        };
        gate(&commit).map_err(staged_note)?;
        git_text(repo, &commit).map_err(staged_note)?;
        Ok(true)
    }

    /// Resolves the upstream for a restack of `branch` onto `onto`: where
    /// `branch` forked, NOT the new base itself. After the parent branch was
    /// rewritten, `onto..branch` still contains the stale pre-image commits
    /// and replaying them conflicts. --fork-point recovers the pre-rewrite
    /// fork from the reflog; plain merge-base covers reflog-less clones;
    /// unrelated histories fall back to `onto`.
    ///
    /// Caller MUST hold the repo mutation lock: the value returned here is
    /// frozen into the argv the harness gate judges AND the argv executed,
    /// closing the plan-vs-execute TOCTOU.
    ///
    /// `requested` is the caller's own record of where `branch` was cut — the
    /// tip its parent carried when the stack was last read. Cascading a stack
    /// needs it: the moment the parent has been rebased, `merge-base(parent,
    /// branch)` collapses back to the trunk, and replaying from there would
    /// re-apply the parent's own commits on top of the parent. A requested
    /// fork point that is not an ancestor of `branch` is refused rather than
    /// quietly swapped for the computed one — the caller planned one specific
    /// rewrite, and silently widening it is how a cascade becomes an accident.
    pub fn prepare_restack(
        repo_canon: &Path,
        branch: &str,
        onto: &str,
        requested: Option<&str>,
    ) -> Result<String, String> {
        validate_ref_name(branch)?;
        validate_ref_name(onto)?;
        if let Some(fork_point) = requested {
            validate_oid(fork_point)?;
            // `git_captured`, not `git_text`: a non-zero exit means "not an
            // ancestor", while a spawn failure means the question was never
            // asked. Flattening both into one Err would let a git that could
            // not run read exactly like a fork point that was checked and
            // rejected — and the caller would retry into the same wall.
            let run = git_captured(
                repo_canon,
                &["merge-base", "--is-ancestor", fork_point, branch],
            )?;
            if !run.success {
                return Err(format!(
                    "Fork point {} is not an ancestor of '{branch}', so the stack this \
                     restack was planned from is no longer the stack on disk. Reload and retry.",
                    &fork_point[..fork_point.len().min(12)]
                ));
            }
            return Ok(fork_point.to_string());
        }
        Ok([
            vec!["merge-base", "--fork-point", onto, branch],
            vec!["merge-base", onto, branch],
        ]
        .into_iter()
        .find_map(|argv| {
            git_text(repo_canon, &argv)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| onto.to_string()))
    }

    /// Executes a restack with full preflight and rollback semantics. Caller
    /// MUST hold the repo mutation lock.
    ///
    /// - Preflight refuses a dirty worktree and any in-progress operation
    ///   (merge, rebase, cherry-pick, revert, `am`, bisect) before any state
    ///   is touched, naming the one it actually found.
    /// - On failure the half-applied rebase is aborted and the user's
    ///   original checkout restored; the error says what was rolled back.
    /// - On success `git rebase <branch>` leaves `branch` checked out, which
    ///   would silently switch the user's working copy; the original branch
    ///   is restored, and a failed restore is reported as partial success —
    ///   never as total failure.
    pub fn execute_restack(
        repo_canon: &Path,
        branch: &str,
        onto: &str,
        upstream: &str,
    ) -> Result<String, String> {
        validate_ref_name(branch)?;
        validate_ref_name(onto)?;
        // Upstream is either the validated `onto` fallback or a git-emitted
        // OID from merge-base; anything else must never reach argv.
        if upstream != onto {
            validate_oid(upstream)?;
        }

        // In-progress detection runs BEFORE the dirty-tree check: a mid-rebase
        // worktree is by definition dirty, and "finish the other rebase" is
        // the actionable cause while "commit your changes" is its symptom.
        // Naming the actual operation matters: "finish the rebase" sends the
        // user hunting for a rebase that is really a parked cherry-pick, and
        // `git rebase --abort` does not clear one.
        if let Some(parked) = crate::engine::repo_op::detect(repo_canon)? {
            return Err(format!(
                "A {} is already in progress in this repository; \
                 finish or abort it before restacking",
                parked.kind.label()
            ));
        }
        let dirty = git_text(repo_canon, &["status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(
                "Working tree has uncommitted changes; commit or stash before restacking".into(),
            );
        }

        // symbolic-ref fails on detached HEAD; then there is no branch to restore.
        let original_head = git_text(repo_canon, &["symbolic-ref", "--quiet", "HEAD"])
            .ok()
            .map(|s| {
                s.trim()
                    .trim_start_matches("refs/heads/")
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty());

        let restore_checkout = |repo_canon: &Path, target: &str| -> Result<(), String> {
            if git_text(repo_canon, &["checkout", target]).is_ok() {
                return Ok(());
            }
            // One retry: index refresh races are the common transient cause.
            if git_text(repo_canon, &["checkout", target]).is_ok() {
                return Ok(());
            }
            Err(format!("checkout '{target}' failed"))
        };

        match git_text(repo_canon, &["rebase", "--onto", onto, upstream, branch]) {
            Ok(output) => {
                if original_head.as_deref() != Some(branch) {
                    if let Some(ref orig) = original_head {
                        if let Err(checkout_err) = restore_checkout(repo_canon, orig) {
                            return Err(format!(
                                "Restack succeeded: '{branch}' was rebased onto '{onto}', but \
                                 restoring your previous branch '{orig}' failed ({checkout_err}). \
                                 The repository is left on '{branch}'."
                            ));
                        }
                    }
                }
                Ok(output)
            }
            Err(rebase_err) => {
                // Abort the half-applied rebase, then put the user back on
                // their original branch (--abort lands on `branch`, not where
                // the user was). Neither cleanup failure hides the primary
                // error; the message names the true end-state instead.
                let _ = git_text(repo_canon, &["rebase", "--abort"]);
                let end_state = match &original_head {
                    Some(orig) if orig != branch => {
                        if restore_checkout(repo_canon, orig).is_err() {
                            format!("'{branch}' is checked out (restore of '{orig}' failed)")
                        } else {
                            format!("the repository is back on '{orig}'")
                        }
                    }
                    _ => format!("'{branch}' is checked out"),
                };
                Err(format!(
                    "Restack of '{branch}' onto '{onto}' failed and was rolled back ({end_state}): {}",
                    summarize_git_failure(&rebase_err)
                ))
            }
        }
    }

    /// Convenience wrapper used outside the gated command path (tests): takes
    /// the mutation lock, resolves the upstream, executes with preflight and
    /// rollback. The production command plans/judges/executes under ONE lock
    /// span via [`Self::prepare_restack`] + [`Self::execute_restack`] instead.
    pub fn restack(repo_path: &str, branch: &str, onto: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _guard = guard;
        let upstream = Self::prepare_restack(&repo, branch, onto, None)?;
        Self::execute_restack(&repo, branch, onto, &upstream)
    }

    pub fn create_tag(
        repo_path: &str,
        tag_name: &str,
        commit_id: Option<&str>,
        message: Option<&str>,
    ) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(tag_name)?;
        let mut args = vec!["tag"];
        if let Some(msg) = message {
            args.push("-a");
            args.push(tag_name);
            args.push("-m");
            args.push(msg);
        } else {
            args.push(tag_name);
        }
        if let Some(cid) = commit_id {
            validate_oid(cid)?;
            args.push(cid);
        }
        git_text(&repo, &args)?;
        Ok(())
    }

    pub fn delete_tag(repo_path: &str, tag_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(tag_name)?;
        git_text(&repo, &["tag", "-d", tag_name])?;
        Ok(())
    }

    /// Discards working-tree changes at `file_path`: `git restore` reverts
    /// tracked modifications, `git clean` removes untracked entries.
    ///
    /// Neither failure may read as success. A failed restore is an error —
    /// except when the path was an untracked file that `clean` then removed
    /// (restore cannot match untracked paths, so that pair of outcomes means
    /// the discard completed). A clean failure after a successful restore is
    /// also an error: the tree is half-discarded, and the caller must know.
    pub fn discard_changes(repo_path: &str, file_path: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dest = sandbox_join(&repo, file_path)?;
        // Existence before the fact is what separates "untracked file that
        // clean will remove" from a pathspec that never matched anything.
        let existed_before = std::fs::symlink_metadata(&dest).is_ok();
        // Same :(literal) convention as stage/unstage: glob metacharacters in
        // a user path (notably "*") must match exactly themselves, never
        // expand across the working tree.
        let spec = format!(":(literal){file_path}");
        let restore_result = git_text(&repo, &["restore", "--", &spec]);
        let clean_result = git_text(&repo, &["clean", "-f", "--", &spec]);
        match (restore_result, clean_result) {
            (Ok(_), Ok(_)) => Ok(()),
            (Ok(_), Err(e)) => Err(format!(
                "restored '{}' but cleaning untracked files failed: {}",
                file_path, e
            )),
            (Err(_restore_err), Ok(_)) if existed_before && !dest.exists() => {
                // Purely untracked path: restore could not match it (expected),
                // and clean removed it, so the requested end state was reached.
                Ok(())
            }
            (Err(restore_err), _) => Err(restore_err),
        }
    }

    pub fn stash_save(repo_path: &str, message: Option<&str>) -> Result<String, String> {
        Self::stash_save_with(repo_path, message, StashSaveOptions::default())
    }

    pub fn stash_save_with(
        repo_path: &str,
        message: Option<&str>,
        options: StashSaveOptions,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        options.validate_paths(&repo)?;
        Self::refuse_if_parked(&repo, "stash")?;
        let argv = options.argv(message);
        let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
        git_text(&repo, &args)
    }

    pub fn stash_pop(repo_path: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        git_text(&repo, &["stash", "pop"])
    }

    /// Refuses when the worktree is already parked mid-operation, naming it.
    ///
    /// Starting a cherry-pick on top of a parked rebase does not queue behind
    /// it — git refuses with a message about internals, or worse, the second
    /// operation's control files collide with the first's. Consulting the same
    /// detector the recovery banner uses means the refusal names the operation
    /// the user can actually see and abort.
    fn refuse_if_parked(repo_canon: &Path, verb: &str) -> Result<(), String> {
        if let Some(parked) = crate::engine::repo_op::detect(repo_canon)? {
            return Err(format!(
                "Cannot {verb} while a {} is in progress. Finish or abort it first.",
                parked.kind.label()
            ));
        }
        Ok(())
    }

    /// Renders the argv for a cherry-pick or revert of `commits`.
    ///
    /// Shared by the write gate and the executor so the judged line is the run
    /// line. `--` is not applicable (these take revisions, not paths), so the
    /// leading-dash rejection in `validate_oid_or_revision` is what keeps a
    /// commit-ish from turning into a flag.
    pub fn replay_argv<'a>(
        subcommand: &'a str,
        commits: &'a [String],
        no_commit: bool,
    ) -> Vec<&'a str> {
        let mut argv = vec!["git", subcommand];
        if no_commit {
            argv.push("--no-commit");
        } else {
            // Both verbs open an editor for the message by default. The editor
            // is pinned to `true` process-wide, but saying `--no-edit` here
            // makes the intent explicit in the line the gate judges.
            argv.push("--no-edit");
        }
        for commit in commits {
            argv.push(commit.as_str());
        }
        argv
    }

    /// Validates the commit list shared by cherry-pick and revert.
    ///
    /// An empty list would make git operate on `HEAD` implicitly for some
    /// verbs, which is never what a UI meant to send.
    fn validate_replay_commits(commits: &[String]) -> Result<(), String> {
        if commits.is_empty() {
            return Err("No commits were selected".into());
        }
        if commits.len() > MAX_REPLAY_COMMITS {
            return Err(format!(
                "Too many commits selected ({}); the limit is {MAX_REPLAY_COMMITS}",
                commits.len()
            ));
        }
        for commit in commits {
            validate_oid_or_revision(commit)?;
        }
        Ok(())
    }

    /// Replays `commits` onto the current branch.
    ///
    /// A conflict leaves the repository parked mid-cherry-pick, which is a
    /// legitimate outcome rather than a corruption: `repo_op` detects it and
    /// the banner offers continue/skip/abort. The error therefore reports the
    /// conflict without attempting a rollback that would discard the user's
    /// chance to resolve it.
    pub fn cherry_pick(
        repo_path: &str,
        commits: &[String],
        no_commit: bool,
    ) -> Result<String, String> {
        Self::replay(repo_path, "cherry-pick", commits, no_commit)
    }

    /// Records the inverse of `commits` as new commits. Same parked-on-conflict
    /// semantics as [`cherry_pick`].
    pub fn revert(repo_path: &str, commits: &[String], no_commit: bool) -> Result<String, String> {
        Self::replay(repo_path, "revert", commits, no_commit)
    }

    fn replay(
        repo_path: &str,
        subcommand: &str,
        commits: &[String],
        no_commit: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        Self::validate_replay_commits(commits)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::refuse_if_parked(&repo, subcommand)?;
        let argv = Self::replay_argv(subcommand, commits, no_commit);
        git_text(&repo, &argv[1..])
    }

    /// Renders the argv for a reset. Shared by the gate and the executor.
    pub fn reset_argv(mode: ResetMode, target: &str) -> Vec<&str> {
        vec!["git", "reset", mode.flag(), target]
    }

    /// Moves the current branch to `target`, discarding as much as `mode` says.
    ///
    /// `--hard` destroys uncommitted work irrecoverably, which is why the mode
    /// is an enum rather than a passthrough string: no caller can invent a
    /// fifth mode, and the write gate sees a rendered line it can rank by
    /// destructiveness.
    pub fn reset(repo_path: &str, mode: ResetMode, target: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        validate_oid_or_revision(target)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A reset mid-merge silently abandons the merge's state instead of
        // ending it; the user wants abort, and the banner offers it.
        Self::refuse_if_parked(&repo, "reset")?;
        let argv = Self::reset_argv(mode, target);
        git_text(&repo, &argv[1..])
    }

    pub fn clone_repo(url: &str, target_dir: &str) -> Result<String, String> {
        Self::clone_repo_with(url, target_dir, &CloneOptions::default(), &mut |_| {})
    }

    /// Clones with `options`, reporting each step of git's progress to
    /// `on_progress` as it happens.
    pub fn clone_repo_with(
        url: &str,
        target_dir: &str,
        options: &CloneOptions,
        on_progress: &mut dyn FnMut(&CloneProgress),
    ) -> Result<String, String> {
        validate_clone_url(url)?;
        options.validate()?;
        // Global Git runs from a neutral directory. Resolve local relative
        // sources first so that isolation does not reinterpret the user's URL.
        // Keep URL schemes and scp-like host:path syntax intact.
        let source_path = Path::new(url);
        let local_source = if source_path.is_relative()
            && (!url.contains(':')
                || matches!(
                    source_path.components().next(),
                    Some(
                        std::path::Component::CurDir
                            | std::path::Component::ParentDir
                            | std::path::Component::Prefix(_)
                    )
                )) {
            Some(
                super::git_cli::canonicalize_plain(source_path)
                    .map_err(|error| format!("Cannot resolve local clone source: {error}"))?,
            )
        } else {
            None
        };
        let url = match &local_source {
            Some(source) => source.to_str().ok_or("Local clone source is not UTF-8")?,
            None => url,
        };
        let requested = Path::new(target_dir);
        let dest = resolve_clone_destination(requested)?;
        let is_parent_directory = dest.is_dir();
        if dest.join(".git").exists() {
            return Err("Destination is already a Git repository".into());
        }
        let clone_path = if is_parent_directory {
            resolve_clone_destination(&dest.join(crate::engine::git_cli::repo_name_from_url(url)))?
        } else {
            dest
        };
        if clone_path.join(".git").exists() {
            return Err(format!("Already cloned at {}", clone_path.display()));
        }
        let clone_str = clone_path.to_string_lossy().into_owned();
        let staging = allocate_clone_staging(
            clone_path
                .parent()
                .ok_or("Clone destination has no parent")?,
        )?;
        let staging_text = staging.to_string_lossy().into_owned();
        let mut argv = options.argv();
        argv.extend([url.to_string(), staging_text]);
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut observer = CloneProgressObserver {
            pending: Vec::new(),
            last: None,
            sink: on_progress,
        };
        let result = git_global_observed(&args, NETWORK_TIMEOUT, &mut observer)
            .and_then(|run| {
                if run.success {
                    Ok(())
                } else {
                    Err(clone_failure(&run))
                }
            })
            .and_then(|_| {
                crate::fs_entry::rename_noreplace(&staging, &clone_path).map_err(|error| {
                    format!(
                        "Cannot publish clone at {} without replacing an existing entry: {error}",
                        clone_path.display()
                    )
                })
            });
        if let Err(clone_err) = result {
            // This attempt exclusively created staging. Never remove `.git`
            // at the requested destination: another client may own it now.
            let removal = match std::fs::remove_dir_all(&staging) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            };
            return Err(partial_clone_message(&clone_err, &staging, removal));
        }
        Ok(clone_str)
    }
}

fn allocate_clone_staging(parent: &Path) -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("Cannot allocate clone staging directory: {error}"))?
        .as_nanos();
    for _ in 0..32 {
        let next = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".gitpulse-clone-{}-{timestamp}-{next}",
            std::process::id()
        ));
        // `mode` is Unix-only; a `mut` builder is unused_mut on Windows
        // under clippy `-D warnings` (release/CI).
        let created = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700);
                builder.create(&path)
            }
            #[cfg(not(unix))]
            std::fs::DirBuilder::new().create(&path)
        };
        match created {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Cannot create clone staging directory: {error}")),
        }
    }
    Err("Could not allocate a unique clone staging directory after 32 attempts".into())
}

/// Composes the user-facing error for a clone that failed and left a partial
/// private staging directory behind, reporting the cleanup that happened.
///
/// Cleanup failure preserves both the primary error and the retained path.
/// Taking the removal result makes that failure branch independently testable.
fn partial_clone_message(clone_err: &str, leftover: &Path, removal: std::io::Result<()>) -> String {
    match removal {
        Ok(()) => format!(
            "clone failed ({clone_err}); removed this attempt's staging directory at {}",
            leftover.display()
        ),
        Err(rm_err) => format!(
            "clone failed ({clone_err}); this attempt's staging directory at {} could not be removed \
             ({rm_err}); it is retained for manual recovery",
            leftover.display()
        ),
    }
}

/// Composes the user-facing error for a failed rebase step together with any
/// recovery failures that occurred while rolling back.
///
/// The step error always leads verbatim. When `abort_result` is
/// `Some(Err(..))` (a cherry-pick was mid-flight and could not be aborted) or
/// `restore_result` is `Err(..)` (the checkout back to the original branch /
/// HEAD failed), an explicit clause names the failure and the true end-state,
/// so a locked index or full disk can never hide behind the primary error.
/// Pure: no git, fully unit-testable.
fn combine_step_failure_with_recovery(
    step_error: &str,
    abort_result: Option<&Result<(), String>>, // None when no cherry-pick was in progress
    restore_result: &Result<(), String>,
    original_branch: Option<&str>,
    original_head: &str,
) -> String {
    let mut message = step_error.to_string();
    if let Some(Err(abort_err)) = abort_result {
        message.push_str(&format!(
            "; additionally, `cherry-pick --abort` failed ({abort_err}) — \
             the repository may still be mid-cherry-pick"
        ));
    }
    if let Err(restore_err) = restore_result {
        let target = match original_branch {
            Some(branch) => branch.to_string(),
            None => format!("HEAD {original_head}"),
        };
        message.push_str(&format!(
            "; additionally, restoring {target} failed ({restore_err}) — \
             HEAD may remain detached at the rebase base"
        ));
    }
    message
}

/// Resolves a clone destination the way [`crate::engine::git_cli::sandbox_join_canonical`]
/// resolves in-repo paths: the parent is canonicalized (resolving every
/// existing symlinked prefix, including macOS `/var` → `/private/var`), an
/// existing final component must not be a symlink itself, and whatever exists
/// must stay under the canonical parent. The returned path is what git is told
/// to create, so a symlinked destination can no longer redirect a clone past
/// the directory the caller picked.
fn resolve_clone_destination(dest: &Path) -> Result<PathBuf, String> {
    if !dest.is_absolute() {
        return Err("Clone destination must be an absolute path".into());
    }
    let name = dest.file_name().ok_or_else(|| {
        format!(
            "Clone destination '{}' does not name a directory entry",
            dest.display()
        )
    })?;
    let parent = dest.parent().ok_or_else(|| {
        format!(
            "Clone destination '{}' has no parent directory",
            dest.display()
        )
    })?;
    let parent_canonical = crate::engine::git_cli::canonicalize_plain(parent).map_err(|e| {
        format!(
            "Cannot resolve clone destination parent '{}': {}",
            parent.display(),
            e
        )
    })?;
    match std::fs::symlink_metadata(dest) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "Clone destination '{}' is a symlink; refusing so the clone cannot land \
                     outside the chosen location",
                    dest.display()
                ));
            }
            let actual = crate::engine::git_cli::canonicalize_plain(dest).map_err(|e| {
                format!(
                    "Cannot resolve clone destination '{}': {}",
                    dest.display(),
                    e
                )
            })?;
            if !actual.starts_with(&parent_canonical) {
                return Err(format!(
                    "Clone destination '{}' escapes its parent via symlink",
                    actual.display()
                ));
            }
            Ok(actual)
        }
        // Not yet present: the remaining components stay purely lexical,
        // which is safe because `..`/relative forms were refused above and
        // the parent was resolved through every existing link.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(parent_canonical.join(name))
        }
        Err(error) => Err(format!(
            "Cannot inspect clone destination '{}': {error}",
            dest.display()
        )),
    }
}

/// Journals the tip SHA of a branch about to be deleted.
///
/// Gate rows authorise the request; this row is the restore record. `before_ref`
/// holds the tip so `create_branch(name, tip)` can put the ref back after the
/// in-memory Undo toast is gone. Uses [`crate::ledger::record`] so a ledger
/// outage never blocks the delete itself.
pub(crate) fn record_deleted_branch_tip(
    repo_path: &str,
    branch_name: &str,
    tip: &str,
    force: bool,
) {
    use crate::ledger::{ActorKind, Draft, Outcome};

    let flag = if force { "-D" } else { "-d" };
    let argv = ["git", "branch", flag, branch_name];
    let address = crate::ledger::bindings::repository_address(repo_path).ok();
    let ledger_repo = address
        .as_ref()
        .map(|address| address.anchor.clone())
        .unwrap_or_else(|| repo_path.to_string());
    let worktree_path = address
        .and_then(|address| (address.worktree != address.anchor).then_some(address.worktree));

    let _ = crate::ledger::record(Draft {
        repo_path: ledger_repo,
        worktree_path,
        actor_kind: Some(ActorKind::Human),
        action: crate::ledger::action_for_argv(&argv),
        object: Some(branch_name.to_string()),
        argv_json: serde_json::to_string(&argv).ok(),
        outcome: Some(Outcome::Ok),
        before_ref: Some(tip.to_string()),
        detail_json: Some(r#"{"phase":"mutate","kind":"branch.delete"}"#.to_string()),
        ..Default::default()
    });
}

/// True when `branch_name` is the branch the repository's HEAD resolves to by
/// default: the primary remote's HEAD branch first, then conventional
/// main/master/trunk/develop — the same resolution `list_branches` uses.
pub(crate) fn is_default_branch(repo: &Path, branch_name: &str) -> bool {
    default_branch_short_name(repo).is_some_and(|short| short == branch_name)
}

/// The short name [`is_default_branch`] compares against, for a caller that
/// checks several branches and should resolve it once rather than per branch.
pub(crate) fn default_branch_short_name(repo: &Path) -> Option<String> {
    let remote = crate::engine::git_reader::resolve_default_remote(repo);
    let head_ref = crate::engine::git_reader::remote_head_ref(&remote);
    let remote_head = git_text(repo, &["symbolic-ref", "--quiet", head_ref.as_str()]).ok();
    crate::engine::git_reader::resolve_default_base_on(repo, &remote, remote_head.as_deref())
        .map(|(short, _)| short)
}

/// True when any worktree of `repo` (including the main one) has
/// `branch_name` checked out.
pub(crate) fn is_checked_out_in_any_worktree(
    repo: &Path,
    branch_name: &str,
) -> Result<bool, String> {
    Ok(checked_out_branches(repo)?.contains(branch_name))
}

/// Short names of every local branch checked out in any worktree of `repo`
/// (including the main one). One `git worktree list` for a caller that checks
/// many branches: asking [`is_checked_out_in_any_worktree`] per branch spent
/// one spawn per branch against the shared spawn budget.
pub(crate) fn checked_out_branches(repo: &Path) -> Result<HashSet<String>, String> {
    let stdout = git_text(repo, &["worktree", "list", "--porcelain"])?;
    Ok(stdout
        .lines()
        .filter_map(|line| line.strip_prefix("branch "))
        .filter_map(|r| r.trim().strip_prefix("refs/heads/"))
        .map(str::to_string)
        .collect())
}

/// Rewrites only the subject line of a commit message, keeping the body —
/// including its blank-line separation from the subject — intact.
///
/// Canonical for the crate: the rebase argv preview in [`crate::commands`]
/// must compose the very message this writer will pass to git, so it calls
/// this rather than mirroring it — a second copy could drift and make the
/// gate approve an argv that differs from the one executed.
pub(crate) fn reworded_message(original: &str, new_subject: &str) -> String {
    match original.split_once('\n') {
        Some((_, rest)) => format!("{new_subject}\n{rest}"),
        None => new_subject.to_string(),
    }
}

/// Rejects clone URLs whose transport could execute local commands.
///
/// Allowlist: `http(s)://`, `ssh://`, `git://`, `ftp(s)://`, `file://`, plus
/// bare local paths (absolute or relative) and scp-like `user@host:path`
/// shorthand. Everything else is refused — notably git's pseudo-transports,
/// where `<scheme>::<args>` hands the argument string to an arbitrary helper
/// (`ext::sh -c <cmd>` executes it through /bin/sh). A leading `-` is refused
/// so a URL can never be parsed as a git option.
pub fn validate_clone_url(url: &str) -> Result<(), String> {
    if url.is_empty() || url.contains('\0') || url.chars().any(|c| c.is_control()) {
        return Err("Invalid clone URL".into());
    }
    if url.starts_with('-') {
        return Err("Clone URL must not start with '-'".into());
    }
    const ALLOWED_SCHEMES: [&str; 7] = [
        "http://", "https://", "ssh://", "git://", "ftps://", "ftp://", "file://",
    ];
    let lower = url.to_ascii_lowercase();
    if ALLOWED_SCHEMES
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        return Ok(());
    }
    // Any remaining `<scheme>::` pseudo-transport form is rejected wholesale;
    // plain local paths and scp-like syntax carry no `::` and fall through.
    if url.contains("::") {
        let scheme = url.split(':').next().unwrap_or("");
        return Err(format!(
            "Clone URL uses unsupported transport '{scheme}': use http(s), ssh, git, ftp(s), \
             file, or a local path"
        ));
    }
    Ok(())
}

/// Condenses raw git stderr for user-facing mutation errors: drops the
/// advisory `hint:` lines (they prescribe terminal commands this client does
/// not expose, e.g. `git rebase --continue`), collapses whitespace runs, and
/// caps the length so a pathological failure cannot flood the UI banner.
pub(crate) fn summarize_git_failure(raw: &str) -> String {
    let meaningful: String = raw
        .lines()
        .filter(|line| !line.trim_start().starts_with("hint:"))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    const CAP: usize = 400;
    if meaningful.chars().count() <= CAP {
        meaningful
    } else {
        let truncated: String = meaningful.chars().take(CAP).collect();
        format!("{truncated}…")
    }
}

const PUSH_DETACHED: &str = "Cannot push: HEAD is detached. Check out a branch, then push.";
const PUSH_NO_REMOTES: &str = "This repository has no remotes. Add a remote before pushing.";

fn branch_has_upstream(repo: &Path) -> bool {
    git_text(
        repo,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .map(|text| !text.trim().is_empty())
    .unwrap_or(false)
}

fn current_branch_name(repo: &Path) -> Result<String, String> {
    match git_text(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(name) => {
            let name = name.trim();
            if name.is_empty() {
                Err(PUSH_DETACHED.into())
            } else {
                Ok(name.to_string())
            }
        }
        Err(_) => Err(PUSH_DETACHED.into()),
    }
}

fn configured_remotes(repo: &Path) -> Result<Vec<String>, String> {
    let text = git_text(repo, &["remote"])?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

/// Remote to publish an unpublished branch to when the caller did not name one.
///
/// Priority: `checkout.defaultRemote` when it names a configured remote,
/// `origin` when it exists, a lone remote, else a refusal naming the choices.
/// Never invents `origin` — `git push -u origin …` against a missing remote is
/// the same class of raw fatal this path exists to replace.
fn unpublished_push_remote(repo: &Path) -> Result<String, String> {
    let remotes = configured_remotes(repo)?;
    if remotes.is_empty() {
        return Err(PUSH_NO_REMOTES.into());
    }
    if let Ok(configured) = git_text(repo, &["config", "--get", "checkout.defaultRemote"]) {
        let name = configured.trim();
        if remotes.iter().any(|remote| remote == name) {
            return Ok(name.to_string());
        }
    }
    if remotes.iter().any(|remote| remote == "origin") {
        return Ok("origin".into());
    }
    if let [only] = remotes.as_slice() {
        return Ok(only.clone());
    }
    Err(format!(
        "This branch has no upstream. GitPulse cannot choose among remotes {} — specify one to push.",
        remotes.join(", ")
    ))
}

fn plan_push_argv(
    repo: &Path,
    remote: Option<&str>,
    branch: Option<&str>,
    force: bool,
) -> Result<Vec<String>, String> {
    let mut args = vec!["push".to_string()];
    if force {
        args.push("--force-with-lease".into());
    }
    match (remote, branch) {
        (None, None) => {
            if !branch_has_upstream(repo) {
                let branch = current_branch_name(repo)?;
                let remote = unpublished_push_remote(repo)?;
                validate_ref_name(&remote)?;
                validate_ref_name(&branch)?;
                args.push("-u".into());
                args.push(remote);
                args.push(branch);
            }
        }
        (Some(remote), None) => {
            validate_ref_name(remote)?;
            args.push(remote.to_string());
        }
        (Some(remote), Some(branch)) => {
            validate_ref_name(remote)?;
            validate_ref_name(branch)?;
            args.push(remote.to_string());
            args.push(branch.to_string());
        }
        (None, Some(branch)) => {
            validate_ref_name(branch)?;
            args.push(branch.to_string());
        }
    }
    Ok(args)
}

pub fn validate_ref_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.starts_with('-') || name.contains('\0') {
        return Err("Invalid ref name".into());
    }
    if name.contains("..") {
        return Err("Invalid ref name: contains traversal '..'".into());
    }
    if name.starts_with('.') || name.ends_with('.') || name.ends_with('/') {
        return Err("Invalid ref name: invalid prefix or suffix".into());
    }
    // A ".lock" suffix on ANY component collides with git's lock files under
    // .git/refs/ — not just when the whole ref ends in .lock. The split also
    // covers the single-component case.
    for component in name.split('/') {
        if component.starts_with('.') || component.ends_with(".lock") {
            return Err("Invalid ref name: invalid path component".into());
        }
    }
    if name == "@" || name.contains("@{") {
        return Err("Invalid ref name: invalid '@' sequence".into());
    }
    if name.contains("//") {
        return Err("Invalid ref name: contains '//'".into());
    }
    if name.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                ' ' | '~'
                    | '^'
                    | ':'
                    | '?'
                    | '*'
                    | '['
                    | ']'
                    | '\\'
                    | ';'
                    | '`'
                    | '$'
                    | '|'
                    | '&'
                    | '<'
                    | '>'
                    | '!'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '='
                    | '"'
                    | '\''
            )
    }) {
        return Err("Invalid ref name: contains forbidden characters".into());
    }
    Ok(())
}

pub fn validate_oid(oid: &str) -> Result<(), String> {
    if oid.is_empty() || oid.len() > 64 || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid commit id".into());
    }
    Ok(())
}

/// Validates a single revision argument supplied by the UI.
///
/// Caller audit (all four call sites pass exactly one commit-ish each,
/// sourced from the frontend as OIDs or branch names):
///
/// - `execute_rebase_sequence`: `onto_commit` → `checkout --detach`, step
///   `commit_id` → `cherry-pick`;
/// - `worktree::add_worktree`: `start_point` → `worktree add … <start>`;
/// - `git_reader::get_file_blob`: still independently rejects `:` (a leftover
///   belt from when it concatenated `<rev>:<path>`). Blob bytes now come from
///   `ls-tree` + `cat-file`, never from a `rev:path` object name.
///
/// No caller ever passes ranges (`a..b`), reflog syntax (`@{u}`, `HEAD@{1}`),
/// or peel suffixes (`^{tree}`), so tightening to forbid them is proven safe:
/// `:` would inject into downstream `rev:path` specs, `{}` enables the
/// reflog/peel/search grammar, and `..` turns a single revision into a range.
pub fn validate_oid_or_revision(rev: &str) -> Result<(), String> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains('\0') {
        return Err("Invalid revision".into());
    }
    if rev.contains("..") {
        return Err("Invalid revision: ranges ('..') are not accepted".into());
    }
    if rev.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                ' ' | ';' | '&' | '|' | '`' | '$' | '(' | ')' | '<' | '>' | ':' | '{' | '}'
            )
    }) {
        return Err("Invalid revision".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;

    /// Git words an empty commit two ways depending on tree state:
    /// "nothing to commit, working tree clean" and "nothing added to commit
    /// but untracked files present". Both exit 1; both mean retry.
    fn is_empty_commit_refusal(error: &str) -> bool {
        let lower = error.to_lowercase();
        lower.contains("nothing to commit") || lower.contains("nothing added to commit")
    }

    #[test]
    fn fixture_email_matches_the_hook_denylist() {
        for email in [
            "contract@gitpulse.test",
            "GitPulse@test.local",
            "person@example.invalid",
            "person@example.com",
            "t@t",
            "t@e.com",
            "a@test",
            "a@invalid",
            "test@test.com",
            "test@gitpulse.local",
        ] {
            assert!(
                GitWriter::is_fixture_author_email(email),
                "{email} must be a fixture address"
            );
        }
        for email in [
            "ada@gitpulse.dev",
            "bharath.vbcr@gmail.com",
            "dev@github.com",
        ] {
            assert!(
                !GitWriter::is_fixture_author_email(email),
                "{email} must be committable"
            );
        }
    }

    fn git_ok(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output_locked()
            .unwrap_or_else(|err| panic!("spawn git {}: {err}", args.join(" ")));
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn mark_guarded(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join(".githooks")).unwrap();
        std::fs::write(dir.join(".githooks/pre-commit"), "#!/bin/sh\nexit 0\n").unwrap();
    }

    #[test]
    fn a_guarded_checkout_refuses_the_contract_identity_and_keeps_the_index() {
        let dir = tempfile::TempDir::new().unwrap();
        git_ok(dir.path(), &["init", "-q", "-b", "main"]);
        git_ok(dir.path(), &["config", "user.name", "GitPulse Contract"]);
        git_ok(
            dir.path(),
            &["config", "user.email", "contract@gitpulse.test"],
        );
        git_ok(dir.path(), &["config", "commit.gpgsign", "false"]);
        mark_guarded(dir.path());
        std::fs::write(dir.path().join("f.txt"), "one\n").unwrap();
        git_ok(dir.path(), &["add", "--", "f.txt"]);
        crate::test_support::trust_repo(dir.path());

        let err = GitWriter::commit(dir.path().to_str().unwrap(), "should not land", false)
            .expect_err("fixture identity must be refused");
        assert!(
            err.contains("contract@gitpulse.test"),
            "refusal must name the address, got {err}"
        );
        let head = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        assert!(!head.status.success(), "the refused commit must not exist");
        let staged = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&staged.stdout).trim(), "f.txt");
    }

    #[test]
    fn a_guarded_checkout_commits_as_a_real_identity() {
        let dir = tempfile::TempDir::new().unwrap();
        git_ok(dir.path(), &["init", "-q", "-b", "main"]);
        git_ok(dir.path(), &["config", "user.name", "Ada"]);
        git_ok(dir.path(), &["config", "user.email", "ada@gitpulse.dev"]);
        git_ok(dir.path(), &["config", "commit.gpgsign", "false"]);
        mark_guarded(dir.path());
        std::fs::write(dir.path().join("f.txt"), "one\n").unwrap();
        git_ok(dir.path(), &["add", "--", "f.txt"]);
        crate::test_support::trust_repo(dir.path());

        GitWriter::commit(dir.path().to_str().unwrap(), "real author", false)
            .expect("a real identity must still commit");
        let email = std::process::Command::new("git")
            .args(["log", "-1", "--format=%ae"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&email.stdout).trim(),
            "ada@gitpulse.dev"
        );
    }

    #[test]
    fn amend_refuses_to_keep_a_fixture_author_after_the_config_is_fixed() {
        let dir = tempfile::TempDir::new().unwrap();
        git_ok(dir.path(), &["init", "-q", "-b", "main"]);
        git_ok(dir.path(), &["config", "user.name", "GitPulse Contract"]);
        git_ok(
            dir.path(),
            &["config", "user.email", "contract@gitpulse.test"],
        );
        git_ok(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("f.txt"), "one\n").unwrap();
        git_ok(dir.path(), &["add", "--", "f.txt"]);
        git_ok(dir.path(), &["commit", "-q", "-m", "fixture"]);
        mark_guarded(dir.path());
        git_ok(dir.path(), &["config", "user.name", "Ada"]);
        git_ok(dir.path(), &["config", "user.email", "ada@gitpulse.dev"]);
        crate::test_support::trust_repo(dir.path());

        let err = GitWriter::commit(dir.path().to_str().unwrap(), "rewritten", true)
            .expect_err("amend must not preserve the fixture author");
        assert!(err.contains("contract@gitpulse.test"), "{err}");
        let email = std::process::Command::new("git")
            .args(["log", "-1", "--format=%ae"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&email.stdout).trim(),
            "contract@gitpulse.test"
        );
    }

    #[test]
    fn test_validate_ref_name() {
        assert!(validate_ref_name("feat/auth").is_ok());
        assert!(validate_ref_name("main").is_ok());
        assert!(validate_ref_name("v1.0.0").is_ok());

        assert!(validate_ref_name("-evil").is_err());
        assert!(validate_ref_name("foo bar").is_err());
        assert!(validate_ref_name("refs/../evil").is_err());
        assert!(validate_ref_name("feature.lock").is_err());
        // A ".lock" suffix on ANY component collides with git's lock files,
        // not only when it ends the whole ref.
        assert!(validate_ref_name("feature/foo.lock").is_err());
        assert!(validate_ref_name("foo.lock/bar").is_err());
        assert!(validate_ref_name("feature/lockfile").is_ok());
        assert!(validate_ref_name("foo.lockdown").is_ok());
        assert!(validate_ref_name("branch/").is_err());
        assert!(validate_ref_name(".hidden").is_err());
        assert!(validate_ref_name("foo..bar").is_err());
        assert!(validate_ref_name("@").is_err());
        assert!(validate_ref_name("HEAD@{1}").is_err());
        assert!(validate_ref_name("foo//bar").is_err());
    }

    /// No slash-separated component may start with '.' (git check-ref-format):
    /// "feat/.hidden" must be rejected even though the whole name does not.
    #[test]
    fn test_validate_ref_name_component_dot_rule() {
        assert!(validate_ref_name("feat/.hidden").is_err());
        assert!(validate_ref_name(".hidden/inner").is_err());
        assert!(validate_ref_name("feat/dot.file").is_ok());
        assert!(validate_ref_name("feat/auth").is_ok());
    }

    /// The destination resolver mirrors sandbox_join_canonical: an existing
    /// leaf is canonicalized and must stay under its canonical parent, a
    /// symlinked leaf is refused outright, a missing leaf stays lexical under
    /// the canonical parent, and non-absolute destinations are refused.
    #[cfg(unix)]
    #[test]
    fn resolve_clone_destination_canonicalizes_and_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let base = tempfile::TempDir::new().unwrap();
        let real = base.path().join("real");
        std::fs::create_dir(&real).unwrap();

        // Happy path: existing directory resolves to its canonical spelling
        // (on macOS TempDir paths live behind /var -> /private/var).
        let resolved = resolve_clone_destination(&real).expect("existing dir resolves");
        let canonical_real = real.canonicalize().unwrap();
        assert_eq!(resolved, canonical_real);

        // Missing leaf: joined lexically onto the CANONICAL parent.
        let missing = base.path().join("real").join("fresh-clone");
        let resolved = resolve_clone_destination(&missing).expect("missing leaf resolves");
        assert_eq!(resolved, canonical_real.join("fresh-clone"));

        // Symlinked final component: refused even when it points INSIDE the
        // parent, because git would write through whatever it targets.
        let link = base.path().join("link");
        symlink(&canonical_real, &link).unwrap();
        let err = resolve_clone_destination(&link).expect_err("symlink leaf must refuse");
        assert!(
            err.contains("symlink"),
            "refusal must name the symlink, got: {err}"
        );

        // Relative destination keeps its explicit refusal.
        let err = resolve_clone_destination(Path::new("relative/dest"))
            .expect_err("relative destination must refuse");
        assert!(err.contains("absolute"), "got: {err}");

        // Parent does not exist: cannot establish containment, refuse.
        let orphan = base.path().join("no-such-parent").join("clone");
        assert!(resolve_clone_destination(&orphan).is_err());
    }

    /// Regression (clone destination hardening): a symlinked destination used
    /// to be handed to `git clone` verbatim, so the clone materialized at the
    /// LINK's target — anywhere on disk. The destination must now be refused,
    /// and the link target must stay untouched.
    #[cfg(unix)]
    #[test]
    fn clone_repo_refuses_symlinked_destination_and_leaves_target_untouched() {
        use std::os::unix::fs::symlink;

        let src = init_repo_with_commit();
        let parent = tempfile::TempDir::new().unwrap();
        let elsewhere = tempfile::TempDir::new().unwrap();
        let escape_root = elsewhere.path().join("escape-root");
        std::fs::create_dir(&escape_root).unwrap();

        let link = parent.path().join("innocent-name");
        symlink(&escape_root, &link).unwrap();

        let result = GitWriter::clone_repo(src.path().to_str().unwrap(), link.to_str().unwrap());
        assert!(
            matches!(&result, Err(e) if e.contains("symlink")),
            "symlinked destination must be refused with a reason, got {result:?}"
        );
        let landed: Vec<_> = std::fs::read_dir(&escape_root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert!(
            landed.is_empty(),
            "nothing may be written through the symlink, found {landed:?}"
        );
    }

    #[test]
    fn test_validate_oid() {
        assert!(validate_oid("a1b2c3d4e5f6").is_ok());
        assert!(validate_oid("0123456789abcdef0123456789abcdef01234567").is_ok());
        assert!(validate_oid("").is_err());
        assert!(validate_oid("not-hex!").is_err());
        assert!(validate_oid("; rm -rf /").is_err());
    }

    #[test]
    fn test_validate_oid_or_revision() {
        assert!(validate_oid_or_revision("HEAD~3").is_ok());
        assert!(validate_oid_or_revision("HEAD^").is_ok());
        assert!(validate_oid_or_revision("main").is_ok());
        assert!(validate_oid_or_revision("a1b2c3d4").is_ok());
        assert!(validate_oid_or_revision("; rm -rf /").is_err());
        assert!(validate_oid_or_revision("-evil").is_err());
        // Tightened forms: no caller passes ranges or rev:path / reflog /
        // peel syntax, so they are refused (see the fn's caller audit).
        assert!(validate_oid_or_revision("a..b").is_err());
        assert!(validate_oid_or_revision("@{u}").is_err());
        assert!(validate_oid_or_revision("HEAD@{1}").is_err());
        assert!(validate_oid_or_revision("HEAD^{tree}").is_err());
        assert!(validate_oid_or_revision("rev:path.txt").is_err());
    }

    /// The clone transport allowlist: network schemes, file://, local paths
    /// and scp shorthand pass; pseudo-transports and option-shaped URLs fail.
    #[test]
    fn test_validate_clone_url() {
        for good in [
            "https://github.com/acme/gitpulse.git",
            "http://example.com/repo.git",
            "ssh://git@host/team/repo.git",
            "git://host/repo.git",
            "ftps://host/repo.git",
            "ftp://host/repo.git",
            "file:///tmp/some/repo.git",
            "/tmp/local/path",
            "some-relative-name",
            "git@github.com:acme/gitpulse.git",
            "HTTPS://HOST/REPO.GIT",
        ] {
            assert!(validate_clone_url(good).is_ok(), "{good} must be allowed");
        }
        for evil in [
            "ext::sh -c touch /tmp/gitpulse-pwned",
            "fd::9",
            "vsock::1234",
            "weird-scheme::data",
            "-oProxyCommand=evil",
            "",
            "has\0nul",
            "line\nbreak",
        ] {
            assert!(
                validate_clone_url(evil).is_err(),
                "{evil:?} must be rejected"
            );
        }
    }

    fn configure_identity(dir: &std::path::Path) {
        for (key, value) in [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
        ] {
            let output = std::process::Command::new("git")
                .args(["config", key, value])
                .current_dir(dir)
                .output_locked()
                .expect("git config");
            assert!(
                output.status.success(),
                "git config {key} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn init_repo_with_commit() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git init");
        assert!(output.status.success());
        configure_identity(dir.path());
        // Windows runners ship `core.autocrlf=true`, which rewrites the file
        // to CRLF on checkout. That is correct git behaviour, so pin it off
        // here rather than loosening the assertion: what this test is about is
        // that discard restored the content, not what EOLs git converts to.
        let output = std::process::Command::new("git")
            .args(["config", "core.autocrlf", "false"])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git config");
        assert!(output.status.success());
        std::fs::write(dir.path().join("tracked.txt"), "base\n").unwrap();
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["add", "--", "tracked.txt"])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git add");
        assert!(output.status.success());
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git commit");
        assert!(
            output.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        crate::test_support::trust_repo(dir.path());
        dir
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output_locked()
            .unwrap_or_else(|err| panic!("spawn git {}: {err}", args.join(" ")));
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        if args.first() == Some(&"init") {
            crate::test_support::trust_repo(dir);
        }
    }

    fn git_text(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output_locked()
            .map_err(|err| format!("spawn git {}: {err}", args.join(" ")))?;
        if !output.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn repo_path(dir: &tempfile::TempDir) -> String {
        dir.path().to_str().expect("utf-8 repo path").to_string()
    }

    fn init_bare() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git init --bare");
        assert!(
            output.status.success(),
            "bare init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        crate::test_support::trust_repo(dir.path());
        dir
    }

    /// Regression: a pathspec that matches nothing must surface as an error.
    /// The old body discarded both `restore` and `clean` failures and always
    /// returned Ok, so a typo'd path silently read as "discard succeeded".
    #[test]
    fn discard_changes_errors_when_pathspec_matches_nothing() {
        let dir = init_repo_with_commit();
        let result = GitWriter::discard_changes(dir.path().to_str().unwrap(), "ghost.txt");
        assert!(
            result.is_err(),
            "unknown pathspec must not report success, got {:?}",
            result
        );
    }

    #[test]
    fn discard_changes_reverts_tracked_modification() {
        let dir = init_repo_with_commit();
        std::fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
        GitWriter::discard_changes(dir.path().to_str().unwrap(), "tracked.txt")
            .expect("discard of a modified tracked file should succeed");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "base\n",
            "working-tree content must be restored"
        );
    }

    #[test]
    fn discard_changes_removes_untracked_file() {
        let dir = init_repo_with_commit();
        std::fs::write(dir.path().join("fresh.txt"), "new\n").unwrap();
        // `git restore` fails for an untracked pathspec; the discard still
        // succeeded when `clean` removed the file, so this stays Ok.
        GitWriter::discard_changes(dir.path().to_str().unwrap(), "fresh.txt")
            .expect("discard of an untracked file should succeed");
        assert!(
            !dir.path().join("fresh.txt").exists(),
            "untracked file should be gone"
        );
    }

    fn write_commit(dir: &tempfile::TempDir, file: &str, content: &str, msg: &str) -> String {
        std::fs::write(dir.path().join(file), content).unwrap();
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["add", "--", file])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git add");
        assert!(output.status.success());
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                msg,
            ])
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git commit");
        assert!(
            output.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output_locked()
            .expect("rev-parse");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn head_message(dir: &tempfile::TempDir) -> String {
        let output = std::process::Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(dir.path())
            .output_locked()
            .expect("git log");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// Regression (audit B1): Squash folds B into A but must preserve A's
    /// commit message. The old code amended with -m "Squashed commit",
    /// destroying the squashed-into commit's real message.
    #[test]
    fn rebase_squash_preserves_first_commit_message() {
        let dir = init_repo_with_commit();
        let base = write_commit(&dir, "a.txt", "a\n", "base commit");
        let c_a = write_commit(&dir, "b.txt", "b\n", "add feature A\n\nbody of A");
        let _c_b = write_commit(&dir, "c.txt", "c\n", "add feature B");

        let steps = vec![
            RebaseStep {
                commit_id: c_a.clone(),
                action: RebaseActionKind::Pick,
            },
            RebaseStep {
                commit_id: _c_b.clone(),
                action: RebaseActionKind::Squash,
            },
        ];
        GitWriter::execute_rebase_sequence(dir.path().to_str().unwrap(), &base, &steps)
            .expect("pick+squash sequence should succeed");

        let msg = head_message(&dir);
        assert_eq!(
            msg, "add feature A\n\nbody of A",
            "squash must fold into the picked commit without replacing its message"
        );
    }

    /// Regression (audit B2): starting a rebase with uncommitted changes must
    /// be refused up front; the old rollback (`checkout -f`) wiped them.
    #[test]
    fn rebase_refuses_dirty_working_tree_and_leaves_it_intact() {
        let dir = init_repo_with_commit();
        let base = write_commit(&dir, "a.txt", "a\n", "base commit");
        let c_a = write_commit(&dir, "b.txt", "b\n", "commit A");

        std::fs::write(
            dir.path().join("tracked.txt"),
            "precious uncommitted work\n",
        )
        .unwrap();

        let result = GitWriter::execute_rebase_sequence(
            dir.path().to_str().unwrap(),
            &base,
            &[RebaseStep {
                commit_id: c_a,
                action: RebaseActionKind::Pick,
            }],
        );
        assert!(result.is_err(), "dirty tree must refuse to rebase");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "precious uncommitted work\n",
            "uncommitted changes must survive the refusal untouched"
        );
        let branch = std::process::Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        assert_eq!(
            String::from_utf8(branch.stdout).unwrap().trim(),
            "main",
            "must still be on the original branch after refusal"
        );
    }

    /// Regression (audit A7): when every checkout strategy fails, the first
    /// (most meaningful) error must surface, not the last retry's.
    #[test]
    fn checkout_branch_reports_first_error_when_all_strategies_fail() {
        let dir = init_repo_with_commit();
        std::fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
        let err = GitWriter::checkout_branch(dir.path().to_str().unwrap(), "nonexistent-branch")
            .expect_err("missing branch must fail");
        assert!(
            err.contains("nonexistent-branch") || err.to_lowercase().contains("invalid"),
            "first error should name the failing ref, got: {err}"
        );
    }

    /// The per-repo mutation registry hands out one lock instance per repo
    /// path (canonicalized by validate_repo upstream) and distinct ones for
    /// different repos, so unrelated repos never serialize against each other.
    #[test]
    fn repo_mutation_lock_is_stable_per_repo_and_distinct_across_repos() {
        let dir_a = init_repo_with_commit();
        let dir_b = init_repo_with_commit();
        let canon_a = validate_repo(dir_a.path().to_str().unwrap()).unwrap();
        let canon_b = validate_repo(dir_b.path().to_str().unwrap()).unwrap();
        let l1 = super::repo_mutation_lock(&canon_a);
        let l2 = super::repo_mutation_lock(&canon_a);
        let l3 = super::repo_mutation_lock(&canon_b);
        assert!(Arc::ptr_eq(&l1, &l2), "same repo must yield the same lock");
        assert!(
            !Arc::ptr_eq(&l1, &l3),
            "different repos must not share a lock"
        );
    }

    /// Stress: concurrent mutations on one repo must all land, in either
    /// order, with no lost updates or index.lock failures — the per-repo
    /// mutation lock serializes them before git ever sees a race.
    #[test]
    fn concurrent_commits_on_one_repo_all_land_without_loss() {
        use std::sync::Barrier;
        let dir = init_repo_with_commit();
        let path = dir.path().to_str().unwrap().to_string();
        const THREADS: usize = 8;
        const PER_THREAD: usize = 4;
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for i in 0..PER_THREAD {
                        let file = format!("t{t}_f{i}.txt");
                        let content = format!("thread {t} round {i}\n");
                        std::fs::write(std::path::Path::new(&path).join(&file), content.clone())
                            .unwrap();
                        // Stage→commit spans two lock acquisitions, so a
                        // sibling mutation may consume this thread's staged
                        // entry first and leave `git commit` with nothing to
                        // do. Once HEAD carries our exact content the work
                        // has landed (under a sibling's message), and a
                        // re-stage can never make our own commit non-empty
                        // again — that outcome is success, not a retry.
                        let mut attempts = 0;
                        loop {
                            GitWriter::stage_file(&path, &file)
                                .unwrap_or_else(|e| panic!("stage {t}.{i}: {e}"));
                            match GitWriter::commit(&path, &format!("commit t{t}.{i}"), false) {
                                Ok(_) => break,
                                Err(e) if is_empty_commit_refusal(&e) => {
                                    let landed = std::process::Command::new("git")
                                        .args(["show", &format!("HEAD:{file}")])
                                        .current_dir(std::path::Path::new(&path))
                                        .output_locked()
                                        .expect("git show HEAD:path");
                                    if landed.status.success()
                                        && String::from_utf8_lossy(&landed.stdout) == content
                                    {
                                        break;
                                    }
                                    attempts += 1;
                                    assert!(
                                        attempts < 200,
                                        "stage/commit retry never converged for {t}.{i}"
                                    );
                                }
                                Err(e) => panic!("commit {t}.{i}: {e}"),
                            }
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread must not panic");
        }
        // The invariant is no lost updates, not one commit object per worker
        // round: under the shared index a sibling's commit legitimately
        // carries several workers' staged entries in one commit. Pickaxe over
        // each unique content proves it landed exactly once.
        for t in 0..THREADS {
            for i in 0..PER_THREAD {
                let file = format!("t{t}_f{i}.txt");
                let needle = format!("thread {t} round {i}");
                let log = std::process::Command::new("git")
                    .args(["log", "--format=%H", "-S", &needle, "--", &file])
                    .current_dir(dir.path())
                    .output_locked()
                    .unwrap();
                let stdout = String::from_utf8(log.stdout).unwrap();
                let hits: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
                assert_eq!(
                    hits.len(),
                    1,
                    "content of {file} must appear in exactly one commit, found {hits:?}"
                );
            }
        }
    }

    /// Stress: `commit_files` is the atomic stage+commit primitive. Under
    /// concurrency every round must produce its OWN commit — no sibling can
    /// absorb its staged bytes because stage and commit share one lock
    /// acquisition.
    #[test]
    fn concurrent_commit_files_produce_one_commit_per_round() {
        let dir = init_repo_with_commit();
        let path = dir.path().to_str().unwrap().to_string();
        const THREADS: usize = 8;
        const PER_THREAD: usize = 4;
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for i in 0..PER_THREAD {
                        let file = format!("t{t}_f{i}.txt");
                        std::fs::write(
                            std::path::Path::new(&path).join(&file),
                            format!("thread {t} round {i}\n"),
                        )
                        .unwrap();
                        GitWriter::commit_files(
                            &path,
                            &format!("commit t{t}.{i}"),
                            std::slice::from_ref(&file),
                        )
                        .unwrap_or_else(|e| panic!("commit_files {t}.{i}: {e}"));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread must not panic");
        }
        let log = std::process::Command::new("git")
            .args(["log", "--format=%s"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        let stdout = String::from_utf8(log.stdout).unwrap();
        assert_eq!(
            stdout.lines().count(),
            1 + THREADS * PER_THREAD,
            "atomic commit_files must yield exactly one commit per round"
        );
        for t in 0..THREADS {
            for i in 0..PER_THREAD {
                let msg = format!("commit t{t}.{i}");
                assert_eq!(
                    stdout.lines().filter(|l| *l == msg).count(),
                    1,
                    "{msg} must appear exactly once"
                );
            }
        }
    }

    /// `commit_files` must refuse empty inputs instead of silently creating
    /// an empty commit or running `git add` with no pathspec.
    #[test]
    fn commit_files_rejects_empty_inputs() {
        let dir = init_repo_with_commit();
        let path = dir.path().to_str().unwrap().to_string();
        assert!(GitWriter::commit_files(&path, "msg", &[]).is_err());
        assert!(GitWriter::commit_files(&path, "msg", &[String::new()]).is_err());
        assert!(GitWriter::commit_files(&path, "   ", &["a.txt".into()]).is_err());
        assert!(GitWriter::commit_files(&path, "msg", &["../escape.txt".into()]).is_err());
        assert_eq!(head_message(&dir), "init", "no commit may be created");
    }

    #[test]
    fn quick_commit_add_argv_is_add_all() {
        assert_eq!(GitWriter::QUICK_COMMIT_ADD_ARGV, &["add", "--all"]);
    }

    #[test]
    fn quick_commit_refuses_empty_message_and_clean_tree() {
        let dir = init_repo_with_commit();
        configure_identity(dir.path());
        let path = dir.path().to_str().unwrap().to_string();
        assert!(GitWriter::quick_commit(&path, "   ").is_err());
        assert!(
            GitWriter::quick_commit(&path, "feat: nothing").is_err(),
            "a clean tree must not produce a commit"
        );
        assert_eq!(head_message(&dir), "init");
    }

    #[test]
    fn quick_commit_stages_unstaged_untracked_and_deletions_but_not_ignored() {
        let dir = init_repo_with_commit();
        configure_identity(dir.path());
        let path = dir.path().to_str().unwrap().to_string();
        std::fs::write(dir.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "do not commit\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "untracked\n").unwrap();
        std::fs::write(dir.path().join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(dir.path().join("doomed.txt"), "gone soon\n").unwrap();
        GitWriter::commit_files(
            &path,
            "chore: seed extra",
            &["doomed.txt".into(), ".gitignore".into()],
        )
        .expect("seed");
        std::fs::remove_file(dir.path().join("doomed.txt")).unwrap();

        GitWriter::quick_commit(&path, "feat: everything").expect("quick commit");
        assert_eq!(head_message(&dir), "feat: everything");

        let show = std::process::Command::new("git")
            .args(["show", "--name-only", "--pretty=format:", "HEAD"])
            .current_dir(dir.path())
            .output_locked()
            .expect("git show");
        let names = String::from_utf8(show.stdout).unwrap();
        assert!(
            names.contains("new.txt"),
            "untracked file must be committed: {names}"
        );
        assert!(
            names.contains("tracked.txt"),
            "unstaged edit must be committed: {names}"
        );
        assert!(
            names.contains("doomed.txt"),
            "deletion must be committed: {names}"
        );
        assert!(
            !names.contains("secret.txt"),
            "ignored untracked file must stay untracked: {names}"
        );
        assert!(dir.path().join("secret.txt").exists());
    }

    /// Linked worktrees must share the mutation lock keyed by the common git
    /// dir, so two tabs of the same repository serialize instead of racing
    /// on refs while each holding a different working-tree mutex.
    #[test]
    fn repo_mutation_lock_is_shared_across_linked_worktrees() {
        let dir = init_repo_with_commit();
        let parent = tempfile::TempDir::new().unwrap();
        let wt = parent.path().join("agent-wt");
        crate::engine::worktree::add_worktree(
            dir.path().to_str().unwrap(),
            wt.to_str().unwrap(),
            Some("agent/lock-share"),
            Some("main"),
            false,
        )
        .expect("add worktree");
        crate::test_support::trust_repo(&wt);
        let canon_main = validate_repo(dir.path().to_str().unwrap()).unwrap();
        let canon_wt = validate_repo(wt.to_str().unwrap()).unwrap();
        assert_ne!(
            canon_main, canon_wt,
            "worktree working directory is distinct from the main checkout"
        );
        let l_main = super::repo_mutation_lock(&canon_main);
        let l_wt = super::repo_mutation_lock(&canon_wt);
        assert!(
            Arc::ptr_eq(&l_main, &l_wt),
            "main checkout and linked worktree must share one mutation lock"
        );
    }

    /// A stale `.git/index.lock` from a concurrent agent must be retried, not
    /// surfaced as a hard failure, once the other process drops it.
    #[test]
    fn stage_retries_while_index_lock_is_held_then_released() {
        let dir = init_repo_with_commit();
        std::fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
        let lock_path = dir.path().join(".git").join("index.lock");
        std::fs::write(&lock_path, b"").unwrap();
        let lock_clone = lock_path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            let _ = std::fs::remove_file(lock_clone);
        });
        GitWriter::stage_file(dir.path().to_str().unwrap(), "tracked.txt")
            .expect("stage must succeed after the contended lock is released");
    }

    #[test]
    fn rebase_rejects_squash_or_fixup_as_the_first_step() {
        let dir = init_repo_with_commit();
        let base = write_commit(&dir, "a.txt", "a\n", "base commit");
        let c_a = write_commit(&dir, "b.txt", "b\n", "commit A");
        let squash_err = GitWriter::execute_rebase_sequence(
            dir.path().to_str().unwrap(),
            &base,
            &[RebaseStep {
                commit_id: c_a.clone(),
                action: RebaseActionKind::Squash,
            }],
        )
        .expect_err("first-step squash must be refused");
        assert!(
            squash_err.to_lowercase().contains("squash"),
            "error must name squash, got: {squash_err}"
        );
        let fixup_err = GitWriter::execute_rebase_sequence(
            dir.path().to_str().unwrap(),
            &base,
            &[RebaseStep {
                commit_id: c_a,
                action: RebaseActionKind::Fixup,
            }],
        )
        .expect_err("first-step fixup must be refused");
        assert!(
            fixup_err.to_lowercase().contains("fixup"),
            "error must name fixup, got: {fixup_err}"
        );
        assert_eq!(head_message(&dir), "commit A", "HEAD must be untouched");
    }

    /// Pure-message tests for rebase recovery reporting: the step error always
    /// leads verbatim, and every recovery failure (abort and/or restore) must
    /// be named with its true end-state instead of being swallowed behind the
    /// primary failure.
    #[test]
    fn combine_step_failure_clean_recovery_returns_step_error_verbatim() {
        let msg = combine_step_failure_with_recovery(
            "Rebase step failed (pick abc123): conflict",
            None,
            &Ok(()),
            Some("main"),
            "0000000000000000000000000000000000000000",
        );
        assert_eq!(
            msg, "Rebase step failed (pick abc123): conflict",
            "clean recovery must not add any clauses"
        );
    }

    #[test]
    fn combine_step_failure_names_failed_cherry_pick_abort() {
        let msg = combine_step_failure_with_recovery(
            "step boom",
            Some(&Err("index.lock exists".to_string())),
            &Ok(()),
            Some("main"),
            "abc",
        );
        assert!(msg.starts_with("step boom"), "step error leads: {msg}");
        assert!(
            msg.contains("`cherry-pick --abort` failed (index.lock exists)"),
            "{msg}"
        );
        assert!(
            msg.contains("may still be mid-cherry-pick"),
            "end-state must warn about mid-cherry-pick: {msg}"
        );
        assert!(!msg.contains("restoring"), "restore succeeded: {msg}");
    }

    #[test]
    fn combine_step_failure_names_failed_branch_restore() {
        let msg = combine_step_failure_with_recovery(
            "step boom",
            None,
            &Err("disk full".to_string()),
            Some("feature/x"),
            "abc",
        );
        assert!(msg.starts_with("step boom"), "step error leads: {msg}");
        assert!(
            msg.contains("restoring feature/x failed (disk full)"),
            "{msg}"
        );
        assert!(
            msg.contains("HEAD may remain detached"),
            "end-state must warn about detached HEAD: {msg}"
        );
        assert!(
            !msg.contains("abort"),
            "no cherry-pick was in flight: {msg}"
        );
    }

    #[test]
    fn combine_step_failure_reports_both_recovery_failures_in_order() {
        let msg = combine_step_failure_with_recovery(
            "step boom",
            Some(&Err("abort err".to_string())),
            &Err("restore err".to_string()),
            Some("main"),
            "def456",
        );
        assert!(msg.starts_with("step boom"), "{msg}");
        let abort_at = msg
            .find("`cherry-pick --abort` failed")
            .expect("abort clause present");
        let restore_at = msg.find("restoring main failed").expect("restore clause");
        assert!(
            abort_at < restore_at,
            "abort clause precedes restore clause: {msg}"
        );
    }

    #[test]
    fn combine_step_failure_detached_head_restore_names_head_oid() {
        let msg = combine_step_failure_with_recovery(
            "step boom",
            None,
            &Err("locked index".to_string()),
            None,
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        );
        assert!(
            msg.contains(
                "restoring HEAD deadbeefdeadbeefdeadbeefdeadbeefdeadbeef failed \
                          (locked index)"
            ),
            "detached HEAD wording must name the oid: {msg}"
        );
    }

    /// Failed cleanup must identify retained data without claiming removal.
    #[test]
    fn a_failed_cleanup_is_never_reported_as_a_removal() {
        let leftover = Path::new("/repos/.gitpulse-clone-test");

        let removed = partial_clone_message("timeout", leftover, Ok(()));
        assert!(
            removed.contains("removed this attempt's staging directory"),
            "{removed}"
        );
        assert!(removed.contains("/repos/.gitpulse-clone-test"), "{removed}");

        let kept = partial_clone_message(
            "timeout",
            leftover,
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied",
            )),
        );
        assert!(
            !kept.contains("removed this attempt"),
            "a failed removal must not claim to have removed anything: {kept}"
        );
        assert!(kept.contains("could not be removed"), "{kept}");
        assert!(kept.contains("denied"), "the reason must survive: {kept}");
        assert!(
            kept.contains("retained for manual recovery"),
            "the consequence must be stated, not left to be discovered: {kept}"
        );

        // Both branches keep the primary failure; the cleanup is the footnote.
        for message in [&removed, &kept] {
            assert!(message.contains("clone failed (timeout)"), "{message}");
        }
    }

    #[test]
    fn unpublished_push_plans_set_upstream_to_origin() {
        let origin = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        let argv = GitWriter::plan_push(&repo_path(&repo), None, None, false).unwrap();
        assert_eq!(
            argv,
            vec!["push", "-u", "origin", "main"],
            "an unpublished branch must be judged as the -u that will run, not a bare git push"
        );
    }

    #[test]
    fn unpublished_force_push_keeps_lease_and_sets_upstream() {
        let origin = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        let argv = GitWriter::plan_push(&repo_path(&repo), None, None, true).unwrap();
        assert_eq!(
            argv,
            vec!["push", "--force-with-lease", "-u", "origin", "main"]
        );
    }

    #[test]
    fn push_with_upstream_stays_a_bare_push() {
        let origin = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        GitWriter::push(&repo_path(&repo), None, None, false).expect("first push publishes");
        let argv = GitWriter::plan_push(&repo_path(&repo), None, None, false).unwrap();
        assert_eq!(argv, vec!["push"]);
    }

    #[test]
    fn explicit_remote_and_branch_do_not_invent_set_upstream() {
        let origin = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        let argv =
            GitWriter::plan_push(&repo_path(&repo), Some("origin"), Some("main"), false).unwrap();
        assert_eq!(argv, vec!["push", "origin", "main"]);
    }

    #[test]
    fn unpublished_push_uses_the_only_remote_even_when_it_is_not_origin() {
        let remote = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "company", remote.path().to_str().unwrap()],
        );
        let argv = GitWriter::plan_push(&repo_path(&repo), None, None, false).unwrap();
        assert_eq!(argv, vec!["push", "-u", "company", "main"]);
    }

    #[test]
    fn unpublished_push_prefers_checkout_default_remote_when_named() {
        let alpha = init_bare();
        let beta = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "alpha", alpha.path().to_str().unwrap()],
        );
        git_in(
            repo.path(),
            &["remote", "add", "beta", beta.path().to_str().unwrap()],
        );
        git_in(repo.path(), &["config", "checkout.defaultRemote", "beta"]);
        let argv = GitWriter::plan_push(&repo_path(&repo), None, None, false).unwrap();
        assert_eq!(argv, vec!["push", "-u", "beta", "main"]);
    }

    #[test]
    fn unpublished_push_prefers_origin_when_several_remotes_are_unconfigured() {
        let origin = init_bare();
        let other = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git_in(
            repo.path(),
            &["remote", "add", "other", other.path().to_str().unwrap()],
        );
        let argv = GitWriter::plan_push(&repo_path(&repo), None, None, false).unwrap();
        assert_eq!(argv, vec!["push", "-u", "origin", "main"]);
    }

    #[test]
    fn unpublished_push_refuses_to_invent_a_remote_when_none_exist() {
        let repo = init_repo_with_commit();
        let err = GitWriter::plan_push(&repo_path(&repo), None, None, false).unwrap_err();
        assert!(
            err.contains("no remotes"),
            "must not fall through to git's no-upstream fatal, got: {err}"
        );
        assert!(!err.to_lowercase().contains("set-upstream"));
    }

    #[test]
    fn unpublished_push_refuses_when_remotes_are_ambiguous() {
        let alpha = init_bare();
        let beta = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "alpha", alpha.path().to_str().unwrap()],
        );
        git_in(
            repo.path(),
            &["remote", "add", "beta", beta.path().to_str().unwrap()],
        );
        let err = GitWriter::plan_push(&repo_path(&repo), None, None, false).unwrap_err();
        assert!(err.contains("cannot choose among remotes"), "got: {err}");
        assert!(err.contains("alpha"));
        assert!(err.contains("beta"));
    }

    #[test]
    fn unpublished_push_refuses_detached_head() {
        let origin = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git_in(repo.path(), &["checkout", "--detach"]);
        let err = GitWriter::plan_push(&repo_path(&repo), None, None, false).unwrap_err();
        assert!(err.contains("HEAD is detached"), "got: {err}");
    }

    #[test]
    fn unpublished_push_sets_upstream_on_a_local_origin() {
        let origin = init_bare();
        let repo = init_repo_with_commit();
        git_in(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        GitWriter::push(&repo_path(&repo), None, None, false).expect("publish");
        let tracking = git_text(
            repo.path(),
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )
        .expect("upstream");
        assert_eq!(tracking.trim(), "origin/main");
    }

    #[test]
    fn push_planned_refuses_a_raw_force_flag() {
        let repo = init_repo_with_commit();
        let err = GitWriter::push_planned(
            &repo_path(&repo),
            &["push".into(), "--force".into(), "origin".into()],
        )
        .unwrap_err();
        assert_eq!(
            err, "Invalid ref name",
            "raw --force starts with '-' so validate_ref_name must refuse it before git runs"
        );
    }

    #[test]
    fn repo_mutation_lock_prunes_idle_locks_at_cap() {
        let held_lock = repo_mutation_lock(Path::new("/mock/repo_held"));
        for i in 0..70 {
            let path = format!("/mock/repo_{i}");
            let _ = repo_mutation_lock(Path::new(&path));
        }
        let reacquired = repo_mutation_lock(Path::new("/mock/repo_held"));
        assert!(Arc::ptr_eq(&held_lock, &reacquired));
    }

    /// Installs an executable `name` hook in `dir`'s default hooks directory.
    #[cfg(unix)]
    fn install_hook(dir: &std::path::Path, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let hooks = dir.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let path = hooks.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn head_oid(dir: &tempfile::TempDir) -> String {
        git_text(dir.path(), &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    /// A hook that outlives the hook budget is stopped, and the error names
    /// the hook rather than calling it a git hang.
    #[cfg(unix)]
    #[test]
    fn a_hook_past_its_budget_is_named_in_the_timeout() {
        let dir = init_repo_with_commit();
        install_hook(dir.path(), "pre-commit", "sleep 30");
        std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
        git_in(dir.path(), &["add", "tracked.txt"]);
        let before = head_oid(&dir);
        let started = std::time::Instant::now();
        let err = crate::engine::git_cli::hooks::with_hook_timeout(
            std::time::Duration::from_secs(2),
            || GitWriter::commit(&repo_path(&dir), "never lands", false),
        )
        .unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{err}"
        );
        assert!(err.contains("git commit timed out after "), "{err}");
        assert!(err.contains("pre-commit"), "the hook must be named: {err}");
        assert!(
            !err.contains("commit-msg"),
            "only installed hooks are named: {err}"
        );
        assert_eq!(head_oid(&dir), before);
    }

    /// The user can stop a slow hook from the UI; the commit does not land
    /// and the hook's process goes with git's.
    #[cfg(unix)]
    #[test]
    fn a_running_hook_is_cancelled_on_request() {
        let dir = init_repo_with_commit();
        let marker = dir.path().join(".git/hook-started");
        install_hook(
            dir.path(),
            "pre-commit",
            &format!("touch '{}'\nsleep 30", marker.display()),
        );
        std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
        git_in(dir.path(), &["add", "tracked.txt"]);
        let before = head_oid(&dir);
        let canonical = dir.path().canonicalize().unwrap();
        let canceller = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !marker.exists() {
                assert!(std::time::Instant::now() < deadline, "hook never started");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            crate::engine::git_cli::cancel_hooked_git(&canonical)
        });
        let started = std::time::Instant::now();
        let err = GitWriter::commit(&repo_path(&dir), "never lands", false).unwrap_err();
        assert_eq!(canceller.join().unwrap(), 1, "one run was in flight");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{err}"
        );
        assert!(
            crate::engine::git_cli::is_hooked_git_cancelled(&err),
            "{err}"
        );
        assert!(err.contains("pre-commit"), "{err}");
        assert_eq!(head_oid(&dir), before);
    }

    /// Overrides any global identity with an empty one, which git refuses
    /// to commit under, so the repository has no usable identity at all.
    fn blank_identity(dir: &std::path::Path) {
        git_ok(dir, &["config", "user.name", ""]);
        git_ok(dir, &["config", "user.email", ""]);
    }

    #[test]
    fn identity_reads_config_then_an_environment_covering_author_and_committer() {
        let dir = init_repo_with_commit();
        blank_identity(dir.path());
        let none = |_: &str| None;
        assert_eq!(
            GitWriter::identity_with(dir.path(), &none).unwrap(),
            GitIdentity {
                name: None,
                email: None
            }
        );
        let author_only = |key: &str| (key == "GIT_AUTHOR_NAME").then(|| "Ada".to_string());
        assert_eq!(
            GitWriter::identity_with(dir.path(), &author_only)
                .unwrap()
                .name,
            None,
            "an author without a committer is not enough to record a commit"
        );
        let full = |key: &str| match key {
            "GIT_AUTHOR_NAME" | "GIT_COMMITTER_NAME" => Some("Ada".to_string()),
            "EMAIL" => Some("ada@gitpulse.dev".to_string()),
            _ => None,
        };
        let identity = GitWriter::identity_with(dir.path(), &full).unwrap();
        assert!(identity.is_complete(), "{identity:?}");
        git_ok(dir.path(), &["config", "user.name", "Grace"]);
        assert_eq!(
            GitWriter::identity_with(dir.path(), &full)
                .unwrap()
                .name
                .as_deref(),
            Some("Grace"),
            "configuration wins over the environment"
        );
    }

    #[test]
    fn a_commit_without_an_identity_is_refused_before_git_runs_and_setting_one_fixes_it() {
        for key in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
            assert!(
                std::env::var_os(key).is_none(),
                "this test needs an environment without {key}"
            );
        }
        let dir = init_repo_with_commit();
        blank_identity(dir.path());
        std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        git_in(dir.path(), &["add", "tracked.txt"]);
        let before = head_oid(&dir);
        for err in [
            GitWriter::commit(&repo_path(&dir), "no identity", false).unwrap_err(),
            GitWriter::quick_commit(&repo_path(&dir), "no identity").unwrap_err(),
            GitWriter::commit_files(&repo_path(&dir), "no identity", &["new.txt".into()])
                .unwrap_err(),
        ] {
            assert!(err.starts_with(IDENTITY_MISSING), "{err}");
            assert!(err.contains("user.name"), "{err}");
        }
        assert_eq!(head_oid(&dir), before);
        let staged = git_text(dir.path(), &["diff", "--cached", "--name-only"]).unwrap();
        assert_eq!(
            staged.trim(),
            "tracked.txt",
            "quick commit must not have run `add`"
        );

        assert!(
            GitWriter::set_identity(&repo_path(&dir), "  ", "a@b", IdentityScope::Repo).is_err()
        );
        assert!(GitWriter::set_identity(
            &repo_path(&dir),
            "Ada",
            "not-an-email",
            IdentityScope::Repo
        )
        .is_err());
        let identity = GitWriter::set_identity(
            &repo_path(&dir),
            " Ada Lovelace ",
            "ada@gitpulse.dev",
            IdentityScope::Repo,
        )
        .unwrap();
        assert_eq!(identity.name.as_deref(), Some("Ada Lovelace"));
        GitWriter::commit(&repo_path(&dir), "with identity", false).unwrap();
        let author = git_text(dir.path(), &["log", "-1", "--format=%an <%ae>"]).unwrap();
        assert_eq!(author.trim(), "Ada Lovelace <ada@gitpulse.dev>");
    }

    #[test]
    fn identity_argv_writes_the_chosen_scope_only() {
        let [name, email] = GitWriter::identity_argv(IdentityScope::Global, "Ada", "a@b.dev");
        assert_eq!(name, ["git", "config", "--global", "user.name", "Ada"]);
        assert_eq!(
            email,
            ["git", "config", "--global", "user.email", "a@b.dev"]
        );
        let [name, _] = GitWriter::identity_argv(IdentityScope::Repo, "Ada", "a@b.dev");
        assert_eq!(name[2], "--local");
    }

    /// A repository with `feature` two commits ahead of `main`, on `main`.
    fn feature_ahead() -> tempfile::TempDir {
        let dir = init_repo_with_commit();
        git_in(dir.path(), &["checkout", "-q", "-b", "feature"]);
        write_commit(&dir, "a.txt", "a\n", "feature one");
        write_commit(&dir, "b.txt", "b\n", "feature two");
        git_in(dir.path(), &["checkout", "-q", "main"]);
        dir
    }

    fn parents_of_head(dir: &tempfile::TempDir) -> usize {
        git_text(dir.path(), &["rev-list", "--parents", "-n", "1", "HEAD"])
            .unwrap()
            .split_whitespace()
            .count()
            - 1
    }

    #[test]
    fn merge_modes_run_the_argv_the_gate_was_shown() {
        assert_eq!(
            GitWriter::merge_argv("f", MergeMode::Default),
            ["merge", "--no-edit", "f"]
        );
        assert_eq!(
            GitWriter::merge_argv("f", MergeMode::FfOnly),
            ["merge", "--ff-only", "--no-edit", "f"]
        );
        assert_eq!(
            GitWriter::merge_argv("f", MergeMode::NoFf),
            ["merge", "--no-ff", "--no-edit", "f"]
        );
        assert_eq!(
            GitWriter::merge_argv("f", MergeMode::Squash),
            ["merge", "--squash", "f"]
        );
        assert_eq!(
            crate::engine::worktree::merge_teardown_argv("f", true),
            GitWriter::merge_argv("f", MergeMode::Squash),
            "worktree teardown squashes through the same path"
        );
    }

    #[test]
    fn no_ff_records_a_merge_commit_where_a_fast_forward_was_possible() {
        let dir = feature_ahead();
        let mut asked = Vec::new();
        GitWriter::merge_branch(&repo_path(&dir), "feature", MergeMode::NoFf, &mut |args| {
            asked.push(args.join(" "));
            Ok(())
        })
        .unwrap();
        assert_eq!(asked, ["merge --no-ff --no-edit feature"]);
        assert_eq!(parents_of_head(&dir), 2);
    }

    #[test]
    fn squash_merges_into_one_commit_and_asks_the_gate_about_each_step() {
        let dir = feature_ahead();
        let before = head_oid(&dir);
        let mut asked = Vec::new();
        let output = GitWriter::merge_branch(
            &repo_path(&dir),
            "feature",
            MergeMode::Squash,
            &mut |args| {
                asked.push(args.join(" "));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            asked,
            [
                "merge --squash feature",
                "commit -m Merge branch 'feature' (squashed)"
            ]
        );
        assert!(output.contains("Squashed feature"), "{output}");
        assert_eq!(parents_of_head(&dir), 1, "a squash has no merge parent");
        assert_eq!(
            git_text(dir.path(), &["rev-parse", "HEAD~1"])
                .unwrap()
                .trim(),
            before
        );
        assert!(dir.path().join("a.txt").exists() && dir.path().join("b.txt").exists());

        let refused =
            GitWriter::merge_branch(&repo_path(&dir), "feature", MergeMode::Squash, &mut |_| {
                Err("policy: no".into())
            })
            .unwrap_err();
        assert_eq!(refused, "policy: no");
    }

    #[test]
    fn pull_argv_names_the_rebase_choice_and_never_a_branch_without_a_remote() {
        assert_eq!(GitWriter::pull_argv(None, None, None), ["pull"]);
        assert_eq!(
            GitWriter::pull_argv(None, None, Some(true)),
            ["pull", "--rebase"]
        );
        assert_eq!(
            GitWriter::pull_argv(Some("origin"), Some("main"), Some(false)),
            ["pull", "--no-rebase", "origin", "main"]
        );
        assert_eq!(
            GitWriter::pull_argv(None, Some("main"), None),
            ["pull"],
            "git would read a lone branch as a remote"
        );
    }

    /// `local` is a clone of a bare `origin`; `other` pushed one commit there.
    fn diverged_clone() -> (tempfile::TempDir, tempfile::TempDir) {
        let seed = init_repo_with_commit();
        let origin = tempfile::TempDir::new().unwrap();
        git_ok(
            origin.path(),
            &["clone", "-q", "--bare", &repo_path(&seed), "."],
        );
        let local = tempfile::TempDir::new().unwrap();
        git_ok(
            local.path(),
            &["clone", "-q", &origin.path().to_string_lossy(), "."],
        );
        configure_identity(local.path());
        crate::test_support::trust_repo(local.path());
        let other = tempfile::TempDir::new().unwrap();
        git_ok(
            other.path(),
            &["clone", "-q", &origin.path().to_string_lossy(), "."],
        );
        configure_identity(other.path());
        std::fs::write(other.path().join("theirs.txt"), "theirs\n").unwrap();
        git_ok(other.path(), &["add", "theirs.txt"]);
        git_ok(other.path(), &["commit", "-q", "-m", "theirs"]);
        git_ok(other.path(), &["push", "-q", "origin", "HEAD"]);
        std::fs::write(local.path().join("mine.txt"), "mine\n").unwrap();
        git_ok(local.path(), &["add", "mine.txt"]);
        git_ok(local.path(), &["commit", "-q", "-m", "mine"]);
        (local, origin)
    }

    #[test]
    fn pull_with_rebase_replays_local_commits_without_a_merge() {
        let (local, _origin) = diverged_clone();
        assert_eq!(
            GitWriter::pull_rebase_config(&repo_path(&local)).unwrap(),
            None
        );
        GitWriter::pull(&repo_path(&local), None, None, Some(true)).unwrap();
        assert_eq!(parents_of_head(&local), 1, "rebased, not merged");
        let log = git_text(local.path(), &["log", "--format=%s", "-3"]).unwrap();
        assert_eq!(log.lines().collect::<Vec<_>>(), ["mine", "theirs", "init"]);
        git_ok(local.path(), &["config", "pull.rebase", "merges"]);
        assert_eq!(
            GitWriter::pull_rebase_config(&repo_path(&local))
                .unwrap()
                .as_deref(),
            Some("merges")
        );
    }

    #[test]
    fn stash_with_paths_sets_aside_only_the_selection() {
        let dir = init_repo_with_commit();
        write_commit(&dir, "other.txt", "base\n", "other");
        std::fs::write(dir.path().join("tracked.txt"), "stash me\n").unwrap();
        std::fs::write(dir.path().join("other.txt"), "keep me\n").unwrap();
        std::fs::write(dir.path().join("*"), "literal star\n").unwrap();
        let options = StashSaveOptions {
            paths: vec!["tracked.txt".into(), "*".into(), "tracked.txt".into()],
            ..StashSaveOptions::default()
        };
        assert_eq!(
            options.argv(Some("part")),
            [
                "git",
                "stash",
                "push",
                "-u",
                "-m",
                "part",
                "--",
                ":(literal)tracked.txt",
                ":(literal)*"
            ]
        );
        GitWriter::stash_save_with(&repo_path(&dir), Some("part"), options).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "base\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("other.txt")).unwrap(),
            "keep me\n"
        );
        assert!(
            !dir.path().join("*").exists(),
            "the literal `*` file was stashed"
        );

        let escape = StashSaveOptions {
            paths: vec!["../outside".into()],
            ..StashSaveOptions::default()
        };
        assert!(GitWriter::stash_save_with(&repo_path(&dir), None, escape).is_err());
        let huge = StashSaveOptions {
            paths: (0..2000).map(|i| format!("file-number-{i}.txt")).collect(),
            ..StashSaveOptions::default()
        };
        let err = GitWriter::stash_save_with(&repo_path(&dir), None, huge).unwrap_err();
        assert!(err.contains("too long to stash in one command"), "{err}");
    }

    #[test]
    fn clone_progress_reads_git_phases_and_ignores_diagnostics() {
        let parse = CloneProgress::parse;
        assert_eq!(
            parse("Receiving objects:  45% (450/1000), 1.20 MiB | 2.00 MiB/s"),
            Some(CloneProgress {
                phase: "Receiving objects".into(),
                percent: Some(45)
            })
        );
        assert_eq!(
            parse("remote: Counting objects: 100% (12/12), done."),
            Some(CloneProgress {
                phase: "Counting objects".into(),
                percent: Some(100)
            })
        );
        assert_eq!(
            parse("Cloning into '/tmp/x'..."),
            Some(CloneProgress {
                phase: "Cloning".into(),
                percent: None
            })
        );
        assert_eq!(parse("fatal: repository 'x' does not exist"), None);
        assert_eq!(parse("warning: --depth is ignored in local clones"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn clone_options_shallow_branch_and_progress() {
        let src = init_repo_with_commit();
        write_commit(&src, "two.txt", "2\n", "two");
        git_in(src.path(), &["branch", "side"]);
        write_commit(&src, "three.txt", "3\n", "three");
        let parent = tempfile::TempDir::new().unwrap();
        let url = format!("file://{}", src.path().display());
        let options = CloneOptions {
            branch: Some("side".into()),
            depth: Some(1),
            recurse_submodules: false,
        };
        let mut seen = Vec::new();
        let cloned = GitWriter::clone_repo_with(
            &url,
            parent.path().join("shallow").to_str().unwrap(),
            &options,
            &mut |progress| seen.push(progress.clone()),
        )
        .unwrap();
        let cloned = Path::new(&cloned);
        assert_eq!(
            git_text(cloned, &["rev-list", "--count", "HEAD"])
                .unwrap()
                .trim(),
            "1",
            "depth 1"
        );
        assert_eq!(
            git_text(cloned, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap()
                .trim(),
            "side"
        );
        assert!(!cloned.join("three.txt").exists());
        assert!(
            seen.iter().any(|p| p.percent.is_some()),
            "progress was streamed: {seen:?}"
        );
        for window in seen.windows(2) {
            assert_ne!(window[0], window[1], "unchanged progress is not resent");
        }
        assert!(CloneOptions {
            depth: Some(0),
            ..CloneOptions::default()
        }
        .validate()
        .is_err());
        assert!(CloneOptions {
            branch: Some("-x".into()),
            ..CloneOptions::default()
        }
        .validate()
        .is_err());
        assert_eq!(
            CloneOptions {
                recurse_submodules: true,
                ..CloneOptions::default()
            }
            .argv(),
            ["clone", "--progress", "--recurse-submodules", "--"]
        );
        let missing = GitWriter::clone_repo_with(
            &format!("file://{}/nope", parent.path().display()),
            parent.path().join("missing").to_str().unwrap(),
            &CloneOptions::default(),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(
            missing.contains("fatal:"),
            "the diagnosis survives: {missing}"
        );
    }

    #[test]
    fn auto_fetch_fetches_skips_a_busy_repository_and_never_prunes() {
        assert!(!AUTO_FETCH_ARGV.contains(&"--prune"));
        let (local, _origin) = diverged_clone();
        let before = git_text(local.path(), &["rev-parse", "origin/main"]).unwrap();
        let canon = local.path().canonicalize().unwrap();
        {
            let lock = repo_mutation_lock(&canon);
            let _held = lock.lock().unwrap();
            let busy = GitWriter::auto_fetch(&repo_path(&local)).unwrap();
            assert!(matches!(busy, AutoFetchOutcome::Skipped { .. }), "{busy:?}");
            assert_eq!(
                git_text(local.path(), &["rev-parse", "origin/main"]).unwrap(),
                before,
                "a skipped fetch ran nothing"
            );
        }
        assert_eq!(
            GitWriter::auto_fetch(&repo_path(&local)).unwrap(),
            AutoFetchOutcome::Fetched
        );
        assert_ne!(
            git_text(local.path(), &["rev-parse", "origin/main"]).unwrap(),
            before
        );
    }

    /// The bug as reported: a pre-commit hook slower than the 90 s read
    /// timeout. Real time, so ignored by default; run it with
    /// `cargo test --lib slow_pre_commit_hook_past_ninety_seconds -- --ignored`.
    #[cfg(unix)]
    #[test]
    #[ignore = "sleeps 95 s in a real pre-commit hook"]
    fn slow_pre_commit_hook_past_ninety_seconds_still_commits() {
        let dir = init_repo_with_commit();
        install_hook(dir.path(), "pre-commit", "sleep 95");
        std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
        git_in(dir.path(), &["add", "tracked.txt"]);
        GitWriter::commit(&repo_path(&dir), "slow hook", false)
            .expect("a slow hook is not a hung git");
        assert_eq!(head_message(&dir).trim(), "slow hook");
    }
}
