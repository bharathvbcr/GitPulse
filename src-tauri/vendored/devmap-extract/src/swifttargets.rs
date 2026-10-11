//! Swift build targets, read from the manifests that name them.
//!
//! A Swift target is a module: every file in it shares one unqualified namespace
//! and `import Name` names it. Which files belong to which target is written in
//! two places this kernel can read as data:
//!
//! * `Package.swift` — `.target(name: "Core", path: "Sources/Core")`,
//!   `.executableTarget`, `.testTarget`. Read as text for the string-literal
//!   arguments only: the manifest is Swift *code*, and a target spelled any
//!   other way (a name from a variable, a `path:` built by concatenation) is
//!   reported as a gap rather than guessed.
//! * `project.yml` — the XcodeGen spec. `targets.<Name>.sources` lists the
//!   folders compiled into the target, so a folder named by two targets (the
//!   usual `Shared/`) belongs to both, which no directory heuristic can say.
//!
//! The path heuristic (`languages::swift_module_of`) stays the fallback for a
//! file no manifest claims. `project.pbxproj` is not read: it is an old-style
//! property list, and no corpus this index serves has an `.xcodeproj` without a
//! `project.yml` (see `docs/devmap/DIVERGENCES.md`).

use regex::Regex;

/// One target and what it compiles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwiftTarget {
    /// The module name `import` spells.
    pub name: String,
    /// Repo-relative folders (or files) the target compiles, `/`-separated.
    pub paths: Vec<String>,
    /// Whether the claim came from `Package.swift`. A package target owns its
    /// sources exclusively; an Xcode target may share a folder with another.
    pub package: bool,
}

/// Whether `file_name` is a manifest this module reads.
pub fn is_swift_manifest_name(file_name: &str) -> bool {
    file_name == "project.yml" || file_name == "Package.swift"
}

