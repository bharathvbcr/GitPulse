//! Managed Antigravity (`agy`) Agent Plugins 1.0.0 sync (`~/.gemini/config/plugins/gitpulse`).
//!
//! Why this exists:
//! - Claude Code accepts per-launch `--settings '{"hooks": ...}'` on the CLI,
//!   so `claude_hook.rs` injects `gitpulse-hook` without touching disk.
//! - Antigravity (`agy`) discovers Agent Plugins 1.0.0 bundles exclusively from
//!   `~/.gemini/config/plugins/<name>/` (`plugin.json`, `hooks.json`, `mcp.json`,
//!   `skills/<skill>/SKILL.md`), and `agy plugin validate --strict` rejects
//!   absolute executable paths in `mcp.json` (`command` must be either a bare
//!   executable name or a `./` relative path inside the plugin directory).
//!
//! Safety & hygiene rules:
//! 1. Sync runs ONLY when `session_alerts.enabled && session_alerts.configure_agents`.
//!    Turning either off removes the managed copy cleanly.
//! 2. Every parent directory and the target directory itself are checked with
//!    `symlink_metadata`; symlinks and directories lacking our `.gitpulse-managed`
//!    producer marker are NEVER overwritten or deleted.
//! 3. Helper binaries (`gitpulse-mcp` and `gitpulse-hook`) are wrapped inside
//!    `bin/gitpulse-mcp` and `bin/gitpulse-hook` (`0755`) inside the plugin:
//!    - `mcp.json` invokes `"./bin/gitpulse-mcp"` (satisfying strict Agent
//!      Plugins 1.0.0 validation while still resolving an unindexed app bundle).
//!    - `bin/gitpulse-hook` fails closed (`exit 2` with `"decision":"block"`)
//!      when invoked inside a GitPulse PTY (`GITPULSE_SESSION_ID` or
//!      `GITPULSE_TERMINAL_SESSION` set) if the binary disappears, while exiting
//!      `0` outside GitPulse so moving `GitPulse.app` to Trash never bricks
//!      standalone `agy` sessions.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const PLUGIN_JSON: &str = include_str!("../../../plugins/gitpulse/plugin.json");
const HOOKS_JSON: &str = include_str!("../../../plugins/gitpulse/hooks.json");
const SKILL_COLLISIONS: &str =
    include_str!("../../../plugins/gitpulse/skills/gitpulse-collisions/SKILL.md");
const SKILL_INSIGHTS: &str =
    include_str!("../../../plugins/gitpulse/skills/gitpulse-insights/SKILL.md");
const SKILL_TASKS: &str = include_str!("../../../plugins/gitpulse/skills/gitpulse-tasks/SKILL.md");
const SKILL_TERMINAL: &str =
    include_str!("../../../plugins/gitpulse/skills/gitpulse-terminal/SKILL.md");

/// Producer marker proving GitPulse created `~/.gemini/config/plugins/gitpulse`.
pub(crate) const PRODUCER_MARKER: &str = ".gitpulse-managed";
const PRODUCER_MARKER_BODY: &str = "managed_by=gitpulse\nschema=agent-plugins-1.0.0\n";

const SKILLS: [(&str, &str); 4] = [
    ("gitpulse-collisions", SKILL_COLLISIONS),
    ("gitpulse-insights", SKILL_INSIGHTS),
    ("gitpulse-tasks", SKILL_TASKS),
    ("gitpulse-terminal", SKILL_TERMINAL),
];

fn shell_single_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', r"'\''"))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

fn resolve_helper_binary(env_var: &str, name: &str, exe: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os(env_var).filter(|v| !v.is_empty()) {
        let candidate = PathBuf::from(explicit);
        if is_executable_file(&candidate) {
            return Ok(candidate);
        }
        return Err(format!(
            "{env_var} points to {}, which is not an executable file",
            candidate.display()
        ));
    }
    let bin_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let beside = exe.and_then(Path::parent).map(|dir| dir.join(&bin_name));
    match beside {
        Some(path) if is_executable_file(&path) => Ok(path),
        Some(path) => Err(format!(
            "{bin_name} is not found beside executable at {}",
            path.display()
        )),
        None => Err("current executable path could not be determined".into()),
    }
}

fn ensure_non_symlink_dir(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "refusing to traverse symlink at {}",
                    path.display()
                ));
            }
            if !meta.is_dir() {
                return Err(format!("expected directory at {}", path.display()));
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path)
            .map_err(|e| format!("could not create {}: {e}", path.display())),
        Err(err) => Err(format!(
            "could not inspect {}: {e}",
            path.display(),
            e = err
        )),
    }
}

