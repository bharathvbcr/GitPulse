//! `WorktreeCreate` and `WorktreeRemove`: Claude Code's own worktrees, made and
//! removed by GitPulse.
//!
//! `claude --worktree`, the `EnterWorktree` tool, a subagent with
//! `isolation: "worktree"`, a workflow agent and a background session each ask
//! for a worktree. Without these hooks Claude Code runs `git worktree add`
//! itself and the result gets none of GitPulse's setup: no `post_create` hooks,
//! no copied build caches, no `.worktreeinclude`, no seeded DevMap store, and
//! no record in the ledger. With them, the worktree is made by the same owner
//! function the app's add-worktree path uses
//! ([`crate::engine::worktree::add_worktree_with_devmap`]), at the path Claude
//! Code would have used, and removed by the app's removal path
//! ([`crate::engine::worktree::remove_worktree`], which renames the directory
//! aside under the repository lock).
//!
//! # The protocol (hooks reference, read 2026-11-06)
//!
//! <https://code.claude.com/docs/en/hooks>, "WorktreeCreate" and
//! "WorktreeRemove":
//!
//! * `WorktreeCreate` "replaces" Claude Code's own `git worktree add`. Its
//!   input carries `name`, "a slug identifier for the new worktree". The hook
//!   prints the path "as the last non-empty line of stdout" and sends anything
//!   else to stderr; "any non-zero exit code makes worktree creation fail".
//!   Claude Code "refuses an absolute path that contains `.` or `..` segments,
//!   and any path that passes through a symlink below the repository root".
//! * Claude Code's own default is "`.claude/worktrees/<name>/` at your
//!   repository root, on a new branch named `worktree-<name>`"
//!   (<https://code.claude.com/docs/en/worktrees>). That is the path and the
//!   branch made here, so `engine::worktree::agent_layout` attributes the
//!   result to Claude exactly as before. The repository root is the main
//!   checkout: run live (Claude Code 2.1.319, 2026-11-06), a session started
//!   inside a linked worktree gets its worktree under the main checkout, with
//!   or without this hook, never nested under the linked one.
//! * With the hook configured, "`.worktreeinclude` is not processed", so
//!   GitPulse processes it ([`crate::engine::cow_clone::worktree_include`]).
//! * `WorktreeRemove` carries `worktree_path`. Exit 0 counts as removed;
//!   non-zero fails the removal "if the directory at `worktree_path` still
//!   exists afterward". It does not run for subagent or workflow worktrees,
//!   which Claude Code keeps, and "Claude Code never deletes a branch belonging
//!   to a hook-created worktree", so the hook deletes the `worktree-<name>`
//!   branch it made: with `-d` when it held no work, so git keeps it unless it
//!   is merged, and with `-D` when the removal discarded work.
//! * The `WorktreeRemove` input has no `discard_changes` (that is the
//!   `ExitWorktree` tool's parameter, and Claude Code 2.1.319 builds the hook
//!   payload from `worktree_path` alone). Claude Code confirms with the user
//!   before it removes a worktree that holds work, and removes one silently only
//!   after finding it clean, so every removal that reaches the hook is a
//!   confirmed one: work found is discarded and said on stderr. Only a check
//!   that could not run refuses.
//! * A `post_create` hook that fails leaves the worktree in place, as the
//!   Worktrees panel does; the failure is said on stderr.
//! * The command-hook `timeout` defaults to 600 s. The plugin declares that
//!   number explicitly ([`HOST_TIMEOUT_SECS`]) and the work stops at
//!   [`WORKTREE_BUDGET`], inside it.
//!
//! # Exit codes, and who falls back
//!
//! * [`EXIT_DONE`]: created (the path is the only stdout) or removed.
//! * [`EXIT_FAILED`]: a refusal or a git failure. Creation or removal fails
//!   and no path is printed. Falling back would not help: git itself said no,
//!   a rule fired, the input was unusable, or whether the worktree holds work
//!   could not be checked.
//! * [`EXIT_UNAVAILABLE`]: GitPulse could not do its part and did nothing, so
//!   plain git can still do Claude Code's. The plugin's launcher
//!   (`plugins/gitpulse/hooks/gitpulse-worktree.sh`) then runs the plain
//!   `git worktree add` / `remove` Claude Code would have run. This is how an
//!   **untrusted repository** is handled (the owner's decision: plain git, not
//!   a refusal): GitPulse spawns nothing in a repository it was not asked to
//!   trust (`procguard::spawn` admits no such working directory), so it hands
//!   the job back, untouched, to the launcher. It is also the answer when the
//!   trust store cannot be read or the command gate could not run, and when
//!   the work ran out of time or panicked before its first git write.
//!
//! Every git write made here (the add, the remove, a branch delete, and the
//! undo of a worktree a later GitPulse step refused) is judged first by
//! [`crate::harness::judge_command`], which writes the ledger row before it
//! answers; reads (`status`, `rev-list`, `check-ignore`) are not, as in the
//! app.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::HookInput;
use crate::engine::cow_clone::CacheSelection;
use crate::engine::worktree::{
    add_worktree_argv, add_worktree_with_devmap, remove_worktree, remove_worktree_argv, DevmapMode,
};
use crate::engine::GitWriter;
use crate::harness::PolicyVerdict;

