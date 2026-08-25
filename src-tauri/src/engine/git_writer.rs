use crate::engine::git_cli::{
    git_global_with_timeout, git_text, git_text_network, git_with_stdin, resolve_git_common_dir,
    sandbox_join, validate_repo, NETWORK_TIMEOUT,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) fn repo_mutation_lock(canon: &Path) -> Arc<Mutex<()>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let key = resolve_git_common_dir(canon).unwrap_or_else(|_| canon.to_path_buf());
    let mut map = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

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

/// One command in a planned [`GitWriter::execute_rebase_sequence`] run.
///
/// The gate must judge every command line before the first mutation happens,
/// so the whole sequence is composed up front as these commands and both the
/// policy layer and the executor consume the same list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebasePlanCommand {
    /// Full argv including the program name; element 0 is always "git".
    pub argv: Vec<String>,
    /// When set, an execution failure is reported as
    /// `Rebase step failed ({label}): {error}` — the historical wording for
    /// cherry-pick steps. `None` propagates the git error unchanged.
    pub failure_label: Option<String>,
}

/// Runs a builder-produced argv in `repo`: element 0 is the program name
/// ("git"), elements 1.. are the subcommand and its arguments. Every executor
/// that gates through an argv builder runs its git call through here, so the
/// gated line and the executed line cannot drift apart.
fn run_argv(repo: &Path, argv: &[String]) -> Result<String, String> {
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    git_text(repo, &args)
}

/// Like [`run_argv`], but under [`crate::engine::git_cli::NETWORK_TIMEOUT`]:
/// clone/fetch/pull/push transfer real data over the network where the 90s
/// default cap kills healthy multi-gigabyte transfers mid-flight.
fn run_network_argv(repo: &Path, argv: &[String]) -> Result<String, String> {
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    git_text_network(repo, &args)
}

/// Builds one full argv from a program name plus argument literals.
fn argv0(program: &str, args: &[&str]) -> Vec<String> {
    std::iter::once(program.to_string())
        .chain(args.iter().map(|s| s.to_string()))
        .collect()
}

/// The exact argv [`GitWriter::push`] executes. Authoritative for the command
/// gate: force pushes run `--force-with-lease` (never bare `--force`), so
/// this — not the caller's assumption — is what gets judged.
pub fn push_argv(remote: Option<&str>, branch: Option<&str>, force: bool) -> Vec<String> {
    let mut argv = vec!["git".to_string(), "push".to_string()];
    if force {
        argv.push("--force-with-lease".to_string());
    }
    if let Some(r) = remote {
        argv.push(r.to_string());
    }
    if let Some(b) = branch {
        argv.push(b.to_string());
    }
    argv
}

/// The exact argv [`GitWriter::pull`] executes.
pub fn pull_argv(remote: Option<&str>, branch: Option<&str>) -> Vec<String> {
    let mut argv = vec!["git".to_string(), "pull".to_string()];
    if let Some(r) = remote {
        argv.push(r.to_string());
    }
    if let Some(b) = branch {
        argv.push(b.to_string());
    }
    argv
}

/// The exact argv [`GitWriter::merge_branch`] executes, including the
/// `--no-edit` flag the writer always adds to keep merges non-interactive.
pub fn merge_argv(branch_name: &str, ff_only: bool) -> Vec<String> {
    let mut argv = vec!["git".to_string(), "merge".to_string()];
    if ff_only {
        argv.push("--ff-only".to_string());
    }
    argv.push("--no-edit".to_string());
    argv.push(branch_name.to_string());
    argv
}

/// The exact argv [`commit_inner`] executes for [`GitWriter::commit`],
/// including the empty-message amend form (`--amend --no-edit`) used by
/// internal squash flows.
pub fn commit_argv(message: &str, amend: bool) -> Vec<String> {
    let mut argv = vec!["git".to_string(), "commit".to_string()];
    if amend && message.is_empty() {
        argv.push("--amend".to_string());
        argv.push("--no-edit".to_string());
    } else {
        argv.push("-m".to_string());
        argv.push(message.to_string());
        if amend {
            argv.push("--amend".to_string());
        }
    }
    argv
}

