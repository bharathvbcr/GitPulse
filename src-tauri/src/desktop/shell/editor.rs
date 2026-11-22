//! Opening a repository file in the user's own editor, at a line.
//!
//! GitPulse does not host VS Code extensions (they need Node's extension host
//! and the `vscode` API), so the way to keep a user's extensions working is to
//! hand the file to the editor they already run. This module decides *which*
//! editor, builds its command line, and nothing else; the path rules and the
//! trust gate are [`super::resolve_worktree_path`] and
//! [`crate::repository_trust::require`], the same ones the OS-default open
//! passes through.
//!
//! The webview never names a program. It sends a closed [`EditorChoice`]; the
//! program names and flags are the constants below, so a hostile page cannot
//! turn this command into "run anything".

use crate::ipc_error::{CommandError, IpcErrorKind};
use serde::Deserialize;
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// An editor CLI hands the file to the running window (or starts one) and
/// exits; a first launch can take a few seconds, none should take this long.
const EDITOR_CLI_TIMEOUT: Duration = Duration::from_secs(20);

/// The editor the user picked. `Auto` is the default: the first of the known
/// editors that is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorChoice {
    #[default]
    Auto,
    Code,
    Cursor,
    Zed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EditorSpec {
    choice: EditorChoice,
    /// What the user calls it, for messages.
    display: &'static str,
    /// The command-line launcher, searched on the GUI-launch PATH.
    program: &'static str,
    /// VS Code-family launchers take the location after `-g`; Zed takes it
    /// bare, as `path:line:col`.
    goto_flag: bool,
    /// The launcher inside the macOS app bundle. A Dock-launched app has
    /// neither Homebrew nor the editor's own "install shell command" symlink
    /// on its PATH, and an editor installed only as an app has no symlink at
    /// all, so the bundle is looked at first. It is a fixed absolute path, not
    /// something a repository or a PATH entry can substitute.
    macos_bundle_cli: &'static str,
}

/// In `Auto` order. The array is the whole allowlist of programs this module
/// will ever start.
const EDITORS: [EditorSpec; 3] = [
    EditorSpec {
        choice: EditorChoice::Code,
        display: "Visual Studio Code",
        program: "code",
        goto_flag: true,
        macos_bundle_cli: "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
    },
    EditorSpec {
        choice: EditorChoice::Cursor,
        display: "Cursor",
        program: "cursor",
        goto_flag: true,
        macos_bundle_cli: "/Applications/Cursor.app/Contents/Resources/app/bin/cursor",
    },
    EditorSpec {
        choice: EditorChoice::Zed,
        display: "Zed",
        program: "zed",
        goto_flag: false,
        macos_bundle_cli: "/Applications/Zed.app/Contents/MacOS/cli",
    },
];

/// Why a file was not opened in an editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EditorOpenError {
    /// `Auto` found none of the known editors.
    NoEditorFound,
    /// The chosen editor's launcher is not installed. Never answered by
    /// quietly opening a different editor.
    NotInstalled(&'static str),
    /// The line, column or path cannot be written as an editor argument.
    InvalidLocation(String),
    /// The launcher ran and said no, or could not be started.
    Failed {
        editor: &'static str,
        detail: String,
    },
}

impl fmt::Display for EditorOpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEditorFound => f.write_str(
                "No editor was found. Install the command-line launcher for Visual Studio Code \
                 (`code`), Cursor (`cursor`) or Zed (`zed`), or choose an editor in Settings.",
            ),
            Self::NotInstalled(editor) => write!(
                f,
                "{editor} is not installed, or its command-line launcher is not on this app's \
                 search path. Install the launcher or choose another editor in Settings."
            ),
            Self::InvalidLocation(why) => f.write_str(why),
            Self::Failed { editor, detail } => {
                write!(f, "{editor} could not open the file: {detail}")
            }
        }
    }
}

impl From<EditorOpenError> for CommandError {
    fn from(error: EditorOpenError) -> Self {
        let message = error.to_string();
        match error {
            EditorOpenError::NoEditorFound | EditorOpenError::NotInstalled(_) => {
                CommandError::typed(IpcErrorKind::EditorNotFound, message)
            }
            _ => CommandError::plain(message),
        }
    }
}

