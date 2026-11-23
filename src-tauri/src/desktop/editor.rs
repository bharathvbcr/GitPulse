//! "Open this repository file in my editor", at a line when one is known.
//!
//! GitPulse cannot host editor extensions, so it hands the file to the editor
//! the user already has. The webview picks from a **closed** set
//! ([`EditorChoice`]); the program names and flags in here are constants, so
//! the page can never name a program to run, only which of four to prefer.
//!
//! The path is validated by the same gate "open with the default app" uses
//! ([`super::shell::resolve_trusted`]): repository root and relative path
//! arrive separately, the absolute path is rebuilt and proven inside the
//! canonical root, and an untrusted repository is refused. The execute-bit and
//! launcher-extension confirmation that gate adds for the OS default app is
//! not applied: an editor shows a file, it never runs it.
//!
//! A chosen editor that is not installed is an error that names it
//! ([`IpcErrorKind::EditorNotFound`]); it never falls back to another editor,
//! because the user said which one they wanted. Only `Auto` looks at more than
//! one, in a fixed order.

use crate::engine::process::{self, RunOutcome};
use crate::ipc_error::{CommandError, IpcError, IpcErrorKind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a launcher may take to hand the file over. These CLIs return as
/// soon as the running editor accepts the request (or once they have started
/// it), so this is generous; it bounds a launcher that wedges.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

/// How much of a failing launcher's stderr is shown.
const STDERR_TAIL_BYTES: usize = 2048;

/// Which editor to hand a file to. Spelled `auto`, `vscode`, `antigravity`,
/// `cursor`, `zed` on the wire; `src/lib/desktop/editorChoice.ts` mirrors it.
/// `antigravity` is the Antigravity IDE (a VS Code fork), not the separate
/// Antigravity agent app, which has no file-open launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorChoice {
    /// The first installed of VS Code, Antigravity IDE, Cursor, Zed, in that order.
    #[default]
    Auto,
    Vscode,
    Antigravity,
    Cursor,
    Zed,
}

/// One concrete editor: everything about launching it is a constant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Editor {
    VsCode,
    Antigravity,
    Cursor,
    Zed,
}

/// What `Auto` tries, in order.
const AUTO_ORDER: [Editor; 4] = [
    Editor::VsCode,
    Editor::Antigravity,
    Editor::Cursor,
    Editor::Zed,
];

impl Editor {
    fn from_choice(choice: EditorChoice) -> Option<Self> {
        match choice {
            EditorChoice::Auto => None,
            EditorChoice::Vscode => Some(Self::VsCode),
            EditorChoice::Antigravity => Some(Self::Antigravity),
            EditorChoice::Cursor => Some(Self::Cursor),
            EditorChoice::Zed => Some(Self::Zed),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::VsCode => "VS Code",
            Self::Antigravity => "Antigravity IDE",
            Self::Cursor => "Cursor",
            Self::Zed => "Zed",
        }
    }

    /// The command name looked up on the GUI-launch `PATH`.
    fn program(self) -> &'static str {
        match self {
            Self::VsCode => "code",
            Self::Antigravity => "antigravity-ide",
            Self::Cursor => "cursor",
            Self::Zed => "zed",
        }
    }

    /// Where the app bundle keeps its command-line launcher on macOS, as the
    /// directory and the file in it. Checked before `PATH`: the shell command
    /// is an optional install step, the bundle is where the app already is.
    fn bundle_cli(self) -> (&'static str, &'static str) {
        match self {
            Self::VsCode => (
                "/Applications/Visual Studio Code.app/Contents/Resources/app/bin",
                "code",
            ),
            Self::Antigravity => (
                "/Applications/Antigravity IDE.app/Contents/Resources/app/bin",
                "antigravity-ide",
            ),
            Self::Cursor => (
                "/Applications/Cursor.app/Contents/Resources/app/bin",
                "cursor",
            ),
            Self::Zed => ("/Applications/Zed.app/Contents/MacOS", "cli"),
        }
    }

    /// The arguments that open `target` (an absolute path), at `line` and
    /// `column` when given. VS Code, Antigravity IDE and Cursor take
    /// `-g path:line[:col]`; Zed
    /// takes the bare `path:line[:col]`. No line is a plain path.
    fn args(self, target: &str, line: Option<u32>, column: Option<u32>) -> Vec<String> {
        let Some(line) = line else {
            return vec![target.to_string()];
        };
        let spec = match column {
            Some(column) => format!("{target}:{line}:{column}"),
            None => format!("{target}:{line}"),
        };
        match self {
            Self::VsCode | Self::Antigravity | Self::Cursor => vec!["-g".to_string(), spec],
            Self::Zed => vec![spec],
        }
    }

    /// Whether this editor would read a trailing `:<digits>` of a plain path
    /// as a position. Zed's bare path form does; `-g` is the only form VS Code,
    /// Antigravity IDE and Cursor read it from.
    fn reads_position_from_bare_path(self) -> bool {
        matches!(self, Self::Zed)
    }
}

