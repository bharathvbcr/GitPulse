//! The Codex transcript source, end to end: synthetic rollouts (the structure
//! of what Codex CLI 0.149.1 writes, with invented content) through the whole
//! pass into a ledger. Nothing here reads a real `~/.codex` or `~/.claude`.

use super::*;

const CLAUDE_ENV: &str = "GITPULSE_TRANSCRIPT_ROOT";
const CODEX_ENV: &str = "GITPULSE_CODEX_TRANSCRIPT_ROOT";

/// Runs `f` with the two transcript roots as given: `None` leaves the variable
/// unset, so the root falls to the (test-moved) home directory.
fn with_roots<T>(
    claude: Option<&std::path::Path>,
    codex: Option<&std::path::Path>,
    f: impl FnOnce() -> T,
) -> T {
    let serial = crate::test_support::env::env_serial();
    let mut env = crate::test_support::env::bind_env(&serial);
    env = match claude {
        Some(path) => env.set(CLAUDE_ENV, path),
        None => env.remove(CLAUDE_ENV),
    };
    env = match codex {
        Some(path) => env.set(CODEX_ENV, path),
        None => env.remove(CODEX_ENV),
    };
    let _env = env;
    f()
}

fn stamp(second: u32) -> String {
    format!("2026-09-01T12:00:{second:02}.000Z")
}

fn envelope(second: u32, kind: &str, payload: serde_json::Value) -> String {
    serde_json::json!({
        "timestamp": stamp(second),
        "ordinal": second,
        "type": kind,
        "payload": payload,
    })
    .to_string()
}

fn session_meta(thread: &str, cwd: &str) -> String {
    envelope(
        0,
        "session_meta",
        serde_json::json!({
            "id": thread,
            "timestamp": stamp(0),
            "cwd": cwd,
            "originator": "codex_sdk_ts",
            "cli_version": "0.149.1",
            "history_mode": "paginated",
        }),
    )
}

/// A `custom_tool_call` named `exec`, whose input is the given script.
fn exec(second: u32, script: &str) -> String {
    envelope(
        second,
        "response_item",
        serde_json::json!({
            "type": "custom_tool_call",
            "id": format!("ctc_{second}"),
            "status": "completed",
            "call_id": format!("call_{second}"),
            "name": "exec",
            "input": script,
        }),
    )
}

fn run(second: u32, cmd: &str, workdir: &str) -> String {
    exec(
        second,
        &format!(
            "const r = await tools.exec_command({{cmd:{}, workdir:{}, yield_time_ms:1000}});",
            serde_json::to_string(cmd).unwrap(),
            serde_json::to_string(workdir).unwrap()
        ),
    )
}

fn patch(second: u32, header: &str) -> String {
    exec(
        second,
        &format!(
            "const patch = `*** Begin Patch\n{header}\n+x\n*** End Patch`;\nprint(await tools.apply_patch(patch));"
        ),
    )
}

fn inert(second: u32) -> Vec<String> {
    vec![
        envelope(
            second,
            "event_msg",
            serde_json::json!({ "type": "task_started" }),
        ),
        envelope(
            second,
            "response_item",
            serde_json::json!({ "type": "reasoning", "id": "rs_1" }),
        ),
        envelope(
            second,
            "response_item",
            serde_json::json!({ "type": "message", "role": "assistant", "content": [] }),
        ),
        envelope(
            second,
            "response_item",
            serde_json::json!({ "type": "custom_tool_call_output", "call_id": "c", "output": [] }),
        ),
        envelope(
            second,
            "turn_context",
            serde_json::json!({ "turn_id": "t" }),
        ),
        envelope(second, "world_state", serde_json::json!({})),
    ]
}

fn rollout_path(root: &std::path::Path, thread: &str) -> std::path::PathBuf {
    root.join("2026/09/01")
        .join(format!("rollout-2026-09-01T12-00-00-{thread}.jsonl"))
}

