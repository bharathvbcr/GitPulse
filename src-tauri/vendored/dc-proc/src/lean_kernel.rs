//! Isolated, process-group-bounded Lean 4 kernel and `lean4checker --fresh` runner.
//!
//! Shared by `devmap` (`devmap lean-tcb --kernel|--require-kernel`) and `dcverify`
//! (`DC_VERIFY_LEAN_KERNEL=auto|require`) so both surfaces resolve binaries outside
//! the untrusted repository (`resolve_outside`), stage specifications inside a private
//! `0700` scratch directory, enforce wall-clock deadlines with process-group teardown
//! (`run_bounded`), and replay compiled `.olean` artifacts through `lean4checker --fresh`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Bounds, Failure, Resolved, resolve_outside, run_bounded};

/// Default wall-clock budget for each `lean` / `lean4checker` invocation.
///
/// `lean4checker --fresh` replays the full transitive `Init` prelude (~150k+
/// kernel declarations) from scratch on a single core (~35–75s on 2-vCPU CI
/// runners), so 180s provides deterministic headroom while still reaping hung
/// elaborators or infinite tactics via process-group teardown.
pub const DEFAULT_LEAN_KERNEL_TIMEOUT: Duration = Duration::from_secs(180);

const STDOUT_CAP: usize = 1 << 20;
const STDERR_CAP: usize = 256 << 10;
const TARGET_MODULE: &str = "DcVerifyTargetSpec";
const DRIVER_MODULE: &str = "DcVerifyDriver";
const MAX_STAGED_LOCAL_MODULES: usize = 64;
const MAX_STAGED_MODULE_BYTES: u64 = 4 * 1024 * 1024;

const REAL_LEAN_ENV_AUDIT_SNIPPET: &str = r#"
open Lean Elab Command in
#eval show CommandElabM Unit from do
  let env ← getEnv
  let mods := env.header.moduleNames
  let coreRoots : List Name := [Name.mkSimple "Init", Name.mkSimple "Std", Name.mkSimple "Lean", Name.mkSimple "Lake"]
  let allowedAx : List Name := [Name.mkSimple "propext", Name.mkStr2 "Classical" "choice", Name.mkStr2 "Quot" "sound"]
  for (cname, cinfo) in env.constants.map₁.toList do
    if let some modIdx := env.getModuleIdxFor? cname then
      if h : modIdx.toNat < mods.size then
        let mname := mods[modIdx.toNat]
        if !(coreRoots.contains mname.getRoot) then
          let hasRange := (← Lean.findDeclarationRanges? cname).isSome
          let userName := (Lean.privateToUserName? cname).getD cname
          let isCompilerStage := match cname with
            | .str _ "_cstage1" | .str _ "_cstage2" => true
            | _ => false
          match cinfo with
          | .axiomInfo v =>
            if (!v.isUnsafe || (hasRange && !userName.hasMacroScopes)) && !(allowedAx.contains cname) then
              IO.println s!"'{cname}' depends on axioms: [{cname}]"
          | .opaqueInfo _ =>
            if !userName.hasMacroScopes then
              throwError s!"TCB opaque constant {cname} in module {mname}"
          | _ =>
            if !isCompilerStage then
              if cinfo.isUnsafe && hasRange && !userName.hasMacroScopes then
                throwError s!"TCB unsafe constant {cname} in module {mname}"
              if Lean.isExtern env cname then
                throwError s!"TCB @[extern] constant {cname} in module {mname}"
          if !cinfo.isUnsafe then
            let (_, st) := (CollectAxioms.collect cname).run env {}
            for ax in st.axioms do
              if !(allowedAx.contains ax) then
                IO.println s!"'{cname}' depends on axioms: [{ax}]"
"#;

/// Verification depth reached by the external Lean 4 toolchain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeanKernelTier {
    LexicalOnly,
    LeanKernel,
    KernelAndLean4checker,
}

/// Reason a Lean 4 kernel or `lean4checker --fresh` replay check failed closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeanKernelFailure {
    Unavailable(String),
    KernelRejected(String),
    Lean4CheckerRejected(String),
}

