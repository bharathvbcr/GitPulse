//! Attack the `gitpulse-hook` protocol surface at the real executable boundary.
//!
//! The unit tests in `src/hooks/mod.rs` prove the decisions are right over
//! facts someone already gathered. This file proves the *process* keeps its
//! three promises to the host no matter what is thrown at it, because those
//! promises are what make a wrong answer survivable:
//!
//! 1. **Exit 0, always.** Exit 2 blocks the tool call whatever the JSON says.
//!    A hook that fell over must never be the reason a user's edit is refused.
//! 2. **stdout is empty, or exactly one JSON document.** It is the protocol
//!    channel; a stray byte on it is read as a malformed decision.
//! 3. **A check that could not run says so.** Silence means "looked, found
//!    nothing". Anything that did not look must emit a `systemMessage` instead,
//!    or the gate has been disabled without telling anybody.
//!
//! Property 3 is the one that cannot be tested from inside the process, which
//! is why the harness failure modes below stand up deliberately broken `manvi`
//! executables rather than mocking a verdict.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HOOK: &str = env!("CARGO_BIN_EXE_gitpulse-hook");
const SUBCOMMANDS: [&str; 3] = ["collision-guard", "command-gate", "session-brief"];

/// Generous on purpose. The hook's own ceiling is 5s and the host's is 10s;
/// this is only here to turn a true hang into a failure rather than a suite
/// that never finishes, so it must not be tight enough to flake on a loaded
/// machine.
const HANG: Duration = Duration::from_secs(60);

struct Answer {
    code: Option<i32>,
    stdout: Vec<u8>,
    elapsed: Duration,
}

