//! The command gate judges a *string*, the executor runs an *argv*.
//!
//! `render_command` is the seam between them. If its quoting is imperfect, the
//! policy sees a different command from the one that runs — either refusing
//! something harmless, or judging a paraphrase of something dangerous. That is
//! the one property worth testing here, and it is testable against the real
//! thing: render the argv, hand the string to a shell, and compare what the
//! shell parsed back against what went in.

use gitpulse_lib::harness::render_command;
use std::process::Command;

/// Parse a rendered command line with a real shell and return its argv.
fn shell_parse(rendered: &str) -> Vec<String> {
    // NUL-separated, not newline-separated: an argument may legitimately
    // contain a newline, and splitting output on newlines cannot tell that
    // from two arguments. Using '\0' the separator cannot occur in the data.
    let script = format!("printf '%s\\0' {rendered}");
    let out = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("bash must be on PATH");
    assert!(
        out.status.success(),
        "shell rejected {rendered:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let mut args: Vec<String> = text.split('\0').map(str::to_string).collect();
    // printf writes a trailing separator after the final argument.
    args.pop();
    args
}

fn assert_round_trips(argv: &[&str]) {
    let rendered = render_command(argv);
    let parsed = shell_parse(&rendered);
    let expected: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        parsed, expected,
        "render_command({argv:?}) produced {rendered:?}, which a shell parses as {parsed:?}"
    );
}

#[test]
fn ordinary_git_commands_round_trip() {
    assert_round_trips(&["git", "status", "--short"]);
    assert_round_trips(&["git", "push", "--force", "origin", "main"]);
    assert_round_trips(&["git", "add", "--", ":(literal)src/main.rs"]);
}

#[test]
fn arguments_containing_shell_metacharacters_round_trip() {
    // The case the gate exists for: an argument that would be read as extra
    // commands if it were not quoted. If rendering lost the quoting, the gate
    // would judge "git commit -m x" plus a separate "rm -rf /".
    assert_round_trips(&["git", "commit", "-m", "x; rm -rf /"]);
    assert_round_trips(&["git", "commit", "-m", "a && b"]);
    assert_round_trips(&["git", "commit", "-m", "a | b"]);
    assert_round_trips(&["git", "commit", "-m", "$(whoami)"]);
    assert_round_trips(&["git", "commit", "-m", "`whoami`"]);
    assert_round_trips(&["git", "commit", "-m", "${HOME}"]);
    assert_round_trips(&["git", "commit", "-m", "a > out.txt"]);
    assert_round_trips(&["git", "commit", "-m", "a\\b"]);
}

#[test]
fn arguments_containing_quotes_round_trip() {
    // Single quotes are the hard case: the quoting style is single-quoting.
    assert_round_trips(&["git", "commit", "-m", "it's fine"]);
    assert_round_trips(&["git", "commit", "-m", "'"]);
    assert_round_trips(&["git", "commit", "-m", "''"]);
    assert_round_trips(&["git", "commit", "-m", "a'b'c"]);
    assert_round_trips(&["git", "commit", "-m", "\"double\""]);
    assert_round_trips(&["git", "commit", "-m", "mixed '\" quotes"]);
}

#[test]
fn whitespace_and_empty_arguments_round_trip() {
    // An empty argument must survive as an argument rather than disappearing,
    // or the gate sees a command with fewer words than the one that runs.
    assert_round_trips(&["git", "commit", "-m", ""]);
    assert_round_trips(&["git", "commit", "-m", " "]);
    assert_round_trips(&["git", "commit", "-m", "two  spaces"]);
    assert_round_trips(&["git", "commit", "-m", "tab\there"]);
    assert_round_trips(&["git", "commit", "-m", "new\nline"]);
}

#[test]
fn non_ascii_arguments_round_trip() {
    assert_round_trips(&["git", "commit", "-m", "héllo café"]);
    assert_round_trips(&["git", "commit", "-m", "日本語のメッセージ"]);
    assert_round_trips(&["git", "commit", "-m", "emoji 👩‍👩‍👧‍👦 here"]);
}

#[test]
fn a_rendered_command_never_gains_or_loses_arguments() {
    // Whatever the content, the shell must see exactly as many words as the
    // argv had — the count is what a rule like "git push --force" matches on.
    let awkward = [
        "a b", "a\tb", "a\nb", "'", "\"", "\\", "$x", "*", "?", "[a]", "~", "#c", "!h", "", " ",
    ];
    for arg in awkward {
        let argv = ["git", "commit", "-m", arg];
        let parsed = shell_parse(&render_command(&argv));
        assert_eq!(
            parsed.len(),
            argv.len(),
            "argument {arg:?} changed the word count"
        );
    }
}
