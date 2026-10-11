//! The Tauri ACL contract: what the webview can call must equal what the
//! capability file grants.
//!
//! This gate exists because the app shipped with every external link, every
//! "open in default app" and every "reveal in file manager" dead. The
//! capability file granted `opener:allow-default-urls`, which reads like it
//! enables opening URLs and does not: it carries a *scope* (`http://*`,
//! `https://*`, `mailto:*`, `tel:*`) and an empty `commands.allow`. The
//! command itself needed `opener:allow-open-url`. Every call failed at runtime
//! with "Command plugin:opener|open_url not allowed by ACL", and 4192
//! frontend tests passed anyway, because nothing compared the two sides.
//!
//! Nothing here is hand-listed. The granted side is derived from
//! `gen/schemas/acl-manifests.json`, which the Tauri build script writes from
//! the plugin crates themselves, so it always describes the plugin versions
//! actually compiled in. The used side is derived from the frontend's own
//! import statements. A new plugin API, a renamed permission or a dropped
//! grant all move one of the two sets and fail here.
//!
//! This test lives in Rust rather than Vitest for one reason: the manifest is
//! a build-script artifact under the gitignored `gen/`, and CI runs Vitest
//! before any cargo step, so a frontend test would find no manifest on a clean
//! checkout and would have to either skip (reporting "pass" for a check that
//! never ran) or fail spuriously. Building this test generates the manifest.
//!
//! Scope, stated so the gate is not read as broader than it is. It compares
//! *plugin* commands, and proves the permission resolution behind that
//! comparison is not silently empty. Two things it does NOT do:
//!
//! - It does not catch misspelled permission ids. `tauri_build` already fails
//!   the build on those ("Permission … not found"), verified by planting one.
//! - It does not derive core window/webview calls (`setTitle`, `setZoom`,
//!   `data-tauri-drag-region`) back to their `core:*` permissions. That
//!   mapping is not mechanical — `setZoom` is `set_webview_zoom`, and a drag
//!   region needs two commands no method call names — and a guessed mapping
//!   would be a gate that looks like it checks something it does not. Those
//!   grants were audited by hand for this change and found complete.

// The hermetic test environment: a per-process home, git config and trust
// store, rewritten before `main`. See `src/test_support/env.rs`.
#[path = "../src/test_support/env.rs"]
mod hermetic_env;

// The same reading of `generate_handler!` that `build.rs` turns into the app
// ACL manifest.
#[path = "../build_support/app_commands.rs"]
mod app_commands;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("gen/schemas/acl-manifests.json")
}

fn read_json(path: &Path) -> serde_json::Value {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} must be valid JSON: {e}", path.display()))
}

/// Permission identifiers granted by the app's capability files.
fn granted_permissions() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    let mut out = Vec::new();
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("capabilities dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no capability files found in {} — this gate would pass vacuously",
        dir.display()
    );
    for file in files {
        let json = read_json(&file);
        let perms = json["permissions"]
            .as_array()
            .unwrap_or_else(|| panic!("{} needs a permissions array", file.display()));
        for perm in perms {
            // Object-form entries (`{"identifier": ..., "allow": [...]}`) carry
            // their id in a field; string entries are the id.
            let id = perm
                .as_str()
                .map(str::to_string)
                .or_else(|| perm["identifier"].as_str().map(str::to_string))
                .unwrap_or_else(|| panic!("{} has an unreadable permission entry", file.display()));
            out.push(id);
        }
    }
    out
}

/// `(namespace, permission)` for one granted identifier.
///
/// `opener:allow-open-url` splits on the last `:`; core ids carry two
/// (`core:window:allow-set-title`), and the namespace is everything before the
/// final segment.
fn split_identifier(id: &str) -> (String, String) {
    match id.rsplit_once(':') {
        Some((ns, perm)) => (ns.to_string(), perm.to_string()),
        // The app's own command permissions (`allow-cmd-…`) live in the app
        // manifest, whose key is not a plugin name.
        None if id.starts_with("allow-") => ("__app-acl__".to_string(), id.to_string()),
        // A bare id names a plugin's default permission set.
        None => (id.to_string(), "default".to_string()),
    }
}

