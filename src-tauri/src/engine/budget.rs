//! Payload budgets for everything that crosses the IPC boundary.
//!
//! # Why these exist
//!
//! The git engine's [`MAX_OUTPUT_BYTES`](crate::engine::git_cli::MAX_OUTPUT_BYTES)
//! is a 64 MiB backstop against a runaway process. It is not a sane payload
//! size, and treating it as one is how a "lightweight" client ends up holding
//! well over a gigabyte. Measured, on a single real commit that rewrote a
//! 400k-line file:
//!
//! | stage                                   | cost     |
//! |-----------------------------------------|----------|
//! | `git show` output                       | 43.7 MB  |
//! | + lossy `String` copy in this process   | 90 MB    |
//! | + `serde_json` serialization for IPC    | 144 MB   |
//! | webview string + 533k parsed row objects| 330 MB   |
//!
//! ~475 MB, for one click, on one commit — and the viewer renders at most
//! 300k rows of it anyway. Two such selections is the 1.5 GB the app was
//! reported at.
//!
//! # The rule
//!
//! Every payload whose size is driven by repository content, not by a fixed
//! schema, declares its budget here. A budget is chosen from what the
//! receiving surface can actually render, not from what git can emit.
//!
//! # Truncation is data, never silence
//!
//! Hitting a budget is never an error and never invisible. The payload carries
//! a `truncated` flag, the UI says so, and any action whose correctness needs
//! the whole payload (staging a hunk from a cut-off diff) is disabled rather
//! than allowed to act on a prefix. A partial result presented as complete is
//! the failure this module exists to prevent.

/// Diff text for one file, one commit, or one range.
///
/// 8 MiB is roughly 100k lines of ordinary source — well past the point a
/// human reads a diff, and comfortably under the viewer's own 300k-row render
/// cap, so nothing renderable is lost to it.
pub const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;

/// `git blame --line-porcelain` output for one file.
///
/// Porcelain emits ~10 metadata lines per source line, so this is deliberately
/// larger than the file-content budget for the same file: 16 MiB of porcelain
/// is roughly a 40k-line file, past which the blame gutter is not usable.
pub const MAX_BLAME_BYTES: usize = 16 * 1024 * 1024;

/// One working-tree or blob file loaded into the viewer.
///
/// Binary content is base64-expanded ~1.33x before it crosses IPC, so the wire
/// cost is ~10.7 MiB at this bound. The previous 64 MiB ceiling allowed an
/// 85 MiB IPC message for a file no editor pane can display.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Reflog entries shipped in one payload.
pub const MAX_REFLOG_BYTES: usize = 4 * 1024 * 1024;

/// `git log --numstat` walk that feeds the Pulse view.
///
/// 16 MiB is thousands of ordinary commits with path-level churn. Past that
/// the parser would still finish, but the IPC payload and the derived
/// heatmap would stall the UI. Truncation is a flag, never an error: the
/// Scan Deeper control raises the commit cap, not this byte budget.
pub const MAX_PULSE_BYTES: usize = 16 * 1024 * 1024;

/// Cut `text` to at most `cap` bytes, ending on a line boundary.
///
/// A diff cut mid-line renders as a corrupt row and, worse, can parse as a
/// *valid* one — a truncated `-foo` line is indistinguishable from a real
/// deletion of a shorter string. Cutting at the last newline keeps every row
/// that survives an honest row. Returns the text and whether anything was
/// dropped.
///
/// When the first line alone exceeds `cap` there is no newline to cut at; the
/// text is cut at the last character boundary instead and still reported as
/// truncated, because returning nothing would lose the only content there is.
pub fn truncate_at_line_boundary(text: String, cap: usize) -> (String, bool) {
    if text.len() <= cap {
        return (text, false);
    }
    let head = &text[..char_boundary_at_or_before(&text, cap)];
    let cut = match head.rfind('\n') {
        // Keep the newline: the parser drops exactly one trailing empty
        // element, so a cut that ends mid-file still ends cleanly.
        Some(index) => index + 1,
        None => head.len(),
    };
    let mut out = text;
    out.truncate(cut);
    (out, true)
}

/// Drops a trailing partial line from text the engine already cut at an
/// arbitrary byte offset.
///
/// [`truncate_at_line_boundary`] cannot do this job: by the time the drain cap
/// has fired the text is already at or under the budget, so a length-based
/// check sees nothing to trim and leaves the half-line — and a lossy UTF-8
/// conversion of a mid-character cut leaves a replacement char there too. A
/// half `-foo` row in a diff is not visibly broken; it reads as a real
/// deletion of a shorter string, and a half porcelain record parses into a
/// `BlameLine` with fabricated fields.
///
/// Text with no newline at all is returned unchanged: there is no boundary to
/// cut back to, and dropping everything would turn a truncated payload into an
/// empty one — which reads as "nothing changed".
pub fn drop_partial_last_line(text: String) -> String {
    match text.rfind('\n') {
        Some(index) => {
            let mut out = text;
            out.truncate(index + 1);
            out
        }
        None => text,
    }
}

