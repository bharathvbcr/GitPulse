//! Prebuilt release install rung: download, verify checksum, install.
//!
//! A checksum mismatch is a **refusal** — never a fallback to running the
//! binary anyway. Until sibling repos publish release assets, this rung
//! reports itself unavailable with a platform-specific reason.

use super::{ExternalTool, PUBLIC_DEVMAP_REPO, PUBLIC_MANVI_REPO};
use crate::engine::git_cli;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(5 * 60);
const DOWNLOAD_CAP: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseAvailability {
    Available {
        asset_url: String,
        checksum_url: String,
        asset_name: String,
    },
    Unavailable {
        reason: String,
    },
}

/// Target triple fragment used in release asset names.
pub fn platform_target() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        "unsupported"
    }
}

fn repo_for(tool: ExternalTool) -> &'static str {
    match tool {
        ExternalTool::Devmap => PUBLIC_DEVMAP_REPO,
        ExternalTool::Manvi => PUBLIC_MANVI_REPO,
    }
}

fn asset_stem(tool: ExternalTool) -> &'static str {
    tool.as_str()
}

pub(crate) fn binary_name(tool: ExternalTool) -> &'static str {
    if cfg!(windows) {
        match tool {
            ExternalTool::Devmap => "devmap.exe",
            ExternalTool::Manvi => "manvi.exe",
        }
    } else {
        tool.as_str()
    }
}

/// Manvi has no Windows release target (CGO / `O_NOFOLLOW`).
fn platform_supported(tool: ExternalTool) -> Result<(), String> {
    let target = platform_target();
    if target == "unsupported" {
        return Err(format!(
            "no prebuilt {} release for this platform",
            tool.as_str()
        ));
    }
    if matches!(tool, ExternalTool::Manvi) && cfg!(windows) {
        return Err(
            "manvi has no Windows prebuilt (syscall::O_NOFOLLOW); use go install or a checkout"
                .into(),
        );
    }
    Ok(())
}

pub fn release_urls(tool: ExternalTool) -> ReleaseAvailability {
    if let Err(reason) = platform_supported(tool) {
        return ReleaseAvailability::Unavailable { reason };
    }
    let target = platform_target();
    let stem = asset_stem(tool);
    let asset_name = format!("{stem}-{target}.tar.gz");
    let base = format!(
        "{}/releases/latest/download",
        repo_for(tool).trim_end_matches(".git")
    );
    ReleaseAvailability::Available {
        asset_url: format!("{base}/{asset_name}"),
        checksum_url: format!("{base}/{asset_name}.sha256"),
        asset_name,
    }
}

/// Probe whether a release asset exists (HEAD / lightweight GET). Soft: a
/// network failure is "unavailable", not an install error yet.
pub fn probe_release(tool: ExternalTool) -> ReleaseAvailability {
    let urls = release_urls(tool);
    let ReleaseAvailability::Available {
        asset_url,
        checksum_url,
        asset_name,
    } = urls
    else {
        return urls;
    };
    match http_head_ok(&asset_url) {
        Ok(true) => ReleaseAvailability::Available {
            asset_url,
            checksum_url,
            asset_name,
        },
        Ok(false) => ReleaseAvailability::Unavailable {
            reason: format!(
                "no prebuilt release asset {asset_name} yet (publish a GitHub release with checksums)"
            ),
        },
        Err(e) => ReleaseAvailability::Unavailable {
            reason: format!("could not check releases for {}: {e}", tool.as_str()),
        },
    }
}

fn http_head_ok(url: &str) -> Result<bool, String> {
    let curl =
        git_cli::find_external_tool("curl").ok_or_else(|| "curl is not installed".to_string())?;
    let mut cmd = Command::new(&curl);
    cmd.args(["-fsI", "-L", "--max-time", "15", url]);
    let run =
        git_cli::run_bounded_capped(cmd, "curl head", Duration::from_secs(20), None, 64 * 1024)?;
    Ok(run.success)
}

fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    let curl =
        git_cli::find_external_tool("curl").ok_or_else(|| "curl is not installed".to_string())?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut cmd = Command::new(&curl);
    cmd.args([
        "-fsSL",
        "-L",
        "--max-time",
        &DOWNLOAD_DEADLINE.as_secs().to_string(),
        "-o",
        &dest.display().to_string(),
        url,
    ]);
    let run =
        git_cli::run_bounded_capped(cmd, "curl download", DOWNLOAD_DEADLINE, None, DOWNLOAD_CAP)?;
    if !run.success {
        let err = String::from_utf8_lossy(&run.stderr);
        return Err(format!(
            "download failed (exit {}): {}",
            run.status_code,
            err.chars().take(240).collect::<String>()
        ));
    }
    if !dest.is_file() {
        return Err(format!("download produced no file at {}", dest.display()));
    }
    Ok(())
}