fn write_lines(path: &std::path::Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn append_text(path: &std::path::Path, text: &str) {
    let mut file = std::fs::File::options().append(true).open(path).unwrap();
    std::io::Write::write_all(&mut file, text.as_bytes()).unwrap();
}

fn rows(ledger_repo: &str) -> Vec<ledger::LedgerEvent> {
    ledger::tail(ledger_repo, 0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| transcript::ACTIONS.contains(&e.action.as_str()))
        .collect()
}

#[test]
fn a_codex_rollout_yields_session_rows_with_actor_codex() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let mut lines = vec![session_meta("THREAD-1", repo)];
    lines.extend(inert(1));
    lines.push(run(2, "cargo test", repo));
    lines.push(patch(3, "*** Update File: src/a.rs"));
    lines.push(patch(4, &format!("*** Add File: {repo}/src/new.rs")));
    write_lines(&rollout_path(codex.path(), "THREAD-1"), &lines);

    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        ingest_transcripts(repo)
    });

    assert_eq!(out.recorded, 3, "{out:?}");
    assert_eq!(
        (out.skipped_lines, out.walk_complete),
        (0, Some(true)),
        "{out:?}"
    );
    assert_eq!(out.error, "", "{out:?}");
    let rows = rows(repo);
    let mut seen: Vec<(String, String)> = rows
        .iter()
        .map(|e| (e.action.clone(), e.object.clone().unwrap_or_default()))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("session.command".to_string(), "cargo test".to_string()),
            ("session.edit".to_string(), format!("{repo}/src/a.rs")),
            ("session.edit".to_string(), format!("{repo}/src/new.rs")),
        ]
    );
    for row in &rows {
        assert_eq!(row.actor_id.as_deref(), Some("codex"), "{row:?}");
        assert_eq!(row.actor_kind, "agent");
        assert_eq!(row.session_id.as_deref(), Some("THREAD-1"));
        assert!(row.verdict_json.is_none(), "no gate saw this: {row:?}");
        let detail: serde_json::Value =
            serde_json::from_str(row.detail_json.as_deref().unwrap()).unwrap();
        assert_eq!(detail["source"], "transcript");
        assert_eq!(detail["transcript_version"], "0.149.1");
    }
}

#[test]
fn unknown_and_malformed_codex_lines_are_counted_and_never_panic() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let valid = run(9, "ls", repo);
    let truncated = valid[..valid.len() - 5].to_string();
    let lines = vec![
        session_meta("THREAD-2", repo),
        "this is not json".to_string(),
        truncated,
        r#"{"foo":1}"#.to_string(),
        r#"{"timestamp":"2026-09-01T12:00:01.000Z","type":"surprise","payload":{}}"#.to_string(),
        // The shape an older Codex wrote: no envelope.
        r#"{"type":"message","role":"user","content":[]}"#.to_string(),
        // A shell call in a shape this reader does not know.
        envelope(
            2,
            "response_item",
            serde_json::json!({ "type": "function_call", "name": "shell_command",
                                "arguments": "{\"command\":\"ls\"}", "call_id": "c" }),
        ),
        envelope(
            3,
            "response_item",
            serde_json::json!({ "type": "local_shell_call",
                                "action": { "type": "other", "command": ["ls"] } }),
        ),
        // A command that is not a literal.
        exec(4, "const c = 'ls'; await tools.exec_command({cmd: c});"),
        // A call to a tool that changes nothing is not a gap.
        envelope(
            5,
            "response_item",
            serde_json::json!({ "type": "function_call", "name": "wait", "arguments": "{}" }),
        ),
        valid,
    ];
    write_lines(&rollout_path(codex.path(), "THREAD-2"), &lines);

    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        ingest_transcripts(repo)
    });

    assert_eq!(out.recorded, 1, "the one readable command: {out:?}");
    // Not JSON, truncated, no envelope, an unknown kind, the older envelope-less
    // shape, a shell call and a local shell call in unknown shapes, and a
    // command that is not a literal.
    assert_eq!(out.skipped_lines, 8, "{out:?}");
    assert_eq!(rows(repo).len(), 1);
}

