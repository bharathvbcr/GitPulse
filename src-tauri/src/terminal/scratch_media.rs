//! Ephemeral `0600` scratch PNG storage for terminal clipboard attachments and
//! native browser-pane snapshots.
//!
//! Files live under `<temp_dir>/gitpulse-terminal-captures/` with owner-only
//! permissions (`0700` directory, `0600` files on Unix) and are pruned lazily
//! once older than [`RETENTION`], so local coding agents (`claude`, `agy`) can
//! read `@<path>` attachments without leaving unbounded screenshots behind.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Standard 8-byte PNG file signature (`\x89PNG\r\n\x1a\n`).
pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Upper bound on any single scratch PNG (16 MiB).
pub const MAX_SCRATCH_PNG_BYTES: usize = 16 * 1024 * 1024;

/// Maximum number of copied Finder file paths returned on a single paste.
pub const MAX_CLIPBOARD_FILES: usize = 32;

const MAX_CLIPBOARD_PATH_BYTES: usize = 4096;

const SCRATCH_DIR_NAME: &str = "gitpulse-terminal-captures";
const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_TOKEN_LEN: usize = 48;

static NEXT_CAPTURE_SEQ: AtomicU64 = AtomicU64::new(1);

fn sanitize_token(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .take(MAX_TOKEN_LEN)
        .collect();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    }
}

pub fn validate_png_bytes(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < PNG_SIGNATURE.len() {
        return Err("image payload is empty or truncated".into());
    }
    if bytes.len() > MAX_SCRATCH_PNG_BYTES {
        return Err(format!(
            "image payload ({} bytes) exceeds the {} MiB limit",
            bytes.len(),
            MAX_SCRATCH_PNG_BYTES / (1024 * 1024)
        ));
    }
    if bytes[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err("image payload must be a valid PNG".into());
    }
    Ok(())
}

fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(dir)
            .map_err(|e| format!("could not create scratch capture directory: {e}"))?;
        let meta = fs::symlink_metadata(dir)
            .map_err(|e| format!("could not inspect scratch capture directory: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err("scratch capture directory must not be a symlink".into());
        }
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
            .map_err(|e| format!("could not create scratch capture directory: {e}"))
    }
}

/// Removes `.png` entries inside `dir` whose `modified` timestamp is older than `max_age`.
pub fn prune_expired_in(dir: &Path, now: SystemTime, max_age: Duration) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("png") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age >= max_age && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Validates and writes `bytes` into `dir` as a unique owner-only `.png` file.
pub fn write_scratch_png_in(
    dir: &Path,
    prefix: &str,
    owner_id: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    validate_png_bytes(bytes)?;
    ensure_private_dir(dir)?;

    let now = SystemTime::now();
    let _ = prune_expired_in(dir, now, RETENTION);

    let safe_prefix = sanitize_token(prefix, "capture");
    let safe_owner = sanitize_token(owner_id, "pane");
    let millis = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let seq = NEXT_CAPTURE_SEQ.fetch_add(1, Ordering::Relaxed);
    let filename = format!("{safe_prefix}-{safe_owner}-{millis}-{seq}.png");
    let path = dir.join(filename);

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options
        .open(&path)
        .map_err(|e| format!("could not create scratch PNG: {e}"))?;
    if let Err(err) = file.write_all(bytes).and_then(|()| file.flush()) {
        let _ = fs::remove_file(&path);
        return Err(format!("could not write scratch PNG: {err}"));
    }
    Ok(path)
}

/// Writes `bytes` into the default scratch capture directory under `std::env::temp_dir()`.
pub fn write_scratch_png(prefix: &str, owner_id: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(SCRATCH_DIR_NAME);
    write_scratch_png_in(&dir, prefix, owner_id, bytes)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_decode_utf8(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            if idx + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_nibble(bytes[idx + 1])?;
            let lo = hex_nibble(bytes[idx + 2])?;
            out.push((hi << 4) | lo);
            idx += 3;
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }
    if out.contains(&0) {
        return None;
    }
    String::from_utf8(out).ok()
}

