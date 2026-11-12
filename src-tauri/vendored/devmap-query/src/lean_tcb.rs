//! Repository-level Lean 4 Trusted Computing Base (TCB) boundary reporting.
//!
//! Exposes [`devmap_extract::lean::analyze_lean_tcb`] through `devmap-query`
//! alongside [`audit_lean_tcb_tree`], which audits `.lean` files under a
//! repository root while refusing symlink escapes and disclosing any unreadable
//! or budget-capped files via `walk_incomplete` rather than treating unexamined
//! paths as sound proofs.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
pub use devmap_extract::lean::{
    analyze_lean_tcb, analyze_lean_tcb_with_kernel, byte_to_line_1based,
    probe_lean_kernel_toolchain, LeanKernelMode, LeanTcbDeclaration, LeanTcbHole, LeanTcbHoleKind,
    LeanTcbReport, LeanTcbRole, LeanVerificationTier, MAX_LEAN_BYTES,
};
use serde::{Deserialize, Serialize};

/// Default ceiling on `.lean` files scanned in one [`audit_lean_tcb_tree`] call.
pub const DEFAULT_MAX_LEAN_FILES: usize = 2_000;

/// One `.lean` declaration annotated with 1-based line numbers for CLI/MCP reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanTcbFileDeclaration {
    pub qualified_name: String,
    pub keyword: String,
    pub role: LeanTcbRole,
    pub attributes: Vec<String>,
    pub is_unsafe: bool,
    pub is_partial: bool,
    pub start_line: u32,
    pub end_line: u32,
}

/// One `.lean` soundness hole annotated with 1-based line numbers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanTcbFileHole {
    pub kind: LeanTcbHoleKind,
    pub token: String,
    pub enclosing_symbol: Option<String>,
    pub line: u32,
}

/// Per-file Lean 4 TCB boundary report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanTcbFileAudit {
    pub path: String,
    pub sound_tcb: bool,
    pub verification_tier: LeanVerificationTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_note: Option<String>,
    pub imports: Vec<String>,
    pub declarations: Vec<LeanTcbFileDeclaration>,
    pub holes: Vec<LeanTcbFileHole>,
    pub unread_bytes: usize,
}

/// A path inside the walk that could not be audited (symlink, I/O error, non-UTF-8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanTcbSkippedPath {
    pub path: String,
    pub reason: String,
}

/// Complete repository or subtree Lean 4 TCB boundary report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanTcbTreeReport {
    pub root: String,
    /// `true` iff `!walk_incomplete`, `truncated_files == 0`, every audited
    /// `.lean` file satisfied `sound_tcb == true`, and any `--require-kernel`
    /// toolchain check succeeded.
    pub sound: bool,
    /// Explicit verification tier (`"lexical_only"`, `"lean_kernel"`, or `"kernel_and_lean4checker"`).
    pub verification_tier: LeanVerificationTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_note: Option<String>,
    pub walk_incomplete: bool,
    pub truncated_files: usize,
    pub files_audited: usize,
    pub unsound_files: usize,
    pub files: Vec<LeanTcbFileAudit>,
    pub skipped: Vec<LeanTcbSkippedPath>,
}

fn is_ignored_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".lake" | "target" | "node_modules" | ".devcouncil" | ".devmap" | "dist"
    )
}

fn rel_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_bounded_lean_file(path: &Path) -> Result<String, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        return Err("symlink refused".to_string());
    }
    if !meta.file_type().is_file() {
        return Err("not a regular file".to_string());
    }
    // Read up to MAX_LEAN_BYTES + 1 so analyze_lean_tcb sees unread_bytes > 0
    // without allocating unbounded memory on a multi-gigabyte file.
    let cap = (MAX_LEAN_BYTES as u64).saturating_add(4096);
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.take(cap)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    String::from_utf8(buf).map_err(|_| "invalid UTF-8".to_string())
}