/// Parse a `.sha256` file: either `hex  filename` or a bare 64-char hex.
pub fn parse_checksum_file(text: &str) -> Result<String, String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| "checksum file is empty".to_string())?;
    // GNU checksum tools prefix the record with one backslash when the
    // filename contains backslashes/newlines. It is not part of the digest.
    // Only accept that marker with the full GNU digest + mode + filename shape.
    let line = if let Some(escaped) = line.strip_prefix('\\') {
        let bytes = escaped.as_bytes();
        if bytes.len() <= 66
            || bytes.get(64) != Some(&b' ')
            || !matches!(bytes.get(65), Some(b' ' | b'*'))
        {
            return Err("malformed escaped checksum record".into());
        }
        escaped
    } else {
        line
    };
    let hex = line
        .split_whitespace()
        .next()
        .ok_or_else(|| "checksum file has no hex digest".to_string())?;
    let hex = hex.to_ascii_lowercase();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("checksum digest is not a 64-char hex: {hex}"));
    }
    Ok(hex)
}

/// Compute SHA-256 of a file via `shasum` / `sha256sum` (no new crate).
pub fn file_sha256_hex(path: &Path) -> Result<String, String> {
    let try_cmd = |program: &str, args: &[&str]| -> Result<String, String> {
        let bin =
            git_cli::find_external_tool(program).ok_or_else(|| format!("{program} not found"))?;
        let mut cmd = Command::new(&bin);
        cmd.args(args);
        cmd.arg(path);
        let run =
            git_cli::run_bounded_capped(cmd, program, Duration::from_secs(60), None, 64 * 1024)?;
        digest_from_run(run, program)
    };
    try_cmd("shasum", &["-a", "256"]).or_else(|_| try_cmd("sha256sum", &[]))
}

fn digest_from_run(run: git_cli::BoundedRun, program: &str) -> Result<String, String> {
    let run = run.require_complete(program)?;
    if !run.success {
        return Err(format!("{program} failed"));
    }
    let out = String::from_utf8_lossy(&run.stdout);
    parse_checksum_file(&out).map_err(|error| format!("{program}: {error}"))
}

/// Verify `path`'s digest equals `expected_hex`. Mismatch is a hard error.
pub fn verify_checksum(path: &Path, expected_hex: &str) -> Result<(), String> {
    let actual = file_sha256_hex(path)?;
    let expected = expected_hex.to_ascii_lowercase();
    if actual != expected {
        return Err(format!(
            "checksum mismatch for {}: expected {expected}, got {actual} — refusing to install (will not fall back)",
            path.display()
        ));
    }
    Ok(())
}

/// App-owned bin directory for installed prebuilts.
///
/// Follows `config_path()` so `GITPULSE_TOOL_CONFIG` tests (and anyone who
/// relocates tools.json) keep binaries next to that file instead of writing
/// into the real Application Support directory.
pub fn app_bin_dir() -> Result<PathBuf, String> {
    let cfg = crate::tool_config::config_path()?;
    let base = cfg
        .parent()
        .ok_or_else(|| format!("tools.json path {} has no parent directory", cfg.display()))?;
    let dir = base.join("bin");
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn extract_binary_from_tar_gz(
    archive: &Path,
    tool: ExternalTool,
    dest: &Path,
) -> Result<(), String> {
    let tar =
        git_cli::find_external_tool("tar").ok_or_else(|| "tar is not installed".to_string())?;
    let tmp = archive
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("extract-{}", tool.as_str()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("extract dir: {e}"))?;
    let mut cmd = Command::new(&tar);
    cmd.args([
        "-xzf",
        &archive.display().to_string(),
        "-C",
        &tmp.display().to_string(),
    ]);
    let run = git_cli::run_bounded_capped(
        cmd,
        "tar extract",
        Duration::from_secs(120),
        None,
        64 * 1024,
    )?;
    if !run.success {
        return Err(format!(
            "tar extract failed: {}",
            String::from_utf8_lossy(&run.stderr)
        ));
    }
    let want = binary_name(tool);
    let found = find_named_file(&tmp, want)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(&found, dest).map_err(|e| format!("copy binary: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(dest).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(dest, perms).map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_dir_all(&tmp);
    Ok(())
}

fn find_named_file(root: &Path, name: &str) -> Result<PathBuf, String> {
    fn walk(dir: &Path, name: &str, out: &mut Option<PathBuf>) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, name, out)?;
            } else if path.file_name().and_then(|s| s.to_str()) == Some(name) {
                *out = Some(path);
                return Ok(());
            }
        }
        Ok(())
    }
    let mut found = None;
    walk(root, name, &mut found).map_err(|e| e.to_string())?;
    found.ok_or_else(|| format!("archive did not contain {name}"))
}