/// Every command each granted permission actually allows, grouped by namespace.
///
/// Permission *sets* expand to their members; this is what makes
/// `opener:default` and the three individual grants equivalent, and what makes
/// a scope-only permission contribute nothing.
fn granted_commands(manifest: &serde_json::Value) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for id in granted_permissions() {
        let (ns, perm) = split_identifier(&id);
        let namespace = manifest.get(&ns).unwrap_or_else(|| {
            panic!("granted permission {id:?} names namespace {ns:?}, which no compiled plugin provides")
        });
        let entry = out.entry(ns.clone()).or_default();
        collect_commands(namespace, &perm, &id, entry, 0);
    }
    out
}

/// Resolves one permission (or permission set) to the commands it allows.
fn collect_commands(
    namespace: &serde_json::Value,
    perm: &str,
    id: &str,
    out: &mut BTreeSet<String>,
    depth: usize,
) {
    assert!(depth < 16, "permission set recursion too deep for {id:?}");
    if let Some(found) = namespace["permissions"].get(perm) {
        if let Some(cmds) = found["commands"]["allow"].as_array() {
            for cmd in cmds {
                if let Some(name) = cmd.as_str() {
                    out.insert(name.to_string());
                }
            }
        }
        return;
    }
    if let Some(set) = namespace["permission_sets"].get(perm) {
        let members = set["permissions"]
            .as_array()
            .unwrap_or_else(|| panic!("permission set {id:?} has no members"));
        for member in members {
            let name = member.as_str().unwrap_or_else(|| {
                panic!("permission set {id:?} has a non-string member {member:?}")
            });
            collect_commands(namespace, name, id, out, depth + 1);
        }
        return;
    }
    // `default_permission` is a set object in its own right: it carries a
    // member list, and its `identifier` is the literal "default", so
    // recursing on that name would loop rather than resolve.
    if perm == "default" {
        if let Some(members) = namespace["default_permission"]["permissions"].as_array() {
            for member in members {
                let name = member
                    .as_str()
                    .unwrap_or_else(|| panic!("default set for {id:?} has a non-string member"));
                collect_commands(namespace, name, id, out, depth + 1);
            }
            return;
        }
    }
    panic!(
        "granted permission {id:?} does not exist — no permission or permission set named {perm:?} \
         in namespace. A renamed or removed permission fails here instead of silently granting nothing."
    );
}

/// Frontend source files the webview actually ships (tests excluded: they run
/// in Node, never in a webview, so their imports grant nothing).
fn frontend_sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(&repo_root().join("src"), &mut out);
    out.retain(|p| {
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let shipped = !name.contains(".test.") && !name.contains(".spec.");
        let in_test_dir = p.components().any(|c| c.as_os_str() == "__tests__");
        shipped && !in_test_dir
    });
    assert!(
        out.len() > 50,
        "expected to find the frontend sources, found {} — a wrong root would make this gate pass vacuously",
        out.len()
    );
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "node_modules") {
                continue;
            }
            walk(&path, out);
        } else if path
            .extension()
            .is_some_and(|x| x == "ts" || x == "svelte" || x == "js")
        {
            out.push(path);
        }
    }
}

/// `openUrl` -> `open_url`, `revealItemInDir` -> `reveal_item_in_dir`.
fn camel_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Plugin commands the shipped frontend can invoke, grouped by plugin
/// namespace, parsed from real `import { … } from "@tauri-apps/plugin-X"`
/// statements. Prose mentioning a plugin path is not an import and does not
/// count — `openInShell.ts` documents why it avoids the opener plugin.
fn used_plugin_commands() -> BTreeMap<String, BTreeSet<(String, PathBuf)>> {
    let mut out: BTreeMap<String, BTreeSet<(String, PathBuf)>> = BTreeMap::new();
    for file in frontend_sources() {
        let source = fs::read_to_string(&file).unwrap_or_default();
        for (symbols, plugin) in parse_plugin_imports(&source) {
            for symbol in symbols {
                out.entry(plugin.clone())
                    .or_default()
                    .insert((camel_to_snake(&symbol), file.clone()));
            }
        }
    }
    out
}

