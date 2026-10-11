//! Python source roots, read from the manifests that declare them.
//!
//! `from pkg.core import Engine` names `lib/pkg/core.py` only to a reader who
//! knows `lib/` is on the import path, and that knowledge is written down in
//! the project's own manifest — `pythonpath` for pytest, `packages` for hatch,
//! `where` / `package-dir` for setuptools, `src` for ruff. The resolver probed
//! `src/` and nothing else, so every package under any other root had its
//! imports filed unresolved (Lappi-decision: 550 of 1,184 imports of its own
//! `qd_*` packages, all under `python/`).
//!
//! This module only reads: a manifest in, the roots it declares out, each
//! relative to the directory holding the manifest. Deciding which importer a
//! manifest governs, and whether a root holds the module, is the resolver's.
//! A manifest that does not parse, or that names a root outside the
//! repository, is an `Err` the caller records as a coverage gap — silence
//! would read as "this project declares no roots".

/// The roots one manifest declares, as repo-relative directories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PythonSourceRoots {
    /// Repo-relative directory holding the manifest; empty at the root. The
    /// manifest governs every file under it.
    pub dir: String,
    /// Directories modules are imported from, repo-relative, `/`-separated.
    /// Never empty strings and never containing `..`: the manifest's own
    /// directory is a root already and needs no entry.
    pub roots: Vec<String>,
}

/// `dir` joined with a manifest-relative `path`, normalised, or an error when
/// the result leaves the repository. `Ok(None)` is the manifest's own
/// directory.
fn anchor(dir: &str, path: &str) -> Result<Option<String>, String> {
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    let path = path.trim().replace('\\', "/");
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return Err(format!("source root `{path}` is an absolute path"));
    }
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("source root `{path}` leaves the repository"));
                }
            }
            part => parts.push(part),
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

/// The directory a package path is imported from: `lib/pkg` is imported as
/// `pkg` from `lib`. A bare `pkg` is imported from the manifest's directory.
fn package_parent(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent)
}

