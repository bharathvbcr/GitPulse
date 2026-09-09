//! The one gate between the webview and "hand this path to the OS".
//!
//! Opening a path with its default application is the single most powerful
//! thing the frontend can ask for: on macOS and Windows, `open` on a bundle or
//! an executable *runs* it. So the decision of which paths qualify cannot live
//! in TypeScript, where it is one forgotten call away from being skipped —
//! which is exactly how it was: three of the four call sites joined the path
//! through `joinWorktreePath`, and the fourth interpolated `${repo}/${file}`
//! raw.
//!
//! These commands take the repository root and a repo-relative path as two
//! separate arguments and rebuild the absolute path here, so the frontend
//! never gets to name an absolute path at all. Containment is then proven
//! against the canonicalized root, which also closes the symlink escape a
//! textual prefix check cannot see: a repo can ship `link -> /Applications`,
//! and `"/repo/link".starts_with("/repo")` is true while the file it opens is
//! not in the repo.
//!
//! The plugin's own `open_path`/`reveal_item_in_dir` commands are deliberately
//! NOT granted in `capabilities/default.json`; they take an absolute path
//! straight from the webview and would make this gate optional.

use std::path::{Path, PathBuf};

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

/// Rebuilds `repo` + `relative` into an absolute path proven to sit inside the
/// repository, or explains which rule rejected it.
///
/// Every rejection names the situation rather than returning a bare bool: the
/// frontend surfaces these verbatim, and "Cannot open a path outside the
/// repository" for a merely-missing file sent users hunting for a permission
/// problem that was not there.
pub fn resolve_worktree_path(repo: &str, relative: &str) -> Result<PathBuf, String> {
    if repo.trim().is_empty() {
        return Err("No repository is open".to_string());
    }
    if relative.trim().is_empty() {
        return Err("No file path was given".to_string());
    }

    // Rejected before touching the filesystem: an absolute or drive-qualified
    // "relative" path would make `Path::join` discard the root entirely, and
    // `..` would walk out of it. Backslashes are normalized first so a Windows
    // -style payload cannot smuggle a separator past the segment scan on Unix,
    // where `\` is an ordinary filename character.
    let normalized = relative.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(format!("Not a repository-relative path: {relative}"));
    }
    let mut chars = normalized.chars();
    if let (Some(drive), Some(':')) = (chars.next(), chars.next()) {
        if drive.is_ascii_alphabetic() {
            return Err(format!("Not a repository-relative path: {relative}"));
        }
    }
    for segment in normalized.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("Not a repository-relative path: {relative}"));
        }
    }

    let root = Path::new(repo)
        .canonicalize()
        .map_err(|e| format!("Cannot read the repository at {repo}: {e}"))?;
    if !root.is_dir() {
        return Err(format!("Not a directory: {repo}"));
    }

    // Joined component-wise from the already-scanned segments rather than from
    // the raw string, so nothing that survived the scan can reintroduce a
    // separator the scan did not see.
    let mut joined = root.clone();
    for segment in normalized.split('/') {
        joined.push(segment);
    }

    let target = joined
        .canonicalize()
        .map_err(|e| format!("Cannot read {relative}: {e}"))?;

    // Canonicalized on both sides, so this compares real locations: a symlink
    // inside the repo that points out of it fails here.
    if !target.starts_with(&root) {
        return Err(format!(
            "Refusing to open a path outside the repository: {relative}"
        ));
    }
    // `starts_with` on `Path` is component-wise, so a sibling directory whose
    // name merely extends the root's ("/repo-backup" against "/repo") cannot
    // match. Proved by a test rather than assumed.
    Ok(target)
}

/// Opens a repo-relative path with the OS default application.
#[tauri::command(async)]
pub fn cmd_open_worktree_path(
    app: AppHandle,
    repo: String,
    relative: String,
) -> Result<(), String> {
    let target = resolve_worktree_path(&repo, &relative)?;
    app.opener()
        .open_path(target.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| format!("Could not open {relative}: {e}"))
}

/// Reveals a repo-relative path in the OS file manager.
#[tauri::command(async)]
pub fn cmd_reveal_worktree_path(
    app: AppHandle,
    repo: String,
    relative: String,
) -> Result<(), String> {
    let target = resolve_worktree_path(&repo, &relative)?;
    app.opener()
        .reveal_item_in_dir(&target)
        .map_err(|e| format!("Could not reveal {relative}: {e}"))
}

