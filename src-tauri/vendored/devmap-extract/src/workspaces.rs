//! npm / yarn / pnpm / bun workspaces, read from `package.json`.
//!
//! `import { x } from "@acme/shared"` names a file only through the workspace
//! that links `@acme/shared` to a directory of this tree: a root `package.json`
//! lists member directories in `workspaces` (an array, or `{ "packages": [...] }`
//! as yarn writes it), and each member's own `name` is the specifier. Without
//! the mapping every such import was filed `External` — the corpus was told a
//! local package came from outside it — while the calls through it bound by
//! bare name at the global tier, right only because the name happened to be
//! unique.
//!
//! What is modelled: member discovery by the workspace patterns (`*` and `**`,
//! `!` negations), the member's `name`, and the entry its `types` / `module` /
//! `main` / `exports["."]` strings name. Not modelled, and recorded in
//! `docs/devmap/DIVERGENCES.md`: conditional `exports` subpaths, `imports`
//! (`#internal` specifiers), and pnpm's `pnpm-workspace.yaml` (YAML a build
//! tool owns; reopen when an owner repository has one without a `workspaces`
//! field).
//!
//! The manifest is data collected alongside source, never as source: a
//! `package.json` that cannot be parsed, and a workspace pattern that names no
//! member, are returned as reasons for the caller to record as coverage gaps,
//! not dropped. A two-member claim on one name is dropped *and* reported,
//! because choosing one would be a guess about which package the importer
//! meant.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A package that a workspace links into the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkspacePackage {
    /// The specifier the package is imported by (`@acme/shared`).
    pub name: String,
    /// Repo-relative directory holding the package's `package.json`; `""` is
    /// the root.
    pub dir: String,
    /// Entry files the manifest names, relative to `dir`, in preference order:
    /// `types`, `module`, `main`, then `exports["."]`. A build output path
    /// (`dist/index.js`) is recorded as written; the resolver tries the
    /// conventional source entry after these.
    pub entries: Vec<String>,
}

/// What one `package.json` says that this module reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackageManifest {
    pub name: Option<String>,
    /// The workspace patterns this manifest declares, relative to its own
    /// directory. Empty for a package that is not a workspace root.
    pub workspaces: Vec<String>,
    pub entries: Vec<String>,
}

