//! TypeScript / JavaScript project configs (`tsconfig.json`, `jsconfig.json`)
//! read for the one thing the resolver needs from them: how a non-relative
//! module specifier maps onto files in the tree.
//!
//! `import { x } from "@core/util"` names a file only through the config that
//! governs the importer — `compilerOptions.paths` and `baseUrl`, inherited
//! through `extends`, and in a solution-style root spread across the configs
//! its `references` name. Without them every such import was filed `External`:
//! the corpus was told a local module came from outside it.
//!
//! What is modelled, in TypeScript's own terms:
//!
//! * `extends` — a string or (TS 5.0) an array, relative to the extending
//!   config, `.json` optional; or a package name, looked up under the
//!   repository's own `node_modules`. Later entries override earlier ones and
//!   the extending config overrides all of them, per `compilerOptions` key.
//! * `baseUrl` — resolved against the config that *wrote* it.
//! * `paths` — targets resolved against the effective `baseUrl`, or, when there
//!   is none, against the config that wrote `paths`.
//! * `references[].path` — a directory (meaning its `tsconfig.json`) or a
//!   file. A referenced config's mappings follow the referencing config's own,
//!   which is what makes a solution-style root (`"files": []` plus
//!   `references` to `tsconfig.app.json`) map anything at all.
//!
//! Not modelled, and recorded in `docs/devmap/DIVERGENCES.md`: `include` /
//! `exclude` / `files` (a config governs its own directory, nearest first),
//! `rootDirs`, `moduleSuffixes`, `customConditions`, and package `exports`.
//!
//! Every read is bounded: a config over [`MAX_CONFIG_BYTES`] is skipped, an
//! `extends` / `references` chain stops at [`MAX_CHAIN_DEPTH`], a cycle stops at
//! the first repeat, and no path may leave the collect root.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A config larger than this is not a hand-written project config.
pub const MAX_CONFIG_BYTES: u64 = 1 << 20;

/// How many `extends` / `references` hops a chain may take.
pub const MAX_CHAIN_DEPTH: usize = 16;

/// One `paths` / `baseUrl` mapping, already resolved to repository-relative
/// directories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TsMapping {
    /// The effective `baseUrl`, repo-relative (`""` is the root).
    pub base_url: Option<String>,
    /// `(pattern, targets)` in declaration order. A pattern has at most one
    /// `*`; each target is repo-relative and carries the `*` it substitutes.
    pub paths: Vec<(String, Vec<String>)>,
}

/// A config that governs the files under `dir`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TsProject {
    /// Directory of the governing `tsconfig.json` / `jsconfig.json`,
    /// repo-relative with `/` separators; `""` is the root.
    pub dir: String,
    /// The config's own mapping first, then each referenced config's, in
    /// `references` order. Empty mappings are dropped.
    pub mappings: Vec<TsMapping>,
}

/// Whether `file_name` is a config discovery should hand to
/// [`build_ts_projects`]: a governing config, or a `tsconfig.*.json` that one
/// may name.
pub fn is_ts_config_name(file_name: &str) -> bool {
    file_name == "tsconfig.json"
        || file_name == "jsconfig.json"
        || (file_name.starts_with("tsconfig.") && file_name.ends_with(".json"))
}

/// Whether a config of this name governs its own directory.
fn governs_its_directory(rel_path: &str) -> bool {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    name == "tsconfig.json" || name == "jsconfig.json"
}