/// Largest index <= `cap` that is a UTF-8 character boundary in `text`.
fn char_boundary_at_or_before(text: &str, cap: usize) -> usize {
    let mut index = cap.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_within_budget_is_returned_untouched() {
        let (out, truncated) = truncate_at_line_boundary("a\nb\n".to_string(), 64);
        assert_eq!(out, "a\nb\n");
        assert!(!truncated);
    }

    #[test]
    fn a_cut_lands_on_a_line_boundary_and_is_reported() {
        let text = "aaaa\nbbbb\ncccc\n".to_string();
        let (out, truncated) = truncate_at_line_boundary(text, 12);
        assert!(truncated);
        assert_eq!(out, "aaaa\nbbbb\n", "must not keep a partial line");
        assert!(out.ends_with('\n'));
    }

    /// A cut that split a multi-byte character would panic on the slice and,
    /// if it did not, would emit replacement garbage into the viewer.
    #[test]
    fn a_cut_never_splits_a_multibyte_character() {
        // Each '\u{1F600}' is 4 bytes; the cap deliberately lands inside one.
        let text = "😀😀😀😀\ntail\n".to_string();
        for cap in 1..text.len() {
            let (out, truncated) = truncate_at_line_boundary(text.clone(), cap);
            assert!(truncated || out == text);
            assert!(
                std::str::from_utf8(out.as_bytes()).is_ok(),
                "cap {cap} produced invalid UTF-8"
            );
            assert!(out.len() <= cap || !truncated);
        }
    }

    /// A single line longer than the whole budget has no newline to cut at.
    /// Returning an empty string would report "truncated" while showing the
    /// user nothing at all.
    #[test]
    fn a_single_oversized_line_is_cut_rather_than_dropped() {
        let text = format!("{}\n", "x".repeat(1000));
        let (out, truncated) = truncate_at_line_boundary(text, 100);
        assert!(truncated);
        assert_eq!(out.len(), 100);
        assert!(out.chars().all(|c| c == 'x'));
    }

    #[test]
    fn an_empty_input_and_a_zero_cap_are_both_handled() {
        assert_eq!(
            truncate_at_line_boundary(String::new(), 0),
            (String::new(), false)
        );
        let (out, truncated) = truncate_at_line_boundary("abc".to_string(), 0);
        assert!(truncated);
        assert!(out.is_empty());
    }

    #[test]
    fn a_cut_exactly_at_the_length_keeps_everything() {
        let text = "abcdef".to_string();
        let (out, truncated) = truncate_at_line_boundary(text.clone(), text.len());
        assert_eq!(out, text);
        assert!(!truncated);
    }

    #[test]
    fn a_partial_last_line_is_dropped_after_an_engine_cut() {
        assert_eq!(
            drop_partial_last_line("+aaa\n+bbb\n+cc".to_string()),
            "+aaa\n+bbb\n"
        );
        assert_eq!(
            drop_partial_last_line("+aaa\n+bbb\n".to_string()),
            "+aaa\n+bbb\n"
        );
    }

    /// Dropping everything would turn "truncated" into "empty", which reads as
    /// "this commit changed nothing".
    #[test]
    fn text_with_no_newline_survives_the_partial_line_drop() {
        assert_eq!(
            drop_partial_last_line("no newline".to_string()),
            "no newline"
        );
        assert_eq!(drop_partial_last_line(String::new()), "");
    }

    /// A byte-level cut through a multi-byte character leaves a replacement
    /// char after lossy conversion; the trim must remove the whole row it sits
    /// on rather than leave it on screen.
    #[test]
    fn a_replacement_char_from_a_mid_character_cut_is_trimmed_away() {
        let raw = "+ok\n+\u{1F600}";
        let bytes = &raw.as_bytes()[..raw.len() - 2];
        let lossy = String::from_utf8_lossy(bytes).into_owned();
        assert!(
            lossy.contains('\u{FFFD}'),
            "setup: expected a replacement char"
        );
        let trimmed = drop_partial_last_line(lossy);
        assert_eq!(trimmed, "+ok\n");
        assert!(!trimmed.contains('\u{FFFD}'));
    }

    /// The budgets themselves: each must be small enough that the whole
    /// pipeline (bytes + lossy copy + JSON + webview string + parsed rows,
    /// empirically ~11x the raw bytes) stays well under a gigabyte.
    #[test]
    fn every_budget_leaves_the_pipeline_under_a_gigabyte() {
        const PIPELINE_MULTIPLIER: usize = 11;
        const CEILING: usize = 1024 * 1024 * 1024;
        for (name, bytes) in [
            ("diff", MAX_DIFF_BYTES),
            ("blame", MAX_BLAME_BYTES),
            ("file", MAX_FILE_BYTES as usize),
            ("reflog", MAX_REFLOG_BYTES),
        ] {
            assert!(
                bytes * PIPELINE_MULTIPLIER < CEILING,
                "{name} budget of {bytes} bytes costs ~{} MB end to end",
                bytes * PIPELINE_MULTIPLIER / 1_000_000
            );
            assert!(bytes > 0, "{name} budget must admit some content");
        }
    }
}
