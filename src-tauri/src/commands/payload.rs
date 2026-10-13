//! Size bounds on content the webview hands to a command.
//!
//! A buffer arrives as a JSON string and is then copied again on the Rust side
//! (cloned into a worker, re-encoded, written). Nothing bounded how large it
//! could be, so a runaway editor buffer or a hostile page could push hundreds
//! of megabytes through IPC and have them duplicated. These checks run at the
//! command boundary, before the first copy, and say what was too large rather
//! than truncating: a cut-off buffer saved or previewed as if whole is worse
//! than a refusal.
//!
//! [`PayloadError`] is a typed value rendered to the `String` the IPC layer
//! carries only at that edge (`Display`), so the typed IPC error can adopt the
//! variants later without touching the checks.

use crate::engine::budget::MAX_FILE_BYTES;

/// One buffer previewed or written: the largest the file viewer can load, so a
/// save can never exceed what a legitimate edit started from.
pub(crate) const MAX_CONTENT_BYTES: usize = MAX_FILE_BYTES as usize;

/// Content of one file in a preview request. A single source file past this is
/// not something the parse frontend can answer for within its deadline.
pub(crate) const MAX_PREVIEW_FILE_BYTES: usize = 2 * 1024 * 1024;

/// Content (and paths) of a whole `preview_many` request.
pub(crate) const MAX_PREVIEW_TOTAL_BYTES: usize = MAX_CONTENT_BYTES;

/// Entries in a `preview_many` request. The client sends at most
/// [`crate::devmap::cli::MAX_PREVIEW_FILES`]; the backend previews that many and
/// reports the rest as omitted, so a generous multiple is accepted before the
/// request is refused as a whole.
pub(crate) const MAX_PREVIEW_ENTRIES: usize = 16 * crate::devmap::cli::MAX_PREVIEW_FILES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PayloadError {
    /// Content (or the sum of it) is over its byte limit.
    TooLarge {
        what: &'static str,
        bytes: usize,
        limit: usize,
    },
    /// More entries than a request may carry.
    TooMany {
        what: &'static str,
        count: usize,
        limit: usize,
    },
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { what, bytes, limit } => {
                write!(f, "{what} is too large: {bytes} bytes, limit {limit} bytes")
            }
            Self::TooMany { what, count, limit } => {
                write!(f, "too many {what}: {count}, limit {limit}")
            }
        }
    }
}

impl From<PayloadError> for String {
    fn from(error: PayloadError) -> Self {
        error.to_string()
    }
}

/// `content` for a single-file command (`devmap_preview`, `write_file_content`).
pub(crate) fn check_content(
    what: &'static str,
    content: &str,
    limit: usize,
) -> Result<(), PayloadError> {
    if content.len() > limit {
        return Err(PayloadError::TooLarge {
            what,
            bytes: content.len(),
            limit,
        });
    }
    Ok(())
}

/// A `preview_many` request: entry count first, then each file, then the sum.
/// Measures `len()` only, so no entry is copied to decide.
pub(crate) fn check_preview_batch(files: &[(String, String)]) -> Result<(), PayloadError> {
    if files.len() > MAX_PREVIEW_ENTRIES {
        return Err(PayloadError::TooMany {
            what: "files in one preview request",
            count: files.len(),
            limit: MAX_PREVIEW_ENTRIES,
        });
    }
    let mut total = 0usize;
    for (path, content) in files {
        check_content("preview file content", content, MAX_PREVIEW_FILE_BYTES)?;
        total = total
            .saturating_add(path.len())
            .saturating_add(content.len());
        if total > MAX_PREVIEW_TOTAL_BYTES {
            return Err(PayloadError::TooLarge {
                what: "preview request",
                bytes: total,
                limit: MAX_PREVIEW_TOTAL_BYTES,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod payload_tests {
    use super::*;

    fn file(path: &str, bytes: usize) -> (String, String) {
        (path.to_string(), "x".repeat(bytes))
    }

    #[test]
    fn content_at_the_limit_passes_and_one_byte_over_is_refused() {
        assert!(check_content("c", &"x".repeat(10), 10).is_ok());
        assert_eq!(
            check_content("c", &"x".repeat(11), 10),
            Err(PayloadError::TooLarge {
                what: "c",
                bytes: 11,
                limit: 10
            })
        );
    }

    #[test]
    fn a_batch_of_ordinary_files_passes_even_past_the_preview_fanout() {
        let files: Vec<_> = (0..40)
            .map(|i| file(&format!("src/f{i}.rs"), 1_000))
            .collect();
        assert_eq!(check_preview_batch(&files), Ok(()));
    }

    #[test]
    fn seventeen_files_with_one_huge_body_are_refused() {
        let mut files: Vec<_> = (0..16).map(|i| file(&format!("src/f{i}.rs"), 10)).collect();
        files.push(file("src/huge.rs", MAX_PREVIEW_FILE_BYTES + 1));
        assert!(matches!(
            check_preview_batch(&files),
            Err(PayloadError::TooLarge {
                what: "preview file content",
                ..
            })
        ));
    }

    #[test]
    fn many_individually_allowed_files_are_refused_by_their_sum() {
        let files: Vec<_> = (0..8)
            .map(|i| file(&format!("src/f{i}.rs"), MAX_PREVIEW_FILE_BYTES))
            .collect();
        assert!(matches!(
            check_preview_batch(&files),
            Err(PayloadError::TooLarge {
                what: "preview request",
                ..
            })
        ));
    }

    #[test]
    fn too_many_entries_are_refused_without_looking_at_them() {
        let files: Vec<_> = (0..=MAX_PREVIEW_ENTRIES)
            .map(|i| (format!("f{i}"), String::new()))
            .collect();
        assert_eq!(
            check_preview_batch(&files),
            Err(PayloadError::TooMany {
                what: "files in one preview request",
                count: MAX_PREVIEW_ENTRIES + 1,
                limit: MAX_PREVIEW_ENTRIES
            })
        );
    }

    #[test]
    fn the_error_renders_the_size_and_the_limit_at_the_ipc_edge() {
        let text: String = PayloadError::TooLarge {
            what: "file content",
            bytes: 9,
            limit: 8,
        }
        .into();
        assert_eq!(text, "file content is too large: 9 bytes, limit 8 bytes");
    }
}