fn join(dir: &str, relative: &str) -> String {
    let relative = relative
        .trim()
        .trim_start_matches("./")
        .trim_end_matches('/');
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn directory_of(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The targets `rel_path` (a `Package.swift` or a `project.yml`) declares.
///
/// `Err` is a manifest that names targets this reader could not follow; the
/// caller records it as a gap, so the heuristic answering for those files is
/// never mistaken for the manifest's own.
pub fn parse_swift_manifest(rel_path: &str, source: &str) -> Result<Vec<SwiftTarget>, String> {
    if rel_path.ends_with("Package.swift") {
        parse_package_swift(rel_path, source)
    } else {
        parse_xcodegen(rel_path, source)
    }
}

fn parse_package_swift(rel_path: &str, source: &str) -> Result<Vec<SwiftTarget>, String> {
    let dir = directory_of(rel_path);
    let call = Regex::new(r"\.(target|executableTarget|testTarget|macro)\s*\(").expect("regex");
    let name_literal = Regex::new(r#"^\s*name\s*:\s*"([^"]+)""#).expect("regex");
    let path_literal = Regex::new(r#"(?:^|,)\s*path\s*:\s*"([^"]+)""#).expect("regex");
    let mut targets: Vec<SwiftTarget> = Vec::new();
    let mut unread: Vec<String> = Vec::new();
    let mut inside_until = 0usize;
    for found in call.captures_iter(source) {
        let whole = found.get(0).expect("match");
        // A `.target(name: "Util")` written inside another target's
        // `dependencies:` names a target; it does not declare one.
        if whole.start() < inside_until {
            continue;
        }
        let kind = &found[1];
        let Some(arguments) = balanced_arguments(&source[whole.end()..]) else {
            unread.push(format!(".{kind} call is not closed"));
            continue;
        };
        inside_until = whole.end() + arguments.len();
        let top = top_level(arguments);
        let Some(name) = name_literal.captures(&top).map(|c| c[1].to_string()) else {
            // A nested `.target(name: "Core")` inside `dependencies:` carries
            // its name at depth one of its own call; a name that is not a
            // literal is the case this reader cannot follow.
            if !top.trim_start().starts_with("name") || arguments.contains("name:") {
                unread.push(format!(".{kind} with a name that is not a string literal"));
            }
            continue;
        };
        let default = if kind == "testTarget" {
            "Tests"
        } else {
            "Sources"
        };
        let path = match path_literal.captures(&top) {
            Some(path) => join(dir, &path[1]),
            None => join(dir, &format!("{default}/{name}")),
        };
        // A `path:` written as anything but a literal is a gap.
        if top.contains("path:") && !path_literal.is_match(&top) {
            unread.push(format!(
                "target {name} has a path that is not a string literal"
            ));
            continue;
        }
        targets.push(SwiftTarget {
            name,
            paths: vec![path],
            package: true,
        });
    }
    if unread.is_empty() {
        Ok(targets)
    } else {
        unread.sort();
        unread.dedup();
        Err(format!(
            "Package.swift targets could not be read: {}",
            unread.join("; ")
        ))
    }
}

/// The text between a call's parentheses, given the text after its opening one.
fn balanced_arguments(rest: &str) -> Option<&str> {
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if in_string {
            match (escaped, ch) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `arguments` with everything nested inside brackets or parentheses removed,
/// leaving the call's own arguments.
fn top_level(arguments: &str) -> String {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut out = String::new();
    for ch in arguments.chars() {
        if in_string {
            if depth == 0 {
                out.push(ch);
            }
            match (escaped, ch) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                if depth == 0 {
                    out.push(ch);
                }
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let value = value.split(" #").next().unwrap_or(value).trim();
    value
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_string()
}

fn parse_xcodegen(rel_path: &str, source: &str) -> Result<Vec<SwiftTarget>, String> {
    let dir = directory_of(rel_path);
    let lines: Vec<(usize, &str)> = source
        .lines()
        .map(|line| (indent_of(line), line.trim_end()))
        .filter(|(_, line)| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .collect();
    let Some(start) = lines
        .iter()
        .position(|(indent, line)| *indent == 0 && line.trim_end() == "targets:")
    else {
        // Not an XcodeGen spec (or one with no targets): nothing to read.
        return Ok(Vec::new());
    };
    let mut targets = Vec::new();
    let mut unread: Vec<String> = Vec::new();
    let mut index = start + 1;
    let mut target_indent: Option<usize> = None;
    while index < lines.len() {
        let (indent, line) = lines[index];
        if indent == 0 {
            break;
        }
        let level = *target_indent.get_or_insert(indent);
        if indent != level || !line.trim_end().ends_with(':') {
            index += 1;
            continue;
        }
        let name = unquote(line.trim().trim_end_matches(':'));
        // The target's own block: every following line indented deeper.
        let mut end = index + 1;
        while end < lines.len() && lines[end].0 > level {
            end += 1;
        }
        let block = &lines[index + 1..end];
        let mut paths: Vec<String> = Vec::new();
        let mut read = false;
        if let Some(at) = block
            .iter()
            .position(|(_, text)| text.trim_start().starts_with("sources:"))
        {
            let (key_indent, key_line) = block[at];
            let value = key_line.trim_start()["sources:".len()..].trim();
            let mut entries: Vec<String> = Vec::new();
            if let Some(flow) = value.strip_prefix('[') {
                match flow.trim_end().strip_suffix(']') {
                    Some(inner) => {
                        entries.extend(inner.split(',').map(unquote).filter(|e| !e.is_empty()));
                        read = true;
                    }
                    None => unread.push(format!("target {name} has a multi-line flow list")),
                }
            } else if !value.is_empty() {
                entries.push(unquote(value));
                read = true;
            } else {
                read = true;
                for (item_indent, item) in &block[at + 1..] {
                    if *item_indent <= key_indent {
                        break;
                    }
                    let trimmed = item.trim_start();
                    if let Some(rest) = trimmed.strip_prefix("- ") {
                        let rest = rest.trim();
                        if let Some(path) = rest.strip_prefix("path:") {
                            entries.push(unquote(path));
                        } else if rest.contains(':') && !rest.starts_with('"') {
                            // `- name: …` / `- group: …`: a source entry shape
                            // this reader does not know.
                            unread.push(format!("target {name} has a sources entry `{rest}`"));
                            read = false;
                        } else {
                            entries.push(unquote(rest));
                        }
                    }
                }
            }
            paths = entries.iter().map(|entry| join(dir, entry)).collect();
        }
        if read && !paths.is_empty() {
            targets.push(SwiftTarget {
                name,
                paths,
                package: false,
            });
        }
        index = end;
    }
    if unread.is_empty() {
        Ok(targets)
    } else {
        Err(format!(
            "project.yml targets could not be read: {}",
            unread.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xcodegen_flow_and_block_sources_both_read() {
        let spec = "name: Demo\ntargets:\n  AppTarget:\n    type: application\n    sources: [AppTarget, Shared]\n  WidgetTarget:\n    sources:\n      - WidgetTarget\n      - path: Shared\n        excludes: [x]\n      - \"Other\"\npackages:\n  CoreKit:\n    path: CoreKit\n";
        let targets = parse_swift_manifest("ios/project.yml", spec).expect("reads");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].name, "AppTarget");
        assert_eq!(targets[0].paths, vec!["ios/AppTarget", "ios/Shared"]);
        assert_eq!(
            targets[1].paths,
            vec!["ios/WidgetTarget", "ios/Shared", "ios/Other"]
        );
        assert!(!targets[0].package);
    }

    #[test]
    fn a_spec_with_no_targets_reads_nothing_and_is_not_a_gap() {
        assert_eq!(
            parse_swift_manifest("project.yml", "name: x\npackages: {}\n"),
            Ok(Vec::new())
        );
    }

    #[test]
    fn an_xcodegen_sources_entry_it_cannot_follow_is_a_gap() {
        let spec = "targets:\n  A:\n    sources:\n      - name: App\n        path: X\n";
        let error = parse_swift_manifest("project.yml", spec).expect_err("a gap");
        assert!(error.contains("target A"), "{error}");
    }

    #[test]
    fn package_targets_read_literal_names_and_paths() {
        let manifest = r#"// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "P", targets: [
    .target(name: "Core", dependencies: [.target(name: "Util")]),
    .target(name: "Util", path: "Libs/Util"),
    .executableTarget(name: "Tool", dependencies: ["Core"]),
    .testTarget(name: "CoreTests", dependencies: ["Core"]),
])"#;
        let targets = parse_swift_manifest("pkg/Package.swift", manifest).expect("reads");
        let find = |name: &str| targets.iter().find(|t| t.name == name).expect(name);
        assert_eq!(find("Core").paths, vec!["pkg/Sources/Core"]);
        assert_eq!(find("Util").paths, vec!["pkg/Libs/Util"]);
        assert_eq!(find("Tool").paths, vec!["pkg/Sources/Tool"]);
        assert_eq!(find("CoreTests").paths, vec!["pkg/Tests/CoreTests"]);
        assert!(targets.iter().all(|t| t.package));
    }

    #[test]
    fn a_package_target_whose_name_is_not_a_literal_is_a_gap() {
        let manifest = "let t = [.target(name: coreName)]\n";
        let error = parse_swift_manifest("Package.swift", manifest).expect_err("a gap");
        assert!(error.contains("not a string literal"), "{error}");
    }

    #[test]
    fn a_package_path_that_is_not_a_literal_is_a_gap() {
        let manifest = "let t = [.target(name: \"A\", path: base + \"/A\")]\n";
        let error = parse_swift_manifest("Package.swift", manifest).expect_err("a gap");
        assert!(error.contains("not a string literal"), "{error}");
    }
}
