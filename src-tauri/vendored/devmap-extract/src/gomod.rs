//! Go module manifests (`go.mod`) collected alongside source, not as source.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::DiscoverySkipReason;

/// One `go.mod` in the indexed tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GoModule {
    pub prefix: String,
    /// Directory containing this `go.mod`, repo-relative with `/` separators.
    /// Empty string means the collect root.
    pub dir: String,
    /// `(module_path, replace_target)` in declaration order. Relative targets
    /// are stored as written; resolution joins them with `dir`.
    pub replaces: Vec<(String, String)>,
}

pub fn parse_go_mod(rel_path: &str, source: &str) -> Option<GoModule> {
    let dir = Path::new(rel_path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .filter(|parent| !parent.is_empty() && parent != ".")
        .unwrap_or_default();
    let mut prefix = None;
    let mut replaces = Vec::new();
    let mut in_replace_block = false;

    for raw in source.lines() {
        let line = strip_go_line_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if in_replace_block {
            if line.starts_with(')') {
                in_replace_block = false;
                continue;
            }
            if let Some(replace) = parse_replace_spec(&line) {
                replaces.push(replace);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("module ") {
            let rest = unquote(rest.trim());
            if !rest.is_empty() && !rest.starts_with('(') {
                prefix = Some(rest);
            }
            continue;
        }
        if line.starts_with("replace (") {
            in_replace_block = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("replace ") {
            if let Some(replace) = parse_replace_spec(rest) {
                replaces.push(replace);
            }
        }
    }
    Some(GoModule {
        prefix: prefix?,
        dir,
        replaces,
    })
}

fn strip_go_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(index) => &line[..index],
        None => line,
    }
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('`')
        .trim()
        .to_string()
}

fn parse_replace_spec(spec: &str) -> Option<(String, String)> {
    let (left, right) = spec.split_once("=>")?;
    let module = left.split_whitespace().next().map(unquote)?;
    let target = right.split_whitespace().next().map(unquote)?;
    if module.is_empty() || target.is_empty() {
        return None;
    }
    Some((module, target))
}

/// What one manifest read came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManifestRead {
    /// Nothing at the path: a package nobody installed, a `references` entry
    /// naming a directory with no config. Not a hole.
    Absent,
    /// There, and passed over the way discovery passes over the same shape:
    /// not a regular file, or bytes past the source ceiling. Not a hole either —
    /// `DiscoverySkipReason::Oversized` is not a refusal, and this agrees.
    Skipped,
    Read(String),
    /// There, and not read: the reason it could not be.
    Refused(String),
}

/// Read one manifest under `root`, through the one reader that keeps a read
/// inside the repository.
///
/// Both manifest readers used `fs::read_to_string`, which follows every link it
/// meets. A `go.mod` symlinked out of the repository was read and its module
/// indexed, and a tsconfig `extends` chain through a linked package directory
/// read whatever config sat at the far end — bytes the repository does not
/// contain, put into a graph that claims to describe it, which is the line
/// `candidate_kind` draws for source. `safe_fs::read_repo_source` is that line
/// for a read by path: the canonical target must stay under the root (a linked
/// *ancestor* included, which a final-component check cannot see), the open
/// does not follow links and does not block on a FIFO, a non-regular file is
/// refused, and the size is bounded.
pub(crate) fn read_manifest(root: &Path, rel_path: &str, byte_limit: u64) -> ManifestRead {
    // A directory, a FIFO, a socket — or a link to one: not a file a config
    // could be read from, and stepped over exactly as discovery steps over the
    // same shape (`CandidateKind::Other`). A lookup that fails falls through,
    // so a dangling link or one in a loop is refused below rather than passed.
    if fs::metadata(root.join(rel_path)).is_ok_and(|meta| !meta.is_file()) {
        return ManifestRead::Skipped;
    }
    match crate::safe_fs::read_repo_source(Some(root), rel_path, byte_limit) {
        Ok(source) => ManifestRead::Read(source),
        Err(error) if crate::tsconfig::is_absent(&error) => ManifestRead::Absent,
        Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => ManifestRead::Skipped,
        Err(error) => ManifestRead::Refused(error.to_string()),
    }
}

/// Collect every non-gitignored `go.mod` under `root`.
pub fn collect_go_modules(root: &Path) -> anyhow::Result<Vec<GoModule>> {
    collect_project_manifests(root).map(|manifests| manifests.go_modules)
}

/// The project manifests the resolver reads beside source: Go modules and
/// TypeScript / JavaScript project configs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectManifests {
    pub go_modules: Vec<GoModule>,
    pub ts_projects: Vec<crate::tsconfig::TsProject>,
    /// Every manifest this pass opened or tried to, repo-relative. The read
    /// verdict on each of these is this pass's own, current answer.
    pub examined: BTreeSet<String>,
    /// The manifests among `examined` that exist and could not be read, by
    /// path.
    ///
    /// Recorded because "could not read" and "nothing there" must not share a
    /// representation. A `go.mod` or `tsconfig.json` this pass skips takes
    /// its module prefix or its `paths` with it, every import that named one
    /// is filed `External`, and nothing downstream could tell that from a
    /// repository with no such mapping. Discovery already refuses the
    /// manifests it walks to; this pass also reads ones discovery never
    /// sees — an `extends` target under `node_modules/`, a `go.mod` under a
    /// directory the default index excludes — and those had no record
    /// anywhere.
    pub refusals: Vec<(String, DiscoverySkipReason)>,
    /// Every `Cargo.toml` package this pass read, with the names a `use` may
    /// spell its library by.
    ///
    /// A crate's directory name is not its name. GitPulse's library lives in
    /// `src-tauri`, is packaged as `gitpulse` and built as `[lib] name =
    /// "gitpulse_lib"`, so `use gitpulse_lib::…` named an in-repo crate the
    /// resolver only knew by its directory and filed every such import
    /// `External` — "expected, not worth acting on" — about code it indexes.
    pub rust_crates: Vec<RustCrateManifest>,
}