fn audit_single_lean(
    repo_root: &Path,
    spec_path: &Path,
    rel: String,
    source: &str,
    mode: LeanKernelMode,
) -> LeanTcbFileAudit {
    let (rep, verification_tier, kernel_note) =
        analyze_lean_tcb_with_kernel(repo_root, Some(spec_path), source, mode);
    let sound_tcb = rep.is_sound_tcb()
        && (mode != LeanKernelMode::Require
            || verification_tier == LeanVerificationTier::KernelAndLean4checker);
    let imports = rep
        .imports
        .into_iter()
        .map(|i| i.module_specifier)
        .collect();
    let declarations = rep
        .declarations
        .into_iter()
        .map(|d| LeanTcbFileDeclaration {
            qualified_name: d.qualified_name,
            keyword: d.keyword,
            role: d.role,
            attributes: d.attributes,
            is_unsafe: d.is_unsafe,
            is_partial: d.is_partial,
            start_line: byte_to_line_1based(source, d.span.start_byte),
            end_line: byte_to_line_1based(
                source,
                d.span.end_byte.saturating_sub(1).max(d.span.start_byte),
            ),
        })
        .collect();
    let holes = rep
        .holes
        .into_iter()
        .map(|h| LeanTcbFileHole {
            kind: h.kind,
            token: h.token,
            enclosing_symbol: h.enclosing_symbol,
            line: byte_to_line_1based(source, h.span.start_byte),
        })
        .collect();
    LeanTcbFileAudit {
        path: rel,
        sound_tcb,
        verification_tier,
        kernel_note,
        imports,
        declarations,
        holes,
        unread_bytes: rep.unread_bytes,
    }
}

fn collect_lean_candidates(
    canonical_root: &Path,
    dir: &Path,
    max_files: usize,
    candidates: &mut Vec<PathBuf>,
    truncated_files: &mut usize,
    skipped: &mut Vec<LeanTcbSkippedPath>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            skipped.push(LeanTcbSkippedPath {
                path: rel_slash(canonical_root, dir),
                reason: e.to_string(),
            });
            return;
        }
    };
    let mut sorted: Vec<PathBuf> = Vec::new();
    for entry in entries {
        match entry {
            Ok(e) => sorted.push(e.path()),
            Err(e) => skipped.push(LeanTcbSkippedPath {
                path: rel_slash(canonical_root, dir),
                reason: e.to_string(),
            }),
        }
    }
    sorted.sort();

    for path in sorted {
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                skipped.push(LeanTcbSkippedPath {
                    path: rel_slash(canonical_root, &path),
                    reason: e.to_string(),
                });
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            if path.extension().is_some_and(|ext| ext == "lean") {
                skipped.push(LeanTcbSkippedPath {
                    path: rel_slash(canonical_root, &path),
                    reason: "symlink refused".to_string(),
                });
            }
            continue;
        }
        if meta.file_type().is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_ignored_dir(name) {
                continue;
            }
            collect_lean_candidates(
                canonical_root,
                &path,
                max_files,
                candidates,
                truncated_files,
                skipped,
            );
        } else if meta.file_type().is_file() && path.extension().is_some_and(|ext| ext == "lean") {
            if path.file_name().and_then(|n| n.to_str()) == Some("lakefile.lean") {
                continue;
            }
            if candidates.len() < max_files {
                candidates.push(path);
            } else {
                *truncated_files += 1;
            }
        }
    }
}

/// Audit `.lean` specifications under `root` (or restricted to `targets` within `root`)
/// using the lexical + metaprogramming TCB scanner.
pub fn audit_lean_tcb_tree(
    root: &Path,
    targets: &[PathBuf],
    max_files: usize,
) -> anyhow::Result<LeanTcbTreeReport> {
    audit_lean_tcb_tree_with_mode(root, targets, max_files, LeanKernelMode::Off)
}

