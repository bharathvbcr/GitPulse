use super::*;

fn heavy(command: &str) -> Option<String> {
    classify(command).heavy.map(|hit| hit.label)
}

fn advised(command: &str) -> Vec<&'static str> {
    classify(command).advise.iter().map(|h| h.row.id).collect()
}

#[test]
fn every_heavy_command_the_table_names_is_heavy() {
    for (command, label) in [
        ("cargo build", "cargo build"),
        ("cargo b", "cargo b"),
        ("cargo check --all-targets", "cargo check"),
        ("cargo c", "cargo c"),
        ("cargo clippy --locked -- -D warnings", "cargo clippy"),
        ("cargo test --lib -- hooks::", "cargo test"),
        ("cargo t", "cargo t"),
        ("cargo bench", "cargo bench"),
        ("cargo nextest run", "cargo nextest"),
        ("cargo llvm-cov --lib", "cargo llvm-cov"),
        ("cargo doc --no-deps", "cargo doc"),
        ("cargo +nightly test", "cargo test"),
        ("/Users/x/.claude-bin/cargo test", "cargo test"),
        ("go build ./...", "go build"),
        ("go test ./pkg", "go test"),
        ("npm test", "npm test"),
        ("npm run test", "npm run test"),
        ("npm run test:browser", "npm run test:browser"),
        ("bun run ci:local", "bun run ci:local"),
        ("pnpm run test:unit", "pnpm run test:unit"),
        ("npm --prefix web test", "npm test"),
        ("vitest run src/a.test.ts", "vitest"),
        ("jest --runInBand", "jest"),
        ("node node_modules/vitest/vitest.mjs run x", "node"),
    ] {
        assert_eq!(heavy(command).as_deref(), Some(label), "{command}");
    }
}

#[test]
fn cheap_and_read_only_commands_are_not_heavy() {
    for command in [
        "cargo run",
        "cargo fmt",
        "cargo fmt --check",
        "cargo metadata --format-version 1",
        "cargo --version",
        "cargo",
        "cargo tree",
        "go version",
        "go env GOPATH",
        "go run main.go",
        "npm install",
        "npm run build",
        "npm run lint",
        "npm run dev",
        "git status",
        "git log --oneline",
        "ls -la",
        "rg cargo",
        "cat package.json",
        "echo done",
        "",
    ] {
        assert_eq!(heavy(command), None, "{command}");
    }
}

#[test]
fn it_sees_through_prefixes_chains_pipes_and_wrappers() {
    for command in [
        "FOO=1 cargo test",
        "FOO=1 BAR=2 cargo test",
        "env cargo test",
        "env -i FOO=1 cargo test",
        "env -u HOME cargo test",
        "cd src-tauri && cargo test",
        "cd src-tauri; cargo test",
        "false || cargo build",
        "ls | cargo test",
        "(cd a && (cargo test))",
        "echo a && echo b && { cargo clippy; }",
        "time cargo test",
        "nice -n 10 cargo bench",
        "nohup cargo build",
        "npx vitest run",
        "pnpm exec vitest run",
        "bun x vitest",
        "bash -c 'cargo test'",
        "sh -c \"cd x && cargo build\"",
        "echo $(cargo test)",
        "echo `cargo check`",
        "if cargo test; then echo ok; fi",
        "cargo test 2>&1 | tail -5",
    ] {
        assert!(heavy(command).is_some(), "{command}");
    }
}

#[test]
fn a_word_inside_a_quoted_string_is_not_a_program() {
    for command in [
        "echo \"cargo test\"",
        "echo 'cargo build && npm test'",
        "git commit -m \"run cargo test\"",
        "git commit -m 'go test ./...'",
        "printf '%s\\n' \"vitest\"",
        "echo cargo\\ test",
        "cat <<'EOF'\ncargo test\nEOF",
        "grep -n \"npm test\" README.md",
    ] {
        let found = classify(command);
        assert!(found.heavy.is_none(), "{command}: {:?}", found.heavy);
    }
}

#[test]
fn the_heavy_lane_holder_is_exempt_for_the_command_it_wraps_only() {
    for command in [
        "mac_heavy.sh cargo test",
        "./scripts/mac_heavy.sh cargo build",
        "cd x && mac_heavy.sh cargo test",
        "bash scripts/mac_heavy.sh cargo test",
        "MAC_HEAVY_LABEL=x mac_heavy.sh go test ./...",
    ] {
        assert!(heavy(command).is_none(), "{command}");
    }
    assert!(
        heavy("mac_heavy.sh cargo test && cargo build").is_some(),
        "the second command is not the holder's"
    );
}

#[test]
fn advise_only_rows_name_the_commands_that_took_the_host_down() {
    assert_eq!(advised("grep -r foo ."), vec!["grep-recursive"]);
    assert_eq!(advised("grep -rn foo src"), vec!["grep-recursive"]);
    assert_eq!(advised("grep -R foo ."), vec!["grep-recursive"]);
    assert_eq!(
        advised("grep -r --exclude-dir=target -z foo ."),
        vec!["grep-recursive"],
        "-z is on its own enough"
    );
    assert_eq!(
        advised("grep -r --exclude-dir=target foo ."),
        Vec::<&str>::new()
    );
    assert_eq!(advised("grep foo file.txt"), Vec::<&str>::new());
    assert_eq!(advised("rg -uu foo"), vec!["rg-unrestricted"]);
    assert_eq!(advised("rg --no-ignore foo"), vec!["rg-unrestricted"]);
    assert_eq!(advised("rg -uu -g '!target*' foo"), Vec::<&str>::new());
    assert_eq!(advised("rg foo"), Vec::<&str>::new());
    assert_eq!(advised("go test ./..."), vec!["go-test-unbounded"]);
    assert_eq!(
        advised("go test -fuzz FuzzX ./pkg"),
        vec!["go-test-unbounded"]
    );
    assert_eq!(advised("go test -timeout 5m ./..."), Vec::<&str>::new());
    assert_eq!(advised("go test ./pkg"), Vec::<&str>::new());
    assert_eq!(
        advised("python3 train.py --epochs 3"),
        vec!["python-train-bench"]
    );
    assert_eq!(advised("python bench_gemm.py"), vec!["python-train-bench"]);
    assert_eq!(advised("python script.py"), Vec::<&str>::new());
    assert!(
        heavy("go test ./...").is_some() && heavy("rg -uu foo").is_none(),
        "advise-only rows are never heavy"
    );
}

