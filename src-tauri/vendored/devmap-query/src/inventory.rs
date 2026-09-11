//! The repository evidence the extraction pass does not carry.
//!
//! `repo_map.json`'s `package_managers` and `test_commands` shipped as `[]` on
//! every repository this kernel has ever mapped, marked
//! `package_managers_computed: false` / `test_commands_computed: false`. The
//! reason recorded in `manifest.rs` was accurate about the *extractions*: a
//! `.lock` file matches no language spec, so `is_indexable_source` excludes it
//! and it never reaches an `Extraction`; and a test command needs the contents
//! of `pyproject.toml` or `package.json`, not their declaration.
//!
//! It was not a reason the kernel could not answer the question. The kernel is
//! handed a repository root — `build_code_graph_value` already takes one — so
//! reading a *bounded* inventory off that root answers both from evidence this
//! producer can support, and `hotspots` needs the same treatment for a
//! different reason (repository history, which extraction also does not read).
//! Both live here so there is one owner for "look at the repository itself",
//! with one set of bounds.
//!
//! Git marker discovery reuses the bounded Git listing and accepts at most
//! [`GIT_FILE_CAP`] paths without a depth limit. Without Git metadata, the
//! filesystem fallback bounds depth, directory count, pending frontier, entries
//! and elapsed time between filesystem calls. Truncation and read failures make
//! completeness false. A blocked OS filesystem call is not interruptible here.
//!
//! Manifests share the descriptor-based source reader with a tighter
//! [`MANIFEST_READ_CAP`] ceiling. Oversized, unreadable and malformed manifests
//! are reported separately from a valid manifest declaring no commands.
//! `git log` has its own deadline, commit cap and output cap.

use devmap_analyze::graph_intel::FileChurn;
use devmap_extract::subprocess::{run_bounded, Bounds, Failure};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

/// Largest manifest this reader will pull into memory.
///
/// A `package.json` or `pyproject.toml` is kilobytes; a quarter of a megabyte
/// is far past any hand-written one and small enough that reading it costs
/// nothing measurable on the artifact-write path. A file past it is recorded,
/// not read.
pub const MANIFEST_READ_CAP: u64 = 256 * 1024;

/// Depth ceiling for the non-Git filesystem fallback. A bound hit is reported;
/// Git repositories use their path inventory and are not limited by this depth.
pub const WALK_DEPTH_CAP: usize = 8;

/// Most directories the marker walk will open.
///
/// The bound exists so a pathological tree — a generated fixture corpus, a
/// checked-in dependency cache the skip list does not name — cannot turn an
/// artifact write into a full-disk traversal. On this repository the walk opens
/// roughly 400.
pub const WALK_DIR_CAP: usize = 20_000;
/// Entries examined by the filesystem fallback, including skipped entries.
pub const WALK_ENTRY_CAP: usize = 200_000;
/// Cooperative deadline between filesystem operations; it cannot interrupt a
/// filesystem call stalled in the operating system.
pub const WALK_DEADLINE: Duration = Duration::from_secs(5);
pub const GIT_FILE_CAP: usize = 50_000;
const ERROR_SAMPLE_CAP: usize = 64;

/// Hard ceiling for the churn subprocess, matching the shape
/// `devmap-store`'s `run_git_head_with_deadline` established for `rev-parse`:
/// `git` can stall on a network mount, a hook, or a lock, and unbounded it
/// would stall the artifact write behind it.
pub const CHURN_DEADLINE: Duration = Duration::from_secs(10);

/// Most commits the churn window will look at.
///
/// Paired with the date window rather than replacing it: `--since` alone is
/// unbounded on a repository with a very busy quarter, and this is what stops
/// the output being a function of commit rate.
pub const CHURN_COMMIT_CAP: usize = 5_000;

/// Most bytes of `git log` output the churn reader will accept.
pub const CHURN_OUTPUT_CAP: usize = 8 * 1024 * 1024;

/// The churn window. The Python original's `--since=90.days`, kept.
pub const CHURN_SINCE: &str = "90.days";

/// The day the churn window is anchored to, as days since the Unix epoch.
///
/// `--since=90.days` is relative to *now*, so the set of commits churn counts
/// changes with the calendar even when the repository does not: a quarter with
/// no new commit still rolls its oldest ones out of the window, and the hotspot
/// counts shrink. Nothing else the artifact stamp records moves with the clock
/// — `built_head` and the fingerprints are properties of the tree — so an
/// artifact written yesterday matched every input today and was kept, silently
/// stale on the one field that depends on the date. This is that input, at day
/// granularity: one regeneration per calendar day at most, and only on a run
/// that would otherwise have skipped.
pub fn churn_window_day() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() / 86_400)
        .unwrap_or(0)
}