/// The error for an editor that is not installed, typed so the frontend can
/// point at the setting. Never wrapped: wrapping would drop the kind.
pub(crate) fn not_found(choice: EditorChoice, message: String) -> CommandError {
    CommandError::typed_with_detail(
        IpcErrorKind::EditorNotFound,
        message,
        serde_json::json!({ "editor": choice }),
    )
}

/// The installed launcher for `editor`, from its app bundle (macOS only) and
/// then the GUI-launch `PATH`. Both lookups are injected so tests need no
/// editor installed.
fn locate(
    editor: Editor,
    macos: bool,
    find_in_dir: &dyn Fn(&str, &Path) -> Option<PathBuf>,
    find_on_path: &dyn Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    if macos {
        let (dir, file) = editor.bundle_cli();
        if let Some(found) = find_in_dir(file, Path::new(dir)) {
            return Some(found);
        }
    }
    find_on_path(editor.program()).map(PathBuf::from)
}

/// The editor to run and where its launcher is.
fn resolve_editor(
    choice: EditorChoice,
    macos: bool,
    find_in_dir: &dyn Fn(&str, &Path) -> Option<PathBuf>,
    find_on_path: &dyn Fn(&str) -> Option<String>,
) -> Result<(Editor, PathBuf), CommandError> {
    if let Some(editor) = Editor::from_choice(choice) {
        return locate(editor, macos, find_in_dir, find_on_path)
            .map(|launcher| (editor, launcher))
            .ok_or_else(|| {
                not_found(
                    choice,
                    format!(
                        "{} is not installed: no `{}` command was found. Install it, or choose \
                         another editor in Settings.",
                        editor.label(),
                        editor.program()
                    ),
                )
            });
    }
    AUTO_ORDER
        .iter()
        .find_map(|&editor| {
            locate(editor, macos, find_in_dir, find_on_path).map(|launcher| (editor, launcher))
        })
        .ok_or_else(|| {
            not_found(
                choice,
                "No supported editor was found: looked for VS Code (`code`), Antigravity IDE \
                 (`antigravity-ide`), Cursor (`cursor`) and Zed (`zed`). Install one, or open the file another way."
                    .to_string(),
            )
        })
}

/// Refuses a position the editor would read as something else.
fn check_position(
    editor: Editor,
    target: &Path,
    line: Option<u32>,
    column: Option<u32>,
) -> Result<(), CommandError> {
    if line == Some(0) {
        return Err("Line numbers start at 1".into());
    }
    if column == Some(0) {
        return Err("Column numbers start at 1".into());
    }
    if column.is_some() && line.is_none() {
        return Err("A column needs a line".into());
    }
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if line.is_some() && target.is_dir() {
        return Err("A line can only be opened in a file, not a folder".into());
    }
    // `path:12` is how a position is spelled, so a name that already holds a
    // colon would change which file opens: `a:5` + line 12 is `a:5:12`, which
    // reads as file `a` at line 5, column 12.
    if line.is_some() && name.contains(':') {
        return Err(format!(
            "Cannot open {name} at a line: a ':' in the file name would be read as part of the \
             position. Open it without a line."
        )
        .into());
    }
    if line.is_none() && editor.reads_position_from_bare_path() {
        if let Some((_, tail)) = name.rsplit_once(':') {
            if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!(
                    "{} would read the end of {name} as a line number, so it would open a \
                     different file.",
                    editor.label()
                )
                .into());
            }
        }
    }
    Ok(())
}