/// The subcommand registered for `WorktreeCreate`.
pub const CREATE: &str = "worktree-create";
/// The subcommand registered for `WorktreeRemove`.
pub const REMOVE: &str = "worktree-remove";

/// Created or removed.
pub const EXIT_DONE: i32 = 0;
/// Refused, or git failed: Claude Code's operation fails, nothing falls back.
pub const EXIT_FAILED: i32 = 1;
/// GitPulse did nothing; plain git may still do the job (`EX_TEMPFAIL`).
pub const EXIT_UNAVAILABLE: i32 = 75;

/// The `timeout` the plugin declares on both worktree hooks: Claude Code's own
/// default for a command hook, written down so it cannot drift silently.
pub const HOST_TIMEOUT_SECS: u64 = 600;

/// Wall-clock ceiling on one worktree hook, from the process's start. Leaves
/// [`super::HOST_TIMEOUT_MARGIN`] and more inside [`HOST_TIMEOUT_SECS`] for
/// printing, reaping children and the launcher's own work after a hand-back.
pub const WORKTREE_BUDGET: Duration = Duration::from_secs(540);

/// The directories, under the repository root, Claude Code puts its
/// worktrees in.
pub const CONTAINER: [&str; 2] = [".claude", "worktrees"];

/// What Claude Code names a worktree's branch: this, then the worktree's name.
pub const BRANCH_PREFIX: &str = "worktree-";

/// The launcher the plugin runs the worktree hooks through, relative to the
/// plugin root (`plugins/gitpulse/`). It holds the plain-git fallback.
pub const LAUNCHER: &str = "hooks/gitpulse-worktree.sh";

/// What the shipped manifest puts in front of `gitpulse-hook <subcommand>` for
/// the worktree events. Every reader of `hooks.json` strips exactly this
/// ([`strip_launcher`]) and then reads the rest as it reads any other entry.
pub const LAUNCHER_PREFIX: &str = "sh \"${CLAUDE_PLUGIN_ROOT}/hooks/gitpulse-worktree.sh\" ";

/// A manifest command without the launcher in front of it, and whether it had
/// one: `sh "…/gitpulse-worktree.sh" gitpulse-hook worktree-create` reads as
/// `gitpulse-hook worktree-create`.
pub fn strip_launcher(command: &str) -> (&str, bool) {
    match command.strip_prefix(LAUNCHER_PREFIX) {
        Some(rest) => (rest, true),
        None => (command, false),
    }
}

/// Longest worktree name accepted. Claude Code's are a few words and a hash.
const MAX_NAME_BYTES: usize = 128;

/// The exit code a panic in this run should produce: [`EXIT_UNAVAILABLE`]
/// until the first git write is about to start, [`EXIT_FAILED`] after, so the
/// launcher never runs plain git over a half-made worktree. Zero outside a
/// worktree hook.
static PANIC_EXIT: AtomicI32 = AtomicI32::new(0);

/// The exit code for a panic, given how far the run got.
pub fn exit_code_after_panic() -> i32 {
    PANIC_EXIT.load(Ordering::SeqCst)
}

/// Called by the binary as soon as it knows it is running a worktree hook,
/// before it reads stdin: from here a panic hands the job back to plain git
/// ([`EXIT_UNAVAILABLE`]) instead of exiting 0 with no path, which Claude Code
/// would read as a failed creation with nothing to fall back to.
pub fn begin() {
    PANIC_EXIT.store(EXIT_UNAVAILABLE, Ordering::SeqCst);
}

/// Records, in the run's own marker, that a git write is about to start.
fn mark_write_started(progress: &AtomicI32) {
    progress.store(EXIT_FAILED, Ordering::SeqCst);
}

/// How one worktree hook ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub exit_code: i32,
    /// The created worktree's path: the only thing that goes to stdout, and
    /// only on [`EXIT_DONE`] from `worktree-create`.
    pub path: Option<String>,
    /// Lines for stderr, in order. Each says whose failure it was.
    pub notes: Vec<String>,
}

impl Finished {
    fn done(path: Option<String>, notes: Vec<String>) -> Self {
        Self {
            exit_code: EXIT_DONE,
            path,
            notes,
        }
    }

    fn failed(mut notes: Vec<String>, line: String) -> Self {
        notes.push(line);
        Self {
            exit_code: EXIT_FAILED,
            path: None,
            notes,
        }
    }

    fn unavailable(mut notes: Vec<String>, line: String) -> Self {
        notes.push(line);
        Self {
            exit_code: EXIT_UNAVAILABLE,
            path: None,
            notes,
        }
    }
}