/// Download, verify checksum (refuse on mismatch), install into app bin dir.
pub fn install_prebuilt(tool: ExternalTool) -> Result<PathBuf, String> {
    let availability = probe_release(tool);
    let ReleaseAvailability::Available {
        asset_url,
        checksum_url,
        asset_name,
    } = availability
    else {
        let ReleaseAvailability::Unavailable { reason } = availability else {
            unreachable!()
        };
        return Err(reason);
    };

    let work = app_bin_dir()?.join(format!(".download-{}", tool.as_str()));
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let archive = work.join(&asset_name);
    let checksum_file = work.join(format!("{asset_name}.sha256"));

    download_to(&checksum_url, &checksum_file)?;
    let expected = {
        let mut f = File::open(&checksum_file).map_err(|e| e.to_string())?;
        let mut text = String::new();
        f.read_to_string(&mut text).map_err(|e| e.to_string())?;
        parse_checksum_file(&text)?
    };

    download_to(&asset_url, &archive)?;

    // Verify before trusting — mismatch refuses; never fall back.
    verify_checksum(&archive, &expected)?;

    let dest = app_bin_dir()?.join(binary_name(tool));
    extract_binary_from_tar_gz(&archive, tool, &dest)?;
    let _ = fs::remove_dir_all(&work);
    Ok(dest)
}

/// Test seam: write bytes and refuse when checksum disagrees.
#[cfg(test)]
pub fn install_bytes_with_checksum(
    bytes: &[u8],
    expected_hex: &str,
    dest: &Path,
) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    {
        let mut f = File::create(dest).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
    }
    verify_checksum(dest, expected_hex)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn audit_digest_requires_complete_hex_output() {
        for (digest, incomplete) in [
            ("z".repeat(64), None),
            (
                "a".repeat(64),
                Some(crate::engine::git_cli::Incomplete::OverCap(64)),
            ),
        ] {
            let run = crate::engine::git_cli::BoundedRun {
                stdout: digest.into_bytes(),
                stderr: Vec::new(),
                success: true,
                status_code: 0,
                incomplete,
                stderr_incomplete: None,
                cancelled: false,
            };
            assert!(super::digest_from_run(run, "fixture").is_err());
        }
    }
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_checksum_accepts_gnu_escaped_filename_marker() {
        let digest = "abcdef0123456789".repeat(4);
        for separator in ["  ", " *"] {
            let record = format!("\\{digest}{separator}C:\\\\temp\\\\blob\\nname\n");
            assert_eq!(parse_checksum_file(&record).unwrap(), digest);
        }
        for malformed in [
            format!("\\{digest}"),
            format!("\\\\{digest}  file"),
            format!("\\{digest} wrong"),
        ] {
            assert!(parse_checksum_file(&malformed).is_err(), "{malformed}");
        }
    }

    #[test]
    fn parse_checksum_accepts_gnu_and_bare() {
        assert_eq!(
            parse_checksum_file(
                "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789  devmap.tar.gz\n"
            )
            .unwrap(),
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
        assert_eq!(
            parse_checksum_file(
                "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789\n"
            )
            .unwrap(),
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
    }

    #[test]
    fn checksum_mismatch_refuses_without_fallback() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("blob");
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(b"hello").unwrap();
        }
        // Wrong digest — must refuse.
        let err = verify_checksum(
            &path,
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(err.contains("refusing"), "{err}");
        assert!(err.contains("will not fall back"), "{err}");
    }

    #[test]
    fn install_bytes_refuses_bad_checksum() {
        let dir = tempfile::TempDir::new().unwrap();
        let dest = dir.path().join("bin");
        let err = install_bytes_with_checksum(
            b"payload",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            &dest,
        )
        .unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        // File may exist on disk but must not be treated as installed success.
        assert!(err.contains("refusing"));
    }

    #[test]
    fn manvi_windows_is_explicitly_unavailable() {
        let urls = release_urls(ExternalTool::Manvi);
        if cfg!(windows) {
            match urls {
                ReleaseAvailability::Unavailable { reason } => {
                    assert!(reason.to_ascii_lowercase().contains("windows"), "{reason}");
                }
                ReleaseAvailability::Available { .. } => {
                    panic!("manvi must not advertise a Windows prebuilt")
                }
            }
        }
    }

    #[test]
    fn platform_target_is_concrete() {
        assert_ne!(platform_target(), "");
    }
}
