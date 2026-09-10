//! Bounded discovery across explicitly selected roots, including closed repos.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Discovery {
    pub repos: BTreeSet<PathBuf>,
    pub issues: Vec<String>,
    pub visited: u32,
    pub partial: bool,
}

pub fn roots(values: &[String], exclusions: bool) -> Result<Vec<PathBuf>, String> {
    if values.len() > if exclusions { 128 } else { 16 } {
        return Err("Too many configured paths".into());
    }
    let mut out = BTreeSet::new();
    for value in values {
        let path = Path::new(value);
        if !path.is_absolute() || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err("Roots and exclusions must be absolute directory paths".into());
        }
        let path = crate::engine::git_cli::canonicalize_plain(path)
            .map_err(|e| format!("{value}: {e}"))?;
        super::tree::no_symlinks(Path::new(value))?;
        if !path.is_dir() || path.parent().is_none() {
            return Err("Choose a project directory, not a filesystem root".into());
        }
        out.insert(path);
    }
    let all: Vec<_> = out.into_iter().collect();
    Ok(all
        .iter()
        .filter(|p| !all.iter().any(|q| p != &q && p.starts_with(q)))
        .cloned()
        .collect())
}

/// Windows `validate_repo` returns a verbatim `\\?\` path. The walk already
/// used `canonicalize_plain`, so `==` treats a real repository as a boundary
/// mismatch. Compare after stripping that prefix; do not change `validate_repo`
/// itself — the sandbox containment check depends on the verbatim spelling.
pub(crate) fn same_discovered_repo(validated: &Path, walked: &Path) -> bool {
    crate::engine::git_cli::canonicalize_plain(validated)
        .ok()
        .as_deref()
        == Some(walked)
}

pub fn discover(roots: &[PathBuf], excludes: &[PathBuf], cancel: &AtomicBool) -> Discovery {
    let mut report = Discovery::default();
    let mut stack: Vec<_> = roots.iter().rev().map(|p| (p.clone(), 0usize)).collect();
    let mut seen = BTreeSet::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while let Some((path, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed)
            || Instant::now() >= deadline
            || report.visited >= 40_000
            || report.repos.len() >= 128
        {
            report.partial = true;
            report.issues.push("Discovery stopped at its budget or was cancelled; no complete inventory is claimed".into());
            break;
        }
        if excludes.iter().any(|p| path.starts_with(p)) || !seen.insert(path.clone()) {
            continue;
        }
        report.visited += 1;
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                report.partial = true;
                if report.issues.len() < 128 {
                    report.issues.push(format!("{}: {e}", path.display()));
                }
                continue;
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        if path.join(".git").try_exists().unwrap_or(true) {
            match path
                .to_str()
                .ok_or_else(|| "Non-UTF-8 repository".to_string())
                .and_then(crate::engine::git_cli::validate_repo)
            {
                Ok(root) if same_discovered_repo(&root, &path) => {
                    report.repos.insert(path.clone());
                }
                Ok(_) => {
                    report.partial = true;
                    report
                        .issues
                        .push(format!("Repository boundary mismatch: {}", path.display()));
                }
                Err(e) => {
                    report.partial = true;
                    if report.issues.len() < 128 {
                        report.issues.push(format!("{}: {e}", path.display()));
                    }
                }
            }
        }
        if depth >= 24 {
            report.partial = true;
            if report.issues.len() < 128 {
                report
                    .issues
                    .push(format!("Depth limit at {}", path.display()));
            }
            continue;
        }
        let children = match std::fs::read_dir(&path) {
            Ok(r) => r,
            Err(e) => {
                report.partial = true;
                if report.issues.len() < 128 {
                    report.issues.push(format!("{}: {e}", path.display()));
                }
                continue;
            }
        };
        // Limit queued entries as well as visited ones: a huge directory may
        // otherwise allocate an unbounded stack before the next budget check.
        for child in children {
            if stack.len() + report.visited as usize >= 40_000
                || Instant::now() >= deadline
                || cancel.load(Ordering::SeqCst)
            {
                report.partial = true;
                break;
            }
            report.visited += 1;
            match child {
                Ok(child) => {
                    let name = child.file_name().to_string_lossy().to_string();
                    if name.starts_with('.')
                        || super::providers::protected_artifact(&name)
                        || matches!(
                            name.as_str(),
                            "target" | "build" | "dist" | "out" | "obj" | "vendor"
                        )
                        || name.starts_with("target-")
                    {
                        continue;
                    }
                    match child.file_type() {
                        Ok(t) if t.is_dir() && !t.is_symlink() => {
                            stack.push((child.path(), depth + 1))
                        }
                        Ok(_) => {}
                        Err(e) => {
                            report.partial = true;
                            if report.issues.len() < 128 {
                                report.issues.push(e.to_string());
                            }
                        }
                    }
                }
                Err(e) => {
                    report.partial = true;
                    if report.issues.len() < 128 {
                        report.issues.push(e.to_string());
                    }
                }
            }
        }
    }
    report
}