fn write_regular_file(path: &Path, contents: &[u8], executable: bool) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() || meta.is_dir() {
            return Err(format!(
                "refusing to overwrite non-regular entry at {}",
                path.display()
            ));
        }
    }
    std::fs::write(path, contents)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("could not chmod {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(())
}

fn rewrite_agy_hooks(raw_hooks_json: &str, wrapper_hook_path: &Path) -> Result<String, String> {
    let mut parsed: Value = serde_json::from_str(raw_hooks_json)
        .map_err(|e| format!("plugins/gitpulse/hooks.json is invalid JSON: {e}"))?;
    let quoted_hook = shell_single_quote(&wrapper_hook_path.to_string_lossy());
    let prefix = "gitpulse-hook ";

    let root = parsed
        .get_mut("gitpulse")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            "plugins/gitpulse/hooks.json missing top-level 'gitpulse' object".to_owned()
        })?;

    for (_event, entries) in root.iter_mut() {
        let Some(arr) = entries.as_array_mut() else {
            continue;
        };
        for item in arr.iter_mut() {
            if let Some(nested) = item.get_mut("hooks").and_then(Value::as_array_mut) {
                for handler in nested.iter_mut() {
                    if let Some(cmd) = handler.get("command").and_then(Value::as_str) {
                        if let Some(rest) = cmd.strip_prefix(prefix) {
                            handler["command"] = Value::String(format!("{quoted_hook} {rest}"));
                        }
                    }
                }
            } else if let Some(cmd) = item.get("command").and_then(Value::as_str) {
                if let Some(rest) = cmd.strip_prefix(prefix) {
                    item["command"] = Value::String(format!("{quoted_hook} {rest}"));
                }
            }
        }
    }

    let mut out = serde_json::to_string_pretty(&parsed)
        .map_err(|e| format!("could not serialize rewritten hooks.json: {e}"))?;
    out.push('\n');
    Ok(out)
}

fn build_mcp_wrapper(mcp_bin: &Path) -> String {
    let quoted = shell_single_quote(&mcp_bin.to_string_lossy());
    format!(
        "#!/bin/sh\nif [ -x {quoted} ]; then\n  exec {quoted} \"$@\"\nfi\nexec gitpulse-mcp \"$@\"\n"
    )
}

fn build_hook_wrapper(hook_bin: &Path) -> String {
    let quoted = shell_single_quote(&hook_bin.to_string_lossy());
    format!(
        "#!/bin/sh\nif [ -x {quoted} ]; then\n  exec {quoted} \"$@\"\nfi\nif command -v gitpulse-hook >/dev/null 2>&1; then\n  exec gitpulse-hook \"$@\"\nfi\nif [ -z \"${{GITPULSE_SESSION_ID:-}}${{GITPULSE_TERMINAL_SESSION:-}}\" ]; then\n  exit 0\nfi\nprintf '%s\\n' '{{\"decision\":\"block\",\"reason\":\"GitPulse safety hook binary is missing from this terminal session.\"}}'\nexit 2\n"
    )
}

fn verify_owned_or_absent(plugin_dir: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(plugin_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "refusing to modify symlinked Antigravity plugin at {}",
                    plugin_dir.display()
                ));
            }
            if !meta.is_dir() {
                return Err(format!(
                    "refusing to overwrite non-directory at {}",
                    plugin_dir.display()
                ));
            }
            let marker = plugin_dir.join(PRODUCER_MARKER);
            match std::fs::symlink_metadata(&marker) {
                Ok(m) if !m.file_type().is_symlink() && m.is_file() => Ok(true),
                _ => Err(format!(
                    "refusing to touch {} because it lacks {}",
                    plugin_dir.display(),
                    PRODUCER_MARKER
                )),
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("could not inspect {}: {err}", plugin_dir.display())),
    }
}

