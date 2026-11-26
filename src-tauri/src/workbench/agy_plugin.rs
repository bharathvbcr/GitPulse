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
//! 4. No `PreToolUse` hook is registered. Antigravity IDE 2.32.1 turned every
//!    answer from one that was not `"decision":"allow"` — `{}` included — into
//!    "tool call denied by pre-tool hook", so the collision guard and command
//!    gate blocked every edit and command. `"allow"` would override the user's
//!    own approval settings, so Antigravity runs ungated; GitPulse still records
//!    its edits through `PostToolUse`.

#[cfg(unix)]
use serde_json::{json, Value};
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
const PLUGIN_JSON: &str = include_str!("../../../plugins/gitpulse/plugin.json");
#[cfg(unix)]
const HOOKS_JSON: &str = include_str!("../../../plugins/gitpulse/hooks.json");
#[cfg(unix)]
const SKILL_COLLISIONS: &str = include_str!("../../../plugins/gitpulse/skills/collisions/SKILL.md");
#[cfg(unix)]
const SKILL_COMMIT_AND_MERGE: &str =
    include_str!("../../../plugins/gitpulse/skills/commit-and-merge/SKILL.md");
#[cfg(unix)]
const SKILL_INSIGHTS: &str = include_str!("../../../plugins/gitpulse/skills/insights/SKILL.md");
#[cfg(unix)]
const SKILL_LOAD: &str = include_str!("../../../plugins/gitpulse/skills/load/SKILL.md");
#[cfg(unix)]
const SKILL_MERGE_PRS: &str = include_str!("../../../plugins/gitpulse/skills/merge-prs/SKILL.md");
#[cfg(unix)]
const SKILL_MERGE_TRAIN: &str =
    include_str!("../../../plugins/gitpulse/skills/merge-train/SKILL.md");
#[cfg(unix)]
const MERGE_TRAIN_FAILSET_GO_MOD: &str =
    include_str!("../../../plugins/gitpulse/skills/merge-train/failset/go.mod");
#[cfg(unix)]
const MERGE_TRAIN_FAILSET_MAIN: &str =
    include_str!("../../../plugins/gitpulse/skills/merge-train/failset/main.go");
#[cfg(unix)]
const MERGE_TRAIN_FAILSET_TEST: &str =
    include_str!("../../../plugins/gitpulse/skills/merge-train/failset/main_test.go");
#[cfg(unix)]
const SKILL_TASKS: &str = include_str!("../../../plugins/gitpulse/skills/tasks/SKILL.md");
#[cfg(unix)]
const SKILL_TERMINAL: &str = include_str!("../../../plugins/gitpulse/skills/terminal/SKILL.md");
#[cfg(unix)]
const SKILL_WRAP_UP: &str = include_str!("../../../plugins/gitpulse/skills/wrap-up/SKILL.md");

/// Producer marker proving GitPulse created `~/.gemini/config/plugins/gitpulse`.
#[cfg(unix)]
pub(crate) const PRODUCER_MARKER: &str = ".gitpulse-managed";
#[cfg(unix)]
const PRODUCER_MARKER_BODY: &str = "managed_by=gitpulse\nschema=agent-plugins-1.0.0\n";

/// Antigravity runs a plugin's skill as a bare `/<skill-name>`, with no plugin
/// namespace, so its copies keep this prefix (`/gitpulse-insights`). Hosts
/// that namespace by plugin read `plugins/gitpulse/skills/<name>` as it is and
/// show `gitpulse:insights`.
pub(crate) const ANTIGRAVITY_SKILL_PREFIX: &str = "gitpulse-";

/// Highest Antigravity version known to deny every `PreToolUse` response that
/// is not `"decision":"allow"` (`{}` included). The `PreToolUse` canary stays
/// unconditionally disabled at or below this version (Opus A2).
#[allow(dead_code)]
pub const PRE_TOOL_USE_BROKEN_MAX_VERSION: (u32, u32, u32) = (2, 32, 1);