/// The command gate as these hooks are handed it: the repository and the argv
/// (program first), answering with the recorded verdict.
pub(crate) type Gate<'a> = &'a (dyn Fn(&str, &[&str]) -> Result<PolicyVerdict, String> + Sync);

/// Runs `worktree-create` or `worktree-remove` under [`WORKTREE_BUDGET`].
pub fn run(subcommand: &str, input: &HookInput) -> Finished {
    let budget = super::STARTED.get().map_or(WORKTREE_BUDGET, |started| {
        WORKTREE_BUDGET.saturating_sub(started.elapsed())
    });
    let subcommand = subcommand.to_string();
    let input = input.clone();
    run_within(budget, &PANIC_EXIT, move |progress| {
        let gate: Gate<'_> = &crate::harness::judge_command;
        match subcommand.as_str() {
            CREATE => create(&input, gate, progress),
            REMOVE => remove(&input, gate, progress),
            other => Finished::failed(Vec::new(), format!("{other} is not a worktree hook")),
        }
    })
}

/// `work` on its own thread, abandoned at `budget`. Abandoning it is safe
/// because the process exits right after: `procguard` takes down whatever
/// git or hook child it left running. `progress` is the run's marker of
/// whether a git write has started ([`PANIC_EXIT`] in the binary), which
/// decides whether giving up may hand the job back to plain git.
pub(crate) fn run_within(
    budget: Duration,
    progress: &'static AtomicI32,
    work: impl FnOnce(&AtomicI32) -> Finished + Send + 'static,
) -> Finished {
    progress.store(EXIT_UNAVAILABLE, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    // Supervised: a panic is recorded, and the dropped sender reads below as
    // "no answer", which exits with whatever `progress` says.
    let spawned =
        crate::supervisor::spawn_once("gitpulse-hook-worktree", "hook-worktree", move || {
            let _ = tx.send(work(progress));
        });
    if let Err(error) = spawned {
        return Finished::unavailable(
            Vec::new(),
            format!("GitPulse failure: could not start the worktree hook's worker: {error}"),
        );
    }
    match rx.recv_timeout(budget) {
        Ok(finished) => finished,
        Err(mpsc::RecvTimeoutError::Timeout) => Finished {
            exit_code: progress.load(Ordering::SeqCst),
            path: None,
            notes: vec![format!(
                "GitPulse failure: the worktree hook did not finish within {}s, so no worktree is handed to Claude Code{}",
                budget.as_secs(),
                if progress.load(Ordering::SeqCst) == EXIT_FAILED {
                    "; a git write had started, so anything it left is on disk for you to inspect"
                } else {
                    "; nothing had been written"
                }
            )],
        },
        Err(mpsc::RecvTimeoutError::Disconnected) => Finished {
            exit_code: progress.load(Ordering::SeqCst),
            path: None,
            notes: vec!["GitPulse failure: the worktree hook's worker stopped without an answer".into()],
        },
    }
}

/* ── create ───────────────────────────────────────────────────────────────── */

/// Where a new worktree goes and what it is cut from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatePlan {
    /// The working tree holding the session's `cwd`, canonical.
    pub root: PathBuf,
    /// `<root>/.claude/worktrees/<name>`.
    pub target: PathBuf,
    /// `worktree-<name>`.
    pub branch: String,
    /// The local default branch, or `None` for `HEAD` ([`local_default_base`]).
    pub base: Option<String>,
    /// The repository's common git directory, whose `worktrees/` registry the
    /// new worktree must appear in ([`is_ready`]).
    pub common: PathBuf,
}