/// Directories the marker walk never descends into, at any depth.
///
/// `target` and `node_modules` are build and dependency output whose size is
/// unrelated to the repository's own, and both are conventionally named — a
/// source directory called `node_modules` does not exist, and one called
/// `target` is rare enough that missing a marker inside it costs less than
/// walking a multi-gigabyte cargo tree on every artifact write. `vendor` and
/// `__pycache__` are the same argument.
///
/// `dist`, `build` and `out` are deliberately *not* here and are skipped only
/// at the top level (see [`skip_dir`]), for the reason `freshness.rs` records
/// against the same names: this repository contains a source directory literally
/// named `build`, and excluding it by name at any depth drops real files.
const SKIP_DIR_ANY_DEPTH: &[&str] = &["target", "node_modules", "vendor", "__pycache__"];

/// Directories skipped only where a build tool would put them: the repository
/// root.
const SKIP_DIR_TOP_LEVEL: &[&str] = &["dist", "build", "out"];

/// Marker basenames the walk records wherever it finds them.
///
/// A nested one is real evidence: this repository's `Cargo.toml` is at
/// `rust/Cargo.toml` and its `go.mod` at
/// `backend/go_orchestrator/go.mod`, so a top-level-only rule would report a
/// polyglot repository as Python-only.
const NESTED_MARKERS: &[&str] = &[
    "go.mod",
    "go.sum",
    "Cargo.toml",
    "Package.swift",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradlew",
];

/// Marker basenames that only count at the repository root.
///
/// A lock file names the manager *this repository* is built with. One inside a
/// fixture, an example, or a vendored sub-project names that sub-project's, and
/// reporting it as the repository's is how `package_managers` comes to list six
/// managers for a repository that uses one.
const TOP_LEVEL_MARKERS: &[&str] = &[
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "requirements.txt",
    "uv.lock",
    "poetry.lock",
    "pyproject.toml",
    "setup.py",
    "Gemfile.lock",
    "composer.lock",
    "Podfile.lock",
    "Makefile",
    "justfile",
    "ruff.toml",
    ".ruff.toml",
    "mypy.ini",
];

/// What the repository declares about how it is built and checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoInventory {
    /// Package managers named by a manifest or lock file, in rule order.
    pub package_managers: Vec<String>,
    /// Test, lint and typecheck commands the manifests name, in rule order.
    pub test_commands: Vec<String>,
    /// Whether this scan ran at all. `false` means the two lists above are
    /// constants and nothing looked — the distinction the `*_computed` markers
    /// exist to publish.
    pub computed: bool,
    /// Why the scan did not run, when it did not.
    pub unavailable_reason: String,
    /// Manifests past [`MANIFEST_READ_CAP`], named rather than dropped.
    pub refused_oversize: Vec<String>,
    /// Manifests that were there and could not be read, each with the reason.
    ///
    /// The size cap has [`Self::refused_oversize`]; every *other* way a read
    /// fails — a permission denial, a document that is not UTF-8, a file that
    /// changed type between the walk and the read — used to come back as a bare
    /// `None` that the caller could not tell from "the file is absent". The
    /// scan then reported `test_commands` computed, with the manifest's
    /// contents contributing nothing and nothing saying so, while the file's
    /// *existence* still contributed its package manager: an artifact naming
    /// npm and no scripts, which is exactly what a repository with an empty
    /// `scripts` block produces.
    pub unreadable: Vec<String>,
    /// Whether a walk bound stopped the search before the tree was exhausted.
    pub walk_truncated: bool,
    /// Directories opened, so a reader can size the walk that produced this.
    pub directories_visited: usize,
    /// `git` lists tracked and unignored paths; `filesystem` is the fallback
    /// for a directory without Git metadata. Both apply the marker skip policy.
    pub source: String,
    pub entries_examined: usize,
    pub files_total: Option<usize>,
    /// Total failures; `unreadable` is a bounded sample of this many failures.
    pub unreadable_count: usize,
}

impl RepoInventory {
    /// Complete within the declared marker policy, not complete code coverage.
    pub fn is_complete(&self) -> bool {
        self.computed
            && !self.walk_truncated
            && self.unreadable_count == 0
            && self.refused_oversize.is_empty()
    }

    /// A scan that did not happen, with the reason attached.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            unavailable_reason: reason.into(),
            ..Self::default()
        }
    }
}

/// Every marker the walk found, and the one content question it answers on the
/// way (does this repository have Python tests).
#[derive(Debug, Default)]
struct Markers {
    top_level: BTreeSet<String>,
    nested: BTreeSet<String>,
    has_python_test: bool,
    truncated: bool,
    directories_visited: usize,
    entries_examined: usize,
    unreadable: Vec<String>,
    unreadable_count: usize,
    source: String,
    files_total: Option<usize>,
}

