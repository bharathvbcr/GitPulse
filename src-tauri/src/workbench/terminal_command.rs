//! Provider arguments and one temporary source file for an explicitly requested
//! terminal attempt. User task text never becomes shell syntax or an argv blob.

use super::WorkbenchError;
use crate::engine::git_cli::{
    capture_command, extended_child_path, resolve_spawn_program_with, CapturedOutput,
};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(super) struct BriefFile {
    pub path: PathBuf,
    _file: File,
}

impl BriefFile {
    pub fn create(markdown: &str) -> Result<Self, WorkbenchError> {
        Self::under(&std::env::temp_dir(), markdown)
    }
    fn under(root: &Path, markdown: &str) -> Result<Self, WorkbenchError> {
        if markdown.is_empty() || markdown.len() > 2 * 1024 * 1024 {
            return Err(error(
                "invalid_input",
                "The task brief is empty or exceeds 2 MiB.",
            ));
        }
        let tick = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| error("file_error", e.to_string()))?
            .as_nanos();
        let path = root.join(format!(
            "gitpulse-task-{}-{tick}-{}.md",
            std::process::id(),
            FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|e| error("file_error", e.to_string()))?;
        let mut brief = Self { path, _file: file };
        brief
            ._file
            .write_all(markdown.as_bytes())
            .and_then(|()| brief._file.sync_all())
            .map_err(|e| error("file_error", e.to_string()))?;
        Ok(brief)
    }
}
impl Drop for BriefFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(target: "workbench", "could not remove the temporary task brief: {error}");
            }
        }
    }
}

fn error(code: &str, message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new(code, message)
}

pub(super) fn is_terminal_provider(provider: &str) -> bool {
    matches!(provider, "codex" | "claude" | "grok" | "agy")
}

/// Providers GitPulse can run in the **managed** lane, where the harness hosts
/// the session over a protocol and GitPulse mediates every approval — rather
/// than handing the task to a terminal and stepping back.
///
/// Strictly smaller than [`is_terminal_provider`], and it is smaller for a
/// mechanical reason rather than a policy one: a managed run needs an adapter
/// in the harness that speaks that provider's own session protocol. Grok and
/// Antigravity have none, so admitting them here would hand their output to a
/// reader written for someone else's wire format. This list is the mirror of
/// the harness's own `codingagent.ManagedProviders`; the two are checked
/// against each other by `managed-provider-parity-contract`.
pub(super) fn is_managed_provider(provider: &str) -> bool {
    matches!(provider, "codex" | "claude")
}

