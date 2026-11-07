//! The one place that decides where Dev Map keeps its state.
//!
//! Every artifact path used to be a string literal at its point of use —
//! `".devcouncil/codeintel/devmap.sqlite"` in the CLI's clap default,
//! `".devcouncil/graph/code_graph.json"` in `code_graph.rs`,
//! `".devcouncil/cache/content_hashes.json"` in `freshness.rs`,
//! `".devcouncil/workspace.json"` in `workspace.rs`, and the same directory name
//! again in two separate ignore lists. Eleven literals naming one directory is
//! how a rename comes to be applied to ten of them.
//!
//! # Why the directory is resolved rather than fixed
//!
//! `.devcouncil/` is the *orchestrator's* directory. Dev Map is being separated
//! into a product that stands on its own, so a fresh repository that has never
//! heard of DevCouncil should not have an orchestrator's directory created in it
//! by a code-intelligence tool.
//!
//! Compatibility remains for checkouts that already have `.devcouncil/`.
//! Writing the map somewhere else would leave that directory holding a stale
//! `repo_map.json` that still looks like the live one, which is the exact
//! failure mode this codebase spends its comments guarding against.
//!
//! So the directory is *resolved*, in this order:
//!
//! 1. `$DEVMAP_HOME` — an explicit answer always wins, and is the only way to
//!    put state outside the repository.
//! 2. `<root>/.devmap/` when it already exists — the standalone layout.
//! 3. `<root>/.devcouncil/` when it already exists — a DevCouncil repository
//!    keeps its layout, so both writers keep agreeing about one file.
//! 4. `<root>/.devmap/` — a repository with neither gets the standalone layout.
//!
//! Rule 3 is the compatibility rule and it is deliberately *not* a fallback that
//! fires after a failed read: it is checked before anything is created, so a
//! DevCouncil repository never ends up with two state directories, one of them
//! half-written. Rule 2 precedes it so that a repository which has migrated —
//! `.devmap/` present, `.devcouncil/` still on disk for the orchestrator — reads
//! and writes the migrated location.
//!
//! # What is *not* resolved
//!
//! Nothing here reads a config file. The resolution is a pure function of the
//! environment and what exists on disk, because it has to answer identically in
//! the CLI, the daemon, a hook invoked with no shell, and the MCP server — none
//! of which share a config loader.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The standalone state directory: what a repository gets when nothing else
/// applies.
pub const STATE_DIR: &str = ".devmap";

/// The state directory Dev Map shares with DevCouncil when a repository already
/// has one. Kept because DevCouncil's Python consumers read artifacts out of it
/// by name.
pub const LEGACY_STATE_DIR: &str = ".devcouncil";

/// Environment override for the state directory. An absolute path is used as
/// given; a relative one is joined onto the repository root.
pub const HOME_ENV: &str = "DEVMAP_HOME";

/// Both directory names, for the ignore lists that must never index generated
/// state. Order is not significant.
pub const STATE_DIR_NAMES: &[&str] = &[STATE_DIR, LEGACY_STATE_DIR];

/// The store, relative to the state directory.
pub const STORE_RELPATH: &str = "codeintel/devmap.sqlite";
/// The repository map, relative to the state directory.
pub const REPO_MAP_RELPATH: &str = "repo_map.json";
/// The symbol-level graph companion, relative to the state directory.
pub const CODE_GRAPH_RELPATH: &str = "graph/code_graph.json";
/// The interned encoding of the same graph, relative to the state directory.
pub const CODE_GRAPH_COMPACT_RELPATH: &str = "graph/code_graph.compact.json";
/// The content-hash memo, relative to the state directory.
pub const CONTENT_CACHE_RELPATH: &str = "cache/content_hashes.json";
/// The `devmap`-binary digest memo, relative to the state directory.
///
/// Beside `content_hashes.json` in `cache/` because it is the same kind of
/// thing — a stat-keyed memo of a digest, advisory in both directions — and
/// separate from it because the two answer about different subjects with
/// different algorithms: repository sources under BLAKE2b at relative paths,
/// executables under SHA-256 at absolute ones. Merging them would put paths
/// from outside the repository into a file the Python side reads.
pub const BINARY_DIGEST_CACHE_RELPATH: &str = "cache/binary_digests.json";
/// The workspace registry, relative to the state directory.
pub const WORKSPACE_RELPATH: &str = "workspace.json";
/// The emitted Claude Code plugin bundle, relative to the state directory.
///
/// `devmap-plugin`, not `claude-plugin`: DevCouncil's own emitter
/// (`integrations/claude_assets.py`) writes a bundle *and* a single-repo
/// marketplace to `<state>/claude-plugin/.claude-plugin/marketplace.json`, and
/// this emitter used to default to the same path. The two marketplaces have
/// different names — `devcouncil-local` and `devmap-local` — so whichever ran
/// last silently replaced the other's registration.
pub const PLUGIN_RELPATH: &str = "devmap-plugin";
/// StackedStore lease metadata, relative to the state directory.
pub const STACK_LEASE_RELPATH: &str = "codeintel/stack_lease.json";
/// StackedStore worktree-local delta SQLite database, relative to the state directory.
pub const STACK_DELTA_RELPATH: &str = "codeintel/delta.sqlite";