#[test]
fn a_call_before_the_session_meta_is_counted_not_placed() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(codex.path(), "THREAD-3"),
        &[run(1, "ls", repo), session_meta("THREAD-3", repo)],
    );

    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        ingest_transcripts(repo)
    });

    assert_eq!((out.recorded, out.skipped_lines), (0, 1), "{out:?}");
}

#[test]
fn a_missing_codex_override_is_reported_not_read_as_no_sessions() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let missing = scratch.path().join("no-such-dir");

    let out = with_roots(Some(claude.path()), Some(&missing), || {
        ingest_transcripts(repo)
    });

    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains("Codex"), "{out:?}");
    assert!(out.error.contains("no-such-dir"), "{out:?}");

    // The same when a Claude corpus is there to read: the Codex gap is still
    // said, and does not cost the Claude rows.
    let file = scratch.path().join("a-file");
    std::fs::write(&file, "x").unwrap();
    std::fs::create_dir_all(claude.path().join("slug")).unwrap();
    std::fs::write(
        claude.path().join("slug/S1.jsonl"),
        serde_json::json!({
            "type": "assistant", "sessionId": "S1", "timestamp": stamp(1), "cwd": repo,
            "message": { "content": [
                { "type": "tool_use", "name": "Edit", "input": { "file_path": format!("{repo}/a.rs") } }
            ]}
        })
        .to_string()
            + "\n",
    )
    .unwrap();
    let out = with_roots(Some(claude.path()), Some(&file), || {
        ingest_transcripts(repo)
    });
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains("Codex"), "{out:?}");
}

#[cfg(unix)]
#[test]
fn an_unreadable_codex_root_is_an_incomplete_walk_with_a_reason() {
    use std::os::unix::fs::PermissionsExt;
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let shut = codex.path().join("2026");
    std::fs::create_dir_all(&shut).unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&shut).is_ok();

    let out = with_roots(Some(claude.path()), Some(codex.path()), || {
        ingest_transcripts(repo)
    });

    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        return; // running as a user the mode does not bind
    }
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains("Codex"), "{out:?}");
}

#[test]
fn a_codex_directory_that_was_never_there_is_not_an_error() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let claude = tempfile::tempdir().unwrap();

    // `None` leaves the Codex root at its default under the test's own HOME.
    let out = with_roots(Some(claude.path()), None, || ingest_transcripts(repo));

    assert_eq!(out.error, "", "{out:?}");
    assert_eq!(out.walk_complete, Some(true), "{out:?}");
}

/// Two worktrees as the family tests have them: plain directories, the ledger
/// under the first.
struct Pair {
    _root: tempfile::TempDir,
    base: String,
    wt: [String; 2],
}

impl Pair {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let wt = ["w1", "w2"].map(|name| {
            let path = base.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path.to_string_lossy().into_owned()
        });
        Self {
            base: base.to_string_lossy().into_owned(),
            _root: root,
            wt,
        }
    }

    fn pass(&self) -> CatchUp {
        pass::ingest_family_until(&self.wt[0], &self.wt[0], &[self.wt[1].clone()], &mut |_| {
            false
        })
    }
}

fn claude_edit(cwd: &str, session: &str, second: u32, file: &str) -> String {
    serde_json::json!({
        "type": "assistant", "sessionId": session, "timestamp": stamp(second),
        "version": "2.1.241", "cwd": cwd,
        "message": { "content": [
            { "type": "tool_use", "name": "Edit", "input": { "file_path": file } }
        ]}
    })
    .to_string()
}