/// One `Cargo.toml` package: the directory it governs and the crate names its
/// library answers to — `[package] name` with `-` read as `_`, and `[lib]
/// name` when set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RustCrateManifest {
    /// Repo-relative directory holding the `Cargo.toml`; empty at the root.
    pub dir: String,
    pub names: Vec<String>,
}

/// The library names a `Cargo.toml` declares, or `None` when it declares no
/// package (a virtual workspace manifest) or does not parse.
pub fn parse_cargo_crate_names(source: &str) -> Option<Vec<String>> {
    let manifest: toml::Table = source.parse().ok()?;
    let package = manifest.get("package")?.as_table()?;
    let mut names = Vec::new();
    if let Some(name) = package.get("name").and_then(toml::Value::as_str) {
        names.push(name.replace('-', "_"));
    }
    if let Some(name) = manifest
        .get("lib")
        .and_then(toml::Value::as_table)
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
    {
        names.push(name.replace('-', "_"));
    }
    names.sort();
    names.dedup();
    (!names.is_empty()).then_some(names)
}

impl ProjectManifests {
    /// Whether this pass has just read `path` cleanly and is the only pass
    /// that could have refused it.
    ///
    /// The incremental drain carries the previous generation's refusals
    /// forward and drops only what it re-decided. A manifest discovery does
    /// not walk is re-decided by every manifest pass and by nothing else, so
    /// without this a repaired one never leaves the inventory. A path
    /// discovery admits is left to discovery's verdict, which the drain
    /// re-decides on its own terms.
    pub fn repaired(&self, path: &str) -> bool {
        self.examined.contains(path)
            && !crate::languages::is_indexable_source(path)
            && !self.refusals.iter().any(|(refused, _)| refused == path)
    }
}

/// What the parallel manifest walk gathers, under one lock.
#[derive(Default)]
struct Found {
    modules: Vec<GoModule>,
    rust_crates: Vec<RustCrateManifest>,
    ts_configs: Vec<String>,
    examined: BTreeSet<String>,
    refusals: Vec<(String, DiscoverySkipReason)>,
    /// A walk error about the pass as a whole: the root, or nothing at all.
    fatal: Option<ignore::Error>,
}

