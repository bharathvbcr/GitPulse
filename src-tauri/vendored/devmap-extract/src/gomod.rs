//! Go module manifests (`go.mod`) collected alongside source, not as source.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One `go.mod` in the indexed tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GoModule {
    pub prefix: String,
    /// Directory containing this `go.mod`, repo-relative with `/` separators.
    /// Empty string means the collect root.
    pub dir: String,
    /// `(module_path, replace_target)` in declaration order. Relative targets
    /// are stored as written; resolution joins them with `dir`.
    pub replaces: Vec<(String, String)>,
}

pub fn parse_go_mod(rel_path: &str, source: &str) -> Option<GoModule> {
    let dir = Path::new(rel_path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .filter(|parent| !parent.is_empty() && parent != ".")
        .unwrap_or_default();
    let mut prefix = None;
    let mut replaces = Vec::new();
    let mut in_replace_block = false;

    for raw in source.lines() {
        let line = strip_go_line_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if in_replace_block {
            if line.starts_with(')') {
                in_replace_block = false;
                continue;
            }
            if let Some(replace) = parse_replace_spec(&line) {
                replaces.push(replace);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("module ") {
            let rest = unquote(rest.trim());
            if !rest.is_empty() && !rest.starts_with('(') {
                prefix = Some(rest);
            }
            continue;
        }
        if line.starts_with("replace (") {
            in_replace_block = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("replace ") {
            if let Some(replace) = parse_replace_spec(rest) {
                replaces.push(replace);
            }
        }
    }
    Some(GoModule {
        prefix: prefix?,
        dir,
        replaces,
    })
}

fn strip_go_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(index) => &line[..index],
        None => line,
    }
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('`')
        .trim()
        .to_string()
}

fn parse_replace_spec(spec: &str) -> Option<(String, String)> {
    let (left, right) = spec.split_once("=>")?;
    let module = left.split_whitespace().next().map(unquote)?;
    let target = right.split_whitespace().next().map(unquote)?;
    if module.is_empty() || target.is_empty() {
        return None;
    }
    Some((module, target))
}

/// Collect every non-gitignored `go.mod` under `root`.
pub fn collect_go_modules(root: &Path) -> anyhow::Result<Vec<GoModule>> {
    let mut modules = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build();
    for result in walker {
        let entry = result?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) != Some("go.mod") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let Some(rel_str) = rel.to_str() else {
            continue;
        };
        let rel_str = rel_str.replace('\\', "/");
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        if let Some(module) = parse_go_mod(&rel_str, &source) {
            modules.push(module);
        }
    }
    modules.sort_by(|left, right| {
        right
            .prefix
            .len()
            .cmp(&left.prefix.len())
            .then_with(|| left.prefix.cmp(&right.prefix))
            .then_with(|| left.dir.cmp(&right.dir))
    });
    Ok(modules)
}

/// Git worktree root containing `start`, if any. `.git` may be a directory or
/// a gitlink file (worktrees).
pub fn git_worktree_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_module_and_block_replace() {
        let parsed = parse_go_mod(
            "backend/go.mod",
            r#"
module scholarlm/backend/go_orchestrator

go 1.22

replace (
    github.com/acme/lib v1.2.3 => ../lib
    example.com/other => ./third_party/other
)
"#,
        )
        .unwrap();
        assert_eq!(parsed.prefix, "scholarlm/backend/go_orchestrator");
        assert_eq!(parsed.dir, "backend");
        assert_eq!(
            parsed.replaces,
            vec![
                ("github.com/acme/lib".into(), "../lib".into()),
                ("example.com/other".into(), "./third_party/other".into()),
            ]
        );
    }

    #[test]
    fn comments_and_missing_module_are_fail_closed() {
        assert!(parse_go_mod("go.mod", "replace a => b\n").is_none());
        let parsed = parse_go_mod(
            "go.mod",
            "module example.com/app // trailing\nreplace a => b // c\n",
        )
        .unwrap();
        assert_eq!(parsed.prefix, "example.com/app");
        assert_eq!(parsed.replaces, vec![("a".into(), "b".into())]);
    }
}

#[cfg(test)]
mod parse_guard_tests {
    use super::*;