fn remove_managed_plugin(plugin_dir: &Path) -> Result<(), String> {
    let exists = verify_owned_or_absent(plugin_dir)?;
    if !exists {
        return Ok(());
    }
    for (skill_name, _) in SKILLS {
        let skill_dir = plugin_dir.join("skills").join(skill_name);
        let skill_file = skill_dir.join("SKILL.md");
        let _ = std::fs::remove_file(&skill_file);
        let _ = std::fs::remove_dir(&skill_dir);
    }
    let _ = std::fs::remove_dir(plugin_dir.join("skills"));
    for rel in [
        "bin/gitpulse-mcp",
        "bin/gitpulse-hook",
        "plugin.json",
        "hooks.json",
        "mcp.json",
        "mcp_config.json",
        PRODUCER_MARKER,
    ] {
        let _ = std::fs::remove_file(plugin_dir.join(rel));
    }
    let _ = std::fs::remove_dir(plugin_dir.join("bin"));
    let _ = std::fs::remove_dir(plugin_dir);
    Ok(())
}

/// Synchronizes `<home_root>/.gemini/config/plugins/gitpulse` deterministically.
pub(crate) fn sync_under_home(
    home_root: &Path,
    enabled: bool,
    mcp_bin: Option<&Path>,
    hook_bin: Option<&Path>,
) -> Result<(), String> {
    let gemini_dir = home_root.join(".gemini");
    let config_dir = gemini_dir.join("config");
    let plugins_dir = config_dir.join("plugins");
    let plugin_dir = plugins_dir.join("gitpulse");

    for ancestor in [&gemini_dir, &config_dir, &plugins_dir] {
        if let Ok(meta) = std::fs::symlink_metadata(ancestor) {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "refusing to traverse symlinked directory {}",
                    ancestor.display()
                ));
            }
        }
    }

    if !enabled {
        return remove_managed_plugin(&plugin_dir);
    }

    let (Some(mcp_bin), Some(hook_bin)) = (mcp_bin, hook_bin) else {
        return Err(
            "missing gitpulse-mcp or gitpulse-hook binary path for Antigravity plugin sync".into(),
        );
    };

    let _already_owned = verify_owned_or_absent(&plugin_dir)?;
    ensure_non_symlink_dir(&gemini_dir)?;
    ensure_non_symlink_dir(&config_dir)?;
    ensure_non_symlink_dir(&plugins_dir)?;
    ensure_non_symlink_dir(&plugin_dir)?;

    write_regular_file(
        &plugin_dir.join(PRODUCER_MARKER),
        PRODUCER_MARKER_BODY.as_bytes(),
        false,
    )?;

    let bin_dir = plugin_dir.join("bin");
    ensure_non_symlink_dir(&bin_dir)?;
    let wrapper_mcp = bin_dir.join("gitpulse-mcp");
    let wrapper_hook = bin_dir.join("gitpulse-hook");
    write_regular_file(&wrapper_mcp, build_mcp_wrapper(mcp_bin).as_bytes(), true)?;
    write_regular_file(&wrapper_hook, build_hook_wrapper(hook_bin).as_bytes(), true)?;

    write_regular_file(
        &plugin_dir.join("plugin.json"),
        PLUGIN_JSON.as_bytes(),
        false,
    )?;

    let mut mcp_json = serde_json::to_string_pretty(&json!({
        "mcpServers": {
            "gitpulse": {
                "type": "stdio",
                "command": "./bin/gitpulse-mcp"
            }
        }
    }))
    .map_err(|e| e.to_string())?;
    mcp_json.push('\n');
    write_regular_file(&plugin_dir.join("mcp.json"), mcp_json.as_bytes(), false)?;

    let mut mcp_config_json = serde_json::to_string_pretty(&json!({
        "mcpServers": {
            "gitpulse": {
                "command": wrapper_mcp.to_string_lossy()
            }
        }
    }))
    .map_err(|e| e.to_string())?;
    mcp_config_json.push('\n');
    write_regular_file(
        &plugin_dir.join("mcp_config.json"),
        mcp_config_json.as_bytes(),
        false,
    )?;

    let rewritten_hooks = rewrite_agy_hooks(HOOKS_JSON, &wrapper_hook)?;
    write_regular_file(
        &plugin_dir.join("hooks.json"),
        rewritten_hooks.as_bytes(),
        false,
    )?;

    let skills_root = plugin_dir.join("skills");
    ensure_non_symlink_dir(&skills_root)?;
    for (skill_name, skill_body) in SKILLS {
        let skill_dir = skills_root.join(skill_name);
        ensure_non_symlink_dir(&skill_dir)?;
        write_regular_file(&skill_dir.join("SKILL.md"), skill_body.as_bytes(), false)?;
    }

    Ok(())
}