/// One walked entry: a manifest is collected, read or refused; anything else
/// is passed over.
fn visit_manifest_entry(root: &Path, entry: &ignore::DirEntry, found: &std::sync::Mutex<Found>) {
    let path = entry.path();
    // The name first: it costs nothing, and it rules out all but a handful of
    // the entries. Asking `path.is_file()` first was a `stat` of every file in
    // the tree — on this repository 5,800 of them, on the warm build path, to
    // find a few dozen manifests.
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let is_go_mod = file_name == "go.mod";
    let is_cargo_toml = file_name == "Cargo.toml";
    if !is_go_mod && !is_cargo_toml && !crate::tsconfig::is_ts_config_name(file_name) {
        return;
    }
    // A link is a candidate whatever it points at. `is_file()` follows it and
    // answered `false` for a dangling one, so a `go.mod` naming nothing was
    // passed over like a directory; `read_manifest` decides what any link is
    // worth, under the same containment rule as source. The walk's own file
    // type is the entry's, unfollowed, and free.
    if !entry
        .file_type()
        .is_some_and(|kind| kind.is_file() || kind.is_symlink())
    {
        return;
    }
    let Some(rel_str) = path.strip_prefix(root).ok().and_then(Path::to_str) else {
        return;
    };
    let rel_str = rel_str.replace('\\', "/");
    if is_cargo_toml {
        let read = read_manifest(root, &rel_str, crate::MAX_SOURCE_BYTES);
        let mut found = crate::recover_lock(found);
        found.examined.insert(rel_str.clone());
        match read {
            ManifestRead::Read(source) => {
                if let Some(names) = parse_cargo_crate_names(&source) {
                    let dir = rel_str
                        .rsplit_once('/')
                        .map(|(dir, _)| dir.to_string())
                        .unwrap_or_default();
                    found.rust_crates.push(RustCrateManifest { dir, names });
                }
            }
            ManifestRead::Absent | ManifestRead::Skipped => {}
            ManifestRead::Refused(reason) => found
                .refusals
                .push((rel_str, DiscoverySkipReason::Unreadable { reason })),
        }
        return;
    }
    if !is_go_mod {
        crate::recover_lock(found).ts_configs.push(rel_str);
        return;
    }
    // Read outside the lock: this is the part worth running in parallel.
    let read = read_manifest(root, &rel_str, crate::MAX_SOURCE_BYTES);
    let mut found = crate::recover_lock(found);
    found.examined.insert(rel_str.clone());
    match read {
        // A `go.mod` that reads and names no module is as lost as one that
        // cannot be read: its prefix is what maps every import under it, and
        // dropping it without a record filed those imports `External` with
        // nothing saying why.
        ManifestRead::Read(source) => match parse_go_mod(&rel_str, &source) {
            Some(module) => found.modules.push(module),
            None => found.refusals.push((
                rel_str,
                DiscoverySkipReason::Unreadable {
                    reason: "could not be parsed: no `module` directive names a module path"
                        .to_string(),
                },
            )),
        },
        ManifestRead::Absent | ManifestRead::Skipped => {}
        // The error's own text, as discovery records the same failure, so a
        // `go.mod` both passes refuse is one row with one reason.
        ManifestRead::Refused(reason) => found
            .refusals
            .push((rel_str, DiscoverySkipReason::Unreadable { reason })),
    }
}