/// Run one hook to completion, killing it if it outlives [`HANG`].
fn ask(subcommand: &str, payload: &[u8], env: &[(&str, &Path)]) -> Answer {
    let mut command = Command::new(HOOK);
    command
        .arg(subcommand)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    let started = Instant::now();
    let mut child = command.spawn().expect("spawn gitpulse-hook");

    // Written from a thread: a payload larger than the pipe buffer would
    // deadlock a parent that writes it all before reading any output.
    let mut stdin = child.stdin.take().expect("stdin");
    let owned = payload.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&owned);
        // Dropping closes the pipe, which is the EOF the hook waits for.
    });

    let output = loop {
        match child.try_wait().expect("wait") {
            Some(_) => break child.wait_with_output().expect("collect"),
            None if started.elapsed() > HANG => {
                child.kill().expect("kill");
                let _ = child.wait();
                panic!("{subcommand} did not answer within {HANG:?}");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let _ = writer.join();
    Answer {
        code: output.status.code(),
        stdout: output.stdout,
        elapsed: started.elapsed(),
    }
}

/// Assert the two promises every answer must keep, and return the parsed JSON.
fn assert_protocol(label: &str, answer: &Answer) -> Option<serde_json::Value> {
    assert_eq!(
        answer.code,
        Some(0),
        "{label}: exited {:?}; a non-zero hook can block a user's tool call",
        answer.code
    );
    let text = String::from_utf8(answer.stdout.clone())
        .unwrap_or_else(|e| panic!("{label}: stdout is not UTF-8: {e}"));
    if text.trim().is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("{label}: stdout is not one JSON document ({e}): {text:?}"));
    assert!(
        value.is_object(),
        "{label}: stdout is JSON but not an object: {text:?}"
    );
    Some(value)
}

/// Payloads a host should never send and a hostile one might.
fn hostile_corpus(repo: &str) -> Vec<(&'static str, Vec<u8>)> {
    let cwd = repo.as_bytes();
    let mut cases: Vec<(&'static str, Vec<u8>)> = vec![
        ("empty", b"".to_vec()),
        ("not-json", b"hello world".to_vec()),
        ("json-array", b"[1,2,3]".to_vec()),
        ("json-string", br#""just a string""#.to_vec()),
        ("json-null", b"null".to_vec()),
        ("json-number", b"123".to_vec()),
        ("bare-object", b"{}".to_vec()),
        (
            "nul-escape",
            b"{\"cwd\":\"/tmp/\0evil\",\"tool_input\":{\"command\":\"ls\"}}".to_vec(),
        ),
        (
            "wrong-types",
            br#"{"cwd":123,"tool_name":[],"tool_input":"a string","session_id":null}"#.to_vec(),
        ),
        (
            "tool_input-is-array",
            br#"{"cwd":"/a","tool_input":[{"command":"ls"}]}"#.to_vec(),
        ),
        (
            "duplicate-keys",
            br#"{"cwd":"/a","cwd":"/b","tool_input":{"command":"ls"}}"#.to_vec(),
        ),
        ("trailing-garbage", br#"{"cwd":"/a"} trailing"#.to_vec()),
        ("bom", b"\xef\xbb\xbf{\"cwd\":\"/a\"}".to_vec()),
        ("surrounding-newlines", b"\n\n{\"cwd\":\"/a\"}\n\n".to_vec()),
        ("relative-cwd", br#"{"cwd":"rel/path"}"#.to_vec()),
        (
            "missing-cwd",
            br#"{"tool_input":{"command":"ls"}}"#.to_vec(),
        ),
        (
            "cwd-does-not-exist",
            br#"{"cwd":"/no/such/dir","tool_input":{"command":"ls"}}"#.to_vec(),
        ),
    ];

    // Structural depth: serde_json must refuse this rather than blow the stack.
    let mut deep = Vec::from(&br#"{"tool_input":"#[..]);
    deep.extend(std::iter::repeat_n(b'[', 5000));
    deep.extend(std::iter::repeat_n(b']', 5000));
    deep.push(b'}');
    cases.push(("deep-nesting", deep));

    // Size, just under the 4 MiB cap, so it is parsed rather than refused.
    let mut huge = Vec::from(&br#"{"cwd":""#[..]);
    huge.extend_from_slice(cwd);
    huge.extend_from_slice(br#"","tool_input":{"command":""#);
    huge.extend(std::iter::repeat_n(b'A', 1_000_000));
    huge.extend_from_slice(br#""}}"#);
    cases.push(("huge-command", huge));

    // Size, over the cap: refused before interpretation.
    let mut over = Vec::from(&br#"{"cwd":"/a","x":""#[..]);
    over.extend(std::iter::repeat_n(b'B', 4 * 1024 * 1024));
    over.extend_from_slice(br#""}"#);
    cases.push(("over-4mib", over));

    // A path that walks out of the worktree, and one that is simply elsewhere.
    let mut traversal = Vec::from(&br#"{"cwd":""#[..]);
    traversal.extend_from_slice(cwd);
    traversal.extend_from_slice(br#"","tool_input":{"file_path":""#);
    traversal.extend_from_slice(cwd);
    traversal.extend(std::iter::repeat_n(b'x', 0));
    traversal.extend_from_slice(br#"/../../../../etc/passwd"}}"#);
    cases.push(("path-traversal", traversal));

    // Not UTF-8 at all: refused at the boundary, never interpreted.
    let mut invalid = Vec::from(&br#"{"cwd":"/a","tool_input":{"command":""#[..]);
    invalid.extend_from_slice(&[0xff, 0xfe]);
    invalid.extend_from_slice(br#""}}"#);
    cases.push(("invalid-utf8", invalid));

    cases
}

#[test]
fn hostile_payloads_never_block_a_tool_call_and_never_corrupt_the_channel() {
    let repo = repo_root();
    let repo = repo.to_string_lossy().into_owned();
    for (name, payload) in hostile_corpus(&repo) {
        for subcommand in SUBCOMMANDS {
            let label = format!("{subcommand}/{name}");
            let answer = ask(subcommand, &payload, &[]);
            assert_protocol(&label, &answer);
        }
    }
}

#[test]
fn a_payload_that_could_not_be_read_is_never_answered_with_a_decision() {
    // Guessing at a verdict from input we could not parse would be the worst
    // of both worlds: neither a check that ran nor an admission that one did
    // not. Empty stdout leaves the host's own permission flow untouched.
    for payload in [&b""[..], &b"not json"[..], &b"[]"[..]] {
        for subcommand in SUBCOMMANDS {
            let answer = ask(subcommand, payload, &[]);
            assert_eq!(answer.code, Some(0));
            assert!(
                answer.stdout.is_empty(),
                "{subcommand} answered an unreadable payload with {:?}",
                String::from_utf8_lossy(&answer.stdout)
            );
        }
    }
}

/// A `PreToolUse` payload naming the command the gate exists to refuse.
///
/// Serialized rather than interpolated into a string literal. `repo_root()` is
/// `D:\a\GitPulse\GitPulse` on a Windows runner, and a raw backslash makes
/// `\a` an invalid JSON escape, so the document never parsed. The hook then did
/// precisely the right thing with an unreadable payload — stayed silent — and
/// the test that forbids silence failed for the exact opposite of its premise.
/// One owner, so neither caller can reintroduce the quoting bug.
fn force_push_payload() -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "cwd": repo_root().to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": "git push --force origin main"},
    })
    .to_string()
}

/// The payload is one JSON document whose `cwd` survives the trip.
///
/// Cheap, and it fails where the bug actually lived: interpolating the path
/// into a literal satisfied this on unix and produced an unparseable document
/// on Windows, where the separator is an invalid escape. Asserting the round
/// trip rather than the spelling keeps the guard platform-agnostic.
#[test]
fn the_force_push_payload_is_one_document_whose_cwd_round_trips() {
    let value: serde_json::Value =
        serde_json::from_str(&force_push_payload()).expect("payload is one JSON document");
    assert_eq!(value["cwd"], repo_root().to_string_lossy().as_ref());
}

#[test]
fn concurrent_hooks_each_answer_completely_and_none_interleave() {
    // A host fires these in parallel on parallel tool calls. Each is its own
    // process writing its own pipe, so a torn document here would mean the
    // bounded writer, not a shared buffer — which is exactly why it is worth
    // checking at the process boundary rather than reasoning about.
    let payload = force_push_payload();

    let workers: Vec<_> = (0..24)
        .map(|i| {
            let payload = payload.clone();
            std::thread::spawn(move || (i, ask("command-gate", payload.as_bytes(), &[])))
        })
        .collect();

    for worker in workers {
        let (i, answer) = worker.join().expect("hook thread");
        let label = format!("concurrent #{i}");
        let value = assert_protocol(&label, &answer);
        // Whatever the machine's harness state, one of the two must be true:
        // a verdict, or a notice saying no verdict was reached. The forbidden
        // third outcome is silence, which would read as a clean allow.
        let value =
            value.unwrap_or_else(|| panic!("{label}: a force push was answered with silence"));
        assert!(
            value.get("hookSpecificOutput").is_some() || value.get("systemMessage").is_some(),
            "{label}: answered with neither a decision nor a notice: {value}"
        );
    }
}

/// Write an executable stand-in for `manvi` into a fresh directory.
///
/// Gated with its only caller. The body already branches on `cfg!(windows)` for
/// the filename, so it was written to work on both, but the one test that
/// stands these up is `#[cfg(unix)]` — it drives the harness through `#!/bin/sh`
/// scripts and a colon-separated PATH. Ungated, this compiles on Windows with
/// nothing to call it, which `-D warnings` reports as dead code.
#[cfg(unix)]
fn broken_harness(dir: &Path, name: &str, script: &str) -> PathBuf {
    let bin = dir.join(name);
    std::fs::create_dir_all(&bin).expect("mkdir");
    let file = bin.join(if cfg!(windows) { "manvi.exe" } else { "manvi" });
    std::fs::write(&file, script).expect("write fake manvi");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    bin
}

#[cfg(unix)]
#[test]
fn a_harness_that_cannot_answer_is_never_rendered_as_an_allow() {
    // The whole point of the command gate, stated as a property: for a command
    // the gate exists to refuse, there is no environment in which this binary
    // stays silent. Silence is reserved for "checked, and permitted".
    let scratch = tempfile::tempdir().expect("tempdir");
    let payload = force_push_payload();

    // `HOME` is redirected so the real harness cannot be found through
    // `~/.local/bin`.
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).expect("mkdir home");

    // Every inherited PATH entry that does *not* hold a manvi. Keeping the
    // whole inherited PATH would leave the real harness reachable, and the
    // "missing" case below would quietly become a test of the installed
    // harness instead of a test of its absence — passing for the wrong reason
    // on exactly the machines where a developer runs this. Filtering by
    // content rather than by a hardcoded directory keeps that true wherever
    // the harness happens to be installed.
    let inherited = std::env::var("PATH").unwrap_or_default();
    let harness_free: Vec<&str> = inherited
        .split(':')
        .filter(|dir| !dir.is_empty() && !Path::new(dir).join("manvi").exists())
        .collect();
    let inherited = harness_free.join(":");

    let cases = [
        ("missing", None),
        ("hung", Some("#!/bin/sh\nexec sleep 600\n")),
        ("exits", Some("#!/bin/sh\nexit 3\n")),
        (
            "garbage",
            Some("#!/bin/sh\nwhile read -r _; do printf 'not json\\n'; done\n"),
        ),
    ];

    for (name, script) in cases {
        let path = match script {
            // "missing" gets a genuinely empty directory. Writing an empty
            // executable here instead would test a harness that starts and
            // says nothing, which is a different case and already covered by
            // "garbage".
            None => {
                let empty = scratch.path().join(name);
                std::fs::create_dir_all(&empty).expect("mkdir");
                assert!(
                    !empty.join("manvi").exists(),
                    "the missing-harness case must contain no manvi"
                );
                empty
            }
            Some(script) => broken_harness(scratch.path(), name, script),
        };
        let search = format!("{}:{inherited}", path.display());
        let answer = ask(
            "command-gate",
            payload.as_bytes(),
            &[("PATH", Path::new(&search)), ("HOME", &home)],
        );
        let value = assert_protocol(name, &answer)
            .unwrap_or_else(|| panic!("{name}: an unjudged force push was answered with silence"));

        let decided = value
            .get("hookSpecificOutput")
            .and_then(|o| o.get("permissionDecision"))
            .and_then(|d| d.as_str());
        // With no manvi anywhere on the search path there is nothing that
        // could have judged this, so the notice is the only correct answer.
        // Asserted rather than merely allowed: without it this case passes on
        // any machine where a harness is still reachable, which is the way a
        // test of an absence quietly stops testing anything.
        if name == "missing" {
            assert!(
                decided.is_none(),
                "{name}: a decision appeared with no harness installed: {value}"
            );
        }

        match decided {
            // A real verdict is fine: some of these environments still reach a
            // harness, and a refusal is the correct answer when they do.
            Some("deny") => {}
            Some(other) => panic!("{name}: a broken harness produced a {other} decision"),
            None => {
                let notice = value
                    .get("systemMessage")
                    .and_then(|m| m.as_str())
                    .unwrap_or_default();
                assert!(
                    notice.contains("UNGATED") || notice.contains("could not"),
                    "{name}: no decision and no notice naming the gap: {value}"
                );
            }
        }
    }
}

#[test]
fn the_identity_flag_cannot_be_reached_by_a_hook_payload() {
    // `--version` prints to the same stdout a decision goes to. A host only
    // ever spawns declared subcommands, so this is a check that the two can
    // never be confused from the payload side.
    let answer = ask(
        "--version",
        br#"{"cwd":"/a","tool_input":{"command":"ls"}}"#,
        &[],
    );
    assert_eq!(answer.code, Some(0));
    let text = String::from_utf8(answer.stdout).expect("utf-8");
    assert!(
        text.starts_with("gitpulse-hook "),
        "identity did not lead with the binary name: {text:?}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(text.trim()).is_err(),
        "identity output parses as hook JSON, so a host could read it as a decision"
    );
    assert!(
        answer.elapsed < Duration::from_secs(10),
        "identity waited on stdin it does not read"
    );
}

/// This repository's root, resolved from the test binary's own manifest.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}