/// The program and arguments that open `file` at `line`/`column`.
///
/// `column` means nothing without a `line` and is dropped, rather than being
/// sent as a bare number the editor would read as part of the path. A line or
/// column of zero is refused: both are 1-based in every editor here, and
/// "0" is a caller bug worth hearing about, not a value to round up.
fn editor_args(
    spec: &EditorSpec,
    file: &Path,
    line: Option<u32>,
    column: Option<u32>,
) -> Result<Vec<String>, EditorOpenError> {
    let text = file.to_str().ok_or_else(|| {
        EditorOpenError::InvalidLocation(format!(
            "The path is not valid text and cannot be passed to an editor: {}",
            file.display()
        ))
    })?;
    if line == Some(0) || column == Some(0) {
        return Err(EditorOpenError::InvalidLocation(
            "Lines and columns start at 1.".to_string(),
        ));
    }
    let Some(line) = line else {
        // No position: the plain path, which no editor reads a suffix from.
        return Ok(vec![text.to_string()]);
    };
    // `path:line:col` is parsed from the right, so a file whose own name ends
    // in `:digits` would be read as a different file and line. Refused instead
    // of guessed; a drive letter earlier in the path is not part of the name.
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.contains(':') {
        return Err(EditorOpenError::InvalidLocation(format!(
            "{name} has a colon in its name, so a line cannot be added to it without \
             changing which file the editor opens."
        )));
    }
    let target = match column {
        Some(column) => format!("{text}:{line}:{column}"),
        None => format!("{text}:{line}"),
    };
    Ok(if spec.goto_flag {
        vec!["-g".to_string(), target]
    } else {
        vec![target]
    })
}

/// Which launcher to run: the editor the user chose, or the first installed.
///
/// `on_path` and `bundle_exists` are passed in so the choice is testable
/// without installing three editors; production passes the GUI-launch PATH
/// search ([`crate::engine::process::find_external_tool`], the one owner of
/// that rule) and a file check. `use_bundles` is true on macOS.
fn resolve_editor(
    choice: EditorChoice,
    use_bundles: bool,
    bundle_exists: &dyn Fn(&str) -> bool,
    on_path: &dyn Fn(&str) -> Option<String>,
) -> Result<(EditorSpec, String), EditorOpenError> {
    let candidates: Vec<&EditorSpec> = EDITORS
        .iter()
        .filter(|spec| choice == EditorChoice::Auto || spec.choice == choice)
        .collect();
    for spec in &candidates {
        if use_bundles && bundle_exists(spec.macos_bundle_cli) {
            return Ok((**spec, spec.macos_bundle_cli.to_string()));
        }
        if let Some(found) = on_path(spec.program) {
            return Ok((**spec, found));
        }
    }
    Err(match candidates.as_slice() {
        [only] if choice != EditorChoice::Auto => EditorOpenError::NotInstalled(only.display),
        _ => EditorOpenError::NoEditorFound,
    })
}

/// Everything [`open_in_editor`] needs from the environment, so a test can run
/// the whole decision with a fake launcher.
pub(super) struct EditorEnv<'a> {
    pub use_bundles: bool,
    pub bundle_exists: &'a dyn Fn(&str) -> bool,
    pub on_path: &'a dyn Fn(&str) -> Option<String>,
    /// Runs the launcher and returns its exit status and stderr, or why it
    /// could not run.
    pub run: &'a dyn Fn(&str, &[String], &Path) -> Result<(bool, String), CommandError>,
}

/// Opens `repo`/`relative` in an editor. The path has already been proven
/// inside the trusted repository by the caller.
pub(super) fn open_in_editor(
    repo_root: &Path,
    file: &Path,
    line: Option<u32>,
    column: Option<u32>,
    choice: EditorChoice,
    env: &EditorEnv<'_>,
) -> Result<(), CommandError> {
    if !file.is_file() {
        return Err(EditorOpenError::InvalidLocation(format!(
            "{} is not a file, so there is nothing to open at a line.",
            file.display()
        ))
        .into());
    }
    let (spec, program) = resolve_editor(choice, env.use_bundles, env.bundle_exists, env.on_path)?;
    let args = editor_args(&spec, file, line, column)?;
    let (success, stderr) = (env.run)(&program, &args, repo_root)?;
    if success {
        return Ok(());
    }
    Err(EditorOpenError::Failed {
        editor: spec.display,
        detail: first_line(&stderr),
    }
    .into())
}

/// The launcher's own explanation, bounded: stderr is an editor's, not ours,
/// and can be long or empty.
fn first_line(stderr: &str) -> String {
    let line = stderr.lines().map(str::trim).find(|l| !l.is_empty());
    match line {
        Some(line) => line.chars().take(300).collect(),
        None => "it exited with an error and no message".to_string(),
    }
}

/// Production environment: the GUI-launch PATH search, the real bundle, and
/// the bounded, gated process runner with the repository as working directory.
pub(super) fn run_launcher(
    program: &str,
    args: &[String],
    cwd: &Path,
) -> Result<(bool, String), CommandError> {
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = crate::engine::process::capture_command(
        program,
        &argv,
        Some(cwd),
        EDITOR_CLI_TIMEOUT,
        &[],
    )?;
    Ok((
        out.success,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod editor_tests;