fn strings(value: &toml::Value) -> Vec<String> {
    match value {
        toml::Value::String(text) => text.split_whitespace().map(str::to_string).collect(),
        toml::Value::Array(items) => items
            .iter()
            .filter_map(toml::Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn dig<'a>(table: &'a toml::Table, path: &[&str]) -> Option<&'a toml::Value> {
    let (first, rest) = path.split_first()?;
    let value = table.get(*first)?;
    if rest.is_empty() {
        Some(value)
    } else {
        dig(value.as_table()?, rest)
    }
}

/// Settle raw manifest-relative entries into the manifest's roots.
fn finish(dir: &str, declared: Vec<String>) -> Result<PythonSourceRoots, String> {
    let mut roots = Vec::new();
    for path in declared {
        if let Some(root) = anchor(dir, &path)? {
            roots.push(root);
        }
    }
    roots.sort();
    roots.dedup();
    Ok(PythonSourceRoots {
        dir: dir.to_string(),
        roots,
    })
}

/// The roots a `pyproject.toml` at `dir` declares. `Err` carries the reason
/// the manifest could not be read as a whole.
pub fn parse_pyproject_roots(dir: &str, source: &str) -> Result<PythonSourceRoots, String> {
    let manifest: toml::Table = source
        .parse()
        .map_err(|error| format!("could not be parsed as TOML: {error}"))?;
    let mut declared: Vec<String> = Vec::new();
    for path in [
        &["tool", "pytest", "ini_options", "pythonpath"][..],
        &["tool", "setuptools", "packages", "find", "where"],
        &["tool", "ruff", "src"],
    ] {
        if let Some(value) = dig(&manifest, path) {
            declared.extend(strings(value));
        }
    }
    // Hatch names the package directories; the import root is each one's
    // parent.
    if let Some(value) = dig(
        &manifest,
        &["tool", "hatch", "build", "targets", "wheel", "packages"],
    ) {
        declared.extend(
            strings(value)
                .iter()
                .map(|package| package_parent(package).to_string()),
        );
    }
    // `package-dir`: `"" = "src"` roots every package under `src`; `pkg =
    // "lib/pkg"` places one package, importable from `lib` when the directory
    // keeps the package's own name. Any other mapping renames the package and
    // is no root at all.
    if let Some(table) =
        dig(&manifest, &["tool", "setuptools", "package-dir"]).and_then(toml::Value::as_table)
    {
        for (package, value) in table {
            if let Some(path) = value.as_str() {
                declared.extend(package_dir_root(package, path));
            }
        }
    }
    finish(dir, declared)
}

fn package_dir_root(package: &str, path: &str) -> Option<String> {
    if package.is_empty() {
        return Some(path.to_string());
    }
    let path = path.trim_end_matches('/');
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let name = package.rsplit('.').next().unwrap_or(package);
    (leaf == name && package.split('.').count() == 1).then(|| package_parent(path).to_string())
}

/// The roots a `setup.cfg` at `dir` declares: `[options] package_dir` and
/// `[options.packages.find] where`. The ini shape is small enough to read by
/// line; a file with no section header at all is the only unreadable one.
pub fn parse_setup_cfg_roots(dir: &str, source: &str) -> Result<PythonSourceRoots, String> {
    let mut section = String::new();
    let mut key = String::new();
    let mut declared: Vec<String> = Vec::new();
    let mut saw_section = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let continuation = line.starts_with(char::is_whitespace);
        if !continuation && trimmed.starts_with('[') {
            let Some(name) = trimmed
                .strip_prefix('[')
                .and_then(|rest| rest.split_once(']'))
            else {
                return Err("could not be parsed: unterminated section header".to_string());
            };
            section = name.0.trim().to_string();
            key.clear();
            saw_section = true;
            continue;
        }
        if !saw_section {
            return Err("could not be parsed: a setting precedes any section".to_string());
        }
        let (entry_key, value) = if continuation {
            (key.clone(), trimmed.to_string())
        } else if let Some((name, value)) = trimmed.split_once('=') {
            key = name.trim().to_string();
            (key.clone(), value.trim().to_string())
        } else {
            continue;
        };
        match (section.as_str(), entry_key.as_str()) {
            ("options.packages.find", "where") => declared.push(value),
            ("options", "package_dir") => {
                // `= src` roots everything; `pkg = lib/pkg` places one package.
                // On a continuation line the key is the part before `=`.
                if let Some((package, path)) = value.split_once('=') {
                    declared.extend(package_dir_root(package.trim(), path.trim()));
                }
            }
            _ => {}
        }
    }
    finish(dir, declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(result: Result<PythonSourceRoots, String>) -> Vec<String> {
        result.unwrap().roots
    }

    #[test]
    fn each_declaring_table_contributes_its_roots() {
        assert_eq!(
            roots(parse_pyproject_roots(
                "",
                "[tool.pytest.ini_options]\npythonpath = [\"lib\", \".\"]\n"
            )),
            ["lib"]
        );
        assert_eq!(
            roots(parse_pyproject_roots(
                "",
                "[tool.hatch.build.targets.wheel]\npackages = [\"python/rootpkg\", \"top\"]\n"
            )),
            ["python"]
        );
        assert_eq!(
            roots(parse_pyproject_roots(
                "",
                "[tool.setuptools.packages.find]\nwhere = [\"src\"]\n"
            )),
            ["src"]
        );
        assert_eq!(
            roots(parse_pyproject_roots(
                "",
                "[tool.ruff]\nsrc = [\"python\"]\n"
            )),
            ["python"]
        );
        assert_eq!(
            roots(parse_pyproject_roots(
                "",
                "[tool.setuptools.package-dir]\n\"\" = \"pysrc\"\nother = \"lib/other\"\nrenamed = \"lib/else\"\n"
            )),
            ["lib", "pysrc"]
        );
    }

    #[test]
    fn a_manifest_governs_its_own_directory() {
        let parsed =
            parse_pyproject_roots("svc/api", "[tool.ruff]\nsrc = [\"src\", \"../shared\"]\n")
                .unwrap();
        assert_eq!(parsed.dir, "svc/api");
        assert_eq!(parsed.roots, ["svc/api/src", "svc/shared"]);
    }

    #[test]
    fn a_manifest_that_does_not_read_is_an_error_not_an_empty_answer() {
        assert!(parse_pyproject_roots("", "[tool.ruff\nsrc = [")
            .unwrap_err()
            .contains("TOML"));
        assert!(
            parse_pyproject_roots("", "[tool.ruff]\nsrc = [\"../out\"]\n")
                .unwrap_err()
                .contains("leaves the repository")
        );
        assert!(parse_pyproject_roots("", "[tool.ruff]\nsrc = [\"/abs\"]\n")
            .unwrap_err()
            .contains("absolute"));
        assert!(parse_setup_cfg_roots("", "name = x\n")
            .unwrap_err()
            .contains("precedes any section"));
        assert!(parse_setup_cfg_roots("", "[options\nx = 1\n")
            .unwrap_err()
            .contains("unterminated"));
    }

    #[test]
    fn setup_cfg_reads_package_dir_and_find_where() {
        let cfg = "[options]\npackage_dir =\n    = python\npackages = find:\n\n[options.packages.find]\nwhere = lib\n";
        assert_eq!(roots(parse_setup_cfg_roots("", cfg)), ["lib", "python"]);
        assert_eq!(
            roots(parse_setup_cfg_roots(
                "",
                "[options]\npackage_dir = pkg = src/pkg\n"
            )),
            ["src"]
        );
    }

    #[test]
    fn a_manifest_without_roots_is_an_empty_set() {
        assert!(roots(parse_pyproject_roots("", "[project]\nname = \"x\"\n")).is_empty());
        assert!(roots(parse_setup_cfg_roots("", "[metadata]\nname = x\n")).is_empty());
    }
}