/// Resolve the state directory for `root`, reading `$DEVMAP_HOME` and the
/// filesystem.
///
/// See the module docs for the precedence. `root` is used as given: a relative
/// root yields relative artifact paths, which is what the CLI's CWD-relative
/// defaults need.
pub fn state_dir(root: impl AsRef<Path>) -> PathBuf {
    resolve_state_dir(root.as_ref(), std::env::var_os(HOME_ENV), |path| {
        path.is_dir()
    })
}

/// The resolution itself, with the environment and the filesystem passed in.
///
/// Split out so the precedence can be tested without setting process-wide
/// environment variables — which no test can do safely while other tests run in
/// the same process.
pub fn resolve_state_dir(
    root: &Path,
    home: Option<OsString>,
    is_dir: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(home) = home {
        // An empty value is a set-but-unset variable, which is far more often a
        // broken shell expansion than a request to put state at the repository
        // root. Treating it as unset costs nothing and cannot silently scatter
        // artifacts across the tree.
        if !home.is_empty() {
            let candidate = PathBuf::from(home);
            return if candidate.is_absolute() {
                candidate
            } else {
                root.join(candidate)
            };
        }
    }

    let standalone = root.join(STATE_DIR);
    if is_dir(&standalone) {
        return standalone;
    }

    let legacy = root.join(LEGACY_STATE_DIR);
    if is_dir(&legacy) {
        return legacy;
    }

    standalone
}

/// `base` joined with a `/`-separated relative path, one component at a time,
/// so the result uses the platform's separator throughout. A plain join kept
/// the `/` on Windows (`.\.devmap\codeintel/devmap.sqlite` in `doctor`).
fn join_rel(base: PathBuf, rel: &str) -> PathBuf {
    rel.split('/').fold(base, |path, part| path.join(part))
}

/// Absolute-or-relative path to the store for `root`.
pub fn store_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), STORE_RELPATH)
}

/// Path to `repo_map.json` for `root`.
pub fn repo_map_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), REPO_MAP_RELPATH)
}

/// Path to `code_graph.json` for `root`.
pub fn code_graph_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), CODE_GRAPH_RELPATH)
}

/// Path to the interned `code_graph.compact.json` for `root`.
pub fn compact_code_graph_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), CODE_GRAPH_COMPACT_RELPATH)
}

/// Path to the content-hash memo for `root`.
pub fn content_cache_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), CONTENT_CACHE_RELPATH)
}

/// Path to the `devmap`-binary digest memo for `root`.
pub fn binary_digest_cache_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), BINARY_DIGEST_CACHE_RELPATH)
}

/// Path to the workspace registry for `root`.
pub fn workspace_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), WORKSPACE_RELPATH)
}

/// Path to the emitted Claude Code plugin bundle for `root`.
pub fn plugin_dir(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), PLUGIN_RELPATH)
}

/// Path to `stack_lease.json` for `root`.
pub fn stack_lease_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), STACK_LEASE_RELPATH)
}

/// Path to `delta.sqlite` for `root`.
pub fn stack_delta_path(root: impl AsRef<Path>) -> PathBuf {
    join_rel(state_dir(root), STACK_DELTA_RELPATH)
}

