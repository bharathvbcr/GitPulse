use serde::{Deserialize, Serialize};
use std::path::Path;

fn truncate_for_error(s: &str) -> String {
    const MAX: usize = 60;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX).collect();
    format!("{head}…")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLineType {
    Context,
    Addition,
    Deletion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiedDiffLine {
    pub line_type: DiffLineType,
    pub old_line_no: Option<u32>,
    pub new_line_no: Option<u32>,
    pub content: String,
    pub is_selected: bool, // Used for partial/single-line staging
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiedDiffHunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub header: String,
    pub lines: Vec<UnifiedDiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatch {
    pub old_path: String,
    pub new_path: String,
    pub hunks: Vec<UnifiedDiffHunk>,
}

pub struct PatchBuilder;

impl PatchBuilder {
    /// Validates a [`FilePatch`] before its contents are interpolated into
    /// unified-diff headers and hunks.
    ///
    /// Paths and line contents are written verbatim into the patch text that
    /// `git apply --cached` parses, so a crafted value could smuggle extra
    /// header lines (`--- a/…`) or break hunk arithmetic. Absolute paths,
    /// traversal segments, backslashes, NUL bytes and embedded newlines are
    /// therefore rejected here — once, at the single owner of the rules.
    pub fn validate_file_patch(file_patch: &FilePatch) -> Result<(), String> {
        for (role, path) in [
            ("&old_path", &file_patch.old_path),
            ("&new_path", &file_patch.new_path),
        ] {
            Self::validate_patch_path(role, path)?;
        }
        for hunk in &file_patch.hunks {
            for line in &hunk.lines {
                if line.content.contains('\n') || line.content.contains('\r') {
                    return Err(format!(
                        "diff line content contains a line break; staging needs one \
                         logical line per entry (got {:?})",
                        truncate_for_error(&line.content)
                    ));
                }
                if line.content.contains('\0') {
                    return Err("diff line content contains a NUL byte".to_string());
                }
            }
        }
        Ok(())
    }

    fn validate_patch_path(role: &str, path: &str) -> Result<(), String> {
        if path.is_empty() {
            return Err(format!("diff {} path is empty", role));
        }
        if path.contains('\0') {
            return Err(format!("diff {} path contains a NUL byte", role));
        }
        if path.contains('\\') || path.contains('\n') || path.contains('\r') {
            return Err(format!(
                "diff {} path contains a backslash or line break: {:?}",
                role,
                truncate_for_error(path)
            ));
        }
        if path.starts_with('/') {
            return Err(format!(
                "diff {} path must be relative, got '{}'",
                role, path
            ));
        }
        if Path::new(path)
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(format!(
                "diff {} path escapes the repository via '..': '{}'",
                role, path
            ));
        }
        Ok(())
    }

    /// Builds a standard Git unified diff patch applying ONLY the selected lines in the hunk.
    /// This patch is directly applicable to Git's index via `git apply --cached`.
    ///
    /// Callers reachable from the UI must pass the patch through
    /// [`PatchBuilder::validate_file_patch`] first; this builder itself only
    /// formats what it is given.
    pub fn build_selective_patch(file_patch: &FilePatch, is_staging: bool) -> String {
        let mut patch_buffer = String::new();

        patch_buffer.push_str(&format!("--- a/{}\n", file_patch.old_path));
        patch_buffer.push_str(&format!("+++ b/{}\n", file_patch.new_path));

        for hunk in &file_patch.hunks {
            let mut hunk_lines_out = Vec::new();
            let mut old_count = 0;
            let mut new_count = 0;

            for line in &hunk.lines {
                match line.line_type {
                    DiffLineType::Context => {
                        hunk_lines_out.push(format!(" {}", line.content));
                        old_count += 1;
                        new_count += 1;
                    }
                    DiffLineType::Addition => {
                        if line.is_selected {
                            if is_staging {
                                hunk_lines_out.push(format!("+{}", line.content));
                                new_count += 1;
                            } else {
                                // For unstaging an addition: it becomes a deletion in the reverse patch
                                hunk_lines_out.push(format!("-{}", line.content));
                                old_count += 1;
                            }
                        } else if is_staging {
                            // Skipped addition: keep in original working tree, do not add to index
                        } else {
                            // Unstaging skipped addition: treat as unchanged context in the reverse index
                            hunk_lines_out.push(format!(" {}", line.content));
                            old_count += 1;
                            new_count += 1;
                        }
                    }
                    DiffLineType::Deletion => {
                        if line.is_selected {
                            if is_staging {
                                hunk_lines_out.push(format!("-{}", line.content));
                                old_count += 1;
                            } else {
                                // For unstaging a deletion: restore it
                                hunk_lines_out.push(format!("+{}", line.content));
                                new_count += 1;
                            }
                        } else if is_staging {
                            // Skipped deletion: keep the line as context in the staged patch
                            hunk_lines_out.push(format!(" {}", line.content));
                            old_count += 1;
                            new_count += 1;
                        } else {
                            // Unstaging skipped deletion: leave deleted
                        }
                    }
                }
            }

            if !hunk_lines_out.is_empty() {
                let start_old = hunk.old_start;
                let start_new = hunk.new_start;
                patch_buffer.push_str(&format!(
                    "@@ -{},{} +{},{} @@\n",
                    start_old, old_count, start_new, new_count
                ));
                for hl in hunk_lines_out {
                    patch_buffer.push_str(&hl);
                    patch_buffer.push('\n');
                }
            }
        }

        patch_buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stage_single_line_addition() {
        let file_patch = FilePatch {
            old_path: "src/main.rs".to_string(),
            new_path: "src/main.rs".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 2,
                new_start: 1,
                new_lines: 4,
                header: "".to_string(),
                lines: vec![
                    UnifiedDiffLine {
                        line_type: DiffLineType::Context,
                        old_line_no: Some(1),
                        new_line_no: Some(1),
                        content: "fn main() {".to_string(),
                        is_selected: false,
                    },
                    UnifiedDiffLine {
                        line_type: DiffLineType::Addition,
                        old_line_no: None,
                        new_line_no: Some(2),
                        content: "    println!(\"First\");".to_string(),
                        is_selected: true, // Stage this addition
                    },
                    UnifiedDiffLine {
                        line_type: DiffLineType::Addition,
                        old_line_no: None,
                        new_line_no: Some(3),
                        content: "    println!(\"Second\");".to_string(),
                        is_selected: false, // Do not stage this addition
                    },
                    UnifiedDiffLine {
                        line_type: DiffLineType::Context,
                        old_line_no: Some(2),
                        new_line_no: Some(4),
                        content: "}".to_string(),
                        is_selected: false,
                    },
                ],
            }],
        };

        let patch = PatchBuilder::build_selective_patch(&file_patch, true);
        assert!(patch.contains("--- a/src/main.rs"));
        assert!(patch.contains("+++ b/src/main.rs"));
        assert!(patch.contains("+    println!(\"First\");"));
        assert!(!patch.contains("+    println!(\"Second\");"));
    }

    fn line(t: DiffLineType, content: &str, selected: bool) -> UnifiedDiffLine {
        UnifiedDiffLine {
            line_type: t,
            old_line_no: None,
            new_line_no: None,
            content: content.to_string(),
            is_selected: selected,
        }
    }

    /// Documents the hazard the validation closes: a newline inside one
    /// logical diff line is emitted verbatim, so the patch text gains extra
    /// physical lines that no hunk header accounts for.
    #[test]
    fn unvalidated_newline_content_corrupts_patch_shape() {
        let file_patch = FilePatch {
            old_path: "src/a.rs".to_string(),
            new_path: "src/a.rs".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 2,
                header: String::new(),
                lines: vec![
                    line(DiffLineType::Addition, "alpha", true),
                    line(DiffLineType::Addition, "beta\ngamma", true),
                ],
            }],
        };
        let patch = PatchBuilder::build_selective_patch(&file_patch, true);
        // The newline is interpolated verbatim: one logical line becomes two
        // physical body lines, so the text no longer matches its own header
        // arithmetic (header promises 2 new lines; body carries 3).
        assert!(
            patch.contains("+beta\ngamma\n"),
            "content must be interpolated verbatim to show the hazard"
        );
        let body_physical = patch
            .lines()
            .filter(|l| !l.starts_with("---") && !l.starts_with("+++") && !l.starts_with("@@"))
            .count();
        assert_eq!(body_physical, 3, "newline smuggled an extra body line");
        assert!(PatchBuilder::validate_file_patch(&file_patch).is_err());
    }

    #[test]
    fn validate_rejects_unsafe_paths_in_both_roles() {
        let bad_paths = [
            "../../evil",
            "/absolute/path",
            "back\\slash.rs",
            "nul\0byte",
            "",
        ];
        for path in bad_paths {
            for role in [0, 1] {
                let fp = FilePatch {
                    old_path: if role == 0 {
                        path.to_string()
                    } else {
                        "ok.rs".to_string()
                    },
                    new_path: if role == 1 {
                        path.to_string()
                    } else {
                        "ok.rs".to_string()
                    },
                    hunks: Vec::new(),
                };
                let err = PatchBuilder::validate_file_patch(&fp)
                    .expect_err(&format!("path {path:?} (role {role}) must be rejected"));
                assert!(!err.is_empty());
            }
        }
    }

    #[test]
    fn validate_rejects_line_breaks_and_nul_in_content() {
        for evil in ["alpha\nbeta", "carriage\rreturn", "nul\0byte"] {
            let fp = FilePatch {
                old_path: "a.txt".to_string(),
                new_path: "a.txt".to_string(),
                hunks: vec![UnifiedDiffHunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    header: String::new(),
                    lines: vec![line(DiffLineType::Addition, evil, true)],
                }],
            };
            assert!(
                PatchBuilder::validate_file_patch(&fp).is_err(),
                "content {evil:?} must be rejected"
            );
        }
    }

    #[test]
    fn valid_multi_hunk_patch_still_builds() {
        let fp = FilePatch {
            old_path: "src/lib.rs".to_string(),
            new_path: "src/lib.rs".to_string(),
            hunks: vec![
                UnifiedDiffHunk {
                    old_start: 1,
                    old_lines: 2,
                    new_start: 1,
                    new_lines: 2,
                    header: String::new(),
                    lines: vec![
                        line(DiffLineType::Context, "fn a() {}", false),
                        line(DiffLineType::Addition, "fn b() {}", true),
                    ],
                },
                UnifiedDiffHunk {
                    old_start: 10,
                    old_lines: 2,
                    new_start: 11,
                    new_lines: 2,
                    header: String::new(),
                    lines: vec![
                        line(DiffLineType::Deletion, "fn gone() {}", true),
                        line(DiffLineType::Context, "fn stays() {}", false),
                    ],
                },
            ],
        };
        PatchBuilder::validate_file_patch(&fp).expect("valid multi-hunk patch");
        let patch = PatchBuilder::build_selective_patch(&fp, true);
        assert!(patch.contains("--- a/src/lib.rs"));
        // Hunk 1: 1 context + 1 selected addition. Hunk 2: 1 selected deletion
        // + 1 context.
        assert!(patch.contains("@@ -1,1 +1,2 @@"));
        assert!(patch.contains("@@ -10,2 +11,1 @@"));
        assert!(patch.contains("+fn b() {}"));
        assert!(patch.contains("-fn gone() {}"));
        assert!(patch.contains(" fn stays() {}"));
    }
}