/// Strip JSONC: `//` and `/* */` comments outside strings, and a trailing
/// comma before `}` or `]`. TypeScript accepts both in every config.
pub fn strip_jsonc(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    // Trailing commas, outside strings: a second pass over the comment-free
    // text, so a comment between the comma and the bracket cannot hide it.
    let mut cleaned = String::with_capacity(out.len());
    let mut in_string = false;
    let mut escaped = false;
    let bytes: Vec<char> = out.chars().collect();
    for (index, &c) in bytes.iter().enumerate() {
        if in_string {
            cleaned.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
        } else if c == ',' {
            let next = bytes[index + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        cleaned.push(c);
    }
    cleaned
}

/// Normalise `base/relative` into a repo-relative path, refusing one that
/// climbs above the root.
fn join(base: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn parent_dir(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

/// `paths` as written: `(pattern, targets)` in declaration order.
type PathEntries = Vec<(String, Vec<String>)>;

/// A value tagged with the directory of the config that wrote it, which is
/// what a relative `baseUrl` or `paths` target resolves against.
type WrittenIn<T> = (T, String);

/// A config with its `extends` chain merged: `baseUrl`, `paths`, and the
/// referenced config paths (never inherited).
type Effective = (
    Option<WrittenIn<String>>,
    Option<WrittenIn<PathEntries>>,
    Vec<String>,
);

/// The raw, unmerged content of one config.
#[derive(Debug, Clone, Default)]
struct RawConfig {
    extends: Vec<String>,
    /// `(value, directory of the config that wrote it)`.
    base_url: Option<WrittenIn<String>>,
    paths: Option<WrittenIn<PathEntries>>,
    references: Vec<String>,
}

fn parse_raw(rel_path: &str, source: &str) -> Option<RawConfig> {
    let value: Value = serde_json::from_str(&strip_jsonc(source)).ok()?;
    let object = value.as_object()?;
    let dir = parent_dir(rel_path).to_string();
    let extends = match object.get("extends") {
        Some(Value::String(one)) => vec![one.clone()],
        Some(Value::Array(many)) => many
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let options = object.get("compilerOptions").and_then(Value::as_object);
    let base_url = options
        .and_then(|options| options.get("baseUrl"))
        .and_then(Value::as_str)
        .map(|value| (value.to_string(), dir.clone()));
    let paths = options
        .and_then(|options| options.get("paths"))
        .and_then(Value::as_object)
        .map(|paths| {
            let entries = paths
                .iter()
                .map(|(pattern, targets)| {
                    let targets = targets
                        .as_array()
                        .map(|targets| {
                            targets
                                .iter()
                                .filter_map(|target| target.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    (pattern.clone(), targets)
                })
                .filter(|(pattern, _)| pattern.matches('*').count() <= 1)
                .collect();
            (entries, dir.clone())
        });
    let references = object
        .get("references")
        .and_then(Value::as_array)
        .map(|references| {
            references
                .iter()
                .filter_map(|reference| reference.get("path").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(RawConfig {
        extends,
        base_url,
        paths,
        references,
    })
}

/// Reads configs by repo-relative path, bounded and memoised.
struct Reader<'a> {
    root: &'a Path,
    cache: BTreeMap<String, Option<RawConfig>>,
}

impl Reader<'_> {
    fn read(&mut self, rel_path: &str) -> Option<RawConfig> {
        if let Some(cached) = self.cache.get(rel_path) {
            return cached.clone();
        }
        let path = self.root.join(rel_path);
        let raw = std::fs::metadata(&path)
            .ok()
            .filter(|meta| meta.is_file() && meta.len() <= MAX_CONFIG_BYTES)
            .and_then(|_| std::fs::read_to_string(&path).ok())
            .and_then(|source| parse_raw(rel_path, &source));
        self.cache.insert(rel_path.to_string(), raw.clone());
        raw
    }

    /// The repo-relative config an `extends` entry names, if it exists.
    fn resolve_extends(&mut self, from_dir: &str, spec: &str) -> Option<String> {
        let candidates: Vec<String> = if spec.starts_with("./") || spec.starts_with("../") {
            let base = join(from_dir, spec)?;
            vec![
                base.clone(),
                format!("{base}.json"),
                format!("{base}/tsconfig.json"),
            ]
        } else if spec.starts_with('/') || spec.split('/').any(|segment| segment == "..") {
            return None;
        } else {
            // A package: the repository's own `node_modules`, nearest first.
            let mut dirs = Vec::new();
            let mut dir = from_dir.to_string();
            loop {
                dirs.push(dir.clone());
                if dir.is_empty() {
                    break;
                }
                dir = parent_dir(&dir).to_string();
            }
            dirs.iter()
                .flat_map(|dir| {
                    let base = join(dir, &format!("node_modules/{spec}"))?;
                    Some([
                        base.clone(),
                        format!("{base}.json"),
                        format!("{base}/tsconfig.json"),
                    ])
                })
                .flatten()
                .collect()
        };
        candidates.into_iter().find(|candidate| {
            std::fs::metadata(self.root.join(candidate)).is_ok_and(|meta| meta.is_file())
        })
    }

    fn resolve_reference(&mut self, from_dir: &str, spec: &str) -> Option<String> {
        let base = join(from_dir, spec)?;
        if base.ends_with(".json") {
            return Some(base);
        }
        Some(if base.is_empty() {
            "tsconfig.json".to_string()
        } else {
            format!("{base}/tsconfig.json")
        })
    }

    /// The config at `rel_path` with its `extends` chain merged in:
    /// `(baseUrl, paths)`, each still tagged with the directory it was
    /// written in, and the config's own `references`.
    fn effective(
        &mut self,
        rel_path: &str,
        depth: usize,
        seen: &mut BTreeSet<String>,
    ) -> Option<Effective> {
        if depth > MAX_CHAIN_DEPTH || !seen.insert(rel_path.to_string()) {
            return None;
        }
        let raw = self.read(rel_path)?;
        let dir = parent_dir(rel_path).to_string();
        let mut base_url = None;
        let mut paths = None;
        for parent in &raw.extends {
            let Some(parent_path) = self.resolve_extends(&dir, parent) else {
                continue;
            };
            if let Some((parent_base, parent_paths, _)) =
                self.effective(&parent_path, depth + 1, seen)
            {
                if parent_base.is_some() {
                    base_url = parent_base;
                }
                if parent_paths.is_some() {
                    paths = parent_paths;
                }
            }
        }
        if raw.base_url.is_some() {
            base_url = raw.base_url.clone();
        }
        if raw.paths.is_some() {
            paths = raw.paths.clone();
        }
        // `references` are not inherited through `extends`.
        let references = raw
            .references
            .iter()
            .filter_map(|reference| self.resolve_reference(&dir, reference))
            .collect();
        Some((base_url, paths, references))
    }

    fn mapping(&mut self, rel_path: &str) -> (Option<TsMapping>, Vec<String>) {
        let mut seen = BTreeSet::new();
        let Some((base_url, paths, references)) = self.effective(rel_path, 0, &mut seen) else {
            return (None, Vec::new());
        };
        let base_url = base_url.and_then(|(value, written_in)| join(&written_in, &value));
        let paths = paths
            .map(|(entries, written_in)| {
                let anchor = base_url.clone().unwrap_or(written_in);
                entries
                    .into_iter()
                    .map(|(pattern, targets)| {
                        let targets = targets
                            .iter()
                            .filter(|target| target.matches('*').count() <= 1)
                            .filter_map(|target| join(&anchor, target))
                            .collect();
                        (pattern, targets)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mapping =
            (base_url.is_some() || !paths.is_empty()).then_some(TsMapping { base_url, paths });
        (mapping, references)
    }
}

/// Build the governing projects from the configs discovery found.
///
/// `found` holds repo-relative paths whose names pass [`is_ts_config_name`].
/// Configs reached only through `extends` / `references` are read from disk
/// under `root` as needed, within the same bounds.
pub fn build_ts_projects(root: &Path, found: &[String]) -> Vec<TsProject> {
    let mut reader = Reader {
        root,
        cache: BTreeMap::new(),
    };
    let mut projects = Vec::new();
    for rel_path in found.iter().filter(|path| governs_its_directory(path)) {
        let mut mappings = Vec::new();
        let mut visited = BTreeSet::new();
        let mut queue = vec![(rel_path.clone(), 0usize)];
        while let Some((config, depth)) = queue.pop() {
            if depth > MAX_CHAIN_DEPTH || !visited.insert(config.clone()) {
                continue;
            }
            let (mapping, references) = reader.mapping(&config);
            if let Some(mapping) = mapping {
                mappings.push(mapping);
            }
            // Popped from the back, so push in reverse to visit in order.
            for reference in references.into_iter().rev() {
                queue.push((reference, depth + 1));
            }
        }
        if !mappings.is_empty() {
            projects.push(TsProject {
                dir: parent_dir(rel_path).to_string(),
                mappings,
            });
        }
    }
    // Deepest first, so the nearest governing config is the first that
    // contains a file.
    projects.sort_by(|left, right| {
        right
            .dir
            .split('/')
            .count()
            .cmp(&left.dir.split('/').count())
            .then_with(|| right.dir.len().cmp(&left.dir.len()))
            .then_with(|| left.dir.cmp(&right.dir))
    });
    projects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_comments_and_trailing_commas_are_stripped_outside_strings() {
        let source = r#"{
  // a line comment
  "a": "http://not-a-comment", /* block */
  "b": ["x", "y",],
  "c": "/* kept */",
}"#;
        let value: Value = serde_json::from_str(&strip_jsonc(source)).unwrap();
        assert_eq!(value["a"], "http://not-a-comment");
        assert_eq!(value["b"], serde_json::json!(["x", "y"]));
        assert_eq!(value["c"], "/* kept */");
    }

    #[test]
    fn a_join_cannot_climb_above_the_root() {
        assert_eq!(join("a/b", "../c").as_deref(), Some("a/c"));
        assert_eq!(join("", "../c"), None);
    }
}
