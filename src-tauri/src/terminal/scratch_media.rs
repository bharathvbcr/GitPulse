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
}
