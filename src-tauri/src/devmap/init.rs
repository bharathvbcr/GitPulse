//! Per-repository DevCouncil initialization — the setup a repository needs
//! once, done when it is opened instead of retyped for every clone.
//!
//! ## What is automatic, and what is not
//!
//! Everything here writes *only* inside `.git/info/exclude` and the resolved
//! DevMap state directory. Nothing it does appears in `git status`, in a diff,
//! or in a commit. That boundary is the whole design: the state directory and
//! the workspace registry are machine state this application already manages,
//! while agent guides, editor rules and `.mcp.json` are tracked content in a
//! repository GitPulse does not own — those live behind
//! [`super::integrate`], which previews before it writes and never runs
//! unasked.
//!
//! ## Why `info/exclude` and not `.gitignore`
//!
//! `devmap build` leaves `.devmap/` untracked, and nothing in the DevCouncil
//! toolchain ignores it. Indexing a repository automatically would therefore
//! put a permanent `?? .devmap/` into the one view this application exists to
//! render. `.gitignore` is the wrong instrument for fixing that: it is tracked
//! content, so writing to it creates a diff the user has to review, commit or
//! revert. `$GIT_COMMON_DIR/info/exclude` is git's own documented mechanism
//! for ignoring something in one clone, it is never committed, and it applies
//! to linked worktrees through the common directory.

use crate::engine::git_cli::{git_captured, resolve_git_common_dir, validate_repo};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Marker written above a pattern this module added, so a reader can tell who
/// wrote it and a maintainer can remove it by hand without guessing.
const EXCLUDE_MARKER: &str = "# GitPulse: DevMap index state (machine-generated, never committed)";

/// A pathological `info/exclude` must not be read into memory unbounded.
const EXCLUDE_READ_CAP: u64 = 1024 * 1024;

/// Ignore hygiene for one repository's DevMap state directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ExcludeOutcome {
    /// Git already ignores the state directory. `source` is git's own answer
    /// from `check-ignore -v`, so a project `.gitignore` that already carries
    /// the entry is never duplicated into a local exclude file.
    AlreadyIgnored { source: String },
    /// A pattern was appended to `$GIT_COMMON_DIR/info/exclude`, and git was
    /// asked again afterwards to confirm it took effect.
    Added { file: String, pattern: String },
    /// There is nothing to ignore — `DEVMAP_HOME` puts this repository's state
    /// outside the work tree.
    NotNeeded { reason: String },
    /// Nothing was written and the state directory is still not ignored.
    Refused { reason: String },
}

impl ExcludeOutcome {
    /// True when the state directory will stay out of `git status`.
    pub fn is_clean(&self) -> bool {
        matches!(
            self,
            Self::AlreadyIgnored { .. } | Self::Added { .. } | Self::NotNeeded { .. }
        )
    }
}

/// The state directory as git needs to see it, in both of the forms git wants.
///
/// They are deliberately different strings, and mixing them up fails loudly in
/// one direction and silently in the other. A *pattern* takes a leading slash
/// to anchor it to the repository root — without it, `.devmap` would also hide
/// a directory of that name nested anywhere in the tree. A *pathname argument*
/// to `check-ignore` must not: git reads a leading slash there as an absolute
/// filesystem path and refuses with `Invalid path`.
///
/// `None` when the resolved state directory is not inside this repository,
/// which `DEVMAP_HOME` can arrange. Path resolution is delegated to
/// `devmap_extract::paths` — the CLI's canonical owner — so a legacy
/// `.devcouncil` tree is matched where the CLI actually writes.
struct StateDirPaths {
    /// `.devmap/` — what `check-ignore` is asked about.
    query: String,
    /// `/.devmap/` — what goes in the exclude file.
    pattern: String,
}

fn state_dir_paths(repo: &Path) -> Option<StateDirPaths> {
    let state = devmap_extract::paths::state_dir(repo);
    let relative = state.strip_prefix(repo).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            // A state directory reached through `..` or a root is not under
            // this work tree however the prefix stripped.
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("/");
    Some(StateDirPaths {
        query: format!("{joined}/"),
        pattern: format!("/{joined}/"),
    })
}

