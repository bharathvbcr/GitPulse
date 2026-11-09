//! Putting `gitpulse-hook` on every Claude Code session GitPulse launches.
//!
//! The shipped plugin (`plugins/gitpulse/hooks/hooks.json`) runs the hook, but
//! only in a Claude Code configuration directory that enables the plugin. A
//! session launched under `CLAUDE_CONFIG_DIR=~/.claude-chandra` has no hooks at
//! all, so the command gate never sees what that agent runs and the notifier
//! hears only a bell. GitPulse already passes every launch a session-scoped
//! `--settings` object ([`super::terminal_command::notify_flags`]); this adds
//! the plugin's gate and notification hooks ([`INJECTED_EVENTS`]) to it.
//!
//! What Claude Code's documentation says, and what follows from it
//! (<https://code.claude.com/docs/en/cli-reference> and `/hooks`):
//!
//! * `--settings` takes "a settings JSON file or an inline JSON string" whose
//!   values "override the same keys in your `settings.json` files"; keys left
//!   out keep their file-based values.
//! * "Hook entries merge across settings levels rather than replacing each
//!   other", so an injected `hooks` key adds to the user's hooks.
//! * "If you define the same handler in more than one settings file, it runs
//!   once. A plugin's or skill's copy of the same handler stays separate."
//!   So a session whose configuration also enables the plugin runs the hook
//!   twice. That configuration cannot be known here: a shell wrapper may set
//!   `CLAUDE_CONFIG_DIR` after GitPulse hands over the command line, and a
//!   guess that the plugin is enabled leaves the session ungated — the very
//!   case this exists for. So the hook always goes on, and the hook itself
//!   makes the second run harmless: every check is a read, both copies reach
//!   the same decision, an identical warning for one call is said once
//!   (`hooks::advisory_once`), and a notification said twice is folded by the
//!   order book (`alerts::intake::OrderBook::repeats`).
//!
//! The hook entries are not written out a second time here: they are the
//! plugin manifest's own `PreToolUse` entries with the bare `gitpulse-hook`
//! name replaced by the absolute path of the binary GitPulse ships beside
//! itself. A bare name is not reachable from the released app's PATH, and a
//! mistyped path in settings leaves the gate silently disabled, which is why
//! a missing binary is reported rather than injected.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// The plugin's hook manifest: the one owner of which hooks run and when.
const PLUGIN_HOOKS: &str = include_str!("../../../plugins/gitpulse/hooks/hooks.json");

/// The hook events injected, by the plugin manifest's own name for each.
///
/// `PreToolUse` is the safety gate. The rest are the notification hooks that
/// say what a session is doing: a question asked (`PermissionRequest`), a tool
/// run (`PostToolUse`, `PostToolUseFailure`), a prompt answered
/// (`UserPromptSubmit`), a turn ended (`Stop`) and a session ended
/// (`SessionEnd`). Without them a Claude launched under a configuration that
/// does not enable the plugin reports only a bell, which says something
/// happened and never what, and stays "signalled" until someone types. They
/// only report: `gitpulse-hook notify` writes nothing to stdout, so it can
/// neither allow nor deny anything.
///
/// `Stop` also carries `gitpulse-hook deliver`, the `asyncRewake` entry that
/// lets a person's message reach an idle session (`hooks::deliver`). It rides
/// in with the rest of `Stop`'s manifest entries, unchanged but for the binary's
/// path: its `timeout` (1800 s) and `asyncRewake` are the manifest's own, and
/// `the_stop_event_carries_the_waiter_in_the_manifest_and_in_every_launch`
/// holds both copies to them. Two copies of it can run for one stop; the
/// mailbox's one-use claim is what keeps that to one wake.
///
/// Left to the plugin: the `Notification` matchers and `StopFailure`, which
/// are banners, and `SessionStart`'s brief. A session without the plugin gets
/// its banners from the bell it already rings.
///
/// A session that does enable the plugin runs both copies of each. The
/// `PreToolUse` copies are made harmless by `hooks::advisory_once`; the
/// notification copies by the order book (`alerts::intake`), which folds one
/// event reported twice into one report.
const INJECTED_EVENTS: [&str; 7] = [
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "UserPromptSubmit",
    "Stop",
    "SessionEnd",
];