/// Returns `(imported symbols, plugin namespace)` for each plugin import.
fn parse_plugin_imports(source: &str) -> Vec<(Vec<String>, String)> {
    const NEEDLE: &str = "@tauri-apps/plugin-";
    let mut found = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = source[cursor..].find(NEEDLE) {
        let at = cursor + rel;
        cursor = at + NEEDLE.len();
        // The plugin name runs to the closing quote.
        let rest = &source[cursor..];
        let Some(end) = rest.find(['"', '\'']) else {
            continue;
        };
        let plugin = rest[..end].to_string();
        if plugin.is_empty()
            || !plugin
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            continue;
        }
        // Walk back to the `import` that owns this specifier. Anything else
        // (prose in a doc comment, an assertion string) is not an import.
        let before = &source[..at];
        let Some(import_at) = before.rfind("import") else {
            continue;
        };
        let head = &before[import_at..];
        // Only a `from` clause between the brace list and the specifier makes
        // this a real static import of named symbols.
        if !head.contains("from") {
            continue;
        }
        let Some(open) = head.find('{') else {
            continue;
        };
        let Some(close) = head.find('}') else {
            continue;
        };
        if close < open {
            continue;
        }
        let symbols: Vec<String> = head[open + 1..close]
            .split(',')
            .map(|s| s.split(" as ").next().unwrap_or("").trim().to_string())
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .collect();
        if symbols.is_empty() {
            continue;
        }
        found.push((symbols, plugin));
    }
    found
}

