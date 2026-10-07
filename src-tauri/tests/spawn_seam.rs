//! Every production subprocess must go through the gated seam in
//! `engine::git_cli`.
//!
//! The descriptor budget is only a budget for what passes through the gate.
//! A `Command::new` written anywhere else spawns outside it, and a workspace
//! with several repositories open walks back into the "Failed to spawn git
//! ...: Too many open files (os error 24)" storm that `limits::raise_open_
//! file_limit` and the gate exist between them to prevent — on a process whose
//! limit was raised, which is the version of the failure that looks impossible
//! from the log. It also loses the command timeout, the stdout/stderr caps,
//! the scrubbed `GIT_*` environment and the GUI-launch program lookup.
//!
//! So the absence is asserted rather than intended. This walks `src/`, strips
//! test-only items and the files of test-only modules, and fails on any
//! `Command::new` outside the allowlist below — naming the file and line, and
//! reporting how much it actually read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Files permitted to construct a `Command` directly, and why.
const ALLOWED: &[(&str, &str)] = &[
    (
        "src/engine/git_cli.rs",
        "the seam itself: it owns the gate, the timeout and the output caps",
    ),
    (
        "src/harness/sidecar.rs",
        "one long-lived sidecar per session; holding a gate permit for its \
         whole life would starve every git call behind it",
    ),
    (
        "src/procguard/mod.rs",
        "the Windows arm of the tree kill (`taskkill /T /F`), which moved here \
         from `git_cli::kill_process_tree` when process-group teardown became \
         one owner. It kills children rather than doing work: a gate permit, a \
         timeout and an output cap are exactly what a shutdown must not wait \
         on. `procguard::spawn` itself never constructs a `Command` — it is \
         handed one the caller built inside the seam, so the rule this test \
         exists for is untouched",
    ),
    (
        "src/tool_install/mod.rs",
        "optional-tools install ladder (cargo/go/curl and binary probes). These \
         are not git; every `Command` is handed to `git_cli::run_bounded_capped` \
         so the timeout and stdout/stderr caps still apply. Holding a gate \
         permit for a multi-minute `cargo install` would starve every git call",
    ),
    (
        "src/tool_install/release.rs",
        "release-asset discovery for the same ladder; `Command`s are probes \
         handed to `run_bounded_capped`, not ungated spawns",
    ),
    (
        "src/devmap/cli.rs",
        "devmap binary invocation for map builds/queries; builds a `Command` \
         then hands it to `git_cli::run_bounded_capped`",
    ),
    (
        "src/tool_install/components.rs",
        "DevCouncil component discovery, the same probe ladder as \
         `tool_install/mod.rs`: its single `Command` is handed to \
         `git_cli::run_bounded_capped` with a 5s timeout and a 64 KiB cap, and \
         it runs in `std::env::temp_dir()` so a version probe can never read a \
         repository. Not git, and a gate permit held across a probe of every \
         optional component would starve the git calls behind it",
    ),
    (
        "src/engine/worktree_hooks.rs",
        "repository hook commands (`sh -c` / `cmd /C`) with the repository's \
         own env, run only after `repository_trust::require`. Each `Command` is \
         handed to `git_cli::run_bounded_capped` with a 15-minute deadline and a \
         1 MiB cap; it was `child.output()`, unbounded",
    ),
    (
        "src/secrets/run.rs",
        "the kingfisher secret scanner: `scrubbed_command` builds a `Command` \
         with `env_clear()` and a scrubbed environment, which the git seam's \
         helpers cannot express, then hands it to `git_cli::run_bounded_capped` \
         with the scan and version deadlines and a stdout cap",
    ),
];