pub(crate) fn create(input: &HookInput, gate: Gate<'_>, progress: &AtomicI32) -> Finished {
    let mut notes = Vec::new();
    let name = match validate_name(&input.worktree_name) {
        Ok(name) => name,
        Err(why) => return Finished::failed(notes, format!("refused: {why}")),
    };
    let cwd = session_dir(input);
    let Some(root) = crate::engine::git_cli::find_git_root(&cwd) else {
        return Finished::failed(
            notes,
            format!(
                "refused: {} is not inside a git working tree",
                cwd.display()
            ),
        );
    };
    let root = match root.canonicalize() {
        Ok(root) => root,
        Err(e) => {
            return Finished::failed(
                notes,
                format!("refused: cannot resolve {}: {e}", root.display()),
            )
        }
    };
    // Claude Code's own `--worktree`, started from inside a linked worktree,
    // puts the new one under the repository's main checkout, not under that
    // worktree (Claude Code 2.1.319, run live on 2026-11-06), and sends the
    // main checkout as `cwd`. Anchoring there ourselves keeps that placement
    // whatever `cwd` a payload carries. A repository with no main checkout
    // (a bare one) keeps the working tree the session is in.
    let root = main_checkout(&root).unwrap_or(root);
    if let Some(unavailable) = untrusted(&root) {
        return Finished::unavailable(notes, unavailable);
    }
    let common = match crate::repository_trust::git_directories(&root) {
        Ok((_, common)) => common,
        Err(error) => {
            return Finished::unavailable(
                notes,
                format!(
                    "GitPulse failure: could not find {}'s git directory: {}",
                    root.display(),
                    error.into_message()
                ),
            )
        }
    };
    // Two registrations of this hook (the plugin's and a settings copy) run
    // for one worktree, possibly at once. The second waits for the first and
    // then hands over what it made, instead of failing on "already exists".
    let _one_at_a_time = match crate::engine::agent_bus::exclusive_file_lock(
        &creation_lock(&common, name),
        WORKTREE_BUDGET,
        "worktree creation",
    ) {
        Ok(held) => held,
        Err(error) => {
            return Finished::unavailable(
                notes,
                format!("GitPulse failure: {}", error.into_message()),
            )
        }
    };
    if let Some(path) = already_made(&root, &common, name) {
        notes.push(format!(
            "{} was already made by an earlier run of this hook (is it registered twice, by the plugin and a settings copy?); handing it over unchanged",
            path.display()
        ));
        return Finished::done(path.to_str().map(str::to_string), notes);
    }
    let plan = match plan_create(&root, name) {
        Ok(plan) => plan,
        Err(why) => return Finished::failed(notes, format!("refused: {why}")),
    };
    let Some(target) = plan.target.to_str().map(str::to_string) else {
        return Finished::failed(notes, "refused: the worktree path is not UTF-8".into());
    };
    let Some(root_text) = plan.root.to_str().map(str::to_string) else {
        return Finished::failed(notes, "refused: the repository path is not UTF-8".into());
    };

    let argv = add_worktree_argv(&target, Some(&plan.branch), plan.base.as_deref(), false);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    match gate(&root_text, &refs) {
        Ok(verdict) if verdict.blocks() => {
            return Finished::failed(notes, format!("refused by policy: {}", verdict.refusal()))
        }
        // Recorded as unchecked. A gate that could not run must not cost the
        // agent a worktree plain git would make, and GitPulse will not run its
        // setup (or the post_create hooks it gates) unjudged: plain git it is.
        Ok(verdict) if verdict.gate_failed() => {
            return Finished::unavailable(
                notes,
                format!("GitPulse failure: {}", verdict.gate_failure()),
            )
        }
        Ok(_) => {}
        Err(error) => {
            return Finished::unavailable(
                notes,
                format!("GitPulse failure: the command gate had nothing to judge under: {error}"),
            )
        }
    }

    mark_write_started(progress);
    let selection = CacheSelection::default();
    let created = match add_worktree_with_devmap(
        &root_text,
        &target,
        Some(&plan.branch),
        plan.base.as_deref(),
        false,
        Some(&selection),
        // Approved hooks only: an unapproved or changed file runs nothing and
        // says so in `hook_notice`.
        true,
        // A copy-on-write clone of the anchor's store, rebound to the new
        // worktree. Not the zero-copy pointer: the `devmap` binary an agent's
        // session runs does not read `base_store_ref.json`, so a pointer lane
        // still cold-builds its own index (measured 2026-11-06). Skipped, and
        // said, under low disk.
        DevmapMode::Copy,
    ) {
        Ok(created) => created,
        Err(error) => {
            if is_ready(&plan.target, &plan.common) {
                // Git made it and a GitPulse step after that refused: not a
                // worktree set up the way the repository says it should be.
                let undo = discard(&plan, gate);
                return Finished::failed(
                    notes,
                    format!(
                        "GitPulse failure after git created the worktree: {}; {undo}",
                        error.into_message()
                    ),
                );
            }
            return Finished::failed(notes, format!("git failed: {}", error.into_message()));
        }
    };
    if let Some(notice) = &created.hook_notice {
        notes.push(format!("post_create hooks did not run: {notice}"));
    }
    if let Some(hook_error) = &created.hook_error {
        // Kept, as the Worktrees panel keeps it (`cmd_add_worktree` reports
        // `hook_error` beside the worktree): a hook-made worktree is set up
        // exactly as an app-made one, and plain git would have given Claude
        // Code a worktree here. The task-lane provisioner discards instead
        // because nothing would tell its unattended agent; this session is
        // told, on stderr.
        notes.push(format!(
            "the repository's post_create hook failed: {hook_error}; the worktree is kept, \
             as the Worktrees panel keeps it, but its setup did not finish"
        ));
        // Claude Code shows the session none of this stderr on success; the
        // session's brief carries it instead.
        if let Err(why) = record_setup_failure(&plan.target, hook_error) {
            notes.push(format!("the session will not be told: {why}"));
        }
    }
    if let Some(cache) = &created.cache_error {
        notes.push(format!("build caches: {cache}"));
    }
    if let Some(low) = &created.low_disk {
        notes.push(format!("build caches: {}", low.message()));
    }
    match &created.devmap {
        crate::engine::worktree::DevmapOutcome::Failed { error } => {
            notes.push(format!("DevMap store not seeded: {error}"))
        }
        crate::engine::worktree::DevmapOutcome::Skipped { reason } => {
            notes.push(format!("DevMap store not seeded: {reason}"))
        }
        _ => {}
    }
    if !is_ready(&plan.target, &plan.common) {
        return Finished::failed(
            notes,
            format!("git reported success but {target} is not a ready worktree"),
        );
    }
    notes.push(format!(
        "created {target} on {} from {} through GitPulse",
        plan.branch,
        plan.base.as_deref().unwrap_or("HEAD")
    ));
    Finished::done(Some(target), notes)
}

