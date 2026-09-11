use crate::model::*;

pub fn is_test_path(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    let norm_lower = norm.to_lowercase();
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    let name_lower = name.to_lowercase();

    let parts: Vec<&str> = norm_lower.split('/').collect();
    let mut in_test_dir = parts.iter().rev().skip(1).any(|p| {
        *p == "tests" || *p == "test" || *p == "__tests__" || *p == "spec" || *p == "androidtest"
    });

    // The Android / JVM layout, which contributes to the *directory* verdict
    // and does not stand in for it. This used to `return true` outright, ahead
    // of the `!name.starts_with('.')` guard below, so the rule the kernel
    // published held for `tests/.eslintrc` and not for `app/src/test/.eslintrc`
    // — the same file, one directory layout apart. Every dotfile a JVM project
    // keeps beside its tests was annotated `TestFile`, which exempts the whole
    // file from liveness.
    //
    // Kept as a branch rather than deleted even though the segment table above
    // already answers `test` and `androidtest`: it mirrors
    // `devcouncil.indexing.wiring.is_test_path`, which the parity module pins,
    // and it is what keeps the JVM layout working if that table ever narrows.
    if norm_lower.contains("/src/test/") || norm_lower.contains("/src/androidtest/") {
        in_test_dir = true;
    }

    let looks_like_test = name_lower.starts_with("test_")
        || name_lower == "conftest.py"
        || name_lower.ends_with("_test.py")
        || name_lower.ends_with("_test.go")
        || name_lower.ends_with(".test.js")
        || name_lower.ends_with(".test.ts")
        || name_lower.ends_with(".test.jsx")
        || name_lower.ends_with(".test.tsx")
        || name_lower.ends_with(".spec.js")
        || name_lower.ends_with(".spec.ts")
        || name_lower.ends_with(".spec.jsx")
        || name_lower.ends_with(".spec.tsx")
        || name_lower.ends_with("_spec.rb")
        || name.ends_with("Tests.swift")
        || name.ends_with("Test.kt")
        || name.ends_with("Tests.kt");

    looks_like_test || (in_test_dir && !name.starts_with('.'))
}

/// Suffixes a minifier writes, and a human does not.
///
/// `.mjs` and `.cjs` are here because the same bundlers emit them and the same
/// grammar parses them; `src/mining.js` and `src/minify.js` are not, which is
/// why the match is on the compound suffix rather than on the substring
/// `min`.
const MINIFIED_SUFFIXES: &[&str] = &[".min.js", ".min.mjs", ".min.cjs", ".min.css"];

/// Whether `path` is minifier output — a bundle, not source someone reads.
///
/// Split out of [`is_vendored_path`] because two different decisions ask it and
/// only one of them is about vendoring. `is_vendored_path` asks it to hang a
/// [`WiringKind::Vendored`] annotation, which exempts a file from *liveness*;
/// [`crate::treesitter::extract_treesitter_with_budget`] asks it to decline the
/// parse outright. Keeping one owner is the point: while the shape lived only
/// inside `is_vendored_path`, the extractor had no way to consult it, and a
/// 177 KB bundle was handed to tree-sitter on every build — see
/// `tests/a_minified_bundle_is_not_parsed.rs` for what that cost.
///
/// Deliberately narrower than `is_vendored_path`. That predicate's directory
/// arms — `vendor/`, `vendored/`, `third_party/` — hold ordinary readable
/// source that parses fine and yields real symbols, and refusing to parse all
/// of it would be a far larger change than the defect calls for. Only the
/// name-declared minified forms are declined.
pub fn is_minified_bundle(path: &str) -> bool {
    let norm = path.replace('\\', "/").to_lowercase();
    MINIFIED_SUFFIXES
        .iter()
        .any(|suffix| norm.ends_with(suffix))
}

pub fn is_vendored_path(path: &str) -> bool {
    let norm = path.replace('\\', "/").to_lowercase();
    let parts: Vec<&str> = norm.split('/').collect();
    if parts
        .iter()
        .any(|p| *p == "vendor" || *p == "vendored" || *p == "node_modules" || *p == "third_party")
    {
        return true;
    }
    // Tauri apps vendor tao/wry/webkit under `src-tauri/framework/`. A bare
    // `framework` segment would swallow `src/framework/app.ts`.
    if norm.starts_with("src-tauri/framework/") || norm.contains("/src-tauri/framework/") {
        return true;
    }
    is_minified_bundle(&norm)
}

/// Whether `path`'s basename is one a well-known code generator writes.
///
/// Each arm names a generator and the exact file it emits, because the
/// annotation this feeds — `WiringKind::GeneratedFile` — exempts every symbol
/// in the file from the dead-code answer. A suffix that is merely *suggestive*
/// of generation clears real findings.
///
/// Two arms were wrong in opposite directions, and
/// `devcouncil.indexing.wiring._GENERATED_PATH_RE` is the spec for both:
///
/// * grpcio-tools writes `*_pb2_grpc.pyi` beside `*_pb2_grpc.py` and only the
///   second was here, so the type stub half of every gRPC service was reported
///   on as hand-written code.
/// * `zz_generated` is kubebuilder's convention and kubebuilder writes Go. A
///   bare `starts_with` claimed `zz_generated.txt` and `zz_generated_notes.md`
///   as generated too — prose, exempted on a prefix.
///
/// 42 of the 651 paths in Lane W's differential diverged from the Python rule
/// across these two arms.
pub fn is_generated_path(path: &str) -> bool {
    let norm = path.replace('\\', "/").to_lowercase();
    let filename = norm.rsplit('/').next().unwrap_or(&norm);
    filename.ends_with("_pb2.py")
        || filename.ends_with("_pb2.pyi")
        || filename.ends_with("_pb2_grpc.py")
        || filename.ends_with("_pb2_grpc.pyi")
        || filename.ends_with(".pb.go")
        || filename.ends_with(".pb.gw.go")
        || filename.ends_with("_pb.js")
        || filename.ends_with("_pb.ts")
        || filename.ends_with("_pb.d.ts")
        || (filename.starts_with("zz_generated") && filename.ends_with(".go"))
}

pub fn source_has_generated_header(source: &str) -> bool {
    let markers = [
        "do not edit",
        "code generated",
        "@generated",
        "auto-generated",
        "autogenerated",
        "automatically generated",
        "generated by",
    ];
    let prefixes = ["#", "//", "/*", "*", "--", "<!--", ";", "'"];

    for line in source.lines().take(5) {
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        if !prefixes.iter().any(|p| stripped.starts_with(p)) {
            return false;
        }
        let lower = stripped.to_lowercase();
        if markers.iter().any(|m| lower.contains(m)) {
            return true;
        }
    }
    false
}

/// Whether a decorator line is framework registration rather than ordinary
/// Python plumbing.
///
/// A dotted hint (`app.`, `pytest.`, …) matches as a **prefix** of the
/// decorator's dotted base; a bare hint (`route`, `task`, `register`, …)
/// matches a **whole identifier segment**. Neither matches a bare substring.
///
/// It used to be `lower.contains(h)` over the raw line, which made
/// `@multitask` a `task`, `@preregister` a `register` and `@FastAPI_thing` a
/// `fastapi`. That is not a cosmetic over-match: the annotation this feeds is
/// `WiringKind::FrameworkDecorator` targeting the *file*
/// (`extract_wiring_annotations`), and `is_file_exempt`
/// (`devmap-analyze/src/liveness.rs`) exempts every symbol in a file carrying
/// one — so a single such line hid a whole file from the dead-code scan.
///
/// `devcouncil.indexing.wiring.is_wiring_decorated` is the spec, and this is
/// its rule transcribed: split off everything from the first `(`, strip the
/// leading `@`, trim, lowercase, then split on `.` and whitespace for the
/// segment set. The hint table itself is unchanged and is pinned equal to the
/// Python one by `tests/unit/test_wiring_parity_with_kernel.py`.
pub fn is_wiring_decorator(decorator: &str) -> bool {
    let hints = [
        "app.",
        "router.",
        "typer.",
        "click.",
        "pytest.",
        "celery.",
        "flask",
        "fastapi",
        "command",
        "route",
        "task",
        "fixture",
        "register",
        "hookimpl",
        "hookable",
        "receiver",
        "api_view",
        "action",
        "subscriber",
        "listener",
        "on_event",
        "dramatiq.",
        "huey.",
    ];
    let base = decorator
        .split('(')
        .next()
        .unwrap_or(decorator)
        .trim_start_matches('@')
        .trim()
        .to_lowercase();
    if base.is_empty() {
        return false;
    }
    let segments: Vec<&str> = base
        .split(|c: char| c == '.' || c.is_whitespace())
        .filter(|segment| !segment.is_empty())
        .collect();
    hints.iter().any(|hint| {
        if hint.ends_with('.') {
            base.starts_with(hint)
        } else {
            segments.contains(hint)
        }
    })
}

// ---------------------------------------------------------------------------
// Per-symbol entry-point rules.
//
// These answer "does something outside the observable call graph invoke this
// one symbol?", which is a strictly narrower question than the file-level rules
// above. They are deliberately keyed on names and attributes that the language
// or a framework reserves, so a rule can never quietly exempt an ordinary
// function that merely lives near an entry point.
// ---------------------------------------------------------------------------

/// Rust attribute that hands a function to a runner, the linker, or the
/// compiler rather than to a caller.
///
/// Matched on the attribute's last path segment so `#[test]`, `#[tokio::test]`
/// and `#[actix_rt::test]` are one rule instead of an ever-growing list of
/// crate prefixes.
pub fn rust_attribute_entry_reason(attribute_path: &str) -> Option<&'static str> {
    let last = attribute_path
        .rsplit("::")
        .next()
        .unwrap_or(attribute_path)
        .trim();
    Some(match last {
        "test" | "bench" | "rstest" | "test_case" | "should_panic" | "quickcheck" | "proptest" => {
            "test harness invokes it"
        }
        "main" => "async runtime entry point",
        "ctor" | "dtor" => "static initializer run before/after main",
        "no_mangle" | "export_name" => "linker-exported symbol",
        "proc_macro" | "proc_macro_derive" | "proc_macro_attribute" => {
            "procedural macro invoked by the compiler"
        }
        "global_allocator" | "panic_handler" | "alloc_error_handler" | "lang" => {
            "language item invoked by the runtime"
        }
        "wasm_bindgen" => "exported to the JavaScript host",
        _ => return None,
    })
}