/// Ask git whether the state directory is ignored, and by what.
///
/// The trailing slash matters: a `dir/` rule only matches a directory, and
/// `check-ignore` can only know a path is a directory from the trailing slash
/// when the directory does not exist yet — which is exactly the case before
/// the first build. `--no-index` so a state directory someone once committed
/// still reports the rule that covers it rather than its tracked status.
fn ignore_source(repo: &Path, query: &str) -> Result<Option<String>, String> {
    let run = git_captured(repo, &["check-ignore", "-v", "--no-index", "--", query])?;
    match run.status_code {
        0 => {
            let stdout = String::from_utf8_lossy(&run.stdout);
            let first = stdout.lines().next().unwrap_or_default().trim().to_string();
            Ok(Some(if first.is_empty() {
                "an existing ignore rule".to_string()
            } else {
                first
            }))
        }
        1 => Ok(None),
        other => Err(format!(
            "git check-ignore exited {other}: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        )),
    }
}

/// Append `pattern` to this repository's local exclude file.
fn append_exclude(exclude: &Path, pattern: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    // A symlink here would redirect the write outside the git directory.
    if let Ok(meta) = std::fs::symlink_metadata(exclude) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "{} is a symbolic link; refusing to write through it",
                exclude.display()
            ));
        }
        if !meta.is_file() {
            return Err(format!("{} is not a regular file", exclude.display()));
        }
        if meta.len() > EXCLUDE_READ_CAP {
            return Err(format!(
                "{} is larger than {EXCLUDE_READ_CAP} bytes; refusing to edit it",
                exclude.display()
            ));
        }
    }
    let existing = std::fs::read_to_string(exclude).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == pattern) {
        // Git said the directory is not ignored, yet the pattern is already
        // here — a later rule re-includes it. Appending a second copy of a
        // line that is already being overridden would change nothing.
        return Err(format!(
            "{} already lists `{pattern}`, but git still does not ignore it — a later rule re-includes it",
            exclude.display()
        ));
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(exclude)
        .map_err(|e| format!("cannot open {}: {e}", exclude.display()))?;
    let lead = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    file.write_all(format!("{lead}{EXCLUDE_MARKER}\n{pattern}\n").as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", exclude.display()))?;
    file.flush()
        .map_err(|e| format!("cannot flush {}: {e}", exclude.display()))
}

/// Keep this repository's DevMap state directory out of `git status`.
///
/// Idempotent, and verified rather than assumed: after writing, git is asked
/// again whether the directory is ignored, and a pattern that did not take
/// effect is reported as a refusal instead of a success.
pub fn ensure_state_dir_excluded(repo: &Path) -> ExcludeOutcome {
    let Some(paths) = state_dir_paths(repo) else {
        return ExcludeOutcome::NotNeeded {
            reason: "this repository's DevMap state directory is outside its work tree".into(),
        };
    };
    match ignore_source(repo, &paths.query) {
        Ok(Some(source)) => return ExcludeOutcome::AlreadyIgnored { source },
        Ok(None) => {}
        Err(reason) => return ExcludeOutcome::Refused { reason },
    }
    let common = match resolve_git_common_dir(repo) {
        Ok(dir) => dir,
        Err(reason) => return ExcludeOutcome::Refused { reason },
    };
    let exclude = common.join("info").join("exclude");
    if let Err(reason) = append_exclude(&exclude, &paths.pattern) {
        return ExcludeOutcome::Refused { reason };
    }
    // Never report success on an unverified write: an exclude file git does
    // not read, or a pattern a later rule overrides, must not look the same as
    // a directory that is genuinely ignored now.
    match ignore_source(repo, &paths.query) {
        Ok(Some(_)) => ExcludeOutcome::Added {
            file: exclude.to_string_lossy().into_owned(),
            pattern: paths.pattern,
        },
        Ok(None) => ExcludeOutcome::Refused {
            reason: format!(
                "wrote `{}` to {} but git still does not ignore it",
                paths.pattern,
                exclude.display()
            ),
        },
        Err(reason) => ExcludeOutcome::Refused { reason },
    }
}

/// Everything one repository's automatic initialization did or declined to do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InitReport {
    pub repo: String,
    /// Resolved DevMap state directory, wherever `devmap_extract::paths` puts
    /// it for this repository.
    pub state_dir: String,
    pub exclude: ExcludeOutcome,
    /// Path of the workspace registry this repository now hosts, when the
    /// open-tab set was synchronized into it.
    pub workspace_registry: Option<String>,
    /// Why the registry was not written, when it was not.
    pub workspace_reason: Option<String>,
    /// `devmap` resolved to a binary. Initialization still runs its ignore
    /// hygiene without one — it costs nothing and the directory may already
    /// exist from another host — but it will not create state for a tool that
    /// is not installed.
    pub devmap_available: bool,
}

