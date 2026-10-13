//! One advisory per tool call when two copies of the hook judge it.
//!
//! GitPulse puts its `PreToolUse` hooks on every Claude Code session it
//! launches (`workbench::claude_hook`), whether or not the configuration the
//! session ends up using also enables the plugin. Which configuration that is
//! cannot be known at launch: a shell wrapper may set `CLAUDE_CONFIG_DIR`
//! after GitPulse has handed over the command line. Claude Code runs a
//! plugin's handler and a settings handler separately even when they are the
//! same command, so a session with both runs this hook twice per call.
//!
//! Running twice is safe for the decision itself: every check here is a pure
//! read, and both copies reach the same deny or ask, which Claude Code merges.
//! What is not harmless is the user reading the same warning twice on every
//! call. So an exact repeat of an advisory — a `systemMessage` with no
//! decision attached — for the same call is dropped.
//!
//! Invariants:
//!
//! * A decision (`deny`, `ask`, or anything else in `hookSpecificOutput`) is
//!   never touched. Deduplicating one could only ever lose a refusal.
//! * Only an identical message for the same `tool_use_id` and subcommand is
//!   dropped. Two copies that say different things (one timed out, one did
//!   not; two hook versions) both speak.
//! * Every failure — no `tool_use_id`, an id that is not a plain token, a
//!   directory that cannot be made private, any I/O error — keeps the
//!   message. The cost of failing is a duplicate, never a lost warning.
//! * The claim is `O_CREAT | O_EXCL`, which the kernel arbitrates, so two
//!   concurrent copies cannot both suppress or both claim.
//! * The marker directory is bounded: claims older than [`MARKER_TTL`] are
//!   pruned, at most [`PRUNE_SCAN`] entries per claim.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{fnv1a, HookInput, HookOutput};

/// How long a claim is kept. Both copies of the hook start within moments of
/// each other for one call; anything older belongs to a finished call.
pub const MARKER_TTL: Duration = Duration::from_secs(15 * 60);

/// Most directory entries one claim looks at while pruning.
pub const PRUNE_SCAN: usize = 256;

/// Longest `tool_use_id` accepted as a marker name.
const MAX_ID: usize = 128;

/// The marker directory: private to this user, in the per-user temporary
/// directory on macOS and named for the uid elsewhere, where it is shared.
pub fn marker_dir() -> PathBuf {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        std::env::temp_dir().join(format!("gitpulse-hook-advisories-{uid}"))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("gitpulse-hook-advisories")
    }
}

/// `output` with an exact repeat of an advisory for this call removed.
pub fn dedupe(subcommand: &str, input: &HookInput, output: HookOutput) -> HookOutput {
    dedupe_in(&marker_dir(), subcommand, input, output)
}

/// [`dedupe`] against an explicit directory, for tests.
pub fn dedupe_in(
    dir: &Path,
    subcommand: &str,
    input: &HookInput,
    output: HookOutput,
) -> HookOutput {
    if output.hook_specific_output.is_some() {
        return output;
    }
    let Some(message) = output.system_message.as_deref() else {
        return output;
    };
    let Some(name) = marker_name(subcommand, &input.tool_use_id, message) else {
        return output;
    };
    match claim(dir, &name) {
        Claim::Repeat => HookOutput::silent(),
        Claim::First => output,
        Claim::Unavailable(reason) => {
            log::debug!(target: "hook", "advisory dedupe unavailable, keeping the message: {reason}");
            output
        }
    }
}