/// Applies the user's current `session_alerts` preference (`enabled && configure_agents`)
/// to `~/.gemini/config/plugins/gitpulse` on Unix hosts.
pub(crate) fn apply_from_settings() {
    #[cfg(not(unix))]
    {
        return;
    }
    #[cfg(unix)]
    {
        let Some(home) = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
        else {
            return;
        };
        let settings = crate::tool_config::session_alerts();
        let enabled = settings.enabled && settings.configure_agents;
        if !enabled {
            if let Err(error) = sync_under_home(&home, false, None, None) {
                log::debug!(target: "workbench", "Antigravity plugin cleanup skipped: {error}");
            }
            return;
        }
        let exe = std::env::current_exe().ok();
        let mcp = match resolve_helper_binary("GITPULSE_MCP_PATH", "gitpulse-mcp", exe.as_deref()) {
            Ok(p) => p,
            Err(reason) => {
                log::debug!(target: "workbench", "Antigravity plugin sync skipped ({reason})");
                return;
            }
        };
        let hook =
            match resolve_helper_binary("GITPULSE_HOOK_PATH", "gitpulse-hook", exe.as_deref()) {
                Ok(p) => p,
                Err(reason) => {
                    log::debug!(target: "workbench", "Antigravity plugin sync skipped ({reason})");
                    return;
                }
            };
        if let Err(error) = sync_under_home(&home, true, Some(&mcp), Some(&hook)) {
            log::warn!(target: "workbench", "Antigravity plugin sync did not complete: {error}");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn make_dummy_bin(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn stages_valid_agent_plugins_bundle_and_cleans_up_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let bin_dir = tmp.path().join("bins");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let mcp = make_dummy_bin(&bin_dir, "gitpulse-mcp");
        let hook = make_dummy_bin(&bin_dir, "gitpulse-hook");

        sync_under_home(&home, true, Some(&mcp), Some(&hook)).expect("stage plugin");

        let plugin_dir = home.join(".gemini/config/plugins/gitpulse");
        assert!(plugin_dir.join(PRODUCER_MARKER).is_file());
        assert!(plugin_dir.join("plugin.json").is_file());
        assert!(plugin_dir.join("bin/gitpulse-mcp").is_file());
        assert!(plugin_dir.join("bin/gitpulse-hook").is_file());
        assert!(plugin_dir
            .join("skills/gitpulse-terminal/SKILL.md")
            .is_file());

        let mcp_json: Value =
            serde_json::from_str(&std::fs::read_to_string(plugin_dir.join("mcp.json")).unwrap())
                .unwrap();
        assert_eq!(
            mcp_json["mcpServers"]["gitpulse"]["command"],
            "./bin/gitpulse-mcp"
        );

        let hooks_json: Value =
            serde_json::from_str(&std::fs::read_to_string(plugin_dir.join("hooks.json")).unwrap())
                .unwrap();
        let pre_cmd = hooks_json["gitpulse"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            pre_cmd.contains("bin/gitpulse-hook' --host=agy collision-guard"),
            "unexpected hook command: {pre_cmd}"
        );

        // If `agy` is installed on the host, run `agy plugin validate --strict --json`.
        use crate::procguard::LockedSpawn;
        if let Ok(output) = std::process::Command::new("agy")
            .args([
                "plugin",
                "validate",
                plugin_dir.to_str().unwrap(),
                "--strict",
                "--json",
            ])
            .output_locked()
        {
            assert!(
                output.status.success(),
                "agy plugin validate --strict failed: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }

        // Turning configure_agents off removes our managed bundle cleanly.
        sync_under_home(&home, false, None, None).expect("remove managed plugin");
        assert!(!plugin_dir.exists());
    }

    #[test]
    fn refuses_to_overwrite_or_delete_unmanaged_or_symlinked_plugin_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let unmanaged = home.join(".gemini/config/plugins/gitpulse");
        std::fs::create_dir_all(&unmanaged).unwrap();
        let custom_file = unmanaged.join("custom.txt");
        std::fs::write(&custom_file, "user-owned").unwrap();

        let bin_dir = tmp.path().join("bins");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let mcp = make_dummy_bin(&bin_dir, "gitpulse-mcp");
        let hook = make_dummy_bin(&bin_dir, "gitpulse-hook");

        assert!(sync_under_home(&home, true, Some(&mcp), Some(&hook)).is_err());
        assert!(sync_under_home(&home, false, None, None).is_err());
        assert_eq!(std::fs::read_to_string(&custom_file).unwrap(), "user-owned");
    }
}