/// The lock that serializes creations of worktree `name` in one repository:
/// `<common>/gitpulse/worktree-create/<name>.lock`, beside the agent bus's
/// `lanes/`, out of every working tree. Per name, so parallel subagents making
/// different worktrees never wait on each other.
pub(crate) fn creation_lock(common: &Path, name: &str) -> PathBuf {
    common
        .join("gitpulse")
        .join("worktree-create")
        .join(format!("{name}.lock"))
}

/// The worktree a run of this hook already made for `name` under `root`: a
/// registered worktree ([`is_ready`]) at Claude Code's path whose `HEAD` is
/// still `worktree-<name>`. Anything else at that path is not ours to hand
/// over, and [`plan_create`] refuses it.
fn already_made(root: &Path, common: &Path, name: &str) -> Option<PathBuf> {
    let mut target = root.to_path_buf();
    target.extend(CONTAINER);
    target.push(name);
    let gitdir = registered_gitdir(&target, common)?;
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    (head.trim() == format!("ref: refs/heads/{BRANCH_PREFIX}{name}")).then_some(target)
}

/// The file a failed `post_create` leaves in the new worktree's own git
/// directory (beside the task lanes' `gitpulse-setup-complete`), holding the
/// failure. Out of the working tree, so it is never committed and never shows
/// in `git status`, and gone with the worktree.
pub const SETUP_FAILED: &str = "gitpulse-post-create-failed";

/// Most bytes of the failure kept in [`SETUP_FAILED`] and read back.
const MAX_SETUP_FAILED_BYTES: usize = 2000;

/// Why the marker exists: Claude Code shows nothing a `WorktreeCreate` hook
/// writes to stderr when it succeeds (2.1.319 logs only the exit status, even
/// in its debug log), so a session handed a worktree whose setup failed would
/// never learn it. The `SessionStart` brief, which does reach the model, reads
/// the marker back ([`setup_failure`]).
fn record_setup_failure(worktree: &Path, failure: &str) -> Result<(), String> {
    let gitdir = crate::engine::git_cli::resolve_git_dir(worktree).map_err(|e| e.into_message())?;
    let mut text = failure.to_string();
    if text.len() > MAX_SETUP_FAILED_BYTES {
        let mut cut = MAX_SETUP_FAILED_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push('…');
    }
    std::fs::write(gitdir.join(SETUP_FAILED), text)
        .map_err(|e| format!("could not record it for the session ({e})"))
}

/// The line a session brief opens with when the worktree at `root` was handed
/// over with its `post_create` setup failed, `None` otherwise.
pub fn setup_failure(root: &Path) -> Option<String> {
    let gitdir = crate::engine::git_cli::resolve_git_dir(root).ok()?;
    let mut text = std::fs::read_to_string(gitdir.join(SETUP_FAILED)).ok()?;
    if text.len() > MAX_SETUP_FAILED_BYTES + '…'.len_utf8() {
        let mut cut = MAX_SETUP_FAILED_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Some(format!(
        "WARNING: this worktree's setup did not finish. The repository's post_create hook failed \
         when GitPulse created it, so dependencies or other setup it does may be missing: {}",
        text.trim()
    ))
}

/// The `cwd` the payload names, else this process's own.
fn session_dir(input: &HookInput) -> PathBuf {
    if input.cwd.is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        PathBuf::from(&input.cwd)
    }
}

/// `None` when the repository is trusted; otherwise why GitPulse hands the job
/// back to plain git.
fn untrusted(root: &Path) -> Option<String> {
    match crate::repository_trust::require(root) {
        Ok(()) => None,
        Err(error) if crate::repository_trust::refused(error.message()) => Some(format!(
            "{} is not trusted in GitPulse, so GitPulse runs nothing there; plain git will make \
             the worktree, without post_create hooks, cache copies or .worktreeinclude",
            root.display()
        )),
        Err(error) => Some(format!(
            "GitPulse failure: could not read whether {} is trusted: {}",
            root.display(),
            error.into_message()
        )),
    }
}

/// A worktree name Claude Code could have sent: one path segment of letters,
/// digits, `.`, `_` and `-`, not starting with `.` or `-`.
pub(crate) fn validate_name(name: &str) -> Result<&str, String> {
    if name.is_empty() {
        return Err("Claude Code sent no worktree name".into());
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(format!(
            "the worktree name is longer than {MAX_NAME_BYTES} bytes"
        ));
    }
    if name.starts_with('.') || name.starts_with('-') {
        return Err(format!("the worktree name {name:?} starts with '.' or '-'"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "the worktree name {name:?} is not one segment of letters, digits, '.', '_' and '-'"
        ));
    }
    if name.contains("..") || name.ends_with(".lock") || name.ends_with('.') {
        return Err(format!(
            "the worktree name {name:?} cannot name a git branch"
        ));
    }
    Ok(name)
}

