//! Prebuilt release install rung: download, verify checksum, install.
//!
//! A checksum mismatch is a **refusal** — never a fallback to running the
//! binary anyway. What may be installed is decided in this repository, not by
//! the server: [`PINNED_RELEASES`] names the exact version, asset and SHA-256
//! of each prebuilt. A `releases/latest` redirect or a `.sha256` file served
//! beside the archive proves nothing (whoever can replace the archive can
//! replace its checksum), so neither is consulted. A platform with no pin is
//! unavailable with a reason, and the source installs remain.

use super::{ExternalTool, PUBLIC_DEVMAP_REPO, PUBLIC_MANVI_REPO};
use crate::engine::git_cli;
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(test)]
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// curl follows redirects (`-L`), and by default would follow one to `http`,
/// `ftp` or `file`. These pin both the first request and every hop to https.
const CURL_HTTPS_ONLY: [&str; 4] = ["--proto", "=https", "--proto-redir", "=https"];

const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(5 * 60);
const DOWNLOAD_CAP: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseAvailability {
    Available {
        asset_url: String,
        /// Release tag the pin names, e.g. `v0.2.3`.
        version: String,
        /// Expected SHA-256 of the archive, from [`PINNED_RELEASES`].
        sha256: String,
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

/// One prebuilt this build of GitPulse will install: a release tag, the asset
/// in it for one target, and that asset's SHA-256 as recorded here.
#[derive(Debug, Clone, Copy)]
pub struct PinnedRelease {
    pub tool: &'static str,
    /// Release tag, `vMAJOR.MINOR.PATCH`.
    pub version: &'static str,
    pub target: &'static str,
    pub asset_name: &'static str,
    /// 64 lowercase hex characters.
    pub sha256: &'static str,
}

/// The prebuilts GitPulse will install.
///
/// Empty on purpose: the published DevCouncil/Manvi assets are not in the
/// format this installer unpacks (devmap ships `devmap-cli-<target>.tar.xz`
/// and `.zip`, this extracts `.tar.gz`), and a SHA-256 is only worth pinning
/// for an artifact that was actually installed and checked. Add an entry
/// (tool, tag, target, asset, digest) when a release in the supported format
/// is published and verified; until then the rung reports itself unavailable
/// and the pinned source installs are used.
pub const PINNED_RELEASES: &[PinnedRelease] = &[];

/// The pin for `tool` on `target`, if the table has one.
fn pinned_for<'a>(
    table: &'a [PinnedRelease],
    tool: ExternalTool,
    target: &str,
) -> Option<&'a PinnedRelease> {
    table
        .iter()
        .find(|pin| pin.tool == tool.as_str() && pin.target == target)
}

/// Where `pin` is downloaded from: an exact tag, never `latest`.
fn pinned_asset_url(tool: ExternalTool, pin: &PinnedRelease) -> String {
    format!(
        "{}/releases/download/{}/{}",
        repo_for(tool).trim_end_matches(".git"),
        pin.version,
        pin.asset_name
    )
}

fn release_urls_in(table: &[PinnedRelease], tool: ExternalTool) -> ReleaseAvailability {
    if let Err(reason) = platform_supported(tool) {
        return ReleaseAvailability::Unavailable { reason };
    }
    let target = platform_target();
    match pinned_for(table, tool, target) {
        Some(pin) => ReleaseAvailability::Available {
            asset_url: pinned_asset_url(tool, pin),
            version: pin.version.to_string(),
            sha256: pin.sha256.to_ascii_lowercase(),
            asset_name: pin.asset_name.to_string(),
        },
        None => ReleaseAvailability::Unavailable {
            reason: format!(
                "no pinned prebuilt {} release for {target}; GitPulse installs only releases whose \
                 checksum it carries",
                tool.as_str()
            ),
        },
    }
}

pub fn release_urls(tool: ExternalTool) -> ReleaseAvailability {
    release_urls_in(PINNED_RELEASES, tool)
}

