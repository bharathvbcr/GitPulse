//! Round-2 adversarial review of lane W3-L5 (the round-1 fixes). Synthetic
//! fixtures only. A failing test names a defect in the lane code; a passing
//! one records a held invariant. Failing tests are meant to stay red.

use super::*;
use source::RecordParser;

fn parser() -> Box<dyn RecordParser> {
    source::sources()
        .into_iter()
        .find(|s| s.actor_id() == "codex")
        .unwrap()
        .open(std::path::Path::new("/nonexistent/rollout.jsonl"), 0)
}

/// `(commands read, lines/calls counted unread)` for one `exec` script.
fn js(script: &str) -> (Vec<String>, i64) {
    let mut p = parser();
    let (mut cmds, mut skipped) = (Vec::new(), 0);
    for line in [session_meta("T", "/r"), exec(1, script)] {
        let parsed = p.parse(&line);
        cmds.extend(parsed.calls.into_iter().filter_map(|c| c.command));
        skipped += parsed.skipped;
    }
    (cmds, skipped)
}

// ------------------------------------------------------------ 1. the lexer

#[test]
fn l01a_division_then_a_real_call_is_a_call() {
    let (cmds, skipped) = js("const x = a / b / tools.exec_command({cmd:'real'});");
    assert_eq!((cmds, skipped), (vec!["real".to_string()], 0));
}

#[test]
fn l01b_a_regex_literal_holding_a_call_is_not_a_call_and_the_next_call_is_read() {
    let (cmds, skipped) =
        js("x = /tools.exec_command({cmd:'fake'})/; await tools.exec_command({cmd:'ok'})");
    assert_eq!((cmds, skipped), (vec!["ok".to_string()], 0));
}

/// `n++ / 2` is a division; the lexer takes the `/` after `+` for a regex start
/// and the regex runs to the next `/` on the line: the `//` of a trailing
/// comment. The real call between them is swallowed and counted as "seen".
#[test]
fn l01c_a_real_call_is_not_hidden_by_a_division_after_a_postfix_increment() {
    let (cmds, skipped) = js("n++ / 2; await tools.exec_command({cmd:'real'}); // done");
    assert!(
        cmds == vec!["real".to_string()] || skipped > 0,
        "REAL CALL LOST SILENTLY: cmds={cmds:?} skipped={skipped}"
    );
}

#[test]
fn l01d_a_division_after_a_closing_brace_does_not_hide_a_real_call() {
    let (cmds, skipped) = js("const h = {a:1}.a; const o = {a:4}\n/ 2; await tools.exec_command({cmd:'real'}); const r = 6 / 3;");
    assert!(
        cmds == vec!["real".to_string()] || skipped > 0,
        "REAL CALL LOST SILENTLY: cmds={cmds:?} skipped={skipped}"
    );
}

/// After `return`/`typeof`/`case` a `/` starts a regex; the lexer sees an
/// identifier and takes it for division, so the regex body is lexed as code.
#[test]
fn l01e_a_regex_after_return_holding_a_call_is_not_a_fabricated_call() {
    let (cmds, _) = js("(async()=>{ return /tools.exec_command({cmd:'fake'})/.test('x') })()");
    assert!(cmds.is_empty(), "FABRICATED: {cmds:?}");
}

#[test]
fn l01f_a_quote_inside_a_regex_after_return_does_not_swallow_the_next_real_call() {
    let (cmds, skipped) =
        js("const f = () => { return /'/.test(s) }; await tools.exec_command({cmd:'real'}); const q = 'x'");
    assert!(
        cmds == vec!["real".to_string()] || skipped > 0,
        "REAL CALL LOST SILENTLY: cmds={cmds:?} skipped={skipped}"
    );
}

#[test]
fn l02a_nested_templates_keep_depth_and_the_next_call_is_read() {
    let (cmds, skipped) = js(
        "const s = `a ${ `inner ${ 1+1 } x` } b ${ {k:'}'}.k }`; await tools.exec_command({cmd:'after'})",
    );
    assert_eq!((cmds, skipped), (vec!["after".to_string()], 0));
}

#[test]
fn l02b_an_escaped_backtick_does_not_end_a_template() {
    let (cmds, skipped) = js(
        "const s = `a \\` tools.exec_command({cmd:'fake'}) b`; await tools.exec_command({cmd:'real'})",
    );
    assert_eq!((cmds, skipped), (vec!["real".to_string()], 0));
}