impl Markers {
    fn record_error(&mut self, root: &Path, path: &Path, error: impl std::fmt::Display) {
        self.unreadable_count += 1;
        if self.unreadable.len() < ERROR_SAMPLE_CAP {
            let relative = path.strip_prefix(root).unwrap_or(path);
            let label = if relative.as_os_str().is_empty() {
                ".".to_string()
            } else {
                relative.display().to_string()
            };
            self.unreadable.push(format!("{label}: {error}"));
        }
    }

    fn record_file(&mut self, relative: &Path) {
        let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        if relative.components().count() == 1 && TOP_LEVEL_MARKERS.contains(&name) {
            self.top_level.insert(name.to_string());
        }
        if NESTED_MARKERS.contains(&name) {
            self.nested.insert(name.to_string());
        }
        if !self.has_python_test && name.ends_with(".py") {
            self.has_python_test =
                devmap_extract::wiring::is_test_path(&relative.to_string_lossy());
        }
    }

    fn top(&self, name: &str) -> bool {
        self.top_level.contains(name)
    }

    /// A marker at the root or anywhere below it.
    fn anywhere(&self, name: &str) -> bool {
        self.nested.contains(name) || self.top_level.contains(name)
    }

    fn any_gradle(&self) -> bool {
        [
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
            "gradlew",
        ]
        .iter()
        .any(|name| self.anywhere(name))
    }
}

/// Whether the walk descends into `name` at `depth`.
fn skip_dir(name: &str, depth: usize) -> bool {
    // Every dot-directory. `.git` alone is larger than most repositories, and
    // `.venv`, `.tox`, `.mypy_cache`, `.devcouncil` and `.devmap` are all
    // output rather than source. A marker file inside a dot-directory is
    // configuration for a tool, not a declaration by the repository.
    if name.starts_with('.') {
        return true;
    }
    if SKIP_DIR_ANY_DEPTH.contains(&name) {
        return true;
    }
    depth == 0 && SKIP_DIR_TOP_LEVEL.contains(&name)
}

/// Find every marker file, bounded in depth and in directories opened.
fn walk_markers(root: &Path) -> Markers {
    walk_markers_with_limits(root, WALK_DIR_CAP, WALK_ENTRY_CAP, WALK_DEADLINE)
}

fn walk_markers_with_limits(
    root: &Path,
    dir_cap: usize,
    entry_cap: usize,
    deadline: Duration,
) -> Markers {
    let mut found = Markers {
        source: "filesystem".into(),
        ..Markers::default()
    };
    let started = Instant::now();
    let mut stack = vec![(root.to_path_buf(), 0)];
    'walk: while let Some((directory, depth)) = stack.pop() {
        if found.directories_visited >= dir_cap || started.elapsed() >= deadline {
            found.truncated = true;
            break;
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                found.record_error(root, &directory, error);
                continue;
            }
        };
        found.directories_visited += 1;
        for entry in entries {
            if found.entries_examined >= entry_cap || started.elapsed() >= deadline {
                found.truncated = true;
                break 'walk;
            }
            found.entries_examined += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    found.record_error(root, &directory, error);
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    found.record_error(root, &entry.path(), error);
                    continue;
                }
            };
            if kind.is_dir() {
                if skip_dir(&name, depth) {
                    continue;
                }
                // Account for the pending frontier as well as opened directories.
                if depth + 1 > WALK_DEPTH_CAP || found.directories_visited + stack.len() >= dir_cap
                {
                    found.truncated = true;
                } else {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            let regular = if kind.is_symlink() {
                match std::fs::metadata(entry.path()) {
                    Ok(metadata) => metadata.is_file(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => {
                        found.record_error(root, &entry.path(), error);
                        false
                    }
                }
            } else {
                kind.is_file()
            };
            if regular {
                found.record_file(entry.path().strip_prefix(root).unwrap_or(&entry.path()));
            }
        }
    }
    found
}