/// `1.2.3` from the first `MAJOR.MINOR.PATCH` run in `text` (`v0.2.3`,
/// `devmap 0.2.4 (store schema 26 …)`). Pre-release suffixes are ignored.
pub(crate) fn version_triple(text: &str) -> Option<(u32, u32, u32)> {
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_digit() {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            end += 1;
        }
        let mut parts = text[start..end].split('.').map(|p| p.parse::<u32>().ok());
        if let (Some(Some(a)), Some(Some(b)), Some(Some(c))) =
            (parts.next(), parts.next(), parts.next())
        {
            return Some((a, b, c));
        }
        start = end;
    }
    None
}

/// Refuses to replace an installed `installed` version with an older
/// `pinned` one. An installed binary whose version cannot be read does not
/// block the install; a pin whose own version cannot be read does.
fn refuse_downgrade(installed: Option<&str>, pinned: &str) -> Result<(), String> {
    let pinned_version = version_triple(pinned)
        .ok_or_else(|| format!("pinned release version {pinned:?} is not MAJOR.MINOR.PATCH"))?;
    match installed.and_then(version_triple) {
        Some(current) if current > pinned_version => Err(format!(
            "refusing to downgrade: {}.{}.{} is installed and the pinned release is {pinned}",
            current.0, current.1, current.2
        )),
        _ => Ok(()),
    }
}

/// Probe whether a release asset exists (HEAD / lightweight GET). Soft: a
/// network failure is "unavailable", not an install error yet.
pub fn probe_release(tool: ExternalTool) -> ReleaseAvailability {
    let urls = release_urls(tool);
    let ReleaseAvailability::Available {
        asset_url,
        version,
        sha256,
        asset_name,
    } = urls
    else {
        return urls;
    };
    match http_head_ok(&asset_url) {
        Ok(true) => ReleaseAvailability::Available {
            asset_url,
            version,
            sha256,
            asset_name,
        },
        Ok(false) => ReleaseAvailability::Unavailable {
            reason: format!(
                "pinned release asset {asset_name} ({version}) is not published at its URL"
            ),
        },
        Err(e) => ReleaseAvailability::Unavailable {
            reason: format!(
                "could not check releases for {}: {e}",
                tool.as_str(),
                e = e.message()
            ),
        },
    }
}

/// Every network round trip this module has made, counted so a test can assert
/// that a path it believes is local really made none.
///
/// A wall-clock bound cannot answer that question: a probe served from a warm
/// DNS cache is fast, and a loaded machine is slow with no probe at all. This
/// counts the thing itself. Callers serialize with [`super::network_serial`],
/// because the count is process-wide and the suite runs in parallel.
#[cfg(test)]
pub(super) static NETWORK_PROBES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(super) fn network_probes() -> usize {
    NETWORK_PROBES.load(std::sync::atomic::Ordering::SeqCst)
}

/// Prefix of the line `curl_head_args` asks curl to print after the headers:
/// the final response's status, after every redirect.
const HEAD_STATUS_PREFIX: &str = "gitpulse-http-status:";

/// No `-f`: with it a 404 exits with a code (56 on a HEAD to GitHub, measured
/// 2026-10-17) that does not tell "the asset is not there" from "the server
/// could not be asked". The status is read from `-w` instead.
fn curl_head_args(url: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-sI", "-L"].map(String::from).into();
    args.extend(CURL_HTTPS_ONLY.map(String::from));
    args.extend([
        "--max-time".to_string(),
        "15".to_string(),
        "-w".to_string(),
        format!("\n{HEAD_STATUS_PREFIX}%{{http_code}}\n"),
        url.to_string(),
    ]);
    args
}