fn clean_local_posix_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_CLIPBOARD_PATH_BYTES
        || !trimmed.starts_with('/')
        || trimmed.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n')
    {
        return None;
    }
    // Reject unresolved macOS file-reference URLs (`/.file/id=...`) if NSURL failed to map them.
    if trimmed.starts_with("/.file/") {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in trimmed.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        Some("/".to_owned())
    } else {
        Some(format!("/{}", parts.join("/")))
    }
}

/// Decodes a `file:///...` or `file://localhost/...` URL into a normalized local POSIX path.
pub fn parse_file_url_path(raw_url: &str) -> Option<String> {
    let trimmed = raw_url.trim();
    let rest = trimmed.strip_prefix("file://")?;
    let encoded_path: std::borrow::Cow<'_, str> = if rest.starts_with('/') {
        std::borrow::Cow::Borrowed(rest)
    } else {
        let after_host = rest.strip_prefix("localhost/")?;
        std::borrow::Cow::Owned(format!("/{after_host}"))
    };
    let raw_encoded = encoded_path.as_ref();
    let without_query = raw_encoded
        .split_once(['?', '#'])
        .map_or(raw_encoded, |(head, _)| head);
    let decoded = percent_decode_utf8(without_query)?;
    clean_local_posix_path(&decoded)
}

/// Normalizes, validates, deduplicates, and caps a list of raw clipboard file URLs or POSIX paths.
pub fn normalize_clipboard_file_paths<I, S>(candidates: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in candidates {
        let raw = item.as_ref();
        let resolved = if raw.trim_start().starts_with("file://") {
            parse_file_url_path(raw)
        } else {
            clean_local_posix_path(raw)
        };
        let Some(clean) = resolved else {
            continue;
        };
        if seen.insert(clean.clone()) {
            out.push(clean);
            if out.len() >= MAX_CLIPBOARD_FILES {
                break;
            }
        }
    }
    out
}

/// Reads copied local file paths from `NSPasteboard.generalPasteboard` (`public.file-url`),
/// resolving Finder `file:///.file/id=...` reference URLs via `NSURL.filePathURL`.
///
/// Must be called on the AppKit main thread on macOS.
#[cfg(target_os = "macos")]
pub fn read_clipboard_file_paths() -> Vec<String> {
    use objc2::exception::catch;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    use std::ffi::CStr;
    use std::panic::AssertUnwindSafe;

    let raw_candidates = catch(AssertUnwindSafe(|| -> Vec<String> {
        unsafe {
            let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
            if pb.is_null() {
                return Vec::new();
            }
            let items: *mut AnyObject = msg_send![&*pb, pasteboardItems];
            if items.is_null() {
                return Vec::new();
            }
            let count: usize = msg_send![&*items, count];
            if count == 0 {
                return Vec::new();
            }
            let uti = NSString::from_str("public.file-url");
            let limit = count.min(MAX_CLIPBOARD_FILES * 2);
            let mut found = Vec::with_capacity(limit.min(MAX_CLIPBOARD_FILES));
            for idx in 0..limit {
                let item: *mut AnyObject = msg_send![&*items, objectAtIndex: idx];
                if item.is_null() {
                    continue;
                }
                let raw_url_ns: *mut AnyObject = msg_send![&*item, stringForType: &*uti];
                if raw_url_ns.is_null() {
                    continue;
                }
                let nsurl: *mut AnyObject = msg_send![class!(NSURL), URLWithString: &*raw_url_ns];
                if !nsurl.is_null() {
                    let file_url: *mut AnyObject = msg_send![&*nsurl, filePathURL];
                    let target_url = if !file_url.is_null() { file_url } else { nsurl };
                    let path_ns: *mut AnyObject = msg_send![&*target_url, path];
                    if !path_ns.is_null() {
                        let utf8: *const std::ffi::c_char = msg_send![&*path_ns, UTF8String];
                        if !utf8.is_null() {
                            found.push(CStr::from_ptr(utf8).to_string_lossy().into_owned());
                            continue;
                        }
                    }
                }
                let raw_utf8: *const std::ffi::c_char = msg_send![&*raw_url_ns, UTF8String];
                if !raw_utf8.is_null() {
                    found.push(CStr::from_ptr(raw_utf8).to_string_lossy().into_owned());
                }
            }
            found
        }
    }))
    .unwrap_or_default();

    normalize_clipboard_file_paths(raw_candidates)
}