/// Documented unlock gate required before `PreToolUse` can ever be enabled for
/// Antigravity sessions.
#[allow(dead_code)]
pub const PRE_TOOL_USE_UNLOCK_GATE: &str =
    "Disabled on Antigravity <= 2.32.1 (every non-allow PreToolUse response, including {}, denies the tool call). Unlocks ONLY when installed version > 2.32.1 AND a live passthrough canary confirms {} does not deny.";

/// Honest runtime labels surfaced to Desktop and Mobile for Antigravity
/// sessions (Opus A2).
#[allow(dead_code)]
pub const AGY_GATING_LABEL_UNGATED: &str = "ungated";
#[allow(dead_code)]
pub const AGY_IDLE_WAKE_LABEL_QUEUED: &str = "Queued (next turn)";

/// Result of evaluating the version-gated `PreToolUse` canary for Antigravity.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgyPreToolUseCanary {
    pub detected_version: Option<String>,
    pub passthrough_probe_passed: bool,
    pub enabled: bool,
    pub unlock_gate: &'static str,
}

/// Explicit runtime capability metadata for an Antigravity session.
///
/// Manvi does NOT gate Antigravity's own tool calls, and idle Antigravity
/// sessions have no `asyncRewake` background waiter — both facts are surfaced
/// explicitly rather than hidden behind a static capability matrix.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgyRuntimeCapabilities {
    pub harness: &'static str,
    pub approval_channel: &'static str,
    pub gating_label: &'static str,
    pub idle_wake_label: &'static str,
    pub supports_remote_approval: bool,
    pub supports_active_steering: bool,
    pub supports_idle_wake: bool,
    pub ungated: bool,
    pub queued_steering_ttl_sec: u64,
    pub max_stop_continuations: u32,
    pub pre_tool_use_canary: AgyPreToolUseCanary,
}

/// Parses the first `MAJOR.MINOR.PATCH` semver triple from an `agy --version`
/// string (e.g. `"2.32.1"`, `"agy 2.32.1"`, `"Antigravity 2.33.0-beta.1"`).
#[allow(dead_code)]
pub fn parse_agy_semver(raw: &str) -> Option<(u32, u32, u32)> {
    for token in raw.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let trimmed = token.trim().trim_start_matches('v');
        let core = trimmed.split(['-', '+']).next().unwrap_or("");
        let mut parts = core.split('.');
        let (Some(maj), Some(min), Some(pat)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if parts.next().is_some() {
            continue;
        }
        if let (Ok(maj), Ok(min), Ok(pat)) = (
            maj.parse::<u32>(),
            min.parse::<u32>(),
            pat.parse::<u32>(),
        ) {
            return Some((maj, min, pat));
        }
    }
    None
}

/// Evaluates the version-gated `PreToolUse` canary.
///
/// Stays unconditionally disabled for Antigravity `<= 2.32.1` or unknown
/// versions, even if `passthrough_probe_passed` is `true`.
#[allow(dead_code)]
pub fn evaluate_pre_tool_use_canary(
    detected_version: Option<&str>,
    passthrough_probe_passed: bool,
) -> AgyPreToolUseCanary {
    let parsed = detected_version.and_then(parse_agy_semver);
    let version_eligible = parsed.is_some_and(|v| v > PRE_TOOL_USE_BROKEN_MAX_VERSION);
    AgyPreToolUseCanary {
        detected_version: detected_version.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
        passthrough_probe_passed,
        enabled: version_eligible && passthrough_probe_passed,
        unlock_gate: PRE_TOOL_USE_UNLOCK_GATE,
    }
}

/// Builds the explicit runtime capability metadata for an Antigravity session.
#[allow(dead_code)]
pub fn runtime_capabilities(
    detected_version: Option<&str>,
    passthrough_probe_passed: bool,
) -> AgyRuntimeCapabilities {
    let canary = evaluate_pre_tool_use_canary(detected_version, passthrough_probe_passed);
    let gated = canary.enabled;
    AgyRuntimeCapabilities {
        harness: "antigravity",
        approval_channel: if gated {
            "presence_gated_hook"
        } else {
            "native_only"
        },
        gating_label: if gated {
            "canary_gated"
        } else {
            AGY_GATING_LABEL_UNGATED
        },
        idle_wake_label: AGY_IDLE_WAKE_LABEL_QUEUED,
        supports_remote_approval: gated,
        supports_active_steering: true,
        supports_idle_wake: false,
        ungated: !gated,
        queued_steering_ttl_sec: crate::agent_mailbox::PENDING_TTL.as_secs(),
        max_stop_continuations: crate::agent_mailbox::MAX_CONTINUATIONS,
        pre_tool_use_canary: canary,
    }
}

