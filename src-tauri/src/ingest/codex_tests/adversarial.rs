//! Adversarial review of the Codex source (lane W3-L5). Every fixture is
//! synthetic. A test that FAILS here names a defect in the lane code; a test
//! that passes records a held invariant. Failing tests are meant to stay red.

use super::*;
use source::RecordParser;

fn parser() -> Box<dyn RecordParser> {
    let codex = source::sources()
        .into_iter()
        .find(|s| s.actor_id() == "codex")
        .unwrap();
    codex
        .open(
            std::path::Path::new("/nonexistent/rollout.jsonl"),
            0,
            &mut 0,
        )
        .unwrap()
}

fn feed(lines: &[String]) -> (Vec<transcript::ToolCall>, i64) {
    let mut p = parser();
    let (mut calls, mut skipped) = (Vec::new(), 0);
    for line in lines {
        let parsed = p.parse(line);
        calls.extend(parsed.calls);
        skipped += parsed.skipped;
    }
    (calls, skipped)
}

fn commands(calls: &[transcript::ToolCall]) -> Vec<String> {
    calls.iter().filter_map(|c| c.command.clone()).collect()
}

fn fcall(second: u32, name: &str, arguments: serde_json::Value) -> String {
    envelope(
        second,
        "response_item",
        serde_json::json!({"type":"function_call","name":name,"arguments":arguments,"call_id":"c"}),
    )
}

// ---------------------------------------------------------------- 1. parser

#[test]
fn p01_a_json_depth_bomb_is_counted_and_does_not_overflow_the_stack() {
    let depth = 300_000;
    let bomb = format!(
        r#"{{"timestamp":"t","type":"response_item","payload":{{"type":"function_call","x":{}{}}}}}"#,
        "[".repeat(depth),
        "]".repeat(depth)
    );
    let (calls, skipped) = feed(&[session_meta("T", "/r"), bomb]);
    assert!(calls.is_empty());
    assert_eq!(skipped, 1);
    // Just under serde_json's own limit: parsed, found to hold nothing.
    let nested = format!(
        r#"{{"timestamp":"t","type":"response_item","payload":{{"type":"function_call","x":{}{}}}}}"#,
        "[".repeat(100),
        "]".repeat(100)
    );
    let (calls, _) = feed(&[session_meta("T", "/r"), nested]);
    assert!(calls.is_empty());
}

#[test]
fn p02_a_hundred_megabyte_line_is_skipped_in_bounded_memory_and_the_next_line_is_read() {
    let pair = Pair::new();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let w1 = &pair.wt[0];
    let path = rollout_path(codex.path(), "BIG");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", session_meta("BIG", w1)).unwrap();
        // One envelope line holding 100 MB of text and the call needle.
        write!(
            f,
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call","name":"exec_command","arguments":""#,
            stamp(1)
        )
        .unwrap();
        let chunk = vec![b'a'; 1 << 20];
        for _ in 0..100 {
            f.write_all(&chunk).unwrap();
        }
        writeln!(f, r#""}}}}"#).unwrap();
        writeln!(f, "{}", run(2, "after-the-big-line", w1)).unwrap();
    }
    let started = std::time::Instant::now();
    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        pair.pass()
    });
    eprintln!("p02 pass over 100MB line: {:?}", started.elapsed());
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.skipped_lines, 1, "{out:?}");
}

#[test]
fn p03_invalid_utf8_and_nul_bytes_are_counted_per_line_and_never_poison_the_next() {
    let pair = Pair::new();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let w1 = &pair.wt[0];
    let path = rollout_path(codex.path(), "BAD");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for line in [session_meta("BAD", w1), run(1, "one", w1)] {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes
        .extend_from_slice(b"{\"type\":\"response_item\",\"payload\":\xff\xfe\"function_call\"}\n");
    bytes.extend_from_slice(b"{\"type\":\"response_item\",\x00\x00\"function_call\"}\n");
    bytes.extend_from_slice(run(2, "two", w1).as_bytes());
    bytes.push(b'\n');
    std::fs::write(&path, bytes).unwrap();
    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        pair.pass()
    });
    assert_eq!((out.recorded, out.skipped_lines), (2, 2), "{out:?}");
}