pub(super) fn program(provider: &str) -> Result<String, WorkbenchError> {
    if !is_terminal_provider(provider) {
        return Err(error("invalid_input", "Unsupported terminal provider."));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let path = extended_child_path(std::env::var_os("PATH").as_deref(), home.as_deref());
    let resolved = resolve_spawn_program_with(provider, path.as_deref(), home.as_deref());
    if !Path::new(&resolved).is_absolute() || !Path::new(&resolved).is_file() {
        return Err(error(
            "not_installed",
            format!("{provider} is not available on the terminal PATH."),
        ));
    }
    Ok(resolved)
}

pub(super) fn check(
    program: &str,
    cwd: &str,
    provider: &str,
    mode: &str,
) -> Result<(), WorkbenchError> {
    let version = capture_command(
        program,
        &["--version"],
        Some(Path::new(cwd)),
        Duration::from_secs(8),
        &[],
    )
    .map_err(|e| error("capability_error", e))?;
    let help = capture_command(
        program,
        &["--help"],
        Some(Path::new(cwd)),
        Duration::from_secs(8),
        &[],
    )
    .map_err(|e| error("capability_error", e))?;
    advertises(provider, mode, &version, &help)
}

/// Whether what a build *said* proves it can be launched the way `mode` asks.
///
/// Split from [`check`] because this is the whole decision, and it is a decision
/// about two byte strings: which stream they arrived on, what they bound, and
/// what the help advertises. Driving it through two real `--version`/`--help`
/// spawns made every case here wait on a child reaching `main` inside eight
/// seconds — a bound on host load, not on any of this. `capture_command` owns
/// the other half, that the spawn happens and its output comes back.
fn advertises(
    provider: &str,
    mode: &str,
    version: &CapturedOutput,
    help: &CapturedOutput,
) -> Result<(), WorkbenchError> {
    if !version.success
        || version.stdout.len() > 1024
        || version.stderr.len() > 1024
        || !help.success
        || help.stdout.len() > 128 * 1024
        || help.stderr.len() > 128 * 1024
    {
        return Err(error(
            "capability_error",
            "The provider did not return bounded version and help responses.",
        ));
    }
    let version = command_text(&version.stdout, &version.stderr)
        .map_err(|_| error("capability_error", "Invalid provider version response."))?;
    let help = command_text(&help.stdout, &help.stderr)
        .map_err(|_| error("capability_error", "Invalid provider help response."))?;
    let identity = match provider {
        "codex" => version.starts_with("codex-cli "),
        "claude" => version.contains("Claude Code"),
        "grok" => version.starts_with("grok "),
        // `agy --version` prints only a semver. Identity is the help banner;
        // Antigravity writes that banner to stderr, which `command_text` prefers
        // only when stdout is empty.
        "agy" => help.contains("Usage of agy:") && help.contains("--prompt-interactive"),
        _ => false,
    };
    let (flags, _) = policy(provider, mode)?;
    let supported = flags
        .iter()
        .enumerate()
        .filter(|(_, flag)| flag.starts_with("--"))
        .all(|(index, flag)| {
            let value = flags
                .get(index + 1)
                .copied()
                .filter(|next| !next.starts_with("--"));
            advertised_option(help, flag, value)
        });
    let required: &[&str] = match provider {
        "codex" => &["--cd"],
        "grok" => &["--cwd"],
        "agy" => &[
            "--mode",
            "--prompt-interactive",
            "--dangerously-skip-permissions",
        ],
        _ => &[],
    };
    if !identity
        || !supported
        || required
            .iter()
            .any(|flag| !advertised_option(help, flag, None))
    {
        return Err(error(
            "unsupported_capability",
            "This provider build does not advertise the requested launch controls.",
        ));
    }
    Ok(())
}

/// stdout when the provider printed there, otherwise stderr.
///
/// Claude Code, Codex and Grok write `--help` to stdout. Antigravity's `agy`
/// writes it to stderr (Go `flag`). A check that only read stdout would treat
/// a real install as having no launch controls.
fn command_text<'a>(stdout: &'a [u8], stderr: &'a [u8]) -> Result<&'a str, std::str::Utf8Error> {
    std::str::from_utf8(if stdout.is_empty() { stderr } else { stdout })
}

/// Verify the exact option and value within its own help block. A similarly
/// named flag or a value mentioned by a different option is not capability proof.
fn advertised_option(help: &str, flag: &str, value: Option<&str>) -> bool {
    let mut found = false;
    let mut description = String::new();
    for line in help.lines() {
        let trimmed = line.trim_start();
        let is_option = trimmed.starts_with("--")
            || (trimmed.starts_with('-') && trimmed.as_bytes().get(2) == Some(&b','));
        if is_option {
            if found {
                break;
            }
            found = trimmed
                .split(|c: char| c.is_whitespace() || c == ',' || c == '=')
                .any(|word| word == flag);
        }
        if found {
            description.push_str(line);
            description.push('\n');
        }
    }
    found
        && value.is_none_or(|value| {
            description
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .any(|word| word == value)
        })
}

/// Permission modes, in the order a chooser should offer them: least
/// authority first, so the dangerous end of the range is the far end rather
/// than a neighbour of the safe default.
///
/// Read by `agentDefaults.contract.test.ts`, which fails if the frontend's
/// list drifts from this one. The frontend needs the *names* to render a
/// chooser and to say which mode a tab will start in; it must never carry the
/// flags, because a transcribed flag table is a table that drifts.
pub(crate) const PERMISSION_MODES: [&str; 6] = [
    "inspect",
    "ask",
    "edit",
    "auto_review",
    "preapproved",
    "bypass",
];

/// The one mode that turns a safety control off, named once so the two places
/// that must treat it specially cannot disagree about which one it is.
pub(crate) const BYPASS_MODE: &str = "bypass";

/// Launchers that accept a permission mode, derived by asking [`policy`]
/// rather than by writing the list down a second time.
///
/// Every launcher the tab strip offers is tried against every mode, and one
/// that can express them all is included. Derived because the alternative —
/// a hand-kept list — is the thing that goes stale when a provider gains or
/// loses a policy, and a stale list here is a chooser offering a mode that
/// refuses at spawn.
pub(crate) fn permission_launchers() -> Vec<String> {
    crate::terminal::AGENT_LAUNCHERS
        .iter()
        .filter(|launcher| {
            PERMISSION_MODES
                .iter()
                .all(|mode| policy(launcher, mode).is_ok())
        })
        .map(|launcher| (*launcher).to_owned())
        .collect()
}

/// Whether this launcher/mode pair is one a launch could actually apply.
///
/// Asks [`policy`] rather than restating its arms, so a pair this accepts is
/// exactly a pair that expands. Used to validate stored defaults at the point
/// they are saved *and* again when they are read, because the file they live
/// in is editable by hand.
pub(crate) fn validate_permission_default(launcher: &str, mode: &str) -> Result<(), String> {
    if !is_terminal_provider(launcher) {
        return Err(format!("{launcher} does not take a permission mode"));
    }
    policy(launcher, mode)
        .map(|_| ())
        .map_err(|error| error.message)
}

/// Expands a stored default permission mode into that provider's own flags,
/// ahead of the arguments the caller already built.
///
/// This exists so the terminal tab strip and the workbench handoff share one
/// table. [`policy`] is the only place that knows which flag means "plan" for
/// which CLI; the frontend knows mode *names* and nothing else, so there is no
/// second copy to drift.
///
/// **Order matters and is why the flags go in front.** Claude Code's prompt
/// form is `-- <text>`, after which every argument is positional: a permission
/// flag appended behind the prompt would be read as part of the prompt rather
/// than obeyed. The caller's own arguments (notification flags, then the
/// prompt) keep their relative order behind these.
///
/// Refuses rather than ignores in three cases, all of them the same rule: a
/// control the user asked for that cannot be applied must never look like one
/// that was.
///
/// * A mode for a launcher with no policy (`shell`, `manvi`) — silently
///   dropping it would leave a tab the user believes is in plan mode running
///   with the CLI's own default.
/// * An unknown mode — the same, one layer down.
/// * An acknowledgement that does not match the mode. Exactly as
///   [`arguments`] does it: `bypass` needs one and every other mode must not
///   carry one. The symmetry is the point — a caller that hardcoded
///   `acknowledged: true` would fail on its very first ordinary launch, rather
///   than working until the day someone stores `bypass` and silently getting
///   the session nobody agreed to.
pub(crate) fn apply_permission_mode(
    program: Option<&str>,
    mode: Option<&str>,
    acknowledged: bool,
    args: Option<Vec<String>>,
) -> Result<Option<Vec<String>>, String> {
    let Some(mode) = mode.map(str::trim).filter(|mode| !mode.is_empty()) else {
        return Ok(args);
    };
    let provider = program.map(str::trim).unwrap_or_default();
    if !is_terminal_provider(provider) {
        return Err(format!(
            "{} does not take a permission mode.",
            if provider.is_empty() {
                "The login shell"
            } else {
                provider
            }
        ));
    }
    if (mode == BYPASS_MODE) != acknowledged {
        return Err(if acknowledged {
            "Only bypass takes an acknowledgment.".into()
        } else {
            "Bypass requires acknowledgment for this attempt.".to_owned()
        });
    }
    let (flags, _) = policy(provider, mode).map_err(|error| error.message)?;
    let mut expanded: Vec<String> = flags.into_iter().map(String::from).collect();
    expanded.extend(args.unwrap_or_default());
    Ok(Some(expanded))
}

fn policy(provider: &str, mode: &str) -> Result<(Vec<&'static str>, bool), WorkbenchError> {
    let flags = match (provider, mode) {
        ("codex", "inspect") => vec!["--sandbox", "read-only", "--ask-for-approval", "never"],
        ("codex", "ask") => vec!["--sandbox", "read-only", "--ask-for-approval", "on-request"],
        ("codex", "edit") => vec![
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "on-request",
        ],
        ("codex", "preapproved") => vec![
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "never",
        ],
        ("codex", "auto_review") => vec!["--approve-for-me"],
        ("codex", "bypass") => vec!["--dangerously-bypass-approvals-and-sandbox"],
        ("claude", "inspect") => vec!["--permission-mode", "plan"],
        ("claude", "ask") => vec!["--permission-mode", "manual"],
        ("claude", "edit") => vec!["--permission-mode", "acceptEdits"],
        ("claude", "auto_review") => vec!["--permission-mode", "auto"],
        ("claude", "preapproved") => vec!["--permission-mode", "dontAsk"],
        ("claude", "bypass") => vec!["--permission-mode", "bypassPermissions"],
        // Grok's modes are Claude-compatible except `ask`: it has `default`
        // (prompt for permissions) rather than `manual`.
        ("grok", "inspect") => vec!["--permission-mode", "plan"],
        ("grok", "ask") => vec!["--permission-mode", "default"],
        ("grok", "edit") => vec!["--permission-mode", "acceptEdits"],
        ("grok", "auto_review") => vec!["--permission-mode", "auto"],
        ("grok", "preapproved") => vec!["--permission-mode", "dontAsk"],
        ("grok", "bypass") => vec!["--permission-mode", "bypassPermissions"],
        ("agy", "inspect") => vec!["--mode", "plan"],
        ("agy", "ask") => vec![],
        ("agy", "edit") => vec!["--mode", "accept-edits"],
        ("agy", "auto_review") => vec!["--sandbox"],
        ("agy", "preapproved") => vec!["--mode", "accept-edits", "--sandbox"],
        ("agy", "bypass") => vec!["--dangerously-skip-permissions"],
        _ => {
            return Err(error(
                "unsupported_capability",
                "Unsupported provider permission mode.",
            ))
        }
    };
    Ok((flags, mode == "inspect"))
}

pub(super) fn arguments(
    provider: &str,
    mode: &str,
    acknowledged: bool,
    cwd: &str,
    brief: &Path,
) -> Result<Vec<String>, WorkbenchError> {
    if (mode == "bypass") != acknowledged {
        return Err(error(
            "invalid_input",
            "Bypass requires acknowledgment for this attempt.",
        ));
    }
    let (flags, inspect) = policy(provider, mode)?;
    let path = brief
        .to_str()
        .ok_or_else(|| error("file_error", "Task brief path is not Unicode."))?;
    let quoted = serde_json::to_string(path).map_err(|e| error("file_error", e.to_string()))?;
    let mut args: Vec<String> = flags.into_iter().map(String::from).collect();
    match provider {
        "codex" => args.extend(["--cd".into(), cwd.into()]),
        "grok" => args.extend(["--cwd".into(), cwd.into()]),
        // Antigravity ignores positional arguments as prompts.
        "agy" => args.push("--prompt-interactive".into()),
        _ => {}
    }
    let scope = if inspect {
        "Inspect and propose a plan; do not modify files."
    } else {
        "Carry out the saved task within the selected permission mode. Report verification and anything requiring human review."
    };
    args.push(format!("Read the UTF-8 task brief at {quoted}. It contains the user's saved task and repository references. {scope} Do not mark the task accepted or publish changes on the user's behalf."));
    if args.iter().any(|a| a.len() > 16 * 1024 || a.contains('\0')) {
        return Err(error(
            "invalid_input",
            "The launch arguments exceed terminal limits.",
        ));
    }
    Ok(args)
}

#[cfg(test)]
mod permission_default_tests {
    use super::{
        apply_permission_mode, permission_launchers, policy, validate_permission_default,
        BYPASS_MODE, PERMISSION_MODES,
    };

    fn args(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|s| (*s).to_owned()).collect())
    }

    /// The load-bearing property. Claude Code's `-- <text>` makes everything
    /// after it positional, so a permission flag that landed behind the prompt
    /// would be read as prompt text and the session would run unrestricted
    /// while appearing configured.
    #[test]
    fn policy_flags_precede_the_callers_own_arguments() {
        let out = apply_permission_mode(
            Some("claude"),
            Some("inspect"),
            false,
            args(&["--settings", "{}", "--", "do the thing"]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            out,
            vec![
                "--permission-mode",
                "plan",
                "--settings",
                "{}",
                "--",
                "do the thing"
            ]
        );
        let separator = out.iter().position(|a| a == "--").unwrap();
        assert!(
            out.iter().position(|a| a == "--permission-mode").unwrap() < separator,
            "a permission flag after `--` is prompt text, not a permission"
        );
    }

    #[test]
    fn every_mode_expands_for_every_launcher_that_offers_them() {
        for launcher in permission_launchers() {
            for mode in PERMISSION_MODES {
                let acknowledged = mode == BYPASS_MODE;
                let out =
                    apply_permission_mode(Some(&launcher), Some(mode), acknowledged, None).unwrap();
                assert!(
                    out.is_some(),
                    "{launcher}/{mode} expanded to nothing at all"
                );
                assert_eq!(
                    out.unwrap(),
                    policy(&launcher, mode).unwrap().0,
                    "{launcher}/{mode} disagreed with the policy table"
                );
            }
        }
    }

    /// `agy`'s `ask` is the one pair with no flags. It must still be offered
    /// and still expand — to an empty list, which is a mode that was applied,
    /// not a mode that was skipped.
    #[test]
    fn a_mode_whose_policy_is_empty_is_still_applied() {
        assert_eq!(policy("agy", "ask").unwrap().0, Vec::<&str>::new());
        let out = apply_permission_mode(Some("agy"), Some("ask"), false, args(&["--keep"]))
            .unwrap()
            .unwrap();
        assert_eq!(out, vec!["--keep"]);
    }

    #[test]
    fn bypass_without_acknowledgment_is_refused_for_every_launcher() {
        for launcher in permission_launchers() {
            let refused = apply_permission_mode(Some(&launcher), Some(BYPASS_MODE), false, None);
            assert!(
                refused.is_err(),
                "{launcher} produced a bypassed session with no acknowledgment"
            );
        }
    }

    /// The symmetry that makes a hardcoded `acknowledged: true` fail loudly on
    /// an ordinary launch instead of lying dormant until bypass is stored.
    #[test]
    fn an_acknowledgment_without_bypass_is_refused() {
        for mode in PERMISSION_MODES.iter().filter(|m| **m != BYPASS_MODE) {
            assert!(
                apply_permission_mode(Some("claude"), Some(mode), true, None).is_err(),
                "{mode} accepted an acknowledgment it does not need"
            );
        }
    }

    /// A mode asked for and not applied must be a refusal, never a quiet
    /// passthrough: a tab the user believes is in plan mode would otherwise
    /// run with the CLI's own default.
    #[test]
    fn a_launcher_with_no_policy_refuses_a_mode_rather_than_dropping_it() {
        for launcher in [None, Some(""), Some("shell"), Some("manvi"), Some("bash")] {
            let refused = apply_permission_mode(launcher, Some("inspect"), false, None);
            assert!(
                refused.is_err(),
                "{launcher:?} silently ignored a permission mode"
            );
        }
    }

    #[test]
    fn an_unknown_mode_is_refused_rather_than_guessed() {
        for mode in ["", "  ", "plan", "yolo", "BYPASS", "inspect ", "--sandbox"] {
            let out = apply_permission_mode(Some("claude"), Some(mode), false, args(&["--keep"]));
            if mode.trim().is_empty() {
                // Nothing asked for, so the caller's arguments pass through.
                assert_eq!(out.unwrap().unwrap(), vec!["--keep"]);
            } else if mode == "inspect " {
                // Trimmed, then honoured: a stored value with stray space is a
                // typo, not a different mode.
                assert_eq!(
                    out.unwrap().unwrap(),
                    vec!["--permission-mode", "plan", "--keep"]
                );
            } else {
                assert!(out.is_err(), "{mode:?} was accepted");
            }
        }
    }

    #[test]
    fn no_mode_leaves_the_arguments_exactly_as_they_came() {
        assert_eq!(
            apply_permission_mode(Some("claude"), None, false, args(&["--settings", "{}"]))
                .unwrap()
                .unwrap(),
            vec!["--settings", "{}"]
        );
        assert!(
            apply_permission_mode(None, None, false, None)
                .unwrap()
                .is_none(),
            "a plain shell gained arguments it never asked for"
        );
    }

    /// The chooser's list is derived, so this pins what it derives *to* — a
    /// provider silently dropping out of the list would otherwise be invisible.
    #[test]
    fn permission_launchers_are_the_terminal_providers_and_not_the_tab_strip() {
        let mut launchers = permission_launchers();
        launchers.sort();
        assert_eq!(launchers, vec!["agy", "claude", "codex", "grok"]);
        assert!(
            !launchers.contains(&"manvi".to_owned()),
            "manvi has no policy; offering it a mode would refuse at spawn"
        );
    }

    /// Sweeps the whole grid plus the ways a caller could be wrong, and
    /// asserts the one property that must hold across all of it: every call
    /// either applies exactly the policy table's flags, or refuses. There is
    /// no third outcome, and in particular no "returned the arguments
    /// unchanged while the caller believes a mode was applied".
    #[test]
    fn every_input_either_applies_the_table_or_refuses_and_never_silently_passes() {
        let launchers = [
            "claude", "codex", "grok", "agy", "manvi", "shell", "", "sh", "CLAUDE",
        ];
        let modes = [
            "inspect",
            "ask",
            "edit",
            "auto_review",
            "preapproved",
            "bypass",
            "plan",
            "yolo",
            "",
            "  ",
            "BYPASS",
            "bypass\u{0}",
            "../bypass",
        ];
        let (mut applied, mut refused, mut passthrough) = (0usize, 0usize, 0usize);
        for launcher in launchers {
            for mode in modes {
                for acknowledged in [false, true] {
                    let carried = args(&["--carried"]);
                    let outcome = apply_permission_mode(
                        Some(launcher),
                        Some(mode),
                        acknowledged,
                        carried.clone(),
                    );
                    let Ok(out) = outcome else {
                        refused += 1;
                        continue;
                    };
                    let out = out.expect("an applied mode must produce arguments");
                    if mode.trim().is_empty() {
                        // Nothing was asked for, so nothing was applied and the
                        // caller's own arguments survive untouched.
                        assert_eq!(out, vec!["--carried"]);
                        passthrough += 1;
                        continue;
                    }
                    // Anything else that succeeded must be exactly the table's
                    // flags followed by what the caller passed.
                    let (flags, _) =
                        policy(launcher, mode.trim()).expect("a mode applied without a policy arm");
                    let mut expected: Vec<String> = flags.into_iter().map(String::from).collect();
                    expected.push("--carried".into());
                    assert_eq!(out, expected, "{launcher}/{mode}/ack={acknowledged}");
                    // And bypass only ever with an acknowledgement.
                    assert_eq!(mode.trim() == BYPASS_MODE, acknowledged);
                    applied += 1;
                }
            }
        }
        // Carrying all three numbers rather than one total: a sweep where
        // everything refused would "pass" a refusal-only assertion while
        // proving nothing about the applied path.
        assert_eq!(
            applied,
            permission_launchers().len() * PERMISSION_MODES.len(),
            "expected exactly one applied outcome per real launcher/mode pair"
        );
        assert!(refused > 0 && passthrough > 0, "a branch went untested");
    }

    /// A caller that hands over an enormous argument list must not have the
    /// flags silently dropped or reordered; expansion is a prepend and stays
    /// one whatever the volume.
    #[test]
    fn expansion_prepends_regardless_of_how_many_arguments_the_caller_brought() {
        let carried: Vec<String> = (0..10_000).map(|i| format!("--arg{i}")).collect();
        let out = apply_permission_mode(
            Some("claude"),
            Some("inspect"),
            false,
            Some(carried.clone()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.len(), carried.len() + 2);
        assert_eq!(&out[..2], &["--permission-mode", "plan"]);
        assert_eq!(&out[2..], &carried[..]);
    }

    #[test]
    fn stored_defaults_are_validated_against_the_policy_table() {
        assert!(validate_permission_default("claude", "edit").is_ok());
        for (launcher, mode) in [
            ("manvi", "edit"),
            ("shell", "edit"),
            ("claude", "yolo"),
            ("", "edit"),
            ("claude", ""),
        ] {
            assert!(
                validate_permission_default(launcher, mode).is_err(),
                "{launcher}={mode} passed validation"
            );
        }
    }

    /// Bypass is storable — that is the whole point of the acknowledgement
    /// design — but storing it must not be what applies it.
    #[test]
    fn bypass_validates_as_a_stored_default_yet_still_needs_acknowledgment() {
        assert!(validate_permission_default("claude", BYPASS_MODE).is_ok());
        assert!(apply_permission_mode(Some("claude"), Some(BYPASS_MODE), false, None).is_err());
        assert_eq!(
            apply_permission_mode(Some("claude"), Some(BYPASS_MODE), true, None)
                .unwrap()
                .unwrap(),
            vec!["--permission-mode", "bypassPermissions"]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{arguments, BriefFile, CapturedOutput};
    #[test]
    #[ignore = "requires explicitly installed Claude Code, Codex, Grok and Antigravity binaries; probes help only"]
    fn installed_provider_help_supports_requested_modes() {
        let root = tempfile::tempdir().unwrap();
        for provider in ["claude", "codex", "grok", "agy"] {
            let program = super::program(provider).unwrap();
            for mode in [
                "inspect",
                "ask",
                "edit",
                "auto_review",
                "preapproved",
                "bypass",
            ] {
                super::check(&program, root.path().to_str().unwrap(), provider, mode)
                    .unwrap_or_else(|error| panic!("{provider}/{mode}: {}", error.message));
            }
        }
    }
    #[test]
    fn capability_values_belong_to_the_exact_advertised_option() {
        let help = "  -s, --sandbox <MODE>\n    [possible values: read-only, workspace-write]\n  -a, --ask-for-approval <POLICY>\n    - on-request: ask\n    - never: deny\n  -C, --cd <DIR>\n    Set root\n";
        assert!(super::advertised_option(
            help,
            "--sandbox",
            Some("workspace-write")
        ));
        assert!(!super::advertised_option(help, "--sandbox", Some("never")));
        assert!(super::advertised_option(
            help,
            "--ask-for-approval",
            Some("never")
        ));
        assert!(!super::advertised_option(help, "--ask", None));
        assert!(!super::advertised_option(
            "  --permission-mode-legacy <MODE>\n    auto\n",
            "--permission-mode",
            Some("auto")
        ));
        assert!(!super::advertised_option(
            "  --permission-mode <MODE>\n    automatic\n",
            "--permission-mode",
            Some("auto")
        ));
        assert!(!super::advertised_option("", "--cd", None));
    }
    #[cfg(unix)]
    #[test]
    fn probe_refuses_a_build_that_advertises_the_flag_without_the_requested_value() {
        let version = on_stdout("2.1.263 (Claude Code)\n");
        let help = on_stdout("  --permission-mode <mode>\n    (choices: \"plan\", \"manual\")\n");
        assert!(super::advertises("claude", "ask", &version, &help).is_ok());
        assert_eq!(
            super::advertises("claude", "bypass", &version, &help)
                .unwrap_err()
                .code,
            "unsupported_capability",
            "the flag is advertised but the value `bypass` needs is not"
        );
    }
    #[test]
    fn large_source_is_a_private_file_and_never_an_argument() {
        let root = tempfile::tempdir().unwrap();
        let text = "Unchanged $(never_execute) 🧪\n".repeat(20_000);
        let brief = BriefFile::under(root.path(), &text).unwrap();
        assert_eq!(std::fs::read_to_string(&brief.path).unwrap(), text);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&brief.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let args = arguments("codex", "edit", false, "/checkout with spaces", &brief.path).unwrap();
        assert!(args
            .iter()
            .all(|a| !a.contains("never_execute") && a.len() < 16 * 1024));
        assert!(args.iter().any(|a| a == "/checkout with spaces"));
        let path = brief.path.clone();
        drop(brief);
        assert!(!path.exists());
    }
    #[test]
    fn each_provider_mode_is_explicit_and_bypass_is_never_inherited() {
        for provider in ["codex", "claude", "grok", "agy"] {
            for mode in [
                "inspect",
                "ask",
                "edit",
                "auto_review",
                "preapproved",
                "bypass",
            ] {
                let args = arguments(
                    provider,
                    mode,
                    mode == "bypass",
                    "/repo",
                    std::path::Path::new("/brief.md"),
                )
                .unwrap();
                let names_bypass = args
                    .iter()
                    .any(|a| a.contains("bypass") || a.contains("dangerously-skip-permissions"));
                assert_eq!(
                    names_bypass,
                    mode == "bypass",
                    "{provider}/{mode}: {args:?}"
                );
                assert!(arguments(
                    provider,
                    mode,
                    mode != "bypass",
                    "/repo",
                    std::path::Path::new("/brief.md")
                )
                .is_err());
            }
        }
        assert!(arguments(
            "shell",
            "edit",
            false,
            "/repo",
            std::path::Path::new("/brief.md")
        )
        .is_err());
    }
    #[test]
    fn grok_is_a_first_class_terminal_provider() {
        match super::program("grok") {
            Ok(path) => assert!(
                path.contains("grok"),
                "resolved grok path must name grok: {path}"
            ),
            Err(error) => assert_eq!(
                error.code, "not_installed",
                "an unknown provider is invalid_input; a missing install is not_installed: {} {}",
                error.code, error.message
            ),
        }
        assert_eq!(super::program("gemini").unwrap_err().code, "invalid_input");
        let args = arguments(
            "grok",
            "inspect",
            false,
            "/checkout with spaces",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--permission-mode", "plan"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--cwd", "/checkout with spaces"]),
            "{args:?}"
        );
        assert!(!args.iter().any(|a| a.contains("bypass")));
        let ask = arguments(
            "grok",
            "ask",
            false,
            "/repo",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert!(
            ask.windows(2)
                .any(|pair| pair == ["--permission-mode", "default"]),
            "{ask:?}"
        );
    }
    #[test]
    fn antigravity_is_a_first_class_terminal_provider() {
        match super::program("agy") {
            Ok(path) => assert!(
                path.contains("agy"),
                "resolved agy path must name agy: {path}"
            ),
            Err(error) => assert_eq!(
                error.code, "not_installed",
                "an unknown provider is invalid_input; a missing install is not_installed: {} {}",
                error.code, error.message
            ),
        }
        let inspect = arguments(
            "agy",
            "inspect",
            false,
            "/checkout with spaces",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert!(
            inspect.windows(2).any(|pair| pair == ["--mode", "plan"]),
            "{inspect:?}"
        );
        assert!(
            inspect
                .windows(2)
                .any(|pair| pair[0] == "--prompt-interactive" && pair[1].contains("brief.md")),
            "{inspect:?}"
        );
        assert!(!inspect.iter().any(|a| a.contains("dangerously-skip")));
        let ask = arguments(
            "agy",
            "ask",
            false,
            "/repo",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert_eq!(ask[0], "--prompt-interactive", "{ask:?}");
        let bypass = arguments(
            "agy",
            "bypass",
            true,
            "/repo",
            std::path::Path::new("/brief.md"),
        )
        .unwrap();
        assert!(bypass.iter().any(|a| a == "--dangerously-skip-permissions"));
    }
    #[test]
    fn grok_probe_requires_identity_permission_modes_and_cwd() {
        let version = on_stdout("grok 1.0.34 (deadbeef) [stable]\n");
        let help = on_stdout(
            "Grok Build TUI\n      --permission-mode <MODE>\n          [possible values: \
             default, acceptEdits, auto, dontAsk, bypassPermissions, plan]\n      --cwd <CWD>\n \
                      Working directory\n",
        );
        super::advertises("grok", "ask", &version, &help).unwrap();
        super::advertises("grok", "bypass", &version, &help).unwrap();

        // The same build without the mode `ask` needs, and without `--cwd`.
        let narrower = on_stdout(
            "Grok Build TUI\n      --permission-mode <MODE>\n          [possible values: \
             default, plan]\n",
        );
        assert_eq!(
            super::advertises("grok", "ask", &version, &narrower)
                .unwrap_err()
                .code,
            "unsupported_capability"
        );
    }

    #[test]
    fn antigravity_help_on_stderr_is_still_capability_proof() {
        // Help on stderr, version on stdout — the real `agy` layout, and the
        // one a reader of stdout alone would report as having no controls.
        let version = on_stdout("1.1.22\n");
        let help = on_stderr(
            "Usage of agy:\n  --mode                          Set the agent execution mode for \
             this session (accept-edits, plan)\n  --prompt-interactive            Run an initial \
             prompt interactively\n  --dangerously-skip-permissions  Auto-approve all tool \
             permission requests\n  --sandbox                       Run in a sandbox\n",
        );
        super::advertises("agy", "ask", &version, &help).unwrap();
        super::advertises("agy", "inspect", &version, &help).unwrap();
        super::advertises("agy", "bypass", &version, &help).unwrap();

        let without_controls =
            on_stderr("Usage of agy:\n  --prompt-interactive            prompt\n");
        assert_eq!(
            super::advertises("agy", "ask", &version, &without_controls)
                .unwrap_err()
                .code,
            "unsupported_capability"
        );
    }

    /// An answer too large to be a version or a help screen is not one, however
    /// well it reads. Only a fake can produce this: a stub script that printed
    /// 128KiB would be testing the shell's buffering as much as the bound.
    #[test]
    fn an_unbounded_answer_is_not_capability_proof() {
        let version = on_stdout("grok 1.0.34 (deadbeef) [stable]\n");
        let help = on_stdout(
            "Grok Build TUI\n      --permission-mode <MODE>\n          [possible values: \
             default, acceptEdits, auto, dontAsk, bypassPermissions, plan]\n      --cwd <CWD>\n \
                      Working directory\n",
        );
        super::advertises("grok", "ask", &version, &help).expect("the bounded case still passes");

        let mut flood = help.clone();
        flood.stdout.resize(128 * 1024 + 1, b' ');
        assert_eq!(
            super::advertises("grok", "ask", &version, &flood)
                .unwrap_err()
                .code,
            "capability_error",
            "an over-cap help screen is an unusable answer, not an unsupported build"
        );

        let mut chatty = version.clone();
        chatty.stderr.resize(1025, b' ');
        assert_eq!(
            super::advertises("grok", "ask", &chatty, &help)
                .unwrap_err()
                .code,
            "capability_error"
        );
    }

    /// What the provider printed, on the stream it printed it to. Building the
    /// answer directly is what takes these cases off the spawn path: the
    /// decision under test reads two byte strings and an exit status, and a
    /// `#!/bin/sh` stub is only one way — the load-sensitive way — to produce
    /// them.
    fn on_stdout(text: &str) -> CapturedOutput {
        CapturedOutput {
            stdout: text.as_bytes().to_vec(),
            stderr: Vec::new(),
            success: true,
            status_code: 0,
        }
    }

    fn on_stderr(text: &str) -> CapturedOutput {
        CapturedOutput {
            stdout: Vec::new(),
            stderr: text.as_bytes().to_vec(),
            success: true,
            status_code: 0,
        }
    }
}
