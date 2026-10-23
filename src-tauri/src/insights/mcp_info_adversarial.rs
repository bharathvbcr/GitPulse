//! Adversarial tests for `mcp_info` (W6-L5, ft-eeb28bf6 review). `break_*`
//! tests are expected to FAIL on the reviewed tree; `held_*` pass.

use super::*;
use std::fs;

/// Held: a healthy install (real shipped package, both binaries resolvable)
/// reports no plugin error and offers both configs. No false alarm.
#[test]
fn held_healthy_install_reports_no_plugin_error() {
    let exe = std::env::current_exe().unwrap();
    let serial = crate::harness::sidecar::test_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("GITPULSE_MCP_PATH", exe.to_str().unwrap())
        .set("GITPULSE_HOOK_PATH", exe.to_str().unwrap());
    let info = mcp_info();
    assert!(info.plugin_found, "{}", info.plugin_error);
    assert_eq!(info.plugin_error, "");
    assert!(!info.mcp_config_json.is_empty());
    assert!(!info.hooks_config_json.is_empty());
}

/// Held: a healthy install whose hook binary was NOT found (dev checkout) is
/// not an error either: the template is validated against the bare name and
/// no config is offered.
#[test]
fn held_missing_hook_binary_is_not_a_plugin_error() {
    let exe = std::env::current_exe().unwrap();
    let serial = crate::harness::sidecar::test_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("GITPULSE_MCP_PATH", exe.to_str().unwrap())
        .set("GITPULSE_HOOK_PATH", "/no/such/gitpulse-hook");
    let info = mcp_info();
    assert_eq!(info.plugin_error, "");
    assert!(info.hooks_config_json.is_empty());
}

/// BREAK: a package that is a valid native root (`.codex-plugin/plugin.json`
/// and `.mcp.json`, the only two files `is_native_plugin_root` demands) but
/// ships no `hooks/hooks.json` is reported as a plugin error. The hooks file
/// is optional for the resolver and mandatory for the reporter: either the
/// resolver should require it or its absence is not an error. (Severity low:
/// the shipped package always has it.)
#[test]
fn break_optional_hooks_file_absent_is_reported_as_error() {
    let exe = std::env::current_exe().unwrap();
    let serial = crate::harness::sidecar::test_serial();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join(".codex-plugin")).unwrap();
    fs::write(root.path().join(".codex-plugin/plugin.json"), "{}").unwrap();
    fs::write(
        root.path().join(".mcp.json"),
        r#"{"mcpServers":{"gitpulse":{"command":"gitpulse-mcp"}}}"#,
    )
    .unwrap();
    let _env = crate::test_support::env::bind_env(&serial)
        .set("GITPULSE_PLUGIN_ROOT", root.path().to_str().unwrap())
        .set("GITPULSE_MCP_PATH", exe.to_str().unwrap())
        .set("GITPULSE_HOOK_PATH", exe.to_str().unwrap());
    let info = mcp_info();
    assert!(info.plugin_found);
    assert_eq!(
        info.plugin_error, "",
        "a package the resolver accepts must not be reported as broken"
    );
}