/// Why Cargo compiles `path` as a target root, if it does.
///
/// The single owner of Rust's layout conventions. A target root is a file the
/// toolchain compiles directly: nothing in the source imports it, nothing
/// should, and `mod` declarations run *outward* from it. Measured on this
/// repository before this rule existed: seventeen Rust files were reported as
/// unwired candidates and eleven of them were target roots — five `src/lib.rs`
/// crate roots, five `examples/*.rs`, one `build.rs` — each one a delete-this
/// suggestion for a file Cargo names in its own manifest.
///
/// `rust_path_declares_main` answers the narrower symbol-level question and
/// delegates here, so the two cannot disagree about what `examples/` means.
/// Why a JS/TS/HTML file is a toolchain root, if it is.
///
/// Python `structural_exemptions` already treated these as wired-by-convention
/// so they would not be proposed for deletion. The kernel's `unwired_candidates`
/// never asked that question, so a Vite config, a `src/main.ts` HTML entry,
/// a `scripts/` CLI, and an ambient `.d.ts` were reported as stranded modules
/// even though nothing in the source is supposed to import them.
///
/// A `TargetRoot`, not a `ScriptEntry`: the file is reachable and its unused
/// helpers are still dead, the same split `rust_target_root_reason` already
/// owns for Cargo.
///
/// Two arms this function used to own have moved out, and neither is a
/// behaviour change to *this* predicate's callers so much as a correction to
/// what the annotation says. `*.d.ts` is [`WiringKind::AmbientDeclaration`]
/// now, because a declaration file is not a place execution starts and
/// `is_entry_root` is read by `subsystem_map.is_entry_root` as exactly that
/// claim; `*.config.*` is [`WiringKind::ToolConfig`], which stays an entry
/// root and joins the rest of the by-convention table in
/// [`tool_config_reason`] rather than being the one member of it that lives
/// somewhere else.
pub fn js_target_root_reason(path: &str) -> Option<&'static str> {
    let norm = path.replace('\\', "/");
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    if name.contains(".stories.")
        || name.ends_with(".stories.ts")
        || name.ends_with(".stories.tsx")
        || name.ends_with(".stories.js")
        || name.ends_with(".stories.jsx")
    {
        return Some("Component storybook file");
    }
    let suffix = file_suffix(&norm).to_ascii_lowercase();
    let parents: Vec<&str> = norm.split('/').rev().skip(1).collect();
    if parents
        .iter()
        .any(|dir| matches!(*dir, "scripts" | "bin" | "benchmarks"))
    {
        return Some("CLI / script directory");
    }
    // Mirrors `_JS_MAIN_SEED_RE`: `src/main.ts`, `src/index.tsx`, a top-level
    // `App.tsx`, and a service `index` module. These are the files an HTML
    // `<script type="module">` or a bundler names, and they have no importer.
    let stem_path = norm.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(&norm);
    if matches!(suffix.as_str(), "ts" | "tsx" | "js" | "jsx" | "mjs")
        && (stem_path.ends_with("/src/main")
            || stem_path.ends_with("/src/index")
            || stem_path == "src/main"
            || stem_path == "src/index"
            || name == "App.ts"
            || name == "App.tsx"
            || name == "App.js"
            || name == "App.jsx"
            || name == "App.mjs"
            || stem_path.ends_with("/server/index")
            || stem_path.ends_with("/api/index")
            || stem_path.ends_with("/backend/index")
            || stem_path.ends_with("/worker/index")
            || stem_path.ends_with("/functions/index")
            || stem_path.ends_with("/lambda/index"))
    {
        return Some("JavaScript/TypeScript application entry");
    }
    None
}

// ---------------------------------------------------------------------------
// File-level rules for code nothing imports and nothing should.
//
// Each answers "who reaches this file, if not an importer". They feed
// `FileLiveness::Exempt` through an annotation, so a rule here removes a path
// from every file-level verdict at once — `unwired_candidates`,
// `unreachable_files`, and the dead-cluster file roll-up. That is the reason
// each one is keyed on something a toolchain reserves rather than on a
// suggestive substring.
// ---------------------------------------------------------------------------

/// Whether the file opens with a `#!` interpreter line.
///
/// The most reliable "somebody runs this" signal there is, and the kernel had
/// no rule for it at all: measured, 10+ shell scripts in this repository and
/// 40+ in another local tree were reported as stranded modules. Universal
/// across languages, because the kernel is what the operating system reads and
/// it does not care what the extension says.
///
/// A UTF-8 BOM is skipped — editors write one and the two bytes in front of
/// `#!` would otherwise hide the line — and a CRLF file is unaffected, because
/// the marker is a prefix and the `\r` sits at the other end. The line number
/// is not negotiable: `#!` on line 2 is a comment, and `exec(2)` agrees.
pub fn has_shebang(source: &str) -> bool {
    let first = source.split('\n').next().unwrap_or("");
    first
        .strip_prefix('\u{feff}')
        .unwrap_or(first)
        .starts_with("#!")
}

/// Directory names that hold test data rather than program text.
///
/// Kept apart from [`is_test_path`], which is pinned equal to
/// `devcouncil.indexing.wiring.is_test_path` by a parity test — widening that
/// one would break the parity rather than fix the finding.
const FIXTURE_DIR_SEGMENTS: &[&str] = &[
    "testdata",
    "test-data",
    "test_data",
    "fixtures",
    "__fixtures__",
    "__snapshots__",
    "golden",
    "examples",
    "example",
];

/// Whether `path` sits inside a fixture, snapshot or example tree.
///
/// Matched on **directory segments only**. A file *named* `testdata` — a Go
/// project checking one in at the root is ordinary — is program text sitting
/// beside the fixtures, not a fixture, and the same trap
/// `a_file_named_like_the_jvm_test_directory_is_not_a_test_path` already
/// documents for `src/test`.
///
/// Case-insensitive, because `Fixtures/` and `TestData/` are what a JVM or
/// .NET tree calls the same directory.
pub fn is_fixture_path(path: &str) -> bool {
    let norm = path.replace('\\', "/").to_lowercase();
    norm.split('/')
        .rev()
        .skip(1)
        .any(|segment| FIXTURE_DIR_SEGMENTS.contains(&segment))
}

/// Basenames a toolchain finds by convention, and the tool that finds them.
///
/// Exact names, matched case-sensitively where the ecosystem writes them
/// case-sensitively. The reason string names the tool because it is what a
/// reader checks the exemption against: "Bundler / test-runner config" is
/// auditable, "tool config" is not.
const TOOL_CONFIG_BASENAMES: &[(&str, &str)] = &[
    ("build.gradle", "Gradle build script"),
    ("build.gradle.kts", "Gradle build script"),
    ("settings.gradle", "Gradle settings script"),
    ("settings.gradle.kts", "Gradle settings script"),
    ("Package.swift", "Swift package manifest"),
    ("Podfile", "CocoaPods manifest"),
    ("Fastfile", "fastlane lane definitions"),
    ("Gemfile", "Bundler manifest"),
    ("Rakefile", "Rake task definitions"),
    ("Brewfile", "Homebrew bundle manifest"),
    ("Vagrantfile", "Vagrant machine definition"),
    ("Jenkinsfile", "Jenkins pipeline definition"),
    ("Snakefile", "Snakemake workflow"),
    ("setup.py", "setuptools build script"),
    ("noxfile.py", "nox session definitions"),
    ("fabfile.py", "Fabric task definitions"),
    ("manage.py", "Django management entry point"),
    (
        "wsgi.py",
        "WSGI application object a server imports by path",
    ),
    (
        "asgi.py",
        "ASGI application object a server imports by path",
    ),
    ("gunicorn.conf.py", "gunicorn configuration"),
    ("locustfile.py", "Locust load-test definitions"),
];

/// Basenames a tool finds by convention only inside one directory.
///
/// `conf.py` is Sphinx's *and* an ordinary module name, and `env.py` is
/// Alembic's *and* the name half the world gives a settings module. Neither is
/// safe as a bare basename: the exemption would clear a real finding every
/// time somebody wrote `app/conf.py`. Keyed on `(parent directory, basename)`
/// so the claim is as narrow as the convention is.
const TOOL_CONFIG_IN_DIRECTORY: &[(&str, &str, &str)] = &[
    ("docs", "conf.py", "Sphinx configuration"),
    ("alembic", "env.py", "Alembic migration environment"),
    ("migrations", "env.py", "Alembic migration environment"),
];

/// Why a toolchain reads `path` because of what it is called, or `None`.
///
/// The measured false-positive class this exists for: `vite.config.ts`,
/// `vitest.setup.ts`, `playwright.config.ts`, `postcss.config.js`,
/// `tailwind.config.js`, `eslint.config.js`, `build.gradle.kts`,
/// `Package.swift`, `noxfile.py`, `docs/conf.py`. Nothing in a repository
/// imports one, nothing should, and every one of them was a delete-this
/// suggestion.
///
/// A [`WiringKind::ToolConfig`], which `is_entry_root` treats as an entry
/// root — which is what it is. It does not exempt the file's *symbols*: a
/// helper nobody calls inside a `noxfile.py` is as dead as one anywhere else,
/// the same split [`rust_target_root_reason`] already owns.
pub fn tool_config_reason(path: &str) -> Option<&'static str> {
    let norm = path.replace('\\', "/");
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    if let Some((_, reason)) = TOOL_CONFIG_BASENAMES
        .iter()
        .find(|(basename, _)| *basename == name)
    {
        return Some(reason);
    }
    let parent = norm.rsplit('/').nth(1).unwrap_or("");
    if let Some((_, _, reason)) = TOOL_CONFIG_IN_DIRECTORY
        .iter()
        .find(|(directory, basename, _)| *directory == parent && *basename == name)
    {
        return Some(reason);
    }
    // `<anything>.config.<ext>` / `.conf.` / `.setup.`, which is how the
    // JS ecosystem spells the same convention. Anchored on the *compound*
    // suffix rather than on a substring, so `src/configure.ts` and
    // `src/setupNetwork.ts` stay ordinary code.
    let suffix = file_suffix(&norm).to_ascii_lowercase();
    let is_js_suffix = matches!(
        suffix.as_str(),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts"
    );
    if is_js_suffix {
        let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
        if let Some(marker) = stem.rsplit_once('.').map(|(_, marker)| marker) {
            match marker {
                "config" => return Some("Bundler / test-runner config"),
                "conf" => return Some("Tool configuration read by name"),
                "setup" => return Some("Test-runner setup file loaded by the harness"),
                _ => {}
            }
        }
        // Jest and CRA's spelling, which has no dot before the marker.
        if stem == "setupTests" || stem == "setupTest" {
            return Some("Test-runner setup file loaded by the harness");
        }
    }
    None
}

/// Whether `path` is a TypeScript ambient declaration.
///
/// `foo.d.ts`, and not a directory called `d.ts` nor a file called `d.ts`
/// itself — the marker is the compound suffix on a name that has a stem.
pub fn is_ambient_declaration(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    name.len() > ".d.ts".len() && name.ends_with(".d.ts")
}

/// Barrel-module basenames: a file whose job is to re-export its directory.
const BARREL_BASENAMES: &[&str] = &[
    "index.ts",
    "index.tsx",
    "index.js",
    "index.jsx",
    "index.mjs",
    "index.cjs",
    "index.mts",
    "index.cts",
];

