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
use crate::engine::git_reader::DiffPayload;
use crate::engine::git_writer::{repo_mutation_lock, validate_oid};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The closing sentence of both stale-stack refusals.
pub(crate) const STALE_STACK_HINT: &str = "Refresh the stash list and try again.";

/// A stale-stack refusal, typed where it is written (see `crate::ipc_error`):
/// the panel refreshes on the `StaleStash` kind and never reads the sentence.
fn stale(message: String) -> CommandError {
    CommandError::typed(crate::ipc_error::IpcErrorKind::StaleStash, message)
}

/// Upper bound on entries returned in one listing. A stash stack this deep is
/// already pathological; the bound keeps a runaway `.git` from pulling an
/// unbounded payload through the IPC boundary.
const MAX_STASH_ENTRIES: usize = 500;
const MAX_STASH_LIST_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashList {
    pub entries: Vec<StashEntry>,
    /// More entries exist, or a byte/record limit prevented a complete read.
    pub truncated: bool,
}

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
fn parse_stash_list(raw: &str) -> StashList {
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut entries = Vec::new();
    let (groups, remainder) = fields.as_chunks::<4>();
    let mut truncated = groups.len() > MAX_STASH_ENTRIES || remainder.iter().any(|s| !s.is_empty());
    for (position, group) in groups.iter().enumerate() {
        if entries.len() >= MAX_STASH_ENTRIES {
            break;
        }
        let (selector, oid, timestamp, subject) = (group[0], group[1], group[2], group[3]);
        let selector = selector.trim();
        let oid = oid.trim();
        if selector.is_empty() || oid.is_empty() {
            truncated = true;
            continue;
        }
        if selector != format!("stash@{{{position}}}") || validate_oid(oid).is_err() {
            truncated = true;
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
    StashList { entries, truncated }
}

/// Lists the stash stack, newest first.
pub fn list(repo_path: &str) -> Result<StashList, CommandError> {
    let repo = validate_repo(repo_path)?;
    list_in(&repo)
}

fn list_in(repo: &Path) -> Result<StashList, CommandError> {
    let (raw, incomplete) = crate::engine::ref_cache::git_text_capped(
        repo,
        &["refs/stash", "logs/refs/stash"],
        &[
            "stash",
            "list",
            "-z",
            "-n",
            &(MAX_STASH_ENTRIES + 1).to_string(),
            "--format=%gd%x00%H%x00%ct%x00%gs",
        ],
        MAX_STASH_LIST_BYTES,
    )?;
    let mut report = parse_stash_list(&raw);
    report.truncated |= incomplete.is_some();
    Ok(report)
}

/// Renders the diff a stash entry would apply.
///
/// Addressed by object id, so a shifting stack cannot make this show the wrong
/// entry — it is a read, and a read of the wrong thing is still wrong.
pub fn show(repo_path: &str, oid: &str) -> Result<DiffPayload, CommandError> {
    let repo = validate_repo(repo_path)?;
    validate_oid(oid)?;
    // `-u` includes files the stash captured as untracked, which are otherwise
    // invisible in the preview and then appear on apply.
    // Budgeted like every other diff: a stash of a large refactor is the same
    // 40 MB payload a commit diff is, and it lands in the same viewer.
    let (text, incomplete) = crate::engine::git_cli::git_text_capped(
        repo.as_path(),
        &["stash", "show", "-p", "-u", oid],
        crate::engine::budget::MAX_DIFF_BYTES,
    )?;
    Ok(DiffPayload {
        text: if incomplete.is_some() {
            crate::engine::budget::drop_partial_last_line(text)
        } else {
            text
        },
        truncated: incomplete.is_some(),
        truncation_reason: incomplete.map(|reason| reason.describe()),
    })
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
) -> Result<(V, String), CommandError>
where
    J: FnOnce(&[&str]) -> Result<V, CommandError>,
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
    let Some(entry) = entries.entries.iter().find(|e| e.index == index) else {
        return Err(stale(format!(
            "Stash entry {index} no longer exists — the stash stack now holds {} entr{}. \
             {STALE_STACK_HINT}",
            entries.entries.len(),
            if entries.entries.len() == 1 {
                "y"
            } else {
                "ies"
            }
        )));
    };
    if !entry.oid.eq_ignore_ascii_case(expected_oid) {
        // The single failure this module exists to prevent: another worktree,
        // client or agent pushed or dropped a stash, every index shifted, and
        // `drop` would now destroy work the user never saw.
        return Err(stale(format!(
            "Stash entry {index} changed since it was listed — it now holds \"{}\". \
             {STALE_STACK_HINT}",
            entry.subject
        )));
    }

    let argv = action_argv(action, &entry.selector, &entry.oid);
    let verdict = judge(&argv)?;
    let output = git_text(repo.as_path(), &argv[1..])?;
    if matches!(action, StashAction::Pop | StashAction::Drop) {
        verify_dropped_entry(repo.as_path(), action, &entry.oid, &output)?;
    }
    Ok((verdict, output))
}

/// The object id git printed for the entry it just removed, from
/// `Dropped refs/stash@{0} (<oid>)` (printed by both `drop` and `pop`).
///
/// `None` when no line has that shape or the id is not a full object name.
/// Callers must treat that as "unverified", never as a match.
fn parse_dropped_oid(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("Dropped ")?;
        let inner = rest.strip_suffix(')')?.rsplit_once('(')?.1;
        (inner.len() >= 40 && validate_oid(inner).is_ok()).then_some(inner)
    })
}