#[test]
fn p04_a_nul_inside_a_command_is_recorded_without_truncating_or_failing_the_ledger() {
    let pair = Pair::new();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let w1 = &pair.wt[0];
    write_lines(
        &rollout_path(codex.path(), "NUL"),
        &[session_meta("NUL", w1), run(1, "echo a\u{0}b", w1)],
    );
    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        pair.pass()
    });
    assert_eq!(out.recorded, 1, "{out:?}");
    assert_eq!(out.error, "", "{out:?}");
}

#[test]
fn p05_arguments_as_a_json_string_holding_json_is_read_and_other_encodings_are_counted() {
    let (calls, skipped) = feed(&[
        session_meta("T", "/r"),
        fcall(
            1,
            "exec_command",
            serde_json::json!(r#"{"cmd":"ls -la","workdir":"/r/sub"}"#),
        ),
    ]);
    assert_eq!(commands(&calls), vec!["ls -la"]);
    assert_eq!((skipped, calls[0].cwd.as_str()), (0, "/r/sub"));
    for (what, arguments) in [
        (
            "object instead of string",
            serde_json::json!({"cmd":"rm x"}),
        ),
        (
            "double encoded",
            serde_json::json!(serde_json::to_string(r#"{"cmd":"rm x"}"#).unwrap()),
        ),
        ("array", serde_json::json!(["rm x"])),
        (
            "cmd not a string",
            serde_json::json!(r#"{"cmd":["rm","x"]}"#),
        ),
        ("cmd null", serde_json::json!(r#"{"cmd":null}"#)),
    ] {
        let (calls, skipped) =
            feed(&[session_meta("T", "/r"), fcall(1, "exec_command", arguments)]);
        assert!(calls.is_empty(), "{what}");
        assert_eq!(skipped, 1, "{what}: a dropped call must be counted");
    }
}

#[test]
fn p06_argv_with_non_strings_is_counted_and_a_huge_argv_is_bounded_in_the_ledger() {
    for (what, command) in [
        ("number", serde_json::json!(["ls", 1])),
        ("null", serde_json::json!(["ls", null])),
        ("nested", serde_json::json!([["ls"]])),
        ("empty", serde_json::json!([])),
        ("string", serde_json::json!("ls")),
    ] {
        let args = serde_json::json!({ "command": command }).to_string();
        let (calls, skipped) = feed(&[
            session_meta("T", "/r"),
            fcall(1, "shell", serde_json::json!(args)),
        ]);
        assert!(calls.is_empty(), "{what}");
        assert_eq!(skipped, 1, "{what}");
    }
    // 400k words: one command of ~2.4 MB.
    let words: Vec<String> = (0..400_000).map(|i| format!("w{i:05}")).collect();
    let args = serde_json::json!({ "command": words }).to_string();
    let (calls, _) = feed(&[
        session_meta("T", "/r"),
        fcall(1, "shell", serde_json::json!(args)),
    ]);
    assert_eq!(calls.len(), 1);
    let pair = Pair::new();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let w1 = &pair.wt[0];
    write_lines(
        &rollout_path(codex.path(), "HUGE"),
        &[
            session_meta("HUGE", w1),
            fcall(
                1,
                "shell",
                serde_json::json!(serde_json::json!({ "command": calls[0].command.clone().map(|c| vec![c]), "workdir": w1 }).to_string()),
            ),
        ],
    );
    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        pair.pass()
    });
    assert_eq!(out.recorded, 1, "{out:?}");
    let row = &rows(w1)[0];
    let stored = row.object.as_deref().unwrap_or_default().len();
    assert!(stored < 64 * 1024, "stored object is {stored} bytes");
}

/// A code-mode `exec`, fed through the whole parser: the rows read and the
/// calls counted unread.
pub(super) fn js(script: &str) -> (Vec<transcript::ToolCall>, i64) {
    feed(&[session_meta("T", "/r"), exec(1, script)])
}

pub(super) fn assert_code_mode(script: &str, expect: &[&str], skipped: i64) {
    let (calls, counted) = js(script);
    let expect: Vec<String> = expect.iter().map(|c| c.to_string()).collect();
    assert_eq!((commands(&calls), counted), (expect, skipped), "{script:?}");
}

#[test]
fn p07_code_mode_reads_real_calls_and_never_a_decoy() {
    // Round 1 (p07a-g): comments, strings, interpolation, spellings.
    for (script, expect, skipped) in [
        (
            "// tools.exec_command({cmd:'rm -rf /'})\nawait tools.exec_command({cmd:'ls'})",
            vec!["ls"],
            0,
        ),
        (
            r#"console.log("tools.exec_command({cmd:'rm -rf /'})");"#,
            vec![],
            1,
        ),
        (
            r#"tools.exec_command({cmd:"echo 'tools.exec_command({cmd:\"y\"})'"})"#,
            vec![r#"echo 'tools.exec_command({cmd:"y"})'"#],
            0,
        ),
        // A substitution is not a literal: counted.
        (
            "const x='a'; await tools.exec_command({cmd:`echo ${x}`, workdir:'/r'})",
            vec![],
            1,
        ),
        (
            "await tools.exec_command({cmd:`line1\nline2 \\` quoted`, workdir:'/r/x'})",
            vec!["line1\nline2 ` quoted"],
            0,
        ),
        // An alias: one marker, and the alias call is not read.
        (
            "const t = tools; await t.exec_command({cmd:'rm x'})",
            vec![],
            1,
        ),
        ("await tools['exec_command']({cmd:'rm x'})", vec!["rm x"], 0),
        ("await tools.exec_command?.({cmd:'rm x'})", vec!["rm x"], 0),
        ("await tools?.exec_command({cmd:'rm x'})", vec!["rm x"], 0),
    ] {
        assert_code_mode(script, &expect, skipped);
    }
    // The unterminated call is counted and never a row.
    let (calls, skipped) = js("tools.exec_command({cmd:'abc");
    assert!(calls.is_empty() && skipped >= 1, "{calls:?} {skipped}");
}

#[test]
fn p07b_a_code_mode_workdir_is_placed_like_a_structured_one() {
    let (calls, skipped) = js("await tools.exec_command({cmd:'ls', workdir:'sub'}); await tools.exec_command({cmd:'pwd', workdir:'/abs'}); await tools.exec_command({cmd:'id'})");
    assert_eq!(skipped, 0);
    let seen: Vec<_> = calls
        .iter()
        .map(|c| {
            (
                c.cwd.as_str(),
                c.tool.as_str(),
                c.command.as_deref().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            ("/r/sub", "Bash", "ls"),
            ("/abs", "Bash", "pwd"),
            ("/r", "Bash", "id")
        ]
    );
}

#[test]
fn p09_a_hostile_script_costs_at_most_a_bounded_parse() {
    // Over the cap: one skip, not parsed at all.
    for size in [8 << 20, 1 << 20] {
        let unit = "tools.exec_command({cmd:`";
        let script = unit.repeat(size / unit.len());
        let started = std::time::Instant::now();
        assert_code_mode(&script, &[], 1);
        let took = started.elapsed();
        assert!(
            took < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(2)),
            "a {size} byte script took {took:?}"
        );
    }
    // Under it: parsed, bounded by the deadline, nothing recorded, and counted.
    let unit = "tools.exec_command({cmd:`";
    let script = unit.repeat((200 << 10) / unit.len());
    let started = std::time::Instant::now();
    let (calls, skipped) = js(&script);
    let took = started.elapsed();
    assert!(
        calls.is_empty() && skipped >= 1,
        "{} rows, {skipped} skipped",
        calls.len()
    );
    assert!(
        took < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(5)),
        "a 200 KiB script took {took:?}"
    );
}

// ------------------------------------------------------------ 2. attribution

fn owners(pair: &Pair) -> Vec<(String, String)> {
    let mut seen: Vec<_> = rows(&pair.wt[0])
        .into_iter()
        .map(|e| {
            (
                e.worktree_path
                    .unwrap_or_else(|| "<anchor>".into())
                    .replace(&pair.base, "$ROOT"),
                e.object.unwrap_or_default().replace(&pair.base, "$ROOT"),
            )
        })
        .collect();
    seen.sort();
    seen
}

fn one_pass(pair: &Pair, lines: &[String]) -> CatchUp {
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    write_lines(&rollout_path(codex.path(), "A"), lines);
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass())
}

#[test]
fn a01_a_dotdot_workdir_is_attributed_to_where_it_resolves() {
    let pair = Pair::new();
    let [w1, w2] = &pair.wt;
    let out = one_pass(
        &pair,
        &[
            session_meta("A", w1),
            run(1, "absolute-dotdot", &format!("{w1}/../w2")),
            run(2, "relative-dotdot", "../w2"),
            patch(3, "*** Update File: ../w2/f.rs"),
        ],
    );
    assert_eq!(out.recorded, 3, "{out:?}");
    let want = "$ROOT/w2".to_string();
    for (owner, object) in owners(&pair) {
        assert_eq!(
            owner, want,
            "mis-attributed to {owner}: {object} (really ran in {w2})"
        );
    }
}

#[cfg(unix)]
#[test]
fn a02_a_symlinked_workdir_is_attributed_by_path_as_documented() {
    let pair = Pair::new();
    let [w1, w2] = &pair.wt;
    std::os::unix::fs::symlink(w2, format!("{w1}/link")).unwrap();
    let out = one_pass(
        &pair,
        &[
            session_meta("A", w1),
            run(1, "via-link", &format!("{w1}/link")),
        ],
    );
    assert_eq!(out.recorded, 1, "{out:?}");
    // Documented limitation (transcript.rs `attribution_root`: no canonicalization).
    assert_eq!(
        owners(&pair),
        vec![("<anchor>".to_string(), "via-link".to_string())]
    );
}

#[test]
fn a03_a_relative_workdir_with_no_turn_directory_is_counted_not_placed() {
    let pair = Pair::new();
    let out = one_pass(&pair, &[session_meta("A", ""), run(1, "ls", "sub")]);
    assert_eq!((out.recorded, out.skipped_lines), (0, 1), "{out:?}");
}

#[test]
fn a04_a_workdir_outside_every_worktree_is_not_recorded() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let out = one_pass(
        &pair,
        &[
            session_meta("A", w1),
            run(1, "x", "/elsewhere"),
            run(2, "y", &format!("{w1}-sibling")),
        ],
    );
    assert_eq!(out.recorded, 0, "{out:?}");
}

#[test]
fn a05_a_session_in_one_worktree_that_edits_another_is_the_others_row() {
    let pair = Pair::new();
    let [w1, w2] = &pair.wt;
    one_pass(
        &pair,
        &[
            session_meta("A", w1),
            patch(1, &format!("*** Update File: {w2}/f.rs")),
            run(2, "in-w1", w1),
        ],
    );
    assert_eq!(
        owners(&pair),
        vec![
            ("$ROOT/w2".to_string(), "$ROOT/w2/f.rs".to_string()),
            ("<anchor>".to_string(), "in-w1".to_string()),
        ]
    );
}

fn turn_context(second: u32, cwd: &str) -> String {
    envelope(
        second,
        "turn_context",
        serde_json::json!({"turn_id":"t","cwd":cwd}),
    )
}

#[test]
fn a06_a_turn_context_cwd_before_a_resume_point_is_still_the_cwd_of_what_follows() {
    // Control: one pass over the whole file attributes all five calls to w2.
    // Resume: the pass stops after the turn_context, the calls arrive later.
    let calls = 5;
    let appended: Vec<String> = (0..calls)
        .map(|i| {
            fcall(
                10 + i,
                "exec_command",
                serde_json::json!(format!(r#"{{"cmd":"call{i}"}}"#)),
            )
        })
        .collect();

    let whole = Pair::new();
    let [w1, w2] = &whole.wt;
    let mut lines = vec![session_meta("A", w1), turn_context(1, w2)];
    lines.extend(appended.clone());
    one_pass(&whole, &lines);
    let control: Vec<_> = owners(&whole)
        .into_iter()
        .filter(|(o, _)| o == "$ROOT/w2")
        .collect();
    assert_eq!(
        control.len(),
        calls as usize,
        "control: {:?}",
        owners(&whole)
    );

    let resumed = Pair::new();
    let [w1, w2] = &resumed.wt;
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let path = rollout_path(codex.path(), "A");
    write_lines(&path, &[session_meta("A", w1), turn_context(1, w2)]);
    with_roots(Some(claude.path()), Some(codex.path()), || {
        resumed.pass();
        append_text(&path, &(appended.join("\n") + "\n"));
        resumed.pass();
    });
    let right: Vec<_> = owners(&resumed)
        .into_iter()
        .filter(|(o, _)| o == "$ROOT/w2")
        .collect();
    assert_eq!(
        right.len(),
        calls as usize,
        "after a resume {}/{} calls reached w2; the rest: {:?}",
        right.len(),
        calls,
        owners(&resumed)
    );
}

#[test]
fn a07_a_session_meta_outside_the_family_with_a_turn_context_inside_loses_resumed_calls() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let path = rollout_path(codex.path(), "A");
    write_lines(
        &path,
        &[session_meta("A", "/elsewhere"), turn_context(1, w1)],
    );
    with_roots(Some(claude.path()), Some(codex.path()), || {
        pair.pass();
        append_text(
            &path,
            &(fcall(5, "exec_command", serde_json::json!(r#"{"cmd":"later"}"#)) + "\n"),
        );
        pair.pass();
    });
    assert_eq!(owners(&pair).len(), 1, "the call ran in {w1} and was lost");
}

#[test]
fn a08_a_session_meta_larger_than_the_head_window_loses_resumed_calls_but_counts_them() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let path = rollout_path(codex.path(), "A");
    let mut meta: serde_json::Value = serde_json::from_str(&session_meta("A", w1)).unwrap();
    meta["payload"]["base_instructions"] = serde_json::json!("x".repeat(2 << 20));
    write_lines(&path, &[meta.to_string()]);
    let mut second = CatchUp::default();
    with_roots(Some(claude.path()), Some(codex.path()), || {
        pair.pass();
        append_text(&path, &(run(5, "later", w1) + "\n"));
        second = pair.pass();
    });
    assert_eq!(
        second.recorded, 1,
        "resumed call lost, skipped={}: {second:?}",
        second.skipped_lines
    );
}

// ------------------------------------------------------------------ 3. walk

fn cursor_kept(pair: &Pair, path: &std::path::Path) -> bool {
    ledger::transcript_progress(&pair.wt[0], &pair.wt[0])
        .unwrap()
        .offsets
        .contains_key(&ledger::transcript_key(path))
}

#[cfg(unix)]
#[test]
fn w01_a_symlink_loop_in_the_codex_root_is_walked_once() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(codex.path(), "L"),
        &[session_meta("L", w1), run(1, "once", w1)],
    );
    std::os::unix::fs::symlink(codex.path(), codex.path().join("2026/loop")).unwrap();
    std::os::unix::fs::symlink("../..", codex.path().join("2026/09/up")).unwrap();
    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert_eq!((out.recorded, out.transcripts_listed), (1, 1), "{out:?}");
}

#[cfg(unix)]
#[test]
fn w02_a_symlink_out_of_the_codex_root_is_refused_and_counted() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(outside.path(), "O"),
        &[session_meta("O", w1), run(1, "stolen", w1)],
    );
    std::os::unix::fs::symlink(outside.path(), codex.path().join("dirlink")).unwrap();
    std::os::unix::fs::symlink(
        rollout_path(outside.path(), "O"),
        codex.path().join("filelink.jsonl"),
    )
    .unwrap();
    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert_eq!(
        (out.recorded, out.refused_outside_corpus),
        (0, 2),
        "{out:?}"
    );
}

#[test]
fn w03_hard_links_to_one_rollout_are_recorded_once() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let a = rollout_path(codex.path(), "H");
    write_lines(&a, &[session_meta("H", w1), run(1, "hard", w1)]);
    std::fs::hard_link(&a, a.with_file_name("rollout-copy.jsonl")).unwrap();
    with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert_eq!(rows(w1).len(), 1);
}