/// Audit `.lean` specifications under `root` with an explicit [`LeanKernelMode`].
pub fn audit_lean_tcb_tree_with_mode(
    root: &Path,
    targets: &[PathBuf],
    max_files: usize,
    mode: LeanKernelMode,
) -> anyhow::Result<LeanTcbTreeReport> {
    if max_files == 0 {
        bail!("max_files must be at least 1");
    }
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("repository root {}", root.display()))?;
    if !canonical_root.is_dir() {
        bail!("{}: not a directory", canonical_root.display());
    }

    let mut candidates = Vec::new();
    let mut truncated_files = 0usize;
    let mut skipped = Vec::new();

    if targets.is_empty() {
        collect_lean_candidates(
            &canonical_root,
            &canonical_root,
            max_files,
            &mut candidates,
            &mut truncated_files,
            &mut skipped,
        );
    } else {
        for raw in targets {
            let joined = if raw.is_absolute() {
                raw.clone()
            } else {
                canonical_root.join(raw)
            };
            let sym_meta = fs::symlink_metadata(&joined)
                .with_context(|| format!("target {}", raw.display()))?;
            if sym_meta.file_type().is_symlink() {
                bail!(
                    "refusing symlink target {}: Lean TCB audit reads only regular in-tree files",
                    raw.display()
                );
            }
            let canon_target = joined
                .canonicalize()
                .with_context(|| format!("target {}", raw.display()))?;
            if !canon_target.starts_with(&canonical_root) {
                bail!(
                    "target {} resolves outside repository root {}",
                    raw.display(),
                    canonical_root.display()
                );
            }
            if canon_target.is_dir() {
                collect_lean_candidates(
                    &canonical_root,
                    &canon_target,
                    max_files,
                    &mut candidates,
                    &mut truncated_files,
                    &mut skipped,
                );
            } else if canon_target.extension().is_some_and(|ext| ext == "lean") {
                if candidates.len() < max_files {
                    candidates.push(canon_target);
                } else {
                    truncated_files += 1;
                }
            } else {
                bail!("target {} is not a .lean file or directory", raw.display());
            }
        }
    }

    candidates.sort();
    candidates.dedup();

    let mut files = Vec::with_capacity(candidates.len());
    let mut unsound_files = 0usize;
    for path in candidates {
        let rel = rel_slash(&canonical_root, &path);
        match read_bounded_lean_file(&path) {
            Ok(src) => {
                let audit = audit_single_lean(&canonical_root, &path, rel, &src, mode);
                if !audit.sound_tcb {
                    unsound_files += 1;
                }
                files.push(audit);
            }
            Err(reason) => {
                skipped.push(LeanTcbSkippedPath { path: rel, reason });
            }
        }
    }

    let (verification_tier, kernel_note, toolchain_ok) = if files.is_empty() {
        match probe_lean_kernel_toolchain(&canonical_root, mode) {
            Ok((tier, note)) => {
                // With 0 audited `.lean` files, only claim a kernel tier if toolchain probe ran for it.
                let effective = if mode == LeanKernelMode::Off {
                    LeanVerificationTier::LexicalOnly
                } else {
                    tier
                };
                (effective, note, true)
            }
            Err(reason) => (LeanVerificationTier::LexicalOnly, Some(reason), false),
        }
    } else {
        let min_tier = files
            .iter()
            .map(|f| f.verification_tier)
            .min()
            .unwrap_or(LeanVerificationTier::LexicalOnly);
        let first_note = files.iter().find_map(|f| f.kernel_note.clone());
        let ok = mode != LeanKernelMode::Require
            || min_tier == LeanVerificationTier::KernelAndLean4checker;
        (min_tier, first_note, ok)
    };

    let walk_incomplete = !skipped.is_empty() || truncated_files > 0;
    let sound = !walk_incomplete && unsound_files == 0 && toolchain_ok;

    Ok(LeanTcbTreeReport {
        root: canonical_root.to_string_lossy().into_owned(),
        sound,
        verification_tier,
        kernel_note,
        walk_incomplete,
        truncated_files,
        files_audited: files.len(),
        unsound_files,
        files,
        skipped,
    })
}
