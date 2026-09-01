//! The stash stack: listing it, and applying/popping/dropping entries safely.
//!
//! Before this module the client could only `stash push` and `stash pop`, and
//! `pop` always took entry 0 sight-unseen. That is the wrong shape for a GUI,
//! for two reasons that are the same reason:
//!
//! * **The stack is shared and it moves.** Every worktree of a repository, and
//!   every other client or agent touching it, pushes onto one stack. An index
//!   the user saw thirty seconds ago can now name someone else's work.
//! * **`pop` and `drop` are destructive and index-addressed.** `git stash drop`
//!   refuses an object id — it only takes `stash@{n}` — so an index that has
//!   shifted silently destroys the wrong entry.
//!
//! Every mutating call therefore carries the object id the caller *believed*
//! that index held. Under the repository mutation lock this module re-resolves
//! the index and refuses when the two disagree, so a stale UI can fail loudly
//! but can never discard work the user never looked at. `apply` accepts an
//! object id directly (git allows it there), so it addresses the exact commit
//! and needs no index at all.

use crate::engine::git_cli::{git_text, validate_repo};
use crate::engine::git_writer::{repo_mutation_lock, validate_oid};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Upper bound on entries returned in one listing. A stash stack this deep is
/// already pathological; the bound keeps a runaway `.git` from pulling an
/// unbounded payload through the IPC boundary.
const MAX_STASH_ENTRIES: usize = 500;

/// One entry on the stash stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashEntry {
    /// Position on the stack right now. Valid only until the stack moves,
    /// which is exactly why `oid` travels beside it.
    pub index: usize,
    /// `stash@{n}` — the only spelling `pop` and `drop` accept.
    pub selector: String,
    /// The stash commit. Stable identity, and the guard against a stale index.
    pub oid: String,
    /// Git's own subject line, e.g. `On main: refactor the parser`.
    pub subject: String,
    /// The user's message with git's `On <branch>: ` / `WIP on <branch>: `
    /// prefix removed, or the whole subject when it carries no prefix.
    pub message: String,
    /// Branch the stash was taken on, when the subject records one.
    pub branch: Option<String>,
    /// Author timestamp, seconds since the epoch.
    pub timestamp: i64,
}

/// What to do with an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StashAction {
    /// Restore the entry's changes and leave it on the stack.
    Apply,
    /// Restore the entry's changes and remove it from the stack.
    Pop,
    /// Remove the entry without restoring anything. Destructive.
    Drop,
}

impl StashAction {
    pub fn label(self) -> &'static str {
        match self {
            StashAction::Apply => "apply",
            StashAction::Pop => "pop",
            StashAction::Drop => "drop",
        }
    }
}

/// Splits git's stash subject into its branch and the human message.
///
/// Git writes `WIP on <branch>: <sha> <subject>` for an automatic message and
/// `On <branch>: <message>` for an explicit one. Anything else is a message in
/// its own right and is returned whole — inventing a branch from an
/// unrecognized shape would attribute a stash to the wrong branch.
fn split_subject(subject: &str) -> (Option<String>, String) {
    for prefix in ["WIP on ", "On "] {
        if let Some(rest) = subject.strip_prefix(prefix) {
            if let Some((branch, message)) = rest.split_once(": ") {
                let branch = branch.trim();
                if !branch.is_empty() {
                    return (Some(branch.to_string()), message.trim().to_string());
                }
            }
        }
    }
    (None, subject.trim().to_string())
}