/// The path, branch and base for `name` under `root`. Refuses a container
/// directory that is a link (Claude Code refuses such a path anyway) and a
/// target that already exists.
pub(crate) fn plan_create(root: &Path, name: &str) -> Result<CreatePlan, String> {
    let mut container = root.to_path_buf();
    for segment in CONTAINER {
        container.push(segment);
        match container.symlink_metadata() {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{} is not a directory (Claude Code refuses a worktree path through a link inside the repository)",
                    container.display()
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot read {}: {e}", container.display())),
        }
    }
    let target = container.join(name);
    if target.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", target.display()));
    }
    let branch = format!("{BRANCH_PREFIX}{name}");
    crate::engine::git_writer::validate_ref_name(&branch).map_err(|e| e.into_message())?;
    let (_, common) =
        crate::repository_trust::git_directories(root).map_err(|e| e.into_message())?;
    Ok(CreatePlan {
        root: root.to_path_buf(),
        target,
        branch,
        base: local_default_base(root),
        common,
    })
}

/// What a hook-created worktree branches from: the repository's default
/// branch **as it is locally** (`main`, not `origin/main`), so work committed
/// to it but not yet pushed is in the new worktree. Claude Code's own default
/// is the remote's (`origin/HEAD`), which an earlier session saw 76 commits
/// behind the local branch. The name is [`GitReader::default_branch_name`]'s
/// (the primary remote's HEAD, else `main`, `master`, `trunk`, `develop`);
/// `None`, meaning `HEAD`, when that branch does not exist locally or nothing
/// resolves.
///
/// [`GitReader::default_branch_name`]: crate::engine::git_reader::GitReader::default_branch_name
pub(crate) fn local_default_base(root: &Path) -> Option<String> {
    let root_text = root.to_str()?;
    let name = crate::engine::git_reader::GitReader::default_branch_name(root_text)
        .ok()
        .flatten()?;
    let local = format!("refs/heads/{name}");
    let run = crate::engine::git_cli::git_captured(
        root,
        &["show-ref", "--verify", "--quiet", local.as_str()],
    )
    .ok()?;
    (run.status_code == 0).then_some(name)
}

/// Whether `target` is a linked worktree git finished writing: what a path
/// handed to Claude Code must be. Its `.git` file names a git directory in
/// `common`'s `worktrees/` that names `target` back and holds a `HEAD`, and
/// `target` is reached without a link. Read from the filesystem, not by running
/// git in it: a repository approved before approvals covered the whole family
/// does not admit a child process in the new worktree, and that must not turn
/// a worktree git made into one reported as missing.
fn is_ready(target: &Path, common: &Path) -> bool {
    registered_gitdir(target, common).is_some()
}

/// The registry entry (`<common>/worktrees/<id>`) that [`is_ready`] matches to
/// `target`, or `None` when there is none.
fn registered_gitdir(target: &Path, common: &Path) -> Option<PathBuf> {
    let canonical = target.canonicalize().ok()?;
    if canonical != target {
        return None;
    }
    let gitfile = std::fs::read_to_string(target.join(".git")).ok()?;
    let gitdir = gitfile.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = target.join(gitdir).canonicalize().ok()?;
    let registry = common.join("worktrees").canonicalize().ok()?;
    if gitdir.parent() != Some(registry.as_path()) || !gitdir.join("HEAD").is_file() {
        return None;
    }
    let back = std::fs::read_to_string(gitdir.join("gitdir")).ok()?;
    let back = Path::new(back.trim()).parent()?.canonicalize().ok()?;
    (back == canonical).then_some(gitdir)
}

/// Takes away a worktree this run made and could not set up: the directory,
/// then the branch it created (which has no commits of its own yet). Each
/// write is judged; whatever could not be undone is said.
fn discard(plan: &CreatePlan, gate: Gate<'_>) -> String {
    let (Some(root), Some(target)) = (plan.root.to_str(), plan.target.to_str()) else {
        return "it could not be removed (its path is not UTF-8)".into();
    };
    let argv = remove_worktree_argv(target, true);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    if let Some(refused) = refusal(gate(root, &refs)) {
        return format!(
            "it is left at {target} on {}: removing it was refused: {refused}",
            plan.branch
        );
    }
    if let Err(error) = remove_worktree(root, target, true) {
        return format!(
            "it is left at {target} on {}: removing it failed: {}",
            plan.branch,
            error.into_message()
        );
    }
    match GitWriter::delete_branch_argv(&plan.branch, true) {
        Ok(argv) => {
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            if let Some(refused) = refusal(gate(root, &refs)) {
                return format!(
                    "the worktree was removed; branch {} is kept: {refused}",
                    plan.branch
                );
            }
            match GitWriter::delete_branch(root, &plan.branch, true) {
                Ok(_) => "the worktree and its branch were removed".into(),
                Err(error) => format!(
                    "the worktree was removed; branch {} is kept: {}",
                    plan.branch,
                    error.into_message()
                ),
            }
        }
        Err(error) => format!(
            "the worktree was removed; branch {} is kept: {}",
            plan.branch,
            error.into_message()
        ),
    }
}

