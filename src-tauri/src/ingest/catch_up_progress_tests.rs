//! Why the ledger's catch-up never finished on a repository with two dozen
//! worktrees: one five-second budget for the address, the reflogs and the
//! transcripts, retried every ten seconds for ever, from the reactive class.

use super::*;
use crate::engine::process::{spawn_log, Admission};
use crate::test_support::repo_with_worktrees;

fn plain(path: &std::path::Path) -> String {
    crate::engine::process::canonicalize_plain(path)
        .expect("canonical")
        .to_string_lossy()
        .into_owned()
}

/// Runs `f` with the transcript root pointed at an empty corpus.
fn with_empty_transcripts<T>(f: impl FnOnce() -> T) -> T {
    let corpus = tempfile::tempdir().expect("corpus");
    let serial = crate::test_support::env::env_serial();
    let _env =
        crate::test_support::env::bind_env(&serial).set("GITPULSE_TRANSCRIPT_ROOT", corpus.path());
    f()
}

/// Every git child the gate started in `dirs`, with the class it was admitted
/// under.
fn classed_git_children(dirs: &[std::path::PathBuf]) -> Vec<(Vec<String>, Admission)> {
    dirs.iter()
        .flat_map(|dir| spawn_log::classed_spawns_in(dir))
        .collect()
}

/// A background catch-up must not spend the reactive budget the interface
/// reads from: the app asks for it on every tab switch, and a pass over a large
/// family starts dozens of children.
#[test]
fn a_catch_up_pass_starts_its_git_children_in_the_background_class() {
    let (main, _parent, linked) = repo_with_worktrees(2);
    let anchor = plain(main.path());
    let dirs: Vec<std::path::PathBuf> = std::iter::once(std::path::PathBuf::from(&anchor))
        .chain(linked.iter().cloned())
        .collect();

    let out = with_empty_transcripts(|| catch_up_coalesced(&anchor));
    assert!(out.error.is_empty(), "{out:?}");

    let children = classed_git_children(&dirs);
    let reflogs: Vec<_> = children
        .iter()
        .filter(|(argv, _)| argv.iter().any(|arg| arg == "reflog"))
        .collect();
    assert!(!reflogs.is_empty(), "the pass read no reflog: {children:?}");
    let not_background: Vec<_> = children
        .iter()
        .filter(|(_, class)| *class != Admission::Background)
        .collect();
    assert!(
        not_background.is_empty(),
        "the pass admitted children outside the background class: {not_background:?}"
    );
}