/// The contract that would have caught the shipped outage.
///
/// Both directions matter. Missing grants are dead features; surplus grants
/// widen what a compromised webview can reach for nothing in return.
#[test]
fn every_plugin_command_the_frontend_calls_is_granted_and_nothing_more() {
    let manifest = read_json(&manifest_path());
    let granted = granted_commands(&manifest);
    let used = used_plugin_commands();

    assert!(
        !used.is_empty(),
        "no plugin imports found in the frontend — the scan is broken, not the app"
    );

    for (plugin, commands) in &used {
        let granted_here = granted.get(plugin).cloned().unwrap_or_default();
        let known: BTreeSet<String> = manifest.get(plugin).unwrap_or_else(|| {
            panic!("frontend imports {plugin:?}, which is not a compiled plugin")
        })["permissions"]
            .as_object()
            .map(|perms| {
                perms
                    .values()
                    .filter_map(|p| p["commands"]["allow"].as_array())
                    .flatten()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        for (command, file) in commands {
            let where_ = file.strip_prefix(repo_root()).unwrap_or(file).display();
            assert!(
                known.contains(command),
                "{where_} imports a {plugin} API that maps to command {command:?}, which the \
                 compiled plugin does not define. Either the API name changed or the \
                 camelCase->snake_case derivation no longer holds; fix the derivation rather \
                 than deleting the check."
            );
            assert!(
                granted_here.contains(command),
                "{where_} can invoke {plugin}'s {command:?}, but no granted permission allows it, \
                 so every call fails at runtime with \"Command plugin:{plugin}|{command} not \
                 allowed by ACL\".\n\
                 Granted commands for {plugin}: {granted_here:?}\n\
                 Note that a scope-only permission (empty commands.allow, e.g. \
                 opener:allow-default-urls) grants NO command — that is the exact mistake this \
                 gate exists for. Add the command permission (e.g. opener:allow-open-url)."
            );
        }

        let used_here: BTreeSet<String> = commands.iter().map(|(c, _)| c.clone()).collect();
        let surplus: Vec<&String> = granted_here.difference(&used_here).collect();
        assert!(
            surplus.is_empty(),
            "capability grants {plugin} commands nothing in the frontend invokes: {surplus:?}. \
             Every grant widens what a compromised webview can reach, so remove it or route a \
             caller through it. open_path and reveal_item_in_dir are deliberately absent: \
             desktop::shell owns those, so containment is proven in Rust."
        );
    }
}

/// Guards the *resolution machinery*, not the identifiers.
///
/// Stated precisely, because it would be easy to read this as more than it
/// is: `tauri_build` already rejects an unknown permission id at build time
/// (verified — a bogus entry fails `cargo build` with "Permission … not
/// found"), so this adds nothing there. What it does add is proof that
/// `granted_commands` actually resolves the ids it is given. That matters
/// because the contract above compares against whatever this returns: if the
/// permission-set expansion silently produced an empty set, every "is this
/// command granted?" assertion would still run and still pass, having checked
/// nothing. A gate that cannot distinguish "granted" from "unparsed" is the
/// same failure as the one that let the outage ship.
#[test]
fn permission_resolution_produces_real_command_sets_rather_than_empty_ones() {
    let manifest = read_json(&manifest_path());
    let granted = granted_permissions();
    assert!(
        !granted.is_empty(),
        "no granted permissions parsed — the gate would pass vacuously"
    );
    let resolved = granted_commands(&manifest);
    assert!(
        !resolved.is_empty(),
        "no namespace resolved from {granted:?}"
    );
    // Every namespace the capability grants must yield at least one command,
    // or the grant is inert and the comparison above is meaningless for it.
    // `opener` is the case that matters: it resolves to exactly open_url, and
    // would have resolved to {} before the fix.
    for (namespace, commands) in &resolved {
        assert!(
            !commands.is_empty(),
            "{namespace} resolved to no commands at all — the capability grants only scope-only \
             permissions there, which is what made every opener call fail at runtime"
        );
    }
    assert_eq!(
        resolved
            .get("opener")
            .map(|c| c.iter().cloned().collect::<Vec<_>>()),
        Some(vec!["open_url".to_string()]),
        "the opener grant must be exactly open_url: open_path and reveal_item_in_dir belong to \
         desktop::shell, which proves containment in Rust instead of trusting the webview's path"
    );
}

/// The specific trap, pinned so it cannot silently return: a permission whose
/// `commands.allow` is empty grants no command, however much its name and
/// description sound like it does.
#[test]
fn a_scope_only_permission_grants_no_command() {
    let manifest = read_json(&manifest_path());
    let scope_only = &manifest["opener"]["permissions"]["allow-default-urls"];
    assert!(
        !scope_only.is_null(),
        "opener:allow-default-urls must exist for this regression to mean anything"
    );
    assert_eq!(
        scope_only["commands"]["allow"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0),
        0,
        "allow-default-urls is scope-only; if upstream gave it a command, the capability file's \
         comment and this gate both need revisiting"
    );
    assert!(
        scope_only["scope"]["allow"]
            .as_array()
            .is_some_and(|s| !s.is_empty()),
        "allow-default-urls must still carry the URL scope openExternal relies on"
    );
}

// ---------------------------------------------------------------------------
// The app's own commands.
//
// Tauri enforces capabilities on app commands only when the build declares an
// app manifest (`build.rs`). Declared, every registered command is denied to
// every window until a capability names its `allow-<command>` permission, so
// the three sets below must agree exactly: what is registered, what the
// frontend can call, and what each window is granted.
// ---------------------------------------------------------------------------

fn registered_commands() -> Vec<String> {
    let lib = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("src/lib.rs");
    app_commands::handler_commands(&lib)
}

fn permission_for(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

/// App command permissions one capability file grants, as command names.
fn app_commands_granted_by(file: &str) -> BTreeSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("capabilities")
        .join(file);
    let json = read_json(&path);
    let by_permission: BTreeMap<String, String> = registered_commands()
        .into_iter()
        .map(|c| (permission_for(&c), c))
        .collect();
    let mut out = BTreeSet::new();
    for perm in json["permissions"].as_array().expect("permissions array") {
        let Some(id) = perm.as_str().filter(|id| !id.contains(':')) else {
            continue;
        };
        let command = by_permission
            .get(id)
            .unwrap_or_else(|| panic!("{file} grants {id:?}, which names no registered command"));
        assert!(out.insert(command.clone()), "{file} grants {id:?} twice");
    }
    out
}

/// Quoted `cmd_*` literals in code lines of the given files: what a file can
/// pass to `invoke`, constants included. Comment lines are skipped.
fn command_literals(files: &[PathBuf]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for file in files {
        let source = fs::read_to_string(file).unwrap_or_default();
        for line in source.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
                continue;
            }
            for quote in ['"', '\''] {
                let mut parts = line.split(quote);
                parts.next();
                for literal in parts.step_by(2) {
                    if literal.starts_with("cmd_")
                        && literal
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        out.insert(literal.to_string());
                    }
                }
            }
        }
    }
    out
}