/// The `timeout` of the injected `SessionEnd` hook, in seconds.
///
/// The hooks reference (<https://code.claude.com/docs/en/hooks>) says:
/// "`SessionEnd` hooks share a 1.5-second budget; if your settings set a longer
/// per-hook `timeout`, Claude Code raises the budget to match, up to 60
/// seconds." A `timeout: 5` on `SessionEnd` would be a promise to hold every
/// exit of that session open for up to 5 s whenever the hook is slow, so the
/// plugin manifest's copy has the same 1 s (`hooks::notify_input_tests` and
/// `plugin-contract.test.ts` fail if either exceeds 1.5 s). The docs do not
/// say `SessionEnd` takes `async: true` (they say an async hook is killed at
/// teardown in `-p` mode and give no list of events), so this does not rely
/// on it: both copies ask for less than the default budget, so neither ever
/// raises it. The report is a few
/// milliseconds when GitPulse is up; a slow one is lost, and the process-exit
/// `session_ended` GitPulse records itself covers the session.
pub(crate) const SESSION_END_TIMEOUT_SECS: u64 = crate::hooks::SESSION_END_HOST_TIMEOUT.as_secs();

/// The name the plugin manifest invokes the hook by.
const HOOK_NAME: &str = "gitpulse-hook";

/// What a launch does about the hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookInjection {
    /// Put the hook on this launch, run from this binary.
    Inject(PathBuf),
    /// The provider takes no hooks.
    NotApplicable,
    /// The binary could not be found. The launch goes ahead without the hook
    /// and the reason is reported.
    BinaryMissing(String),
}

impl HookInjection {
    /// The binary to inject, if this launch injects.
    pub(crate) fn binary(&self) -> Option<&Path> {
        match self {
            Self::Inject(path) => Some(path),
            _ => None,
        }
    }
}

/// Whether `path` is a file this user could execute.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// The hook binary: `GITPULSE_HOOK_PATH`, else the one beside this
/// executable (the app bundle's `Contents/MacOS`, or `target/<profile>` in
/// development) — the way the app finds `gitpulse-mcp`. An explicit path that
/// is not a file ends the search rather than falling through to a different
/// binary than the one asked for.
pub(crate) fn resolve_binary(
    explicit: Option<&Path>,
    exe: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit {
        return if is_executable_file(explicit) {
            Ok(explicit.to_path_buf())
        } else {
            Err(format!(
                "GITPULSE_HOOK_PATH is {}, which is not an executable file",
                explicit.display()
            ))
        };
    }
    let name = if cfg!(windows) {
        "gitpulse-hook.exe"
    } else {
        HOOK_NAME
    };
    let beside = exe.and_then(Path::parent).map(|dir| dir.join(name));
    match beside {
        Some(path) if is_executable_file(&path) => Ok(path),
        Some(path) => Err(format!("{HOOK_NAME} is not at {}", path.display())),
        None => Err("this app's own path could not be determined".to_owned()),
    }
}

/// The pure decision: inject the binary that was found, or say why not.
pub(crate) fn decide(binary: Result<PathBuf, String>) -> HookInjection {
    match binary {
        Ok(path) => HookInjection::Inject(path),
        Err(reason) => HookInjection::BinaryMissing(reason),
    }
}