/// Why `path` is a package marker rather than a module somebody imports, or
/// `None`.
///
/// `__init__.py` is the measured case: 35 of them were unwired candidates here,
/// because [`looks_like_reexport_init`] clears only the *re-export-only* ones
/// and an empty or docstring-only marker is the commoner shape. Every
/// `import pkg.sub` in the tree runs the package's `__init__.py`, and not one
/// of them names it.
///
/// A barrel `index.ts` earns the same verdict only when it really is one:
/// every meaningful line re-exports, and at least one does. A file that
/// happens to be called `index.ts` and holds real code is ordinary product
/// code, and clearing it would hide a stranded module behind a filename.
pub fn package_marker_reason(path: &str, source: &str) -> Option<&'static str> {
    let norm = path.replace('\\', "/");
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    if name == "__init__.py" {
        return Some("Python package marker: every submodule import runs it, and none names it");
    }
    if name == "package-info.java" {
        return Some("Java package declaration file read by the compiler");
    }
    if BARREL_BASENAMES.contains(&name) && looks_like_reexport_barrel(source) {
        return Some("Re-export barrel: it names its directory's modules and nothing names it");
    }
    None
}

/// Whether a JS/TS module's whole body is re-exports.
///
/// Line-based, like [`looks_like_reexport_init`], and refuses on anything it
/// does not recognise — a multi-line `export { … } from` falls through to
/// "not a barrel". That is the fail-open direction for this rule: the file
/// stays a candidate, so a reader sees one finding too many rather than one
/// too few.
fn looks_like_reexport_barrel(source: &str) -> bool {
    let mut has_reexport = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
        {
            continue;
        }
        let is_reexport = (trimmed.starts_with("export ") || trimmed.starts_with("export{"))
            && trimmed.contains(" from ")
            && trimmed.ends_with(';');
        if is_reexport {
            has_reexport = true;
            continue;
        }
        if trimmed.starts_with("import ") {
            continue;
        }
        return false;
    }
    has_reexport
}

pub fn rust_target_root_reason(path: &str) -> Option<&'static str> {
    let norm = path.replace('\\', "/");
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    if name == "build.rs" {
        return Some("Cargo build script");
    }
    let parent = norm.rsplit('/').nth(1);
    match name {
        // A crate root only at a crate root's own place. A `lib.rs` nested
        // inside a module directory is an ordinary module, and calling it a
        // target root would exempt a file that really can be stranded.
        "main.rs" if parent == Some("src") || parent.is_none() => {
            return Some("Cargo binary crate root")
        }
        "lib.rs" if parent == Some("src") || parent.is_none() => {
            return Some("Cargo library crate root")
        }
        _ => {}
    }
    let dirs: Vec<&str> = norm.split('/').rev().skip(1).collect();
    if dirs.contains(&"bin") {
        return Some("Cargo binary target");
    }
    if dirs.contains(&"examples") {
        return Some("Cargo example target");
    }
    if dirs.contains(&"benches") {
        return Some("Cargo benchmark target");
    }
    None
}

/// Whether a Rust `fn main` at file scope in `path` is a binary entry point.
///
/// A *different* question from `rust_target_root_reason`, and delegating to it
/// was wrong in a way the existing tests caught immediately: `src/lib.rs` is a
/// target root and has no `fn main` at all, so a `main` written in a library
/// file is an ordinary function and a real dead-code candidate. The two rules
/// overlap on the binary shapes and part company on the library one.
pub fn rust_path_declares_main(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    if name == "main.rs" {
        return true;
    }
    // A build script's `fn main` is invoked by Cargo before the crate compiles
    // and has no call site anywhere in the corpus — the same claim `bin/` makes.
    if name == "build.rs" {
        return true;
    }
    let dirs: Vec<&str> = norm.split('/').rev().skip(1).collect();
    dirs.iter()
        .any(|dir| matches!(*dir, "bin" | "examples" | "benches" | "tests"))
}

/// Go declarations the runtime or toolchain calls with no source-level caller.
///
/// `init` is unreferenceable by the Go spec, so it can never have a call site.
/// `main` only qualifies in `package main`; in any other package it is an
/// ordinary unexported function and a real dead-code candidate.
pub fn go_runtime_entry_reason(
    name: &str,
    package: Option<&str>,
    has_receiver: bool,
) -> Option<&'static str> {
    if has_receiver {
        return None;
    }
    match name {
        "init" => Some("Go runtime calls init() before main; it cannot be referenced"),
        "main" if package == Some("main") => Some("program entry point of package main"),
        _ => None,
    }
}

/// Metal shader entry points, which the host dispatches by name.
///
/// A `kernel`, `vertex` or `fragment` function is the GPU-side half of a call
/// whose caller is CPU code in another language — `newFunctionWithName:` in
/// Swift or Objective-C, or a name string in a pipeline descriptor. No call
/// site exists in any `.metal` file, so it is exactly the situation
/// `RuntimeEntryPoint` already describes for Go's `init`, Rust's `#[no_mangle]`
/// and a React lifecycle hook: reachable, with the evidence outside the corpus.
///
/// This is the only thing that separates a shader entry point from a shader's
/// private helper. The generic C-family extractor marks every declaration
/// exported — it has no visibility keyword to read — so without this annotation
/// both come back under the same blanket "Exported or exempt", and the entry
/// point's reachability is an assumption rather than a recorded fact.
///
/// Restricted to the qualifier actually written on the declaration, so an
/// ordinary helper is never claimed as an entry point.
pub fn metal_shader_entry_reason(qualifier: &str) -> Option<&'static str> {
    Some(match qualifier {
        "kernel" => "compute shader dispatched by the host by name",
        "vertex" => "vertex shader bound to a render pipeline by the host",
        "fragment" => "fragment shader bound to a render pipeline by the host",
        _ => return None,
    })
}

/// Swift attributes that hand a declaration to a runtime.
///
/// Each names a caller outside the corpus. `@main` is the entry point the
/// compiler synthesises `main` from; the `@objc` family publishes a selector
/// that Interface Builder, a target/action pair, KVO or a delegate protocol
/// invokes by string; `@_cdecl` and `@_silgen_name` publish a C symbol; the
/// swift-testing and XCTest attributes are collected by a test runner.
///
/// Restricted to attributes actually written on the declaration, so an ordinary
/// helper is never claimed. This exists because Swift visibility is now read:
/// before it every Swift symbol reported `is_exported` and no exemption could
/// have mattered.
pub fn swift_attribute_entry_reason(attribute: &str) -> Option<&'static str> {
    Some(match attribute {
        "main" | "UIApplicationMain" | "NSApplicationMain" => {
            "application entry point synthesised by the compiler"
        }
        "objc" | "objcMembers" | "IBAction" | "IBOutlet" | "IBSegueAction" | "IBInspectable"
        | "IBDesignable" | "NSManaged" => {
            "published to the Objective-C runtime and invoked by selector"
        }
        "_cdecl" | "_silgen_name" | "_expose" | "_alwaysEmitIntoClient" => {
            "published under a C symbol name and called from outside Swift"
        }
        "Test" | "Suite" => "collected by the swift-testing runner",
        _ => return None,
    })
}

/// Swift supertypes whose conformance makes the *type* a runtime entry point.
///
/// A `struct … : App` is instantiated by SwiftUI, a `UIApplicationDelegate` by
/// UIKit, an `XCTestCase` by the test runner. None of them is constructed by any
/// call in the corpus, so without this the type itself looks unused the moment
/// its visibility is read.
///
/// Deliberately short. It lists only supertypes whose *whole purpose* is to be
/// instantiated by a framework — conforming to `Equatable` or `Codable` says
/// nothing about who constructs the type, and listing those would exempt most of
/// an application on no evidence.
pub fn swift_runtime_supertype_reason(supertype: &str) -> Option<&'static str> {
    Some(match supertype {
        "App" | "Scene" | "WidgetBundle" | "Widget" | "Commands" => {
            "SwiftUI instantiates the conforming type to run the application"
        }
        "UIApplicationDelegate"
        | "NSApplicationDelegate"
        | "UISceneDelegate"
        | "UIWindowSceneDelegate"
        | "UNUserNotificationCenterDelegate" => {
            "the application lifecycle instantiates the delegate by class name"
        }
        "XCTestCase" => "the test runner instantiates the case by reflection",
        _ => return None,
    })
}

/// Kotlin annotations that hand a declaration to a runtime.
///
/// Compose calls a `@Composable` from a composition it owns, JUnit collects
/// `@Test` by reflection, and every dependency-injection and
/// serialization-plugin annotation names a caller in generated or framework code
/// that this corpus does not contain.
pub fn kotlin_annotation_entry_reason(annotation: &str) -> Option<&'static str> {
    Some(match annotation {
        "Composable" | "Preview" => "invoked by the Jetpack Compose runtime",
        "Test" | "Before" | "After" | "BeforeEach" | "AfterEach" | "BeforeClass" | "AfterClass"
        | "BeforeAll" | "AfterAll" | "ParameterizedTest" | "RepeatedTest" => {
            "collected and invoked by the test runner"
        }
        "Inject" | "Provides" | "Binds" | "Module" | "Component" | "HiltAndroidApp"
        | "AndroidEntryPoint" | "HiltViewModel" | "Singleton" | "Factory" | "Assisted"
        | "AssistedInject" => "constructed by the dependency-injection container",
        "Serializable" | "Entity" | "Dao" | "Database" | "TypeConverter" | "JsonClass" => {
            "invoked by generated serialization or persistence code"
        }
        "JavascriptInterface" | "Keep" | "JvmStatic" | "JvmName" | "NativeMethod" => {
            "published to a runtime that resolves it by name"
        }
        "SubscribeEvent" | "Subscribe" | "EventHandler" => "invoked by an event bus by reflection",
        _ => return None,
    })
}

/// Python functions a test runner or plugin system collects by name.
pub fn python_harness_entry_reason(name: &str) -> Option<&'static str> {
    if name.starts_with("test_") {
        return Some("pytest/unittest collects test_* by name");
    }
    if name.starts_with("pytest_") {
        return Some("pytest plugin hook called by name");
    }
    Some(match name {
        "setup_module" | "teardown_module" | "setup_function" | "teardown_function"
        | "setup_class" | "teardown_class" | "setup_method" | "teardown_method" => {
            "pytest xunit-style fixture hook"
        }
        "setUp" | "tearDown" | "setUpClass" | "tearDownClass" | "setUpModule"
        | "tearDownModule" | "runTest" => "unittest lifecycle hook",
        _ => return None,
    })
}

/// Class methods a JS/TS framework calls on the instance's behalf.
///
/// Restricted to class methods on purpose: `render` and `mounted` are ordinary
/// names for a free function, and exempting those would hide real dead code.
pub fn js_lifecycle_hook_reason(name: &str) -> Option<&'static str> {
    Some(match name {
        "constructor" => "invoked by `new`",
        "render"
        | "componentDidMount"
        | "componentWillMount"
        | "componentWillUnmount"
        | "componentDidUpdate"
        | "componentWillUpdate"
        | "componentWillReceiveProps"
        | "shouldComponentUpdate"
        | "getSnapshotBeforeUpdate"
        | "componentDidCatch"
        | "getDerivedStateFromProps"
        | "getDerivedStateFromError" => "React lifecycle hook called by the renderer",
        "ngOnInit"
        | "ngOnDestroy"
        | "ngOnChanges"
        | "ngDoCheck"
        | "ngAfterContentInit"
        | "ngAfterContentChecked"
        | "ngAfterViewInit"
        | "ngAfterViewChecked" => "Angular lifecycle hook called by the framework",
        "connectedCallback"
        | "disconnectedCallback"
        | "adoptedCallback"
        | "attributeChangedCallback" => "custom-element lifecycle hook called by the DOM",
        _ => return None,
    })
}