#[test]
fn w04_a_rollout_below_the_depth_bound_is_counted_and_the_walk_incomplete() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let deep = codex.path().join("a/b/c/d/e/f/g");
    write_lines(
        &deep.join("rollout-deep.jsonl"),
        &[session_meta("D", w1), run(1, "deep", w1)],
    );
    let out = with_roots(Some(claude.path()), Some(codex.path()), || pair.pass());
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.unlisted_depth >= 1, "{out:?}");
}

#[test]
fn w05_a_walk_over_the_file_cap_never_forgets_a_codex_cursor() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let path = rollout_path(codex.path(), "C");
    write_lines(&path, &[session_meta("C", w1), run(1, "cap", w1)]);
    with_roots(Some(claude.path()), Some(codex.path()), || {
        pair.pass();
        assert!(cursor_kept(&pair, &path));
        std::fs::remove_file(&path).unwrap();
        let pad = codex.path().join("pad");
        std::fs::create_dir_all(&pad).unwrap();
        for i in 0..(MAX_WALK_FILES + 1) {
            std::fs::write(pad.join(format!("{i}.jsonl")), "").unwrap();
        }
        let out = pair.pass();
        assert_eq!(out.walk_complete, Some(false), "{out:?}");
        assert!(cursor_kept(&pair, &path), "forgotten on an incomplete walk");
    });
}