/// Proves the entry git removed is the one the caller confirmed.
///
/// `repo_mutation_lock` fences only GitPulse writers, and `judge` is a sidecar
/// round trip, so an outside `git stash push` between the re-listing and the
/// index-addressed `drop`/`pop` shifts every index and removes a different
/// entry. Git names what it removed, so that is compared with `expected_oid`;
/// on a mismatch the removed entry is stored back and the call fails, naming
/// both ids. Output that cannot be parsed is a failure too: success is only
/// reported for an entry that was seen to be the confirmed one.
fn verify_dropped_entry(
    repo: &Path,
    action: StashAction,
    expected_oid: &str,
    output: &str,
) -> Result<(), CommandError> {
    let label = action.label();
    let Some(actual) = parse_dropped_oid(output) else {
        return Err(format!(
            "git stash {label} ran but its output did not name the entry it removed, so it \
             could not be checked against the confirmed entry {expected_oid}. Check `git stash \
             list`; if that entry is gone, `git stash store -m <message> {expected_oid}` \
             restores it. {STALE_STACK_HINT}"
        )
        .into());
    };
    if actual.eq_ignore_ascii_case(expected_oid) {
        return Ok(());
    }
    let message = git_text(repo, &["show", "-s", "--format=%s", actual])
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    let restore = if message.is_empty() {
        git_text(repo, &["stash", "store", actual])
    } else {
        git_text(repo, &["stash", "store", "-m", &message, actual])
    };
    let outcome = match restore {
        Ok(_) => "that entry was restored to the stash stack".to_string(),
        Err(e) => format!(
            "restoring it failed ({}); `git stash store {actual}` brings it back",
            e.message()
        ),
    };
    let applied = if action == StashAction::Pop {
        " The pop also applied that entry's changes to the working tree."
    } else {
        ""
    };
    Err(stale(format!(
        "git stash {label} removed {actual} instead of the confirmed entry {expected_oid}: \
         the stack changed after it was listed. {outcome}.{applied} {STALE_STACK_HINT}"
    )))
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
        let entries = parse_stash_list(&raw).entries;
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
        assert!(parse_stash_list("").entries.is_empty());
        assert!(parse_stash_list("\0").entries.is_empty());
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
        let entries = parse_stash_list(&raw).entries;
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
        let entries = parse_stash_list(&raw).entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].timestamp, 0);
    }

    #[test]
    fn the_listing_is_bounded() {
        let raw: String = (0..MAX_STASH_ENTRIES + 50)
            .map(|i| record(&format!("stash@{{{i}}}"), "aaaa1111", "1", "On main: x"))
            .collect::<Vec<_>>()
            .join("\0");
        assert_eq!(parse_stash_list(&raw).entries.len(), MAX_STASH_ENTRIES);
    }

    const OID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn the_oid_git_printed_is_parsed_from_drop_and_pop_output() {
        assert_eq!(
            parse_dropped_oid(&format!("Dropped refs/stash@{{0}} ({OID_A})\n")),
            Some(OID_A)
        );
        // `pop` prints the status of the restored tree before the drop line.
        let pop = format!("On branch main\nChanges not staged:\n\tmodified: a\nDropped refs/stash@{{2}} ({OID_A})\n");
        assert_eq!(parse_dropped_oid(&pop), Some(OID_A));
    }

    #[test]
    fn malformed_drop_output_is_not_a_match() {
        for raw in [
            "",
            "Dropped refs/stash@{0}\n",
            "Dropped refs/stash@{0} ()\n",
            "Dropped refs/stash@{0} (abc123)\n",
            "Dropped refs/stash@{0} (zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz)\n",
            "Dropped refs/stash@{0} (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            "dropped refs/stash@{0} (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)\n",
        ] {
            assert_eq!(parse_dropped_oid(raw), None, "{raw:?}");
        }
        let error = verify_dropped_entry(
            Path::new("."),
            StashAction::Drop,
            OID_A,
            "Gelöscht refs/stash@{0}\n",
        )
        .expect_err("unparseable output must fail closed");
        assert!(error.message().contains(OID_A), "{}", error.message());
    }

    mod real_git {
        use super::*;
        use crate::procguard::LockedSpawn;
        use std::process::Command;

        fn git(path: &Path, args: &[&str]) -> String {
            let mut command = Command::new("git");
            crate::test_support::pin_git(&mut command);
            let output = command
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "GitPulse Test")
                .env("GIT_AUTHOR_EMAIL", "gitpulse@example.test")
                .env("GIT_COMMITTER_NAME", "GitPulse Test")
                .env("GIT_COMMITTER_EMAIL", "gitpulse@example.test")
                .output_locked()
                .expect("git should run");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }

        fn stash_of(path: &Path, content: &str, message: &str) {
            std::fs::write(path.join("a.txt"), content).unwrap();
            git(path, &["stash", "push", "-m", message]);
        }

        /// Stack `[confirmed]`; an outside `git stash push` lands while the
        /// gate is judging, so `stash@{0}` is now someone else's work.
        fn raced_action(action: StashAction) -> (tempfile::TempDir, String, String, CommandError) {
            let dir = tempfile::tempdir().expect("temp repo");
            let path = dir.path();
            git(path, &["init", "-b", "main"]);
            crate::test_support::trust_repo(path);
            std::fs::write(path.join("a.txt"), "base").unwrap();
            git(path, &["add", "a.txt"]);
            git(path, &["commit", "-m", "base"]);
            stash_of(path, "confirmed", "confirmed work");
            let confirmed = git(path, &["rev-parse", "refs/stash"]);

            let repo = path.to_str().unwrap();
            let intruder = std::cell::RefCell::new(String::new());
            let error = run_action_with(repo, action, 0, &confirmed, |_: &[&str]| {
                stash_of(path, "intruder", "outside work");
                *intruder.borrow_mut() = git(path, &["rev-parse", "refs/stash"]);
                Ok(())
            })
            .expect_err("the dropped entry was not the confirmed one");
            (dir, confirmed, intruder.into_inner(), error)
        }

        #[test]
        fn a_stash_pushed_during_the_gate_does_not_cost_the_user_either_entry() {
            let (dir, confirmed, intruder, error) = raced_action(StashAction::Drop);
            let message = error.message();
            assert!(
                message.contains(&intruder),
                "actual oid reported: {message}"
            );
            assert!(
                message.contains(&confirmed),
                "expected oid reported: {message}"
            );
            assert!(message.contains("restored"), "{message}");

            let listed = list(dir.path().to_str().unwrap()).unwrap().entries;
            let oids: Vec<&str> = listed.iter().map(|e| e.oid.as_str()).collect();
            assert_eq!(oids, vec![intruder.as_str(), confirmed.as_str()]);
            assert_eq!(
                listed[0].message, "outside work",
                "restored with its message"
            );
        }

        #[test]
        fn a_raced_pop_is_reported_and_the_popped_entry_restored() {
            let (dir, confirmed, intruder, error) = raced_action(StashAction::Pop);
            assert!(error.message().contains("applied"), "{}", error.message());
            let listed = list(dir.path().to_str().unwrap()).unwrap().entries;
            let oids: Vec<&str> = listed.iter().map(|e| e.oid.as_str()).collect();
            assert_eq!(oids, vec![intruder.as_str(), confirmed.as_str()]);
        }

        #[test]
        fn an_unraced_drop_still_succeeds() {
            let dir = tempfile::tempdir().expect("temp repo");
            let path = dir.path();
            git(path, &["init", "-b", "main"]);
            crate::test_support::trust_repo(path);
            std::fs::write(path.join("a.txt"), "base").unwrap();
            git(path, &["add", "a.txt"]);
            git(path, &["commit", "-m", "base"]);
            stash_of(path, "mine", "mine");
            let oid = git(path, &["rev-parse", "refs/stash"]);
            let (_, output) = run_action_with(
                path.to_str().unwrap(),
                StashAction::Drop,
                0,
                &oid,
                |_: &[&str]| Ok(()),
            )
            .expect("drop");
            assert!(output.contains(&oid), "{output}");
            assert!(list(path.to_str().unwrap()).unwrap().entries.is_empty());
        }
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