/// What `provider`'s launch does about the hook. Only Claude Code takes
/// hooks.
///
/// A missing binary is logged here, once per launch, because nothing else
/// will say so: the launch proceeds and the agent runs ungated.
pub(crate) fn for_launch(provider: &str) -> HookInjection {
    if provider != "claude" {
        return HookInjection::NotApplicable;
    }
    let explicit = std::env::var_os("GITPULSE_HOOK_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    let decision = decide(resolve_binary(explicit.as_deref(), exe.as_deref()));
    if let HookInjection::BinaryMissing(reason) = &decision {
        log::warn!(
            target: "workbench",
            "claude launches without the GitPulse command gate: {reason}"
        );
    }
    decision
}

/// POSIX single-quoting, so a path with spaces or quotes stays one word of
/// the shell command a hook runs as. Windows hooks are never shell form (see
/// [`settings_hooks_for`]), so this is the only quoting there is.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// The `hooks` value for a `--settings` object: the plugin manifest's
/// injected events with the hook invoked by absolute path.
pub(crate) fn settings_hooks(binary: &Path) -> Result<Value, String> {
    settings_hooks_for(binary, cfg!(windows))
}

/// [`settings_hooks`] for the platform `windows` names, so a test on one
/// platform can check the other's.
///
/// On Windows each handler is made by the same generator as the hooks.json the
/// Settings page copies, [`crate::insights::windows_hook_handler`]: Claude
/// Code's documented exec form (`command` is the absolute path, `args` the
/// subcommand words, no shell), with `"onFailure": "block"` on the deciding
/// hooks (`collision-guard`, `command-gate`) and none on the notify-only ones
/// (`notify`, `deliver`), whose loss costs a report, never a verdict. A quoted
/// path followed by bare words is not a command PowerShell can run, and a host
/// without Git Bash runs shell-form hooks under PowerShell, which blocked every
/// tool call. The Claude Code versions involved (`onFailure` needs 2.1.295, and
/// what an older host does) are recorded on that function. Unix keeps the
/// POSIX-quoted shell form.
pub(crate) fn settings_hooks_for(binary: &Path, windows: bool) -> Result<Value, String> {
    let manifest: Value = serde_json::from_str(PLUGIN_HOOKS)
        .map_err(|e| format!("the plugin hook manifest is not JSON: {e}"))?;
    let events = manifest
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or("the plugin hook manifest has no hooks object")?;
    let binary = binary.to_string_lossy();
    let quoted = shell_quote(&binary);
    let prefix = format!("{HOOK_NAME} ");
    let mut out = Map::new();
    for event in INJECTED_EVENTS {
        let mut groups = events
            .get(event)
            .cloned()
            .ok_or_else(|| format!("the plugin hook manifest has no {event} hooks"))?;
        let mut rewritten = 0;
        for group in groups.as_array_mut().into_iter().flatten() {
            for hook in group
                .get_mut("hooks")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                let command = hook
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if let Some(rest) = command.strip_prefix(&prefix) {
                    if windows {
                        crate::insights::windows_hook_handler(hook, &binary, rest)?;
                    } else {
                        hook["command"] = json!(format!("{quoted} {rest}"));
                    }
                    if event == "SessionEnd" {
                        hook["timeout"] = json!(SESSION_END_TIMEOUT_SECS);
                    }
                    rewritten += 1;
                }
            }
        }
        if rewritten == 0 {
            return Err(format!(
                "no {event} hook in the plugin manifest runs {HOOK_NAME}"
            ));
        }
        out.insert(event.to_owned(), groups);
    }
    Ok(Value::Object(out))
}

/// Lays `addition` (a `hooks` object) over `existing` event by event: a hook
/// the user's own `--settings` already defines stays, ours is added after it.
pub(crate) fn merge_hooks(existing: Option<&Value>, addition: Value) -> Value {
    let mut merged = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Value::Object(addition) = addition {
        for (event, groups) in addition {
            let slot = merged.entry(event).or_insert_with(|| json!([]));
            if let (Some(have), Some(add)) = (slot.as_array_mut(), groups.as_array()) {
                for group in add {
                    if !have.contains(group) {
                        have.push(group.clone());
                    }
                }
            }
        }
    }
    Value::Object(merged)
}

