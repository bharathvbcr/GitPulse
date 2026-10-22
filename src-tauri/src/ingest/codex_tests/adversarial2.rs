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
        .open(
            std::path::Path::new("/nonexistent/rollout.jsonl"),
            0,
            &mut 0,
        )
        .unwrap()
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

// ------------------------------------------------------- 1. code mode (parsed)

/// Round 2 showed the old code-mode scanner wrong both ways (a real call lost to
/// a division it took for a regex; a fake one built from a regex literal). The
/// program is now read by the JavaScript grammar: each script below is checked
/// for the exact commands recorded and the exact count of calls it could not
/// read, so a call is neither lost silently nor made up.
fn assert_read(script: &str, expect: &[&str], skipped: i64) {
    let (cmds, counted) = js(script);
    assert_eq!(
        (cmds, counted),
        (
            expect.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            skipped
        ),
        "{script:?}"
    );
}

#[test]
fn l01_the_scripts_that_fooled_the_lexer_are_read_correctly() {
    for (script, expect) in [
        ("const x = a / b / tools.exec_command({cmd:'real'});", vec!["real"]),
        (
            "x = /tools.exec_command({cmd:'fake'})/; await tools.exec_command({cmd:'ok'})",
            vec!["ok"],
        ),
        ("n++ / 2; await tools.exec_command({cmd:'real'}); // done", vec!["real"]),
        (
            "const h = {a:1}.a; const o = {a:4}\n/ 2; await tools.exec_command({cmd:'real'}); const r = 6 / 3;",
            vec!["real"],
        ),
        // A regex literal holding a call is a regex, not a call.
        ("(async()=>{ return /tools.exec_command({cmd:'fake'})/.test('x') })()", vec![]),
        (
            "const f = () => { return /'/.test(s) }; await tools.exec_command({cmd:'real'}); const q = 'x'",
            vec!["real"],
        ),
        // Division after the `}` of an object literal and after `)`.
        ("x = {}\n/ 2; await tools.exec_command({cmd:'real'})", vec!["real"]),
        ("const q = (a + b) / 2 / (c); await tools.exec_command({cmd:'real'})", vec!["real"]),
        // `return /re/` is a regex, so the call inside is not a call.
        ("function f() { return /tools.exec_command({cmd:'fake'})/ }", vec![]),
        // `/'/.test(s); call`.
        ("/'/.test(s); await tools.exec_command({cmd:'real'}); '/'", vec!["real"]),
    ] {
        assert_read(script, &expect, 0);
    }
}

#[test]
fn l02_templates_escapes_names_and_line_ends_are_read_correctly() {
    for (script, expect, skipped) in [
        (
            "const s = `a ${ `inner ${ 1+1 } x` } b ${ {k:'}'}.k }`; await tools.exec_command({cmd:'after'})",
            vec!["after"],
            0,
        ),
        (
            "const s = `a \\` tools.exec_command({cmd:'fake'}) b`; await tools.exec_command({cmd:'real'})",
            vec!["real"],
            0,
        ),
        // Unicode-escaped names are the names they spell.
        ("await tools.exec_comman\\u0064({cmd:'rm x'})", vec!["rm x"], 0),
        ("await tools.\\u0065xec_command({cmd:'rm x'})", vec!["rm x"], 0),
        ("await \\u0074ools.exec_command({cmd:'rm x'})", vec!["rm x"], 0),
        ("await tools.exec_command({c\\u006dd:'rm x'})", vec!["rm x"], 0),
        // A computed name that is not a literal is counted, not read.
        ("await tools['exec_' + 'command']({cmd:'rm x'})", vec![], 1),
        ("await tools[`exec_command`]({cmd:'rm x'})", vec!["rm x"], 0),
        (
            "const s = '</script>'; const t = 'a\\\r\nb'; await tools.exec_command({cmd:'real'})",
            vec!["real"],
            0,
        ),
        // A lone CR and U+2028 end a line comment.
        ("// note\rawait tools.exec_command({cmd:'real'})", vec!["real"], 0),
        ("// note\u{2028}await tools.exec_command({cmd:'real'})", vec!["real"], 0),
    ] {
        assert_read(script, &expect, skipped);
    }
}

/// An unterminated block comment: the call before it is read, the one inside the
/// comment text never is.
#[test]
fn l02b_an_unterminated_comment_hides_nothing_and_fabricates_nothing() {
    let (cmds, skipped) =
        js("await tools.exec_command({cmd:'a'}); /* tools.exec_command({cmd:'b'})");
    assert_eq!(cmds, vec!["a".to_string()]);
    assert!(skipped >= 1, "the damaged tail is counted");
}

#[test]
fn l03_a_script_over_the_size_cap_is_one_counted_skip() {
    let unit = "await tools.exec_command({cmd:'real'});\n";
    for len in [(256 << 10) + 1, 2 << 20, (2 << 20) + 1, 8 << 20] {
        let mut script = unit.repeat(len / unit.len() + 1);
        script.truncate(len);
        assert_read(&script, &[], 1);
    }
}

/// A deterministic fuzz over the old lexer's alphabet: never a panic, never more
/// rows than the script has call-shaped words for, and the same answer twice.
#[test]
fn l04_any_script_is_read_without_panic_and_without_extra_rows() {
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
        "\u{e9}",
        "\u{1f600}",
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
        let first = js(&script);
        assert!(
            first.0.len() <= script.matches("exec_command").count(),
            "more rows than calls: {script:?} -> {first:?}"
        );
        assert_eq!(first, js(&script), "{script:?}");
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
        let mut budget = u64::MAX;
        let _ = codex.open(&path, len, &mut budget);
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