/// Object methods a bundler invokes by name on a plugin it constructed.
///
/// Restricted to `method_definition` at the call site (same gate as
/// [`js_lifecycle_hook_reason`]): `generateBundle` as a free function is an
/// ordinary name. Vite/Rollup look these up on the plugin object, so they
/// have no in-repo caller and otherwise become extracted-dead.
pub fn js_bundler_plugin_hook_reason(name: &str) -> Option<&'static str> {
    Some(match name {
        // Distinctive Rollup/Vite names only. `transform`, `load`, `options`,
        // and `config` are ordinary methods on application objects.
        "generateBundle" | "writeBundle" | "closeBundle" | "renderChunk" => {
            "Rollup plugin hook invoked by the bundler"
        }
        "configureServer"
        | "configurePreviewServer"
        | "transformIndexHtml"
        | "handleHotUpdate"
        | "hotUpdate" => "Vite plugin hook invoked by the bundler",
        _ => return None,
    })
}

fn looks_like_reexport_init(path: &str, source: &str) -> bool {
    let p = path.replace('\\', "/");
    let name = p.rsplit('/').next().unwrap_or(path);
    if name != "__init__.py" {
        return false;
    }
    let mut has_from_import = false;
    for line in source.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with("from .") || t.starts_with("from . ") {
            has_from_import = true;
            continue;
        }
        if t.starts_with("import ") {
            continue;
        }
        // Non-reexport body → not re-export-only
        if !t.starts_with("__all__") {
            return false;
        }
    }
    has_from_import
}

fn is_launcher_file(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let name = p.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Dockerfile"
            | "dockerfile"
            | "Makefile"
            | "makefile"
            | "Procfile"
            | "Justfile"
            | "justfile"
    )
}

/// One parse of a manifest, shared by the file-level claim
/// ([`config_script_entry`]) and the symbol-level one
/// ([`config_entry_point_symbols`]). Until 2026-09-07 both were substring
/// tests over the text — `[project.scripts]` at the start of any line, `"bin"`
/// anywhere in a `package.json` — so a header quoted in a description declared
/// an entry point and a dependency named `bin` declared a script.
///
/// A manifest the parser cannot read makes no claim. That is the fail-open
/// direction for these rules: an exemption not granted is one more finding a
/// reader sees, never a finding hidden.
fn parse_toml(source: &str) -> Option<toml::Table> {
    source.parse::<toml::Table>().ok()
}

/// Does this manifest declare a script or binary target? A claim about the
/// file ([`WiringKind::ScriptEntry`]); the symbols it names are
/// [`config_entry_point_symbols`]'s.
fn config_script_entry(path: &str, source: &str) -> bool {
    let p = path.replace('\\', "/");
    let name = p.rsplit('/').next().unwrap_or(path);
    match name {
        // `[[bin]]` (an array of tables) or a bare `[bin]` table.
        "Cargo.toml" => parse_toml(source).is_some_and(|manifest| {
            manifest
                .get("bin")
                .is_some_and(|bin| bin.is_array() || bin.is_table())
        }),
        "pyproject.toml" => {
            parse_toml(source).is_some_and(|manifest| !entry_point_tables(&manifest).is_empty())
        }
        // Top-level keys only: a dependency named `bin` is not a script.
        "package.json" => serde_json::from_str::<serde_json::Value>(source)
            .ok()
            .and_then(|manifest| {
                manifest.as_object().map(|object| {
                    ["bin", "main", "scripts"]
                        .iter()
                        .any(|key| object.contains_key(*key))
                })
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// The tables whose keys are `name = "module:attr"` entry points, in a fixed
/// order: `project.scripts`, `project.gui-scripts`, every group under
/// `project.entry-points` (each group is its own table — `console_scripts`,
/// `gui_scripts`, and every plugin group a package publishes), then Poetry's
/// `tool.poetry.scripts`. A declared-but-empty table still counts as a
/// declaration for the file-level claim.
fn entry_point_tables(manifest: &toml::Table) -> Vec<&toml::Table> {
    let mut tables = Vec::new();
    if let Some(project) = manifest.get("project").and_then(toml::Value::as_table) {
        for key in ["scripts", "gui-scripts"] {
            if let Some(table) = project.get(key).and_then(toml::Value::as_table) {
                tables.push(table);
            }
        }
        if let Some(groups) = project.get("entry-points").and_then(toml::Value::as_table) {
            tables.extend(groups.iter().filter_map(|(_, group)| group.as_table()));
        }
    }
    if let Some(table) = manifest
        .get("tool")
        .and_then(toml::Value::as_table)
        .and_then(|tool| tool.get("poetry"))
        .and_then(toml::Value::as_table)
        .and_then(|poetry| poetry.get("scripts"))
        .and_then(toml::Value::as_table)
    {
        tables.push(table);
    }
    tables
}

/// The `module:attr` one declaration names — the PEP 621 object reference —
/// from a string value or from Poetry's `{ callable = … }` / `{ reference = … }`
/// table. PEP 621 allows a trailing ` [extra, …]`; the extras are not part of
/// the reference. `qualname` is dotted so `pkg.mod:Class.method` reaches the
/// method's own qualified name, which is exactly the `file::Type.name` the
/// extractor writes for it.
fn object_reference(value: &toml::Value) -> Option<(&str, &str)> {
    let text = value.as_str().or_else(|| {
        let table = value.as_table()?;
        table
            .get("callable")
            .or_else(|| table.get("reference"))
            .and_then(toml::Value::as_str)
    })?;
    let reference = text.split('[').next().unwrap_or(text);
    let (module, attribute) = reference.split_once(':')?;
    let (module, attribute) = (module.trim(), attribute.trim());
    (is_dotted_identifier(module) && is_dotted_identifier(attribute)).then_some((module, attribute))
}

/// `a`, `a.b`, `_x.y2` — ASCII identifiers joined by dots, nothing else.
fn is_dotted_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Most entry-point declarations one manifest is read for.
///
/// Counted in *declarations*, which is what the manifest writes; each one
/// contributes [`ENTRY_POINT_CANDIDATES`] annotations, so the annotation
/// ceiling is the product. Naming it in annotations instead would make the
/// number mean something different from what a reader of a `pyproject.toml`
/// can count.
///
/// A bound, not a sample: past it the exemption is simply not claimed, which
/// is the fail-open direction for this rule — a missing exemption produces an
/// extra dead-symbol *finding*, never a hidden one, so a truncated read can
/// only over-report. No manifest in any corpus measured here comes within two
/// orders of magnitude of it.
const ENTRY_POINT_CAP: usize = 512;

/// Module paths one `pkg.mod:attr` target is tried against.
///
/// Pinned equal to `wiring._add_module_file`'s candidate list by
/// `tests/unit/test_wiring_parity_with_kernel.py`.
const ENTRY_POINT_CANDIDATES: usize = 4;

/// Symbol identities a Python console-script declaration names, with a reason.
///
/// `[project.scripts] cli = "pkg.mod:func"` is a call site: `pip` writes a
/// launcher that imports `pkg.mod` and calls `func`, and that launcher is
/// generated at install time and lives outside the corpus. Nothing in the
/// repository need ever reference `func`.
///
/// Before this, `config_script_entry` marked the manifest itself a
/// [`WiringKind::ScriptEntry`], and `ScriptEntry` exempts only symbols whose
/// `target_symbol` is that same file — a TOML file, which declares none. So the
/// entry function was a `dead_symbol_candidate` at the `extracted` tier, the
/// one agents are told to act on. `devcouncil.indexing.wiring.entry_point_symbols`
/// was the only implementation of the rule and had lost its caller (deleted in
/// `c9f9202`); this is that rule, in the engine.
///
/// **Every** plausible module path is emitted rather than the first that
/// exists on disk, which is where this parts company with the Python original.
/// `extract_wiring_annotations` is a pure function of `(path, source)` and the
/// extraction cache keys on exactly that (`cache::CacheKey`), so a rule that
/// consulted the tree would make a cached payload depend on evidence the key
/// does not cover — the same file would mean different things in two trees and
/// the cache could not tell. The join in `devmap-analyze` is against real
/// extracted symbols, so a candidate naming a module that does not exist
/// matches nothing; this is the shape `WiringKind::DynamicImport` already uses,
/// and `a_dynamic_reference_does_not_resurrect_an_unrelated_cycle` pins that it
/// stays harmless.
///
/// Module paths resolve against the manifest's own directory, so a monorepo's
/// `packages/foo/pyproject.toml` names symbols under `packages/foo/`.
pub fn config_entry_point_symbols(path: &str, source: &str) -> Vec<WiringAnnotation> {
    let normalized = normalize_path(path);
    let name = normalized.rsplit('/').next().unwrap_or(&normalized);
    if name != "pyproject.toml" {
        return Vec::new();
    }
    let base = match normalized.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    };

    let Some(manifest) = parse_toml(source) else {
        return Vec::new();
    };
    let mut declarations: Vec<(&str, &str)> = Vec::new();
    for table in entry_point_tables(&manifest) {
        for (_, value) in table.iter() {
            if declarations.len() >= ENTRY_POINT_CAP {
                break;
            }
            if let Some(reference) = object_reference(value) {
                declarations.push(reference);
            }
        }
    }

    let mut annotations = Vec::new();
    for (module, attribute) in declarations {
        let module_path = module.replace('.', "/");
        let details = format!("declared as an entry point by {normalized}: {module}:{attribute}");
        // Typed to `ENTRY_POINT_CANDIDATES` on purpose: adding a fifth module
        // path here is a type error until the constant moves with it, and the
        // constant is what the parity module's count is checked against.
        let candidates: [String; ENTRY_POINT_CANDIDATES] = [
            format!("{base}{module_path}.py"),
            format!("{base}{module_path}/__init__.py"),
            format!("{base}src/{module_path}.py"),
            format!("{base}src/{module_path}/__init__.py"),
        ];
        for candidate in candidates {
            annotations.push(WiringAnnotation {
                kind: WiringKind::ConfigEntryPoint,
                target_symbol: format!("{candidate}::{attribute}"),
                details: details.clone(),
            });
        }
    }
    annotations
}

fn is_language_main(path: &str, source: &str) -> bool {
    let p = path.replace('\\', "/");
    let name = p.rsplit('/').next().unwrap_or(path);
    if name == "main.rs" && source.contains("fn main(") {
        return true;
    }
    if name == "main.go" && source.contains("package main") && source.contains("func main(") {
        return true;
    }
    if source.contains("if __name__ == '__main__'")
        || source.contains("if __name__ == \"__main__\"")
    {
        return true;
    }
    false
}

pub fn extract_wiring_annotations(path: &str, source: &str) -> Vec<WiringAnnotation> {
    let mut annotations = Vec::new();

    if is_vendored_path(path) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::Vendored,
            target_symbol: path.to_string(),
            details: "Vendored file".to_string(),
        });
    }

    if is_test_path(path) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::TestFile,
            target_symbol: path.to_string(),
            details: "Test suite file".to_string(),
        });
    }

    if is_fixture_path(path) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::Fixture,
            target_symbol: path.to_string(),
            details: "Fixture, snapshot or example tree".to_string(),
        });
    }

    if is_generated_path(path) || source_has_generated_header(source) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::GeneratedFile,
            target_symbol: path.to_string(),
            details: "Generated code".to_string(),
        });
    }

    if looks_like_reexport_init(path, source) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::ReExportPackage,
            target_symbol: path.to_string(),
            details: "Re-export-only package __init__".to_string(),
        });
    }

    // An explicit human declaration outranks a static inference, so this is
    // checked with the same weight as any other exemption rather than as a
    // late override.
    if source.contains(ALLOW_UNWIRED) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::AllowUnwired,
            target_symbol: path.to_string(),
            details: ALLOW_UNWIRED.to_string(),
        });
    }

    for form in dynamic_reference_forms(path, source) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::DynamicImport,
            // The *referenced* form, not this file: the annotation records what
            // this file reaches, and `devmap-analyze` joins it against the
            // corpus. Every other kind is self-scoped, so the file-vs-symbol
            // partition in `liveness.rs` must not mistake this for one.
            target_symbol: form,
            details: format!("dynamic reference from {path}"),
        });
    }

    if is_launcher_file(path) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::Launcher,
            target_symbol: path.to_string(),
            details: format!("Launcher file {}", path),
        });
    }

    if config_script_entry(path, source) || is_language_main(path, source) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::ScriptEntry,
            target_symbol: path.to_string(),
            details: "Script / binary entry point".to_string(),
        });
    }

    // The operating system's own answer to "who runs this". Checked on the
    // file's first line rather than on its extension, because that is what
    // `exec(2)` reads — and `__main__.py`, which `python -m pkg` runs by name
    // and which carries no shebang.
    //
    // A `ScriptEntry`, so it exempts the file's symbols too: a function
    // declared in a script is called by the script, and there is no other
    // place for it to be called from.
    let language = crate::languages::detect_language(std::path::Path::new(path));
    if crate::languages::liveness_unit_for_language(language)
        != crate::languages::LivenessUnit::Data
    {
        let script_reason = if has_shebang(source) {
            Some("Executable script (shebang)")
        } else if path
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .is_some_and(|name| name == "__main__.py")
        {
            Some("Python module entry point run by `python -m`")
        } else {
            None
        };
        if let Some(reason) = script_reason {
            annotations.push(WiringAnnotation {
                kind: WiringKind::ScriptEntry,
                target_symbol: path.to_string(),
                details: reason.to_string(),
            });
        }
    }

    if let Some(reason) = package_marker_reason(path, source) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::PackageMarker,
            target_symbol: path.to_string(),
            details: reason.to_string(),
        });
    }

    if let Some(reason) = tool_config_reason(path) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::ToolConfig,
            target_symbol: path.to_string(),
            details: reason.to_string(),
        });
    }

    if is_ambient_declaration(path) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::AmbientDeclaration,
            target_symbol: path.to_string(),
            details: "TypeScript ambient declaration file".to_string(),
        });
    }

    // The symbol half of the same declaration. `ScriptEntry` above is a claim
    // about the manifest *file*, and a manifest declares no symbols — so on its
    // own it left the function a console script actually names as a dead-symbol
    // candidate. These carry the resolved `module:attr` target.
    annotations.extend(config_entry_point_symbols(path, source));

    // A file the toolchain compiles as a root: nothing in the source imports
    // it, nothing should, and its `mod` declarations run outward from it.
    //
    // Deliberately **not** `ScriptEntry`, which exempts every symbol in the
    // file from the dead-code verdict. That over-exemption is what
    // `test_runtime_entry_points_are_exempt_without_exempting_their_file`
    // exists to refuse, and it fired the moment this rule was first written as
    // a `ScriptEntry`: an unused helper in `src/bin/tool.rs` went from
    // confidently dead to exempt because its file had a `main`. A target root
    // is a claim about the *file's* wiring and nothing else.
    if path.ends_with(".rs") {
        if let Some(reason) = rust_target_root_reason(path) {
            annotations.push(WiringAnnotation {
                kind: WiringKind::TargetRoot,
                target_symbol: path.to_string(),
                details: reason.to_string(),
            });
        }
    } else if let Some(reason) = js_target_root_reason(path) {
        annotations.push(WiringAnnotation {
            kind: WiringKind::TargetRoot,
            target_symbol: path.to_string(),
            details: reason.to_string(),
        });
    }

    for line in source.lines() {
        let t = line.trim();
        if t.starts_with('@') && is_wiring_decorator(t) {
            annotations.push(WiringAnnotation {
                kind: WiringKind::FrameworkDecorator,
                target_symbol: path.to_string(),
                details: t.to_string(),
            });
            break;
        }
    }

    annotations
}