/// Retired Phase 1 worktree base-store pointer relative to the state directory.
pub const LEGACY_BASE_STORE_REF_RELPATH: &str = "codeintel/base_store_ref.json";

/// Locate a retired `base_store_ref.json` pointer file in `root` across either
/// `.devmap`, `.devcouncil`, or `$DEVMAP_HOME`, if present.
pub fn find_legacy_base_store_ref(root: impl AsRef<Path>) -> Option<PathBuf> {
    let root = root.as_ref();
    let candidates = [
        join_rel(state_dir(root), LEGACY_BASE_STORE_REF_RELPATH),
        join_rel(root.join(STATE_DIR), LEGACY_BASE_STORE_REF_RELPATH),
        join_rel(root.join(LEGACY_STATE_DIR), LEGACY_BASE_STORE_REF_RELPATH),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Path to the `<store>.csr` mmap sidecar accompanying `store`.
pub fn csr_path_for_store(store: impl AsRef<Path>) -> PathBuf {
    let mut name = store.as_ref().as_os_str().to_owned();
    name.push(".csr");
    PathBuf::from(name)
}

/// Largest `.git` gitlink or `commondir` / `gitdir` pointer file accepted.
const MAX_GITLINK_BYTES: u64 = 4096;

fn read_bounded_regular_file(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity((meta.len() as usize).min(max_bytes as usize));
    file.take(max_bytes + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > max_bytes {
        return None;
    }
    String::from_utf8(buf).ok()
}

/// Canonical common `.git` directory of `root` when `root` is either a main
/// checkout (with a real `.git` directory) or a linked worktree bidirectionally
/// registered under `<common>/worktrees/<name>/gitdir`.
///
/// Rejects symlinked `.git` entries and forged `.git` gitlink files whose
/// target `<common>/worktrees/<name>/gitdir` does not canonicalize back to
/// `<root>/.git`.
pub fn registered_common_git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let meta = std::fs::symlink_metadata(&dot_git).ok()?;
    if meta.is_dir() {
        let head = dot_git.join("HEAD");
        std::fs::symlink_metadata(&head).ok()?;
        return dot_git.canonicalize().ok();
    }
    if !meta.is_file() {
        return None;
    }
    let text = read_bounded_regular_file(&dot_git, MAX_GITLINK_BYTES)?;
    let gitdir_spec = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|spec| !spec.is_empty() && !spec.contains('\0'))?;
    let gitdir_raw = root.join(gitdir_spec);
    let common_spec = read_bounded_regular_file(&gitdir_raw.join("commondir"), MAX_GITLINK_BYTES)?;
    let common_spec = common_spec.trim();
    if common_spec.is_empty() || common_spec.contains('\0') {
        return None;
    }
    let common = gitdir_raw.join(common_spec).canonicalize().ok()?;
    let gitdir = gitdir_raw.canonicalize().ok()?;
    let worktrees_dir = gitdir.parent()?;
    if worktrees_dir.file_name()? != "worktrees" || worktrees_dir.parent()? != common {
        return None;
    }
    let registered_spec = read_bounded_regular_file(&gitdir.join("gitdir"), MAX_GITLINK_BYTES)?;
    let registered_spec = registered_spec.trim();
    if registered_spec.is_empty() || registered_spec.contains('\0') {
        return None;
    }
    let registered = gitdir.join(registered_spec).canonicalize().ok()?;
    let canon_gitlink = dot_git.canonicalize().ok()?;
    let canon_root = root.canonicalize().ok()?;
    if registered != canon_gitlink || registered.parent()? != canon_root {
        return None;
    }
    Some(common)
}

/// The main checkout of the linked worktree at `root`, when git has
/// bidirectionally registered `root` and the common repository is non-bare.
pub fn main_checkout_of(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if !std::fs::symlink_metadata(&dot_git).is_ok_and(|meta| meta.is_file()) {
        return None;
    }
    let common = registered_common_git_dir(root)?;
    if common.file_name()? != ".git" {
        return None;
    }
    let main = common.parent()?.canonicalize().ok()?;
    let canon_root = root.canonicalize().ok()?;
    (main != canon_root).then_some(main)
}

/// True when `a` and `b` are valid, bidirectionally registered checkouts (main
/// or linked worktrees) of the same Git repository.
pub fn same_registered_repo(a: &Path, b: &Path) -> bool {
    match (registered_common_git_dir(a), registered_common_git_dir(b)) {
        (Some(ca), Some(cb)) => ca == cb,
        _ => false,
    }
}

/// All live, bidirectionally verified worktree roots sharing `common_git_dir`
/// (including the main checkout when `common_git_dir` is `<main>/.git`).
pub fn registered_worktrees_of_common_dir(common_git_dir: &Path) -> Vec<PathBuf> {
    let Ok(common) = common_git_dir.canonicalize() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if common.file_name().and_then(|n| n.to_str()) == Some(".git") {
        if let Some(main) = common.parent() {
            if registered_common_git_dir(main).as_deref() == Some(common.as_path()) {
                if let Ok(canon) = main.canonicalize() {
                    out.push(canon);
                }
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
        for entry in entries.flatten() {
            let wt_meta_dir = entry.path();
            let Some(reg_spec) =
                read_bounded_regular_file(&wt_meta_dir.join("gitdir"), MAX_GITLINK_BYTES)
            else {
                continue;
            };
            let reg_spec = reg_spec.trim();
            if reg_spec.is_empty() || reg_spec.contains('\0') {
                continue;
            }
            let Ok(gitlink) = wt_meta_dir.join(reg_spec).canonicalize() else {
                continue;
            };
            let Some(wt_root) = gitlink.parent() else {
                continue;
            };
            if registered_common_git_dir(wt_root).as_deref() == Some(common.as_path()) {
                let wt_buf = wt_root.to_path_buf();
                if !out.contains(&wt_buf) {
                    out.push(wt_buf);
                }
            }
        }
    }
    out
}

/// The repository root a store path belongs to — the inverse of [`store_path`].
///
/// [`STORE_RELPATH`] has two components under the state directory, so the root
/// is three levels up. Kept here, beside the forward direction, because the two
/// have to move together: callers used to open-code the three `parent()` calls,
/// which silently answers the *grandparent's* directory the moment the store
/// layout gains or loses a level.
///
/// `None` unless the path is `<root>/<state>/codeintel/devmap.sqlite` — a bare
/// `devmap.sqlite`, a store under `$DEVMAP_HOME`, or any other location names
/// no repository, and the caller uses the root it was invoked for. This used to
/// answer the grandparent of *any* path: `--db /x/elsewhere/devmap.sqlite
/// workspace add` wrote the registry under `/x`, a directory that was nobody's
/// repository.
pub fn repo_root_from_store(store: impl AsRef<Path>) -> Option<PathBuf> {
    let store = store.as_ref();
    let layout = Path::new(STORE_RELPATH);
    let codeintel = store.parent()?;
    let state = codeintel.parent()?;
    if store.file_name() != layout.file_name()
        || codeintel.file_name() != layout.parent().and_then(Path::file_name)
    {
        return None;
    }
    if !state
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_state_dir_name)
    {
        return None;
    }
    state.parent().map(Path::to_path_buf)
}

/// True when `name` is one of the state directory names.
///
/// Used by the ignore lists. Both names are always excluded regardless of which
/// one this repository resolved to, because a repository mid-migration has both
/// on disk and indexing either would put generated state in the graph.
pub fn is_state_dir_name(name: &str) -> bool {
    STATE_DIR_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filesystem stub: the listed paths are directories, nothing else is.
    fn dirs<'a>(present: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |path: &Path| present.iter().any(|p| path == Path::new(p))
    }

    #[test]
    fn a_repository_with_neither_directory_gets_the_standalone_layout() {
        let got = resolve_state_dir(Path::new("/repo"), None, dirs(&[]));
        assert_eq!(got, Path::new("/repo/.devmap"));
    }

    #[test]
    fn a_devcouncil_repository_keeps_its_directory() {
        // The compatibility rule. DevCouncil's Python consumers read
        // `.devcouncil/repo_map.json` by name; resolving elsewhere would leave
        // them reading a stale file and reporting it fresh.
        let got = resolve_state_dir(Path::new("/repo"), None, dirs(&["/repo/.devcouncil"]));
        assert_eq!(got, Path::new("/repo/.devcouncil"));
    }

    #[test]
    fn a_migrated_repository_prefers_the_standalone_directory() {
        // Both on disk: `.devmap/` wins, so a repository that has migrated does
        // not silently fall back to the orchestrator's directory just because
        // the orchestrator still keeps its own state there.
        let got = resolve_state_dir(
            Path::new("/repo"),
            None,
            dirs(&["/repo/.devmap", "/repo/.devcouncil"]),
        );
        assert_eq!(got, Path::new("/repo/.devmap"));
    }

    #[test]
    fn an_absolute_home_override_wins_over_both() {
        let got = resolve_state_dir(
            Path::new("/repo"),
            Some(OsString::from("/var/cache/devmap")),
            dirs(&["/repo/.devmap", "/repo/.devcouncil"]),
        );
        assert_eq!(got, Path::new("/var/cache/devmap"));
    }

    #[test]
    fn a_relative_home_override_is_joined_onto_the_root() {
        let got = resolve_state_dir(
            Path::new("/repo"),
            Some(OsString::from("build/index")),
            dirs(&["/repo/.devcouncil"]),
        );
        assert_eq!(got, Path::new("/repo/build/index"));
    }

    #[test]
    fn an_empty_home_override_is_treated_as_unset() {
        // `DEVMAP_HOME=` is a broken expansion far more often than a request to
        // put state at the repository root, and honouring it would scatter
        // `repo_map.json` and a `codeintel/` directory across the tree.
        let got = resolve_state_dir(
            Path::new("/repo"),
            Some(OsString::new()),
            dirs(&["/repo/.devcouncil"]),
        );
        assert_eq!(got, Path::new("/repo/.devcouncil"));
    }

    #[test]
    fn a_relative_root_yields_relative_artifact_paths() {
        // The CLI's defaults are CWD-relative and must stay that way: a caller
        // that passes `.` gets `.devmap/...`, not an absolutised path that would
        // read differently in a `--json` payload.
        let got = resolve_state_dir(Path::new("."), None, dirs(&[]));
        assert_eq!(got, Path::new("./.devmap"));
    }

    #[test]
    fn both_directory_names_are_always_ignorable() {
        // A repository mid-migration has both on disk. Indexing either would put
        // generated state — a sqlite store, a 20 MB graph — into the graph.
        assert!(is_state_dir_name(".devmap"));
        assert!(is_state_dir_name(".devcouncil"));
        assert!(!is_state_dir_name(".git"));
    }

    #[test]
    fn artifact_paths_all_hang_off_one_resolved_directory() {
        // The point of the module: one decision, applied everywhere. If these
        // ever disagree, an artifact is being written outside the resolved
        // directory and a consumer will read a different generation than the
        // one the build wrote.
        let root = Path::new("/repo");
        // Use the same environment and filesystem as the artifact calls. A
        // container can have a real /repo with an existing legacy directory.
        let dir = state_dir(root);
        for path in [
            store_path(root),
            repo_map_path(root),
            code_graph_path(root),
            compact_code_graph_path(root),
            content_cache_path(root),
            binary_digest_cache_path(root),
            workspace_path(root),
            plugin_dir(root),
            stack_lease_path(root),
            stack_delta_path(root),
        ] {
            assert!(
                path.starts_with(&dir),
                "{} escapes the resolved state directory {}",
                path.display(),
                dir.display()
            );
        }
    }

    #[test]
    fn the_store_path_round_trips_through_its_inverse() {
        // The property that matters: whatever `store_path` builds,
        // `repo_root_from_store` must take apart. Three open-coded `parent()`
        // calls at a call site cannot state this, and go wrong silently the
        // moment `STORE_RELPATH` gains or loses a level.
        for root in [Path::new("/repo"), Path::new("/a/b/c/deep")] {
            let store = store_path(root);
            assert_eq!(repo_root_from_store(&store).as_deref(), Some(root));
        }
    }

    /// `--db /x/elsewhere/devmap.sqlite workspace add` wrote the registry under
    /// `/x`: the inverse answered the grandparent of *any* path, and the CLI
    /// took that for a repository. A store that is not at
    /// `<root>/<state>/codeintel/devmap.sqlite` names no repository.
    #[test]
    fn a_store_outside_the_standard_layout_names_no_repository() {
        for store in [
            "/x/elsewhere/devmap.sqlite",
            "/x/.devmap/devmap.sqlite",
            "/x/.devcouncil/codeintel/index.sqlite",
            "/x/codeintel/devmap.sqlite",
        ] {
            assert_eq!(
                repo_root_from_store(Path::new(store)),
                None,
                "{store}: not the store layout, so no repository can be read off it"
            );
        }
    }

    #[test]
    fn a_store_path_with_no_repository_above_it_is_reported_as_such() {
        // Better than handing back `/` and letting a workspace be registered
        // against the filesystem root.
        assert_eq!(repo_root_from_store(Path::new("devmap.sqlite")), None);
    }

    #[test]
    fn the_plugin_bundle_does_not_collide_with_devcouncils() {
        // DevCouncil's emitter owns `<state>/claude-plugin/`, marketplace name
        // `devcouncil-local`. Sharing the directory meant sharing
        // `.claude-plugin/marketplace.json`, and the second writer replaced the
        // first's registration with no warning.
        assert_ne!(PLUGIN_RELPATH, "claude-plugin");
    }

    #[test]
    fn same_registered_repo_accepts_linked_worktrees_and_rejects_gitlink_forgery() {
        let base = std::env::temp_dir().join(format!(
            "devmap-paths-reg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let main = base.join("main");
        let wt1 = base.join("wt1");
        let wt2 = base.join("wt2");
        let forged_main = base.join("forged-main");
        let forged_lane = base.join("forged-lane");
        let unrelated = base.join("unrelated");

        std::fs::create_dir_all(main.join(".git/worktrees/wt1")).unwrap();
        std::fs::create_dir_all(main.join(".git/worktrees/wt2")).unwrap();
        std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        for (name, dir) in [("wt1", &wt1), ("wt2", &wt2)] {
            std::fs::create_dir_all(dir).unwrap();
            let wt_gitdir = main.join(".git/worktrees").join(name);
            std::fs::write(
                dir.join(".git"),
                format!("gitdir: {}\n", wt_gitdir.display()),
            )
            .unwrap();
            std::fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();
            std::fs::write(
                wt_gitdir.join("gitdir"),
                format!("{}\n", dir.join(".git").display()),
            )
            .unwrap();
        }

        std::fs::create_dir_all(&forged_main).unwrap();
        std::fs::write(
            forged_main.join(".git"),
            format!("gitdir: {}\n", main.join(".git").display()),
        )
        .unwrap();

        std::fs::create_dir_all(&forged_lane).unwrap();
        std::fs::write(
            forged_lane.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt1").display()),
        )
        .unwrap();

        std::fs::create_dir_all(unrelated.join(".git")).unwrap();
        std::fs::write(unrelated.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        assert!(same_registered_repo(&main, &main));
        assert!(same_registered_repo(&main, &wt1));
        assert!(same_registered_repo(&wt1, &main));
        assert!(same_registered_repo(&wt1, &wt2));
        assert_eq!(
            main_checkout_of(&wt1).unwrap(),
            main.canonicalize().unwrap()
        );
        assert_eq!(main_checkout_of(&main), None);

        assert!(!same_registered_repo(&main, &forged_main));
        assert!(!same_registered_repo(&main, &forged_lane));
        assert!(!same_registered_repo(&wt1, &forged_lane));
        assert!(!same_registered_repo(&main, &unrelated));
        assert_eq!(main_checkout_of(&forged_lane), None);

        #[cfg(unix)]
        {
            let symlink_forge = base.join("symlink-forge");
            std::fs::create_dir_all(&symlink_forge).unwrap();
            std::os::unix::fs::symlink(wt1.join(".git"), symlink_forge.join(".git")).unwrap();
            assert!(!same_registered_repo(&main, &symlink_forge));
            assert_eq!(main_checkout_of(&symlink_forge), None);
        }

        let common = registered_common_git_dir(&wt1).unwrap();
        let all = registered_worktrees_of_common_dir(&common);
        assert_eq!(all.len(), 3);

        let _ = std::fs::remove_dir_all(&base);
    }
}
