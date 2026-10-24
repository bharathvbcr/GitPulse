//! Adversarial review of the scrubbed-environment allowlist (W8-L3). The
//! inherited environment is the PARENT's, which a repository cannot write; these
//! tests pin that nothing a child tool treats as code injection or config
//! override survives, whatever spelling the parent used.

use super::*;
use std::ffi::{OsStr, OsString};

fn surviving(inherited: &[(&str, &str)]) -> Vec<String> {
    let mut cmd = Command::new("true");
    let inherited: Vec<(OsString, OsString)> = inherited
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect();
    restrict_env_to_allowlist(&mut cmd, inherited, Some(OsStr::new("/h")));
    cmd.get_envs()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn code_injection_and_config_override_variables_never_survive() {
    let hostile = [
        "NODE_OPTIONS",
        "node_options",
        "NODE_PATH",
        "NODE_REPL_EXTERNAL_MODULE",
        "NPM_CONFIG_REGISTRY",
        "npm_config_registry",
        "NPM_CONFIG_USERCONFIG",
        "npm_config_userconfig",
        "NPM_CONFIG_//registry.npmjs.org/:_authToken",
        "npm_config_prefix",
        "npm_package_config_registry",
        "BUN_CONFIG_REGISTRY",
        "BUN_CONFIG_TOKEN",
        "BUN_OPTIONS",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "DYLD_FRAMEWORK_PATH",
        "GIT_ASKPASS",
        "GIT_SSH_COMMAND",
        "SSH_AUTH_SOCK",
        "NPM_TOKEN",
        "NODE_AUTH_TOKEN",
        "GH_TOKEN",
        "PYTHONPATH",
        "BASH_ENV",
        "ENV",
        "SHELL",
        "NODE_TLS_REJECT_UNAUTHORIZED",
    ];
    let pairs: Vec<(&str, &str)> = hostile.iter().map(|k| (*k, "x")).collect();
    let kept = surviving(&pairs);
    assert_eq!(kept, ["HOME"], "survivors: {kept:?}");
}

/// The allowlist compares upper-cased names, so a case variant of an allowed
/// name is allowed too; that must stay confined to the allowed names.
#[test]
fn lc_prefix_is_not_a_back_door() {
    let kept = surviving(&[("LC_ALL", "C"), ("LCX_NODE_OPTIONS", "--require=x")]);
    assert!(kept.contains(&"LC_ALL".to_string()));
    assert!(!kept.contains(&"LCX_NODE_OPTIONS".to_string()));
}

/// An allowed HOME from the parent must lose to the explicit scan home, so a
/// parent HOME cannot point npm at another userconfig than the one pinned.
#[test]
fn explicit_home_beats_inherited_home() {
    let mut cmd = Command::new("true");
    restrict_env_to_allowlist(
        &mut cmd,
        [(OsString::from("HOME"), OsString::from("/elsewhere"))],
        Some(OsStr::new("/h")),
    );
    let home = cmd
        .get_envs()
        .find(|(k, _)| *k == OsStr::new("HOME"))
        .and_then(|(_, v)| v);
    assert_eq!(home, Some(OsStr::new("/h")));
}

/// XDG_CONFIG_HOME / BUN_INSTALL are allowed through from the parent: they
/// relocate where bun and npm look for user config. They come from the parent,
/// not the repository, so this only documents the surface.
#[test]
fn user_config_relocators_are_parent_controlled_only() {
    let kept = surviving(&[("XDG_CONFIG_HOME", "/x"), ("BUN_INSTALL", "/b")]);
    assert!(kept.contains(&"XDG_CONFIG_HOME".to_string()));
    assert!(kept.contains(&"BUN_INSTALL".to_string()));
}