#[cfg(all(test, unix))]
mod notify_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gitpulse-claude-hook-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, relative: &str, text: &str) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn hook() -> Result<PathBuf, String> {
        Ok(PathBuf::from(
            "/Applications/GitPulse.app/Contents/MacOS/gitpulse-hook",
        ))
    }

    /// The incident case: the seat a wrapper picks may not enable the
    /// plugin even when every configuration GitPulse can see does. So the
    /// launch entry point reads no configuration at all: given the binary, a
    /// claude launch is injected.
    #[test]
    fn every_claude_launch_is_injected_when_the_binary_is_there() {
        let dir = temp("launch");
        let binary = dir.join("gitpulse-hook");
        write(&dir, "gitpulse-hook", "#!/bin/sh\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Only `for_launch` reads this variable. Bound through the env owner so
        // the write is serialized against every other test and restored even if
        // an assertion fails before the end of the test.
        let decision = {
            let serial = crate::test_support::env::env_serial();
            let _env =
                crate::test_support::env::bind_env(&serial).set("GITPULSE_HOOK_PATH", &binary);
            for_launch("claude")
        };
        assert_eq!(decision, HookInjection::Inject(binary));
    }

    #[test]
    fn a_missing_binary_is_reported_not_injected() {
        let decision = decide(Err("gitpulse-hook is not at /x".into()));
        assert_eq!(
            decision,
            HookInjection::BinaryMissing("gitpulse-hook is not at /x".into())
        );
        assert!(decision.binary().is_none());
    }

    /// Lockstep with the hook: the checking subcommands injected here are
    /// exactly the ones the hook deduplicates advisories for. One injected but
    /// not deduplicated would print every warning twice; one deduplicated but
    /// not injected is dead. (`notify` is a report, folded by the order book
    /// instead; see `notify_tests`.)
    #[test]
    fn the_injected_subcommands_are_the_ones_the_hook_deduplicates() {
        let hooks = settings_hooks(&hook().unwrap()).unwrap();
        let mut injected: Vec<String> = hooks["PreToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|group| group["hooks"].as_array().unwrap().clone())
            .map(|hook| {
                let command = hook["command"].as_str().unwrap().to_owned();
                command.rsplit(' ').next().unwrap().to_owned()
            })
            .collect();
        injected.sort();
        let mut deduped: Vec<String> = crate::hooks::INJECTED_SUBCOMMANDS
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        deduped.sort();
        assert_eq!(injected, deduped);
    }

    #[test]
    fn the_binary_is_found_beside_the_app_and_a_missing_one_says_where_it_looked() {
        let dir = temp("binary");
        let exe = dir.join("gitpulse");
        let hook_path = dir.join(if cfg!(windows) {
            "gitpulse-hook.exe"
        } else {
            "gitpulse-hook"
        });
        let err = resolve_binary(None, Some(&exe)).unwrap_err();
        assert!(err.contains(&hook_path.display().to_string()), "{err}");
        write(
            &dir,
            hook_path.file_name().unwrap().to_str().unwrap(),
            "#!/bin/sh\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Present but not executable is as good as missing.
            assert!(resolve_binary(None, Some(&exe)).is_err());
            std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(resolve_binary(None, Some(&exe)), Ok(hook_path.clone()));
        assert!(resolve_binary(None, None).is_err());
        // An explicit path that is wrong does not fall back to the sibling.
        let wrong = dir.join("elsewhere");
        let err = resolve_binary(Some(&wrong), Some(&exe)).unwrap_err();
        assert!(err.contains("GITPULSE_HOOK_PATH"), "{err}");
        assert_eq!(resolve_binary(Some(&hook_path), None), Ok(hook_path));
    }

    #[test]
    fn the_injected_hooks_are_the_manifests_pre_tool_use_by_absolute_path() {
        let binary = Path::new("/Applications/Git Pulse.app/Contents/MacOS/gitpulse-hook");
        let hooks = settings_hooks(binary).unwrap();
        let mut keys: Vec<&str> = hooks
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let mut events = INJECTED_EVENTS.to_vec();
        events.sort_unstable();
        assert_eq!(keys, events);
        let manifest: Value = serde_json::from_str(PLUGIN_HOOKS).unwrap();
        let shipped = manifest["hooks"]["PreToolUse"].as_array().unwrap();
        let injected = hooks["PreToolUse"].as_array().unwrap();
        assert_eq!(injected.len(), shipped.len());
        let mut subcommands = Vec::new();
        for (injected, shipped) in injected.iter().zip(shipped) {
            assert_eq!(injected["matcher"], shipped["matcher"]);
            let command = injected["hooks"][0]["command"].as_str().unwrap();
            let original = shipped["hooks"][0]["command"].as_str().unwrap();
            let subcommand = original.strip_prefix("gitpulse-hook ").unwrap();
            assert_eq!(
                command,
                format!("'/Applications/Git Pulse.app/Contents/MacOS/gitpulse-hook' {subcommand}")
            );
            assert_eq!(
                injected["hooks"][0]["timeout"],
                shipped["hooks"][0]["timeout"]
            );
            subcommands.push(subcommand.to_owned());
        }
        subcommands.sort();
        assert_eq!(subcommands, ["collision-guard", "command-gate"]);
    }

    /// The injected Windows hooks are the documented exec form: `command` the
    /// bare executable path, `args` the words, no shell, nothing quoted. A
    /// quoted path plus bare words is a PowerShell parse error, which blocked
    /// every tool call on a host without Git Bash. The deciding hooks carry
    /// `onFailure: block` (Claude Code >= 2.1.295; an older host ignores it and
    /// lets a hook that cannot start through, the stated F2 policy, recorded on
    /// `windows_hook_handler`); the notify-only ones carry none. Built by the
    /// OS-parameterised function; not executed on Windows.
    #[test]
    fn the_injected_windows_hooks_are_exec_form_with_on_failure_on_the_deciding_ones() {
        let binary = r"C:\Program Files\GitPulse\gitpulse-hook.exe";
        let hooks = settings_hooks_for(Path::new(binary), true).unwrap();
        let pre = hooks["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        for (group, sub) in pre.iter().zip(["collision-guard", "command-gate"]) {
            assert_eq!(
                group["hooks"][0],
                json!({
                    "type": "command",
                    "command": binary,
                    "args": [sub],
                    "onFailure": "block",
                    "timeout": group["hooks"][0]["timeout"],
                }),
                "{sub}"
            );
        }
        for (event, groups) in hooks.as_object().unwrap() {
            for group in groups.as_array().unwrap() {
                for handler in group["hooks"].as_array().unwrap() {
                    let sub = handler["args"][0].as_str().unwrap();
                    assert_eq!(handler["command"], binary, "{event}");
                    assert!(handler.get("shell").is_none(), "{event}");
                    assert_eq!(
                        handler.get("onFailure").is_some(),
                        matches!(sub, "collision-guard" | "command-gate"),
                        "{event} {sub}"
                    );
                }
            }
        }
        assert_eq!(
            hooks["SessionEnd"][0]["hooks"][0]["timeout"],
            json!(SESSION_END_TIMEOUT_SECS)
        );
    }

    /// A path PowerShell-or-not cannot be carried as one `command` string is
    /// refused, not emitted: an injected hook is never half-formed.
    #[test]
    fn a_windows_path_with_a_control_character_is_refused() {
        assert!(settings_hooks_for(Path::new("C:\\a\nb\\gitpulse-hook.exe"), true).is_err());
    }

    #[test]
    fn a_quote_in_the_path_cannot_end_the_quoting() {
        let hooks = settings_hooks(Path::new("/tmp/it's/gitpulse-hook")).unwrap();
        let command = hooks["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            command.starts_with(r"'/tmp/it'\''s/gitpulse-hook' "),
            "{command}"
        );
    }

    #[test]
    fn merging_keeps_the_callers_hooks_and_adds_ours_once() {
        let ours = settings_hooks(Path::new("/bin/gitpulse-hook")).unwrap();
        let theirs = json!({"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "mine"}]}],
                            "Stop": [{"hooks": []}]});
        let merged = merge_hooks(Some(&theirs), ours.clone());
        assert_eq!(merged["PreToolUse"].as_array().unwrap().len(), 3);
        assert_eq!(merged["PreToolUse"][0]["hooks"][0]["command"], "mine");
        assert!(merged.get("Stop").is_some());
        assert_eq!(merge_hooks(Some(&merged), ours.clone()), merged);
        assert_eq!(merge_hooks(None, ours.clone()), ours);
    }

    /// The mailbox waiter is a `Stop` entry marked `asyncRewake` with a
    /// `timeout` Claude Code enforces (default 600 s when none is given). Both
    /// the plugin's copy and the one every launch carries must have it, must not
    /// be `async` (which cannot wake), and must outlast the hook's own wait.
    #[test]
    fn the_stop_event_carries_the_waiter_in_the_manifest_and_in_every_launch() {
        let manifest: Value = serde_json::from_str(PLUGIN_HOOKS).unwrap();
        let injected =
            settings_hooks(Path::new("/Applications/Git Pulse.app/gitpulse-hook")).unwrap();
        for (label, stop) in [
            ("manifest", &manifest["hooks"]["Stop"]),
            ("injected", &injected["Stop"]),
        ] {
            let handlers: Vec<&Value> = stop
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|group| group["hooks"].as_array().unwrap())
                .collect();
            let waiters: Vec<&&Value> = handlers
                .iter()
                .filter(|hook| hook["command"].as_str().unwrap().ends_with(" deliver"))
                .collect();
            assert_eq!(waiters.len(), 1, "{label}: {handlers:?}");
            let waiter = waiters[0];
            assert_eq!(waiter["asyncRewake"], json!(true), "{label}");
            assert!(
                waiter.get("async").is_none(),
                "{label}: an async hook cannot wake"
            );
            let timeout = waiter["timeout"]
                .as_u64()
                .unwrap_or_else(|| panic!("{label}: no timeout"));
            assert!(timeout <= 1800, "{label}: {timeout}");
            assert!(
                timeout > crate::hooks::deliver::DELIVER_WAIT.as_secs(),
                "{label}: the host would kill the waiter before it gave up"
            );
            // The existing report is still there, still in the background.
            let report = handlers
                .iter()
                .find(|hook| {
                    hook["command"]
                        .as_str()
                        .unwrap()
                        .ends_with("notify turn_finished")
                })
                .unwrap_or_else(|| panic!("{label}: turn_finished is gone"));
            assert_eq!(report["async"], json!(true), "{label}");
            assert!(report.get("asyncRewake").is_none(), "{label}");
        }
        let command = injected["Stop"][0]["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hook| hook["command"].as_str().unwrap().to_owned())
            .find(|command| command.ends_with(" deliver"))
            .unwrap();
        assert_eq!(
            command,
            "'/Applications/Git Pulse.app/gitpulse-hook' deliver"
        );
    }

    #[test]
    fn other_providers_are_never_injected() {
        assert_eq!(for_launch("codex"), HookInjection::NotApplicable);
    }

    /// Adversary: base 0ea2cc37's Unix renderer, reproduced literally, equals
    /// the new one for awkward paths.
    #[test]
    fn adv_unix_output_is_byte_identical_to_the_base_renderer() {
        let manifest: Value = serde_json::from_str(PLUGIN_HOOKS).unwrap();
        for binary in [
            "/opt/gitpulse/gitpulse-hook",
            "/Applications/Git Pulse.app/Contents/MacOS/gitpulse-hook",
            "/tmp/it's/a\"b$c`d/gitpulse-hook",
            "/tmp/\u{e9}\u{4e16}/gitpulse-hook",
        ] {
            let quoted = format!("'{}'", binary.replace('\'', r"'\''"));
            let mut out = Map::new();
            for event in INJECTED_EVENTS {
                let mut groups = manifest["hooks"][event].clone();
                for group in groups.as_array_mut().unwrap() {
                    for hook in group["hooks"].as_array_mut().unwrap() {
                        let command = hook["command"].as_str().unwrap().to_owned();
                        if let Some(rest) = command.strip_prefix("gitpulse-hook ") {
                            hook["command"] = json!(format!("{quoted} {rest}"));
                            if event == "SessionEnd" {
                                hook["timeout"] = json!(SESSION_END_TIMEOUT_SECS);
                            }
                        }
                    }
                }
                out.insert(event.to_owned(), groups);
            }
            let expected = serde_json::to_string(&Value::Object(out)).unwrap();
            let got = settings_hooks_for(Path::new(binary), false).unwrap();
            assert_eq!(serde_json::to_string(&got).unwrap(), expected, "{binary}");
            assert_eq!(
                serde_json::to_string(&settings_hooks(Path::new(binary)).unwrap()).unwrap(),
                if cfg!(windows) {
                    serde_json::to_string(&settings_hooks_for(Path::new(binary), true).unwrap())
                        .unwrap()
                } else {
                    expected
                },
            );
        }
    }

    /// Adversary: awkward Windows paths ride in `command` verbatim, survive a
    /// JSON round trip, and the deciding set / timeout / async fields match the
    /// shipped hooks.json exactly.
    #[test]
    fn adv_windows_paths_are_verbatim_and_fields_survive() {
        let manifest: Value = serde_json::from_str(PLUGIN_HOOKS).unwrap();
        let long = format!(r"C:\{}\gitpulse-hook.exe", "d".repeat(4000));
        for binary in [
            r"C:\Program Files (x86)\Git Pulse & Co\100%\^caret\it's\gitpulse-hook.exe",
            r"\\server\share\Git Pulse\gitpulse-hook.exe",
            "C:\\caf\u{e9}\\\u{4e16}\u{754c}\\gitpulse-hook.exe",
            r#"C:\a"b\gitpulse-hook.exe"#,
            r"C:\trailing\",
            long.as_str(),
        ] {
            let hooks = settings_hooks_for(Path::new(binary), true).unwrap();
            let round: Value =
                serde_json::from_str(&serde_json::to_string(&hooks).unwrap()).unwrap();
            assert_eq!(round, hooks);
            for event in INJECTED_EVENTS {
                let src = manifest["hooks"][event].as_array().unwrap();
                let got = hooks[event].as_array().unwrap();
                assert_eq!(src.len(), got.len(), "{event}");
                for (sg, gg) in src.iter().zip(got) {
                    assert_eq!(sg["matcher"], gg["matcher"], "{event}");
                    for (sh, gh) in sg["hooks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .zip(gg["hooks"].as_array().unwrap())
                    {
                        let cmd = sh["command"].as_str().unwrap();
                        let rest = cmd.strip_prefix("gitpulse-hook ").unwrap();
                        assert_eq!(gh["command"], binary);
                        let words: Vec<&str> = rest.split_whitespace().collect();
                        assert_eq!(gh["args"], json!(words));
                        let deciding = matches!(words[0], "collision-guard" | "command-gate");
                        assert_eq!(
                            gh["onFailure"] == json!("block"),
                            deciding,
                            "{event} {rest}"
                        );
                        for key in ["async", "asyncRewake", "matcher", "statusMessage", "if"] {
                            assert_eq!(sh.get(key), gh.get(key), "{event} {rest} {key}");
                        }
                        if event != "SessionEnd" {
                            assert_eq!(sh["timeout"], gh["timeout"], "{event} {rest}");
                        }
                    }
                }
            }
        }
    }

    /// Adversary: every control character refuses (no omitted hook), for every
    /// event, and the refusal is an Err, not a smaller object.
    #[test]
    fn adv_windows_control_characters_refuse_the_whole_injection() {
        for bad in ["\n", "\r", "\0", "\u{7f}", "\t", "\u{1b}", "\u{85}"] {
            let path = format!("C:\\a{bad}b\\gitpulse-hook.exe");
            assert!(
                settings_hooks_for(Path::new(&path), true).is_err(),
                "{bad:?}"
            );
        }
        assert!(settings_hooks_for(Path::new(""), true).is_err());
    }
}