/// The exact argv [`GitWriter::restack`] executes: rebase the branch onto the
/// upstream tip (`rebase --onto <upstream> <upstream> <branch>`).
/// The exact argv [`GitWriter::create_branch`] executes. Fallible because ref
/// validation must reject a malformed name before the policy layer is asked
/// to judge it.
pub fn create_branch_argv(
    branch_name: &str,
    start_point: Option<&str>,
) -> Result<Vec<String>, String> {
    validate_ref_name(branch_name)?;
    let mut argv = vec![
        "git".to_string(),
        "branch".to_string(),
        branch_name.to_string(),
    ];
    if let Some(sp) = start_point {
        // Start points are revisions, not refs-to-create: HEAD~1 and raw
        // oids are legal here. Reflog/peel grammar stays excluded by
        // validate_oid_or_revision on purpose.
        validate_oid_or_revision(sp)?;
        argv.push(sp.to_string());
    }
    Ok(argv)
}

/// The exact argvs [`GitWriter::checkout_branch`] may execute, in attempt
/// order. All of them are gated up front because any one could be the one
/// that runs.
///
/// Two strategies, no more: `switch --guess` covers creating a local branch
/// from a remote twin (git >= 2.23), plain `checkout` covers everything
/// older. A former middle attempt (`checkout --guess`) added a third
/// executable spelling without covering any state the other two miss — and
/// every extra strategy is another argv the policy gate must judge
/// identically.
pub fn checkout_branch_attempts(branch_name: &str) -> Vec<Vec<String>> {
    [
        vec!["switch", "--guess", branch_name],
        vec!["checkout", branch_name],
    ]
    .iter()
    .map(|args| argv0("git", args))
    .collect()
}

/// The exact argv [`GitWriter::stash_save`] executes.
pub fn stash_save_argv(message: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "git".to_string(),
        "stash".to_string(),
        "push".to_string(),
        "-u".to_string(),
    ];
    if let Some(msg) = message {
        argv.push("-m".to_string());
        argv.push(msg.to_string());
    }
    argv
}

/// The exact argv [`GitWriter::stash_pop`] executes.
pub fn stash_pop_argv() -> Vec<String> {
    argv0("git", &["stash", "pop"])
}

/// The exact argv [`GitWriter::create_tag`] executes: annotated when a
/// message is given, lightweight otherwise, with the optional commit id last.
pub fn create_tag_argv(
    tag_name: &str,
    commit_id: Option<&str>,
    message: Option<&str>,
) -> Vec<String> {
    let mut argv = vec!["git".to_string(), "tag".to_string()];
    match message {
        Some(msg) => {
            argv.push("-a".to_string());
            argv.push(tag_name.to_string());
            argv.push("-m".to_string());
            argv.push(msg.to_string());
        }
        None => argv.push(tag_name.to_string()),
    }
    if let Some(cid) = commit_id {
        argv.push(cid.to_string());
    }
    argv
}

/// The exact argv [`GitWriter::delete_tag`] executes.
pub fn delete_tag_argv(tag_name: &str) -> Vec<String> {
    argv0("git", &["tag", "-d", tag_name])
}

/// The exact argv [`GitWriter::push_tag`] executes: a fully-qualified tag
/// refspec so an ambiguous name can never publish the wrong object.
pub fn push_tag_argv(remote: &str, tag: &str) -> Vec<String> {
    argv0("git", &["push", remote, &format!("refs/tags/{tag}")])
}