/// `<subcommand>-<tool_use_id>-<digest of the message>`, or `None` when any
/// part is not a plain token (a path separator would escape the directory).
fn marker_name(subcommand: &str, tool_use_id: &str, message: &str) -> Option<String> {
    let plain = |s: &str| {
        !s.is_empty()
            && s.len() <= MAX_ID
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if !plain(subcommand) || !plain(tool_use_id) {
        return None;
    }
    Some(format!(
        "{subcommand}-{tool_use_id}-{:016x}",
        fnv1a(message)
    ))
}

#[derive(Debug, PartialEq, Eq)]
enum Claim {
    First,
    Repeat,
    Unavailable(String),
}

fn claim(dir: &Path, name: &str) -> Claim {
    if let Err(reason) = private_dir(dir) {
        return Claim::Unavailable(reason);
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(name))
    {
        Ok(_) => {
            prune(dir, SystemTime::now());
            Claim::First
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Claim::Repeat,
        Err(error) => Claim::Unavailable(error.to_string()),
    }
}

/// Makes `dir` if needed and confirms it is a real directory only this user
/// can write. A directory someone else planted is refused, not used.
fn private_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err(format!(
                "{} is not a private directory of this user",
                dir.display()
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())
    }
}

/// Removes claims older than [`MARKER_TTL`], looking at no more than
/// [`PRUNE_SCAN`] entries. Errors are ignored: a stale marker costs a few
/// bytes, and a marker another copy is about to read must not be raced away,
/// which the age bound already guarantees.
fn prune(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.take(PRUNE_SCAN).flatten() {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > MARKER_TTL);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::PermissionDecision;

    fn call(id: &str) -> HookInput {
        HookInput {
            tool_use_id: id.into(),
            ..Default::default()
        }
    }

    fn deny() -> HookOutput {
        HookOutput {
            hook_specific_output: Some(super::super::decision(
                "PreToolUse",
                PermissionDecision::Deny,
                "blast radius".into(),
            )),
            system_message: Some("GitPulse refused this".into()),
        }
    }

    #[test]
    fn an_exact_repeat_of_an_advisory_for_one_call_is_dropped() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m");
        let first = dedupe_in(
            &path,
            "command-gate",
            &call("toolu_1"),
            HookOutput::notice("contended"),
        );
        let second = dedupe_in(
            &path,
            "command-gate",
            &call("toolu_1"),
            HookOutput::notice("contended"),
        );
        assert_eq!(first, HookOutput::notice("contended"));
        assert!(
            second.is_silent(),
            "the second copy repeated the warning: {second:?}"
        );
    }

    #[test]
    fn a_decision_is_never_deduplicated() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m");
        for _ in 0..3 {
            assert_eq!(
                dedupe_in(&path, "command-gate", &call("toolu_1"), deny()),
                deny()
            );
        }
    }

    #[test]
    fn different_messages_calls_or_subcommands_all_speak() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m");
        let cases = [
            ("command-gate", "toolu_1", "a"),
            ("command-gate", "toolu_1", "b"),
            ("command-gate", "toolu_2", "a"),
            ("collision-guard", "toolu_1", "a"),
        ];
        for (sub, id, message) in cases {
            let out = dedupe_in(&path, sub, &call(id), HookOutput::notice(message));
            assert_eq!(
                out,
                HookOutput::notice(message),
                "{sub} {id} {message} was dropped"
            );
        }
    }

    #[test]
    fn anything_that_cannot_be_a_marker_keeps_the_message() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m");
        let long = "x".repeat(MAX_ID + 1);
        for id in [
            "",
            "../escape",
            "a/b",
            "toolu 1",
            "toolu\u{0}1",
            long.as_str(),
        ] {
            for _ in 0..2 {
                let out = dedupe_in(&path, "command-gate", &call(id), HookOutput::notice("n"));
                assert_eq!(out, HookOutput::notice("n"), "{id:?} lost its message");
            }
        }
        let entries = std::fs::read_dir(&path).map(|d| d.count()).unwrap_or(0);
        assert_eq!(entries, 0, "a rejected id still wrote a marker");
    }

    #[test]
    fn silence_stays_silent_and_writes_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m");
        assert!(dedupe_in(
            &path,
            "command-gate",
            &call("toolu_1"),
            HookOutput::silent()
        )
        .is_silent());
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_others_can_write_is_refused_and_the_message_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        for _ in 0..2 {
            let out = dedupe_in(
                &path,
                "command-gate",
                &call("toolu_1"),
                HookOutput::notice("n"),
            );
            assert_eq!(out, HookOutput::notice("n"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_place_of_the_directory_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        let path = dir.path().join("m");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        for _ in 0..2 {
            let out = dedupe_in(
                &path,
                "command-gate",
                &call("toolu_1"),
                HookOutput::notice("n"),
            );
            assert_eq!(out, HookOutput::notice("n"));
        }
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }

    #[test]
    fn concurrent_copies_speak_exactly_once_per_call() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m");
        for round in 0..50 {
            let id = format!("toolu_{round}");
            let spoke = std::sync::atomic::AtomicUsize::new(0);
            std::thread::scope(|scope| {
                for _ in 0..8 {
                    scope.spawn(|| {
                        let out =
                            dedupe_in(&path, "command-gate", &call(&id), HookOutput::notice("n"));
                        if !out.is_silent() {
                            spoke.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    });
                }
            });
            assert_eq!(spoke.into_inner(), 1, "round {round}");
        }
    }

    #[test]
    fn stale_claims_are_pruned_and_fresh_ones_kept() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("m");
        dedupe_in(
            &path,
            "command-gate",
            &call("toolu_old"),
            HookOutput::notice("n"),
        );
        dedupe_in(
            &path,
            "command-gate",
            &call("toolu_new"),
            HookOutput::notice("n"),
        );
        let later = SystemTime::now() + MARKER_TTL + Duration::from_secs(1);
        // Everything is stale from far enough in the future.
        prune(&path, later);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        dedupe_in(
            &path,
            "command-gate",
            &call("toolu_new"),
            HookOutput::notice("n"),
        );
        prune(&path, SystemTime::now());
        assert_eq!(
            std::fs::read_dir(&path).unwrap().count(),
            1,
            "a fresh claim was pruned"
        );
    }
}