/// Outcome of running `lean` + `#print axioms` + `lean4checker --fresh` on one specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeanKernelOutcome {
    pub tier: LeanKernelTier,
    pub print_axioms_stdout: Option<String>,
    pub note: Option<String>,
    pub failure: Option<LeanKernelFailure>,
}

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn create() -> io::Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("dc-lean-tcb-{}-{stamp}-{seq}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self { path })
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Resolve the per-step timeout, honoring `DC_LEAN_KERNEL_TIMEOUT_MS` within `[50ms, 600s]`.
pub fn configured_lean_kernel_timeout() -> Duration {
    if let Ok(raw) = std::env::var("DC_LEAN_KERNEL_TIMEOUT_MS")
        && let Ok(ms) = raw.trim().parse::<u64>()
    {
        return Duration::from_millis(ms.clamp(50, 600_000));
    }
    DEFAULT_LEAN_KERNEL_TIMEOUT
}

fn classify_lookup(program: &str, repo_root: &Path) -> Result<Option<Resolved>, LeanKernelFailure> {
    match resolve_outside(
        OsStr::new(program),
        std::env::var_os("PATH").as_deref(),
        repo_root,
    ) {
        Ok(resolved) => Ok(Some(resolved)),
        Err(Failure::Spawn { error, .. }) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(other) => Err(LeanKernelFailure::Unavailable(other.to_string())),
    }
}

/// Probe `lean` and `lean4checker` outside `repo_root` without compiling a file.
pub fn probe_lean_toolchain(
    repo_root: &Path,
    require_kernel: bool,
) -> Result<(LeanKernelTier, Option<String>), LeanKernelFailure> {
    let Some(_lean) = classify_lookup("lean", repo_root)? else {
        if require_kernel {
            return Err(LeanKernelFailure::Unavailable(
                "lean kernel binary is not installed on PATH (--require-kernel / DC_VERIFY_LEAN_KERNEL=require)".to_string(),
            ));
        }
        return Ok((
            LeanKernelTier::LexicalOnly,
            Some("lean is not on PATH; stayed at lexical_only tier".to_string()),
        ));
    };
    let Some(_checker) = classify_lookup("lean4checker", repo_root)? else {
        if require_kernel {
            return Err(LeanKernelFailure::Unavailable(
                "lean4checker binary is not installed on PATH (--require-kernel / DC_VERIFY_LEAN_KERNEL=require)".to_string(),
            ));
        }
        return Ok((
            LeanKernelTier::LeanKernel,
            Some("lean4checker is not on PATH; verified with lean kernel only".to_string()),
        ));
    };
    Ok((LeanKernelTier::KernelAndLean4checker, None))
}

fn write_private_file(path: &Path, contents: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)
    }
}

fn collect_lake_lib_dirs(repo_root: &Path, spec_path: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut push_lake = |base: &Path| {
        for candidate in [
            base.join(".lake").join("build").join("lib").join("lean"),
            base.join(".lake").join("build").join("lib"),
        ] {
            if fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_dir())
                && !dirs.contains(&candidate)
            {
                dirs.push(candidate);
            }
        }
        let pkgs = base.join(".lake").join("packages");
        if let Ok(entries) = fs::read_dir(&pkgs) {
            for entry in entries.flatten() {
                for pkg_lib in [
                    entry
                        .path()
                        .join(".lake")
                        .join("build")
                        .join("lib")
                        .join("lean"),
                    entry.path().join(".lake").join("build").join("lib"),
                ] {
                    if fs::symlink_metadata(&pkg_lib).is_ok_and(|m| m.file_type().is_dir())
                        && !dirs.contains(&pkg_lib)
                    {
                        dirs.push(pkg_lib);
                    }
                }
            }
        }
    };
    if let Some(spec) = spec_path {
        let mut cur = spec.parent();
        while let Some(dir) = cur {
            push_lake(dir);
            if dir == repo_root {
                break;
            }
            cur = dir.parent();
        }
    }
    push_lake(repo_root);
    dirs
}