/// Git supplies the full depth-independent path list. Reuse its bounded command
/// runner, while preserving this inventory's own marker policy (which includes
/// lockfiles and differs from the source-fingerprint filter).
fn git_markers(root: &Path) -> Result<Markers, String> {
    let mut paths = crate::freshness::ls_files(
        OsStr::new("git"),
        root,
        &["--cached", "--others", "--exclude-standard"],
    )?;
    paths.retain(|path| {
        let segments: Vec<_> = path.split('/').collect();
        !segments
            .iter()
            .take(segments.len().saturating_sub(1))
            .enumerate()
            .any(|(depth, name)| skip_dir(name, depth))
    });
    // Root declarations survive a cap before nested evidence is considered.
    paths.sort_by(|a, b| a.contains('/').cmp(&b.contains('/')).then_with(|| a.cmp(b)));
    paths.dedup();
    let mut found = Markers {
        source: "git".into(),
        files_total: Some(paths.len()),
        truncated: paths.len() > GIT_FILE_CAP,
        ..Markers::default()
    };
    let started = Instant::now();
    for relative in paths.iter().take(GIT_FILE_CAP) {
        if started.elapsed() >= WALK_DEADLINE {
            found.truncated = true;
            break;
        }
        found.entries_examined += 1;
        let relative = Path::new(relative);
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            found.record_error(root, relative, "Git returned a non-relative path");
            continue;
        }
        let path = root.join(relative);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => found.record_file(relative),
            Ok(_) => {} // Directories, submodules and special files are not declarations.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {} // Working-tree deletion.
            Err(error) => found.record_error(root, &path, error),
        }
    }
    Ok(found)
}

/// Read a manifest, or record why it could not be read.
///
/// `None` covers "absent" and every kind of refusal; the caller distinguishes
/// them by whether the path landed in one of the two lists, which is the whole
/// point of carrying them.
///
/// Both lists, not just the size one. The size cap was named and every other
/// failure returned a bare `None`: a manifest whose bytes this process may not
/// read, or that is not UTF-8, or that stopped being a regular file between the
/// walk and the read, reached the artifact as "the repository declares no
/// scripts". Only the marker walk decides whether the file *exists*, so its
/// package manager was still published — an answer that names npm and no
/// scripts, indistinguishable from a `package.json` with an empty `scripts`
/// block.
fn read_bounded(
    root: &Path,
    relative: &str,
    refused: &mut Vec<String>,
    unreadable: &mut Vec<String>,
) -> Option<String> {
    let path = root.join(relative);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        // The walk saw this name a moment ago, so "absent" here is itself a
        // failure to read rather than an absence.
        Err(error) => {
            unreadable.push(format!("{relative}: {error}"));
            return None;
        }
    };
    if metadata.len() > MANIFEST_READ_CAP {
        refused.push(relative.to_string());
        return None;
    }
    match devmap_extract::read_source_with_limit(&path, MANIFEST_READ_CAP) {
        Ok(text) => Some(text),
        Err(error) => {
            unreadable.push(format!("{relative}: {error}"));
            None
        }
    }
}

/// Whether a TOML document opens the given table.
///
/// A line scan rather than a parse: this crate has no TOML dependency, and the
/// question is only ever "is this table declared", which a section header
/// answers. A header inside a multi-line string would be a false positive; the
/// consequence is one extra suggested command, which is why this gates only the
/// optional third-party tools and never a claim about what the repository is.
fn declares_table(document: &str, table: &str) -> bool {
    document.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with(table) && trimmed[table.len()..].starts_with([']', '.'])
    })
}

/// Whether a Makefile or justfile declares a `test` target.
fn declares_test_target(document: &str) -> bool {
    document.lines().any(|line| {
        // A target sits in column 0; a recipe line is indented. Anything after
        // the colon is prerequisites, which do not change whether the target
        // exists.
        if line.starts_with([' ', '\t']) {
            return false;
        }
        // A comment that *mentions* a target does not declare one, and it is
        // the shape most likely to be there: `# test: dropped, use pytest`
        // starts in column 0 and carries a colon, which was the whole of the
        // rule. `make test` published from that line is an instruction to an
        // agent to run a target the repository does not have.
        if line.starts_with('#') {
            return false;
        }
        let Some((head, rest)) = line.split_once(':') else {
            return false;
        };
        // `test := pytest -q`, and GNU's `test ::= pytest`, are variable
        // assignments; the name to the left of the colon is not a target. Only
        // an `=` after the colon run disqualifies — `test::` is a double-colon
        // *rule* and does declare one.
        if rest.trim_start_matches(':').starts_with('=') {
            return false;
        }
        head.split_whitespace().any(|word| word == "test")
    })
}

