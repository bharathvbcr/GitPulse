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
//! But DevCouncil is still here, and 38 Python modules under `src/devcouncil/`
//! read `.devcouncil/repo_map.json`. Writing the map somewhere else would strand
//! every one of them silently — they would read a stale file and report it fresh,
//! which is the exact failure mode this codebase spends its comments guarding
//! against.
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

/// Absolute-or-relative path to the store for `root`.
pub fn store_path(root: impl AsRef<Path>) -> PathBuf {
    state_dir(root).join(STORE_RELPATH)
}

/// Path to `repo_map.json` for `root`.
pub fn repo_map_path(root: impl AsRef<Path>) -> PathBuf {
    state_dir(root).join(REPO_MAP_RELPATH)
}

/// Path to `code_graph.json` for `root`.
pub fn code_graph_path(root: impl AsRef<Path>) -> PathBuf {
    state_dir(root).join(CODE_GRAPH_RELPATH)
}

/// Path to the interned `code_graph.compact.json` for `root`.
pub fn compact_code_graph_path(root: impl AsRef<Path>) -> PathBuf {
    state_dir(root).join(CODE_GRAPH_COMPACT_RELPATH)
}

/// Path to the content-hash memo for `root`.
pub fn content_cache_path(root: impl AsRef<Path>) -> PathBuf {
    state_dir(root).join(CONTENT_CACHE_RELPATH)
}

/// Path to the workspace registry for `root`.
pub fn workspace_path(root: impl AsRef<Path>) -> PathBuf {
    state_dir(root).join(WORKSPACE_RELPATH)
}

/// Path to the emitted Claude Code plugin bundle for `root`.
pub fn plugin_dir(root: impl AsRef<Path>) -> PathBuf {
    state_dir(root).join(PLUGIN_RELPATH)
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
        let dir = resolve_state_dir(root, None, dirs(&[]));
        for path in [
            store_path(root),
            repo_map_path(root),
            code_graph_path(root),
            compact_code_graph_path(root),
            content_cache_path(root),
            workspace_path(root),
            plugin_dir(root),
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
}