/// How a launcher run ended, without the runner's types so tests can fake it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    Finished {
        success: bool,
        status: i32,
        stderr: String,
    },
    TimedOut,
}

/// Runs a launcher with its arguments: [`run_launcher`] in production, a fake
/// in tests.
type RunLauncher<'a> = dyn Fn(&Path, &[String]) -> Result<Launch, CommandError> + 'a;

/// The whole operation with every outside effect injected.
#[allow(clippy::too_many_arguments)]
fn open_with(
    target: &Path,
    relative: &str,
    choice: EditorChoice,
    line: Option<u32>,
    column: Option<u32>,
    macos: bool,
    find_in_dir: &dyn Fn(&str, &Path) -> Option<PathBuf>,
    find_on_path: &dyn Fn(&str) -> Option<String>,
    run: &RunLauncher<'_>,
) -> Result<(), CommandError> {
    let (editor, launcher) = resolve_editor(choice, macos, find_in_dir, find_on_path)?;
    check_position(editor, target, line, column)?;
    // The absolute path the gate built, so no argument can begin with `-`.
    let path = process::plain_path_text(&target.to_string_lossy());
    let args = editor.args(&path, line, column);
    match run(&launcher, &args)? {
        Launch::Finished { success: true, .. } => Ok(()),
        Launch::Finished {
            success: false,
            status,
            stderr,
        } => {
            let detail = if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            };
            Err(format!(
                "{} could not open {relative} (exit status {status}){detail}",
                editor.label()
            )
            .into())
        }
        Launch::TimedOut => Err(format!(
            "{} did not answer within {} seconds, so {relative} may not have opened",
            editor.label(),
            LAUNCH_TIMEOUT.as_secs()
        )
        .into()),
    }
}

/// Runs the launcher through the bounded process runner. A launcher whose
/// editor keeps its pipes open after it exits comes back with a cut stderr
/// rather than an error, which is right: the launch itself succeeded.
fn run_launcher(launcher: &Path, args: &[String]) -> Result<Launch, CommandError> {
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match process::run_captured(
        &launcher.to_string_lossy(),
        &argv,
        None,
        LAUNCH_TIMEOUT,
        &[],
        STDERR_TAIL_BYTES,
    )? {
        RunOutcome::Finished(run) => Ok(Launch::Finished {
            success: run.success,
            status: run.status_code,
            stderr: run.stderr_tail.trim().to_string(),
        }),
        RunOutcome::TimedOut(_) => Ok(Launch::TimedOut),
    }
}

/// The production wiring of [`open_with`].
fn open_in_editor(
    target: &Path,
    relative: &str,
    choice: EditorChoice,
    line: Option<u32>,
    column: Option<u32>,
) -> Result<(), CommandError> {
    open_with(
        target,
        relative,
        choice,
        line,
        column,
        cfg!(target_os = "macos"),
        &|file, dir| process::find_in_dirs(file, &[dir.to_path_buf()]),
        &process::find_external_tool,
        &run_launcher,
    )
}