/// The package managers the markers name, in the ported rule order.
fn package_managers(markers: &Markers) -> Vec<String> {
    let mut managers: Vec<String> = Vec::new();
    let push = |name: &str, managers: &mut Vec<String>| {
        if !managers.iter().any(|existing| existing == name) {
            managers.push(name.to_string());
        }
    };
    // `package-lock.json` first, then a bare `package.json` — the Python
    // writer's `elif`, kept: both name npm, and the lock file is the stronger
    // evidence.
    if markers.top("package-lock.json") || markers.top("package.json") {
        push("npm", &mut managers);
    }
    if markers.top("yarn.lock") {
        push("yarn", &mut managers);
    }
    if markers.top("pnpm-lock.yaml") {
        push("pnpm", &mut managers);
    }
    if markers.top("requirements.txt") {
        push("pip", &mut managers);
    }
    // A bare `pyproject.toml` is deliberately absent from this rule.
    // `tests/unit/test_cli_commands.py:91` pins it: every Python project has
    // one, and it names no manager. Only the lock file does.
    if markers.top("uv.lock") {
        push("uv", &mut managers);
    }
    if markers.top("poetry.lock") {
        push("poetry", &mut managers);
    }
    if markers.anywhere("go.mod") || markers.anywhere("go.sum") {
        push("go mod", &mut managers);
    }
    if markers.anywhere("Cargo.toml") {
        push("cargo", &mut managers);
    }
    if markers.anywhere("Package.swift") {
        push("swiftpm", &mut managers);
    }
    if markers.any_gradle() {
        push("gradle", &mut managers);
    }
    if markers.top("Gemfile.lock") {
        push("bundler", &mut managers);
    }
    if markers.top("composer.lock") {
        push("composer", &mut managers);
    }
    if markers.top("Podfile.lock") {
        push("cocoapods", &mut managers);
    }
    managers
}

/// The commands the manifests name, in the ported rule order.
fn test_commands(
    root: &Path,
    markers: &Markers,
    refused: &mut Vec<String>,
    unreadable: &mut Vec<String>,
) -> Vec<String> {
    let mut commands: Vec<String> = Vec::new();
    let push = |command: String, commands: &mut Vec<String>| {
        if !commands.contains(&command) {
            commands.push(command);
        }
    };

    // Node: the scripts the repository actually declares, run through the
    // manager its lock file names.
    if markers.top("package.json") {
        if let Some(text) = read_bounded(root, "package.json", refused, unreadable) {
            let parsed = serde_json::from_str::<serde_json::Value>(&text);
            if let Err(error) = &parsed {
                unreadable.push(format!("package.json: {error}"));
            }
            if let Ok(document) = parsed {
                if !document.is_object()
                    || document
                        .get("scripts")
                        .is_some_and(|scripts| !scripts.is_object())
                {
                    unreadable.push(
                        "package.json: expected an object with an optional scripts object"
                            .to_string(),
                    );
                }
                let manager = if markers.top("pnpm-lock.yaml") {
                    "pnpm"
                } else if markers.top("yarn.lock") {
                    "yarn"
                } else {
                    "npm"
                };
                for key in ["test", "lint", "typecheck", "check", "type-check"] {
                    let Some(script) = document.get("scripts").and_then(|scripts| scripts.get(key))
                    else {
                        continue;
                    };
                    let Some(script) = script.as_str() else {
                        unreadable.push(format!("package.json: scripts.{key} must be a string"));
                        continue;
                    };
                    if script.trim().is_empty() {
                        continue;
                    }
                    // `npm test` is a built-in; every other npm script needs
                    // `run`. yarn and pnpm take the bare name either way.
                    if manager == "npm" && key != "test" {
                        push(format!("npm run {key}"), &mut commands);
                    } else {
                        push(format!("{manager} {key}"), &mut commands);
                    }
                }
            }
        }
    }

    // Python. The ported rule appended `ruff check .` and `mypy .` to every
    // Python project unconditionally; that is a guess about the repository's
    // toolchain rather than a reading of it, and this artifact's whole contract
    // is that a value is evidence. Both are now gated on the repository
    // declaring the tool. `pytest` keeps the original's test-path evidence and
    // gains `[tool.pytest]`, which is a declaration in its own right.
    if markers.top("pyproject.toml") || markers.top("setup.py") {
        let pyproject = if markers.top("pyproject.toml") {
            read_bounded(root, "pyproject.toml", refused, unreadable).unwrap_or_default()
        } else {
            String::new()
        };
        if markers.has_python_test || declares_table(&pyproject, "[tool.pytest") {
            push("pytest".to_string(), &mut commands);
        }
        if declares_table(&pyproject, "[tool.ruff")
            || markers.top("ruff.toml")
            || markers.top(".ruff.toml")
        {
            push("ruff check .".to_string(), &mut commands);
        }
        if declares_table(&pyproject, "[tool.mypy") || markers.top("mypy.ini") {
            push("mypy .".to_string(), &mut commands);
        }
    }

    // Go and Rust keep the original's unconditional pair: `go vet` and `cargo
    // clippy` ship with their toolchains, so naming them is not a guess about
    // what the repository installed the way `ruff` and `mypy` are.
    if markers.anywhere("go.mod") {
        push("go test ./...".to_string(), &mut commands);
        push("go vet ./...".to_string(), &mut commands);
    }
    if markers.anywhere("Cargo.toml") {
        push("cargo test".to_string(), &mut commands);
        push("cargo clippy".to_string(), &mut commands);
    }
    if markers.anywhere("Package.swift") {
        push("swift test".to_string(), &mut commands);
    }
    if markers.anywhere("gradlew") {
        push("./gradlew test".to_string(), &mut commands);
    } else if markers.any_gradle() {
        push("gradle test".to_string(), &mut commands);
    }

    // Task runners, which the Python writer did not read at all: a repository
    // whose real entry point is `make test` was reported as having none.
    if markers.top("Makefile") {
        if let Some(text) = read_bounded(root, "Makefile", refused, unreadable) {
            if declares_test_target(&text) {
                push("make test".to_string(), &mut commands);
            }
        }
    }
    if markers.top("justfile") {
        if let Some(text) = read_bounded(root, "justfile", refused, unreadable) {
            if declares_test_target(&text) {
                push("just test".to_string(), &mut commands);
            }
        }
    }
    commands
}

