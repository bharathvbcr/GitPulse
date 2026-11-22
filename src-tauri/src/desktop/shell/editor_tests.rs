use super::*;
use std::cell::RefCell;
use std::path::PathBuf;

fn spec(choice: EditorChoice) -> EditorSpec {
    *EDITORS
        .iter()
        .find(|s| s.choice == choice)
        .expect("a known editor")
}

fn args(choice: EditorChoice, line: Option<u32>, col: Option<u32>) -> Vec<String> {
    editor_args(&spec(choice), Path::new("/r/src/a.rs"), line, col).expect("arguments")
}

#[test]
fn a_position_is_written_the_way_each_editor_reads_it() {
    for choice in [EditorChoice::Code, EditorChoice::Cursor] {
        assert_eq!(args(choice, None, None), ["/r/src/a.rs"]);
        assert_eq!(args(choice, Some(7), None), ["-g", "/r/src/a.rs:7"]);
        assert_eq!(args(choice, Some(7), Some(3)), ["-g", "/r/src/a.rs:7:3"]);
    }
    assert_eq!(args(EditorChoice::Zed, None, None), ["/r/src/a.rs"]);
    assert_eq!(args(EditorChoice::Zed, Some(7), None), ["/r/src/a.rs:7"]);
    assert_eq!(
        args(EditorChoice::Zed, Some(7), Some(3)),
        ["/r/src/a.rs:7:3"]
    );
}

/// A column with no line would otherwise be dropped into the path as `:3`.
#[test]
fn a_column_without_a_line_is_not_sent() {
    assert_eq!(args(EditorChoice::Code, None, Some(3)), ["/r/src/a.rs"]);
    assert_eq!(args(EditorChoice::Zed, None, Some(3)), ["/r/src/a.rs"]);
}

#[test]
fn a_zero_line_or_column_is_refused_not_rounded_up() {
    for (line, col) in [(Some(0), None), (Some(1), Some(0)), (None, Some(0))] {
        let err = editor_args(&spec(EditorChoice::Code), Path::new("/r/a.rs"), line, col)
            .expect_err("zero is not a position");
        assert!(
            matches!(err, EditorOpenError::InvalidLocation(_)),
            "{err:?}"
        );
    }
}

/// `path:line` is parsed from the right: a file called `a:12` would open
/// `a` at line 12 instead. Refused when a position is asked for, fine without.
#[test]
fn a_file_whose_name_has_a_colon_cannot_carry_a_line() {
    let odd = Path::new("/r/notes:12");
    let err = editor_args(&spec(EditorChoice::Code), odd, Some(3), None).expect_err("ambiguous");
    assert!(
        matches!(err, EditorOpenError::InvalidLocation(_)),
        "{err:?}"
    );
    assert_eq!(
        editor_args(&spec(EditorChoice::Code), odd, None, None).expect("plain path"),
        ["/r/notes:12"]
    );
}

#[test]
fn a_path_with_spaces_and_leading_dashes_stays_one_argument() {
    // No shell is involved, so spaces need no quoting; and an absolute path
    // never starts with `-`, so it cannot be read as an option.
    let a = editor_args(
        &spec(EditorChoice::Code),
        Path::new("/r/my dir/-rf $HOME;x.rs"),
        Some(2),
        None,
    )
    .expect("arguments");
    assert_eq!(a, ["-g", "/r/my dir/-rf $HOME;x.rs:2"]);
}

fn nothing_on_path(_: &str) -> Option<String> {
    None
}

fn resolve(
    choice: EditorChoice,
    use_bundles: bool,
    bundles: &[&str],
    on_path: &[&str],
) -> Result<(EditorSpec, String), EditorOpenError> {
    resolve_editor(choice, use_bundles, &|p| bundles.contains(&p), &|program| {
        on_path
            .contains(&program)
            .then(|| format!("/usr/local/bin/{program}"))
    })
}

#[test]
fn auto_takes_the_first_installed_editor_in_code_cursor_zed_order() {
    let (spec, program) =
        resolve(EditorChoice::Auto, false, &[], &["zed", "cursor"]).expect("found");
    assert_eq!(spec.choice, EditorChoice::Cursor);
    assert_eq!(program, "/usr/local/bin/cursor");
    let (spec, _) =
        resolve(EditorChoice::Auto, false, &[], &["zed", "code", "cursor"]).expect("found");
    assert_eq!(spec.choice, EditorChoice::Code);
}

#[test]
fn a_chosen_editor_is_never_swapped_for_another() {
    let err = resolve(EditorChoice::Zed, false, &[], &["code", "cursor"]).expect_err("zed missing");
    assert_eq!(err, EditorOpenError::NotInstalled("Zed"));
}

#[test]
fn no_editor_at_all_is_a_typed_error_with_the_editor_not_found_kind() {
    let err = resolve(EditorChoice::Auto, true, &[], &[]).expect_err("none");
    assert_eq!(err, EditorOpenError::NoEditorFound);
    let command: CommandError = err.into();
    assert_eq!(command.origin(), Some(IpcErrorKind::EditorNotFound));
    let chosen: CommandError = EditorOpenError::NotInstalled("Zed").into();
    assert_eq!(chosen.origin(), Some(IpcErrorKind::EditorNotFound));
    // A refusal of the arguments is a plain failure, not "no editor".
    let invalid: CommandError = EditorOpenError::InvalidLocation("x".into()).into();
    assert_eq!(invalid.origin(), None);
}

