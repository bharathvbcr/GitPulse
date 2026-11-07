//! Who can reach the tuning apply (owner decision ft-6b68be32, 2026-11-06).
//!
//! The apply writes git config and starts maintenance, so it has to be a click
//! and nothing else: not an MCP tool, a hook, the ingest pipeline, a watcher or
//! a timer. These are asserted over the source, like `git_config_posture.rs`
//! and the commit-graph writer's reach test: every production file that names
//! the executor, the command, or the argv builder is listed, and a new caller
//! fails here until someone decides it belongs.

mod common;

use std::path::{Path, PathBuf};

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "node_modules" && name != "target" {
                files(&path, ext, out);
            }
        } else if name.ends_with(ext) {
            out.push(path);
        }
    }
}

fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name == "test_support.rs"
        || rel.contains("/tests/")
        || rel.starts_with("tests/")
}

/// The production part of a Rust file: up to its first `#[cfg(test)]`.
fn production(text: &str) -> &str {
    text.split("\n#[cfg(test)]").next().unwrap_or_default()
}

fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn only_the_storage_panel_command_reaches_the_tuning_apply() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut all = Vec::new();
    files(&root, ".rs", &mut all);
    assert!(all.len() > 100, "found only {} source files", all.len());
    let needles = [
        "tuning_apply(",
        "tuning_apply_within(",
        "cmd_tuning_apply",
        "tuning_runnable(",
        "change_argv(",
        "is_apply_argv(",
        "plan_apply(",
    ];
    let mut who: Vec<(String, String)> = Vec::new();
    for path in all {
        let rel = rel(&path, &root);
        if is_test_file(&rel) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let production = production(&text);
        for needle in needles {
            if production.contains(needle) {
                who.push((rel.clone(), needle.to_string()));
            }
        }
    }
    who.sort();
    who.dedup();
    let allowed = [
        // the executor and its own entry points
        ("engine/tuning_apply.rs", "cmd_tuning_apply"),
        ("engine/tuning_apply.rs", "tuning_apply("),
        ("engine/tuning_apply.rs", "tuning_apply_within("),
        ("engine/tuning_apply.rs", "tuning_runnable("),
        ("engine/tuning_apply.rs", "is_apply_argv("),
        ("engine/tuning_apply.rs", "plan_apply("),
        // the plan, the argv builder and its allow-list
        ("storage/repo_tuning/advice.rs", "change_argv("),
        ("storage/repo_tuning/advice.rs", "is_apply_argv("),
        ("storage/repo_tuning/advice.rs", "plan_apply("),
        // the one caller of the executor: the Storage panel's confirm click
        ("commands/analysis.rs", "cmd_tuning_apply"),
        ("commands/analysis.rs", "tuning_apply("),
        ("commands/analysis.rs", "tuning_runnable("),
        // registration of the command with the webview
        ("lib.rs", "cmd_tuning_apply"),
    ];
    let unexpected: Vec<_> = who
        .iter()
        .filter(|entry| !allowed.contains(&(entry.0.as_str(), entry.1.as_str())))
        .collect();
    assert!(
        unexpected.is_empty(),
        "something other than cmd_tuning_apply reaches the tuning apply: {unexpected:#?}"
    );
    // The executor has exactly one production caller outside its own file.
    assert!(who.contains(&(
        "commands/analysis.rs".to_string(),
        "tuning_apply(".to_string()
    )));
    let callers: Vec<&String> = who
        .iter()
        .filter(|(_, needle)| needle == "tuning_apply(")
        .map(|(file, _)| file)
        .collect();
    assert_eq!(callers.len(), 2, "{callers:?}");
}

/// The tuning command is not a tool an agent can call, a hook, or a timer:
/// the MCP server, the hook binary, the hook handlers, ingest and the watcher
/// name none of its entry points, by any spelling.
#[test]
fn mcp_hooks_ingest_and_timers_never_name_the_apply() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut all = Vec::new();
    files(&root, ".rs", &mut all);
    let forbidden_dirs = [
        "mcp/",
        "mcp.rs",
        "hooks/",
        "bin/",
        "ingest/",
        "watcher/",
        "insights/",
        "harness/",
    ];
    let words = [
        "tuning_apply",
        "cmd_tuning",
        "TuningChange",
        "repo_tuning::",
        "tuning_plan",
        "tuning_runnable",
    ];
    let mut hits = Vec::new();
    for path in all {
        let rel = rel(&path, &root);
        if !forbidden_dirs
            .iter()
            .any(|d| rel.starts_with(d) || rel == *d)
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for word in words {
            if text.contains(word) {
                hits.push(format!("{rel}: {word}"));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "a non-click path names the tuning apply: {hits:#?}"
    );
}

/// In the webview, only the tuning flow's own module invokes the apply, and it
/// does so from its confirm step.
#[test]
fn only_the_tuning_flow_invokes_the_apply_in_the_webview() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("src");
    let mut all = Vec::new();
    files(&root, ".ts", &mut all);
    files(&root, ".svelte", &mut all);
    assert!(all.len() > 100, "found only {} web files", all.len());
    let mut who = Vec::new();
    for path in all {
        let rel = rel(&path, &root);
        if rel.ends_with(".test.ts") || rel.contains("/__tests__/") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("cmd_tuning_apply\"") || text.contains("cmd_tuning_apply'") {
            who.push(rel);
        }
    }
    who.sort();
    assert_eq!(who, ["lib/health/tuningApply.ts"], "{who:?}");
}