/// Read the repository's own account of how it is built and checked.
pub fn scan(root: &Path) -> RepoInventory {
    if !root.is_dir() {
        return RepoInventory::unavailable(format!(
            "repository root {} is not a readable directory",
            root.display()
        ));
    }
    let markers = if root.join(".git").exists() {
        match git_markers(root) {
            Ok(markers) => markers,
            Err(reason) => return RepoInventory::unavailable(reason),
        }
    } else {
        walk_markers(root)
    };
    let mut refused: Vec<String> = Vec::new();
    let mut unreadable = markers.unreadable.clone();
    let package_managers = package_managers(&markers);
    let test_commands = test_commands(root, &markers, &mut refused, &mut unreadable);
    refused.sort();
    refused.dedup();
    let unreadable_count = markers.unreadable_count + unreadable.len() - markers.unreadable.len();
    unreadable.sort();
    unreadable.dedup();
    unreadable.truncate(ERROR_SAMPLE_CAP);
    RepoInventory {
        package_managers,
        test_commands,
        computed: true,
        unavailable_reason: String::new(),
        refused_oversize: refused,
        unreadable,
        walk_truncated: markers.truncated,
        directories_visited: markers.directories_visited,
        source: markers.source,
        entries_examined: markers.entries_examined,
        files_total: markers.files_total,
        unreadable_count,
    }
}

/// One bounded `git log`, and the per-file commit counts it yields.
///
/// The kernel already shells out to `git` for `HEAD` (`freshness::git_head`,
/// and `devmap-store`'s deadline-bearing variant); this is the same subprocess
/// discipline applied to the one other question only history can answer.
///
/// Everything about the invocation is bounded: a date window, a commit cap, an
/// output cap and a wall-clock deadline. A repository with no commits, no
/// `git`, or a `git` that stalls produces `computed: false` with the reason
/// attached — never an empty map presented as a computed answer.
pub fn churn(root: &Path) -> FileChurn {
    churn_with_program(OsStr::new("git"), root)
}

