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
    /// True when git's source diff carried `\ No newline at end of file`
    /// directly after this line. The marker describes the CONTENT, not the
    /// sign: it must ride along wherever this line lands in the rebuilt
    /// patch (+/-/context), or `git apply` cannot match blobs whose final
    /// line lacks a trailing newline. Serde-defaulted so older payloads
    /// without the field keep deserializing.
    #[serde(default)]
    pub no_newline: bool,
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
    pub fn has_selected_lines(file_patch: &FilePatch) -> bool {
        file_patch.hunks.iter().any(|h| {
            h.lines.iter().any(|l| {
                l.is_selected
                    && matches!(l.line_type, DiffLineType::Addition | DiffLineType::Deletion)
            })
        })
    }

    /// Validates a [`FilePatch`] before its contents are interpolated into
    /// unified-diff headers and hunks.
    ///
    /// Paths and line contents are written verbatim into the patch text that
    /// `git apply --cached` parses, so a crafted value could smuggle extra
    /// header lines (`--- a/…`) or break hunk arithmetic. Absolute paths,
    /// traversal segments, backslashes, NUL bytes and embedded newlines are
    /// therefore rejected here — once, at the single owner of the rules.
    ///
    /// Carriage returns inside line content are ALLOWED: the frontend now
    /// preserves CR bytes deliberately so CRLF files stage byte-exactly (the
    /// CR rides inside the logical line; the builder's own `\n` terminates
    /// it). Only `\n` and NUL remain rejected.
    pub fn validate_file_patch(file_patch: &FilePatch) -> Result<(), String> {
        if !Self::has_selected_lines(file_patch) {
            return Err("No lines selected in patch".to_string());
        }
        for (role, path) in [
            ("old_path", &file_patch.old_path),
            ("new_path", &file_patch.new_path),
        ] {
            Self::validate_patch_path(role, path)?;
        }
        for hunk in &file_patch.hunks {
            for line in &hunk.lines {
                if line.content.contains('\n') {
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
        if path == "/dev/null" {
            return Ok(());
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

        patch_buffer.push_str(&unified_path_header("---", "a", &file_patch.old_path));
        patch_buffer.push_str(&unified_path_header("+++", "b", &file_patch.new_path));

        for hunk in &file_patch.hunks {
            let mut hunk_lines_out = Vec::new();
            let mut old_count = 0;
            let mut new_count = 0;
            // The `\ No newline at end of file` marker attaches to the line
            // content in every direction git emits it, so a row flagged by the
            // parser keeps its marker whichever sign this selection turns it
            // into. Emitting it only "when staging" or only for additions
            // would desync preimage/postimage blob endings and fail apply.
            fn render(prefix: char, line: &UnifiedDiffLine) -> String {
                if line.no_newline {
                    format!("{prefix}{}\n\\ No newline at end of file", line.content)
                } else {
                    format!("{prefix}{}", line.content)
                }
            }

            for line in &hunk.lines {
                match line.line_type {
                    DiffLineType::Context => {
                        hunk_lines_out.push(render(' ', line));
                        old_count += 1;
                        new_count += 1;
                    }
                    DiffLineType::Addition => {
                        if line.is_selected {
                            if is_staging {
                                hunk_lines_out.push(render('+', line));
                                new_count += 1;
                            } else {
                                // For unstaging an addition: it becomes a deletion in the reverse patch
                                hunk_lines_out.push(render('-', line));
                                old_count += 1;
                            }
                        } else if is_staging {
                            // Skipped addition: keep in original working tree, do not add to index
                        } else {
                            // Unstaging skipped addition: treat as unchanged context in the reverse index
                            hunk_lines_out.push(render(' ', line));
                            old_count += 1;
                            new_count += 1;
                        }
                    }
                    DiffLineType::Deletion => {
                        if line.is_selected {
                            if is_staging {
                                hunk_lines_out.push(render('-', line));
                                old_count += 1;
                            } else {
                                // For unstaging a deletion: restore it
                                hunk_lines_out.push(render('+', line));
                                new_count += 1;
                            }
                        } else if is_staging {
                            // Skipped deletion: keep the line as context in the staged patch
                            hunk_lines_out.push(render(' ', line));
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

/// `--- a/path` / `+++ b/path`, except `/dev/null` which git expects unprefixed.
fn unified_path_header(marker: &str, prefix: &str, path: &str) -> String {
    if path == "/dev/null" {
        format!("{marker} /dev/null\n")
    } else {
        format!("{marker} {prefix}/{path}\n")
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
                        no_newline: false,
                    },
                    UnifiedDiffLine {
                        line_type: DiffLineType::Addition,
                        old_line_no: None,
                        new_line_no: Some(2),
                        content: "    println!(\"First\");".to_string(),
                        is_selected: true, // Stage this addition
                        no_newline: false,
                    },
                    UnifiedDiffLine {
                        line_type: DiffLineType::Addition,
                        old_line_no: None,
                        new_line_no: Some(3),
                        content: "    println!(\"Second\");".to_string(),
                        is_selected: false, // Do not stage this addition
                        no_newline: false,
                    },
                    UnifiedDiffLine {
                        line_type: DiffLineType::Context,
                        old_line_no: Some(2),
                        new_line_no: Some(4),
                        content: "}".to_string(),
                        is_selected: false,
                        no_newline: false,
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
            no_newline: false,
        }
    }

    fn line_no_newline(t: DiffLineType, content: &str, selected: bool) -> UnifiedDiffLine {
        UnifiedDiffLine {
            no_newline: true,
            ..line(t, content, selected)
        }
    }

    const MARKER: &str = "\\ No newline at end of file";

    /// The EOF marker rides the CONTENT, not the sign: whichever direction
    /// staging/unstaging transforms a flagged row into, the rebuilt patch must
    /// carry the marker so `git apply` matches blob endings byte-exactly.
    #[test]
    fn no_newline_marker_survives_every_direction() {
        let cases = [
            (DiffLineType::Addition, true, true, "+last"),
            (DiffLineType::Addition, true, false, "-last"),
            (DiffLineType::Deletion, true, true, "-last"),
            (DiffLineType::Deletion, true, false, "+last"),
            (DiffLineType::Context, false, true, " last"),
            (DiffLineType::Context, false, false, " last"),
        ];
        for (kind, selected, staging, expected_prefix_line) in cases {
            let fp = FilePatch {
                old_path: "f.txt".to_string(),
                new_path: "f.txt".to_string(),
                hunks: vec![UnifiedDiffHunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    header: String::new(),
                    lines: vec![line_no_newline(kind.clone(), "last", selected)],
                }],
            };
            let patch = PatchBuilder::build_selective_patch(&fp, staging);
            assert!(
                patch.contains(&format!("{expected_prefix_line}\n{MARKER}")),
                "kind={kind:?} selected={selected} staging={staging} lost its marker:\n{patch}"
            );
        }
    }

    /// A flagged row that the selection SKIPS must not leak its marker into
    /// the context line that replaces it... unless it stays context (both
    /// sides keep the missing newline), which is the one skip that renders.
    #[test]
    fn no_newline_marker_follows_rendered_context_on_skip() {
        // Staging skips additions entirely (no output) — nothing to leak.
        let staged_skip = FilePatch {
            old_path: "f.txt".to_string(),
            new_path: "f.txt".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                header: String::new(),
                lines: vec![line_no_newline(DiffLineType::Addition, "tail", false)],
            }],
        };
        assert!(!PatchBuilder::build_selective_patch(&staged_skip, true).contains(MARKER));

        // Unstaging turns the skipped addition into context — both sides of
        // that context still end without a newline, so the marker MUST stay.
        let unstaged_ctx = PatchBuilder::build_selective_patch(&staged_skip, false);
        assert!(
            unstaged_ctx.contains(&format!(" tail\n{MARKER}")),
            "context replacing a flagged row keeps the marker:\n{unstaged_ctx}"
        );

        // Staging a skipped deletion renders context too — marker stays.
        let deletion_skip = FilePatch {
            old_path: "f.txt".to_string(),
            new_path: "f.txt".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                header: String::new(),
                lines: vec![line_no_newline(DiffLineType::Deletion, "tail", false)],
            }],
        };
        let staged_ctx = PatchBuilder::build_selective_patch(&deletion_skip, true);
        assert!(
            staged_ctx.contains(&format!(" tail\n{MARKER}")),
            "context from a skipped flagged deletion keeps the marker:\n{staged_ctx}"
        );
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
                    hunks: vec![UnifiedDiffHunk {
                        old_start: 1,
                        old_lines: 0,
                        new_start: 1,
                        new_lines: 1,
                        header: String::new(),
                        lines: vec![line(DiffLineType::Addition, "ok", true)],
                    }],
                };
                let err = PatchBuilder::validate_file_patch(&fp)
                    .expect_err(&format!("path {path:?} (role {role}) must be rejected"));
                assert!(!err.is_empty());
            }
        }
    }

    #[test]
    fn validate_rejects_line_breaks_and_nul_in_content() {
        for evil in ["alpha\nbeta", "nul\0byte"] {
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

    /// Regression (audit E): CR bytes inside line content were rejected
    /// outright, making partial staging impossible for legitimate CRLF files.
    /// The frontend preserves CR bytes deliberately, so a trailing `\r` on a
    /// logical line must validate, survive `build_selective_patch`, and land
    /// in the patch text as `+...\r\n` — one physical line per logical line,
    /// with hunk arithmetic intact. Bare `\n` and NUL stay rejected.
    #[test]
    fn crlf_content_is_accepted_end_to_end() {
        let crlf_lines = ["fn first() {\r", "}\r"];
        let lines: Vec<UnifiedDiffLine> =
            std::iter::once(line(DiffLineType::Context, "ctx", false))
                .chain(
                    crlf_lines
                        .iter()
                        .map(|c| line(DiffLineType::Addition, c, true)),
                )
                .collect();
        let fp = FilePatch {
            old_path: "win.rs".to_string(),
            new_path: "win.rs".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 3,
                header: String::new(),
                lines,
            }],
        };
        PatchBuilder::validate_file_patch(&fp).expect("CRLF content must pass validation");
        let patch = PatchBuilder::build_selective_patch(&fp, true);
        assert!(patch.contains("+fn first() {\r\n"), "got:\n{patch:?}");
        assert!(patch.contains("+}\r\n"), "got:\n{patch:?}");
        // One physical body line per logical line: the embedded CR must not
        // have split anything.
        let body_physical = patch
            .lines()
            .filter(|l| !l.starts_with("---") && !l.starts_with("+++") && !l.starts_with("@@"))
            .count();
        assert_eq!(body_physical, 3, "context + two CRLF additions");

        // A lone \r without \n is equally legal content.
        let lone_cr = FilePatch {
            old_path: "a.txt".to_string(),
            new_path: "a.txt".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                header: String::new(),
                lines: vec![line(DiffLineType::Addition, "carriage\rreturn", true)],
            }],
        };
        PatchBuilder::validate_file_patch(&lone_cr)
            .expect("lone CR inside content is not a line break");
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

    #[test]
    fn validate_rejects_empty_selection_before_emitting_headers() {
        let fp = FilePatch {
            old_path: "a.txt".to_string(),
            new_path: "a.txt".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                header: String::new(),
                lines: vec![line(DiffLineType::Addition, "x", false)],
            }],
        };
        let err = PatchBuilder::validate_file_patch(&fp).expect_err("empty selection");
        assert!(
            err.to_lowercase().contains("no lines selected"),
            "got: {err}"
        );
        let built = PatchBuilder::build_selective_patch(&fp, true);
        assert!(
            !built.contains("@@"),
            "empty selection must not emit a hunk that git apply would reject"
        );
    }

    #[test]
    fn validate_allows_dev_null_for_new_and_deleted_files() {
        let created = FilePatch {
            old_path: "/dev/null".to_string(),
            new_path: "fresh.txt".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 0,
                old_lines: 0,
                new_start: 1,
                new_lines: 1,
                header: String::new(),
                lines: vec![line(DiffLineType::Addition, "hello", true)],
            }],
        };
        PatchBuilder::validate_file_patch(&created).expect("new-file /dev/null must be allowed");
        let patch = PatchBuilder::build_selective_patch(&created, true);
        assert!(patch.contains("--- /dev/null\n"), "got:\n{patch}");
        assert!(patch.contains("+++ b/fresh.txt\n"), "got:\n{patch}");
        assert!(
            !patch.contains("--- a//dev/null"),
            "must not prefix /dev/null"
        );

        let deleted = FilePatch {
            old_path: "gone.txt".to_string(),
            new_path: "/dev/null".to_string(),
            hunks: vec![UnifiedDiffHunk {
                old_start: 1,
                old_lines: 1,
                new_start: 0,
                new_lines: 0,
                header: String::new(),
                lines: vec![line(DiffLineType::Deletion, "bye", true)],
            }],
        };
        PatchBuilder::validate_file_patch(&deleted)
            .expect("deleted-file /dev/null must be allowed");
        let patch = PatchBuilder::build_selective_patch(&deleted, true);
        assert!(patch.contains("--- a/gone.txt\n"), "got:\n{patch}");
        assert!(patch.contains("+++ /dev/null\n"), "got:\n{patch}");
    }
}