/// Why a write may not go ahead: a rule fired, the gate could not run, or
/// there was nothing to judge under. `None` when it may.
fn refusal(verdict: Result<PolicyVerdict, String>) -> Option<String> {
    match verdict {
        Ok(verdict) if verdict.blocks() => Some(verdict.refusal()),
        Ok(verdict) if verdict.gate_failed() => Some(verdict.gate_failure()),
        Ok(_) => None,
        Err(error) => Some(error),
    }
}

/* ── remove ───────────────────────────────────────────────────────────────── */

pub(crate) fn remove(input: &HookInput, gate: Gate<'_>, progress: &AtomicI32) -> Finished {
    let mut notes = Vec::new();
    let path = PathBuf::from(&input.worktree_path);
    if input.worktree_path.is_empty() || !path.is_absolute() {
        return Finished::failed(
            notes,
            format!(
                "refused: Claude Code sent no absolute worktree_path ({:?})",
                input.worktree_path
            ),
        );
    }
    if path
        .components()
        .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Finished::failed(
            notes,
            format!("refused: {} has . or .. segments", path.display()),
        );
    }
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Nothing on disk to protect. Git's record of it, if any, is what
            // `git worktree prune` (and GitPulse's prune) clear.
            notes.push(format!("{} is already gone", path.display()));
            return Finished::done(None, notes);
        }
        Err(e) => {
            return Finished::failed(
                notes,
                format!("refused: cannot read {}: {e}", path.display()),
            )
        }
        Ok(meta) if !meta.file_type().is_dir() => {
            return Finished::failed(
                notes,
                format!("refused: {} is not a directory", path.display()),
            )
        }
        Ok(_) => {}
    }
    let worktree = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return Finished::failed(
                notes,
                format!("refused: cannot resolve {}: {e}", path.display()),
            )
        }
    };
    if crate::engine::git_cli::find_git_root(&worktree).as_deref() != Some(worktree.as_path()) {
        return Finished::failed(
            notes,
            format!(
                "refused: {} is not the top of a git worktree",
                worktree.display()
            ),
        );
    }
    if let Some(unavailable) = untrusted(&worktree) {
        return Finished::unavailable(notes, unavailable);
    }
    let main = match main_checkout(&worktree) {
        Ok(main) => main,
        Err(why) => return Finished::failed(notes, format!("refused: {why}")),
    };
    if main == worktree {
        return Finished::failed(
            notes,
            format!(
                "refused: {} is the repository's main checkout, not a linked worktree",
                worktree.display()
            ),
        );
    }
    let branch = current_branch(&worktree);
    // Claude Code asks before it removes a worktree with work in it (the exit
    // prompt's "Remove worktree" warns that uncommitted changes and commits
    // only on its branch will be lost; `ExitWorktree` refuses one unless told
    // to discard), and removes one silently only after finding it clean. It
    // never sends `discard_changes` here. So a removal that reaches this hook
    // has been confirmed: work found is discarded, with `--force` and `-D`,
    // and said. A check that could not run is still a refusal, because then
    // nobody knows what would be lost.
    let work = if input.discard_changes {
        None
    } else {
        match work_held(&worktree, branch.as_deref()) {
            Ok(work) => work,
            Err(why) => {
                return Finished::failed(
                    notes,
                    format!(
                        "refused: could not tell whether it holds work ({why}). The worktree is kept; \
                         remove it yourself with `git worktree remove --force {}` once you have checked",
                        worktree.display()
                    ),
                )
            }
        }
    };
    let (Some(main_text), Some(worktree_text)) = (main.to_str(), worktree.to_str()) else {
        return Finished::failed(notes, "refused: a path is not UTF-8".into());
    };
    if let Some(work) = &work {
        notes.push(format!(
            "{work}; discarded, as Claude Code confirmed the removal"
        ));
    }
    let force = input.discard_changes || work.is_some();
    let argv = remove_worktree_argv(worktree_text, force);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    if let Some(refused) = refusal(gate(main_text, &refs)) {
        // Unlike a creation, a removal that does not happen loses nothing.
        return Finished::failed(notes, format!("refused: {refused}"));
    }
    mark_write_started(progress);
    match remove_worktree(main_text, worktree_text, force) {
        Ok(removal) => {
            if let Some(cleanup) = removal.cleanup_error {
                notes.push(cleanup);
            }
            notes.extend(removal.sweep_problems);
        }
        Err(error) => {
            return Finished::failed(notes, format!("git failed: {}", error.into_message()))
        }
    }
    if let Some(branch) = branch.filter(|b| made_by_claude(&worktree, b)) {
        notes.push(delete_branch(main_text, &branch, force, gate));
    }
    notes.push(format!("removed {worktree_text} through GitPulse"));
    Finished::done(None, notes)
}