#[test]
fn no_production_code_spawns_outside_the_gated_seam() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(
        files.len() > 50,
        "expected to walk the whole crate, only found {} files — a scan that \
         did not run must not read as a scan that passed",
        files.len()
    );
    let test_only = test_only_files(&root, &files);
    assert!(
        !test_only.is_empty(),
        "found no test-only module files, though `src/` declares several — a \
         resolver that matches nothing would scan them all as production"
    );

    let mut offenders: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut scanned_lines = 0usize;
    for file in &files {
        if test_only.contains(file) {
            continue;
        }
        let rel = file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).expect("read source");
        let production = strip_test_items(&text);
        scanned_lines += production.len();
        if ALLOWED.iter().any(|(allowed, _)| *allowed == rel) {
            continue;
        }
        for (line_no, line) in &production {
            if line.contains("Command::new") && !line.trim_start().starts_with("//") {
                offenders.entry(rel.clone()).or_default().push(*line_no);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these spawn outside `engine::git_cli`, so they are not covered by the \
         descriptor gate, the timeout or the output caps: {offenders:?}\n\
         Route them through `git`, `git_text`, `git_captured` or \
         `capture_command`, or add the file to ALLOWED with the reason.",
    );

    // A pass has to be able to say what it looked at: an allowlist that has
    // rotted into naming files that no longer exist would otherwise read the
    // same as a clean scan.
    for (allowed, _) in ALLOWED {
        assert!(
            root.parent().expect("manifest dir").join(allowed).is_file(),
            "ALLOWED names {allowed}, which is not a file any more"
        );
    }
    eprintln!(
        "scanned {} files, {scanned_lines} production lines, {} allowlisted, \
         {} skipped as test-only modules",
        files.len() - test_only.len(),
        ALLOWED.len(),
        test_only.len()
    );
}

/// Creating a process, or a descriptor that is about to be made `FD_CLOEXEC`,
/// must happen under `procguard::with_inheritance_lock`.
///
/// `std` has no `pipe2` on macOS or the BSDs, so its pipes are `pipe()`
/// followed by two `fcntl(F_SETFD, FD_CLOEXEC)` calls. A process created on
/// another thread in between inherits the pipe for life, and a stolen *write*
/// end means the owner's pipe never reaches EOF — the "could not be read to
/// the end" loss the app's own diagnostics reported against `git diff`,
/// `show`, `rev-list` and `for-each-ref`.
///
/// `Command::new` is covered by the seam above, which funnels every one of
/// those into `procguard::spawn`. The PTY is the other creator, and it is
/// invisible to that check: it builds a `CommandBuilder`, not a `Command`.
/// It is also the one whose children live for hours, so a theft there is not
/// repaid in the next millisecond. A raw `libc::pipe` is the third: it is not
/// close-on-exec at all until its own `fcntl` calls run. Asserted by shape
/// rather than trusted: each must name the lock on its own line.
/// Every child created from a `Command` — tests, benches and tools included —
/// takes the inheritance lock, because `clippy.toml` bans the three `std`
/// methods that create one and the only sanctioned ways round the ban are
/// `procguard::spawn` and `procguard::LockedSpawn`. This keeps the ban from
/// being deleted, and keeps the exemptions to the sites that were reviewed:
/// one test that forked without the lock kept another test's pipe open.
#[test]
fn every_command_spawn_is_banned_outside_the_locked_seams() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config = std::fs::read_to_string(root.join("clippy.toml")).expect("clippy.toml");
    for method in ["spawn", "output", "status"] {
        let path = format!("\"std::process::Command::{method}\"");
        assert!(config.contains(&path), "clippy.toml no longer bans {path}");
    }

    let mut files = vec![root.join("build.rs")];
    for dir in ["src", "tests", "benches"] {
        collect_rs(&root.join(dir), &mut files);
    }
    // Split so this file does not match itself.
    const EXEMPTION: &str = concat!("#[allow(clippy::", "disallowed_methods)]");
    let mut exemptions: BTreeMap<String, usize> = BTreeMap::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source");
        let count = text
            .lines()
            .filter(|line| line.trim_start().starts_with(EXEMPTION))
            .count();
        if count > 0 {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            exemptions.insert(rel, count);
        }
    }
    let expected: BTreeMap<String, usize> = [
        // A build script is its own single-threaded process.
        ("build.rs", 2),
        // `procguard::spawn` and `LockedSpawn::spawn_locked`, both under the lock.
        ("src/procguard/mod.rs", 2),
    ]
    .into_iter()
    .map(|(file, count)| (file.to_string(), count))
    .collect();
    assert_eq!(
        exemptions, expected,
        "a new exemption from the spawn ban: route the child through \
         procguard::spawn or procguard::LockedSpawn instead"
    );
}