/// [`churn`] with the program named, so a test can stand a script in for it.
#[doc(hidden)]
pub fn churn_with_program(program: &OsStr, root: &Path) -> FileChurn {
    let mut command = devmap_extract::subprocess::git_with_program(program, root);
    command.args([
        "log",
        &format!("--since={CHURN_SINCE}"),
        &format!("--max-count={CHURN_COMMIT_CAP}"),
        "--name-only",
        "--no-renames",
        "--pretty=format:",
        // Paths NUL-terminated and unquoted, so a non-ASCII path arrives as
        // the bytes it is rather than as a C-escaped rendering that would
        // never match an extraction's `file_path`.
        "-z",
    ]);
    let bounds = Bounds {
        deadline: CHURN_DEADLINE,
        stdout_cap: CHURN_OUTPUT_CAP,
        stderr_cap: 4096,
    };
    let captured = match run_bounded(&mut command, bounds) {
        Ok(captured) => captured,
        Err(Failure::Deadline { .. }) => {
            return FileChurn::unavailable(format!(
                "git log exceeded {CHURN_DEADLINE:?} and was killed"
            ))
        }
        Err(failure) => return FileChurn::unavailable(format!("could not run git log: {failure}")),
    };
    if !captured.status.success() {
        // git said why — "not a git repository", "does not have any commits
        // yet" — and that is the reason, verbatim, rather than a guess at it.
        let said = captured.stderr_trimmed();
        return FileChurn::unavailable(format!(
            "git log exited {}: {}",
            captured.status.code().unwrap_or(-1),
            if said.is_empty() {
                "no commits, or not a git repository"
            } else {
                said.as_str()
            }
        ));
    }

    let text = captured.stdout_lossy();
    let mut commits_by_path: BTreeMap<String, u32> = BTreeMap::new();
    // Commits, counted so the *commit* cap can be seen at all.
    //
    // Of this invocation's four bounds it is the only one that leaves no trace
    // in the bytes: `--max-count` makes git stop emitting and exit zero, and on
    // a real 5,200-commit repository the whole capped listing is 38 KB against
    // an 8 MB `CHURN_OUTPUT_CAP`, so `stdout_truncated` is `false` and the
    // window was still cut.
    //
    // `-z --name-only --pretty=format:` frames the output as one block per
    // commit — each of its paths NUL-terminated — with the blocks joined by one
    // more NUL. A stream of `f` files across `n` commits therefore carries
    // `f + n - 1` NULs and splits into `f + n` entries, of which `f` are
    // non-empty: every commit contributes exactly one empty entry, whether it
    // named files, named none (an empty commit), or is a merge, which
    // `--name-only` gives no paths for. Measured against git 2.50.1.
    let mut commits_seen = 0usize;
    for entry in text.split('\0') {
        if entry.is_empty() {
            commits_seen += 1;
            continue;
        }
        let path = entry.trim().replace('\\', "/");
        if path.is_empty() {
            continue;
        }
        *commits_by_path.entry(path).or_insert(0) += 1;
    }
    // At exactly the cap the window may or may not have been cut — git stops
    // without saying which — so this reports it as cut. Over-reporting a bound
    // is the safe direction: `truncated` reaching a reader as `false` when
    // history was dropped is the failure this exists to prevent, and the
    // opposite costs one repository in `CHURN_COMMIT_CAP` an accurate flag.
    let commit_cap_reached = commits_seen >= CHURN_COMMIT_CAP;
    if commits_by_path.is_empty() {
        // A repository whose commits in the window touched no file, or none
        // in the window at all: git answered, and the answer was empty.
        return FileChurn::unavailable(
            "git log named no files: no commits in the churn window".to_string(),
        );
    }
    FileChurn {
        commits_by_path,
        computed: true,
        unavailable_reason: String::new(),
        truncated: captured.stdout_truncated || commit_cap_reached,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_pyproject_names_no_manager() {
        let mut markers = Markers::default();
        markers.top_level.insert("pyproject.toml".to_string());
        assert!(package_managers(&markers).is_empty());
    }

    #[test]
    fn a_lock_file_names_its_manager() {
        let mut markers = Markers::default();
        markers.top_level.insert("pyproject.toml".to_string());
        markers.top_level.insert("uv.lock".to_string());
        assert_eq!(package_managers(&markers), vec!["uv".to_string()]);
    }

    #[test]
    fn a_nested_cargo_toml_still_names_cargo() {
        let mut markers = Markers::default();
        markers.nested.insert("Cargo.toml".to_string());
        assert_eq!(package_managers(&markers), vec!["cargo".to_string()]);
    }

    #[test]
    fn a_lock_file_below_the_root_is_not_the_repositorys() {
        // Only `TOP_LEVEL_MARKERS` at depth 0 reach `top_level`, so a fixture's
        // `uv.lock` cannot make the whole repository a uv project.
        let mut markers = Markers::default();
        markers.nested.insert("uv.lock".to_string());
        assert!(package_managers(&markers).is_empty());
    }

    #[test]
    fn table_headers_match_the_table_and_its_subtables() {
        assert!(declares_table(
            "[tool.pytest.ini_options]\n",
            "[tool.pytest"
        ));
        assert!(declares_table("[tool.ruff]\n", "[tool.ruff"));
        assert!(declares_table("  [tool.mypy]  \n", "[tool.mypy"));
        // Not a prefix match on an unrelated table.
        assert!(!declares_table("[tool.pytest_asyncio]\n", "[tool.pytest"));
        assert!(!declares_table("[project]\n", "[tool.ruff"));
    }

    #[test]
    fn a_makefile_target_is_read_from_column_zero_only() {
        assert!(declares_test_target("test:\n\tpytest\n"));
        assert!(declares_test_target("test: lint\n\tpytest\n"));
        assert!(declares_test_target("lint test:\n\tpytest\n"));
        // A recipe line mentioning `test:` is not a target.
        assert!(!declares_test_target("all:\n\techo test: nope\n"));
        assert!(!declares_test_target("lint:\n\truff check .\n"));
    }

    /// A line that only *mentions* a `test` target does not declare one, and
    /// `test_commands` is an artifact of what the repository declares.
    ///
    /// Both of these sit in column 0 and both carry a colon, which was the
    /// whole of the rule. A repository whose Makefile says `# test: removed,
    /// use pytest` was published as declaring `make test`, under
    /// `test_commands_computed: true` — an instruction to an agent to run a
    /// target that does not exist.
    #[test]
    fn a_mention_of_test_is_not_a_declaration_of_it() {
        assert!(
            !declares_test_target("# test: dropped in 2024, use pytest\nall:\n\techo hi\n"),
            "a comment is not a target"
        );
        assert!(
            !declares_test_target("  # test: indented comment\nall:\n"),
            "nor is an indented one"
        );
        assert!(
            !declares_test_target("test := pytest -q\nall:\n\t$(test)\n"),
            "`:=` is an assignment; the name to its left is a variable"
        );
        assert!(
            !declares_test_target("test ::= pytest\n"),
            "and so is GNU's `::=`"
        );
        // Still targets, and the reason the rule cannot simply demand a bare
        // `name:` — `::` is a double-colon rule and `test:` may carry
        // prerequisites.
        assert!(declares_test_target("test:: \n\tpytest\n"));
        assert!(declares_test_target("test: build lint\n\tpytest\n"));
        // A recipe that is a comment is still a recipe, not a target.
        assert!(!declares_test_target("all:\n\t# test: nope\n"));
    }

    #[test]
    fn build_output_directories_are_skipped_where_build_tools_put_them() {
        assert!(skip_dir("target", 3), "cargo output at any depth");
        assert!(skip_dir("node_modules", 5));
        assert!(skip_dir(".git", 0));
        assert!(skip_dir("build", 0), "top-level build output");
        // `freshness.rs`'s rule: a source directory named `build` below the
        // root is real source and must not be dropped.
        assert!(!skip_dir("build", 1));
        assert!(!skip_dir("src", 0));
    }

    #[test]
    fn a_symlinked_lock_file_is_still_a_declaration() {
        // A monorepo whose `uv.lock` is a link into a shared config directory
        // declares uv exactly as much as one that stores the bytes in place.
        let root = std::env::temp_dir().join(format!(
            "devmap-inventory-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("shared")).expect("create fixture");
        std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"x\"\n")
            .expect("write pyproject");
        std::fs::write(root.join("shared/uv.lock"), "version = 1\n").expect("write lock");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("shared/uv.lock"), root.join("uv.lock"))
            .expect("link the lock file");
        #[cfg(not(unix))]
        std::fs::copy(root.join("shared/uv.lock"), root.join("uv.lock")).expect("copy");

        let inventory = scan(&root);
        assert!(inventory.computed);
        assert!(
            inventory.package_managers.iter().any(|name| name == "uv"),
            "a linked uv.lock is still this repository's lock file: {:?}",
            inventory.package_managers
        );

        // A dangling link is not a file and must not be counted, and must not
        // stop the walk either.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("gone/go.mod"), root.join("go.mod"))
                .expect("link a missing file");
            let after = scan(&root);
            assert!(
                !after.package_managers.iter().any(|name| name == "go mod"),
                "a dangling go.mod link declares nothing: {:?}",
                after.package_managers
            );
            assert!(after.package_managers.iter().any(|name| name == "uv"));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_absent_root_is_unavailable_rather_than_empty() {
        let inventory = scan(Path::new("/definitely/not/a/repository/root"));
        assert!(!inventory.computed);
        assert!(!inventory.unavailable_reason.is_empty());
        assert!(inventory.package_managers.is_empty());
    }

    #[test]
    fn fallback_entry_frontier_and_deadline_limits_are_honest() {
        let root =
            std::env::temp_dir().join(format!("devmap-inventory-limits-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for n in 0..128 {
            std::fs::create_dir(root.join(format!("d{n:03}"))).unwrap();
        }
        let entries = walk_markers_with_limits(&root, 1000, 17, Duration::from_secs(5));
        let frontier = walk_markers_with_limits(&root, 9, 1000, Duration::from_secs(5));
        let deadline = walk_markers_with_limits(&root, 1000, 1000, Duration::ZERO);
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(entries.entries_examined, 17);
        assert!(entries.truncated);
        assert_eq!(frontier.directories_visited, 9);
        assert!(frontier.truncated);
        assert_eq!(deadline.directories_visited, 0);
        assert!(deadline.truncated);
    }

    #[test]
    fn inventory_error_samples_preserve_the_total() {
        let mut markers = Markers::default();
        for n in 0..1000 {
            markers.record_error(
                Path::new("/repo"),
                &Path::new("/repo").join(format!("d{n}")),
                "denied",
            );
        }
        assert_eq!(markers.unreadable_count, 1000);
        assert_eq!(markers.unreadable.len(), ERROR_SAMPLE_CAP);
    }
}

#[cfg(test)]
#[path = "tests/inventory_reader.rs"]
mod reader_tests;