fn query_lean_libdir(lean: &Resolved, timeout: Duration) -> Option<PathBuf> {
    let mut cmd = lean.command();
    cmd.arg("--print-libdir");
    let captured = run_bounded(
        &mut cmd,
        Bounds {
            deadline: timeout.min(Duration::from_secs(10)),
            stdout_cap: 16 << 10,
            stderr_cap: 16 << 10,
        },
    )
    .ok()?;
    if !captured.status.success() || captured.stdout_truncated {
        return None;
    }
    let text = captured.stdout_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn build_lean_path(
    scratch: &Path,
    lake_dirs: &[PathBuf],
    libdir: Option<&Path>,
) -> Result<OsString, LeanKernelFailure> {
    let mut paths: Vec<PathBuf> = Vec::new();
    paths.push(scratch.to_path_buf());
    for d in lake_dirs {
        if !paths.contains(d) {
            paths.push(d.clone());
        }
    }
    if let Some(existing) = std::env::var_os("LEAN_PATH") {
        for p in std::env::split_paths(&existing) {
            if !p.as_os_str().is_empty() && !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    if let Some(sys_lib) = libdir {
        let buf = sys_lib.to_path_buf();
        if !paths.contains(&buf) {
            paths.push(buf);
        }
    }
    std::env::join_paths(paths)
        .map_err(|e| LeanKernelFailure::KernelRejected(format!("constructing LEAN_PATH: {e}")))
}

fn summarize_diag(stdout: &str, stderr: &str) -> String {
    let mut combined = String::new();
    let se = stderr.trim();
    let so = stdout.trim();
    if !se.is_empty() {
        combined.push_str(se);
    }
    if !so.is_empty() {
        if !combined.is_empty() {
            combined.push_str(" | ");
        }
        combined.push_str(so);
    }
    let single = combined.replace(['\r', '\n'], " ");
    if single.len() <= 360 {
        single
    } else {
        let mut cut = 360;
        while cut > 0 && !single.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &single[..cut])
    }
}

/// Target module stem (`DcVerifyTargetSpec`) staged inside the scratch directory.
pub const fn target_module_name() -> &'static str {
    TARGET_MODULE
}

fn strip_lean_guillemets(seg: &str) -> &str {
    seg.strip_prefix('«')
        .and_then(|s| s.strip_suffix('»'))
        .unwrap_or(seg)
}

fn extract_lean_header_imports(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut imports = Vec::new();

    let skip_ws_and_comments = |mut pos: usize| -> (usize, bool) {
        let mut saw_newline = false;
        while pos < len {
            let b = bytes[pos];
            if b == b'\n' {
                saw_newline = true;
                pos += 1;
            } else if b.is_ascii_whitespace() {
                pos += 1;
            } else if b == b'-' && pos + 1 < len && bytes[pos + 1] == b'-' {
                pos += 2;
                while pos < len && bytes[pos] != b'\n' {
                    pos += 1;
                }
            } else if b == b'/' && pos + 1 < len && bytes[pos + 1] == b'-' {
                let mut depth = 1usize;
                pos += 2;
                while pos < len && depth > 0 {
                    if bytes[pos] == b'/' && pos + 1 < len && bytes[pos + 1] == b'-' {
                        depth += 1;
                        pos += 2;
                    } else if bytes[pos] == b'-' && pos + 1 < len && bytes[pos + 1] == b'/' {
                        depth -= 1;
                        pos += 2;
                    } else {
                        if bytes[pos] == b'\n' {
                            saw_newline = true;
                        }
                        let ch = source[pos..].chars().next().unwrap();
                        pos += ch.len_utf8();
                    }
                }
            } else {
                break;
            }
        }
        (pos, saw_newline)
    };

    let scan_ident = |mut pos: usize| -> Option<(String, usize)> {
        if pos >= len {
            return None;
        }
        let first = source[pos..].chars().next()?;
        if !(first == '«' || first.is_alphabetic() || first == '_') {
            return None;
        }
        let start = pos;
        loop {
            if source[pos..].starts_with('«') {
                pos += '«'.len_utf8();
                while pos < len && !source[pos..].starts_with('»') {
                    let ch = source[pos..].chars().next().unwrap();
                    pos += ch.len_utf8();
                }
                if pos < len && source[pos..].starts_with('»') {
                    pos += '»'.len_utf8();
                }
            } else {
                while pos < len {
                    let c = source[pos..].chars().next().unwrap();
                    if c.is_alphanumeric() || c == '_' || c == '\'' || c == '?' || c == '!' {
                        pos += c.len_utf8();
                    } else {
                        break;
                    }
                }
            }
            if pos + 1 < len && bytes[pos] == b'.' {
                let next = source[pos + 1..].chars().next().unwrap();
                if next == '«' || next.is_alphabetic() || next == '_' {
                    pos += 1;
                    continue;
                }
            }
            break;
        }
        Some((source[start..pos].to_string(), pos))
    };

    (i, _) = skip_ws_and_comments(i);
    if let Some((word, next)) = scan_ident(i)
        && word == "prelude"
    {
        i = next;
    }

    loop {
        let (pos, _) = skip_ws_and_comments(i);
        i = pos;
        let Some((kw, after_kw)) = scan_ident(i) else {
            break;
        };
        if kw != "import" {
            break;
        }
        i = after_kw;
        let (mut cur, mut nl) = skip_ws_and_comments(i);
        if !nl
            && let Some((maybe_all, after_all)) = scan_ident(cur)
            && maybe_all == "all"
        {
            cur = after_all;
            let (next_cur, next_nl) = skip_ws_and_comments(cur);
            cur = next_cur;
            nl = next_nl;
        }
        while !nl {
            let Some((mod_ident, after_mod)) = scan_ident(cur) else {
                break;
            };
            if mod_ident == "import" {
                break;
            }
            let cleaned = mod_ident
                .split('.')
                .map(strip_lean_guillemets)
                .collect::<Vec<_>>()
                .join(".");
            if !cleaned.is_empty() && !imports.contains(&cleaned) {
                imports.push(cleaned);
            }
            i = after_mod;
            let (next_cur, next_nl) = skip_ws_and_comments(i);
            cur = next_cur;
            nl = next_nl;
        }
        i = cur;
    }

    imports
}

fn module_rel_paths(module_name: &str) -> Result<(PathBuf, PathBuf), LeanKernelFailure> {
    let mut base = PathBuf::new();
    let segs: Vec<&str> = module_name.split('.').collect();
    if segs.is_empty() {
        return Err(LeanKernelFailure::KernelRejected(
            "empty Lean import module name".to_string(),
        ));
    }
    for seg in &segs {
        if seg.is_empty()
            || *seg == "."
            || *seg == ".."
            || seg.contains(['/', '\\', '\0'])
        {
            return Err(LeanKernelFailure::KernelRejected(format!(
                "refusing unsafe Lean import module path `{module_name}`"
            )));
        }
        base.push(seg);
    }
    let mut lean_rel = base.clone();
    lean_rel.set_extension("lean");
    let mut olean_rel = base;
    olean_rel.set_extension("olean");
    Ok((lean_rel, olean_rel))
}

fn resolve_local_import_source(
    canonical_root: &Path,
    spec_path: Option<&Path>,
    lean_rel: &Path,
) -> Result<Option<PathBuf>, LeanKernelFailure> {
    let mut search_bases: Vec<PathBuf> = Vec::new();
    if let Some(spec) = spec_path {
        let joined = if spec.is_absolute() {
            spec.to_path_buf()
        } else {
            canonical_root.join(spec)
        };
        let mut cur = joined.parent();
        while let Some(dir) = cur {
            if let Ok(canon_dir) = dir.canonicalize()
                && canon_dir.starts_with(canonical_root)
                && !search_bases.contains(&canon_dir)
            {
                search_bases.push(canon_dir.clone());
                if canon_dir == canonical_root {
                    break;
                }
            }
            cur = dir.parent();
        }
    }
    if !search_bases.contains(&canonical_root.to_path_buf()) {
        search_bases.push(canonical_root.to_path_buf());
    }

    for base in search_bases {
        let mut cur = base.clone();
        let mut missing = false;
        for comp in lean_rel.components() {
            cur.push(comp);
            match fs::symlink_metadata(&cur) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(LeanKernelFailure::KernelRejected(format!(
                            "refusing symlinked Lean import dependency {}",
                            cur.display()
                        )));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    missing = true;
                    break;
                }
                Err(e) => {
                    return Err(LeanKernelFailure::KernelRejected(format!(
                        "stat Lean import dependency {}: {e}",
                        cur.display()
                    )));
                }
            }
        }
        if missing {
            continue;
        }
        let meta = fs::symlink_metadata(&cur).map_err(|e| {
            LeanKernelFailure::KernelRejected(format!(
                "stat Lean import dependency {}: {e}",
                cur.display()
            ))
        })?;
        if !meta.file_type().is_file() {
            continue;
        }
        if meta.len() > MAX_STAGED_MODULE_BYTES {
            return Err(LeanKernelFailure::KernelRejected(format!(
                "Lean import dependency {} exceeds {} byte cap",
                cur.display(),
                MAX_STAGED_MODULE_BYTES
            )));
        }
        let canon = cur.canonicalize().map_err(|e| {
            LeanKernelFailure::KernelRejected(format!(
                "canonicalize Lean import dependency {}: {e}",
                cur.display()
            ))
        })?;
        if !canon.starts_with(canonical_root) {
            return Err(LeanKernelFailure::KernelRejected(format!(
                "refusing Lean import dependency {} outside repository root",
                canon.display()
            )));
        }
        return Ok(Some(canon));
    }

    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn collect_local_imports_dfs(
    canonical_root: &Path,
    libdir: Option<&Path>,
    parent_spec: Option<&Path>,
    module_name: &str,
    visiting: &mut Vec<String>,
    visited: &mut Vec<String>,
    ordered: &mut Vec<(String, PathBuf, PathBuf, String)>,
) -> Result<(), LeanKernelFailure> {
    if visited.iter().any(|m| m == module_name) {
        return Ok(());
    }
    if visiting.iter().any(|m| m == module_name) {
        return Err(LeanKernelFailure::KernelRejected(format!(
            "cyclic Lean import dependency involving `{module_name}`"
        )));
    }
    let (lean_rel, olean_rel) = module_rel_paths(module_name)?;
    if libdir.is_some_and(|sys| sys.join(&olean_rel).is_file()) {
        visited.push(module_name.to_string());
        return Ok(());
    }
    let Some(canon_src_path) = resolve_local_import_source(canonical_root, parent_spec, &lean_rel)?
    else {
        visited.push(module_name.to_string());
        return Ok(());
    };
    if ordered.len() >= MAX_STAGED_LOCAL_MODULES {
        return Err(LeanKernelFailure::KernelRejected(format!(
            "Lean specification exceeds {MAX_STAGED_LOCAL_MODULES} transitive local module imports"
        )));
    }

    let dep_source = fs::read_to_string(&canon_src_path).map_err(|e| {
        LeanKernelFailure::KernelRejected(format!(
            "reading Lean import dependency {}: {e}",
            canon_src_path.display()
        ))
    })?;

    visiting.push(module_name.to_string());
    for sub_imp in extract_lean_header_imports(&dep_source) {
        collect_local_imports_dfs(
            canonical_root,
            libdir,
            Some(&canon_src_path),
            &sub_imp,
            visiting,
            visited,
            ordered,
        )?;
    }
    visiting.pop();
    visited.push(module_name.to_string());
    ordered.push((module_name.to_string(), lean_rel, olean_rel, dep_source));
    Ok(())
}