/// Composes every mutating command [`GitWriter::execute_rebase_sequence`]
/// will run for `(onto_commit, steps, original_branch)`, in execution order:
/// detach onto the target, then the per-step cherry-pick/expand section, then
/// (when the session started on a branch) the finalize moves. Read-only
/// probes (status, rev-parse, symbolic-ref) are not mutations and stay out.
///
/// This is the single source of truth shared by the command gate (which
/// judges each argv before anything runs) and the executor (which runs this
/// list verbatim), so gating a plan and executing it can never disagree.
pub fn rebase_sequence_plan(
    onto_commit: &str,
    steps: &[RebaseStep],
    original_branch: Option<&str>,
) -> Result<Vec<RebasePlanCommand>, String> {
    validate_oid_or_revision(onto_commit)?;
    if steps.is_empty() {
        return Err("Rebase sequence is empty".into());
    }
    // Squash/fixup fold into a previous commit; with no step before them
    // there is nothing to combine into.
    if let Some(first) = steps.first() {
        if matches!(
            first.action,
            RebaseActionKind::Squash | RebaseActionKind::Fixup
        ) {
            return Err(format!(
                "Cannot '{}' commit {} without a previous commit to combine into",
                match first.action {
                    RebaseActionKind::Squash => "squash",
                    _ => "fixup",
                },
                first.commit_id
            ));
        }
    }

    let mut plan = Vec::new();
    plan.push(RebasePlanCommand {
        argv: argv0("git", &["checkout", "--detach", onto_commit]),
        failure_label: None,
    });

    for step in steps {
        validate_oid_or_revision(&step.commit_id)?;
        match &step.action {
            RebaseActionKind::Pick => plan.push(RebasePlanCommand {
                argv: argv0("git", &["cherry-pick", &step.commit_id]),
                failure_label: Some(format!("pick {}", step.commit_id)),
            }),
            RebaseActionKind::Squash => {
                plan.push(RebasePlanCommand {
                    argv: argv0("git", &["cherry-pick", "-n", &step.commit_id]),
                    failure_label: Some(format!("squash {}", step.commit_id)),
                });
                plan.push(RebasePlanCommand {
                    // commit_inner("", true): fold into HEAD keeping its message.
                    argv: argv0("git", &["commit", "--amend", "--no-edit"]),
                    failure_label: None,
                });
            }
            RebaseActionKind::Fixup => {
                plan.push(RebasePlanCommand {
                    argv: argv0("git", &["cherry-pick", "-n", &step.commit_id]),
                    failure_label: Some(format!("fixup {}", step.commit_id)),
                });
                plan.push(RebasePlanCommand {
                    argv: argv0("git", &["commit", "--amend", "--no-edit"]),
                    failure_label: None,
                });
            }
            RebaseActionKind::Drop => {}
            RebaseActionKind::Reword(new_msg) => {
                plan.push(RebasePlanCommand {
                    argv: argv0("git", &["cherry-pick", &step.commit_id]),
                    failure_label: Some(format!("reword {}", step.commit_id)),
                });
                plan.push(RebasePlanCommand {
                    argv: argv0("git", &["commit", "-m", new_msg, "--amend"]),
                    failure_label: None,
                });
            }
        }
    }

    if let Some(branch) = original_branch {
        validate_ref_name(branch)?;
        plan.push(RebasePlanCommand {
            argv: argv0("git", &["branch", "-f", branch, "HEAD"]),
            failure_label: None,
        });
        plan.push(RebasePlanCommand {
            argv: argv0("git", &["checkout", branch]),
            failure_label: None,
        });
    }
    Ok(plan)
}

pub struct GitWriter;

impl GitWriter {
    pub fn stage_file(repo_path: &str, file_path: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = sandbox_join(&repo, file_path)?;
        // `:(literal)` disables pathspec globbing so a user path can never
        // widen its own blast radius ("*" must match a file named "*", never
        // the whole tree). Same convention as the reader side.
        git_text(&repo, &["add", "--", &format!(":(literal){file_path}")])?;
        Ok(())
    }

    pub fn unstage_file(repo_path: &str, file_path: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = sandbox_join(&repo, file_path)?;
        git_text(
            &repo,
            &[
                "restore",
                "--staged",
                "--",
                &format!(":(literal){file_path}"),
            ],
        )?;
        Ok(())
    }

