//! Putting `gitpulse-hook` on every Claude Code session GitPulse launches.
//!
//! The shipped plugin (`plugins/gitpulse/hooks/hooks.json`) runs the hook, but
//! only in a Claude Code configuration directory that enables the plugin. A
//! session launched under `CLAUDE_CONFIG_DIR=~/.claude-chandra` has no hooks at
//! all, so the command gate never sees what that agent runs. GitPulse already
//! passes every launch a session-scoped `--settings` object
//! ([`super::terminal_command::notify_flags`]); this adds the same PreToolUse
//! hooks to it.
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
//!   So a plugin that is also enabled runs the hook a second time, and the
//!   duplicate cannot be left to Claude Code. [`decide`] therefore skips the
//!   injection when the configuration the launch will use enables the plugin.
//!
//! The hook entries are not written out a second time here: they are the
//! plugin manifest's own `PreToolUse` entries with the bare `gitpulse-hook`
//! name replaced by the absolute path of the binary GitPulse ships beside
//! itself. A bare name is not reachable from the released app's PATH, and a
//! mistyped path in settings leaves the gate silently disabled, which is why
//! a missing binary is reported rather than injected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// The plugin's hook manifest: the one owner of which hooks run and when.
const PLUGIN_HOOKS: &str = include_str!("../../../plugins/gitpulse/hooks/hooks.json");

/// The hook events injected. The safety gate is `PreToolUse`; the plugin's
/// notification and session-brief hooks stay the plugin's.
const INJECTED_EVENTS: [&str; 1] = ["PreToolUse"];

/// The name the plugin manifest invokes the hook by.
const HOOK_NAME: &str = "gitpulse-hook";

/// What a launch does about the hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookInjection {
    /// Put the hook on this launch, run from this binary.
    Inject(PathBuf),
    /// The configuration this launch uses already enables the plugin, which
    /// runs the hook; injecting it too would run it twice.
    PluginEnabled,
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

/// Whether `settings` (a parsed `settings.json`) enables a GitPulse plugin.
/// `Some(false)` is an explicit disable, which a higher-precedence scope
/// uses to switch off what a lower one enabled.
fn plugin_flag(settings: &Value) -> Option<bool> {
    let enabled = settings.get("enabledPlugins")?.as_object()?;
    let mut found = None;
    for (name, value) in enabled {
        if name.split('@').next() == Some("gitpulse") && name.contains('@') {
            match value.as_bool() {
                Some(true) => return Some(true),
                Some(false) => found = Some(false),
                None => {}
            }
        }
    }
    found
}