#[test]
fn pty_creation_runs_under_the_inheritance_lock() {
    const CREATORS: &[&str] = &[".openpty(", ".spawn_command(", "libc::pipe("];
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    let test_only = test_only_files(&root, &files);

    let mut unguarded: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut guarded = 0usize;
    for file in &files {
        if test_only.contains(file) {
            continue;
        }
        let rel = file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).expect("read source");
        for (line_no, line) in strip_test_items(&text) {
            if line.trim_start().starts_with("//") || !CREATORS.iter().any(|c| line.contains(c)) {
                continue;
            }
            if line.contains("with_inheritance_lock") {
                guarded += 1;
            } else {
                unguarded.entry(rel.clone()).or_default().push(line_no);
            }
        }
    }

    assert!(
        unguarded.is_empty(),
        "these create a process, a PTY or a pipe descriptor outside \
         `procguard::with_inheritance_lock`, so they can be handed — or hand \
         away — a descriptor that is not yet `FD_CLOEXEC`: {unguarded:?}",
    );
    // A scan that found nothing to check must not read like a scan that found
    // everything guarded.
    assert_eq!(
        guarded, 3,
        "expected the PTY's `openpty` and `spawn_command` and procguard's wake \
         pipe, and nothing else; found {guarded} guarded creation sites"
    );
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether a `#[cfg(...)]` attribute compiles its item only under test:
/// `cfg(test)`, or an `all(...)` with a bare `test` arm.
///
/// Never `any(...)` — `#[cfg(any(target_os = "macos", test))]` is production
/// code on macOS — and never `not(test)`, which is production only. Anything
/// this does not recognise is scanned as production, which can only make the
/// guards stricter.
fn cfg_requires_test(attr: &str) -> bool {
    let compact: String = attr.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(predicate) = compact
        .strip_prefix("#[cfg(")
        .and_then(|rest| rest.strip_suffix(")]"))
    else {
        return false;
    };
    if predicate == "test" {
        return true;
    }
    let Some(arms) = predicate
        .strip_prefix("all(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in arms.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                if &arms[start..i] == "test" {
                    return true;
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    &arms[start..] == "test"
}

/// The module name of an out-of-line declaration (`mod x;`, `pub(crate) mod
/// x;`), or `None` for anything else, inline modules included.
fn out_of_line_mod(line: &str) -> Option<&str> {
    let mut rest = line.trim();
    if let Some(after) = rest.strip_prefix("pub") {
        rest = after.trim_start();
        if rest.starts_with('(') {
            rest = rest[rest.find(')')? + 1..].trim_start();
        }
    }
    let name = rest.strip_prefix("mod ")?.strip_suffix(';')?.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(name)
}

/// Files under `src/` that are compiled only under `cfg(test)`, though nothing
/// inside them says so.
///
/// An out-of-line test module (`#[cfg(test)] mod tests;`) is a separate file,
/// and its gate lives on the declaration in the parent: `strip_test_items`
/// removes that one line, and the file itself would otherwise be scanned as
/// production. Every module such a file declares is test-only too, cfg or not.
///
/// Resolution follows the compiler's rules, and fails loudly rather than
/// guessing: a test-only declaration that names no file, or a file the walk
/// never found, panics instead of silently exempting nothing — or the wrong
/// thing.
///
/// The exemption is by file, so it must not reach a file that production also
/// compiles: one declared both behind a test gate and by a plain `mod` is
/// production code, and claiming it here would take it out of the scan.
fn test_only_files(src_root: &Path, files: &[PathBuf]) -> BTreeSet<PathBuf> {
    let walked: BTreeSet<&PathBuf> = files.iter().collect();
    let mut test_only = BTreeSet::new();
    let mut pending: Vec<(PathBuf, bool)> = files.iter().map(|f| (f.clone(), false)).collect();
    while let Some((file, file_is_test)) = pending.pop() {
        let text = std::fs::read_to_string(&file).expect("read source");
        for decl in out_of_line_decls(&file, &text) {
            if !file_is_test && !decl.test_gated() {
                continue;
            }
            let resolved =
                locate_module(src_root, &file, decl.name, &decl.attrs).unwrap_or_else(|| {
                    panic!(
                        "{}: test-only `mod {};` resolves to no file",
                        file.display(),
                        decl.name
                    )
                });
            assert!(
                walked.contains(&resolved),
                "{} declares test-only `mod {};` at {}, which the walk of \
                 src/ never found",
                file.display(),
                decl.name,
                resolved.display()
            );
            if test_only.insert(resolved.clone()) {
                pending.push((resolved, true));
            }
        }
    }

    // Only now that the closure is complete: before it, a test-only file's
    // own ungated `mod env;` would read as a production declaration.
    for file in files.iter().filter(|f| !test_only.contains(*f)) {
        let text = std::fs::read_to_string(file).expect("read source");
        for decl in out_of_line_decls(file, &text) {
            if decl.test_gated() {
                continue;
            }
            // A production declaration that names no file is the compiler's
            // error to report, not this scan's.
            let Some(target) = locate_module(src_root, file, decl.name, &decl.attrs) else {
                continue;
            };
            assert!(
                !test_only.contains(&target),
                "{} is declared behind a test gate, and also by production \
                 `mod {};` in {}, so it compiles into production and must be \
                 scanned as production",
                target.display(),
                decl.name,
                file.display()
            );
        }
    }
    test_only
}

/// An out-of-line `mod name;` declaration and the attributes directly above it.
struct ModDecl<'a> {
    name: &'a str,
    attrs: Vec<&'a str>,
}