/// What a HEAD answered: `Ok(true)` for a 2xx, `Ok(false)` only when the
/// server said the thing is not there (404, 410), and `Err` for everything
/// else. A DNS failure, a timeout, a 403 or a 5xx is a question that went
/// unanswered, never evidence of absence.
fn head_verdict(success: bool, stdout: &[u8]) -> Result<bool, String> {
    let text = String::from_utf8_lossy(stdout);
    let code = text
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(HEAD_STATUS_PREFIX))
        .map(str::trim)
        .ok_or_else(|| "curl reported no HTTP status".to_string())?;
    let status: u16 = code
        .parse()
        .map_err(|_| format!("curl reported an unreadable HTTP status '{code}'"))?;
    match (success, status) {
        (true, 200..=299) => Ok(true),
        (true, 404 | 410) => Ok(false),
        (_, 0) => Err("no HTTP response (name resolution, connection or deadline)".into()),
        (false, _) => Err(format!("the request ended early after HTTP {status}")),
        (true, _) => Err(format!("HTTP {status}")),
    }
}

fn curl_download_args(url: &str, dest: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["-fsSL", "-L"].map(String::from).into();
    args.extend(CURL_HTTPS_ONLY.map(String::from));
    args.extend([
        "--max-time".to_string(),
        DOWNLOAD_DEADLINE.as_secs().to_string(),
        "-o".to_string(),
        dest.display().to_string(),
        url.to_string(),
    ]);
    args
}

pub(crate) fn http_head_ok(url: &str) -> Result<bool, CommandError> {
    #[cfg(test)]
    NETWORK_PROBES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let curl =
        git_cli::find_external_tool("curl").ok_or_else(|| "curl is not installed".to_string())?;
    let mut cmd = Command::new(&curl);
    cmd.args(curl_head_args(url));
    let run =
        git_cli::run_bounded_capped(cmd, "curl head", Duration::from_secs(20), None, 64 * 1024)?;
    head_verdict(run.success, &run.stdout)
        .map_err(|e| format!("{e} (curl exit {})", run.status_code))
        .map_err(CommandError::from)
}