fn read_settings(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether the plugin is enabled for a launch.
///
/// Reads the scopes Claude Code merges for a session — user (`config_dir`),
/// project and project-local (`project_dir/.claude`) — in increasing
/// precedence, so a project that disables what the user enabled is honoured.
/// `sources` is the user's `--setting-sources` narrowing: a scope it leaves
/// out is not loaded by the launch, so it cannot enable anything.
///
/// A file that is absent, unreadable or not JSON enables nothing. The cost of
/// that reading is a hook that runs twice, never a gate that does not run.
fn plugin_enabled(
    config_dir: Option<&Path>,
    project_dir: Option<&Path>,
    sources: Option<&str>,
) -> bool {
    let loads = |scope: &str| {
        sources.is_none_or(|list| list.split(',').any(|source| source.trim() == scope))
    };
    let mut enabled = false;
    let mut scopes: Vec<(&str, Option<PathBuf>)> = vec![
        ("user", config_dir.map(|dir| dir.join("settings.json"))),
        (
            "project",
            project_dir.map(|dir| dir.join(".claude").join("settings.json")),
        ),
        (
            "local",
            project_dir.map(|dir| dir.join(".claude").join("settings.local.json")),
        ),
    ];
    for (scope, path) in scopes.drain(..) {
        if !loads(scope) {
            continue;
        }
        if let Some(flag) = path
            .as_deref()
            .and_then(read_settings)
            .as_ref()
            .and_then(plugin_flag)
        {
            enabled = flag;
        }
    }
    enabled
}

/// The configuration directory a Claude Code launch will use:
/// `CLAUDE_CONFIG_DIR` from the launch's own environment, else the app's,
/// else `~/.claude`. A relative value is Claude Code's to resolve against its
/// working directory, so it is resolved against `cwd` here; with no `cwd` it
/// cannot be placed and the answer is `None`.
pub(crate) fn effective_config_dir(
    launch_env: Option<&HashMap<String, String>>,
    process_dir: Option<PathBuf>,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    let explicit = launch_env
        .and_then(|env| env.get("CLAUDE_CONFIG_DIR"))
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    let dir = explicit.or(process_dir)?;
    if dir.is_absolute() {
        Some(dir)
    } else {
        cwd.map(|cwd| cwd.join(dir))
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

/// The pure decision: what a launch with this configuration does.
pub(crate) fn decide(
    config_dir: Option<&Path>,
    project_dir: Option<&Path>,
    sources: Option<&str>,
    binary: Result<PathBuf, String>,
) -> HookInjection {
    if plugin_enabled(config_dir, project_dir, sources) {
        return HookInjection::PluginEnabled;
    }
    match binary {
        Ok(path) => HookInjection::Inject(path),
        Err(reason) => HookInjection::BinaryMissing(reason),
    }
}

/// What `provider`'s launch in `cwd` does about the hook, read from the
/// environment. Only Claude Code takes hooks.
///
/// A missing binary is logged here, once per launch, because nothing else
/// will say so: the launch proceeds and the agent runs ungated.
pub(crate) fn for_launch(
    provider: &str,
    launch_env: Option<&HashMap<String, String>>,
    cwd: Option<&Path>,
    sources: Option<&str>,
) -> HookInjection {
    if provider != "claude" {
        return HookInjection::NotApplicable;
    }
    let process_dir = super::conversation::claude_home();
    let config_dir = effective_config_dir(launch_env, process_dir, cwd);
    let explicit = std::env::var_os("GITPULSE_HOOK_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    let decision = decide(
        config_dir.as_deref(),
        cwd,
        sources,
        resolve_binary(explicit.as_deref(), exe.as_deref()),
    );
    if let HookInjection::BinaryMissing(reason) = &decision {
        log::warn!(
            target: "workbench",
            "claude launches without the GitPulse command gate: {reason}"
        );
    }
    decision
}

/// POSIX single-quoting, so a path with spaces or quotes stays one word of
/// the shell command a hook runs as.
fn shell_quote(path: &str) -> String {
    if cfg!(windows) {
        format!("\"{path}\"")
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// The `hooks` value for a `--settings` object: the plugin manifest's
/// injected events with the hook invoked by absolute path.
pub(crate) fn settings_hooks(binary: &Path) -> Result<Value, String> {
    let manifest: Value = serde_json::from_str(PLUGIN_HOOKS)
        .map_err(|e| format!("the plugin hook manifest is not JSON: {e}"))?;
    let events = manifest
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or("the plugin hook manifest has no hooks object")?;
    let quoted = shell_quote(&binary.to_string_lossy());
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
                let command = hook.get("command").and_then(Value::as_str).unwrap_or("");
                if let Some(rest) = command.strip_prefix(&prefix) {
                    hook["command"] = json!(format!("{quoted} {rest}"));
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

    const ENABLED: &str = r#"{"enabledPlugins":{"gitpulse@gitpulse":true}}"#;

    fn hook() -> Result<PathBuf, String> {
        Ok(PathBuf::from(
            "/Applications/GitPulse.app/Contents/MacOS/gitpulse-hook",
        ))
    }

    #[test]
    fn a_config_dir_without_the_plugin_gets_the_hook() {
        let dir = temp("without");
        write(
            &dir,
            "settings.json",
            r#"{"enabledPlugins":{"other@x":true}}"#,
        );
        assert_eq!(
            decide(Some(&dir), None, None, hook()),
            HookInjection::Inject(hook().unwrap())
        );
    }

    #[test]
    fn a_config_dir_with_the_plugin_enabled_is_not_injected_twice() {
        let dir = temp("with");
        write(&dir, "settings.json", ENABLED);
        assert_eq!(
            decide(Some(&dir), None, None, hook()),
            HookInjection::PluginEnabled
        );
    }

    #[test]
    fn a_plugin_listed_but_disabled_is_injected() {
        let dir = temp("disabled");
        write(
            &dir,
            "settings.json",
            r#"{"enabledPlugins":{"gitpulse@gitpulse":false}}"#,
        );
        assert!(matches!(
            decide(Some(&dir), None, None, hook()),
            HookInjection::Inject(_)
        ));
    }

    #[test]
    fn the_plugin_under_any_marketplace_name_counts() {
        let dir = temp("marketplace");
        write(
            &dir,
            "settings.json",
            r#"{"enabledPlugins":{"gitpulse@elsewhere":true}}"#,
        );
        assert_eq!(
            decide(Some(&dir), None, None, hook()),
            HookInjection::PluginEnabled
        );
    }

    #[test]
    fn a_similarly_named_plugin_does_not_count() {
        let dir = temp("lookalike");
        write(
            &dir,
            "settings.json",
            r#"{"enabledPlugins":{"gitpulse-extras@x":true,"gitpulse":true}}"#,
        );
        assert!(matches!(
            decide(Some(&dir), None, None, hook()),
            HookInjection::Inject(_)
        ));
    }

    #[test]
    fn a_missing_nonexistent_or_malformed_settings_file_is_injected() {
        let missing = temp("no-file");
        assert!(matches!(
            decide(Some(&missing), None, None, hook()),
            HookInjection::Inject(_)
        ));
        let nonexistent = missing.join("never-created");
        assert!(matches!(
            decide(Some(&nonexistent), None, None, hook()),
            HookInjection::Inject(_)
        ));
        assert!(matches!(
            decide(None, None, None, hook()),
            HookInjection::Inject(_)
        ));
        let malformed = temp("malformed");
        write(&malformed, "settings.json", "{ not json");
        assert!(matches!(
            decide(Some(&malformed), None, None, hook()),
            HookInjection::Inject(_)
        ));
        let wrong_shape = temp("wrong-shape");
        write(
            &wrong_shape,
            "settings.json",
            r#"{"enabledPlugins":["gitpulse@gitpulse"]}"#,
        );
        assert!(matches!(
            decide(Some(&wrong_shape), None, None, hook()),
            HookInjection::Inject(_)
        ));
    }

    #[test]
    fn a_project_scope_can_enable_or_disable_what_the_user_set() {
        let user = temp("scope-user");
        let project = temp("scope-project");
        write(&project, ".claude/settings.json", ENABLED);
        assert_eq!(
            decide(Some(&user), Some(&project), None, hook()),
            HookInjection::PluginEnabled
        );
        write(&user, "settings.json", ENABLED);
        write(
            &project,
            ".claude/settings.local.json",
            r#"{"enabledPlugins":{"gitpulse@gitpulse":false}}"#,
        );
        assert!(matches!(
            decide(Some(&user), Some(&project), None, hook()),
            HookInjection::Inject(_)
        ));
    }

    #[test]
    fn a_scope_the_setting_sources_leave_out_is_not_read() {
        let user = temp("sources-user");
        write(&user, "settings.json", ENABLED);
        assert!(matches!(
            decide(Some(&user), None, Some("project,local"), hook()),
            HookInjection::Inject(_)
        ));
        assert_eq!(
            decide(Some(&user), None, Some("user, project"), hook()),
            HookInjection::PluginEnabled
        );
    }

    #[test]
    fn a_missing_binary_is_reported_not_injected() {
        let dir = temp("no-binary");
        let decision = decide(
            Some(&dir),
            None,
            None,
            Err("gitpulse-hook is not at /x".into()),
        );
        assert_eq!(
            decision,
            HookInjection::BinaryMissing("gitpulse-hook is not at /x".into())
        );
        assert!(decision.binary().is_none());
    }

    #[test]
    fn an_enabled_plugin_wins_over_a_missing_binary() {
        let dir = temp("plugin-no-binary");
        write(&dir, "settings.json", ENABLED);
        assert_eq!(
            decide(Some(&dir), None, None, Err("missing".into())),
            HookInjection::PluginEnabled
        );
    }

    #[test]
    fn the_config_dir_comes_from_the_launch_env_then_the_process_then_home() {
        let env: HashMap<String, String> =
            [("CLAUDE_CONFIG_DIR".to_owned(), "/seat".to_owned())].into();
        let home = Some(PathBuf::from("/home/.claude"));
        assert_eq!(
            effective_config_dir(Some(&env), home.clone(), None),
            Some(PathBuf::from("/seat"))
        );
        assert_eq!(effective_config_dir(None, home.clone(), None), home);
        let empty: HashMap<String, String> =
            [("CLAUDE_CONFIG_DIR".to_owned(), String::new())].into();
        assert_eq!(effective_config_dir(Some(&empty), home.clone(), None), home);
        assert_eq!(effective_config_dir(None, None, None), None);
    }

    #[test]
    fn a_relative_config_dir_is_placed_by_the_working_directory_or_not_at_all() {
        let env: HashMap<String, String> =
            [("CLAUDE_CONFIG_DIR".to_owned(), "seat".to_owned())].into();
        assert_eq!(
            effective_config_dir(Some(&env), None, Some(Path::new("/work/repo"))),
            Some(PathBuf::from("/work/repo/seat"))
        );
        assert_eq!(effective_config_dir(Some(&env), None, None), None);
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
        assert_eq!(
            hooks.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["PreToolUse"]
        );
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

    #[test]
    fn other_providers_are_never_injected() {
        assert_eq!(
            for_launch("codex", None, None, None),
            HookInjection::NotApplicable
        );
    }
}