/// One walk for every manifest kind, so a build pays for the tree once: this
/// is already the second walk of it on the build path.
pub fn collect_project_manifests(root: &Path) -> anyhow::Result<ProjectManifests> {
    let walk_root = root.to_path_buf();
    let marker_errors = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
        String,
        String,
    >::new()));
    let marker_error_writer = std::sync::Arc::clone(&marker_errors);
    // The same rule sources as discovery, from the same owner: a manifest the
    // cold walk skips must not be read by this one, or a `.ignore`d `go.mod`
    // would map modules for sources the graph never holds.
    let walker = crate::walk_builder(root)
        .filter_entry(move |entry| {
            if entry.path() == walk_root || !entry.file_type().is_some_and(|kind| kind.is_dir()) {
                return true;
            }
            match crate::is_cache_directory(entry.path()) {
                Ok(tagged) => !tagged,
                Err(error) => {
                    // Let the walk report a directory it cannot open through
                    // the below-root refusal path below. Discovery already
                    // accounts for that hole.
                    if fs::read_dir(entry.path()).is_err() {
                        return true;
                    }
                    // An unreadable marker inside a readable directory is the
                    // directory refused, as discovery refuses it — not the
                    // whole pass failed. Failing it here failed every daemon
                    // drain of the tree, while `devmap build`'s discovery had
                    // already recorded the same directory and carried on.
                    // Worded as discovery words it, so the two passes' rows
                    // for one directory are one row.
                    if let Ok(relative) = entry.path().strip_prefix(&walk_root) {
                        crate::recover_lock(&marker_error_writer).insert(
                            relative.to_string_lossy().replace('\\', "/"),
                            format!("CACHEDIR.TAG could not be examined: {error}"),
                        );
                    }
                    false
                }
            }
        })
        // In parallel: this walk is on the warm build path now, where it was
        // a third of a no-change build single-threaded. Nothing below depends
        // on visit order — every collection is sorted, or is a set, before it
        // leaves this function.
        .build_parallel();
    let found = std::sync::Mutex::new(Found::default());
    walker.run(|| {
        Box::new(|result| {
            let entry = match result {
                Ok(entry) => entry,
                // Stepped over when it names a path below the root, for the
                // reason `collect_sources_with_report` gives — and one more of
                // its own. This is the *second* walk of the same tree on the
                // build path, so fixing only the first left `devmap build`
                // exiting 1 on a `chmod 000` directory exactly as before,
                // after discovery had already recorded that directory as a
                // refusal and carried on.
                //
                // Nothing is recorded here: this walk has no report to record
                // into, and the loss is already disclosed by the discovery pass
                // over the same tree. A second entry for the same directory
                // would double count it.
                Err(error) => {
                    if crate::walk_error_path(&error)
                        .and_then(|path| path.strip_prefix(root).ok())
                        .is_none_or(|relative| relative.as_os_str().is_empty())
                    {
                        // Names the root, or names nothing: about the pass
                        // rather than about an entry under it, and not
                        // steppable.
                        crate::recover_lock(&found).fatal.get_or_insert(error);
                        return ignore::WalkState::Quit;
                    }
                    return ignore::WalkState::Continue;
                }
            };
            visit_manifest_entry(root, &entry, &found);
            ignore::WalkState::Continue
        })
    });
    let Found {
        mut modules,
        mut rust_crates,
        mut ts_configs,
        mut examined,
        mut refusals,
        fatal,
    } = found
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(error) = fatal {
        return Err(error.into());
    }
    refusals.extend(
        std::mem::take(&mut *crate::recover_lock(&marker_errors))
            .into_iter()
            .map(|(path, reason)| (path, DiscoverySkipReason::Unreadable { reason })),
    );
    modules.sort_by(|left, right| {
        right
            .prefix
            .len()
            .cmp(&left.prefix.len())
            .then_with(|| left.prefix.cmp(&right.prefix))
            .then_with(|| left.dir.cmp(&right.dir))
    });
    ts_configs.sort();
    rust_crates.sort_by(|left, right| left.dir.cmp(&right.dir));
    let ts = crate::tsconfig::build_ts_projects(root, &ts_configs);
    examined.extend(ts.examined);
    refusals.extend(
        ts.unreadable
            .into_iter()
            .map(|(path, reason)| (path, DiscoverySkipReason::Unreadable { reason })),
    );
    refusals.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(ProjectManifests {
        go_modules: modules,
        ts_projects: ts.projects,
        examined,
        refusals,
        rust_crates,
    })
}

