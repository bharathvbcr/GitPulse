//! Adopting a ledger that its identity record refused.

use super::*;
use crate::ipc_error::CommandError;

/// Adopts this repository's ledger after the identity record refused it for
/// sitting in another directory than the one it was anchored in: a cross-device
/// move, a restore from backup, a copy whose source is gone. Returns the earlier
/// locations the ledger now accepts as this repository's.
///
/// Only a person can do this. The inspection and the rule (same roots and
/// remotes, no earlier location still a repository) are the ledger's; what this
/// command adds is the approval. The in-page button that reaches it is DOM, so
/// script that can call the IPC bridge could skip it: the approval is an OS
/// dialog drawn from Rust, naming the repository and every earlier location,
/// and the call fails closed when it cannot be shown or is declined. The
/// repository must also be trusted (`validate_repo`). The adoption is itself a
/// ledger row, written with the re-anchor in one transaction.
#[tauri::command(async)]
pub async fn cmd_adopt_ledger(repo_path: String) -> Result<Vec<String>, IpcError> {
    off_thread(move || adopt_ledger_confirmed(&repo_path, &crate::desktop::confirm::native)).await
}

/// The identity the audit row names as the actor: a person, through the OS
/// confirmation, rather than an agent or GitPulse itself.
pub(super) const ADOPTING_ACTOR: &str = "user:os-confirmation";

pub(super) fn adopt_ledger_confirmed(
    repo_path: &str,
    confirm: &dyn Fn(&str, &str) -> bool,
) -> Result<Vec<String>, CommandError> {
    let repo = validate_repo(repo_path)?;
    let here = repo.to_string_lossy().into_owned();
    crate::ledger::bindings::adopt_ledger(&here, ADOPTING_ACTOR, &|earlier| {
        let listed = earlier
            .iter()
            .map(|path| format!("  {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        confirm(
            "Adopt this ledger?",
            &format!(
                "Adopt the activity ledger of the repository at:\n\n  {here}\n\n\
                 as the history of this checkout, taking over the rows recorded under:\n\n{listed}\n\n\
                 Only continue if this checkout is that repository, moved or restored here. \
                 Activity recorded at those locations will be shown as this repository's. \
                 The adoption is written to the ledger."
            ),
        )
    })
    .map_err(|e| e.to_string()).map_err(CommandError::from)
}

#[cfg(test)]
mod ledger_adopt_tests {
    use super::*;

    use crate::ledger;
    use crate::test_support::{git_in, trust_repo};

    fn repo_at(parent: &std::path::Path, name: &str) -> String {
        let path = parent.canonicalize().unwrap().join(name);
        std::fs::create_dir(&path).unwrap();
        git_in(&path, &["init", "-b", "main"]);
        std::fs::write(path.join("seed.txt"), name).unwrap();
        git_in(&path, &["add", "seed.txt"]);
        git_in(&path, &["commit", "-m", "seed"]);
        trust_repo(&path);
        path.to_string_lossy().into_owned()
    }

    /// An untrusted checkout is refused before the ledger is looked at or any
    /// dialog is drawn.
    #[test]
    fn an_untrusted_repository_is_refused_without_asking() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().canonicalize().unwrap().join("untrusted");
        std::fs::create_dir(&path).unwrap();
        git_in(&path, &["init", "-b", "main"]);
        let asked = std::cell::Cell::new(false);
        let out = adopt_ledger_confirmed(&path.to_string_lossy(), &|_, _| {
            asked.set(true);
            true
        });
        assert!(out.is_err(), "{out:?}");
        assert!(
            !asked.get(),
            "the dialog was drawn for an untrusted repository"
        );
    }

    /// A declined dialog adopts nothing, and a repository whose ledger is at
    /// home has nothing to adopt and is never asked about.
    #[test]
    fn a_declined_or_pointless_adoption_writes_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let repo = repo_at(parent.path(), "home");
        ledger::append(ledger::Draft {
            repo_path: repo.clone(),
            action: "git.commit".into(),
            ..Default::default()
        })
        .unwrap();
        let asked = std::cell::Cell::new(false);
        let out = adopt_ledger_confirmed(&repo, &|_, _| {
            asked.set(true);
            true
        });
        assert!(out.unwrap_err().contains("nothing to adopt"));
        assert!(!asked.get(), "nothing to adopt, so nothing to confirm");
        let adopts = ledger::tail(&repo, 0, 100)
            .unwrap()
            .into_iter()
            .filter(|event| event.action == "ledger.adopt")
            .count();
        assert_eq!(adopts, 0);
    }
}