/// The committed wire case for `editor_not_found` is what the producer really
/// sends, so the TypeScript mirror's fixture cannot drift from the words.
#[test]
fn the_no_editor_message_is_the_committed_wire_case() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../ipc_error/wire_cases.json")).expect("cases");
    let case = cases
        .iter()
        .find(|case| case["kind"] == "editor_not_found")
        .expect("a wire case for editor_not_found");
    assert_eq!(
        case["message"].as_str(),
        Some(EditorOpenError::NoEditorFound.to_string().as_str())
    );
}

#[test]
fn the_app_bundle_launcher_beats_the_path_one_only_when_bundles_are_in_use() {
    let bundle = spec(EditorChoice::Cursor).macos_bundle_cli;
    let (_, program) = resolve(EditorChoice::Cursor, true, &[bundle], &["cursor"]).expect("found");
    assert_eq!(program, bundle);
    let (_, program) = resolve(EditorChoice::Cursor, false, &[bundle], &["cursor"]).expect("found");
    assert_eq!(program, "/usr/local/bin/cursor");
}

/// Every program this module can start is one of three fixed names; the
/// request type has no string in it.
#[test]
fn the_launcher_allowlist_is_exactly_the_three_known_editors() {
    let programs: Vec<&str> = EDITORS.iter().map(|s| s.program).collect();
    assert_eq!(programs, ["code", "cursor", "zed"]);
    for spec in EDITORS {
        assert!(Path::new(spec.macos_bundle_cli).is_absolute());
        assert!(spec.macos_bundle_cli.ends_with(spec.program) || spec.program == "zed");
    }
    // The wire form is the lowercase name; anything else does not parse.
    for ok in ["auto", "code", "cursor", "zed"] {
        serde_json::from_str::<EditorChoice>(&format!("\"{ok}\"")).expect(ok);
    }
    for bad in ["vim", "/bin/sh", "Code", "code --wait", ""] {
        assert!(
            serde_json::from_str::<EditorChoice>(&format!("{bad:?}")).is_err(),
            "{bad:?} must not parse as an editor"
        );
    }
}

struct Recorded {
    calls: RefCell<Vec<(String, Vec<String>, PathBuf)>>,
}

fn run_open(
    file: &Path,
    root: &Path,
    line: Option<u32>,
    choice: EditorChoice,
    exit_ok: bool,
    stderr: &str,
) -> (Result<(), CommandError>, Recorded) {
    let recorded = Recorded {
        calls: RefCell::new(Vec::new()),
    };
    let result = open_in_editor(
        root,
        file,
        line,
        Some(4),
        choice,
        &EditorEnv {
            use_bundles: false,
            bundle_exists: &|_| false,
            on_path: &|p| Some(format!("/opt/bin/{p}")),
            run: &|program, args, cwd| {
                recorded.calls.borrow_mut().push((
                    program.to_string(),
                    args.to_vec(),
                    cwd.to_path_buf(),
                ));
                Ok((exit_ok, stderr.to_string()))
            },
        },
    );
    (result, recorded)
}

#[test]
fn the_launcher_is_run_once_with_a_vector_of_arguments_in_the_repository() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("a b.rs");
    std::fs::write(&file, "x").expect("file");
    let (result, recorded) = run_open(&file, dir.path(), Some(12), EditorChoice::Cursor, true, "");
    result.expect("opened");
    let calls = recorded.calls.borrow();
    assert_eq!(calls.len(), 1);
    let (program, args, cwd) = &calls[0];
    assert_eq!(program, "/opt/bin/cursor");
    assert_eq!(
        args,
        &["-g".to_string(), format!("{}:12:4", file.display())]
    );
    assert_eq!(cwd, dir.path());
}

#[test]
fn a_launcher_that_exits_with_an_error_is_reported_with_its_own_words() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "x").expect("file");
    let (result, _) = run_open(
        &file,
        dir.path(),
        None,
        EditorChoice::Code,
        false,
        "\n  Error: No Cursor IDE installation found.\nsecond line\n",
    );
    let message = result.expect_err("failed").message().to_string();
    assert!(
        message.contains("Visual Studio Code could not open the file"),
        "{message}"
    );
    assert!(
        message.contains("No Cursor IDE installation found"),
        "{message}"
    );
    assert!(!message.contains("second line"), "{message}");

    let (silent, _) = run_open(&file, dir.path(), None, EditorChoice::Code, false, "");
    assert!(silent.expect_err("failed").message().contains("no message"));
}

/// A directory (or a file that vanished between the click and here) has no
/// line to go to; the launcher must not run for it.
#[test]
fn a_path_that_is_not_a_file_never_reaches_the_launcher() {
    let dir = tempfile::tempdir().expect("dir");
    let (result, recorded) = run_open(
        dir.path(),
        dir.path(),
        Some(1),
        EditorChoice::Auto,
        true,
        "",
    );
    assert!(result.is_err());
    assert!(recorded.calls.borrow().is_empty());
    let (gone, recorded) = run_open(
        &dir.path().join("gone.rs"),
        dir.path(),
        None,
        EditorChoice::Auto,
        true,
        "",
    );
    assert!(gone.is_err());
    assert!(recorded.calls.borrow().is_empty());
}

#[test]
fn when_no_editor_is_installed_nothing_runs_and_the_error_is_the_typed_one() {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "x").expect("file");
    let ran = RefCell::new(false);
    let err = open_in_editor(
        dir.path(),
        &file,
        Some(1),
        None,
        EditorChoice::Auto,
        &EditorEnv {
            use_bundles: true,
            bundle_exists: &|_| false,
            on_path: &nothing_on_path,
            run: &|_, _, _| {
                *ran.borrow_mut() = true;
                Ok((true, String::new()))
            },
        },
    )
    .expect_err("no editor");
    assert!(!*ran.borrow());
    assert_eq!(err.origin(), Some(IpcErrorKind::EditorNotFound));
}