#[cfg(unix)]
#[test]
fn w06_an_unreadable_codex_root_between_passes_forgets_nothing_with_claude_whole() {
    use std::os::unix::fs::PermissionsExt;
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let path = rollout_path(codex.path(), "U");
    write_lines(&path, &[session_meta("U", w1), run(1, "u", w1)]);
    with_roots(Some(claude.path()), Some(codex.path()), || {
        pair.pass();
        std::fs::set_permissions(codex.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(codex.path()).is_ok() {
            std::fs::set_permissions(codex.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let out = pair.pass();
        std::fs::set_permissions(codex.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(out.walk_complete, Some(false), "{out:?}");
        assert!(cursor_kept(&pair, &path));
    });
}

/// Runs `f` with CODEX_HOME as given, no Codex override, and Claude's root set.
fn with_codex_home<T>(
    claude: &std::path::Path,
    home: &std::path::Path,
    f: impl FnOnce() -> T,
) -> T {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(CLAUDE_ENV, claude)
        .remove(CODEX_ENV)
        .set("CODEX_HOME", home);
    f()
}

#[cfg(unix)]
#[test]
fn w07_a_codex_home_that_becomes_unreachable_between_passes_forgets_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("codexhome");
    let claude = tempfile::tempdir().unwrap();
    let path = rollout_path(&home.join("sessions"), "V");
    write_lines(&path, &[session_meta("V", w1), run(1, "v", w1)]);
    with_codex_home(claude.path(), &home, || {
        assert_eq!(pair.pass().recorded, 1);
        // EACCES on an ancestor: `sessions` cannot be stat'ed, which `is_dir()` reads as absent.
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o000)).unwrap();
        let out = pair.pass();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            cursor_kept(&pair, &path) && out.walk_complete != Some(true),
            "root was unreadable yet the pass was complete={:?}, cursor kept={} ({out:?})",
            out.walk_complete,
            cursor_kept(&pair, &path)
        );
    });
}