/// Compile `source` with `lean`, execute `driver_source` (`#print axioms`), and replay
/// the resulting `.olean` through `lean4checker --fresh` when installed.
pub fn run_lean_kernel_check(
    repo_root: &Path,
    spec_path: Option<&Path>,
    source: &str,
    driver_source: &str,
    require_kernel: bool,
    timeout: Duration,
) -> LeanKernelOutcome {
    let lean = match classify_lookup("lean", repo_root) {
        Ok(Some(res)) => res,
        Ok(None) => {
            if require_kernel {
                return LeanKernelOutcome {
                    tier: LeanKernelTier::LexicalOnly,
                    print_axioms_stdout: None,
                    note: None,
                    failure: Some(LeanKernelFailure::Unavailable(
                        "lean kernel binary is not installed on PATH (--require-kernel / DC_VERIFY_LEAN_KERNEL=require)".to_string(),
                    )),
                };
            }
            return LeanKernelOutcome {
                tier: LeanKernelTier::LexicalOnly,
                print_axioms_stdout: None,
                note: Some("lean is not on PATH; stayed at lexical_only tier".to_string()),
                failure: None,
            };
        }
        Err(fail) => {
            return LeanKernelOutcome {
                tier: LeanKernelTier::LexicalOnly,
                print_axioms_stdout: None,
                note: None,
                failure: Some(fail),
            };
        }
    };

    let checker = match classify_lookup("lean4checker", repo_root) {
        Ok(opt) => opt,
        Err(fail) => {
            return LeanKernelOutcome {
                tier: LeanKernelTier::LexicalOnly,
                print_axioms_stdout: None,
                note: None,
                failure: Some(fail),
            };
        }
    };

    if require_kernel && checker.is_none() {
        return LeanKernelOutcome {
            tier: LeanKernelTier::LexicalOnly,
            print_axioms_stdout: None,
            note: None,
            failure: Some(LeanKernelFailure::Unavailable(
                "lean4checker binary is not installed on PATH (--require-kernel / DC_VERIFY_LEAN_KERNEL=require)".to_string(),
            )),
        };
    }

    let scratch = match ScratchDir::create() {
        Ok(dir) => dir,
        Err(e) => {
            return LeanKernelOutcome {
                tier: LeanKernelTier::LexicalOnly,
                print_axioms_stdout: None,
                note: None,
                failure: Some(LeanKernelFailure::KernelRejected(format!(
                    "creating isolated Lean scratch directory: {e}"
                ))),
            };
        }
    };

    let libdir = query_lean_libdir(&lean, timeout);
    let has_real_lean_lib = libdir
        .as_deref()
        .is_some_and(|d| d.join("Lean.olean").is_file());
    let effective_driver = if has_real_lean_lib {
        format!("import Lean\n{driver_source}\n{REAL_LEAN_ENV_AUDIT_SNIPPET}")
    } else {
        driver_source.to_string()
    };

    let target_lean = scratch.path.join(format!("{TARGET_MODULE}.lean"));
    let target_olean = scratch.path.join(format!("{TARGET_MODULE}.olean"));
    let driver_lean = scratch.path.join(format!("{DRIVER_MODULE}.lean"));

    if let Err(e) = write_private_file(&target_lean, source)
        .and_then(|()| write_private_file(&driver_lean, &effective_driver))
    {
        return LeanKernelOutcome {
            tier: LeanKernelTier::LexicalOnly,
            print_axioms_stdout: None,
            note: None,
            failure: Some(LeanKernelFailure::KernelRejected(format!(
                "staging Lean verification files: {e}"
            ))),
        };
    }

    let lake_dirs = collect_lake_lib_dirs(repo_root, spec_path);
    let lean_path = match build_lean_path(&scratch.path, &lake_dirs, libdir.as_deref()) {
        Ok(lp) => lp,
        Err(fail) => {
            return LeanKernelOutcome {
                tier: LeanKernelTier::LexicalOnly,
                print_axioms_stdout: None,
                note: None,
                failure: Some(fail),
            };
        }
    };

    let bounds = Bounds {
        deadline: timeout,
        stdout_cap: STDOUT_CAP,
        stderr_cap: STDERR_CAP,
    };

    // Stage and compile any unbuilt in-tree `.lean` dependencies in topological order.
    if let Ok(canonical_root) = repo_root.canonicalize() {
        let mut visiting = Vec::new();
        let mut visited = Vec::new();
        let mut ordered = Vec::new();
        for imp in extract_lean_header_imports(source) {
            if let Err(fail) = collect_local_imports_dfs(
                &canonical_root,
                libdir.as_deref(),
                spec_path,
                &imp,
                &mut visiting,
                &mut visited,
                &mut ordered,
            ) {
                return LeanKernelOutcome {
                    tier: LeanKernelTier::LexicalOnly,
                    print_axioms_stdout: None,
                    note: None,
                    failure: Some(fail),
                };
            }
        }
        for (mod_name, lean_rel, olean_rel, dep_src) in ordered {
            let staged_lean = scratch.path.join(&lean_rel);
            let staged_olean = scratch.path.join(&olean_rel);
            if let Some(parent) = staged_lean.parent()
                && let Err(e) = fs::create_dir_all(parent)
            {
                return LeanKernelOutcome {
                    tier: LeanKernelTier::LexicalOnly,
                    print_axioms_stdout: None,
                    note: None,
                    failure: Some(LeanKernelFailure::KernelRejected(format!(
                        "creating scratch module dir for `{mod_name}`: {e}"
                    ))),
                };
            }
            if let Err(e) = write_private_file(&staged_lean, &dep_src) {
                return LeanKernelOutcome {
                    tier: LeanKernelTier::LexicalOnly,
                    print_axioms_stdout: None,
                    note: None,
                    failure: Some(LeanKernelFailure::KernelRejected(format!(
                        "staging local Lean import `{mod_name}`: {e}"
                    ))),
                };
            }
            let mut dep_cmd = lean.command();
            dep_cmd
                .current_dir(&scratch.path)
                .env("LEAN_PATH", &lean_path)
                .arg("-o")
                .arg(&staged_olean)
                .arg(&staged_lean);
            let dep_cap = match run_bounded(&mut dep_cmd, bounds) {
                Ok(cap) => cap,
                Err(err) => {
                    return LeanKernelOutcome {
                        tier: LeanKernelTier::LexicalOnly,
                        print_axioms_stdout: None,
                        note: None,
                        failure: Some(LeanKernelFailure::KernelRejected(format!(
                            "compiling local Lean import `{mod_name}` failed: {err}"
                        ))),
                    };
                }
            };
            let dep_out = dep_cap.stdout_lossy();
            let dep_err = dep_cap.stderr_trimmed();
            if !dep_cap.status.success() || dep_cap.stdout_truncated {
                return LeanKernelOutcome {
                    tier: LeanKernelTier::LexicalOnly,
                    print_axioms_stdout: None,
                    note: None,
                    failure: Some(LeanKernelFailure::KernelRejected(format!(
                        "lean kernel rejected imported module `{mod_name}` (exit {:?}): {}",
                        dep_cap.status.code(),
                        summarize_diag(&dep_out, &dep_err)
                    ))),
                };
            }
            if !fs::symlink_metadata(&staged_olean).is_ok_and(|m| m.file_type().is_file()) {
                return LeanKernelOutcome {
                    tier: LeanKernelTier::LexicalOnly,
                    print_axioms_stdout: None,
                    note: None,
                    failure: Some(LeanKernelFailure::KernelRejected(format!(
                        "lean exited 0 without emitting .olean for imported module `{mod_name}`"
                    ))),
                };
            }
            if dep_out.contains("declaration uses 'sorry'")
                || dep_err.contains("declaration uses 'sorry'")
            {
                return LeanKernelOutcome {
                    tier: LeanKernelTier::LeanKernel,
                    print_axioms_stdout: None,
                    note: None,
                    failure: Some(LeanKernelFailure::KernelRejected(format!(
                        "imported module `{mod_name}` reported `declaration uses 'sorry'`"
                    ))),
                };
            }
        }
    }

    // Step 1: Compile target `.lean` -> `.olean` under Lean's kernel.
    let mut compile_cmd = lean.command();
    compile_cmd
        .current_dir(&scratch.path)
        .env("LEAN_PATH", &lean_path)
        .arg("-o")
        .arg(&target_olean)
        .arg(&target_lean);
    let compiled = match run_bounded(&mut compile_cmd, bounds) {
        Ok(cap) => cap,
        Err(err) => {
            return LeanKernelOutcome {
                tier: LeanKernelTier::LexicalOnly,
                print_axioms_stdout: None,
                note: None,
                failure: Some(LeanKernelFailure::KernelRejected(format!(
                    "lean kernel check failed: {err}"
                ))),
            };
        }
    };
    let compile_stdout = compiled.stdout_lossy();
    let compile_stderr = compiled.stderr_trimmed();
    if !compiled.status.success() || compiled.stdout_truncated {
        return LeanKernelOutcome {
            tier: LeanKernelTier::LexicalOnly,
            print_axioms_stdout: None,
            note: None,
            failure: Some(LeanKernelFailure::KernelRejected(format!(
                "lean kernel rejected specification (exit {:?}): {}",
                compiled.status.code(),
                summarize_diag(&compile_stdout, &compile_stderr)
            ))),
        };
    }
    if !fs::symlink_metadata(&target_olean).is_ok_and(|m| m.file_type().is_file()) {
        return LeanKernelOutcome {
            tier: LeanKernelTier::LexicalOnly,
            print_axioms_stdout: None,
            note: None,
            failure: Some(LeanKernelFailure::KernelRejected(
                "lean exited 0 without emitting compiled .olean artifact".to_string(),
            )),
        };
    }
    if compile_stdout.contains("declaration uses 'sorry'")
        || compile_stderr.contains("declaration uses 'sorry'")
    {
        return LeanKernelOutcome {
            tier: LeanKernelTier::LeanKernel,
            print_axioms_stdout: None,
            note: None,
            failure: Some(LeanKernelFailure::KernelRejected(
                "Lean compiler reported `declaration uses 'sorry'`".to_string(),
            )),
        };
    }

    // Step 2: Run verifier-generated `#print axioms` driver over compiled target module.
    let mut driver_cmd = lean.command();
    driver_cmd
        .current_dir(&scratch.path)
        .env("LEAN_PATH", &lean_path)
        .arg(&driver_lean);
    let driven = match run_bounded(&mut driver_cmd, bounds) {
        Ok(cap) => cap,
        Err(err) => {
            return LeanKernelOutcome {
                tier: LeanKernelTier::LeanKernel,
                print_axioms_stdout: None,
                note: None,
                failure: Some(LeanKernelFailure::KernelRejected(format!(
                    "lean `#print axioms` driver failed: {err}"
                ))),
            };
        }
    };
    let driver_stdout = driven.stdout_lossy();
    let driver_stderr = driven.stderr_trimmed();
    if !driven.status.success() || driven.stdout_truncated {
        return LeanKernelOutcome {
            tier: LeanKernelTier::LeanKernel,
            print_axioms_stdout: None,
            note: None,
            failure: Some(LeanKernelFailure::KernelRejected(format!(
                "lean `#print axioms` driver exited {:?}: {}",
                driven.status.code(),
                summarize_diag(&driver_stdout, &driver_stderr)
            ))),
        };
    }

    // Step 3: Replay compiled `.olean` environment from scratch with `lean4checker --fresh` when present.
    if let Some(chk) = checker {
        let mut chk_cmd = chk.command();
        chk_cmd
            .current_dir(&scratch.path)
            .env("LEAN_PATH", &lean_path)
            .args(["--fresh", TARGET_MODULE]);
        let checked = match run_bounded(&mut chk_cmd, bounds) {
            Ok(cap) => cap,
            Err(err) => {
                return LeanKernelOutcome {
                    tier: LeanKernelTier::LeanKernel,
                    print_axioms_stdout: Some(driver_stdout),
                    note: None,
                    failure: Some(LeanKernelFailure::Lean4CheckerRejected(format!(
                        "lean4checker --fresh failed: {err}"
                    ))),
                };
            }
        };
        let chk_stdout = checked.stdout_lossy();
        let chk_stderr = checked.stderr_trimmed();
        if !checked.status.success() || checked.stdout_truncated {
            return LeanKernelOutcome {
                tier: LeanKernelTier::LeanKernel,
                print_axioms_stdout: Some(driver_stdout),
                note: None,
                failure: Some(LeanKernelFailure::Lean4CheckerRejected(format!(
                    "lean4checker --fresh rejected environment (exit {:?}): {}",
                    checked.status.code(),
                    summarize_diag(&chk_stdout, &chk_stderr)
                ))),
            };
        }
        LeanKernelOutcome {
            tier: LeanKernelTier::KernelAndLean4checker,
            print_axioms_stdout: Some(driver_stdout),
            note: None,
            failure: None,
        }
    } else {
        LeanKernelOutcome {
            tier: LeanKernelTier::LeanKernel,
            print_axioms_stdout: Some(driver_stdout),
            note: Some("lean4checker is not on PATH; verified with lean kernel only".to_string()),
            failure: None,
        }
    }
}