/// Initialize one opened repository.
///
/// `open_repos` is the full set of repositories currently open in the app; the
/// registry this repository hosts is made to match it, which is what gives
/// cross-repository search and import links anything to read. Passing an empty
/// set skips the registry without touching it.
pub fn initialize(repo_path: &str, open_repos: &[String]) -> Result<InitReport, String> {
    let repo = validate_repo(repo_path)?;
    let state_dir = devmap_extract::paths::state_dir(&repo);
    let exclude = ensure_state_dir_excluded(&repo);
    let devmap_available = super::cli::resolve_binary().is_ok();

    let mut workspace_registry = None;
    let mut workspace_reason = None;
    if open_repos.is_empty() {
        workspace_reason = Some("no open repositories to register".into());
    } else if !devmap_available && !state_dir.is_dir() {
        // Creating a state directory for a tool that is not installed would
        // put an empty `.devmap/` in a tree that has no use for one.
        workspace_reason =
            Some("devmap is not installed and this repository has no DevMap state yet".into());
    } else if !exclude.is_clean() {
        // Writing the registry now would create exactly the untracked
        // directory the exclude step failed to hide.
        workspace_reason = Some(
            "the DevMap state directory is not ignored; not creating untracked state in it".into(),
        );
    } else {
        match crate::workspace_registry::sync_open_tabs(repo_path, open_repos) {
            Ok(snapshot) => workspace_registry = Some(snapshot.registry_path),
            Err(reason) => workspace_reason = Some(reason),
        }
    }

    Ok(InitReport {
        repo: repo.to_string_lossy().into_owned(),
        state_dir: state_dir.to_string_lossy().into_owned(),
        exclude,
        workspace_registry,
        workspace_reason,
        devmap_available,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@example.invalid"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("a.txt"), "hi").expect("write");
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-m", "init"]);
        // Every git call this module makes runs through `git_captured`, which
        // requires trust — so an untrusted repository is never probed, let
        // alone written to.
        crate::test_support::trust_repo(dir.path());
        dir
    }

    /// `git status` must not gain an untracked entry because the index was
    /// built. This is the defect in its measurable form.
    #[test]
    fn an_indexed_repository_stays_clean_in_git_status() {
        let repo = init_repo();
        let outcome = ensure_state_dir_excluded(repo.path());
        assert!(
            matches!(outcome, ExcludeOutcome::Added { .. }),
            "{outcome:?}"
        );

        // Simulate what a build leaves behind.
        let state = devmap_extract::paths::state_dir(repo.path());
        std::fs::create_dir_all(state.join("codeintel")).expect("state dir");
        std::fs::write(state.join("codeintel").join("devmap.sqlite"), "x").expect("store");
        std::fs::write(state.join("repo_map.json"), "{}").expect("map");

        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .expect("git status");
        assert_eq!(
            String::from_utf8_lossy(&status.stdout).trim(),
            "",
            "the state directory must not appear in git status"
        );
    }

    #[test]
    fn a_second_initialization_adds_nothing() {
        let repo = init_repo();
        assert!(matches!(
            ensure_state_dir_excluded(repo.path()),
            ExcludeOutcome::Added { .. }
        ));
        let exclude = repo.path().join(".git/info/exclude");
        let after_first = std::fs::read_to_string(&exclude).expect("exclude");

        let second = ensure_state_dir_excluded(repo.path());
        assert!(
            matches!(second, ExcludeOutcome::AlreadyIgnored { .. }),
            "{second:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&exclude).expect("exclude"),
            after_first,
            "a repeat run must not append a second copy"
        );
    }

    #[test]
    fn an_existing_gitignore_entry_is_not_duplicated_into_the_exclude_file() {
        let repo = init_repo();
        std::fs::write(repo.path().join(".gitignore"), ".devmap/\n").expect("gitignore");
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-m", "ignore"]);

        let outcome = ensure_state_dir_excluded(repo.path());
        match outcome {
            ExcludeOutcome::AlreadyIgnored { source } => {
                assert!(source.contains(".gitignore"), "{source}");
            }
            other => panic!("expected the project rule to be honoured, got {other:?}"),
        }
        assert!(
            !repo.path().join(".git/info/exclude").exists()
                || !std::fs::read_to_string(repo.path().join(".git/info/exclude"))
                    .unwrap_or_default()
                    .contains(EXCLUDE_MARKER),
            "nothing should have been written"
        );
    }

    /// A linked worktree has no `info/exclude` of its own: git reads the
    /// common directory's. Writing to the worktree's own git directory would
    /// silently do nothing, which is the failure this asserts against.
    #[test]
    fn a_linked_worktree_is_excluded_through_the_common_directory() {
        let repo = init_repo();
        // Its own temp root: the parent of a TempDir is the shared system temp
        // directory, where a fixed name collides with every other run.
        let host = tempfile::TempDir::new().expect("worktree host");
        let linked = host.path().join("linked-wt");
        git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                "side",
                linked.to_str().expect("utf8 path"),
            ],
        );

        // Trust is per checkout; a linked worktree is its own.
        crate::test_support::trust_repo(&linked);

        let outcome = ensure_state_dir_excluded(&linked);
        match &outcome {
            ExcludeOutcome::Added { file, .. } => {
                // Canonicalized on both sides: on macOS a temp path resolves
                // through the `/var` → `/private/var` link, so a raw prefix
                // comparison would fail on a correct answer.
                let common = std::fs::canonicalize(repo.path().join(".git")).expect("common dir");
                let written = std::fs::canonicalize(file).expect("exclude file");
                assert!(
                    written.starts_with(&common),
                    "expected the common directory {}, got {}",
                    common.display(),
                    written.display()
                );
            }
            other => panic!("expected Added, got {other:?}"),
        }

        std::fs::create_dir_all(devmap_extract::paths::state_dir(&linked)).expect("state");
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&linked)
            .output()
            .expect("git status");
        assert_eq!(String::from_utf8_lossy(&status.stdout).trim(), "");
        git(
            repo.path(),
            &[
                "worktree",
                "remove",
                "--force",
                linked.to_str().expect("utf8"),
            ],
        );
    }

    /// A `.devcouncil` tree that already exists is the state directory the CLI
    /// resolves, so it is the one that has to be ignored — not a hard-coded
    /// `.devmap` the tools would never write to.
    #[test]
    fn a_legacy_state_directory_is_the_one_excluded() {
        let repo = init_repo();
        std::fs::create_dir_all(repo.path().join(".devcouncil")).expect("legacy dir");
        let outcome = ensure_state_dir_excluded(repo.path());
        match outcome {
            ExcludeOutcome::Added { pattern, .. } => {
                assert_eq!(pattern, "/.devcouncil/");
            }
            other => panic!("expected the legacy directory to be excluded, got {other:?}"),
        }
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .expect("git status");
        assert_eq!(String::from_utf8_lossy(&status.stdout).trim(), "");
    }

    /// The exclude pattern is anchored, so a `.devmap` directory nested
    /// somewhere in the tree is still the user's business, not ours.
    #[test]
    fn the_pattern_is_anchored_to_the_repository_root() {
        let repo = init_repo();
        let paths = state_dir_paths(repo.path()).expect("inside the work tree");
        assert_eq!(paths.pattern, "/.devmap/");
        assert_eq!(paths.query, ".devmap/");
        ensure_state_dir_excluded(repo.path());
        std::fs::create_dir_all(repo.path().join("vendor/.devmap")).expect("nested");
        std::fs::write(repo.path().join("vendor/.devmap/keep.txt"), "x").expect("write");
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .expect("git status");
        assert!(
            String::from_utf8_lossy(&status.stdout).contains("vendor/"),
            "a nested directory of the same name must stay visible, got {:?}",
            String::from_utf8_lossy(&status.stdout)
        );
    }

    /// Honesty: a pattern already present but overridden by a later rule must
    /// not be re-appended and must not report success.
    #[test]
    fn an_overridden_pattern_is_refused_rather_than_duplicated() {
        let repo = init_repo();
        let exclude = repo.path().join(".git/info/exclude");
        std::fs::create_dir_all(exclude.parent().expect("parent")).expect("info");
        // The pattern is present, and a negation after it wins.
        std::fs::write(&exclude, "/.devmap/\n!/.devmap/\n").expect("exclude");
        let before = std::fs::read_to_string(&exclude).expect("read");

        let outcome = ensure_state_dir_excluded(repo.path());
        match outcome {
            ExcludeOutcome::Refused { reason } => {
                assert!(reason.contains("re-includes it"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&exclude).expect("read"), before);
    }

    #[test]
    fn a_symlinked_exclude_file_is_refused() {
        #[cfg(unix)]
        {
            let repo = init_repo();
            let elsewhere = repo.path().join("target-of-link");
            std::fs::write(&elsewhere, "").expect("target");
            let exclude = repo.path().join(".git/info/exclude");
            std::fs::create_dir_all(exclude.parent().expect("parent")).expect("info");
            let _ = std::fs::remove_file(&exclude);
            std::os::unix::fs::symlink(&elsewhere, &exclude).expect("symlink");

            let outcome = ensure_state_dir_excluded(repo.path());
            match outcome {
                ExcludeOutcome::Refused { reason } => {
                    assert!(reason.contains("symbolic link"), "{reason}")
                }
                other => panic!("expected a refusal, got {other:?}"),
            }
            assert_eq!(
                std::fs::read_to_string(&elsewhere).expect("target"),
                "",
                "the link target must be untouched"
            );
        }
    }

    #[test]
    fn initialize_registers_the_open_tab_set() {
        let _stub = super::super::cli::bind_test_binary("devmap");
        let repo = init_repo();
        let other = init_repo();
        let repo_path = repo.path().to_string_lossy().into_owned();
        let other_path = other.path().to_string_lossy().into_owned();

        let report =
            initialize(&repo_path, &[repo_path.clone(), other_path.clone()]).expect("initialize");
        assert!(report.exclude.is_clean(), "{:?}", report.exclude);
        let registry = report
            .workspace_registry
            .as_deref()
            .unwrap_or_else(|| panic!("registry not written: {:?}", report.workspace_reason));
        let written = std::fs::read_to_string(registry).expect("registry file");
        assert!(written.contains(other.path().file_name().unwrap().to_str().unwrap()));

        // And the registry it just created is itself ignored.
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .expect("git status");
        assert_eq!(String::from_utf8_lossy(&status.stdout).trim(), "");
    }

    #[test]
    fn initialize_writes_no_state_when_the_directory_cannot_be_ignored() {
        let repo = init_repo();
        let exclude = repo.path().join(".git/info/exclude");
        std::fs::create_dir_all(exclude.parent().expect("parent")).expect("info");
        std::fs::write(&exclude, "/.devmap/\n!/.devmap/\n").expect("exclude");

        let repo_path = repo.path().to_string_lossy().into_owned();
        let report = initialize(&repo_path, std::slice::from_ref(&repo_path)).expect("initialize");
        assert!(matches!(report.exclude, ExcludeOutcome::Refused { .. }));
        assert!(report.workspace_registry.is_none());
        assert!(!Path::new(&report.state_dir).exists(), "no state written");
    }
}