/// Parses the NUL-separated `stash list` stream into entries.
///
/// The format emits four fields per record and `-z` separates records with NUL
/// as well, so the stream is a flat run of 4-tuples. A trailing partial group
/// (a truncated read) is dropped rather than filled with defaults: half an
/// entry addressed by index is precisely the hazard this module exists to
/// prevent.
fn parse_stash_list(raw: &str) -> Vec<StashEntry> {
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut entries = Vec::new();
    let (groups, _remainder) = fields.as_chunks::<4>();
    for (position, group) in groups.iter().enumerate() {
        if entries.len() >= MAX_STASH_ENTRIES {
            break;
        }
        let (selector, oid, timestamp, subject) = (group[0], group[1], group[2], group[3]);
        let selector = selector.trim();
        let oid = oid.trim();
        if selector.is_empty() || oid.is_empty() {
            continue;
        }
        let (branch, message) = split_subject(subject);
        entries.push(StashEntry {
            // Git lists newest first and `stash@{n}` counts the same way, so
            // the position in this stream IS the index. Parsing it out of the
            // selector instead would trust a string git already told us.
            index: position,
            selector: selector.to_string(),
            oid: oid.to_string(),
            subject: subject.trim().to_string(),
            message,
            branch,
            timestamp: timestamp.trim().parse::<i64>().unwrap_or(0),
        });
    }
    entries
}

/// Lists the stash stack, newest first.
pub fn list(repo_path: &str) -> Result<Vec<StashEntry>, String> {
    let repo = validate_repo(repo_path)?;
    list_in(&repo)
}

fn list_in(repo: &Path) -> Result<Vec<StashEntry>, String> {
    let raw = git_text(
        repo,
        &["stash", "list", "-z", "--format=%gd%x00%H%x00%ct%x00%gs"],
    )?;
    Ok(parse_stash_list(&raw))
}

/// Renders the diff a stash entry would apply.
///
/// Addressed by object id, so a shifting stack cannot make this show the wrong
/// entry — it is a read, and a read of the wrong thing is still wrong.
pub fn show(repo_path: &str, oid: &str) -> Result<String, String> {
    let repo = validate_repo(repo_path)?;
    validate_oid(oid)?;
    // `-u` includes files the stash captured as untracked, which are otherwise
    // invisible in the preview and then appear on apply.
    git_text(repo.as_path(), &["stash", "show", "-p", "-u", oid])
}

/// The argv an action would execute against `selector`.
///
/// Shared by the write gate and the executor so the judged line is the run
/// line. `apply` is addressed by object id (git accepts one there) and the
/// index-only verbs by selector, after the selector has been proven to still
/// hold `oid`.
pub fn action_argv<'a>(action: StashAction, selector: &'a str, oid: &'a str) -> Vec<&'a str> {
    match action {
        StashAction::Apply => vec!["git", "stash", "apply", oid],
        StashAction::Pop => vec!["git", "stash", "pop", selector],
        StashAction::Drop => vec!["git", "stash", "drop", selector],
    }
}