fn download_to(url: &str, dest: &Path) -> Result<(), CommandError> {
    let curl =
        git_cli::find_external_tool("curl").ok_or_else(|| "curl is not installed".to_string())?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut cmd = Command::new(&curl);
    cmd.args(curl_download_args(url, dest));
    let run =
        git_cli::run_bounded_capped(cmd, "curl download", DOWNLOAD_DEADLINE, None, DOWNLOAD_CAP)?;
    if !run.success {
        let err = String::from_utf8_lossy(&run.stderr);
        return Err(format!(
            "download failed (exit {}): {}",
            run.status_code,
            err.chars().take(240).collect::<String>()
        )
        .into());
    }
    if !dest.is_file() {
        return Err(format!("download produced no file at {}", dest.display()).into());
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
pub fn file_sha256_hex(path: &Path) -> Result<String, CommandError> {
    sha256_with(Some(path), None)
}

/// Compute SHA-256 of `bytes`, fed on stdin to the same tools as
/// [`file_sha256_hex`].
pub fn bytes_sha256_hex(bytes: &[u8]) -> Result<String, CommandError> {
    sha256_with(None, Some(bytes))
}

fn sha256_with(path: Option<&Path>, stdin: Option<&[u8]>) -> Result<String, CommandError> {
    let try_cmd = |program: &str, args: &[&str]| -> Result<String, CommandError> {
        let bin =
            git_cli::find_external_tool(program).ok_or_else(|| format!("{program} not found"))?;
        let mut cmd = Command::new(&bin);
        cmd.args(args);
        if let Some(path) = path {
            cmd.arg(path);
        }
        let run =
            git_cli::run_bounded_capped(cmd, program, Duration::from_secs(60), stdin, 64 * 1024)?;
        digest_from_run(run, program)
    };
    try_cmd("shasum", &["-a", "256"]).or_else(|_| try_cmd("sha256sum", &[]))
}

fn digest_from_run(run: git_cli::BoundedRun, program: &str) -> Result<String, CommandError> {
    let run = run.require_complete(program)?;
    if !run.success {
        return Err(format!("{program} failed").into());
    }
    let out = String::from_utf8_lossy(&run.stdout);
    parse_checksum_file(&out)
        .map_err(|error| format!("{program}: {error}"))
        .map_err(CommandError::from)
}

/// Verify `path`'s digest equals `expected_hex`. Mismatch is a hard error.
pub fn verify_checksum(path: &Path, expected_hex: &str) -> Result<(), CommandError> {
    let actual = file_sha256_hex(path)?;
    let expected = expected_hex.to_ascii_lowercase();
    if actual != expected {
        return Err(format!(
            "checksum mismatch for {}: expected {expected}, got {actual} — refusing to install (will not fall back)",
            path.display()
        ).into());
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
) -> Result<(), CommandError> {
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
        )
        .into());
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

/// A fixed, empty directory the toolchain installs run from, under the app's
/// own data. `cargo` reads `.cargo/config.toml` from every ancestor of its
/// working directory and `go` reads `go.work`/`go.mod`, so running from the
/// launch directory would let a file there redirect the install.
pub fn install_work_dir() -> Result<PathBuf, String> {
    let dir = app_bin_dir()?.join(".install-cwd");
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Download the pinned asset, verify it against the digest recorded in this
/// repository (refuse on mismatch), refuse a downgrade, install into the app
/// bin dir.
pub fn install_prebuilt(tool: ExternalTool) -> Result<PathBuf, CommandError> {
    let availability = probe_release(tool);
    let ReleaseAvailability::Available {
        asset_url,
        version,
        sha256,
        asset_name,
    } = availability
    else {
        let ReleaseAvailability::Unavailable { reason } = availability else {
            unreachable!()
        };
        return Err(reason.into());
    };

    let dest = app_bin_dir()?.join(binary_name(tool));
    if dest.is_file() {
        let installed = super::probe_version(&dest.display().to_string(), tool);
        refuse_downgrade(installed.as_deref(), &version)?;
    }

    let work = app_bin_dir()?.join(format!(".download-{}", tool.as_str()));
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let archive = work.join(&asset_name);
    download_to(&asset_url, &archive)?;

    // Verify before trusting — mismatch refuses; never fall back.
    verify_checksum(&archive, &sha256)?;

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
) -> Result<(), CommandError> {
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
        assert!(
            err.contains("checksum mismatch"),
            "{err}",
            err = err.message()
        );
        assert!(err.contains("refusing"), "{err}", err = err.message());
        assert!(
            err.contains("will not fall back"),
            "{err}",
            err = err.message()
        );
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
        assert!(
            err.contains("checksum mismatch"),
            "{err}",
            err = err.message()
        );
        // File may exist on disk but must not be treated as installed success.
        assert!(err.contains("refusing"));
    }

    #[test]
    fn an_empty_pin_table_makes_the_release_rung_unavailable_not_latest() {
        let ReleaseAvailability::Unavailable { reason } =
            release_urls_in(&[], ExternalTool::Devmap)
        else {
            panic!("no pin must mean unavailable");
        };
        assert!(!platform_target().is_empty());
        assert!(
            reason.contains("pinned") || reason.contains("platform"),
            "{reason}"
        );
    }

    #[test]
    fn a_pinned_release_downloads_a_tag_and_carries_its_own_digest() {
        let digest = "AB".repeat(32);
        let table = [PinnedRelease {
            tool: "devmap",
            version: "v0.2.3",
            target: platform_target(),
            asset_name: "devmap-x.tar.gz",
            sha256: Box::leak(digest.into_boxed_str()),
        }];
        if platform_supported(ExternalTool::Devmap).is_err() {
            return;
        }
        let ReleaseAvailability::Available {
            asset_url,
            version,
            sha256,
            ..
        } = release_urls_in(&table, ExternalTool::Devmap)
        else {
            panic!("a pinned target is available");
        };
        assert!(
            asset_url.contains("/releases/download/v0.2.3/"),
            "{asset_url}"
        );
        assert!(!asset_url.contains("latest"), "{asset_url}");
        assert_eq!(version, "v0.2.3");
        assert_eq!(sha256, "ab".repeat(32));
        // Another tool on the same target has no pin.
        assert!(matches!(
            release_urls_in(&table, ExternalTool::Manvi),
            ReleaseAvailability::Unavailable { .. }
        ));
    }

    #[test]
    fn every_shipped_pin_is_well_formed() {
        for pin in PINNED_RELEASES {
            assert!(pin.sha256.len() == 64 && pin.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(version_triple(pin.version).is_some(), "{}", pin.version);
            assert!(pin.version.starts_with('v'));
        }
    }

    #[test]
    fn a_downgrade_is_refused_and_an_upgrade_or_unknown_is_not() {
        assert!(refuse_downgrade(Some("devmap 0.2.4 (store schema 26)"), "v0.2.3").is_err());
        assert!(refuse_downgrade(Some("0.2.3"), "v0.2.3").is_ok());
        assert!(refuse_downgrade(Some("devmap 0.2.2"), "v0.2.3").is_ok());
        assert!(refuse_downgrade(None, "v0.2.3").is_ok());
        assert!(refuse_downgrade(Some("unknown"), "v0.2.3").is_ok());
        assert!(
            refuse_downgrade(Some("1.0.0"), "latest").is_err(),
            "an unreadable pin blocks"
        );
        assert_eq!(version_triple("v10.20.30-rc1"), Some((10, 20, 30)));
        assert_eq!(version_triple("build 06d704b92682"), None);
    }

    #[test]
    fn curl_is_confined_to_https_for_the_request_and_every_redirect() {
        for args in [
            curl_head_args("https://example.com/a"),
            curl_download_args("https://example.com/a", Path::new("/tmp/a")),
        ] {
            let at = args.iter().position(|a| a == "--proto").expect("--proto");
            assert_eq!(args[at + 1], "=https");
            let at = args
                .iter()
                .position(|a| a == "--proto-redir")
                .expect("--proto-redir");
            assert_eq!(args[at + 1], "=https");
        }
    }

    /// What curl printed for a HEAD, as `head_verdict` reads it: the headers,
    /// then the status line `curl_head_args` asks for.
    fn head_output(final_code: &str) -> Vec<u8> {
        format!(
            "HTTP/2 302 \r\nlocation: https://release-assets.example/a\r\n\r\nHTTP/2 {final_code} \r\n\r\n{HEAD_STATUS_PREFIX}{final_code}\n"
        )
        .into_bytes()
    }

    #[test]
    fn a_head_is_published_only_on_a_2xx_and_absent_only_on_a_404() {
        assert_eq!(head_verdict(true, &head_output("200")), Ok(true));
        assert_eq!(head_verdict(true, &head_output("404")), Ok(false));
        assert_eq!(head_verdict(true, &head_output("410")), Ok(false));
    }

    #[test]
    fn a_head_that_could_not_be_answered_is_an_error_not_an_absent_asset() {
        // These used to read as Ok(false), "not published", and the update
        // check then said "up to date" about a check that never happened.
        for (success, out) in [
            (true, head_output("503")),
            (true, head_output("403")),
            // Name resolution or connection failure: curl reports 000.
            (false, head_output("000")),
            // A deadline cut the transfer after a 200 arrived.
            (false, head_output("200")),
            // No status line at all.
            (false, Vec::new()),
            (true, b"garbage".to_vec()),
        ] {
            let verdict = head_verdict(success, &out);
            assert!(
                verdict.is_err(),
                "{verdict:?} for {:?}",
                String::from_utf8_lossy(&out)
            );
        }
    }

    #[test]
    fn the_head_request_reports_its_status_instead_of_failing_on_it() {
        let args = curl_head_args("https://example.com/a");
        // `-f` turns a 404 into an exit code that a 5xx and a dropped
        // connection share, which is how "absent" and "could not ask" merged.
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('f')),
            "{args:?}"
        );
        let at = args.iter().position(|a| a == "-w").expect("-w");
        assert!(args[at + 1].contains(HEAD_STATUS_PREFIX));
        assert!(args[at + 1].contains("%{http_code}"));
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