#[test]
fn w08_the_forgotten_cursor_of_w07_replays_the_rollout_into_duplicate_rows() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("codexhome");
    let moved = scratch.path().join("away");
    let claude = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(&home.join("sessions"), "V"),
        &[session_meta("V", w1), run(1, "v", w1)],
    );
    with_codex_home(claude.path(), &home, || {
        pair.pass();
        // An unmounted volume holding CODEX_HOME, then remounted.
        std::fs::rename(&home, &moved).unwrap();
        pair.pass();
        std::fs::rename(&moved, &home).unwrap();
        pair.pass();
    });
    assert_eq!(rows(w1).len(), 1, "a remount replayed the rollout");
}

#[test]
fn w09_a_codex_home_that_is_a_file_is_reported() {
    let pair = Pair::new();
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("not-a-dir");
    std::fs::write(&file, "x").unwrap();
    let claude = tempfile::tempdir().unwrap();
    let out = with_codex_home(claude.path(), &file, || pair.pass());
    assert!(
        !out.error.is_empty() || out.walk_complete != Some(true),
        "silent: {out:?}"
    );
}

// ------------------------------------------------------------------- 4. env

#[test]
fn e01_codex_home_edge_values() {
    let home = tempfile::tempdir().unwrap();
    let default = codex_root_with(None, None);
    assert_eq!(
        codex_root_with(Some(std::path::Path::new("")), None),
        default,
        "empty is unset"
    );
    assert_eq!(
        codex_root_with(Some(home.path()), Some(std::path::Path::new(""))),
        home.path().join("sessions"),
        "an empty override is unset"
    );
    let literal = codex_root_with(Some(std::path::Path::new("~/x")), None);
    assert_eq!(
        literal,
        std::path::Path::new("~/x/sessions"),
        "no tilde expansion, as Codex"
    );
}