/// Installed directory name, then the bundled `SKILL.md` it is written from.
#[cfg(unix)]
const SKILLS: [(&str, &str); 9] = [
    ("gitpulse-collisions", SKILL_COLLISIONS),
    ("gitpulse-commit-and-merge", SKILL_COMMIT_AND_MERGE),
    ("gitpulse-insights", SKILL_INSIGHTS),
    ("gitpulse-load", SKILL_LOAD),
    ("gitpulse-merge-prs", SKILL_MERGE_PRS),
    ("gitpulse-merge-train", SKILL_MERGE_TRAIN),
    ("gitpulse-tasks", SKILL_TASKS),
    ("gitpulse-terminal", SKILL_TERMINAL),
    ("gitpulse-wrap-up", SKILL_WRAP_UP),
];

#[cfg(unix)]
const SKILL_SUBFILES: [(&str, &str, &str); 3] = [
    (
        "gitpulse-merge-train",
        "failset/go.mod",
        MERGE_TRAIN_FAILSET_GO_MOD,
    ),
    (
        "gitpulse-merge-train",
        "failset/main.go",
        MERGE_TRAIN_FAILSET_MAIN,
    ),
    (
        "gitpulse-merge-train",
        "failset/main_test.go",
        MERGE_TRAIN_FAILSET_TEST,
    ),
];

#[cfg(unix)]
fn shell_single_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', r"'\''"))
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
fn build_mcp_wrapper(mcp_bin: &Path) -> String {
    let quoted = shell_single_quote(&mcp_bin.to_string_lossy());
    format!(
        "#!/bin/sh\nif [ -x {quoted} ]; then\n  exec {quoted} \"$@\"\nfi\nexec gitpulse-mcp \"$@\"\n"
    )
}

#[cfg(unix)]
fn build_hook_wrapper(hook_bin: &Path) -> String {
    let quoted = shell_single_quote(&hook_bin.to_string_lossy());
    format!(
        "#!/bin/sh\nif [ -x {quoted} ]; then\n  exec {quoted} \"$@\"\nfi\nif command -v gitpulse-hook >/dev/null 2>&1; then\n  exec gitpulse-hook \"$@\"\nfi\nif [ -z \"${{GITPULSE_SESSION_ID:-}}${{GITPULSE_TERMINAL_SESSION:-}}\" ]; then\n  exit 0\nfi\nprintf '%s\\n' '{{\"decision\":\"block\",\"reason\":\"GitPulse safety hook binary is missing from this terminal session.\"}}'\nexit 2\n"
    )
}

#[cfg(unix)]
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

/// What a skill is called where `provider` runs it: Antigravity has no plugin
/// namespace, so it gets [`ANTIGRAVITY_SKILL_PREFIX`]; the other hosts
/// qualify a plugin's skills by the plugin's name.
pub(crate) fn skill_reference(provider: &str, skill: &str) -> String {
    if provider == "agy" {
        format!("{ANTIGRAVITY_SKILL_PREFIX}{skill}")
    } else {
        format!("gitpulse:{skill}")
    }
}

/// The bundled `SKILL.md` as Antigravity's copy at `installed`: the frontmatter
/// `name:` becomes the installed directory name, and every `gitpulse:<skill>`
/// reference to a bundled skill becomes its Antigravity name, so the copy
/// names skills the way Antigravity runs them.
#[cfg(unix)]
fn for_antigravity(installed: &str, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 64);
    let mut renamed = false;
    for line in body.split_inclusive('\n') {
        if !renamed && line.starts_with("name:") {
            out.push_str("name: ");
            out.push_str(installed);
            out.push('\n');
            renamed = true;
        } else {
            out.push_str(line);
        }
    }
    for (other, _) in SKILLS {
        if let Some(short) = other.strip_prefix(ANTIGRAVITY_SKILL_PREFIX) {
            out = out.replace(&format!("gitpulse:{short}"), other);
        }
    }
    out
}