#[test]
fn l02c_a_unicode_escaped_identifier_is_a_call_or_is_counted() {
    let mut lost = Vec::new();
    for src in [
        "await tools.exec_comman\\u0064({cmd:'rm x'})",
        "await tools.\\u0065xec_command({cmd:'rm x'})",
        "await tools['exec_' + 'command']({cmd:'rm x'})",
        "await tools[`exec_command`]({cmd:'rm x'})",
    ] {
        let (cmds, skipped) = js(src);
        if cmds.is_empty() && skipped == 0 {
            lost.push(src);
        }
    }
    assert!(lost.is_empty(), "REAL CALLS LOST SILENTLY: {lost:?}");
}

#[test]
fn l02d_a_script_closing_tag_string_and_crlf_continuations_do_not_hide_a_call() {
    let (cmds, skipped) =
        js("const s = '</script>'; const t = 'a\\\r\nb'; await tools.exec_command({cmd:'real'})");
    assert_eq!((cmds, skipped), (vec!["real".to_string()], 0));
}

/// JS ends a `//` comment at `\r` (and U+2028/U+2029) as well as `\n`.
#[test]
fn l02e_a_line_comment_ends_at_a_lone_carriage_return() {
    let mut lost = Vec::new();
    for sep in ["\r", "\u{2028}", "\u{2029}"] {
        let (cmds, skipped) = js(&format!(
            "// note{sep}await tools.exec_command({{cmd:'real'}})"
        ));
        if !(cmds == vec!["real".to_string()] || skipped > 0) {
            lost.push(sep);
        }
    }
    assert!(
        lost.is_empty(),
        "REAL CALL LOST SILENTLY after a comment ended by {lost:?}"
    );
}

#[test]
fn l02f_an_unterminated_comment_at_eof_hides_only_what_it_covers() {
    let (cmds, skipped) =
        js("await tools.exec_command({cmd:'a'}); /* tools.exec_command({cmd:'b'})");
    assert_eq!((cmds, skipped), (vec!["a".to_string()], 0));
}

#[test]
fn l03_a_script_over_two_mib_is_one_counted_skip_and_exactly_two_mib_is_read() {
    let unit = "await tools.exec_command({cmd:'real'});\n";
    let over = unit.repeat((2 << 20) / unit.len() + 1);
    assert!(over.len() > 2 << 20);
    let started = std::time::Instant::now();
    let (cmds, skipped) = js(&over);
    eprintln!(
        "l03: {} real calls in the script, counted skipped={skipped}, took {:?}",
        over.matches("tools.exec_command").count(),
        started.elapsed()
    );
    assert!(cmds.is_empty());
    assert_eq!(skipped, 1);
    let mut under = unit.repeat((2 << 20) / unit.len());
    under.truncate(2 << 20);
    let (cmds, skipped) = js(&under);
    assert!(
        !cmds.is_empty() && skipped > 0,
        "a script at the bound was not scanned: {} cmds, skipped {skipped}",
        cmds.len()
    );
}

/// A deterministic fuzz over the lexer's own alphabet: never panics, and the
/// number of commands it reads never exceeds the number of occurrences of the
/// call name in the text.
#[test]
fn l04_the_lexer_never_panics_on_its_own_alphabet() {
    let atoms = [
        "tools",
        ".",
        "?.",
        "exec_command",
        "apply_patch",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "\"",
        "'",
        "`",
        "${",
        "\\",
        "/",
        "//",
        "/*",
        "*/",
        "\n",
        "\r\n",
        " ",
        "cmd:",
        "workdir:",
        "a",
        "é",
        "😀",
        "=",
        "+",
        "return ",
        "*** Update File: ",
        "x\\n",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..4000 {
        let len = (next() % 60) as usize;
        let script: String = (0..len)
            .map(|_| atoms[(next() % atoms.len() as u64) as usize])
            .collect();
        let (cmds, _) = js(&script);
        assert!(
            cmds.len() <= script.matches("exec_command").count(),
            "{script:?} -> {cmds:?}"
        );
    }
}

// --------------------------------------------------------------- 2. markers

fn cursor_count(pair: &Pair) -> usize {
    ledger::transcript_progress(&pair.wt[0], &pair.wt[0])
        .unwrap()
        .offsets
        .len()
}

#[test]
fn m01_the_marker_is_not_a_listed_transcript_and_is_one_row_per_worktree_and_source() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(codex.path(), "M1"),
        &[session_meta("M1", w1), run(1, "ls", w1)],
    );
    let (first, second) = with_roots(Some(claude.path()), Some(codex.path()), || {
        (pair.pass(), pair.pass())
    });
    assert_eq!(first.transcripts_listed, 1, "{first:?}");
    assert_eq!(second.transcripts_listed, 1, "{second:?}");
    // One rollout cursor and one marker; not one marker per pass.
    assert_eq!(cursor_count(&pair), 2);
}