// ---------------------------------------------------------------------------
// W3.3 — the two rules the Python wiring module held and the kernel did not

/// The author's explicit "this file is intentionally unwired" declaration.
///
/// One spelling, matching `devcouncil.indexing.wiring.ALLOW_UNWIRED` exactly.
/// The two are asserted equal by `tests/allow_unwired_and_dynamic_imports.rs`,
/// because a marker the kernel spells differently is a marker the kernel
/// ignores — silently, and only for the files that use it.
pub const ALLOW_UNWIRED: &str = "devcouncil: allow-unwired";

/// Suffixes whose source is worth scanning for dynamic references.
///
/// Mirrors `_CODE_CONFIG_SUFFIXES`. Config formats are in the list because
/// `pyproject.toml`, `package.json` and friends name entry points that no
/// import edge records.
///
/// `cfg` and `ini` were in the Python list and are gone from this one. Neither
/// is reachable: `detect_language` names no `.cfg` or `.ini` arm, so both
/// answer `"generic"`, `is_indexable_source` refuses them, and no `Extraction`
/// for one ever exists to be scanned. A suffix listed here that discovery
/// never yields reads as coverage this build does not have —
/// `every_scanned_suffix_is_a_suffix_discovery_yields` is what keeps the list
/// honest about that.
const CODE_CONFIG_SUFFIXES: &[&str] = &[
    "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "svelte", "vue", "astro", "html", "htm", "toml",
    "json", "yaml", "yml",
];

/// Extensions a relative JS specifier resolves through. Mirrors `_JS_RESOLVE_EXTS`.
///
/// `.svelte` / `.vue` / `.astro` belong here because a bundler resolves
/// `import('./Panel.svelte')` to that file, and until they were listed the
/// kernel scanned every `.ts` lazy import and skipped the `.svelte` file that
/// actually wrote `import('./CloneModal.svelte')`.
const JS_RESOLVE_EXTS: &[&str] = &[
    ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".svelte", ".vue", ".astro",
];

fn dynamic_reference_patterns() -> &'static [regex::Regex] {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // Ported verbatim from `devcouncil.indexing.wiring`, each beside the
        // constant it mirrors. Rust's `regex` has no lookaround, and none of
        // these need it.
        [
            // _IMPORTLIB_RE
            r#"(?:importlib(?:\.import_module)?|__import__)\s*\(\s*['"]([^'"]+)['"]"#,
            // _DYNAMIC_IMPORT_RE — `import('./App')`
            r#"import\s*\(\s*['"]([^'"]+)['"]\s*\)"#,
            // _WORKER_URL_RE — Vite/webpack worker entry points
            r#"new\s+URL\s*\(\s*['"]([^'"]+)['"]\s*,\s*import\.meta\.url"#,
            // _PYTHON_DASH_M_RE, both argv shapes
            r#"(?:^|[^\w-])(?:-m|--module)(?:\s+|\s*,\s*)['"]([A-Za-z_][\w.]*)['"]"#,
            r#"['"](?:-m|--module)['"]\s*,\s*['"]([A-Za-z_][\w.]*)['"]"#,
            // _PACKAGE_RESOURCES_RE
            r#"(?:resources\.)?files\s*\(\s*['"]([A-Za-z_][\w.]*)['"]"#,
        ]
        .iter()
        .map(|pattern| regex::Regex::new(pattern).expect("dynamic-reference pattern compiles"))
        .collect()
    })
}

/// `a/b/../c` → `a/c`, and `./x` → `x`. Mirrors `_normalize_rel_path`.
fn normalize_rel_path(target: &str) -> String {
    let normalized = target.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// HTML-only specifiers. Kept off the shared pattern list because
/// `from 'react'` is an ordinary static import in `.ts` and must not become a
/// DynamicImport form — `ordinary_source_produces_no_dynamic_forms` exists to
/// refuse that. An HTML file has no import extractor, so the same syntax is
/// the only record that `index.html` reaches `src/main.ts`.
fn html_reference_patterns() -> &'static [regex::Regex] {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r#"from\s+['"]([^'"]+)['"]"#,
            r#"src\s*=\s*['"]([^'"]+)['"]"#,
        ]
        .iter()
        .map(|pattern| regex::Regex::new(pattern).expect("html-reference pattern compiles"))
        .collect()
    })
}

/// Path-like tokens in a launcher or `package.json` script. Mirrors
/// `_LAUNCHER_PATH_REF_RE` without lookbehind (Rust's `regex` has none).
fn launcher_path_ref_pattern() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?:^|[^\w.-])((?:[\w.-]+/)*[\w-]+\.(?:mjs|cjs|jsx|tsx|py|js|ts|sh|go|rb))\b",
        )
        .expect("launcher path-ref pattern compiles")
    })
}

/// Specs a `package.json` `scripts` table names. Python
/// `_package_json_script_keys` already did this; the kernel never scanned the
/// table, so every `node scripts/vite-dev.mjs` CLI was an unwired candidate.
fn package_json_script_specs(path: &str, source: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(source) else {
        return Vec::new();
    };
    let Some(scripts) = value.get("scripts").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let text = scripts
        .values()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return Vec::new();
    }
    let parent = match normalize_path(path).rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    };
    let mut specs = Vec::new();
    for capture in launcher_path_ref_pattern().captures_iter(&text) {
        let Some(spec) = capture.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let spec = normalize_rel_path(spec);
        if spec.is_empty() {
            continue;
        }
        specs.push(spec.clone());
        if parent.is_empty() {
            continue;
        }
        let joined = normalize_rel_path(&format!("{parent}/{spec}"));
        if !joined.is_empty() {
            specs.push(joined);
        }
    }
    specs
}

/// Comparable dotted + slash forms, extensions stripped, for boundary matching.
///
/// A verbatim port of `wiring._module_forms`. The set it produces is
/// deliberately generous — `src/shared/Panel.tsx` yields `src/shared/Panel/tsx`
/// among others — because it is one side of a set intersection, not a claim
/// that every member names a real file. Reproducing the generosity exactly is
/// the point: a kernel that generated a *tidier* set would clear a different
/// set of files than Python does, which is the disagreement this work order
/// exists to end.
fn module_forms(value: &str) -> Vec<String> {
    let normalized = normalize_path(value);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut forms = vec![
        normalized.clone(),
        normalized.replace('/', "."),
        normalized.replace('.', "/"),
    ];
    for ext in MODULE_FORM_EXTS {
        if let Some(base) = normalized.strip_suffix(ext) {
            if !base.is_empty() {
                forms.push(base.to_string());
                forms.push(base.replace('/', "."));
                forms.push(base.replace('.', "/"));
            }
            break;
        }
    }
    forms.retain(|form| !form.is_empty());
    forms.sort();
    forms.dedup();
    forms
}