/// Opens a repo-relative file in the user's editor, at `line` (and `column`)
/// when given, both 1-based.
///
/// A user action: it is admitted as one, and its error never asks to be
/// retried. Its kind survives to the webview because nothing here wraps it.
#[tauri::command(async)]
pub async fn cmd_open_in_editor(
    repo: String,
    relative: String,
    line: Option<u32>,
    column: Option<u32>,
    editor: Option<EditorChoice>,
) -> Result<(), IpcError> {
    crate::commands::off_thread(move || {
        let target = super::shell::resolve_trusted(&repo, &relative)?;
        process::mark_user_action(&repo);
        open_in_editor(&target, &relative, editor.unwrap_or_default(), line, column)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::rc::Rc;

    fn file_in_temp(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(name);
        fs::write(&path, b"x").expect("write");
        (dir, path)
    }

    /// Lookups that find exactly the named launchers.
    struct Installed {
        bundles: Vec<&'static str>,
        on_path: Vec<&'static str>,
    }

    impl Installed {
        fn none() -> Self {
            Self {
                bundles: vec![],
                on_path: vec![],
            }
        }
    }

    fn run_ok() -> impl Fn(&Path, &[String]) -> Result<Launch, CommandError> {
        |_, _| {
            Ok(Launch::Finished {
                success: true,
                status: 0,
                stderr: String::new(),
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        installed: &Installed,
        target: &Path,
        choice: EditorChoice,
        line: Option<u32>,
        column: Option<u32>,
        macos: bool,
        run: &RunLauncher<'_>,
    ) -> Result<(), CommandError> {
        open_with(
            target,
            "src/main.rs",
            choice,
            line,
            column,
            macos,
            &|file, dir| {
                installed
                    .bundles
                    .iter()
                    .any(|bundle| Path::new(bundle) == dir.join(file))
                    .then(|| dir.join(file))
            },
            &|name| {
                installed
                    .on_path
                    .contains(&name)
                    .then(|| format!("/fake/bin/{name}"))
            },
            run,
        )
    }

    /// Every launcher `run` was asked to execute, with its arguments.
    type Calls = Rc<RefCell<Vec<(PathBuf, Vec<String>)>>>;

    /// What `run` was asked to execute.
    fn recording() -> (
        Calls,
        impl Fn(&Path, &[String]) -> Result<Launch, CommandError>,
    ) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&calls);
        let record = move |program: &Path, args: &[String]| {
            seen.borrow_mut()
                .push((program.to_path_buf(), args.to_vec()));
            Ok(Launch::Finished {
                success: true,
                status: 0,
                stderr: String::new(),
            })
        };
        (calls, record)
    }

    #[test]
    fn code_antigravity_and_cursor_take_g_with_the_position_and_zed_takes_it_bare() {
        for editor in [Editor::VsCode, Editor::Antigravity, Editor::Cursor] {
            assert_eq!(
                editor.args("/r/a.rs", Some(12), None),
                ["-g", "/r/a.rs:12"],
                "{editor:?}"
            );
            assert_eq!(
                editor.args("/r/a.rs", Some(12), Some(3)),
                ["-g", "/r/a.rs:12:3"],
                "{editor:?}"
            );
        }
        assert_eq!(Editor::Zed.args("/r/a.rs", Some(12), None), ["/r/a.rs:12"]);
        assert_eq!(
            Editor::Zed.args("/r/a.rs", Some(12), Some(3)),
            ["/r/a.rs:12:3"]
        );
    }

    #[test]
    fn no_line_is_the_plain_path_for_every_editor() {
        for editor in AUTO_ORDER {
            assert_eq!(
                editor.args("/r/a.rs", None, None),
                ["/r/a.rs"],
                "{editor:?}"
            );
        }
    }

    #[test]
    fn the_wire_spelling_of_the_choice_is_closed() {
        for (text, choice) in [
            ("auto", EditorChoice::Auto),
            ("vscode", EditorChoice::Vscode),
            ("antigravity", EditorChoice::Antigravity),
            ("cursor", EditorChoice::Cursor),
            ("zed", EditorChoice::Zed),
        ] {
            let parsed: EditorChoice = serde_json::from_str(&format!("\"{text}\"")).expect(text);
            assert_eq!(parsed, choice);
            assert_eq!(
                serde_json::to_string(&choice).unwrap(),
                format!("\"{text}\"")
            );
        }
        // The page cannot name a program: anything else does not deserialize.
        for other in [
            "\"code\"",
            "\"/usr/bin/vim\"",
            "\"vim\"",
            "\"\"",
            "\"VSCODE\"",
        ] {
            assert!(
                serde_json::from_str::<EditorChoice>(other).is_err(),
                "{other} must not parse"
            );
        }
    }

    #[test]
    fn auto_prefers_the_bundle_then_path_then_the_next_editor() {
        let bundle_code = "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code";
        let (_dir, target) = file_in_temp("a.rs");

        // Bundle wins over PATH for the same editor.
        let (calls, run) = recording();
        open(
            &Installed {
                bundles: vec![bundle_code],
                on_path: vec!["code"],
            },
            &target,
            EditorChoice::Auto,
            None,
            None,
            true,
            &run,
        )
        .expect("opens");
        assert_eq!(calls.borrow()[0].0, PathBuf::from(bundle_code));

        // Not in the bundle: the PATH command.
        let (calls, run) = recording();
        open(
            &Installed {
                bundles: vec![],
                on_path: vec!["code"],
            },
            &target,
            EditorChoice::Auto,
            None,
            None,
            true,
            &run,
        )
        .expect("opens");
        assert_eq!(calls.borrow()[0].0, PathBuf::from("/fake/bin/code"));

        // Auto skips an editor that is not there and takes the next one.
        let (calls, run) = recording();
        open(
            &Installed {
                bundles: vec![],
                on_path: vec!["zed", "cursor"],
            },
            &target,
            EditorChoice::Auto,
            None,
            None,
            true,
            &run,
        )
        .expect("opens");
        assert_eq!(calls.borrow()[0].0, PathBuf::from("/fake/bin/cursor"));
    }

    #[test]
    fn auto_picks_antigravity_ide_over_cursor_when_vs_code_is_absent() {
        let bundle_antigravity =
            "/Applications/Antigravity IDE.app/Contents/Resources/app/bin/antigravity-ide";
        let bundle_cursor = "/Applications/Cursor.app/Contents/Resources/app/bin/cursor";
        let (_dir, target) = file_in_temp("a.rs");

        // Both installed as bundles, VS Code not: Antigravity IDE is earlier in the order.
        let (calls, run) = recording();
        open(
            &Installed {
                bundles: vec![bundle_cursor, bundle_antigravity],
                on_path: vec![],
            },
            &target,
            EditorChoice::Auto,
            Some(9),
            None,
            true,
            &run,
        )
        .expect("opens");
        assert_eq!(calls.borrow()[0].0, PathBuf::from(bundle_antigravity));
        assert_eq!(
            calls.borrow()[0].1,
            ["-g".to_string(), format!("{}:9", target.display())]
        );

        // Found on PATH instead of a bundle: the program is `antigravity-ide`.
        let (calls, run) = recording();
        open(
            &Installed {
                bundles: vec![],
                on_path: vec!["cursor", "antigravity-ide"],
            },
            &target,
            EditorChoice::Auto,
            None,
            None,
            true,
            &run,
        )
        .expect("opens");
        assert_eq!(
            calls.borrow()[0].0,
            PathBuf::from("/fake/bin/antigravity-ide")
        );
    }

    #[test]
    fn a_bundle_is_only_looked_at_on_macos() {
        let bundle_code = "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code";
        let (_dir, target) = file_in_temp("a.rs");
        let installed = Installed {
            bundles: vec![bundle_code],
            on_path: vec![],
        };
        let err = open(
            &installed,
            &target,
            EditorChoice::Vscode,
            None,
            None,
            false,
            &run_ok(),
        )
        .expect_err("a macOS bundle path is not a launcher elsewhere");
        assert_eq!(err.kind(), IpcErrorKind::EditorNotFound);
    }

    #[test]
    fn a_chosen_editor_that_is_missing_is_an_error_naming_it_and_never_falls_back() {
        let (_dir, target) = file_in_temp("a.rs");
        // Cursor is installed; the user asked for Zed.
        let installed = Installed {
            bundles: vec![],
            on_path: vec!["cursor", "code"],
        };
        let ran = RefCell::new(false);
        let err = open(
            &installed,
            &target,
            EditorChoice::Zed,
            Some(3),
            None,
            true,
            &|_, _| {
                *ran.borrow_mut() = true;
                Ok(Launch::TimedOut)
            },
        )
        .expect_err("Zed is not installed");
        assert!(!*ran.borrow(), "no other editor may be launched");
        assert_eq!(err.kind(), IpcErrorKind::EditorNotFound);
        assert!(err.message().contains("Zed"), "{}", err.message());
        assert!(!err.message().contains("Cursor"), "{}", err.message());
    }

    #[test]
    fn auto_with_nothing_installed_is_editor_not_found_and_names_what_it_tried() {
        let (_dir, target) = file_in_temp("a.rs");
        let err = open(
            &Installed::none(),
            &target,
            EditorChoice::Auto,
            None,
            None,
            true,
            &run_ok(),
        )
        .expect_err("nothing installed");
        assert_eq!(err.kind(), IpcErrorKind::EditorNotFound);
        for name in ["VS Code", "Antigravity IDE", "Cursor", "Zed"] {
            assert!(err.message().contains(name), "{}", err.message());
        }
    }

    #[test]
    fn the_not_found_error_reaches_the_webview_typed_with_the_choice() {
        let (_dir, target) = file_in_temp("a.rs");
        let sent = crate::ipc_error::run_command(|| {
            open(
                &Installed::none(),
                &target,
                EditorChoice::Cursor,
                None,
                None,
                true,
                &run_ok(),
            )
        })
        .expect_err("not installed");
        assert_eq!(sent.kind, IpcErrorKind::EditorNotFound);
        assert_eq!(sent.detail, Some(serde_json::json!({ "editor": "cursor" })));
    }

    #[test]
    fn the_launcher_gets_the_absolute_path_and_the_position() {
        let (_dir, target) = file_in_temp("a.rs");
        let (calls, run) = recording();
        open(
            &Installed {
                bundles: vec![],
                on_path: vec!["code", "zed"],
            },
            &target,
            EditorChoice::Vscode,
            Some(42),
            Some(7),
            false,
            &run,
        )
        .expect("opens");
        let spec = format!("{}:42:7", target.display());
        assert_eq!(calls.borrow()[0].1, ["-g".to_string(), spec]);

        let (calls, run) = recording();
        open(
            &Installed {
                bundles: vec![],
                on_path: vec!["zed"],
            },
            &target,
            EditorChoice::Zed,
            Some(42),
            None,
            false,
            &run,
        )
        .expect("opens");
        assert_eq!(calls.borrow()[0].1, [format!("{}:42", target.display())]);
    }

    #[test]
    fn a_zero_line_or_column_and_a_column_without_a_line_are_refused_before_launching() {
        let (_dir, target) = file_in_temp("a.rs");
        let installed = Installed {
            bundles: vec![],
            on_path: vec!["code"],
        };
        for (line, column) in [(Some(0), None), (Some(1), Some(0)), (None, Some(3))] {
            let err = open(
                &installed,
                &target,
                EditorChoice::Vscode,
                line,
                column,
                false,
                &|_, _| panic!("must not launch for {line:?}:{column:?}"),
            )
            .expect_err("refused");
            assert!(!err.message().is_empty());
        }
    }

    #[test]
    fn a_colon_in_the_file_name_refuses_a_line_but_not_a_plain_open() {
        let (_dir, target) = file_in_temp("a:5");
        let installed = Installed {
            bundles: vec![],
            on_path: vec!["code", "zed"],
        };
        let err = open(
            &installed,
            &target,
            EditorChoice::Vscode,
            Some(12),
            None,
            false,
            &|_, _| panic!("must not launch"),
        )
        .expect_err("would open file `a` instead");
        assert!(err.message().contains("a:5"), "{}", err.message());

        // No line: VS Code reads the path whole.
        open(
            &installed,
            &target,
            EditorChoice::Vscode,
            None,
            None,
            false,
            &run_ok(),
        )
        .expect("plain open");
        // Zed's bare path would read `:5` as a line, so even a plain open is refused.
        open(
            &installed,
            &target,
            EditorChoice::Zed,
            None,
            None,
            false,
            &|_, _| panic!("must not launch"),
        )
        .expect_err("Zed would open `a` at line 5");
    }

    #[test]
    fn a_line_on_a_folder_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let installed = Installed {
            bundles: vec![],
            on_path: vec!["code"],
        };
        open(
            &installed,
            dir.path(),
            EditorChoice::Vscode,
            Some(1),
            None,
            false,
            &|_, _| panic!("must not launch"),
        )
        .expect_err("folders have no lines");
        open(
            &installed,
            dir.path(),
            EditorChoice::Vscode,
            None,
            None,
            false,
            &run_ok(),
        )
        .expect("a folder opens plainly");
    }

    #[test]
    fn a_failing_launcher_surfaces_its_status_and_stderr() {
        let (_dir, target) = file_in_temp("a.rs");
        let installed = Installed {
            bundles: vec![],
            on_path: vec!["code"],
        };
        let err = open(
            &installed,
            &target,
            EditorChoice::Vscode,
            None,
            None,
            false,
            &|_, _| {
                Ok(Launch::Finished {
                    success: false,
                    status: 3,
                    stderr: "bad option: --nope".to_string(),
                })
            },
        )
        .expect_err("launcher failed");
        let text = err.message();
        assert!(text.contains("VS Code"), "{text}");
        assert!(text.contains("src/main.rs"), "{text}");
        assert!(text.contains("status 3"), "{text}");
        assert!(text.contains("bad option: --nope"), "{text}");
        assert_eq!(err.kind(), IpcErrorKind::Unclassified);
    }

    #[test]
    fn a_launcher_that_never_answers_is_an_error_not_a_success() {
        let (_dir, target) = file_in_temp("a.rs");
        let installed = Installed {
            bundles: vec![],
            on_path: vec!["zed"],
        };
        let err = open(
            &installed,
            &target,
            EditorChoice::Zed,
            None,
            None,
            false,
            &|_, _| Ok(Launch::TimedOut),
        )
        .expect_err("timed out");
        assert!(
            err.message().contains("did not answer"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn a_launcher_that_cannot_start_surfaces_the_runners_error() {
        let (_dir, target) = file_in_temp("a.rs");
        let installed = Installed {
            bundles: vec![],
            on_path: vec!["code"],
        };
        let err = open(
            &installed,
            &target,
            EditorChoice::Vscode,
            None,
            None,
            false,
            &|_, _| Err("Failed to spawn code: no such program".into()),
        )
        .expect_err("spawn failed");
        assert!(
            err.message().contains("Failed to spawn"),
            "{}",
            err.message()
        );
    }

    /// The real runner, end to end through a script standing in for the
    /// editor's launcher: it must receive exactly the arguments built, a
    /// non-zero exit must carry its stderr back, and a zero exit is success.
    #[cfg(unix)]
    #[test]
    fn the_real_runner_passes_the_arguments_and_reports_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let log = dir.path().join("argv.txt");
        let ok = dir.path().join("fake-ok");
        fs::write(
            &ok,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", log.display()),
        )
        .expect("script");
        let bad = dir.path().join("fake-bad");
        fs::write(&bad, "#!/bin/sh\necho 'no such window' >&2\nexit 7\n").expect("script");
        for script in [&ok, &bad] {
            fs::set_permissions(script, fs::Permissions::from_mode(0o755)).expect("chmod");
        }

        let args = vec!["-g".to_string(), "/r/a.rs:3".to_string()];
        assert_eq!(
            run_launcher(&ok, &args).expect("runs"),
            Launch::Finished {
                success: true,
                status: 0,
                stderr: String::new()
            }
        );
        assert_eq!(
            fs::read_to_string(&log).expect("argv"),
            "-g\n/r/a.rs:3\n",
            "the launcher saw exactly the built arguments"
        );
        assert_eq!(
            run_launcher(&bad, &args).expect("runs"),
            Launch::Finished {
                success: false,
                status: 7,
                stderr: "no such window".to_string()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_command_refuses_an_untrusted_repository_before_looking_for_an_editor() {
        use crate::procguard::LockedSpawn;
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("a.rs"), b"x").expect("write");
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status_locked()
            .expect("git init");
        assert!(init.success());
        let root = dir.path().to_string_lossy().into_owned();
        let sent = crate::ipc_error::run_command(|| {
            super::super::shell::resolve_trusted(&root, "a.rs").map(|_| ())
        })
        .expect_err("untrusted");
        assert_eq!(sent.kind, IpcErrorKind::RepositoryTrustRequired);
    }

    #[test]
    fn the_command_refuses_paths_that_leave_the_repository() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_string_lossy().into_owned();
        for escape in ["../x", "/etc/passwd", "a/../../x", "C:/x", ""] {
            super::super::shell::resolve_trusted(&root, escape)
                .expect_err(&format!("{escape:?} must be refused"));
        }
    }
}