/// The marker is written when an optional root is *read*, even if it holds
/// nothing: an empty `sessions` directory that is later removed stops every
/// forget, Claude's included, for as long as it stays gone.
#[test]
fn m02_a_codex_root_that_held_nothing_and_is_gone_does_not_stop_claude_cursors_being_forgotten() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let scratch = tempfile::tempdir().unwrap();
    let codex = scratch.path().join("sessions");
    std::fs::create_dir_all(&codex).unwrap();
    let claude = tempfile::tempdir().unwrap();
    let cpath = claude.path().join("slug/C.jsonl");
    write_lines(&cpath, &[claude_edit(w1, "C", 1, &format!("{w1}/c.rs"))]);
    let out = with_roots(Some(claude.path()), Some(&codex), || {
        pair.pass();
        std::fs::remove_dir(&codex).unwrap();
        std::fs::remove_file(&cpath).unwrap();
        pair.pass()
    });
    eprintln!(
        "m02: walk_complete={:?} error={:?} cursors={}",
        out.walk_complete,
        out.error,
        cursor_count(&pair)
    );
    let kept = ledger::transcript_progress(w1, w1)
        .unwrap()
        .offsets
        .contains_key(&ledger::transcript_key(&cpath));
    assert!(
        !kept,
        "a deleted Claude transcript's cursor is kept forever because an empty, removed Codex root once existed (walk_complete={:?}, error={:?})",
        out.walk_complete, out.error
    );
}

/// The same block is silent: no message says why the walk is incomplete.
#[test]
fn m03_a_lost_codex_root_is_reported_with_a_reason() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let scratch = tempfile::tempdir().unwrap();
    let codex = scratch.path().join("sessions");
    write_lines(
        &rollout_path(&codex, "M3"),
        &[session_meta("M3", w1), run(1, "ls", w1)],
    );
    let claude = tempfile::tempdir().unwrap();
    let out = with_roots(Some(claude.path()), Some(&codex), || {
        pair.pass();
        std::fs::remove_dir_all(&codex).unwrap();
        pair.pass()
    });
    assert!(
        out.walk_complete == Some(false) && !out.error.is_empty(),
        "incomplete with no stated reason: {out:?}"
    );
}

#[test]
fn m03b_a_lost_default_codex_home_is_reported_with_a_reason() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("codexhome");
    write_lines(
        &rollout_path(&home.join("sessions"), "M3B"),
        &[session_meta("M3B", w1), run(1, "ls", w1)],
    );
    let claude = tempfile::tempdir().unwrap();
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(CLAUDE_ENV, claude.path())
        .remove(CODEX_ENV)
        .set("CODEX_HOME", &home);
    pair.pass();
    std::fs::remove_dir_all(&home).unwrap();
    let out = pair.pass();
    assert!(
        out.walk_complete == Some(false) && !out.error.is_empty(),
        "incomplete with no stated reason: {out:?}"
    );
}

/// The marker is per source, not per root: pointing Codex at a different, empty
/// directory is a complete walk, forgets the first root's cursors, and returning
/// replays its rollouts as duplicate rows.
#[test]
fn m04_switching_the_codex_root_away_and_back_does_not_duplicate_rows() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(a.path(), "M4"),
        &[session_meta("M4", w1), run(1, "ls", w1)],
    );
    with_roots(Some(claude.path()), Some(a.path()), || pair.pass());
    with_roots(Some(claude.path()), Some(b.path()), || pair.pass());
    with_roots(Some(claude.path()), Some(a.path()), || pair.pass());
    assert_eq!(
        rows(w1).len(),
        1,
        "the rollout was replayed after a root switch"
    );
}

// ------------------------------------------------------------- 3. resume