/// Reveal the canonical repository root, including linked worktrees and bare repositories.
#[tauri::command(async)]
pub fn cmd_reveal_repository(app: AppHandle, repo: String) -> Result<(), String> {
    let resolved = crate::engine::resolve_repo(&repo)?;
    app.opener()
        .reveal_item_in_dir(std::path::Path::new(&resolved.path))
        .map_err(|error| format!("Could not reveal repository: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo_with(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        for file in files {
            let path = dir.path().join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent");
            }
            fs::write(&path, b"x").expect("write");
        }
        dir
    }

    #[test]
    fn resolves_a_plain_repo_relative_file() {
        let repo = repo_with(&["src/main.rs"]);
        let root = repo.path().to_string_lossy().into_owned();
        let resolved = resolve_worktree_path(&root, "src/main.rs").expect("inside the repo");
        assert!(resolved.ends_with("src/main.rs"), "{resolved:?}");
        assert!(
            resolved.starts_with(repo.path().canonicalize().expect("root")),
            "{resolved:?}"
        );
    }

    /// The traversal class, swept rather than spot-checked: every one of these
    /// shapes must be refused before the filesystem is touched.
    #[test]
    fn refuses_every_shape_that_leaves_the_repository() {
        let repo = repo_with(&["src/main.rs"]);
        let root = repo.path().to_string_lossy().into_owned();
        for escape in [
            "../outside.txt",
            "..",
            "src/../../outside.txt",
            "src/./../../outside.txt",
            "/etc/passwd",
            "//etc/passwd",
            "C:/Windows/System32/calc.exe",
            "c:/Windows/System32/calc.exe",
            "..\\outside.txt",
            "src\\..\\..\\outside.txt",
            "\\\\server\\share\\file",
            "src//main.rs",
            "./src/main.rs",
            "",
            "   ",
        ] {
            let err = resolve_worktree_path(&root, escape)
                .expect_err(&format!("{escape:?} must be refused"));
            assert!(
                !err.is_empty(),
                "{escape:?} must be refused with a reason, got an empty message"
            );
        }
    }

    #[test]
    fn refuses_a_missing_file_without_claiming_it_escaped() {
        let repo = repo_with(&["src/main.rs"]);
        let root = repo.path().to_string_lossy().into_owned();
        let err = resolve_worktree_path(&root, "src/absent.rs").expect_err("missing");
        assert!(
            err.contains("Cannot read"),
            "a missing file must not be reported as an escape: {err}"
        );
    }

    #[test]
    fn refuses_an_empty_repository_root() {
        for root in ["", "   "] {
            let err = resolve_worktree_path(root, "src/main.rs").expect_err("no repo");
            assert!(err.contains("No repository"), "{err}");
        }
    }

    /// A sibling directory whose name merely extends the root's must not pass
    /// containment. `starts_with` on `Path` is component-wise, which is what
    /// makes this safe; a string prefix check would let it through.
    #[test]
    fn a_sibling_directory_sharing_the_roots_name_prefix_is_outside() {
        let parent = tempfile::tempdir().expect("temp dir");
        let repo = parent.path().join("repo");
        let sibling = parent.path().join("repo-backup");
        fs::create_dir_all(&repo).expect("repo");
        fs::create_dir_all(&sibling).expect("sibling");
        fs::write(sibling.join("secret.txt"), b"x").expect("write");

        let root = repo.to_string_lossy().into_owned();
        resolve_worktree_path(&root, "../repo-backup/secret.txt")
            .expect_err("a sibling with a shared name prefix is still outside");
    }

    /// A repository can ship a symlink pointing out of itself. A textual
    /// prefix check calls that contained; canonicalizing both sides does not.
    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_that_escapes_the_repository() {
        let parent = tempfile::tempdir().expect("temp dir");
        let repo = parent.path().join("repo");
        fs::create_dir_all(&repo).expect("repo");
        let outside = parent.path().join("outside.txt");
        fs::write(&outside, b"secret").expect("write");
        std::os::unix::fs::symlink(&outside, repo.join("link.txt")).expect("symlink");

        let root = repo.to_string_lossy().into_owned();
        let err = resolve_worktree_path(&root, "link.txt")
            .expect_err("a symlink out of the repo is not inside it");
        assert!(err.contains("outside the repository"), "{err}");
    }

    /// The mirror of the above: a symlink that stays inside must still work,
    /// so the containment rule is not just "reject all symlinks".
    #[cfg(unix)]
    #[test]
    fn allows_a_symlink_that_stays_inside_the_repository() {
        let repo = repo_with(&["src/main.rs"]);
        std::os::unix::fs::symlink(repo.path().join("src/main.rs"), repo.path().join("link.rs"))
            .expect("symlink");
        let root = repo.path().to_string_lossy().into_owned();
        let resolved = resolve_worktree_path(&root, "link.rs").expect("inside");
        assert!(resolved.ends_with("main.rs"), "{resolved:?}");
    }

    /// Unicode paths are ordinary here — the crash this audit started from was
    /// a byte-index split on multi-byte text, so no path rule may index bytes.
    #[test]
    fn handles_multibyte_path_segments() {
        let repo = repo_with(&["日本語/ファイル—名.txt"]);
        let root = repo.path().to_string_lossy().into_owned();
        let resolved = resolve_worktree_path(&root, "日本語/ファイル—名.txt").expect("inside");
        assert!(resolved.exists(), "{resolved:?}");
    }
}