/// A relative `CODEX_HOME` or override would be read against whatever directory
/// the app is in: refused, with the reason, and the walk incomplete.
#[test]
fn e02_a_relative_codex_home_is_refused_not_resolved_against_the_process_directory() {
    let pair = Pair::new();
    let claude = tempfile::tempdir().unwrap();
    let out = with_codex_home(claude.path(), std::path::Path::new("rel"), || pair.pass());
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains("not an absolute path"), "{out:?}");

    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(CLAUDE_ENV, claude.path())
        .set(CODEX_ENV, "rel/sessions");
    let out = pair.pass();
    assert_eq!(out.walk_complete, Some(false), "{out:?}");
    assert!(out.error.contains("not an absolute path"), "{out:?}");
}

// ------------------------------------------------------------- 5/6. dup, mix

#[test]
fn d01_a_cursor_reset_does_not_duplicate_rows_for_either_source() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let cpath = rollout_path(codex.path(), "R");
    write_lines(&cpath, &[session_meta("R", w1), run(1, "codex-once", w1)]);
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(&lpath, &[claude_edit(w1, "C", 2, &format!("{w1}/c.rs"))]);
    with_roots(Some(claude.path()), Some(codex.path()), || {
        pair.pass();
        let keys = [
            ledger::transcript_key(&cpath),
            ledger::transcript_key(&lpath),
        ];
        ledger::forget_transcripts(w1, w1, &keys).unwrap();
        pair.pass();
    });
    let by_actor = |a: &str| {
        rows(w1)
            .iter()
            .filter(|e| e.actor_id.as_deref() == Some(a))
            .count()
    };
    assert_eq!(
        (by_actor("codex"), by_actor("claude-code")),
        (1, 1),
        "the ledger has no content dedup for transcript rows"
    );
}