/// Directories (repo-relative, `/`-separated) that hold an indexed `.go` file
/// **and** a `.go` file the index does not hold.
///
/// Go compiles every `.go` file in a package directory, indexed or not — a
/// gitignored `*.pb.go`, a mock generator's output — so a package whose
/// directory holds one the index never read has declarations no rung can
/// see. The resolver's unexported-selector rung proves "exactly one method of
/// this name in the package" from the index, and that proof needs the index
/// to *be* the package. This is the disk half of that check, kept out of the
/// resolver so resolution stays a pure function of its inputs.
///
/// Reads only the directories that already hold an indexed Go file, one level
/// each, so the cost is one `read_dir` per Go package. A directory that
/// cannot be listed is reported too: unreadable is not evidence of complete.
pub fn go_dirs_with_unindexed_files<'a>(
    root: &Path,
    indexed: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut by_dir: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for path in indexed {
        let path = path.replace('\\', "/");
        if !path.ends_with(".go") {
            continue;
        }
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (dir.to_string(), name.to_string()),
            None => (".".to_string(), path.clone()),
        };
        by_dir.entry(dir).or_default().insert(name);
    }
    let mut incomplete = Vec::new();
    for (dir, names) in by_dir {
        let listing = if dir == "." {
            root.to_path_buf()
        } else {
            root.join(&dir)
        };
        let Ok(entries) = fs::read_dir(&listing) else {
            incomplete.push(dir);
            continue;
        };
        let unread = entries.flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with(".go")
                && !names.contains(&name)
                && entry.file_type().is_ok_and(|kind| !kind.is_dir())
        });
        if unread {
            incomplete.push(dir);
        }
    }
    incomplete
}

