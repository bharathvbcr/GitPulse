//! Adversarial suite for lane W4-L4 (reviewer). Tests marked `#[ignore = "BROKEN ..."]`
//! assert the contract and fail on this branch; run them with `-- --ignored`.

use super::*;

fn cmds_of(src: &str) -> (Vec<String>, Vec<&'static str>) {
    let p = scan(src, &LIMITS);
    (p.commands.iter().map(|c| c.cmd.clone()).collect(), p.skips)
}

#[allow(clippy::disallowed_methods)] // a read-only `ps` of this process, test only
fn rss_kib() -> usize {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

const C: &str = "tools.exec_command({cmd:'X'})";

/// Attack 1: a call that does not run unconditionally must never be recorded.
#[test]
fn a1_conditional_forms_are_counted_not_recorded() {
    for src in [
        format!("false && {C}"),
        format!("cond ? 1 : {C}"),
        format!("(()=>{C})()"),
        format!("()=>{C}"),
        format!("x: {C}"),
        format!("with (o) {C}"),
        format!("if (0) {C}"),
        format!("do {C}; while(0)"),
        format!("class A {{ x = {C} }}"),
        format!("class A {{ static {{ {C} }} }}"),
        format!("function f(a = {C}) {{}}"),
        format!("function* g() {{ yield {C} }}"),
        format!("async function h() {{ await {C} }}"),
        format!("x ||= {C}"),
        format!("[a = {C}] = []"),
        format!("({{[{C}]: 1}})"),
        format!("new Foo({C})"),
        format!("a?.[{C}]"),
        format!("a?.b?.({C})"),
    ] {
        let (cmds, skips) = cmds_of(&src);
        assert!(
            cmds.is_empty() && skips.len() == 1,
            "{src:?}: {cmds:?} {skips:?}"
        );
    }
}

#[test]
fn a1_unconditional_forms_are_recorded() {
    for src in [
        format!("(0, {C})"),
        format!("void {C}"),
        format!("0 * {C}"),
        format!("`${{{C}}}`"),
        format!("typeof {C}"),
        format!("(a?.b)({C})"),
        format!("do {{}} while(0); {C}"),
    ] {
        let (cmds, skips) = cmds_of(&src);
        assert_eq!((cmds.len(), skips.len()), (1, 0), "{src:?}");
    }
}

/// BROKEN: the optional chain `a?.b(...)` short-circuits its arguments too
/// (ECMA-262 OptionalChain evaluation), but the check only inspects a
/// `call_expression` that itself carries `?.`.
#[test]
#[ignore = "BROKEN a1: arguments of a call inside an optional chain"]
fn a1_optional_chain_arguments_are_conditional() {
    for src in [
        format!("a?.b({C})"),
        format!("a?.b.c({C})"),
        format!("a?.b[{C}]"),
        format!("a?.b`${{{C}}}`"),
        format!("a\n?.b({C})"),
    ] {
        let (cmds, _) = cmds_of(&src);
        assert!(cmds.is_empty(), "{src:?} recorded {cmds:?}");
    }
}

/// BROKEN (documented limitation, but statically visible): code after a top-level
/// `throw` or `return` never runs and is recorded.
#[test]
#[ignore = "BROKEN a1: unreachable code after top-level throw/return"]
fn a1_code_after_throw_or_return_is_unreachable() {
    for src in [format!("throw 1; {C}"), format!("return; {C}")] {
        let (cmds, _) = cmds_of(&src);
        assert!(cmds.is_empty(), "{src:?} recorded {cmds:?}");
    }
}

/// BROKEN: a program with a syntax error runs nothing, yet a call whose own
/// ancestors are clean (the ERROR node is a sibling, e.g. ` && (call)` with the left
/// operand deleted, or `if :x) call; else 1`) is recorded. 159 variants of 11
/// wrappers do this; the mention heuristic stays quiet because mentions == candidates.
#[test]
#[ignore = "BROKEN a1: ERROR sibling leaves the call recorded"]
fn a1_error_recovery_never_records_a_conditional_call() {
    let wrappers = [
        "if (x) {{ {C} }}",
        "for (;;) {{ {C} }}",
        "while (x) {{ {C} }}",
        "function f() {{ {C} }}",
        "async () => {{ {C} }}",
        "x && ({C})",
        "x ? {C} : 1",
        "try {{ {C} }} catch (e) {{}}",
        "switch (x) {{ case 1: {C} }}",
        "x || {C}",
        "if (x) {C}; else 1",
    ];
    let junk = [
        "", ")", "(", "}", "{", ";", "@", "#", "\\", "`", "'", "=>", "?", ":",
    ];
    let mut bad = Vec::new();
    for w in wrappers {
        let base = w.replace("{{", "{").replace("}}", "}").replace("{C}", C);
        let positions: Vec<usize> = base.char_indices().map(|(i, _)| i).collect();
        for &i in &positions {
            // Only mutate the wrapper, not the call text.
            let call_at = base.find(C).unwrap();
            if i >= call_at && i < call_at + C.len() {
                continue;
            }
            for j in junk {
                for variant in [
                    format!("{}{}", &base[..i], &base[i..]).replacen("", "", 0),
                    format!("{}{}{}", &base[..i], j, &base[i..]),
                    {
                        let mut e = i + base[i..].chars().next().map_or(0, char::len_utf8);
                        e = e.min(base.len());
                        format!("{}{}{}", &base[..i], j, &base[e..])
                    },
                ] {
                    let (cmds, skips) = cmds_of(&variant);
                    if !cmds.is_empty() && skips.is_empty() {
                        // Recorded: confirm it really is unconditional, i.e. the
                        // mutated program, when run with the call replaced by a
                        // marker, would still reach it. Only flag if the wrapper
                        // text still opens a conditional/loop/function before it.
                        let before = &variant[..variant.find(C).unwrap_or(0)];
                        let opens = [
                            "if", "for", "while", "function", "=>", "&&", "||", "?", "try",
                            "switch",
                        ]
                        .iter()
                        .any(|k| before.contains(k));
                        if opens {
                            bad.push(variant);
                        }
                    }
                }
            }
        }
    }
    bad.sort();
    bad.dedup();
    for b in &bad {
        println!("ADV1FAB {b:?}");
    }
    assert!(
        bad.is_empty(),
        "{} fabrications, first: {:?}",
        bad.len(),
        &bad[..bad.len().min(25)]
    );
}

/// Attack 2: every way to reach the tools object other than a direct callee is
/// counted. Reported by the first column of `skips`.
#[test]
fn a2_alias_forms_are_marked() {
    for src in [
        "const {exec_command} = tools; exec_command({cmd:'Y'})",
        "with(tools) { exec_command({cmd:'Y'}) }",
        "globalThis.tools.exec_command({cmd:'Y'})",
        "this.tools.exec_command({cmd:'Y'})",
        "Reflect.apply(tools.exec_command, tools, [{cmd:'Y'}])",
        "tools.exec_command.call(tools, {cmd:'Y'})",
        "tools.exec_command.apply(tools, [{cmd:'Y'}])",
        "tools.exec_command.bind(tools)({cmd:'Y'})",
        "new tools.exec_command({cmd:'Y'})",
        "Function('tools', 'tools.exec_command({cmd:\\'Y\\'})')(tools)",
    ] {
        let (cmds, skips) = cmds_of(src);
        assert!(
            cmds.is_empty() && !skips.is_empty(),
            "{src:?}: {cmds:?} {skips:?}"
        );
    }
}

/// BROKEN: code reached by a string (eval, Function, timer string) or a computed
/// global lookup mentions `tools` only inside a string literal; no marker, no skip.
#[test]
#[ignore = "BROKEN a2: tools reached through a string leaves no marker"]
fn a2_string_reachable_tools_is_never_silently_lost() {
    for src in [
        "globalThis['tools'].exec_command({cmd:'Y'})",
        "eval(\"tools.exec_command({cmd:'Y'})\")",
        "new Function(\"return tools\")().exec_command({cmd:'Y'})",
        "setTimeout(\"tools.exec_command({cmd:'Y'})\")",
    ] {
        let (cmds, skips) = cmds_of(src);
        assert!(
            cmds.is_empty() && !skips.is_empty(),
            "{src:?}: {cmds:?} {skips:?}"
        );
    }
}

/// Attack 3: decoding against V8 (node 26, sloppy `vm.runInNewContext`; see
/// tmp/oracle.cjs). `None` is "SyntaxError in V8" or, for a lone surrogate, valid in
/// V8 but not representable as a Rust `String` (counted as a bad escape).
#[test]
fn a3_literals_decode_as_v8_does() {
    let cases: &[(&str, Option<&str>)] = &[
        (r"'\u{10FFFF}'", Some("\u{10FFFF}")),
        (r"'\u{110000}'", None),
        (r"'😀'", Some("\u{1F600}")),
        (r"'\u{D83D}\u{DE00}'", Some("\u{1F600}")),
        (r"'\0'", Some("\0")),
        (r"'\00'", Some("\0")),
        (r"'\08'", Some("\08")),
        (r"'\1'", Some("\u{1}")),
        (r"'\377'", Some("\u{ff}")),
        (r"'\400'", Some(" 0")),
        (r"'\8\9'", Some("89")),
        (r"'\x41\x4'", None),
        ("'a\\\nb'", Some("ab")),
        ("'a\\\r\nb'", Some("ab")),
        ("`a\\\r\nb`", Some("ab")),
        ("`a\nb`", Some("a\nb")),
        ("`a\r\nb`", Some("a\nb")),
        ("`a\rb`", Some("a\nb")),
        ("`a\\\rb`", Some("ab")),
        (r"`\0`", Some("\0")),
        (r"`\00`", None),
        (r"`\1`", None),
        (r"`\8`", None),
        (r"`\u{41}`", Some("A")),
        (r"`\x41`", Some("A")),
        (r"`\x4`", None),
        (r"'\u{00000041}'", Some("A")),
        (r"'\u{}'", None),
        (r"'\v\f\b'", Some("\u{b}\u{c}\u{8}")),
        ("'\u{2028}'", Some("\u{2028}")),
        ("'a\\\u{2028}b'", Some("ab")),
        (r"'\a\q\z'", Some("aqz")),
        (r"`\u{1F600}`", Some("\u{1F600}")),
        (r"`\u{110000}`", None),
        (r"'\u{41'", None),
        (r"'\u00'", None),
        (r"'\uD83D'", None),
    ];
    for (lit, want) in cases {
        let src = format!("tools.exec_command({{cmd:{lit}}})");
        let (cmds, skips) = cmds_of(&src);
        match want {
            Some(w) => assert_eq!((cmds, skips.len()), (vec![w.to_string()], 0), "{lit}"),
            None => assert!(
                cmds.is_empty() && skips.len() == 1,
                "{lit}: {cmds:?} {skips:?}"
            ),
        }
    }
    // String.raw is a tagged template: counted, not decoded.
    let (cmds, skips) = cmds_of("tools.exec_command({cmd:String.raw`a\\nb`})");
    assert!(cmds.is_empty() && skips.len() == 1);
}

/// Attack 4: size cap exactly and one over.
#[test]
fn a4_byte_cap_is_exact() {
    let call = "tools.exec_command({cmd:'X'});\n";
    let pad = LIMITS.bytes - call.len();
    let at = format!("{}{call}", " ".repeat(pad));
    assert_eq!(at.len(), LIMITS.bytes);
    let p = scan(&at, &LIMITS);
    assert_eq!((p.commands.len(), p.skips.len()), (1, 0), "{:?}", p.skips);
    let over = format!(" {at}");
    assert_eq!(scan(&over, &LIMITS).skips, vec![TOO_LARGE]);
}

/// Parse wall time and false-skip rate against the shipped 250 ms deadline, for
/// programs of realistic and dense shapes. Prints rows; asserts nothing about time.
#[test]
#[ignore = "benchmark: run explicitly, prints ADV4 rows"]
fn a4_deadline_false_skip_rate() {
    let wide = Limits {
        bytes: usize::MAX,
        parse: Duration::from_secs(60),
        nodes: usize::MAX,
    };
    let shapes: [(&str, String); 2] = [
        (
            "dense-calls",
            "await tools.exec_command({cmd:'echo hello', workdir:'/tmp'});\n".to_string(),
        ),
        (
            "realistic",
            "const r = await tools.exec_command({cmd: `rg -n \"foo\" src/lib | head -50`, workdir: '/repo'});\nconsole.log(r.output.slice(0, 200));\n".to_string(),
        ),
    ];
    for (name, unit) in shapes {
        for size in [4 << 10, 16 << 10, 64 << 10, 128 << 10, 256 << 10] {
            let mut source = unit.repeat(size / unit.len() + 1);
            source.truncate(size - size % unit.len().min(size));
            let mut times = Vec::new();
            let (mut timeouts, mut budget) = (0, 0);
            for _ in 0..15 {
                let t = Instant::now();
                let _ = scan(&source, &wide);
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                let p = scan(&source, &LIMITS);
                timeouts += usize::from(p.skips.contains(&PARSE_TIMEOUT));
                budget += usize::from(p.skips.contains(&NODE_BUDGET));
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!(
                "ADV4 {name} bytes={} min_ms={:.1} median_ms={:.1} max_ms={:.1} shipped_timeouts={timeouts}/15 budget_skips={budget}/15",
                source.len(),
                times[0],
                times[7],
                times[14]
            );
        }
    }
}

/// The node budget discards the whole program, found calls included.
#[test]
fn a4_node_budget_is_all_or_nothing_at_every_threshold() {
    let source = "await tools.exec_command({cmd:'a'});\n".repeat(20);
    let full = scan(&source, &LIMITS);
    assert_eq!((full.commands.len(), full.skips.len()), (20, 0));
    for nodes in (1..=800).step_by(7) {
        let limits = Limits {
            bytes: LIMITS.bytes,
            parse: LIMITS.parse,
            nodes,
        };
        let p = scan(&source, &limits);
        let ok_full = p.commands.len() == 20 && p.skips.is_empty();
        let ok_skip = p.commands.is_empty() && p.skips == [NODE_BUDGET];
        assert!(ok_full || ok_skip, "nodes={nodes}: {p:?}");
    }
}

/// 500k-deep nesting of several shapes, with every bound lifted, must not overflow
/// the stack (test threads have a small one); and under the shipped limits it is a
/// counted skip.
#[test]
fn a4_deep_nesting_does_not_overflow() {
    let wide = Limits {
        bytes: usize::MAX,
        parse: Duration::from_secs(120),
        nodes: usize::MAX,
    };
    let call = "tools.exec_command({cmd:'a'})";
    for (open, close) in [
        ("(", ")"),
        ("[", "]"),
        ("{a:", "}"),
        ("!", ""),
        ("a?", ":b"),
    ] {
        let n = 500_000;
        let source = format!("{}{call}{}", open.repeat(n), close.repeat(n));
        let p = scan(&source, &wide);
        // Whatever the grammar does, a row is only legitimate if nothing hid it.
        assert!(
            p.commands.is_empty() || p.skips.is_empty(),
            "{open}: {} cmds, {:?}",
            p.commands.len(),
            p.skips
        );
        let shipped = scan(&source, &LIMITS);
        assert_eq!(shipped.skips, vec![TOO_LARGE], "{open}");
    }
    // Within the byte cap: 250k levels of `!` (one byte each, unary chain).
    let source = format!("{}{call}", "!".repeat(250_000));
    let p = scan(&source, &LIMITS);
    assert!(p.commands.is_empty() || p.skips.is_empty());
}

/// Fresh parser per program: memory does not grow across thousands of programs,
/// including aborted ones.
#[test]
#[ignore = "process-wide RSS: only meaningful alone, run with --test-threads=1"]
fn a4_many_programs_do_not_grow_memory() {
    let unit = "const r = await tools.exec_command({cmd:'echo hello', workdir:'/tmp'});\n";
    let source = unit.repeat(64 << 10 >> 6);
    let tight = Limits {
        bytes: LIMITS.bytes,
        parse: Duration::from_millis(1),
        nodes: LIMITS.nodes,
    };
    for _ in 0..20 {
        let _ = scan(&source, &LIMITS);
        let _ = scan(&source, &tight);
    }
    let before = rss_kib();
    for i in 0..400 {
        let _ = scan(&source, if i % 2 == 0 { &LIMITS } else { &tight });
    }
    let after = rss_kib();
    println!("ADV4 rss_before_kib={before} rss_after_kib={after}");
    assert!(after < before + 64 * 1024, "{before} -> {after} KiB");
}

/// Attack 5: the mention heuristic only runs on a syntax error and compares raw text
/// counts with candidate call nodes.
#[test]
fn a5_mention_heuristic_cases() {
    // Comment mentions + a real hidden call under a syntax error: over-counts (safe).
    let (cmds, skips) = cmds_of("// tools.exec_command\n tools.exec_command({cmd:'a'}) @@");
    println!("ADV5 comment+err => {cmds:?} {skips:?}");
    assert!(cmds.is_empty() && !skips.is_empty());
    // Escaped spelling inflates candidates but not mentions.
    let (cmds, skips) = cmds_of("tools[n](1); tools.\\u0065xec_command @@ ({cmd:'b'})");
    println!("ADV5 escaped+computed => {cmds:?} {skips:?}");
    assert!(
        cmds.is_empty() && !skips.is_empty(),
        "lost: {cmds:?} {skips:?}"
    );
}

/// BROKEN: no syntax error, so the heuristic never runs; the only call is inside a
/// string passed to eval.
#[test]
#[ignore = "BROKEN a5: heuristic gated on has_error"]
fn a5_valid_program_mentions_without_candidates_are_counted() {
    let (cmds, skips) = cmds_of("eval(\"tools.exec_command({cmd:'a'})\")");
    assert!(cmds.is_empty() && !skips.is_empty(), "{cmds:?} {skips:?}");
}