/// Applies, pops, or drops the entry at `index`, which the caller asserts
/// currently holds `expected_oid`.
///
/// `judge` sees the exact argv about to run and is called under the repository
/// lock, after the index has been re-verified — so the gate cannot approve a
/// line computed from a stack that has since moved.
pub fn run_action_with<J, V>(
    repo_path: &str,
    action: StashAction,
    index: usize,
    expected_oid: &str,
    judge: J,
) -> Result<(V, String), String>
where
    J: FnOnce(&[&str]) -> Result<V, String>,
{
    let repo = validate_repo(repo_path)?;
    validate_oid(expected_oid)?;
    let lock = repo_mutation_lock(&repo);
    let _guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Re-read the stack under the lock. Anything the caller believed about it
    // is a claim from before it took the lock.
    let entries = list_in(&repo)?;
    let Some(entry) = entries.iter().find(|e| e.index == index) else {
        return Err(format!(
            "Stash entry {index} no longer exists — the stash stack now holds {} entr{}. \
             Refresh the stash list and try again.",
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        ));
    };
    if !entry.oid.eq_ignore_ascii_case(expected_oid) {
        // The single failure this module exists to prevent: another worktree,
        // client or agent pushed or dropped a stash, every index shifted, and
        // `drop` would now destroy work the user never saw.
        return Err(format!(
            "Stash entry {index} changed since it was listed — it now holds \"{}\". \
             Refresh the stash list and try again.",
            entry.subject
        ));
    }

    let argv = action_argv(action, &entry.selector, &entry.oid);
    let verdict = judge(&argv)?;
    let output = git_text(repo.as_path(), &argv[1..])?;
    Ok((verdict, output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(selector: &str, oid: &str, ts: &str, subject: &str) -> String {
        format!("{selector}\0{oid}\0{ts}\0{subject}")
    }

    #[test]
    fn parses_a_stack_newest_first_with_indices_that_match_selectors() {
        let raw = [
            record("stash@{0}", "aaaa1111", "1700000200", "On main: second"),
            record(
                "stash@{1}",
                "bbbb2222",
                "1700000100",
                "WIP on feature: abc123 first",
            ),
        ]
        .join("\0");
        let entries = parse_stash_list(&raw);
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].index, 0);
        assert_eq!(entries[0].selector, "stash@{0}");
        assert_eq!(entries[0].oid, "aaaa1111");
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[0].message, "second");
        assert_eq!(entries[0].timestamp, 1_700_000_200);

        assert_eq!(entries[1].index, 1);
        assert_eq!(entries[1].branch.as_deref(), Some("feature"));
        assert_eq!(entries[1].message, "abc123 first");
    }

    #[test]
    fn an_empty_stack_lists_as_empty_rather_than_erroring() {
        assert!(parse_stash_list("").is_empty());
        assert!(parse_stash_list("\0").is_empty());
    }

    #[test]
    fn a_truncated_trailing_record_is_dropped_not_filled_in() {
        // Half an entry addressed by index is the exact hazard this module
        // exists to prevent; a defaulted oid would defeat the staleness guard.
        let raw = format!(
            "{}\0{}",
            record("stash@{0}", "aaaa1111", "1700000200", "On main: kept"),
            "stash@{1}\0bbbb2222"
        );
        let entries = parse_stash_list(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].oid, "aaaa1111");
    }

    #[test]
    fn subjects_without_a_recognized_prefix_keep_their_whole_text() {
        // Never invent a branch: attributing a stash to the wrong branch is
        // worse than showing none.
        let (branch, message) = split_subject("a message with: a colon in it");
        assert_eq!(branch, None);
        assert_eq!(message, "a message with: a colon in it");

        let (branch, message) = split_subject("On : empty branch");
        assert_eq!(branch, None);
        assert_eq!(message, "On : empty branch");
    }

    #[test]
    fn a_message_containing_a_colon_survives_branch_extraction() {
        let (branch, message) = split_subject("On main: fix: the parser: again");
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(message, "fix: the parser: again");
    }

    #[test]
    fn a_branch_name_with_slashes_is_preserved() {
        let (branch, message) = split_subject("On feature/auth/oauth2: half-done");
        assert_eq!(branch.as_deref(), Some("feature/auth/oauth2"));
        assert_eq!(message, "half-done");
    }

    #[test]
    fn an_unparseable_timestamp_reads_as_zero_rather_than_failing_the_listing() {
        let raw = record("stash@{0}", "aaaa1111", "not-a-time", "On main: x");
        let entries = parse_stash_list(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].timestamp, 0);
    }

    #[test]
    fn the_listing_is_bounded() {
        let raw: String = (0..MAX_STASH_ENTRIES + 50)
            .map(|i| record(&format!("stash@{{{i}}}"), "aaaa1111", "1", "On main: x"))
            .collect::<Vec<_>>()
            .join("\0");
        assert_eq!(parse_stash_list(&raw).len(), MAX_STASH_ENTRIES);
    }

    #[test]
    fn apply_addresses_the_object_and_the_destructive_verbs_address_the_selector() {
        // `git stash drop <oid>` is rejected by git outright, so the verbs that
        // remove an entry must use the selector — which is only safe because
        // the selector is re-verified against the oid first.
        assert_eq!(
            action_argv(StashAction::Apply, "stash@{2}", "deadbeef"),
            vec!["git", "stash", "apply", "deadbeef"]
        );
        assert_eq!(
            action_argv(StashAction::Pop, "stash@{2}", "deadbeef"),
            vec!["git", "stash", "pop", "stash@{2}"]
        );
        assert_eq!(
            action_argv(StashAction::Drop, "stash@{2}", "deadbeef"),
            vec!["git", "stash", "drop", "stash@{2}"]
        );
    }
}