    /// `go.mod` parsing keeps every guard it declares.
    ///
    /// Mutation testing flipped each conjunction in `parse_go_mod` and
    /// `parse_replace_spec` without a failure. These feed Go import resolution:
    /// a wrong module prefix or a half-parsed `replace` sends imports to the
    /// wrong files, and since both source and target exist nothing errors — the
    /// graph is simply wrong.
    #[test]
    fn module_prefix_rejects_empty_and_block_forms() {
        let parsed = parse_go_mod("go.mod", "module example.com/app\n").unwrap();
        assert_eq!(parsed.prefix, "example.com/app");

        // `module (` opens a block; the literal `(` is not a module path.
        let block = parse_go_mod("go.mod", "module (\n");
        assert!(
            block.is_none() || block.unwrap().prefix.is_empty(),
            "a `module (` block header must not be taken as the module path"
        );

        // An empty module line yields no prefix.
        let empty = parse_go_mod("go.mod", "module \n");
        assert!(empty.is_none() || empty.unwrap().prefix.is_empty());
    }

    /// The module directory is repo-relative, and `.`/empty normalise away.
    #[test]
    fn module_directory_is_normalised() {
        assert_eq!(parse_go_mod("go.mod", "module m\n").unwrap().dir, "");
        assert_eq!(
            parse_go_mod("sub/pkg/go.mod", "module m\n").unwrap().dir,
            "sub/pkg"
        );

        // A `./`-prefixed path has parent `.`, which must normalise to the
        // collect root and not to a literal `.` component. `dir` is joined with
        // relative replace targets, so a stray `.` silently produces `./x`
        // paths that match no indexed file.
        assert_eq!(
            parse_go_mod("./go.mod", "module m\n").unwrap().dir,
            "",
            "`.` must normalise to the collect root"
        );
        // Only the bare `.` parent is normalised; deeper paths are kept as
        // written, which is what the collect walker produces.
        assert_eq!(
            parse_go_mod("./sub/go.mod", "module m\n").unwrap().dir,
            "./sub"
        );
    }

    /// A `replace` needs both halves; either side empty yields nothing.
    ///
    /// `module.is_empty() || target.is_empty()` was mutable to `&&`, which
    /// accepts a replace with one empty half — rewriting an import to the empty
    /// path, or rewriting the empty path to somewhere real.
    #[test]
    fn a_replace_directive_requires_both_halves() {
        let single =
            parse_go_mod("go.mod", "module m\nreplace example.com/a => ./local/a\n").unwrap();
        assert_eq!(
            single.replaces,
            vec![("example.com/a".to_string(), "./local/a".to_string())],
            "a single-line replace must parse both halves"
        );

        // Block form must parse too.
        let block = parse_go_mod(
            "go.mod",
            "module m\nreplace (\n  example.com/b => ./local/b\n)\n",
        )
        .unwrap();
        assert!(
            block
                .replaces
                .iter()
                .any(|(from, to)| from == "example.com/b" && to == "./local/b"),
            "a replace block must parse its entries: {:?}",
            block.replaces
        );

        // A malformed replace contributes nothing rather than half an entry.
        let malformed = parse_go_mod("go.mod", "module m\nreplace example.com/c =>\n").unwrap();
        assert!(
            malformed.replaces.is_empty(),
            "a replace missing its target must be dropped, not half-recorded: {:?}",
            malformed.replaces
        );

        // A quoted empty half parses as a *present but empty* token, so it
        // reaches the emptiness guard rather than the `?` above it. This is the
        // only shape that distinguishes `||` from `&&` there: with `&&` exactly
        // one empty half is accepted, binding an import to the empty path.
        assert_eq!(
            parse_replace_spec(r#""" => ./local/x"#),
            None,
            "an empty module half must be rejected"
        );
        assert_eq!(
            parse_replace_spec(r#"example.com/d => """#),
            None,
            "an empty target half must be rejected"
        );
        assert_eq!(
            parse_replace_spec("example.com/e => ./local/e"),
            Some(("example.com/e".to_string(), "./local/e".to_string())),
            "positive control: a well-formed replace still parses"
        );
    }
}