/// Extensions `module_forms` strips. Mirrors the tuple inlined in `_module_forms`,
/// which is `_JS_RESOLVE_EXTS` with `.py` in front.
const MODULE_FORM_EXTS: &[&str] = &[
    ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".svelte", ".vue", ".astro",
];

/// `wiring._norm`: forward slashes, no leading `./`.
fn normalize_path(value: &str) -> String {
    let mut normalized = value.replace('\\', "/");
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    normalized
}

/// Resolve one specifier to the specs Python would collect for it.
///
/// A relative specifier resolves against the referring file and contributes
/// *both* the resolved path and its extension-stripped stem, so `import('./App')`
/// and `import('./App.tsx')` each reach `App.tsx`. A bare specifier contributes
/// itself.
fn specs_for(referrer: &str, spec: &str) -> Vec<String> {
    let spec = spec.trim();
    // Vite/HTML root-absolute URLs: `src="/src/main.ts"` and `src="/theme-boot.js"`
    // (the latter is served from `public/`). A protocol-relative `//cdn` is not
    // a project path.
    if spec.starts_with('/') && !spec.starts_with("//") {
        let relative = spec.trim_start_matches('/');
        if relative.is_empty() {
            return Vec::new();
        }
        return vec![relative.to_string(), format!("public/{relative}")];
    }
    if !spec.starts_with('.') {
        return vec![spec.to_string()];
    }
    let parent = match normalize_path(referrer).rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    };
    let joined = if parent.is_empty() {
        spec.to_string()
    } else {
        format!("{parent}/{spec}")
    };
    let resolved = normalize_rel_path(&joined);
    if resolved.is_empty() {
        return Vec::new();
    }
    let mut stem = resolved.clone();
    for ext in JS_RESOLVE_EXTS {
        if let Some(base) = resolved.strip_suffix(ext) {
            stem = base.to_string();
            break;
        }
    }
    vec![resolved, stem]
}

fn file_suffix(path: &str) -> &str {
    path.rsplit_once('/')
        .map_or(path, |(_, name)| name)
        .rsplit_once('.')
        .map_or("", |(_, ext)| ext)
}