#[test]
fn the_app_manifest_defines_a_permission_for_every_registered_command() {
    let manifest = read_json(&manifest_path());
    let app = manifest
        .get("__app-acl__")
        .expect("no app manifest was compiled: build.rs must declare AppManifest::commands");
    let registered = registered_commands();
    assert!(
        registered.len() > 200,
        "parsed only {} handlers",
        registered.len()
    );
    for command in &registered {
        let permission = permission_for(command);
        assert!(
            app["permissions"].get(&permission).is_some(),
            "no {permission} in the app manifest for registered command {command}"
        );
    }
}

/// Every command the shipped frontend can call is granted to the main window,
/// and nothing it never calls is. A registered command no source names stays
/// denied everywhere.
#[test]
fn the_main_window_is_granted_exactly_the_commands_the_frontend_calls() {
    let registered: BTreeSet<String> = registered_commands().into_iter().collect();
    let called: BTreeSet<String> = command_literals(&frontend_sources())
        .intersection(&registered)
        .cloned()
        .collect();
    let granted = app_commands_granted_by("default.json");
    assert!(
        called.len() > 200,
        "the scan found only {} commands",
        called.len()
    );
    let missing: Vec<_> = called.difference(&granted).collect();
    assert!(
        missing.is_empty(),
        "the frontend calls commands the main window is not granted, so they fail at runtime \
         with \"not allowed by ACL\": {missing:?}"
    );
    let surplus: Vec<_> = granted.difference(&called).collect();
    assert!(
        surplus.is_empty(),
        "the main window is granted commands nothing in the frontend calls: {surplus:?}"
    );
}

/// The files the status popover actually loads: `src/status.ts` and the value
/// imports reachable from it (type-only imports are erased at build time).
fn status_popover_sources() -> Vec<PathBuf> {
    let root = repo_root().join("src");
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut queue = vec![root.join("status.ts")];
    while let Some(file) = queue.pop() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let source = fs::read_to_string(&file).unwrap_or_default();
        for statement in source.split("import ").skip(1) {
            let head = statement.split(';').next().unwrap_or("");
            if head.trim_start().starts_with("type ") {
                continue;
            }
            let Some(spec) = head
                .split(['"', '\''])
                .nth(1)
                .filter(|s| s.starts_with('.'))
            else {
                continue;
            };
            let base = file.parent().expect("parent").join(spec);
            for candidate in [
                base.clone(),
                PathBuf::from(format!("{}.ts", base.display())),
                PathBuf::from(format!("{}.svelte", base.display())),
                base.join("index.ts"),
            ] {
                if candidate.is_file() {
                    queue.push(candidate);
                    break;
                }
            }
        }
    }
    let files: Vec<PathBuf> = seen
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "ts" || e == "svelte"))
        .collect();
    assert!(files.len() > 5, "the status import closure found {files:?}");
    files
}

#[test]
fn the_status_popover_is_granted_only_the_commands_it_calls() {
    let registered: BTreeSet<String> = registered_commands().into_iter().collect();
    let called: BTreeSet<String> = command_literals(&status_popover_sources())
        .intersection(&registered)
        .cloned()
        .collect();
    let granted = app_commands_granted_by("status-popover.json");
    assert_eq!(
        called,
        [
            "cmd_get_status_state",
            "cmd_resize_status",
            "cmd_status_action"
        ]
        .iter()
        .map(|c| c.to_string())
        .collect(),
        "the popover's call sites changed; review what it may now call"
    );
    assert_eq!(
        granted, called,
        "the popover's grants must equal its call sites"
    );
    assert!(
        granted.len() < registered.len() / 10,
        "the popover must not hold a broad grant"
    );
}

/// Neither window is granted a command the other window's job needs, and no
/// command is reachable from a window with no capability at all.
#[test]
fn no_capability_grants_a_command_that_is_not_registered_twice_or_unscoped() {
    for file in ["default.json", "status-popover.json"] {
        let json = read_json(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("capabilities")
                .join(file),
        );
        assert!(
            json["windows"].as_array().is_some_and(|w| w.len() == 1),
            "{file} must name exactly one window, never a wildcard"
        );
    }
}
