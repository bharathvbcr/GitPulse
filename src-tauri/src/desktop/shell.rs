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

mod editor;

use crate::ipc_error::CommandError;
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

/// Extensions whose default application is the OS launcher, not a viewer.
///
/// `open` on these runs code: a `.command` or `.terminal` file starts Terminal
/// on macOS, a `.fileloc`/`.inetloc` navigates, an `.app` bundle launches, and
/// the Windows and desktop-Linux entries are launched by their shell handlers.
/// Files cloned from a repository carry no quarantine flag, so nothing else
/// would stop a one-click run.
const LAUNCHER_EXTENSIONS: &[&str] = &[
    "app", "command", "terminal", "fileloc", "inetloc", "webloc", "workflow", "action", "scpt",
    "scptd", "tool", "pkg", "mpkg", "dmg", "sh", "exe", "bat", "cmd", "com", "msi", "msp", "ps1",
    "scr", "lnk", "vbs", "vbe", "jse", "wsf", "hta", "reg", "jar", "url", "desktop", "appimage",
    "run",
];

/// Why opening `target` would execute it rather than show it, if it would.
///
/// A file with any execute bit counts whatever its name: git preserves the
/// bit, so a repository can ship an executable called `README`.
fn execution_risk(target: &Path) -> Option<String> {
    let extension = target
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if let Some(extension) = extension.filter(|e| LAUNCHER_EXTENSIONS.contains(&e.as_str())) {
        return Some(format!(
            "a .{extension} file or bundle that the OS can launch"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(target) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some("an executable file".to_string());
            }
        }
    }
    None
}

/// Everything that must hold before a repo-relative path is handed to the OS:
/// the path stays inside the repository, the repository is trusted, and an
/// executable or launcher is confirmed by the user in a dialog the page cannot
/// answer. `confirm` is [`confirm::native`] in production.
fn prepare_open(
    repo: &str,
    relative: &str,
    confirm: &dyn Fn(&str, &str) -> bool,
) -> Result<PathBuf, CommandError> {
    let target = resolve_worktree_path(repo, relative)?;
    crate::repository_trust::require(Path::new(repo))?;
    if let Some(risk) = execution_risk(&target) {
        let message = format!(
            "{relative} is {risk}. Opening it can run code from this repository.\n\nOpen it anyway?"
        );
        if !confirm("Open a file that can run code?", &message) {
            return Err(
                format!("Not opened: {relative} can run code and was not confirmed").into(),
            );
        }
    }
    Ok(target)
}

/// Opens a repo-relative path with the OS default application.
#[tauri::command(async)]
pub fn cmd_open_worktree_path(
    app: AppHandle,
    repo: String,
    relative: String,
) -> Result<(), String> {
    // This command answers in plain text, as it always has: no kind to keep.
    let target = prepare_open(&repo, &relative, &super::confirm::native)
        .map_err(CommandError::into_message)?;
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
    let resolved = crate::engine::resolve_repo(&repo).map_err(CommandError::into_message)?;
    app.opener()
        .reveal_item_in_dir(std::path::Path::new(&resolved.path))
        .map_err(|error| format!("Could not reveal repository: {error}"))
}

