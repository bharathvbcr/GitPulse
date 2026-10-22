//! Code-mode rollouts read by the JavaScript grammar: what is recorded, what is
//! counted and why, end to end. The attack suite of the old scanner lives in
//! `adversarial` (p07, p09) and `adversarial2` (l01-l04).

use super::adversarial::{assert_code_mode, js};
use super::*;

#[test]
fn only_straight_line_calls_are_recorded_and_the_rest_are_counted() {
    for (script, expect, skipped) in [
        // Top-level await statements, declarations, assignments and arguments.
        ("await tools.exec_command({cmd:'a'}); const r = await tools.exec_command({cmd:'b'}); let q; q = await tools.exec_command({cmd:'c'}); console.log(await tools.exec_command({cmd:'d'}))", vec!["a", "b", "c", "d"], 0),
        ("const [x, y] = await Promise.all([tools.exec_command({cmd:'a'}), tools.exec_command({cmd:'b'})])", vec!["a", "b"], 0),
        ("const out = (await tools.exec_command({cmd:'a'})).output?.trim()", vec!["a"], 0),
        // Code that may run zero or many times is counted, never recorded.
        ("if (ok) { await tools.exec_command({cmd:'a'}) }", vec![], 1),
        ("for (const c of cs) await tools.exec_command({cmd:'a'})", vec![], 1),
        ("while (go) { await tools.exec_command({cmd:'a'}) }", vec![], 1),
        ("try { await tools.exec_command({cmd:'a'}) } catch (e) {}", vec![], 1),
        ("async function f() { await tools.exec_command({cmd:'a'}) }", vec![], 1),
        ("const f = async () => tools.exec_command({cmd:'a'})", vec![], 1),
        ("[1].forEach(async () => { await tools.exec_command({cmd:'a'}) })", vec![], 1),
        ("ok && await tools.exec_command({cmd:'a'})", vec![], 1),
        ("ok ? await tools.exec_command({cmd:'a'}) : 0", vec![], 1),
        ("ok ?? await tools.exec_command({cmd:'a'})", vec![], 1),
        ("f?.(await tools.exec_command({cmd:'a'}))", vec![], 1),
        ("switch (x) { case 1: await tools.exec_command({cmd:'a'}) }", vec![], 1),
        // Straight-line calls around a conditional one are still recorded.
        ("await tools.exec_command({cmd:'a'}); if (x) { await tools.exec_command({cmd:'b'}) } await tools.exec_command({cmd:'c'})", vec!["a", "c"], 1),
    ] {
        assert_code_mode(script, &expect, skipped);
    }
}

#[test]
fn an_alias_of_tools_is_one_marker_and_direct_calls_are_still_recorded() {
    for script in [
        "const t = tools; await t.exec_command({cmd:'x'}); await tools.exec_command({cmd:'a'})",
        "let u; u = tools; await tools.exec_command({cmd:'a'})",
        "const { exec_command } = tools; await tools.exec_command({cmd:'a'})",
        "const f = tools.exec_command; await tools.exec_command({cmd:'a'})",
        "tools = other; await tools.exec_command({cmd:'a'})",
        "run(tools); await tools.exec_command({cmd:'a'})",
        "const t = globalThis.tools; await tools.exec_command({cmd:'a'})",
        "function g(tools) {} await tools.exec_command({cmd:'a'})",
    ] {
        assert_code_mode(script, &["a"], 1);
    }
    // Another tool by a literal name is not an alias.
    assert_code_mode(
        "await tools.read_file({path:'x'}); await tools.exec_command({cmd:'a'})",
        &["a"],
        0,
    );
}

#[test]
fn arguments_that_are_not_literals_are_counted_with_the_call() {
    for script in [
        "await tools.exec_command({cmd: 'a' + 'b'})",
        "await tools.exec_command({cmd: c})",
        "await tools.exec_command({cmd})",
        "await tools.exec_command({...o})",
        "await tools.exec_command({['cmd']: 'a'})",
        "await tools.exec_command({cmd: 'a', workdir: w})",
        "await tools.exec_command({workdir: '/r'})",
        "await tools.exec_command(opts)",
        "await tools.exec_command()",
        "await tools.exec_command({cmd:'a'}, extra)",
        "await tools.exec_command`ls`",
        "await tools.exec_command({cmd: '\\x4'})",
        "await tools.exec_command({cmd: '\\uD800'})",
        "await tools.exec_command({cmd: `a\\1`})",
        "await tools[name]({cmd:'a'})",
        "await tools.apply_patch(patch)",
        "await tools.apply_patch('no headers here')",
        "await tools.apply_patch(`*** Update File: ${f}`)",
    ] {
        assert_code_mode(script, &[], 1);
    }
}

#[test]
fn literal_arguments_are_decoded() {
    assert_code_mode(
        r#"await tools.exec_command({"cmd": "printf 'a\tb\n' \u{41}B", 'workdir': "/w\x2fx"})"#,
        &["printf 'a\tb\n' AB"],
        0,
    );
    // The last of a repeated key wins, as in JavaScript.
    assert_code_mode("await tools.exec_command({cmd:'a', cmd:'b'})", &["b"], 0);
    // Keys other than cmd and workdir are not read.
    assert_code_mode(
        "await tools.exec_command({cmd:'a', yield_time_ms: 1000, tty: true})",
        &["a"],
        0,
    );
}

#[test]
fn a_code_mode_patch_becomes_edit_rows_for_its_header_paths() {
    let (calls, skipped) = js("await tools.apply_patch(`*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: /abs/b.rs\n+z\n*** End Patch`)");
    assert_eq!(skipped, 0);
    let seen: Vec<_> = calls
        .iter()
        .map(|c| (c.tool.as_str(), c.file_path.as_deref().unwrap_or_default()))
        .collect();
    assert_eq!(seen, vec![("Edit", "/r/src/a.rs"), ("Edit", "/abs/b.rs")]);
}

#[test]
fn a_typescript_annotation_does_not_lose_a_call_that_parses_whole() {
    assert_code_mode(
        "const n: number = 1; await tools.exec_command({cmd:'a'})",
        &["a"],
        0,
    );
}

#[test]
fn code_mode_rows_are_attributed_like_structured_ones_end_to_end() {
    let pair = Pair::new();
    let codex = tempfile::tempdir().unwrap();
    let empty_claude = tempfile::tempdir().unwrap();
    let [w1, w2] = &pair.wt;
    write_lines(
        &rollout_path(codex.path(), "CM1"),
        &[
            session_meta("CM1", &pair.base),
            exec(
                1,
                &format!(
                    "await tools.exec_command({{cmd:'ls', workdir:'w2'}});\nawait tools.exec_command({{cmd:'pwd', workdir:'{w1}/../w1'}});\nif (x) {{ await tools.exec_command({{cmd:'never'}}) }}"
                ),
            ),
        ],
    );
    let out = with_roots(Some(empty_claude.path()), Some(codex.path()), || {
        pair.pass()
    });
    assert_eq!(out.recorded, 2, "{out:?}");
    assert_eq!(
        out.skipped_lines, 1,
        "the conditional call is counted: {out:?}"
    );
    let mut seen: Vec<_> = rows(w1)
        .into_iter()
        .map(|e| {
            assert_eq!(e.actor_id.as_deref(), Some("codex"));
            (
                e.action,
                e.object.unwrap(),
                e.worktree_path.unwrap_or_default(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("session.command".to_string(), "ls".to_string(), w2.clone()),
            (
                "session.command".to_string(),
                "pwd".to_string(),
                String::new()
            ),
        ]
    );
}