/// The main checkout of the repository `worktree` belongs to: the directory
/// holding its common `.git`.
fn main_checkout(worktree: &Path) -> Result<PathBuf, String> {
    let (_, common) =
        crate::repository_trust::git_directories(worktree).map_err(|e| e.into_message())?;
    let common = common
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", common.display()))?;
    if common.file_name().and_then(|n| n.to_str()) != Some(".git") {
        return Err(format!(
            "{} is not a .git directory inside a checkout (a bare repository?)",
            common.display()
        ));
    }
    common
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no parent", common.display()))
}

/// The branch checked out in `worktree`, `None` when detached.
fn current_branch(worktree: &Path) -> Option<String> {
    crate::engine::git_cli::git_text(worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
}

/// The work removing `worktree` would discard: uncommitted or untracked files,
/// or commits only this worktree's branch (or detached `HEAD`) reaches, which
/// are neither pushed nor merged anywhere. `Ok(None)` when there is none.
/// Ignored files are not work (`git worktree remove` deletes them too). `Err`
/// when the check could not run, which the caller treats as a refusal, never
/// as "nothing to lose".
pub(crate) fn work_held(worktree: &Path, branch: Option<&str>) -> Result<Option<String>, String> {
    let status = crate::engine::git_cli::git_text(
        worktree,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )
    .map_err(|e| format!("could not read its status: {}", e.into_message()))?;
    let dirty: Vec<&str> = status.lines().filter(|l| !l.trim().is_empty()).collect();
    if !dirty.is_empty() {
        let shown: Vec<&str> = dirty.iter().take(5).copied().collect();
        return Ok(Some(format!(
            "it has {} uncommitted or untracked file(s) ({}{})",
            dirty.len(),
            shown.join(", "),
            if dirty.len() > shown.len() {
                ", …"
            } else {
                ""
            }
        )));
    }
    let exclude;
    let mut args = vec!["rev-list", "--count", "HEAD", "--not"];
    if let Some(branch) = branch {
        // `--exclude` takes a glob; a name with glob characters cannot be
        // excluded exactly, and including it would hide every commit on it.
        if branch.contains(['*', '?', '[', '\\']) {
            return Err(format!(
                "its branch {branch:?} cannot be checked exactly for unpushed commits"
            ));
        }
        exclude = format!("--exclude={branch}");
        args.push(exclude.as_str());
    }
    args.extend(["--branches", "--remotes"]);
    let count = crate::engine::git_cli::git_text(worktree, &args)
        .map_err(|e| format!("could not count its unpushed commits: {}", e.into_message()))?;
    match count.trim().parse::<u64>() {
        Ok(0) => Ok(None),
        Ok(n) => Ok(Some(format!(
            "{n} commit(s) on {} are on no other branch and no remote (unpushed and unmerged)",
            branch.unwrap_or("its detached HEAD")
        ))),
        Err(_) => Err(format!("git rev-list answered {count:?}, not a count")),
    }
}

/// Whether `branch` is the one a `WorktreeCreate` made for this worktree:
/// `worktree-<name>`, checked out at a path ending `.claude/worktrees/<name>`.
///
/// Read from the end of the path, not with `agent_layout`, whose outermost
/// match is the right answer for attribution and the wrong one here. The hook
/// never nests a worktree (it anchors at the main checkout), but a removal can
/// still be asked for one made some other way under another agent's layout
/// (`<lane>/.claude/worktrees/<name>`), and its branch is named after the
/// innermost segment.
pub(crate) fn made_by_claude(worktree: &Path, branch: &str) -> bool {
    let segments: Vec<&str> = worktree.iter().filter_map(|s| s.to_str()).collect();
    match segments.as_slice() {
        [.., claude, worktrees, name] if *claude == CONTAINER[0] && *worktrees == CONTAINER[1] => {
            branch == format!("{BRANCH_PREFIX}{name}")
        }
        _ => false,
    }
}

/// Deletes the branch a removed hook worktree was on, `-d` unless the removal
/// discarded changes. Git refusing (`not fully merged`) keeps it, and the line
/// says so: the worktree is already gone and its commits are safe.
fn delete_branch(main: &str, branch: &str, force: bool, gate: Gate<'_>) -> String {
    let argv = match GitWriter::delete_branch_argv(branch, force) {
        Ok(argv) => argv,
        Err(error) => return format!("branch {branch} is kept: {}", error.into_message()),
    };
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    if let Some(refused) = refusal(gate(main, &refs)) {
        return format!("branch {branch} is kept: {refused}");
    }
    match GitWriter::delete_branch(main, branch, force) {
        Ok(_) => format!("deleted branch {branch}"),
        Err(error) => format!("branch {branch} is kept: {}", error.into_message()),
    }
}

#[cfg(test)]
mod tests;