    pub fn commit(repo_path: &str, message: &str, amend: bool) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if message.trim().is_empty() {
            return Err("Commit message must not be empty".into());
        }
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::commit_inner(&repo, message, amend)
    }

    fn commit_inner(repo: &Path, message: &str, amend: bool) -> Result<String, String> {
        let argv = commit_argv(message, amend);
        run_argv(repo, &argv)
    }

    /// Stage exactly `files` and commit them under a single mutation-lock
    /// acquisition.
    ///
    /// `stage_file()` followed by `commit()` spans two lock acquisitions, so
    /// a concurrent writer can commit the shared index in between and absorb
    /// the staged bytes into its own commit. Programmatic callers
    /// (automation, agents, batch tooling) need stage+commit to be one
    /// indivisible mutation; this is that primitive. Interactive flows that
    /// intentionally commit whatever the user staged keep using
    /// `stage_file()` + `commit()`.
    pub fn commit_files(
        repo_path: &str,
        message: &str,
        files: &[String],
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        if message.trim().is_empty() {
            return Err("Commit message must not be empty".into());
        }
        if files.is_empty() {
            return Err("commit_files requires at least one path".into());
        }
        for file in files {
            if file.is_empty() {
                return Err("commit_files requires non-empty paths".into());
            }
            sandbox_join(&repo, file)?;
        }
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut add_args: Vec<&str> = Vec::with_capacity(files.len() + 2);
        add_args.push("add");
        add_args.push("--");
        add_args.extend(files.iter().map(String::as_str));
        git_text(&repo, &add_args)?;
        Self::commit_inner(&repo, message, false)
    }

    pub fn checkout_branch(repo_path: &str, branch_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        let mut first_err = None;
        for attempt in checkout_branch_attempts(branch_name) {
            match run_argv(&repo, &attempt) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        // Unreachable while the builder produces at least one attempt; a
        // fallback string keeps this total without inventing success.
        Err(first_err.unwrap_or_else(|| "checkout failed".into()))
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
        let argv = create_branch_argv(branch_name, start_point)?;
        run_argv(&repo, &argv)?;
        Ok(())
    }

    pub fn delete_branch(repo_path: &str, branch_name: &str, force: bool) -> Result<(), String> {
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
        let flag = if force { "-D" } else { "-d" };
        git_text(&repo, &["branch", flag, branch_name])?;
        Ok(())
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

    /// Read-only probe used by callers that need to plan a rebase sequence
    /// (and gate it) before executing: the branch HEAD is on, if any. The
    /// result feeds both [`rebase_sequence_plan`] and
    /// [`GitWriter::execute_rebase_sequence`] so the gated plan and the
    /// executed plan are built from the same inputs.
    pub fn rebase_original_branch(repo_path: &str) -> Result<Option<String>, String> {
        let repo = validate_repo(repo_path)?;
        Ok(
            git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        )
    }

    /// Executes a rebase sequence, discovering the checked-out branch itself.
    /// Convenience form for callers that have not gated a pre-built plan;
    /// policy-gated flows should discover the branch once via
    /// [`GitWriter::rebase_original_branch`], gate
    /// [`rebase_sequence_plan`]'s output, then call
    /// [`GitWriter::execute_rebase_sequence_for_branch`] with the same value.
    pub fn execute_rebase_sequence(
        repo_path: &str,
        onto_commit: &str,
        steps: &[RebaseStep],
    ) -> Result<(), String> {
        let original_branch = Self::rebase_original_branch(repo_path)?;
        Self::execute_rebase_sequence_for_branch(
            repo_path,
            onto_commit,
            steps,
            original_branch.as_deref(),
        )
    }

    /// Executes a validated rebase sequence: detach onto `onto_commit`,
    /// replay `steps` via cherry-picks, then move `original_branch` to the
    /// result and check it back out.
    ///
    /// `original_branch` must be the value [`rebase_original_branch`]
    /// returned when the caller planned (and gated) the run; the executor
    /// replays exactly that plan rather than rediscovering state mid-flight,
    /// so what was judged is what runs. Every argv comes from
    /// [`rebase_sequence_plan`] — this body only sequences them and handles
    /// failure rollback (`cherry-pick --abort`, forced restore).
    pub fn execute_rebase_sequence_for_branch(
        repo_path: &str,
        onto_commit: &str,
        steps: &[RebaseStep],
        original_branch: Option<&str>,
    ) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Reword amends with the composed message (new subject over the
        // original body), fetched here because the plan builder is pure.
        // This is the same read-only probe the command gate runs when it
        // plans, so what was judged is what runs.
        let mut prepared: Vec<RebaseStep> = Vec::with_capacity(steps.len());
        for step in steps {
            match &step.action {
                RebaseActionKind::Reword(new_subject) => {
                    let original = git_text(&repo, &["log", "-1", "--format=%B", &step.commit_id])?;
                    prepared.push(RebaseStep {
                        commit_id: step.commit_id.clone(),
                        action: RebaseActionKind::Reword(reworded_message(&original, new_subject)),
                    });
                }
                _ => prepared.push(step.clone()),
            }
        }
        let plan = rebase_sequence_plan(onto_commit, &prepared, original_branch)?;

        let dirty = git_text(&repo, &["status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(
                "Working tree has uncommitted changes; commit or stash before rebasing".into(),
            );
        }

        let original_head = git_text(&repo, &["rev-parse", "HEAD"])?.trim().to_string();
        // A step whose commit is not reachable from the HEAD being rebased
        // would transplant a foreign commit onto the new base; refuse before
        // any state is touched. Read-only probes stay out of the gated plan.
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

        let restore = |repo: &std::path::Path| {
            if let Some(branch) = original_branch {
                let _ = git_text(repo, &["checkout", "-f", branch]);
            } else {
                let _ = git_text(repo, &["checkout", "-f", &original_head]);
            }
        };

        // Phase 1: detach onto the target. Nothing has mutated yet, so a
        // failure here propagates without an abort/restore pass.
        let (detach, rest) = plan.split_first().ok_or("Rebase plan is empty")?;
        run_argv(&repo, &detach.argv)?;

        // Phase 2: the cherry-pick section. Any failure aborts the pick and
        // restores the starting point; labeled steps report their historical
        // "Rebase step failed (...)" wording.
        let finalize_len = usize::from(original_branch.is_some());
        let (body, finalize) = rest.split_at(rest.len() - finalize_len);
        let result = (|| -> Result<(), String> {
            for cmd in body {
                if let Err(e) = run_argv(&repo, &cmd.argv) {
                    return match &cmd.failure_label {
                        Some(label) => Err(format!("Rebase step failed ({label}): {e}")),
                        None => Err(e),
                    };
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            let _ = git_text(&repo, &["cherry-pick", "--abort"]);
            restore(&repo);
            return Err(e);
        }

        // Phase 3: finalize — repoint the branch and check it back out. The
        // rebase is durably applied once `branch -f` succeeded; only the
        // working-tree checkout remains after that, and its retry reruns the
        // same gated argv.
        for cmd in finalize {
            if let Err(e) = run_argv(&repo, &cmd.argv) {
                restore(&repo);
                return Err(e);
            }
        }
        Ok(())
    }

    pub fn fetch(repo_path: &str, remote: Option<&str>) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Fetch only moves remote-tracking refs, but a fetch of a large
        // remote is still a long transfer: the 90s default cap would kill
        // healthy traffic mid-flight, so network work gets NETWORK_TIMEOUT.
        if let Some(r) = remote {
            validate_ref_name(r)?;
            git_text_network(&repo, &["fetch", r])
        } else {
            git_text_network(&repo, &["fetch", "--all", "--prune"])
        }
    }

    pub fn pull(
        repo_path: &str,
        remote: Option<&str>,
        branch: Option<&str>,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(r) = remote {
            validate_ref_name(r)?;
        }
        if let Some(b) = branch {
            validate_ref_name(b)?;
        }
        // Pull fetches before it merges; the fetch leg is the long pole and
        // inherits DEFAULT_TIMEOUT's mid-transfer kill otherwise.
        run_network_argv(&repo, &pull_argv(remote, branch))
    }

    pub fn push(
        repo_path: &str,
        remote: Option<&str>,
        branch: Option<&str>,
        force: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(r) = remote {
            validate_ref_name(r)?;
        }
        if let Some(b) = branch {
            validate_ref_name(b)?;
        }
        // The argv comes from the shared builder so the command gate judges
        // exactly this line (--force-with-lease semantics included).
        run_network_argv(&repo, &push_argv(remote, branch, force))
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
        // Same network class as `push`: the gate judges this exact line via
        // [`push_tag_argv`] in cmd_publish_release.
        run_network_argv(&repo, &push_tag_argv(remote, tag))
    }

    pub fn merge_branch(
        repo_path: &str,
        branch_name: &str,
        ff_only: bool,
    ) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        // The builder owns the flags (--no-edit keeps merges non-interactive)
        // so the gate sees exactly this command line.
        run_argv(&repo, &merge_argv(branch_name, ff_only))
    }

    pub fn restack(repo_path: &str, branch: &str, onto: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch)?;
        validate_ref_name(onto)?;
        // The upstream must be where `branch` forked, not the new base itself:
        // after the parent branch was rewritten, `onto..branch` still contains
        // the stale pre-image commits and replaying them conflicts. --fork-point
        // recovers the pre-rewrite fork from the reflog; plain merge-base covers
        // reflog-less clones; unrelated histories fall back to the old behavior.
        // The command gate resolves the same upstream (restack_planned_argv)
        // so what was judged is what runs.
        let upstream = [
            vec!["merge-base", "--fork-point", onto, branch],
            vec!["merge-base", onto, branch],
        ]
        .into_iter()
        .find_map(|argv| {
            git_text(&repo, &argv)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| onto.to_string());
        git_text(
            &repo,
            &["rebase", "--onto", onto, upstream.as_str(), branch],
        )
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
        if let Some(cid) = commit_id {
            validate_oid(cid)?;
        }
        run_argv(&repo, &create_tag_argv(tag_name, commit_id, message))?;
        Ok(())
    }

    pub fn delete_tag(repo_path: &str, tag_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(tag_name)?;
        run_argv(&repo, &delete_tag_argv(tag_name))?;
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
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        run_argv(&repo, &stash_save_argv(message))
    }

    pub fn stash_pop(repo_path: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        run_argv(&repo, &stash_pop_argv())
    }

    pub fn clone_repo(url: &str, target_dir: &str) -> Result<String, String> {
        validate_clone_url(url)?;
        let requested = Path::new(target_dir);
        let dest = resolve_clone_destination(requested)?;
        if dest.join(".git").exists() {
            return Err("Destination is already a Git repository".into());
        }
        let clone_path = if dest.is_dir() {
            resolve_clone_destination(&dest.join(crate::engine::git_cli::repo_name_from_url(url)))?
        } else {
            dest
        };
        if clone_path.join(".git").exists() {
            return Err(format!("Already cloned at {}", clone_path.display()));
        }
        let clone_str = clone_path.to_string_lossy().into_owned();
        // Clone is the longest network operation in the app (whole history +
        // working tree over possibly slow links). DEFAULT_TIMEOUT's 90s cap
        // kills healthy multi-gigabyte transfers mid-flight and leaves a
        // partial clone behind, so this runs under NETWORK_TIMEOUT.
        if let Err(clone_err) =
            git_global_with_timeout(&["clone", "--", url, &clone_str], NETWORK_TIMEOUT)
        {
            // git materializes <dest>/.git before transferring objects, so a
            // failed or timed-out clone leaves a skeleton behind that blocks
            // every retry ("already exists"). Its absence was verified just
            // above, so anything present now came from our own attempt:
            // remove it best-effort so a retry starts clean.
            let leftover = clone_path.join(".git");
            if leftover.is_dir() {
                let _ = std::fs::remove_dir_all(&leftover);
                return Err(format!(
                    "clone failed ({clone_err}); removed the partial '.git' left at {}",
                    leftover.display()
                ));
            }
            return Err(clone_err);
        }
        Ok(clone_str)
    }
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
    let parent_canonical = parent.canonicalize().map_err(|e| {
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
            let actual = dest.canonicalize().map_err(|e| {
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
        Err(_) => Ok(parent_canonical.join(name)),
    }
}

/// True when `branch_name` is the branch the repository's HEAD resolves to by
/// default: the primary remote's HEAD branch first, then conventional
/// main/master/trunk/develop — the same resolution `list_branches` uses.
fn is_default_branch(repo: &Path, branch_name: &str) -> bool {
    let remote = crate::engine::git_reader::resolve_default_remote(repo);
    let head_ref = crate::engine::git_reader::remote_head_ref(&remote);
    let remote_head = git_text(repo, &["symbolic-ref", "--quiet", head_ref.as_str()]).ok();
    crate::engine::git_reader::resolve_default_base_on(repo, &remote, remote_head.as_deref())
        .is_some_and(|(short, _)| short == branch_name)
}

/// True when any worktree of `repo` (including the main one) has
/// `branch_name` checked out.
fn is_checked_out_in_any_worktree(repo: &Path, branch_name: &str) -> Result<bool, String> {
    let stdout = git_text(repo, &["worktree", "list", "--porcelain"])?;
    let target = format!("refs/heads/{branch_name}");
    Ok(stdout
        .lines()
        .any(|line| line.strip_prefix("branch ").map(str::trim) == Some(target.as_str())))
}

/// Rewrites only the subject line of a commit message, keeping the body —
/// including its blank-line separation from the subject — intact.
fn reworded_message(original: &str, new_subject: &str) -> String {
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
/// - `git_reader::get_file_blob`: builds `<rev>:<path>` and *independently
///   rejects* any `:` before doing so.
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

    /// Git words an empty commit two ways depending on tree state:
    /// "nothing to commit, working tree clean" and "nothing added to commit
    /// but untracked files present". Both exit 1; both mean retry.
    fn is_empty_commit_refusal(error: &str) -> bool {
        let lower = error.to_lowercase();
        lower.contains("nothing to commit") || lower.contains("nothing added to commit")
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

    fn init_repo_with_commit() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git init");
        assert!(output.status.success());
        std::fs::write(dir.path().join("tracked.txt"), "base\n").unwrap();
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["add", "--", "tracked.txt"])
            .current_dir(dir.path())
            .output()
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
            .output()
            .expect("spawn git commit");
        assert!(
            output.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
            .output()
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
            .output()
            .expect("spawn git commit");
        assert!(
            output.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .expect("rev-parse");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn head_message(dir: &tempfile::TempDir) -> String {
        let output = std::process::Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(dir.path())
            .output()
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
        GitWriter::execute_rebase_sequence_for_branch(
            dir.path().to_str().unwrap(),
            &base,
            &steps,
            Some("main"),
        )
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

        let result = GitWriter::execute_rebase_sequence_for_branch(
            dir.path().to_str().unwrap(),
            &base,
            &[RebaseStep {
                commit_id: c_a,
                action: RebaseActionKind::Pick,
            }],
            Some("main"),
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
            .output()
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
                                        .output()
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
                    .output()
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
            .output()
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

    /// Compile-time proof that the network writers reference the shared
    /// network deadline: clone/fetch/pull/push must never run under the 90s
    /// DEFAULT_TIMEOUT (it kills healthy multi-gigabyte transfers mid-flight),
    /// so the constant they use has to be strictly larger.
    #[test]
    fn network_timeout_exceeds_default_timeout() {
        assert!(
            NETWORK_TIMEOUT > crate::engine::git_cli::DEFAULT_TIMEOUT,
            "network operations must not inherit the short default cap"
        );
    }

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// Gate/execution parity: push_argv is what the executor runs, so a force
    /// push must surface `--force-with-lease` — the flag actually executed —
    /// to the gate, never the bare `--force` the old hand-rolled gate line
    /// claimed.
    #[test]
    fn push_argv_is_the_executed_line_including_lease_semantics() {
        assert_eq!(
            push_argv(Some("origin"), Some("main"), false),
            owned(&["git", "push", "origin", "main"])
        );
        assert_eq!(
            push_argv(None, None, true),
            owned(&["git", "push", "--force-with-lease"]),
            "force pushes must be judged as --force-with-lease"
        );
        assert_eq!(
            push_argv(Some("up"), Some("feat/x"), true),
            owned(&["git", "push", "--force-with-lease", "up", "feat/x"])
        );
    }

    #[test]
    fn pull_and_merge_and_commit_argvs_match_execution() {
        assert_eq!(
            pull_argv(Some("origin"), Some("main")),
            owned(&["git", "pull", "origin", "main"])
        );
        assert_eq!(pull_argv(None, None), owned(&["git", "pull"]));

        // The writer always adds --no-edit; the gate must see it.
        assert_eq!(
            merge_argv("feature", false),
            owned(&["git", "merge", "--no-edit", "feature"])
        );
        assert_eq!(
            merge_argv("feature", true),
            owned(&["git", "merge", "--ff-only", "--no-edit", "feature"])
        );

        assert_eq!(
            commit_argv("msg here", false),
            owned(&["git", "commit", "-m", "msg here"])
        );
        assert_eq!(
            commit_argv("msg", true),
            owned(&["git", "commit", "-m", "msg", "--amend"])
        );
        // Empty-message amend is what internal squash flows execute; the gate
        // has to judge those flags, not a "-m ''" line that never runs.
        assert_eq!(
            commit_argv("", true),
            owned(&["git", "commit", "--amend", "--no-edit"])
        );
    }

    /// Rewording rewrites only the subject line; the body — including its
    /// blank-line separation — must survive the composed amend message.
    #[test]
    fn reworded_message_keeps_body_rewrites_subject() {
        assert_eq!(reworded_message("old\n\nbody", "new"), "new\n\nbody");
        assert_eq!(reworded_message("subject only", "new"), "new");
    }

    #[test]
    fn restack_stash_and_tag_argvs_match_execution() {
        // Restack's argv is resolved at run time (fork-point upstream); its
        // gate mirror is covered by restack_planned_argv tests.
        assert_eq!(
            stash_save_argv(None),
            owned(&["git", "stash", "push", "-u"])
        );
        assert_eq!(
            stash_save_argv(Some("wip")),
            owned(&["git", "stash", "push", "-u", "-m", "wip"])
        );
        assert_eq!(stash_pop_argv(), owned(&["git", "stash", "pop"]));
        assert_eq!(
            create_tag_argv("v1.0", None, None),
            owned(&["git", "tag", "v1.0"])
        );
        assert_eq!(
            create_tag_argv("v1.0", Some("abc123"), Some("release")),
            owned(&["git", "tag", "-a", "v1.0", "-m", "release", "abc123"])
        );
        assert_eq!(
            delete_tag_argv("v1.0"),
            owned(&["git", "tag", "-d", "v1.0"])
        );
        assert_eq!(
            push_tag_argv("origin", "v1.0"),
            owned(&["git", "push", "origin", "refs/tags/v1.0"])
        );
    }

    /// Every strategy checkout_branch may attempt is listed up front so all
    /// of them can be gated before the first one runs.
    #[test]
    fn checkout_attempts_cover_every_fallback_strategy() {
        let attempts = checkout_branch_attempts("feature");
        assert_eq!(
            attempts,
            vec![
                owned(&["git", "switch", "--guess", "feature"]),
                owned(&["git", "checkout", "feature"]),
            ]
        );
    }

    /// The plan is the single source of truth for both gate and executor: it
    /// must contain detach, one cherry-pick per non-drop step (plus its amend
    /// for squash/fixup/reword), and the finalize pair only when starting on
    /// a branch.
    #[test]
    fn rebase_plan_composes_the_full_mutation_sequence() {
        let steps = vec![
            RebaseStep {
                commit_id: "a1".into(),
                action: RebaseActionKind::Pick,
            },
            RebaseStep {
                commit_id: "b2".into(),
                action: RebaseActionKind::Squash,
            },
            RebaseStep {
                commit_id: "c3".into(),
                action: RebaseActionKind::Drop,
            },
            RebaseStep {
                commit_id: "d4".into(),
                action: RebaseActionKind::Fixup,
            },
            RebaseStep {
                commit_id: "e5".into(),
                action: RebaseActionKind::Reword("new msg".into()),
            },
        ];
        let plan =
            rebase_sequence_plan("base0", &steps, Some("topic")).expect("plan should compose");
        let argvs: Vec<Vec<String>> = plan.iter().map(|c| c.argv.clone()).collect();
        assert_eq!(
            argvs,
            vec![
                owned(&["git", "checkout", "--detach", "base0"]),
                owned(&["git", "cherry-pick", "a1"]),
                owned(&["git", "cherry-pick", "-n", "b2"]),
                owned(&["git", "commit", "--amend", "--no-edit"]),
                owned(&["git", "cherry-pick", "-n", "d4"]),
                owned(&["git", "commit", "--amend", "--no-edit"]),
                owned(&["git", "cherry-pick", "e5"]),
                owned(&["git", "commit", "-m", "new msg", "--amend"]),
                owned(&["git", "branch", "-f", "topic", "HEAD"]),
                owned(&["git", "checkout", "topic"]),
            ],
            "gate and executor must share this exact sequence"
        );
        // Cherry-picks carry the historical failure labels; amends propagate
        // bare, matching the pre-planner error wording exactly.
        assert_eq!(plan[1].failure_label.as_deref(), Some("pick a1"));
        assert_eq!(plan[2].failure_label.as_deref(), Some("squash b2"));
        assert_eq!(plan[4].failure_label.as_deref(), Some("fixup d4"));
        assert_eq!(plan[6].failure_label.as_deref(), Some("reword e5"));
        assert!(plan[3].failure_label.is_none());

        // Detached sessions have no finalize pair.
        let detached = rebase_sequence_plan("base0", &steps, None).expect("plan");
        assert_eq!(detached.len(), plan.len() - 2);
        assert!(detached
            .iter()
            .all(|c| !c.argv.contains(&"branch".to_string())));
    }

    #[test]
    fn rebase_plan_rejects_invalid_input_before_any_command_exists() {
        let steps = vec![RebaseStep {
            commit_id: "; rm -rf".into(),
            action: RebaseActionKind::Pick,
        }];
        assert!(rebase_sequence_plan("ok-rev", &steps, None).is_err());
        assert!(rebase_sequence_plan("-evil", &steps, None).is_err());
        assert!(rebase_sequence_plan("ok-rev", &[], None).is_err());
        assert!(rebase_sequence_plan("ok-rev", &steps, Some("bad..ref")).is_err());
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
}