#[test]
fn both_sources_in_one_pass_are_attributed_to_their_own_worktrees() {
    let pair = Pair::new();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let [w1, w2] = &pair.wt;
    write_lines(
        &claude.path().join("slug/C1.jsonl"),
        &[
            claude_edit(w1, "C1", 1, &format!("{w1}/claude1.rs")),
            claude_edit(w2, "C1", 2, &format!("{w2}/claude2.rs")),
        ],
    );
    write_lines(
        &rollout_path(codex.path(), "X1"),
        &[
            session_meta("X1", w2),
            run(3, "cargo build", w2),
            patch(4, &format!("*** Update File: {w1}/codex1.rs")),
            // Outside every worktree of the family: nobody's.
            run(5, "ls", "/elsewhere"),
        ],
    );

    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());

    assert_eq!(out.recorded, 4, "{out:?}");
    assert_eq!(out.transcripts, 2, "{out:?}");
    assert_eq!(out.transcripts_listed, 2, "{out:?}");
    assert_eq!(out.walk_complete, Some(true), "{out:?}");
    let mut seen: Vec<(String, String, String)> = rows(&pair.wt[0])
        .into_iter()
        .map(|e| {
            let owner = e.worktree_path.unwrap_or_else(|| "<anchor>".to_string());
            (
                owner.replace(&pair.base, "$ROOT"),
                e.actor_id.unwrap_or_default(),
                e.object.unwrap_or_default().replace(&pair.base, "$ROOT"),
            )
        })
        .collect();
    seen.sort();
    let want = |owner: &str, actor: &str, object: &str| {
        (owner.to_string(), actor.to_string(), object.to_string())
    };
    assert_eq!(
        seen,
        vec![
            want("$ROOT/w2", "claude-code", "$ROOT/w2/claude2.rs"),
            want("$ROOT/w2", "codex", "cargo build"),
            // The ledger's own worktree is the one whose rows name no other.
            want("<anchor>", "claude-code", "$ROOT/w1/claude1.rs"),
            want("<anchor>", "codex", "$ROOT/w1/codex1.rs"),
        ]
    );
}

#[test]
fn a_second_pass_adds_no_codex_rows_and_an_appended_call_is_read_in_context() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let path = rollout_path(codex.path(), "THREAD-4");
    write_lines(
        &path,
        &[session_meta("THREAD-4", repo), run(1, "first", repo)],
    );

    let (first, second, third) = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        let first = ingest_transcripts(repo);
        let second = ingest_transcripts(repo);
        // The cursor now stands past `session_meta`: the new call must still
        // be placed in its thread and directory.
        append_text(&path, &(run(2, "second", repo) + "\n"));
        (first, second, ingest_transcripts(repo))
    });

    assert_eq!((first.recorded, second.recorded, third.recorded), (1, 0, 1));
    assert_eq!(
        second.transcripts, 0,
        "an unchanged rollout is not reopened"
    );
    let rows = rows(repo);
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row.session_id.as_deref(), Some("THREAD-4"));
        assert_eq!(row.actor_id.as_deref(), Some("codex"));
    }
}

/// A legacy `session.edit` row, written before the first pass: it freezes the
/// baseline floor the old watermark drew, at "now".
fn legacy_row(repo: &str) {
    ledger::append(ledger::Draft {
        repo_path: repo.to_string(),
        action: "session.edit".into(),
        object: Some("legacy".into()),
        ..Default::default()
    })
    .unwrap();
}

#[test]
fn the_legacy_baseline_floor_holds_claude_to_it_and_does_not_hold_codex() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    legacy_row(repo);
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    // Both calls are from 2026-09-01, long before the baseline.
    write_lines(
        &claude.path().join("slug/C1.jsonl"),
        &[claude_edit(repo, "C1", 1, &format!("{repo}/claude.rs"))],
    );
    write_lines(
        &rollout_path(codex.path(), "X2"),
        &[session_meta("X2", repo), run(2, "cargo test", repo)],
    );

    let out = with_roots(Some(claude.path()), Some(codex.path()), || {
        ingest_transcripts(repo)
    });

    assert_eq!(out.recorded, 1, "{out:?}");
    let objects: Vec<String> = rows(repo)
        .into_iter()
        .filter_map(|e| e.object)
        .filter(|o| o != "legacy")
        .collect();
    assert_eq!(
        objects,
        vec!["cargo test".to_string()],
        "the Claude call was the old watermark's; the Codex one never was"
    );
}