/// Opens a repo-relative file in the user's editor, at a line and column when
/// the caller knows them. `editor` is a closed choice, never a program name;
/// see [`editor`].
///
/// A file, not a launcher: the execution-risk confirmation of the OS-default
/// open does not apply, because nothing here executes the file — the editor
/// shows it. Containment and trust are the same as for every other command
/// here.
#[tauri::command(async)]
pub fn cmd_open_in_editor(
    repo: String,
    relative: String,
    line: Option<u32>,
    column: Option<u32>,
    editor: Option<editor::EditorChoice>,
) -> Result<(), crate::ipc_error::IpcError> {
    // Named, so the error carries the kind its producer gave it; not on the
    // replay-safe list, because opening an editor is a user action.
    crate::ipc_error::run_command_as(Some("cmd_open_in_editor"), || {
        // A click: the editor must not be deferred behind background reads.
        crate::engine::process::mark_user_action(&repo);
        let target = resolve_worktree_path(&repo, &relative)?;
        crate::repository_trust::require(Path::new(&repo))?;
        let root = Path::new(&repo)
            .canonicalize()
            .map_err(|e| format!("Cannot read the repository at {repo}: {e}"))?;
        editor::open_in_editor(
            &root,
            &target,
            line,
            column,
            editor.unwrap_or_default(),
            &editor::EditorEnv {
                use_bundles: cfg!(target_os = "macos"),
                bundle_exists: &|path| Path::new(path).is_file(),
                on_path: &crate::engine::process::find_external_tool,
                run: &editor::run_launcher,
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::procguard::LockedSpawn;
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

    #[cfg(unix)]
    fn trusted_repo_with(files: &[(&str, u32)]) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = repo_with(&[]);
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status_locked()
            .expect("git init");
        assert!(init.success());
        for (file, mode) in files {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().expect("parent")).expect("parent");
            fs::write(&path, b"x").expect("write");
            fs::set_permissions(&path, fs::Permissions::from_mode(*mode)).expect("mode");
        }
        crate::test_support::trust_repo(dir.path());
        dir
    }

    /// An untrusted clone must not get anything opened, however harmless the
    /// file looks; the refusal carries the marker the desktop already handles.
    #[cfg(unix)]
    #[test]
    fn open_refuses_an_untrusted_repository_without_asking() {
        let dir = repo_with(&["notes.txt"]);
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status_locked()
            .expect("git init");
        assert!(init.success());
        let root = dir.path().to_string_lossy().into_owned();
        let err = prepare_open(&root, "notes.txt", &|_, _| panic!("must not prompt"))
            .expect_err("untrusted");
        assert!(
            err.contains(crate::repository_trust::REQUIRED),
            "{err}",
            err = err.message()
        );
    }

    /// Launchers and executables need a native Yes; a No refuses; plain
    /// documents open without a prompt.
    #[cfg(unix)]
    #[test]
    fn open_confirms_launchers_and_executables_but_not_documents() {
        let repo = trusted_repo_with(&[
            ("docs/readme.md", 0o644),
            ("run.command", 0o644),
            ("Tool.APP/Contents/x", 0o644),
            ("README", 0o755),
            ("x.fileloc", 0o644),
            ("x.terminal", 0o644),
        ]);
        let root = repo.path().to_string_lossy().into_owned();
        prepare_open(&root, "docs/readme.md", &|_, _| {
            panic!("a document must not prompt")
        })
        .expect("plain document");
        for risky in ["run.command", "README", "x.fileloc", "x.terminal"] {
            let asked = std::cell::Cell::new(false);
            let refused = prepare_open(&root, risky, &|_, _| {
                asked.set(true);
                false
            })
            .expect_err(risky);
            assert!(asked.get(), "{risky} must raise a confirmation");
            assert!(
                refused.contains("not confirmed"),
                "{refused}",
                refused = refused.message()
            );
            prepare_open(&root, risky, &|_, _| true).expect("confirmed");
        }
        assert!(execution_risk(&repo.path().join("Tool.APP")).is_some());
    }

    /// An editor CLI hands the file to a window and exits, leaving the editor
    /// running. The runner reaps what it started when the launcher exits; this
    /// proves a process the launcher left behind (the editor, in production)
    /// is not taken down with it, through the production launcher path.
    #[cfg(unix)]
    #[test]
    fn an_editor_the_launcher_leaves_running_survives_the_launcher() {
        use std::os::unix::fs::PermissionsExt;
        let repo = trusted_repo_with(&[]);
        let bin = tempfile::tempdir().expect("bin dir");
        let script = bin.path().join("fake-editor.sh");
        fs::write(
            &script,
            "#!/bin/sh\nsleep 60 </dev/null >/dev/null 2>&1 &\necho $! > \"$1\"\nexit 0\n",
        )
        .expect("script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("mode");
        let pidfile = bin.path().join("editor.pid");

        let (success, _) = editor::run_launcher(
            script.to_str().expect("utf8"),
            &[pidfile.to_string_lossy().into_owned()],
            &repo.path().canonicalize().expect("root"),
        )
        .expect("launcher ran");
        assert!(success, "the fake launcher exits 0");

        let pid = fs::read_to_string(&pidfile)
            .expect("the launcher recorded the editor it left running")
            .trim()
            .to_string();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .status_locked()
            .expect("kill -0")
            .success();
        // Stop the stand-in editor this test started, by the pid it recorded.
        let _ = std::process::Command::new("kill").arg(&pid).status_locked();
        assert!(
            alive,
            "the editor left running by the launcher was killed with it"
        );
    }

    /// Every refusal below happens before a launcher is chosen, so these never
    /// start an editor, whichever are installed on the machine running them.
    #[cfg(unix)]
    #[test]
    fn open_in_editor_refuses_an_untrusted_repository() {
        let dir = repo_with(&["notes.txt"]);
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status_locked()
            .expect("git init");
        assert!(init.success());
        let root = dir.path().to_string_lossy().into_owned();
        let err = cmd_open_in_editor(root, "notes.txt".into(), Some(1), None, None)
            .expect_err("untrusted");
        assert_eq!(
            err.kind,
            crate::ipc_error::IpcErrorKind::RepositoryTrustRequired
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_in_editor_refuses_paths_outside_the_repository() {
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("secret.txt"), b"x").expect("file");
        let repo = trusted_repo_with(&[("src/a.rs", 0o644)]);
        std::os::unix::fs::symlink(outside.path(), repo.path().join("link")).expect("symlink");
        let root = repo.path().to_string_lossy().into_owned();
        for escape in [
            "../outside.txt",
            "/etc/passwd",
            "src/../../x",
            "link/secret.txt",
            "missing.rs",
            "",
        ] {
            let err = cmd_open_in_editor(root.clone(), escape.into(), Some(1), None, None)
                .expect_err(escape);
            // The path gate's own wording, so a refusal later than the gate
            // (the editor lookup, or a launcher actually starting) fails here.
            assert!(
                [
                    "outside the repository",
                    "Not a repository-relative path",
                    "Cannot read",
                    "No file path was given",
                ]
                .iter()
                .any(|reason| err.message.contains(reason)),
                "{escape:?} was not refused by the path gate: {err}"
            );
        }
    }
}