impl ModDecl<'_> {
    fn test_gated(&self) -> bool {
        self.attrs.iter().any(|a| cfg_requires_test(a))
    }
}

/// Every out-of-line module declaration in `text`, with its attribute run.
/// Doc comments and blank lines between the attributes and the item are
/// passed over; any other line ends the run.
fn out_of_line_decls<'a>(file: &Path, text: &'a str) -> Vec<ModDecl<'a>> {
    let mut decls = Vec::new();
    let mut attrs: Vec<&str> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        if line.starts_with("#[") {
            attrs.push(line);
            continue;
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let item_attrs = std::mem::take(&mut attrs);
        let Some(name) = out_of_line_mod(line) else {
            continue;
        };
        // Nested in an inline module, the file sits under that module's
        // directory too. None exist; placing one wrongly could exempt — or
        // fail to see — the wrong file, so it stops the scan instead.
        assert!(
            !raw.starts_with(char::is_whitespace),
            "{}: `{}` is an out-of-line module inside an inline one, which \
             this resolver does not place; extend it rather than guess",
            file.display(),
            line.trim()
        );
        decls.push(ModDecl {
            name,
            attrs: item_attrs,
        });
    }
    decls
}

/// The file `mod name;` in `parent` refers to, by the compiler's rules: a
/// `#[path]` is relative to the declaring file's directory; otherwise a crate
/// root or `mod.rs` owns its own directory, and any other file a directory
/// named after its stem. `None` when no such file exists.
fn locate_module(src_root: &Path, parent: &Path, name: &str, attrs: &[&str]) -> Option<PathBuf> {
    let dir = parent.parent().expect("a source file has a directory");
    let explicit = attrs.iter().find_map(|a| {
        let compact: String = a.chars().filter(|c| !c.is_whitespace()).collect();
        compact
            .strip_prefix("#[path=\"")?
            .strip_suffix("\"]")
            .map(str::to_owned)
    });
    if let Some(path) = explicit {
        let resolved = normalize(&dir.join(path));
        return resolved.is_file().then_some(resolved);
    }
    let stem = parent.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let crate_root = parent == src_root.join("lib.rs")
        || parent == src_root.join("main.rs")
        || dir == src_root.join("bin");
    let base = if crate_root || stem == "mod" {
        dir.to_path_buf()
    } else {
        dir.join(stem)
    };
    [
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

/// Folds `..` and `.` lexically, so a `#[path = "../x.rs"]` compares equal to
/// the path the directory walk produced for the same file.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Returns `(1-based line number, text)` for every line outside a test-only
/// item (see `cfg_requires_test`).
///
/// Brace-matched rather than "everything before the first `#[cfg(test)]`":
/// several modules here put production code *after* their test module, and a
/// truncating scan would report those files as clean without having read them.
fn strip_test_items(src: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut kept = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if cfg_requires_test(lines[i].trim()) {
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim_start().starts_with("#[") {
                j += 1;
            }
            let mut depth: i32 = 0;
            let mut opened = false;
            while j < lines.len() {
                depth += lines[j].matches('{').count() as i32;
                depth -= lines[j].matches('}').count() as i32;
                if lines[j].contains('{') {
                    opened = true;
                }
                if opened && depth <= 0 {
                    break;
                }
                // A `#[cfg(test)] use ...;` has no braces at all.
                if !opened && lines[j].trim_end().ends_with(';') {
                    break;
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        kept.push((i + 1, lines[i]));
        i += 1;
    }
    kept
}

/// The stripper is the load-bearing half of the check above: if it silently
/// swallowed production code, the scan would pass by not looking.
#[test]
fn the_test_stripper_keeps_production_code_on_both_sides_of_a_test_module() {
    let src = "fn before() {}\n\
               #[cfg(test)]\n\
               mod tests {\n\
               fn hidden() { let _ = 1; }\n\
               }\n\
               fn after() {}\n";
    let kept: Vec<&str> = strip_test_items(src).into_iter().map(|(_, l)| l).collect();
    assert_eq!(kept, vec!["fn before() {}", "fn after() {}"]);

    let attr = "#[cfg(test)]\nuse std::process::Command;\nfn after() {}\n";
    let kept: Vec<&str> = strip_test_items(attr).into_iter().map(|(_, l)| l).collect();
    assert_eq!(kept, vec!["fn after() {}"]);

    // `all(test, ...)` is test-only; `any(..., test)` is production on the
    // other arm and must stay in the scan.
    let all = "#[cfg(all(test, unix))]\nmod tests {\nfn hidden() {}\n}\nfn after() {}\n";
    let kept: Vec<&str> = strip_test_items(all).into_iter().map(|(_, l)| l).collect();
    assert_eq!(kept, vec!["fn after() {}"]);
    let any = "#[cfg(any(unix, test))]\nfn kept() {}\n";
    let kept: Vec<&str> = strip_test_items(any).into_iter().map(|(_, l)| l).collect();
    assert_eq!(kept, vec!["#[cfg(any(unix, test))]", "fn kept() {}"]);
}

#[test]
fn cfg_recognition_admits_only_test_only_predicates() {
    for attr in [
        "#[cfg(test)]",
        "#[cfg(all(test, unix))]",
        "#[cfg(all(unix, test))]",
        "#[cfg( all( not(windows), test ) )]",
    ] {
        assert!(cfg_requires_test(attr), "{attr} is test-only");
    }
    for attr in [
        "#[cfg(not(test))]",
        "#[cfg(any(target_os = \"macos\", test))]",
        "#[cfg(all(not(test), unix))]",
        "#[cfg(all(feature = \"test\", unix))]",
        "#[cfg(unix)]",
        "#[path = \"tests.rs\"]",
    ] {
        assert!(!cfg_requires_test(attr), "{attr} compiles into production");
    }

    assert_eq!(out_of_line_mod("mod tests;"), Some("tests"));
    assert_eq!(
        out_of_line_mod("pub(crate) mod test_support;"),
        Some("test_support")
    );
    assert_eq!(out_of_line_mod("pub mod env;"), Some("env"));
    assert_eq!(out_of_line_mod("mod tests {"), None);
    assert_eq!(out_of_line_mod("let module;"), None);
}

/// The resolver against the real tree, one fixture per rule it applies: a
/// `cfg(all(test, unix))` gate, a sibling `#[path]`, a `#[path]` climbing out
/// of `src/bin`, a non-`mod.rs` parent owning a stem directory, and the
/// transitive closure into a test-only file's own modules. If any of these
/// moved, the fixture is what to update — not the rule.
#[test]
fn test_only_module_files_resolve_by_the_compilers_rules() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    let test_only = test_only_files(&root, &files);
    let rel: BTreeSet<String> = test_only
        .iter()
        .map(|f| {
            f.strip_prefix(&root)
                .unwrap_or(f)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    for expected in [
        "lappi/tests.rs",
        "tasks/file_tasks_tests.rs",
        "workbench/intake_merge_tests.rs",
        "storage/hygiene/global/tests.rs",
        "test_support.rs",
        "test_support/env.rs",
    ] {
        assert!(
            rel.contains(expected),
            "{expected} is test-only; got {rel:?}"
        );
    }
    for production in [
        "lappi/mod.rs",
        "lib.rs",
        "bin/gitpulsed.rs",
        "engine/git_cli.rs",
    ] {
        assert!(!rel.contains(production), "{production} is production code");
    }
}

/// A file reached through a test gate *and* a plain `mod` compiles into
/// production; exempting it would hide its spawns. Built in a scratch tree
/// because the real one has no such file, which is what makes the case easy
/// to break unnoticed.
#[test]
#[should_panic(expected = "compiles into production")]
fn a_file_production_also_declares_is_not_exempted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).expect("src dir");
    std::fs::write(
        src.join("lib.rs"),
        "#[cfg(test)]\n#[path = \"shared.rs\"]\nmod probe;\nmod shared;\n",
    )
    .expect("lib.rs");
    std::fs::write(src.join("shared.rs"), "pub fn f() {}\n").expect("shared.rs");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    test_only_files(&src, &files);
}