#[test]
fn caps_apply_per_source_so_a_full_claude_root_does_not_starve_codex() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    for i in 0..(MAX_WALK_FILES + 3) {
        let dir = claude.path().join(format!("p{}", i % 50));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{i}.jsonl")), "").unwrap();
    }
    write_lines(
        &rollout_path(codex.path(), "X3"),
        &[session_meta("X3", repo), run(1, "cargo test", repo)],
    );

    let out = with_roots(Some(claude.path()), Some(codex.path()), || {
        ingest_transcripts(repo)
    });

    assert_eq!(out.unlisted_cap, 3, "{out:?}");
    assert_eq!(
        out.transcripts_listed,
        i64::try_from(MAX_WALK_FILES).unwrap() + 1,
        "{out:?}"
    );
    assert_eq!(out.recorded, 1, "the Codex rollout is read: {out:?}");
    assert!(out.truncated, "{out:?}");
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
}

#[test]
fn a_codex_cursor_is_not_forgotten_while_its_root_is_not_walked() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(codex.path(), "X4"),
        &[session_meta("X4", repo), run(1, "cargo test", repo)],
    );
    let scratch = tempfile::tempdir().unwrap();
    let missing = scratch.path().join("gone");

    let (first, between, again) = with_roots(Some(claude.path()), Some(codex.path()), || {
        let first = ingest_transcripts(repo);
        // The root goes away for a pass (an unmounted volume): the cursor is
        // not evidence that the file is gone.
        let between = with_roots(Some(claude.path()), Some(&missing), || {
            ingest_transcripts(repo)
        });
        (first, between, ingest_transcripts(repo))
    });

    assert_eq!(first.recorded, 1, "{first:?}");
    assert_eq!(between.recorded, 0, "{between:?}");
    assert_eq!(again.recorded, 0, "the rollout was replayed: {again:?}");
    assert_eq!(rows(repo).len(), 1);
}

fn call(second: u32, payload: serde_json::Value) -> String {
    envelope(second, "response_item", payload)
}

/// The shapes the ordinary Codex CLI writes (Codex's protocol types), as
/// synthetic fixtures: the arguments are a JSON *string*, an argv is an array,
/// and a freeform patch is the raw text.
#[test]
fn the_ordinary_codex_tool_call_shapes_are_read() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let args = |v: serde_json::Value| v.to_string();
    let lines = vec![
        session_meta("THREAD-5", repo),
        call(
            1,
            serde_json::json!({ "type": "function_call", "name": "exec_command", "call_id": "a",
                "arguments": args(serde_json::json!({ "cmd": "cargo build", "workdir": repo,
                                                      "yield_time_ms": 1000 })) }),
        ),
        call(
            2,
            serde_json::json!({ "type": "function_call", "name": "shell", "call_id": "b",
                "arguments": args(serde_json::json!({ "command": ["bash", "-lc", "make all"],
                                                      "workdir": "sub" })) }),
        ),
        call(
            3,
            serde_json::json!({ "type": "local_shell_call", "call_id": "c", "status": "completed",
                "action": { "type": "exec", "command": ["git", "status"],
                            "working_directory": repo } }),
        ),
        call(
            4,
            serde_json::json!({ "type": "custom_tool_call", "name": "apply_patch", "call_id": "d",
                "input": format!("*** Begin Patch\n*** Update File: src/p.rs\n@@\n-a\n+b\n*** Add File: {repo}/n.rs\n+x\n*** End Patch") }),
        ),
        // Tools that change nothing, and a function call's output.
        call(
            5,
            serde_json::json!({ "type": "function_call", "name": "wait", "arguments": "{}", "call_id": "e" }),
        ),
        call(
            6,
            serde_json::json!({ "type": "function_call_output", "call_id": "a", "output": "ok" }),
        ),
    ];
    write_lines(&rollout_path(codex.path(), "THREAD-5"), &lines);

    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        ingest_transcripts(repo)
    });

    assert_eq!((out.recorded, out.skipped_lines), (5, 0), "{out:?}");
    let mut seen: Vec<(String, String)> = rows(repo)
        .into_iter()
        .map(|e| (e.action, e.object.unwrap_or_default()))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("session.command".to_string(), "cargo build".to_string()),
            ("session.command".to_string(), "git status".to_string()),
            ("session.command".to_string(), "make all".to_string()),
            ("session.edit".to_string(), format!("{repo}/n.rs")),
            ("session.edit".to_string(), format!("{repo}/src/p.rs")),
        ]
    );
}