#[cfg(unix)]
fn remove_managed_plugin(plugin_dir: &Path) -> Result<(), String> {
    let exists = verify_owned_or_absent(plugin_dir)?;
    if !exists {
        return Ok(());
    }
    for (skill_name, rel_path, _) in SKILL_SUBFILES {
        let subfile = plugin_dir.join("skills").join(skill_name).join(rel_path);
        let _ = std::fs::remove_file(&subfile);
        if let Some(parent) = subfile.parent() {
            let _ = std::fs::remove_dir(parent);
        }
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
#[cfg(unix)]
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
        write_regular_file(
            &skill_dir.join("SKILL.md"),
            for_antigravity(skill_name, skill_body).as_bytes(),
            false,
        )?;
    }
    for (skill_name, rel_path, contents) in SKILL_SUBFILES {
        let skill_dir = skills_root.join(skill_name);
        let subfile = skill_dir.join(rel_path);
        if let Some(parent) = subfile.parent() {
            if parent != skill_dir {
                ensure_non_symlink_dir(parent)?;
            }
        }
        write_regular_file(&subfile, contents.as_bytes(), false)?;
    }

    Ok(())
}

/// Applies the user's current `session_alerts` preference (`enabled && configure_agents`)
/// to `~/.gemini/config/plugins/gitpulse` on Unix hosts.
#[cfg(unix)]
pub(crate) fn apply_from_settings() {
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
    let hook = match resolve_helper_binary("GITPULSE_HOOK_PATH", "gitpulse-hook", exe.as_deref()) {
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

/// Antigravity plugin sync is Unix-only; elsewhere there is nothing to apply.
#[cfg(not(unix))]
pub(crate) fn apply_from_settings() {}

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
        assert!(plugin_dir.join("skills/gitpulse-load/SKILL.md").is_file());
        assert!(plugin_dir
            .join("skills/gitpulse-merge-train/SKILL.md")
            .is_file());
        assert!(plugin_dir
            .join("skills/gitpulse-merge-train/failset/go.mod")
            .is_file());
        for landing in [
            "gitpulse-commit-and-merge",
            "gitpulse-merge-prs",
            "gitpulse-wrap-up",
        ] {
            assert!(
                plugin_dir
                    .join("skills")
                    .join(landing)
                    .join("SKILL.md")
                    .is_file(),
                "{landing}"
            );
        }
        // Antigravity has no plugin namespace: each copy is named for its
        // directory and names its sibling skills the same way.
        for (installed, _) in SKILLS {
            let body =
                std::fs::read_to_string(plugin_dir.join("skills").join(installed).join("SKILL.md"))
                    .unwrap();
            assert!(
                installed.starts_with(ANTIGRAVITY_SKILL_PREFIX),
                "{installed}"
            );
            assert!(
                body.contains(&format!("\nname: {installed}\n")),
                "{installed} frontmatter must carry its installed name"
            );
            for (other, _) in SKILLS {
                let short = other.strip_prefix(ANTIGRAVITY_SKILL_PREFIX).unwrap();
                assert!(
                    !body.contains(&format!("gitpulse:{short}")),
                    "{installed} names gitpulse:{short}, which Antigravity cannot run"
                );
            }
        }

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
        // No pre-tool gate: Antigravity IDE 2.32.1 denied every call such a
        // hook answered without `"decision":"allow"`, `{}` included.
        assert!(
            hooks_json["gitpulse"].get("PreToolUse").is_none(),
            "Antigravity must get no PreToolUse hook: {hooks_json}"
        );
        let brief_cmd = hooks_json["gitpulse"]["SessionStart"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            brief_cmd.contains("bin/gitpulse-hook' --host=agy session-brief"),
            "unexpected hook command: {brief_cmd}"
        );
        let session_inbox_cmd = hooks_json["gitpulse"]["SessionStart"][1]["command"]
            .as_str()
            .unwrap();
        assert!(
            session_inbox_cmd.contains("bin/gitpulse-hook' --host=agy peer-inbox"),
            "missing SessionStart peer-inbox: {session_inbox_cmd}"
        );
        assert_eq!(hooks_json["gitpulse"]["PostToolUse"][0]["matcher"], "*");
        assert!(
            hooks_json["gitpulse"]["PostToolUse"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("bin/gitpulse-hook' --host=agy notify tool_finished")
        );
        assert!(hooks_json["gitpulse"]["PreInvocation"][0]["command"]
            .as_str()
            .unwrap()
            .contains("bin/gitpulse-hook' --host=agy peer-inbox"));
        assert!(hooks_json["gitpulse"]["PostInvocation"][0]["command"]
            .as_str()
            .unwrap()
            .contains("bin/gitpulse-hook' --host=agy peer-inbox"));
        assert!(hooks_json["gitpulse"]["Stop"][0]["command"]
            .as_str()
            .unwrap()
            .contains("bin/gitpulse-hook' --host=agy stop-or-notify"));

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

    #[test]
    fn antigravity_plugin_hooks_match_gitpulse_agy_lifecycle_and_subcommands() {
        let embedded: Value = serde_json::from_str(HOOKS_JSON).unwrap();
        let agy_ref: Value =
            serde_json::from_str(include_str!("../../../plugins/gitpulse-agy/hooks.json")).unwrap();

        let extract = |root: &Value| -> Vec<(String, String, String)> {
            let mut out = Vec::new();
            for (event, entries) in root["gitpulse"].as_object().unwrap() {
                for entry in entries.as_array().unwrap() {
                    if let Some(nested) = entry.get("hooks").and_then(Value::as_array) {
                        let matcher = entry["matcher"].as_str().unwrap_or_default().to_owned();
                        for h in nested {
                            let cmd = h["command"].as_str().unwrap();
                            let norm = cmd.replace(" --host=agy ", " ");
                            out.push((event.clone(), matcher.clone(), norm));
                        }
                    } else {
                        let cmd = entry["command"].as_str().unwrap();
                        let norm = cmd.replace(" --host=agy ", " ");
                        out.push((event.clone(), String::new(), norm));
                    }
                }
            }
            out.sort();
            out
        };

        assert_eq!(extract(&embedded), extract(&agy_ref));
    }

    #[test]
    fn pre_tool_use_canary_stays_disabled_on_2_32_1_and_surfaces_ungated_and_queued_metadata() {
        assert_eq!(parse_agy_semver("2.32.1"), Some((2, 32, 1)));
        assert_eq!(parse_agy_semver("agy v2.33.0-beta.1"), Some((2, 33, 0)));
        assert_eq!(parse_agy_semver("unknown"), None);

        // <= 2.32.1 MUST remain disabled even if passthrough_probe_passed is true.
        for ver in [None, Some("2.31.9"), Some("2.32.1"), Some("agy 2.32.1")] {
            let caps = runtime_capabilities(ver, true);
            assert!(!caps.pre_tool_use_canary.enabled, "{ver:?}");
            assert!(caps.ungated, "{ver:?}");
            assert_eq!(caps.gating_label, "ungated");
            assert_eq!(caps.idle_wake_label, "Queued (next turn)");
            assert_eq!(caps.approval_channel, "native_only");
            assert!(!caps.supports_remote_approval);
            assert!(!caps.supports_idle_wake);
            assert_eq!(caps.queued_steering_ttl_sec, 7200);
            assert_eq!(caps.max_stop_continuations, 3);
            assert!(caps.pre_tool_use_canary.unlock_gate.contains("2.32.1"));
        }

        // > 2.32.1 still stays disabled until passthrough probe passes.
        let unprobed = runtime_capabilities(Some("2.33.0"), false);
        assert!(!unprobed.pre_tool_use_canary.enabled);
        assert!(unprobed.ungated);
        assert_eq!(unprobed.gating_label, "ungated");

        // > 2.32.1 + verified passthrough probe unlocks the canary cleanly.
        let unlocked = runtime_capabilities(Some("2.33.0"), true);
        assert!(unlocked.pre_tool_use_canary.enabled);
        assert!(!unlocked.ungated);
        assert_eq!(unlocked.approval_channel, "presence_gated_hook");
        assert_eq!(unlocked.idle_wake_label, "Queued (next turn)");
    }
}