fn claude_rows(w: &str) -> Vec<String> {
    let mut objects: Vec<String> = rows(w)
        .iter()
        .filter(|e| e.actor_id.as_deref() == Some("claude-code"))
        .filter_map(|e| e.object.clone())
        .collect();
    objects.sort();
    objects
}

#[test]
fn d01b_two_identical_lines_are_two_rows_and_stay_two_after_a_cursor_reset() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    let line = claude_edit(w1, "C", 2, &format!("{w1}/same.rs"));
    write_lines(&lpath, &[line.clone(), line]);
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        assert_eq!(claude_rows(w1).len(), 2, "two lines are two rows");
        ledger::forget_transcripts(w1, w1, &[ledger::transcript_key(&lpath)]).unwrap();
        pair.pass();
    });
    assert_eq!(claude_rows(w1).len(), 2, "a reset must not add a third");
}

#[test]
fn d01c_a_rewritten_transcript_records_only_the_lines_that_changed() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let lpath = claude.path().join("slug/C.jsonl");
    let edit = |second: u32, name: &str| claude_edit(w1, "C", second, &format!("{w1}/{name}"));
    write_lines(&lpath, &[edit(1, "a.rs"), edit(2, "b.rs"), edit(3, "c.rs")]);
    with_roots(Some(claude.path()), None, || {
        pair.pass();
        ledger::forget_transcripts(w1, w1, &[ledger::transcript_key(&lpath)]).unwrap();
        write_lines(&lpath, &[edit(1, "a.rs"), edit(2, "z.rs"), edit(3, "c.rs")]);
        pair.pass();
    });
    let files: Vec<String> = claude_rows(w1)
        .iter()
        .map(|o| o.rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(
        files,
        ["a.rs", "b.rs", "c.rs", "z.rs"],
        "only the changed line is new (a same-length edit leaves every later offset in place)"
    );
}