#[test]
fn hostile_command_lines_neither_panic_nor_pretend() {
    // Unterminated quotes: nothing is claimed, and it is said the line was not read.
    for command in [
        "cargo test \"foo",
        "cargo test 'foo",
        "echo `cargo test",
        "echo $(cargo test",
    ] {
        let found = classify(command);
        assert!(found.heavy.is_none(), "{command}");
        assert!(found.unreadable, "{command}");
    }
    // 10 KB of arguments, then the command.
    let filler = "a".repeat(10_000);
    assert!(heavy(&format!("echo {filler}; cargo test")).is_some());
    assert!(heavy(&format!("echo {filler}")).is_none());
    assert!(heavy(&format!("cargo test {filler}")).is_some());
    // Past the size the lexer reads.
    let huge = format!("echo {}; cargo test", "a".repeat(300 * 1024));
    let found = classify(&huge);
    assert!(found.heavy.is_none() && found.unreadable);
    // Unicode: a lookalike is not cargo, and a real one next to it still is.
    assert!(heavy("cärgo test").is_none());
    assert!(heavy("echo \"cargo test ✓ 日本語\"; cargo build ✓").is_some());
    assert!(heavy("echo ✓✓✓ 🦀").is_none());
    // Deep nesting and bash -c chains stop at their bound instead of recursing.
    let nested = (0..10).fold("cargo test".to_string(), |inner, _| {
        format!("bash -c '{}'", inner.replace('\'', "'\\''"))
    });
    let _ = classify(&nested);
    // The label is bounded and printable whatever the verb.
    let label = heavy("cargo test").unwrap();
    assert!(label.chars().count() <= 48);
}

#[test]
fn the_prefilter_does_not_hide_a_row_from_the_table() {
    // Every program in the table is found by the cheap look at the raw line,
    // or a row would never fire.
    for row in ROWS {
        for program in row.programs {
            let name = program.trim_end_matches('*');
            assert!(names_a_program(&format!("x && {name} y")), "{}", row.id);
        }
    }
    assert!(!names_a_program("git status"));
    assert!(!names_a_program("echo hello"));
}

#[test]
fn cargo_install_rustc_miri_are_heavy() {
    // The verbs the slot-gate shim has always gated and this table lacked:
    // each one compiles a crate graph, so each one is a build.
    for (command, label) in [
        ("cargo install --path .", "cargo install"),
        ("cargo rustc --release -- --emit asm", "cargo rustc"),
        ("cargo rustdoc", "cargo rustdoc"),
        ("cargo fix --allow-dirty", "cargo fix"),
        ("cargo miri test", "cargo miri"),
        ("cargo mutants", "cargo mutants"),
        ("cargo fuzz run target", "cargo fuzz"),
        ("cargo d", "cargo d"),
        (
            "cargo --manifest-path src-tauri/Cargo.toml test",
            "cargo test",
        ),
        ("cargo --target-dir /tmp/t build", "cargo build"),
        ("cargo --color never check", "cargo check"),
    ] {
        assert_eq!(heavy(command).as_deref(), Some(label), "{command}");
    }
}

#[test]
fn cargo_run_and_clean_are_never_leased() {
    // `run` can start a server that never exits: it would hold a slot forever.
    // The rest build nothing.
    for (program, args) in [
        ("cargo", vec!["run"]),
        ("cargo", vec!["r", "--release"]),
        ("cargo", vec!["clean"]),
        ("cargo", vec!["fmt", "--check"]),
        ("cargo", vec!["metadata", "--format-version", "1"]),
        ("cargo", vec!["tree"]),
        ("cargo", vec!["--version"]),
        ("cargo", vec!["help", "test"]),
        ("cargo", vec![]),
        ("go", vec!["run", "main.go"]),
        ("go", vec!["env", "GOPATH"]),
        ("go", vec!["mod", "tidy"]),
        ("go", vec!["version"]),
    ] {
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        assert!(!leased(program, &args), "{program} {args:?} must not lease");
    }
}

#[test]
fn a_build_by_argv_is_leased_whatever_path_named_the_program() {
    for (program, args) in [
        ("cargo", vec!["build", "--release"]),
        ("/Users/x/.cargo/bin/cargo", vec!["+nightly", "test"]),
        ("cargo", vec!["--manifest-path", "a/Cargo.toml", "clippy"]),
        ("cargo", vec!["install", "--path", "."]),
        ("go", vec!["test", "./..."]),
        ("go", vec!["-C", "sub", "build"]),
    ] {
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        assert!(leased(program, &args), "{program} {args:?} must lease");
    }
    // Heavy, but not a lease row: a package script is not something this
    // wrapper is installed in front of, and a test runner has no slot.
    for (program, args) in [("npm", vec!["test"]), ("vitest", vec!["run"])] {
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        assert!(
            classify_argv(program, &args).heavy.is_some(),
            "{program} stays heavy"
        );
        assert!(!leased(program, &args), "{program} has no lease row");
    }
}