#[cfg(not(target_os = "macos"))]
pub fn read_clipboard_file_paths() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_png() -> Vec<u8> {
        let mut buf = PNG_SIGNATURE.to_vec();
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R']);
        buf
    }

    #[test]
    fn rejects_empty_non_png_and_oversized_payloads() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(write_scratch_png_in(tmp.path(), "clip", "tab-1", b"").is_err());
        assert!(write_scratch_png_in(tmp.path(), "clip", "tab-1", b"not-a-png-file").is_err());

        let mut huge = PNG_SIGNATURE.to_vec();
        huge.resize(MAX_SCRATCH_PNG_BYTES + 1, 0);
        let err = write_scratch_png_in(tmp.path(), "clip", "tab-1", &huge).unwrap_err();
        assert!(err.contains("exceeds"), "unexpected error: {err}");
    }

    #[test]
    fn writes_sanitized_filename_with_owner_only_permissions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("captures");
        let written =
            write_scratch_png_in(&dir, "../browser", "../../tab/7", &sample_png()).unwrap();
        assert_eq!(written.parent(), Some(dir.as_path()));
        let name = written.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("browser-tab7-"), "unexpected name: {name}");
        assert!(name.ends_with(".png"));
        assert_eq!(fs::read(&written).unwrap(), sample_png());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = fs::metadata(&written).unwrap().permissions().mode() & 0o777;
            let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600);
            assert_eq!(dir_mode, 0o700);
        }
    }

    #[test]
    fn prunes_expired_png_files_without_touching_fresh_ones() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let stale = write_scratch_png_in(tmp.path(), "snap", "old", &sample_png()).unwrap();
        let fresh_note = tmp.path().join("keep.txt");
        fs::write(&fresh_note, b"keep").unwrap();

        let future = SystemTime::now() + RETENTION + Duration::from_secs(5);
        let removed = prune_expired_in(tmp.path(), future, RETENTION);
        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(fresh_note.exists());
    }

    #[test]
    fn parses_and_normalizes_clipboard_file_urls_and_paths() {
        assert_eq!(
            parse_file_url_path("file:///Users/dev/My%20Project/caf%C3%A9.rs").as_deref(),
            Some("/Users/dev/My Project/café.rs")
        );
        assert_eq!(
            parse_file_url_path("file://localhost/tmp/screen%201.png").as_deref(),
            Some("/tmp/screen 1.png")
        );
        assert!(parse_file_url_path("https://example.com/a.png").is_none());
        assert!(parse_file_url_path("file://evil.example.com/etc/passwd").is_none());
        assert!(parse_file_url_path("file:///tmp/bad%00name.png").is_none());

        let mut many: Vec<String> = vec![
            "file:///repo/src/./main.rs".into(),
            "/repo/src/main.rs".into(), // duplicate after normalization
            "https://not-a-local-file.test/x".into(),
            "relative/path.rs".into(),
            "/bad/\0/null.rs".into(),
            "file:///repo/docs/spec%20v2.md/".into(),
        ];
        for idx in 0..40 {
            many.push(format!("/repo/extra/file-{idx}.ts"));
        }
        let normalized = normalize_clipboard_file_paths(many);
        assert_eq!(normalized.len(), MAX_CLIPBOARD_FILES);
        assert_eq!(normalized[0], "/repo/src/main.rs");
        assert_eq!(normalized[1], "/repo/docs/spec v2.md");
    }
}