#[test]
fn d01d_two_passes_racing_after_a_cursor_reset_record_each_row_once() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let cpath = rollout_path(codex.path(), "R");
    write_lines(&cpath, &[session_meta("R", w1), run(1, "codex-once", w1)]);
    let lpath = claude.path().join("slug/C.jsonl");
    write_lines(&lpath, &[claude_edit(w1, "C", 2, &format!("{w1}/c.rs"))]);
    with_roots(Some(claude.path()), Some(codex.path()), || {
        pair.pass();
        let keys = [
            ledger::transcript_key(&cpath),
            ledger::transcript_key(&lpath),
        ];
        ledger::forget_transcripts(w1, w1, &keys).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| pair.pass());
            }
        });
    });
    assert_eq!(rows(w1).len(), 2, "{:?}", owners(&pair));
}

#[test]
fn d02_one_directory_as_both_roots_parses_each_file_only_by_its_own_source() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let both = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(both.path(), "R"),
        &[session_meta("R", w1), run(1, "codex-cmd", w1)],
    );
    write_lines(
        &both.path().join("slug/C.jsonl"),
        &[claude_edit(w1, "C", 2, &format!("{w1}/c.rs"))],
    );
    let out = with_roots(Some(both.path()), Some(both.path()), || pair.pass());
    let by_actor = |a: &str| {
        rows(w1)
            .iter()
            .filter(|e| e.actor_id.as_deref() == Some(a))
            .count()
    };
    assert_eq!(
        (by_actor("codex"), by_actor("claude-code")),
        (1, 1),
        "{out:?} rows={:?}",
        owners(&pair)
    );
    assert_eq!(out.skipped_lines, 0, "foreign lines were counted: {out:?}");
}

#[test]
fn d03_a_codex_rollout_under_the_claude_root_is_read_by_codex_when_both_roots_are_distinct_dirs_of_one_tree(
) {
    // Claude's override is the parent of Codex's: every rollout is listed by both sources.
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let parent = tempfile::tempdir().unwrap();
    let codex = parent.path().join("codex");
    write_lines(
        &rollout_path(&codex, "R"),
        &[session_meta("R", w1), run(1, "codex-cmd", w1)],
    );
    let out = with_roots(Some(parent.path()), Some(&codex), || pair.pass());
    assert_eq!(rows(w1).len(), 1, "{out:?}");
    assert_eq!(rows(w1)[0].actor_id.as_deref(), Some("codex"));
}

// -------------------------------------------------------------- 7. benchmark

/// Steady-state pass over 2000 unchanged Claude files, with `ADV_CODEX_FILES`
/// unchanged Codex rollouts beside them. Run by name from two builds, interleaved.
#[test]
#[ignore = "benchmark: run explicitly"]
fn adv_bench_unchanged_corpus_pass() {
    let codex_files: usize = std::env::var("ADV_CODEX_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let repo_dir = tempfile::tempdir().unwrap();
    let repo = repo_dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let slug = claude.path().join("slug");
    std::fs::create_dir_all(&slug).unwrap();
    for i in 0..2000 {
        std::fs::write(
            slug.join(format!("B{i}.jsonl")),
            claude_edit(&repo, &format!("B{i}"), 1, &format!("{repo}/f{i}.rs")) + "\n",
        )
        .unwrap();
    }
    for i in 0..codex_files {
        let day = codex.path().join(format!("2026/09/{:02}", 1 + i % 28));
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join(format!("rollout-{i}.jsonl")),
            [session_meta(&format!("X{i}"), &repo), run(2, "ls", &repo)].join("\n") + "\n",
        )
        .unwrap();
    }
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(CLAUDE_ENV, claude.path())
        .set(CODEX_ENV, codex.path());
    let first = pass::ingest_family_until(&repo, &repo, &[], &mut |_| false);
    let mut times = Vec::new();
    for _ in 0..9 {
        let started = std::time::Instant::now();
        let out = pass::ingest_family_until(&repo, &repo, &[], &mut |_| false);
        assert_eq!(out.recorded, 0);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "ADVBENCH codex_files={codex_files} first_recorded={} min_ms={:.2} median_ms={:.2}",
        first.recorded, times[0], times[4]
    );
}