/// Dynamic references made *by* `path`, as normalized target forms.
///
/// The one heuristic the Python wiring module held that the kernel lacked. A
/// lazily imported plugin, a code-split route and a worker entry point are all
/// reachable and all invisible to an import-edge walk, so without this the
/// kernel calls them unwired — confidently, and on every build.
pub fn dynamic_reference_forms(path: &str, source: &str) -> Vec<String> {
    let suffix = file_suffix(path).to_ascii_lowercase();
    if !CODE_CONFIG_SUFFIXES.contains(&suffix.as_str()) {
        return Vec::new();
    }
    let mut specs: Vec<String> = Vec::new();
    for pattern in dynamic_reference_patterns() {
        for capture in pattern.captures_iter(source) {
            let Some(spec) = capture.get(1).map(|m| m.as_str()) else {
                continue;
            };
            if spec.is_empty() {
                continue;
            }
            specs.extend(specs_for(path, spec));
        }
    }
    if suffix == "html" || suffix == "htm" {
        for pattern in html_reference_patterns() {
            for capture in pattern.captures_iter(source) {
                let Some(spec) = capture.get(1).map(|m| m.as_str()) else {
                    continue;
                };
                if spec.is_empty() {
                    continue;
                }
                specs.extend(specs_for(path, spec));
            }
        }
    }
    // Python `wiring._package_json_script_keys`: npm script values name CLI
    // files no import edge records. The kernel never scanned them, so every
    // `scripts/*.mjs` in a Node project was an unwired candidate.
    if file_suffix(path).eq_ignore_ascii_case("json")
        && path
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .is_some_and(|name| name == "package.json")
    {
        specs.extend(package_json_script_specs(path, source));
    }
    let mut forms: Vec<String> = specs.iter().flat_map(|spec| module_forms(spec)).collect();
    forms.sort();
    forms.dedup();
    forms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_rule_covers_names_and_directories() {
        for path in [
            "tests/test_thing.py",
            "pkg/foo_test.go",
            "web/src/Button.test.tsx",
            "web/src/Button.spec.ts",
            "app/__tests__/helper.js",
            "app/src/test/java/Thing.java",
            "ios/AppTests.swift",
            "android/ThingTest.kt",
            "svc/conftest.py",
        ] {
            assert!(is_test_path(path), "{path} should be a test path");
        }
        for path in [
            "src/contest.py",
            "src/latest.go",
            "src/testing_utils.py",
            "src/protest/main.rs",
        ] {
            assert!(!is_test_path(path), "{path} must not be a test path");
        }
    }

    #[test]
    fn test_path_rule_accepts_windows_separators() {
        assert!(is_test_path(r"web\src\__tests__\Button.js"));
    }

    #[test]
    fn test_path_rule_ignores_dotfiles_inside_a_test_directory() {
        // A dotfile only lands in the directory bucket, and that bucket
        // explicitly excludes it; a filename rule would still apply.
        assert!(!is_test_path("tests/.eslintrc"));
        assert!(is_test_path("tests/.eslintrc.test.js"));
    }

    #[test]
    fn vendored_paths_are_directory_scoped_not_substring_scoped() {
        for path in [
            "node_modules/react/index.js",
            "third_party/lib/a.py",
            "vendor/github.com/x/y.go",
            "web/static/jquery.min.js",
            "src-tauri/framework/tao/src/lib.rs",
            "src-tauri/framework/wry/src/android/kotlin/Ipc.kt",
        ] {
            assert!(is_vendored_path(path), "{path} should be vendored");
        }
        for path in [
            "src/vendoring.py",
            "src/node_modules_helper.ts",
            "src/framework/app.ts",
        ] {
            assert!(!is_vendored_path(path), "{path} must not be vendored");
        }
    }

    #[test]
    fn generated_paths_match_the_tool_suffixes() {
        for path in [
            "api/service_pb2.py",
            "api/service_pb2.pyi",
            "api/service_pb2_grpc.py",
            "api/service.pb.go",
            "api/service.pb.gw.go",
            "web/service_pb.js",
            "web/service_pb.d.ts",
            "k8s/zz_generated.deepcopy.go",
        ] {
            assert!(is_generated_path(path), "{path} should be generated");
        }
        assert!(!is_generated_path("api/service.py"));
        assert!(!is_generated_path("api/pb2_helpers.py"));
    }

    #[test]
    fn generated_header_must_appear_in_the_leading_comment_block() {
        assert!(source_has_generated_header(
            "// Code generated by protoc. DO NOT EDIT.\npackage api\n"
        ));
        assert!(source_has_generated_header("# @generated\nimport os\n"));
        assert!(source_has_generated_header(
            "\n\n/* automatically generated */\n"
        ));
        // Real code before the marker: the file is hand-written and merely
        // mentions generation, so it must not be exempted.
        assert!(!source_has_generated_header(
            "import os\n# Code generated by protoc\n"
        ));
        // Beyond the leading five lines the marker is not a header.
        assert!(!source_has_generated_header(
            "#\n#\n#\n#\n#\n# Code generated\n"
        ));
        assert!(!source_has_generated_header("def handler(): pass\n"));
    }

    /// Case folding, on decorators that are wiring in any casing.
    ///
    /// `@FastAPI_thing` used to stand here as the case-folding fixture and has
    /// been replaced by `@FastAPI` and `@App.Route('/')`. It was never a
    /// case-insensitivity case: it matched only because `fastapi` was compared
    /// as a bare *substring* of `fastapi_thing`, which is the defect
    /// `wiring_decorator_hints_are_prefixes_and_segments_not_substrings`
    /// refuses. The two replacements exercise exactly what this test is named
    /// for — a mixed-case segment and a mixed-case dotted prefix — without
    /// re-encoding the over-match.
    #[test]
    fn wiring_decorator_hints_are_case_insensitive() {
        for decorator in [
            "@app.route('/')",
            "@router.get('/x')",
            "@click.command()",
            "@pytest.fixture",
            "@celery.task",
            "@FastAPI",
            "@App.Route('/')",
            "@receiver(post_save)",
        ] {
            assert!(
                is_wiring_decorator(decorator),
                "{decorator} should be wiring"
            );
        }
        for decorator in ["@dataclass", "@property", "@staticmethod", "@lru_cache"] {
            assert!(
                !is_wiring_decorator(decorator),
                "{decorator} must not be wiring"
            );
        }
    }

    #[test]
    fn rust_runner_attributes_match_on_the_last_path_segment() {
        for attribute in [
            "test",
            "tokio::test",
            "actix_rt::test",
            "bench",
            "ctor",
            "no_mangle",
            "proc_macro_derive",
            "tokio::main",
            "global_allocator",
            "wasm_bindgen",
        ] {
            assert!(
                rust_attribute_entry_reason(attribute).is_some(),
                "#[{attribute}] should be an entry point"
            );
        }
        // `#[cfg(test)]` marks a *module* as test-only; it says nothing about
        // any one function, so it must not exempt one.
        for attribute in ["cfg", "derive", "inline", "allow", "serde", "doc"] {
            assert!(
                rust_attribute_entry_reason(attribute).is_none(),
                "#[{attribute}] must not exempt a symbol"
            );
        }
    }

    #[test]
    fn rust_binary_target_roots_declare_main() {
        for path in [
            "src/main.rs",
            "src/bin/tool.rs",
            "examples/demo.rs",
            "benches/bench.rs",
            "tests/integration.rs",
        ] {
            assert!(rust_path_declares_main(path), "{path} may declare main");
        }
        for path in ["src/lib.rs", "src/binary/thing.rs", "src/example.rs"] {
            assert!(
                !rust_path_declares_main(path),
                "{path} must not declare main"
            );
        }
    }

    #[test]
    fn go_main_is_an_entry_point_only_inside_package_main() {
        assert!(go_runtime_entry_reason("init", Some("worker"), false).is_some());
        assert!(go_runtime_entry_reason("main", Some("main"), false).is_some());
        // In any other package `main` is an ordinary unexported function.
        assert!(go_runtime_entry_reason("main", Some("worker"), false).is_none());
        assert!(go_runtime_entry_reason("main", None, false).is_none());
        // A method named `init` is callable and is not the package initializer.
        assert!(go_runtime_entry_reason("init", Some("worker"), true).is_none());
        assert!(go_runtime_entry_reason("helper", Some("main"), false).is_none());
    }

    #[test]
    fn python_harness_names_are_prefix_and_exact_rules() {
        for name in [
            "test_thing",
            "pytest_configure",
            "setup_module",
            "teardown_function",
            "setUp",
            "tearDownClass",
        ] {
            assert!(
                python_harness_entry_reason(name).is_some(),
                "{name} should be a harness entry point"
            );
        }
        // `setup` and `teardown` are ordinary function names outside a test
        // class, and nose-style bare hooks are gone from pytest 8.
        for name in ["setup", "teardown", "testing", "protest", "handler"] {
            assert!(
                python_harness_entry_reason(name).is_none(),
                "{name} must not be a harness entry point"
            );
        }
    }

    #[test]
    fn js_lifecycle_hooks_are_an_exact_name_list() {
        for name in [
            "constructor",
            "render",
            "componentDidMount",
            "componentWillUnmount",
            "ngOnInit",
            "ngAfterViewInit",
            "connectedCallback",
            "attributeChangedCallback",
        ] {
            assert!(
                js_lifecycle_hook_reason(name).is_some(),
                "{name} should be a lifecycle hook"
            );
        }
        for name in ["renderRow", "onInit", "mounted", "handleClick", "update"] {
            assert!(
                js_lifecycle_hook_reason(name).is_none(),
                "{name} must not be a lifecycle hook"
            );
        }
    }

    #[test]
    fn js_bundler_plugin_hooks_are_an_exact_name_list() {
        for name in [
            "generateBundle",
            "writeBundle",
            "closeBundle",
            "renderChunk",
            "configureServer",
            "configurePreviewServer",
            "transformIndexHtml",
            "handleHotUpdate",
            "hotUpdate",
        ] {
            assert!(
                js_bundler_plugin_hook_reason(name).is_some(),
                "{name} should be a bundler plugin hook"
            );
        }
        for name in [
            "transform",
            "load",
            "options",
            "config",
            "handler",
            "buildStart",
        ] {
            assert!(
                js_bundler_plugin_hook_reason(name).is_none(),
                "{name} must not be a bundler plugin hook"
            );
        }
    }

    /// The dotfile exclusion applies to the JVM shape too.
    ///
    /// `/src/test/` and `/src/androidtest/` used to `return true` outright,
    /// ahead of the `!name.starts_with('.')` guard that
    /// `test_path_rule_ignores_dotfiles_inside_a_test_directory` documents. So
    /// the rule the kernel published held for `tests/.eslintrc` and not for
    /// `app/src/test/.eslintrc` — the same file, one directory layout apart.
    #[test]
    fn the_dotfile_exclusion_survives_the_jvm_test_directories() {
        for path in [
            "app/src/test/.eslintrc",
            "app/src/test/.gitkeep",
            "app/src/androidTest/.env",
            "app/src/androidTest/.hidden.py",
        ] {
            assert!(
                !is_test_path(path),
                "{path}: a dotfile lands in the directory bucket, and that bucket \
                 excludes it — the `/src/test/` early return skipped the exclusion"
            );
        }
        // Everything the directory rule did claim, it still claims.
        assert!(is_test_path("app/src/test/java/Thing.java"));
        assert!(is_test_path("app/src/androidTest/Thing.kt"));
        // And a filename rule still outranks the dotfile exclusion.
        assert!(is_test_path("app/src/test/.eslintrc.test.js"));
    }

    /// The JVM branch answers for the *directory* the file sits in, so a file
    /// that happens to be named `test` or `androidTest` under `src/` is not a
    /// test path. The Python copy wrapped the whole path in slashes before
    /// looking for `/src/test/`, so `app/src/test` — a file — read as a test
    /// directory, and a script by that name was exempted from liveness. The
    /// parity module reads this block, so the Python copy is held to it.
    #[test]
    fn a_file_named_like_the_jvm_test_directory_is_not_a_test_path() {
        for path in ["src/test", "app/src/test", "app/src/androidTest"] {
            assert!(
                !is_test_path(path),
                "{path}: the last segment is the file's name, not a directory it sits in"
            );
        }
        // The directory rule still claims what it claimed.
        assert!(is_test_path("src/test/Foo.kt"));
        assert!(is_test_path("app/src/androidTest/Ui.kt"));
    }

    /// A hint is a path prefix or a whole identifier segment, never a substring.
    ///
    /// `lower.contains(h)` made `@multitask` a `task`, `@preregister` a
    /// `register` and `@FastAPI_thing` a `fastapi`. Because the annotation this
    /// feeds targets the *file* and `is_file_exempt` exempts every symbol in
    /// the file, one such line hides a whole file from the dead-code scan.
    /// `devcouncil.indexing.wiring.is_wiring_decorated` is the spec: dotted
    /// hints match as prefixes, bare hints as whole segments.
    #[test]
    fn wiring_decorator_hints_are_prefixes_and_segments_not_substrings() {
        for decorator in [
            "@multitask",
            "@preregister",
            "@FastAPI_thing",
            "@my_action_helper",
            "@reroute",
            "@taskless",
            "@commander",
        ] {
            assert!(
                !is_wiring_decorator(decorator),
                "{decorator} is not framework registration; a substring match \
                 hides every symbol in its file from the dead-code scan"
            );
        }
        // The hint table is unchanged, and everything it was written for still
        // matches.
        for decorator in [
            "@app.route('/')",
            "@router.get('/x')",
            "@click.command()",
            "@pytest.fixture",
            "@celery.task",
            "@receiver(post_save)",
            "@api_view(['GET'])",
            "@task",
            "@register",
        ] {
            assert!(
                is_wiring_decorator(decorator),
                "{decorator} should be wiring"
            );
        }
    }

    /// The generator suffixes are the ones a generator actually writes.
    ///
    /// grpcio-tools emits `*_pb2_grpc.pyi` beside `*_pb2_grpc.py` and the
    /// kernel knew only the second; `zz_generated` is a kubebuilder convention
    /// and kubebuilder writes Go, so a bare `starts_with` claimed prose.
    #[test]
    fn generated_paths_match_what_the_generator_writes() {
        for path in ["api/service_pb2_grpc.pyi", "k8s/zz_generated.deepcopy.go"] {
            assert!(
                is_generated_path(path),
                "{path}: grpcio-tools writes the grpc type stub beside the module, \
                 and kubebuilder writes Go"
            );
        }
        for path in [
            "docs/zz_generated.txt",
            "docs/zz_generated_notes.md",
            "web/zz_generated.ts",
        ] {
            assert!(
                !is_generated_path(path),
                "{path}: `zz_generated` is kubebuilder's Go convention, and \
                 exempting a non-Go file on that prefix clears a real finding"
            );
        }
    }

    /// `[project.scripts]` names a *symbol*, and the annotation carries it.
    ///
    /// `config_script_entry` marks the manifest a `ScriptEntry`, which is a
    /// claim about the file; a TOML file declares no symbols, so on its own it
    /// exempts nothing and the entry function stays a dead-symbol candidate.
    /// Every plausible module path is emitted rather than the first that
    /// exists, because this function is pure in `(path, source)` and the
    /// extraction cache keys on exactly that.
    #[test]
    fn a_console_script_declaration_names_the_function_it_calls() {
        const MANIFEST: &str = "\
[project]
name = \"fx\"

[project.scripts]
fxtool = \"pkg.cli:main_entry\"

[project.gui-scripts]
fxgui = \"pkg.ui:launch\"

[project.entry-points.some_plugins]
plug = \"pkg.plug:Registry.build\"

[tool.poetry.scripts]
poetry_tool = \"pkg.poetry_cli:run\"

[tool.ruff]
line-length = 100
lint = \"not.an:entrypoint\"
";
        let targets: Vec<String> = extract_wiring_annotations("pyproject.toml", MANIFEST)
            .into_iter()
            .filter(|a| a.kind == WiringKind::ConfigEntryPoint)
            .map(|a| a.target_symbol)
            .collect();

        for expected in [
            "pkg/cli.py::main_entry",
            "pkg/cli/__init__.py::main_entry",
            "src/pkg/cli.py::main_entry",
            "src/pkg/cli/__init__.py::main_entry",
            "pkg/ui.py::launch",
            "pkg/plug.py::Registry.build",
            "pkg/poetry_cli.py::run",
        ] {
            assert!(
                targets.iter().any(|t| t == expected),
                "a console script names {expected} and the kernel never resolved \
                 the module:attr target, so the entry function stayed a \
                 dead-symbol candidate: {targets:?}"
            );
        }
        // A key outside an entry-point section is not an entry point, however
        // much it looks like one.
        assert!(
            !targets.iter().any(|t| t.contains("::entrypoint")),
            "a `module:attr`-shaped value under [tool.ruff] is not a declared \
             entry point: {targets:?}"
        );
        // The manifest keeps its own file-scoped claim.
        assert!(extract_wiring_annotations("pyproject.toml", MANIFEST)
            .iter()
            .any(|a| a.kind == WiringKind::ScriptEntry && a.target_symbol == "pyproject.toml"));
    }

    /// A TOML table header inside a multi-line string is text, not a table.
    ///
    /// The line reader took `[project.scripts]` wherever it stood at the start
    /// of a line, so a description quoting one declared an entry point — and
    /// `config_script_entry`'s substring test claimed the file declared a
    /// script. The manifest is parsed now; a string is a string.
    #[test]
    fn a_table_header_inside_a_multi_line_string_declares_nothing() {
        const MANIFEST: &str = "\
[project]
name = \"fx\"
description = \"\"\"
[project.scripts]
fake = \"evil.mod:run\"
\"\"\"
";
        let annotations = extract_wiring_annotations("pyproject.toml", MANIFEST);
        assert!(
            !annotations
                .iter()
                .any(|a| a.kind == WiringKind::ConfigEntryPoint),
            "a header inside a string declares no entry point: {annotations:?}"
        );
        assert!(
            !annotations
                .iter()
                .any(|a| a.kind == WiringKind::ScriptEntry),
            "a header inside a string is not a script declaration: {annotations:?}"
        );
    }

    /// `[project.entry-points]` with each group as an inline table is valid
    /// TOML the line reader could not see: the header has no group suffix.
    #[test]
    fn an_entry_point_group_written_as_an_inline_table_is_read() {
        const MANIFEST: &str =
            "[project.entry-points]\nconsole_scripts = { mytool = \"pkg.cli:main\" }\n";
        let targets: Vec<String> = config_entry_point_symbols("pyproject.toml", MANIFEST)
            .into_iter()
            .map(|a| a.target_symbol)
            .collect();
        assert!(
            targets.iter().any(|t| t == "pkg/cli.py::main"),
            "an inline-table group declares its scripts: {targets:?}"
        );
    }

    /// Poetry's `{ callable = … }` table and PEP 621's `module:attr [extra]`
    /// suffix both name a symbol; the extras are not part of the reference.
    #[test]
    fn poetry_callable_tables_and_extras_suffixes_name_the_symbol() {
        const MANIFEST: &str = "\
[tool.poetry.scripts]
t = { callable = \"pkg.a:go\" }

[project.scripts]
u = \"pkg.b:run [fast]\"
";
        let targets: Vec<String> = config_entry_point_symbols("pyproject.toml", MANIFEST)
            .into_iter()
            .map(|a| a.target_symbol)
            .collect();
        for expected in ["pkg/a.py::go", "pkg/b.py::run"] {
            assert!(
                targets.iter().any(|t| t == expected),
                "{expected} is declared and must be named: {targets:?}"
            );
        }
        assert!(
            !targets
                .iter()
                .any(|t| t.contains("[fast]") || t.contains("run [")),
            "the extras suffix is not part of the symbol: {targets:?}"
        );
    }

    /// A dependency named `bin` is not a `package.json` script claim; the
    /// substring test read any `"bin"` anywhere in the file as one.
    #[test]
    fn a_dependency_named_bin_is_not_a_package_json_script_claim() {
        let dependency = extract_wiring_annotations(
            "package.json",
            "{\"name\": \"x\", \"dependencies\": {\"bin\": \"1.0.0\"}}",
        );
        assert!(
            !dependency.iter().any(|a| a.kind == WiringKind::ScriptEntry),
            "a dependency named bin declares no script: {dependency:?}"
        );
        let real = extract_wiring_annotations(
            "package.json",
            "{\"name\": \"x\", \"bin\": {\"x\": \"cli.js\"}}",
        );
        assert!(
            real.iter().any(|a| a.kind == WiringKind::ScriptEntry),
            "a top-level bin is a script declaration: {real:?}"
        );
    }

    /// `[[bin]]` inside a Cargo.toml string is text.
    #[test]
    fn a_bin_header_inside_a_cargo_string_is_not_a_target() {
        let quoted =
            "[package]\nname = \"x\"\ndescription = \"\"\"\n[[bin]]\nname = \"fake\"\n\"\"\"\n";
        assert!(
            !extract_wiring_annotations("Cargo.toml", quoted)
                .iter()
                .any(|a| a.kind == WiringKind::ScriptEntry),
            "a [[bin]] inside a string declares no target"
        );
        let real = "[package]\nname = \"x\"\n\n[[bin]]\nname = \"x\"\npath = \"src/main.rs\"\n";
        assert!(
            extract_wiring_annotations("Cargo.toml", real)
                .iter()
                .any(|a| a.kind == WiringKind::ScriptEntry),
            "a real [[bin]] table is a target declaration"
        );
    }

    /// A nested manifest names symbols under its own directory.
    ///
    /// A monorepo's `packages/foo/pyproject.toml` declares `pkg.cli:main` for
    /// `packages/foo/pkg/cli.py`, not for a `pkg/` at the repository root —
    /// which would be some other package's code.
    #[test]
    fn a_nested_manifest_resolves_against_its_own_directory() {
        let targets: Vec<String> = config_entry_point_symbols(
            "packages/foo/pyproject.toml",
            "[project.scripts]\nfoo = \"pkg.cli:main\"\n",
        )
        .into_iter()
        .map(|a| a.target_symbol)
        .collect();
        assert!(
            targets.contains(&"packages/foo/pkg/cli.py::main".to_string()),
            "{targets:?}"
        );
        assert!(
            !targets.iter().any(|t| t.starts_with("pkg/")),
            "a nested manifest must not claim a root-level module of the same \
             name — that is another package's code: {targets:?}"
        );
    }

    #[test]
    fn a_svelte_lazy_import_names_the_component() {
        let forms = dynamic_reference_forms(
            "src/App.svelte",
            "const loadCloneModal = () => import('./lib/components/CloneModal.svelte');\n",
        );
        assert!(
            forms
                .iter()
                .any(|form| form == "src/lib/components/CloneModal"
                    || form == "src/lib/components/CloneModal.svelte"),
            "a Svelte lazy import must name the component it loads: {forms:?}"
        );
    }

    #[test]
    fn an_html_script_src_names_the_module_entry() {
        let forms = dynamic_reference_forms(
            "index.html",
            "<script type=\"module\" src=\"/src/status.ts\"></script>\n",
        );
        assert!(
            forms
                .iter()
                .any(|form| form == "src/status" || form == "src/status.ts"),
            "an HTML script src must name the module it boots: {forms:?}"
        );
        let public =
            dynamic_reference_forms("index.html", "<script src=\"/theme-boot.js\"></script>\n");
        assert!(
            public
                .iter()
                .any(|form| form == "public/theme-boot.js" || form == "public/theme-boot"),
            "a root-absolute public asset must also resolve under public/: {public:?}"
        );
    }

    #[test]
    fn a_package_json_script_names_the_cli_it_runs() {
        let forms = dynamic_reference_forms(
            "package.json",
            r#"{"name":"x","scripts":{"dev":"node scripts/vite-dev.mjs"}}"#,
        );
        assert!(
            forms
                .iter()
                .any(|form| form == "scripts/vite-dev.mjs" || form == "scripts/vite-dev"),
            "an npm script must name the CLI it launches: {forms:?}"
        );
    }

    /// Python's `structural_exemptions` still hold, whichever kind now carries
    /// them.
    ///
    /// The claim this test has always made is "must not be unwired", and that
    /// is a claim about the *annotation set*, not about which predicate
    /// produced it. Two of the paths moved: `vite.config.ts` is a
    /// [`WiringKind::ToolConfig`] now, so it joins the rest of the
    /// by-convention table in [`tool_config_reason`] instead of being the one
    /// member of it that lived somewhere else, and `src/globals.d.ts` is an
    /// [`WiringKind::AmbientDeclaration`], which is exempt without also
    /// claiming to be a place execution starts.
    ///
    /// So the loop asserts the exemption rather than the function, and
    /// `js_target_root_reason`'s own narrowed surface is pinned beneath it —
    /// both directions, or moving a rule out could be spelled as deleting it.
    #[test]
    fn js_target_roots_match_the_python_structural_exemptions() {
        for path in [
            "src/main.ts",
            "src/index.tsx",
            "vite.config.ts",
            "vite.harness.config.ts",
            "svelte.config.js",
            "src/globals.d.ts",
            "scripts/vite-dev.mjs",
            "src/Button.stories.ts",
        ] {
            let annotations = extract_wiring_annotations(path, "export const x = 1;\n");
            assert!(
                annotations.iter().any(|a| a.target_symbol == path
                    && matches!(
                        a.kind,
                        WiringKind::TargetRoot
                            | WiringKind::ToolConfig
                            | WiringKind::AmbientDeclaration
                    )),
                "{path} is a JS/TS toolchain file and must not be unwired: {annotations:?}"
            );
        }
        // Still a `TargetRoot` specifically, so "the exemption survived" cannot
        // be satisfied by relabelling everything as one kind.
        for path in ["src/main.ts", "src/index.tsx", "scripts/vite-dev.mjs"] {
            assert!(
                js_target_root_reason(path).is_some(),
                "{path} is a bundler entry, which is a target root"
            );
        }
        for path in ["vite.config.ts", "src/globals.d.ts"] {
            assert!(
                js_target_root_reason(path).is_none(),
                "{path} moved to its own kind, and two predicates claiming it \
                 is how they come to disagree"
            );
        }
        for path in ["src/lib/foo.ts", "src/status.ts", "src/App.svelte"] {
            assert!(
                js_target_root_reason(path).is_none(),
                "{path} is ordinary product code, not a toolchain root"
            );
            assert!(
                extract_wiring_annotations(path, "export const x = 1;\n")
                    .iter()
                    .all(|a| a.target_symbol != path),
                "{path} must carry no file-scoped exemption at all"
            );
        }
        assert!(
            extract_wiring_annotations("src/main.ts", "export const boot = 1;\n")
                .iter()
                .any(|a| a.kind == WiringKind::TargetRoot)
        );
    }

    /// Every suffix the dynamic-reference scan claims to read is one discovery
    /// actually yields.
    ///
    /// `cfg` and `ini` were in this list, inherited from the Python
    /// `_CODE_CONFIG_SUFFIXES`, and neither was reachable: `detect_language`
    /// names no arm for either, so both answer `"generic"`,
    /// `is_indexable_source` refuses them, and no `Extraction` for one ever
    /// exists to be scanned. A rule that cannot fire reads as coverage this
    /// build does not have — somebody looking for "why is my `setup.cfg`
    /// entry point not honoured" would have found the suffix listed and
    /// concluded the scan had run.
    #[test]
    fn every_scanned_suffix_is_a_suffix_discovery_yields() {
        for suffix in CODE_CONFIG_SUFFIXES {
            let probe = format!("probe/file.{suffix}");
            assert!(
                crate::languages::is_indexable_source(&probe),
                "`{suffix}` is scanned for dynamic references but discovery \
                 never yields a `.{suffix}` file, so the rule cannot fire"
            );
        }
        // The two that were removed, and the reason they had to be: naming a
        // suffix here that answers `generic` is naming a rule that never runs.
        for suffix in ["cfg", "ini"] {
            assert!(
                !crate::languages::is_indexable_source(&format!("probe/file.{suffix}")),
                "`.{suffix}` is still unindexable, so listing it would still be \
                 an unreachable rule"
            );
            assert!(
                !CODE_CONFIG_SUFFIXES.contains(&suffix),
                "`{suffix}` must not come back without discovery admitting it"
            );
        }
        // And the list is not empty, or the assertion above is vacuous.
        assert!(CODE_CONFIG_SUFFIXES.len() > 10);
    }

    /// Nothing but a `pyproject.toml` declares Python entry points here.
    #[test]
    fn only_a_pyproject_contributes_entry_point_symbols() {
        for path in ["Cargo.toml", "package.json", "docs/pyproject.toml.md"] {
            assert!(
                config_entry_point_symbols(path, "[project.scripts]\na = \"p.m:f\"\n").is_empty(),
                "{path} must not be read as a Python manifest"
            );
        }
    }

    /// The entry-point read is bounded, and the bound truncates fail-open.
    ///
    /// A cap that quietly changed the *verdict* would be the honesty defect this
    /// codebase keeps refusing: a check that could not run reporting what a
    /// check that ran and passed reports. This one cannot. Past
    /// `ENTRY_POINT_CAP` declarations the exemption is simply not claimed, so a
    /// truncated read produces extra dead-symbol *findings* and never hides one
    /// — and the cap is counted in declarations, which is what a reader of a
    /// `pyproject.toml` can count, not in the annotations each one expands to.
    #[test]
    fn the_entry_point_read_is_bounded_and_truncates_fail_open() {
        let mut manifest = String::from("[project.scripts]\n");
        for index in 0..(ENTRY_POINT_CAP * 2) {
            manifest.push_str(&format!("tool{index} = \"pkg.mod{index}:main\"\n"));
        }
        let annotations = config_entry_point_symbols("pyproject.toml", &manifest);
        assert_eq!(
            annotations.len(),
            ENTRY_POINT_CAP * ENTRY_POINT_CANDIDATES,
            "the cap is counted in declarations and each contributes \
             {ENTRY_POINT_CANDIDATES} module-path candidates"
        );
        // Fail-open: the declarations past the cap contribute nothing, so their
        // functions stay dead-symbol candidates rather than being cleared on a
        // read that stopped early.
        assert!(
            !annotations.iter().any(|a| a
                .target_symbol
                .starts_with(&format!("pkg/mod{}.py", ENTRY_POINT_CAP))),
            "a declaration past the cap must contribute no exemption at all"
        );
        // And the first one still does, or the cap has turned the rule off.
        assert!(annotations
            .iter()
            .any(|a| a.target_symbol == "pkg/mod0.py::main"));
    }
}