/// Git worktree root containing `start`, if any. `.git` may be a directory or
/// a gitlink file (worktrees).
pub fn git_worktree_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory whose `.go` files the index holds only in part is reported;
    /// one it holds wholly is not; a directory named `x.go` is not a file; and
    /// a directory that cannot be listed is reported rather than trusted.
    #[test]
    fn a_package_directory_with_an_unindexed_go_file_is_reported() {
        let root = std::env::temp_dir().join(format!("devmap-go-unindexed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["whole", "partial", "dirnamed", "dirnamed/fake.go"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "top.go",
            "whole/a.go",
            "whole/a_test.go",
            "whole/README.md",
            "partial/a.go",
            "partial/api.pb.go",
            "dirnamed/a.go",
        ] {
            fs::write(root.join(file), "package p\n").unwrap();
        }
        let indexed = [
            "top.go",
            "whole/a.go",
            "whole/a_test.go",
            "partial/a.go",
            "dirnamed/a.go",
            "gone/a.go",
        ];
        let reported = go_dirs_with_unindexed_files(&root, indexed);
        assert_eq!(
            reported,
            vec!["gone".to_string(), "partial".to_string()],
            "`partial` holds an unread api.pb.go; `gone` cannot be listed"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn module_discovery_prunes_build_caches_like_source_discovery() {
        let root = std::env::temp_dir().join(format!("devmap-module-cache-{}", std::process::id()));
        fs::create_dir_all(root.join("output/deep")).unwrap();
        fs::write(root.join("go.mod"), "module example.com/source\n").unwrap();
        fs::write(
            root.join("output/CACHEDIR.TAG"),
            crate::CACHEDIR_TAG_SIGNATURE,
        )
        .unwrap();
        fs::write(
            root.join("output/deep/go.mod"),
            "module example.com/cache\n",
        )
        .unwrap();
        let modules = collect_go_modules(&root).unwrap();
        assert_eq!(
            modules
                .iter()
                .map(|module| module.prefix.as_str())
                .collect::<Vec<_>>(),
            ["example.com/source"]
        );
        // Explicitly choosing the cache itself as root must still work.
        assert_eq!(collect_go_modules(&root.join("output")).unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_module_and_block_replace() {
        let parsed = parse_go_mod(
            "backend/go.mod",
            r#"
module scholarlm/backend/go_orchestrator

go 1.22

replace (
    github.com/acme/lib v1.2.3 => ../lib
    example.com/other => ./third_party/other
)
"#,
        )
        .unwrap();
        assert_eq!(parsed.prefix, "scholarlm/backend/go_orchestrator");
        assert_eq!(parsed.dir, "backend");
        assert_eq!(
            parsed.replaces,
            vec![
                ("github.com/acme/lib".into(), "../lib".into()),
                ("example.com/other".into(), "./third_party/other".into()),
            ]
        );
    }

    #[test]
    fn comments_and_missing_module_are_fail_closed() {
        assert!(parse_go_mod("go.mod", "replace a => b\n").is_none());
        let parsed = parse_go_mod(
            "go.mod",
            "module example.com/app // trailing\nreplace a => b // c\n",
        )
        .unwrap();
        assert_eq!(parsed.prefix, "example.com/app");
        assert_eq!(parsed.replaces, vec![("a".into(), "b".into())]);
    }
}

#[cfg(test)]
mod parse_guard_tests {
    use super::*;

    /// `go.mod` parsing keeps every guard it declares.
    ///
    /// Mutation testing flipped each conjunction in `parse_go_mod` and
    /// `parse_replace_spec` without a failure. These feed Go import resolution:
    /// a wrong module prefix or a half-parsed `replace` sends imports to the
    /// wrong files, and since both source and target exist nothing errors — the
    /// graph is simply wrong.
    #[test]
    fn module_prefix_rejects_empty_and_block_forms() {
        let parsed = parse_go_mod("go.mod", "module example.com/app\n").unwrap();
        assert_eq!(parsed.prefix, "example.com/app");

        // `module (` opens a block; the literal `(` is not a module path.
        let block = parse_go_mod("go.mod", "module (\n");
        assert!(
            block.is_none() || block.unwrap().prefix.is_empty(),
            "a `module (` block header must not be taken as the module path"
        );

        // An empty module line yields no prefix.
        let empty = parse_go_mod("go.mod", "module \n");
        assert!(empty.is_none() || empty.unwrap().prefix.is_empty());
    }

    /// The module directory is repo-relative, and `.`/empty normalise away.
    #[test]
    fn module_directory_is_normalised() {
        assert_eq!(parse_go_mod("go.mod", "module m\n").unwrap().dir, "");
        assert_eq!(
            parse_go_mod("sub/pkg/go.mod", "module m\n").unwrap().dir,
            "sub/pkg"
        );

        // A `./`-prefixed path has parent `.`, which must normalise to the
        // collect root and not to a literal `.` component. `dir` is joined with
        // relative replace targets, so a stray `.` silently produces `./x`
        // paths that match no indexed file.
        assert_eq!(
            parse_go_mod("./go.mod", "module m\n").unwrap().dir,
            "",
            "`.` must normalise to the collect root"
        );
        // Only the bare `.` parent is normalised; deeper paths are kept as
        // written, which is what the collect walker produces.
        assert_eq!(
            parse_go_mod("./sub/go.mod", "module m\n").unwrap().dir,
            "./sub"
        );
    }

    /// A `replace` needs both halves; either side empty yields nothing.
    ///
    /// `module.is_empty() || target.is_empty()` was mutable to `&&`, which
    /// accepts a replace with one empty half — rewriting an import to the empty
    /// path, or rewriting the empty path to somewhere real.
    #[test]
    fn a_replace_directive_requires_both_halves() {
        let single =
            parse_go_mod("go.mod", "module m\nreplace example.com/a => ./local/a\n").unwrap();
        assert_eq!(
            single.replaces,
            vec![("example.com/a".to_string(), "./local/a".to_string())],
            "a single-line replace must parse both halves"
        );

        // Block form must parse too.
        let block = parse_go_mod(
            "go.mod",
            "module m\nreplace (\n  example.com/b => ./local/b\n)\n",
        )
        .unwrap();
        assert!(
            block
                .replaces
                .iter()
                .any(|(from, to)| from == "example.com/b" && to == "./local/b"),
            "a replace block must parse its entries: {:?}",
            block.replaces
        );

        // A malformed replace contributes nothing rather than half an entry.
        let malformed = parse_go_mod("go.mod", "module m\nreplace example.com/c =>\n").unwrap();
        assert!(
            malformed.replaces.is_empty(),
            "a replace missing its target must be dropped, not half-recorded: {:?}",
            malformed.replaces
        );

        // A quoted empty half parses as a *present but empty* token, so it
        // reaches the emptiness guard rather than the `?` above it. This is the
        // only shape that distinguishes `||` from `&&` there: with `&&` exactly
        // one empty half is accepted, binding an import to the empty path.
        assert_eq!(
            parse_replace_spec(r#""" => ./local/x"#),
            None,
            "an empty module half must be rejected"
        );
        assert_eq!(
            parse_replace_spec(r#"example.com/d => """#),
            None,
            "an empty target half must be rejected"
        );
        assert_eq!(
            parse_replace_spec("example.com/e => ./local/e"),
            Some(("example.com/e".to_string(), "./local/e".to_string())),
            "positive control: a well-formed replace still parses"
        );
    }
}