/// Parse one `package.json`, or say why it could not be.
pub fn parse_package_json(source: &str) -> Result<PackageManifest, String> {
    let value: Value = serde_json::from_str(source)
        .map_err(|error| format!("could not be parsed as JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "could not be parsed: the top level is not a JSON object".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let workspaces = match object.get("workspaces") {
        Some(Value::Array(patterns)) => patterns.iter().filter_map(Value::as_str).collect(),
        Some(Value::Object(table)) => table
            .get("packages")
            .and_then(Value::as_array)
            .map(|patterns| patterns.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
    .into_iter()
    .map(str::to_string)
    .collect();
    let mut entries = Vec::new();
    for key in ["types", "typings", "module", "main"] {
        if let Some(entry) = object.get(key).and_then(Value::as_str) {
            entries.push(entry.to_string());
        }
    }
    if let Some(exports) = object.get("exports") {
        collect_root_export(exports, &mut entries);
    }
    entries.retain(|entry| !entry.is_empty());
    let mut seen = BTreeSet::new();
    entries.retain(|entry| seen.insert(entry.clone()));
    Ok(PackageManifest {
        name,
        workspaces,
        entries,
    })
}

/// The string paths `exports` gives the package root: `"./x.js"`, or `"."`
/// mapped to a string or to a conditions object whose leaves are strings.
fn collect_root_export(exports: &Value, entries: &mut Vec<String>) {
    fn leaves(value: &Value, entries: &mut Vec<String>, depth: usize) {
        if depth > 4 {
            return;
        }
        match value {
            Value::String(path) => entries.push(path.clone()),
            Value::Object(conditions) => {
                for condition in ["types", "import", "module", "default", "require"] {
                    if let Some(inner) = conditions.get(condition) {
                        leaves(inner, entries, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
    match exports {
        Value::String(path) => entries.push(path.clone()),
        Value::Object(table) => match table.get(".") {
            Some(root) => leaves(root, entries, 0),
            // `exports` written as a bare conditions object.
            None if !table.keys().any(|key| key.starts_with('.')) => leaves(exports, entries, 0),
            None => {}
        },
        _ => {}
    }
}

/// A workspace pattern as a regex over a repo-relative directory: `*` is one
/// path segment's worth of anything, `**` any number of segments.
fn pattern_regex(pattern: &str) -> Option<Regex> {
    let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
    let mut out = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                // `**/` also matches nothing at all: `a/**/b` is `a/b`.
                if chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    Regex::new(&out).ok()
}

/// Whether `dir`, relative to a workspace root, is selected by `patterns`: it
/// matches a positive pattern and no `!` negation.
fn selected(relative_dir: &str, positives: &[Regex], negatives: &[Regex]) -> bool {
    positives
        .iter()
        .any(|pattern| pattern.is_match(relative_dir))
        && !negatives
            .iter()
            .any(|pattern| pattern.is_match(relative_dir))
}

/// Everything the workspaces of one tree say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspaces {
    pub packages: Vec<WorkspacePackage>,
    /// `(manifest, reason)` for what this pass could not turn into a mapping:
    /// a workspace pattern that names no member, and a package name two
    /// members claim. Each is a hole the caller records, never a silence.
    pub gaps: Vec<(String, String)>,
}

/// Link member packages to workspace roots.
///
/// `manifests` holds every parsed `package.json` as `(repo-relative path,
/// manifest)`. A member is a manifest, with a `name`, in a directory a root's
/// patterns select; the root itself is a member only when a pattern selects
/// `.`. Two members claiming one name are both dropped and reported.
pub fn link_workspaces(manifests: &[(String, PackageManifest)]) -> Workspaces {
    let dir_of = |path: &str| -> String {
        path.rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default()
    };
    let mut gaps = Vec::new();
    let mut claims: BTreeMap<String, Vec<WorkspacePackage>> = BTreeMap::new();
    for (root_path, root) in manifests.iter().filter(|(_, m)| !m.workspaces.is_empty()) {
        let root_dir = dir_of(root_path);
        let (negated, positive): (Vec<&String>, Vec<&String>) = root
            .workspaces
            .iter()
            .partition(|pattern| pattern.starts_with('!'));
        let negatives: Vec<Regex> = negated
            .iter()
            .filter_map(|pattern| pattern_regex(pattern.trim_start_matches('!')))
            .collect();
        for pattern in positive {
            let Some(regex) = pattern_regex(pattern) else {
                gaps.push((
                    root_path.clone(),
                    format!(
                        "workspace pattern {pattern:?} is not a pattern this reader understands"
                    ),
                ));
                continue;
            };
            let one = [regex];
            let mut matched_any = false;
            for (member_path, member) in manifests {
                let member_dir = dir_of(member_path);
                let relative = if root_dir.is_empty() {
                    member_dir.as_str()
                } else if member_dir == root_dir {
                    ""
                } else if let Some(rest) = member_dir.strip_prefix(&format!("{root_dir}/")) {
                    rest
                } else {
                    continue;
                };
                if !selected(relative, &one, &negatives) {
                    continue;
                }
                matched_any = true;
                if let Some(name) = &member.name {
                    let package = WorkspacePackage {
                        name: name.clone(),
                        dir: member_dir.clone(),
                        entries: member.entries.clone(),
                    };
                    let claimants = claims.entry(name.clone()).or_default();
                    if !claimants.iter().any(|known| known.dir == package.dir) {
                        claimants.push(package);
                    }
                }
            }
            if !matched_any {
                gaps.push((
                    root_path.clone(),
                    format!("workspace pattern {pattern:?} names no directory with a package.json"),
                ));
            }
        }
    }
    let mut packages = Vec::new();
    for (name, claimants) in claims {
        if claimants.len() == 1 {
            packages.extend(claimants);
        } else {
            let dirs: Vec<&str> = claimants
                .iter()
                .map(|package| package.dir.as_str())
                .collect();
            gaps.push((
                format!("{}/package.json", claimants[0].dir)
                    .trim_start_matches('/')
                    .to_string(),
                format!(
                    "package name {name:?} is claimed by more than one workspace member: {dirs:?}"
                ),
            ));
        }
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    gaps.sort();
    gaps.dedup();
    Workspaces { packages, gaps }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: Option<&str>, workspaces: &[&str]) -> PackageManifest {
        PackageManifest {
            name: name.map(str::to_string),
            workspaces: workspaces.iter().map(|w| (*w).to_string()).collect(),
            entries: Vec::new(),
        }
    }

    #[test]
    fn a_workspace_array_and_yarns_packages_object_both_parse() {
        let array = parse_package_json(r#"{"workspaces": ["apps/*", "packages/*"]}"#).unwrap();
        assert_eq!(array.workspaces, ["apps/*", "packages/*"]);
        let object =
            parse_package_json(r#"{"workspaces": {"packages": ["libs/*"], "nohoist": ["x"]}}"#)
                .unwrap();
        assert_eq!(object.workspaces, ["libs/*"]);
    }

    #[test]
    fn entries_come_from_types_module_main_and_the_root_export() {
        let parsed = parse_package_json(
            r#"{"name":"p","types":"src/index.ts","main":"dist/index.js",
                "exports":{".":{"import":"./dist/index.mjs","types":"./src/index.ts"},"./x":"./x.js"}}"#,
        )
        .unwrap();
        assert_eq!(
            parsed.entries,
            [
                "src/index.ts",
                "dist/index.js",
                "./src/index.ts",
                "./dist/index.mjs"
            ]
        );
    }

    #[test]
    fn a_manifest_that_is_not_json_is_an_error_not_an_empty_manifest() {
        assert!(parse_package_json("{ not json")
            .unwrap_err()
            .contains("JSON"));
        assert!(parse_package_json("[1]")
            .unwrap_err()
            .contains("not a JSON object"));
    }

    #[test]
    fn members_are_the_directories_a_pattern_selects() {
        let linked = link_workspaces(&[
            (
                "package.json".into(),
                manifest(Some("root"), &["packages/*", "!packages/skip"]),
            ),
            (
                "packages/shared/package.json".into(),
                manifest(Some("@acme/shared"), &[]),
            ),
            (
                "packages/skip/package.json".into(),
                manifest(Some("@acme/skip"), &[]),
            ),
            (
                "tools/package.json".into(),
                manifest(Some("@acme/tools"), &[]),
            ),
        ]);
        let names: Vec<&str> = linked.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["@acme/shared"]);
        assert_eq!(linked.packages[0].dir, "packages/shared");
        assert!(linked.gaps.is_empty(), "{:?}", linked.gaps);
    }

    #[test]
    fn a_double_star_pattern_reaches_nested_members() {
        let linked = link_workspaces(&[
            ("package.json".into(), manifest(None, &["packages/**"])),
            (
                "packages/a/b/package.json".into(),
                manifest(Some("deep"), &[]),
            ),
            (
                "packages/top/package.json".into(),
                manifest(Some("top"), &[]),
            ),
        ]);
        assert_eq!(linked.packages.len(), 2);
    }

    #[test]
    fn a_nested_workspace_root_governs_only_its_own_directory() {
        let linked = link_workspaces(&[
            ("sub/package.json".into(), manifest(None, &["pkgs/*"])),
            ("sub/pkgs/a/package.json".into(), manifest(Some("a"), &[])),
            ("pkgs/b/package.json".into(), manifest(Some("b"), &[])),
        ]);
        let names: Vec<&str> = linked.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a"]);
    }

    #[test]
    fn a_pattern_naming_no_member_is_a_gap() {
        let linked = link_workspaces(&[
            (
                "package.json".into(),
                manifest(None, &["packages/*", "missing/*"]),
            ),
            ("packages/a/package.json".into(), manifest(Some("a"), &[])),
        ]);
        assert_eq!(linked.gaps.len(), 1);
        assert!(linked.gaps[0].1.contains("missing/*"), "{:?}", linked.gaps);
    }

    #[test]
    fn a_name_two_members_claim_is_dropped_and_reported() {
        let linked = link_workspaces(&[
            ("package.json".into(), manifest(None, &["packages/*"])),
            ("packages/a/package.json".into(), manifest(Some("dup"), &[])),
            ("packages/b/package.json".into(), manifest(Some("dup"), &[])),
        ]);
        assert!(linked.packages.is_empty());
        assert!(linked.gaps.iter().any(|(_, reason)| reason.contains("dup")));
    }
}
