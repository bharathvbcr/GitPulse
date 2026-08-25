use crate::engine::git_cli::{git_global, git_text, git_with_stdin, sandbox_join, validate_repo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

fn repo_mutation_lock(canon: &Path) -> Arc<Mutex<()>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(canon.to_path_buf())
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

pub struct GitWriter;

impl GitWriter {
    pub fn stage_file(repo_path: &str, file_path: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = sandbox_join(&repo, file_path)?;
        git_text(&repo, &["add", "--", file_path])?;
        Ok(())
    }

    pub fn unstage_file(repo_path: &str, file_path: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = sandbox_join(&repo, file_path)?;
        git_text(&repo, &["restore", "--staged", "--", file_path])?;
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

    pub fn checkout_branch(repo_path: &str, branch_name: &str) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
        let attempts: [&[&str]; 3] = [
            &["switch", "--guess", branch_name],
            &["checkout", "--guess", branch_name],
            &["checkout", branch_name],
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
            validate_ref_name(sp)?;
            args.push(sp);
        }
        git_text(&repo, args.as_slice())?;
        Ok(())
    }

    pub fn delete_branch(repo_path: &str, branch_name: &str, force: bool) -> Result<(), String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch_name)?;
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
        let original_branch = git_text(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let restore = |repo: &std::path::Path| {
            if let Some(ref branch) = original_branch {
                let _ = git_text(repo, &["checkout", "-f", branch]);
            } else {
                let _ = git_text(repo, &["checkout", "-f", &original_head]);
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
                        Self::commit_inner(&repo, new_msg, true)?;
                    }
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            let _ = git_text(&repo, &["cherry-pick", "--abort"]);
            restore(&repo);
            return Err(e);
        }

        if let Some(ref branch) = original_branch {
            if let Err(e) = git_text(&repo, &["branch", "-f", branch, "HEAD"]) {
                restore(&repo);
                return Err(e);
            }
            if let Err(e) = git_text(&repo, &["checkout", branch]) {
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
        if let Some(r) = remote {
            validate_ref_name(r)?;
            git_text(&repo, &["fetch", r])
        } else {
            git_text(&repo, &["fetch", "--all", "--prune"])
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
        match (remote, branch) {
            (Some(r), Some(b)) => {
                validate_ref_name(r)?;
                validate_ref_name(b)?;
                git_text(&repo, &["pull", r, b])
            }
            (Some(r), None) => {
                validate_ref_name(r)?;
                git_text(&repo, &["pull", r])
            }
            _ => git_text(&repo, &["pull"]),
        }
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
        let mut args = vec!["push"];
        if force {
            args.push("--force-with-lease");
        }
        if let Some(r) = remote {
            validate_ref_name(r)?;
            args.push(r);
        }
        if let Some(b) = branch {
            validate_ref_name(b)?;
            args.push(b);
        }
        git_text(&repo, &args)
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
        git_text(&repo, &["push", remote, &refspec])
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
        if ff_only {
            git_text(&repo, &["merge", "--ff-only", "--no-edit", branch_name])
        } else {
            git_text(&repo, &["merge", "--no-edit", branch_name])
        }
    }

    pub fn restack(repo_path: &str, branch: &str, onto: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_ref_name(branch)?;
        validate_ref_name(onto)?;
        git_text(&repo, &["rebase", "--onto", onto, onto, branch])
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
        let restore_result = git_text(&repo, &["restore", "--", file_path]);
        let clean_result = git_text(&repo, &["clean", "-f", "--", file_path]);
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
        if let Some(msg) = message {
            git_text(&repo, &["stash", "push", "-u", "-m", msg])
        } else {
            git_text(&repo, &["stash", "push", "-u"])
        }
    }

    pub fn stash_pop(repo_path: &str) -> Result<String, String> {
        let repo = validate_repo(repo_path)?;
        let _repo_lock = repo_mutation_lock(&repo);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        git_text(&repo, &["stash", "pop"])
    }

    pub fn clone_repo(url: &str, target_dir: &str) -> Result<String, String> {
        if url.is_empty() || url.contains('\0') {
            return Err("Invalid clone URL".into());
        }
        let dest = Path::new(target_dir);
        if !dest.is_absolute() {
            return Err("Clone destination must be an absolute path".into());
        }
        if dest.join(".git").exists() {
            return Err("Destination is already a Git repository".into());
        }
        let clone_path = if dest.is_dir() {
            dest.join(crate::engine::git_cli::repo_name_from_url(url))
        } else {
            dest.to_path_buf()
        };
        if clone_path.join(".git").exists() {
            return Err(format!("Already cloned at {}", clone_path.display()));
        }
        let clone_str = clone_path.to_string_lossy().into_owned();
        git_global(&["clone", "--", url, &clone_str])?;
        Ok(clone_str)
    }
}

pub fn validate_ref_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.starts_with('-') || name.contains('\0') {
        return Err("Invalid ref name".into());
    }
    if name.contains("..") {
        return Err("Invalid ref name: contains traversal '..'".into());
    }
    if name.starts_with('.')
        || name.ends_with('.')
        || name.ends_with('/')
        || name.ends_with(".lock")
    {
        return Err("Invalid ref name: invalid prefix or suffix".into());
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

pub fn validate_oid_or_revision(rev: &str) -> Result<(), String> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains('\0') {
        return Err("Invalid revision".into());
    }
    if rev.chars().any(|c| {
        c.is_control() || matches!(c, ' ' | ';' | '&' | '|' | '`' | '$' | '(' | ')' | '<' | '>')
    }) {
        return Err("Invalid revision".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_ref_name() {
        assert!(validate_ref_name("feat/auth").is_ok());
        assert!(validate_ref_name("main").is_ok());
        assert!(validate_ref_name("v1.0.0").is_ok());

        assert!(validate_ref_name("-evil").is_err());
        assert!(validate_ref_name("foo bar").is_err());
        assert!(validate_ref_name("refs/../evil").is_err());
        assert!(validate_ref_name("feature.lock").is_err());
        assert!(validate_ref_name("branch/").is_err());
        assert!(validate_ref_name(".hidden").is_err());
        assert!(validate_ref_name("foo..bar").is_err());
        assert!(validate_ref_name("@").is_err());
        assert!(validate_ref_name("HEAD@{1}").is_err());
        assert!(validate_ref_name("foo//bar").is_err());
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
        assert!(!Arc::ptr_eq(&l1, &l3), "different repos must not share a lock");
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
                        std::fs::write(
                            std::path::Path::new(&path).join(&file),
                            format!("thread {t} round {i}\n"),
                        )
                        .unwrap();
                        // Stage→commit spans two lock acquisitions, so a
                        // sibling mutation may consume this thread's staged
                        // entry first and leave `git commit` with nothing to
                        // do. That is correct shared-index behavior; the
                        // caller retries until its own content lands.
                        let mut attempts = 0;
                        loop {
                            GitWriter::stage_file(&path, &file)
                                .unwrap_or_else(|e| panic!("stage {t}.{i}: {e}"));
                            match GitWriter::commit(&path, &format!("commit t{t}.{i}"), false) {
                                Ok(_) => break,
                                Err(e) if e.to_lowercase().contains("nothing to commit") => {
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
        let log = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let count: usize = String::from_utf8(log.stdout).unwrap().trim().parse().unwrap();
        assert_eq!(
            count,
            1 + THREADS * PER_THREAD,
            "seed + every concurrent commit must be present exactly once"
        );
    }
}