#[test]
fn a_relative_workdir_attributes_to_the_turns_directory() {
    let pair = Pair::new();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let [w1, w2] = &pair.wt;
    write_lines(
        &rollout_path(codex.path(), "X5"),
        &[
            session_meta("X5", &pair.base),
            call(
                1,
                serde_json::json!({ "type": "function_call", "name": "exec_command", "call_id": "a",
                    "arguments": serde_json::json!({ "cmd": "ls", "workdir": "w2" }).to_string() }),
            ),
            call(
                2,
                serde_json::json!({ "type": "function_call", "name": "exec_command", "call_id": "b",
                    "arguments": serde_json::json!({ "cmd": "pwd", "workdir": w1 }).to_string() }),
            ),
        ],
    );

    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        pair.pass()
    });

    assert_eq!(out.recorded, 2, "{out:?}");
    let owners: Vec<(String, String)> = {
        let mut v: Vec<_> = rows(w1)
            .into_iter()
            .map(|e| (e.object.unwrap(), e.worktree_path.unwrap_or_default()))
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        owners,
        vec![
            ("ls".to_string(), w2.clone()),
            ("pwd".to_string(), String::new())
        ]
    );
}

#[test]
fn a_function_call_whose_arguments_do_not_parse_is_counted() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir.path().to_str().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let bad = |name: &str, arguments: &str| {
        call(
            1,
            serde_json::json!({ "type": "function_call", "name": name, "arguments": arguments,
                                "call_id": "x" }),
        )
    };
    write_lines(
        &rollout_path(codex.path(), "THREAD-6"),
        &[
            session_meta("THREAD-6", repo),
            bad("exec_command", "not json"),
            bad("exec_command", "{}"),
            bad("shell", r#"{"command":"a string, not an argv"}"#),
            bad("shell", r#"{"command":[]}"#),
        ],
    );

    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        ingest_transcripts(repo)
    });

    assert_eq!((out.recorded, out.skipped_lines), (0, 4), "{out:?}");
}

/// Runs `f` with the Codex variables as given and the Claude root pointed away.
fn codex_root_with(
    codex_home: Option<&std::path::Path>,
    over: Option<&std::path::Path>,
) -> std::path::PathBuf {
    let serial = crate::test_support::env::env_serial();
    let mut env = crate::test_support::env::bind_env(&serial);
    env = match codex_home {
        Some(path) => env.set("CODEX_HOME", path),
        None => env.remove("CODEX_HOME"),
    };
    env = match over {
        Some(path) => env.set(CODEX_ENV, path),
        None => env.remove(CODEX_ENV),
    };
    let _env = env;
    source::sources()
        .iter()
        .find(|s| s.actor_id() == "codex")
        .and_then(|s| s.root())
        .unwrap()
        .path
}

#[test]
fn the_codex_root_is_the_override_then_codex_home_then_the_home_directory() {
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    assert_eq!(
        codex_root_with(Some(home.path()), Some(other.path())),
        other.path(),
        "the override wins"
    );
    assert_eq!(
        codex_root_with(Some(home.path()), None),
        home.path().join("sessions"),
        "CODEX_HOME names Codex's home, whose sessions are below it"
    );
    // Neither: under the process's own (hermetic) HOME, never the real one.
    let default = codex_root_with(None, None);
    assert!(default.ends_with(".codex/sessions"), "{default:?}");
    assert!(
        default.starts_with(crate::test_support::env::hermetic::root()),
        "{default:?} is outside the hermetic root"
    );
}

mod adversarial;