/// An 8 MiB+ line that is not the `session_meta` (a big tool output) sits
/// between the meta and a resume point. The main reader skips such a line and
/// goes on; the context replay stops at it and marks everything after `lost`.
#[test]
fn r01_a_big_line_in_the_middle_does_not_make_every_later_call_unplaceable() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let path = rollout_path(codex.path(), "R1");
    let big = envelope(
        1,
        "response_item",
        serde_json::json!({"type":"custom_tool_call_output","call_id":"c","output":"x".repeat(9 << 20)}),
    );
    write_lines(&path, &[session_meta("R1", w1), big, run(2, "first", w1)]);
    let second = with_roots(Some(claude.path()), Some(codex.path()), || {
        let first = pair.pass();
        assert_eq!(first.recorded, 1, "{first:?}");
        append_text(&path, &(run(5, "later", w1) + "\n"));
        pair.pass()
    });
    assert_eq!(second.recorded, 1, "a resumed call was dropped: {second:?}");
}

/// Past 32 MiB every resumed call is unplaceable, for good: an ordinary long
/// session stops recording after its first pass over that size.
#[test]
fn r02_a_rollout_past_the_scan_cap_still_records_what_is_appended() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let path = rollout_path(codex.path(), "R2");
    let filler = envelope(
        1,
        "response_item",
        serde_json::json!({"type":"custom_tool_call_output","call_id":"c","output":"y".repeat(1 << 20)}),
    );
    let mut lines = vec![session_meta("R2", w1)];
    lines.extend(std::iter::repeat_n(filler, 34));
    write_lines(&path, &lines);
    let second = with_roots(Some(claude.path()), Some(codex.path()), || {
        pair.pass();
        append_text(&path, &(run(5, "later", w1) + "\n"));
        pair.pass()
    });
    assert_eq!(
        second.recorded, 1,
        "a 34 MiB rollout lost its appended call: {second:?}"
    );
}

/// Cost of the replay: 33 MiB of 64 KiB lines before the resume point.
/// Run by name; prints the per-file time and the extrapolation to 100 files.
#[test]
#[ignore = "benchmark: run explicitly"]
fn r03_bench_resume_replay_cost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let filler = envelope(
        1,
        "response_item",
        serde_json::json!({"type":"custom_tool_call_output","call_id":"c","output":"z".repeat(64 << 10)}),
    );
    let mut lines = vec![session_meta("R3", "/r")];
    lines.extend(std::iter::repeat_n(filler, 520));
    write_lines(&path, &lines);
    let len = std::fs::metadata(&path).unwrap().len();
    let codex = source::sources()
        .into_iter()
        .find(|s| s.actor_id() == "codex")
        .unwrap();
    let mut times = Vec::new();
    for _ in 0..7 {
        let started = std::time::Instant::now();
        let _ = codex.open(&path, len);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "R3BENCH file_mib={} min_ms={:.1} median_ms={:.1} x100_min_s={:.2}",
        len >> 20,
        times[0],
        times[3],
        times[0] * 100.0 / 1000.0
    );
}

// ------------------------------------------------------------- 4. ownership

/// Roots that are the same directory; a rollout whose first line is blank (the
/// main reader skips blanks and reads it) is recognised by no source, and the
/// first listed (Claude) takes it.
#[test]
fn o01_a_rollout_with_a_leading_blank_line_in_a_shared_root_is_still_codexs() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let both = tempfile::tempdir().unwrap();
    write_lines(
        &rollout_path(both.path(), "O1"),
        &[String::new(), session_meta("O1", w1), run(1, "ls", w1)],
    );
    let out = with_roots(Some(both.path()), Some(both.path()), || pair.pass());
    let codex_rows = rows(w1)
        .iter()
        .filter(|e| e.actor_id.as_deref() == Some("codex"))
        .count();
    assert_eq!(codex_rows, 1, "owned by the wrong source: {out:?}");
}

/// A rollout still being written when first listed (its first line cut) is
/// owned by no one, then by Codex once the line completes: one row, no replay.
#[test]
fn o02_a_rollout_that_changes_owner_between_passes_records_once() {
    let pair = Pair::new();
    let w1 = &pair.wt[0];
    let both = tempfile::tempdir().unwrap();
    let path = rollout_path(both.path(), "O2");
    let meta = session_meta("O2", w1);
    let (head, tail) = meta.split_at(meta.len() / 2);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, head).unwrap();
    with_roots(Some(both.path()), Some(both.path()), || {
        pair.pass();
        append_text(&path, &format!("{tail}\n{}\n", run(1, "ls", w1)));
        pair.pass();
        pair.pass();
    });
    let codex_rows = rows(w1)
        .iter()
        .filter(|e| e.actor_id.as_deref() == Some("codex"))
        .count();
    assert_eq!(codex_rows, 1, "rows: {}", rows(w1).len());
}
