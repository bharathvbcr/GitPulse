//! Credential redaction on the ledger's write path.
//!
//! This module is deliberately thin. It delegates to `dc_verify::rigor`, whose
//! `SECRET_PATTERNS` table is documented as *the* single seam through which
//! every consumer decides what counts as a credential — the harness's secret
//! gate, its evidence builder, and now this.
//!
//! The alternative was to copy the pattern table into GitPulse. That is how a
//! key redacted by one gate leaks out of another: two tables drift, and the
//! one that drifts is discovered by the leak.

/// Redacts every credential-shaped token in `text`.
///
/// Applied to `argv_json`, `detail_json` and `object` before insert. The
/// prefix survives so a row stays identifiable — an operator can still see
/// that a GitHub token was involved — while the secret itself does not reach
/// the disk.
pub fn text(value: &str) -> String {
    dc_verify::rigor::redact_secrets(value)
}

/// Whether `text` carries anything the secret gate would stop.
///
/// Used where a value must be *refused* rather than stored redacted: a field
/// that cannot be safely stored is not the same as one that was stored safely.
pub fn carries_secret(value: &str) -> bool {
    dc_verify::rigor::contains_secret(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";

    #[test]
    fn redacts_through_the_harness_pattern_table() {
        let out = text(&format!("git push https://{KEY}@github.com/o/r"));
        assert!(!out.contains(KEY));
        assert!(out.contains("ghp_"), "the shape stays identifiable: {out}");
    }

    #[test]
    fn ordinary_command_lines_are_returned_unchanged() {
        // The property that keeps this on the write path: a redactor that
        // mangles innocent argv gets turned off, and then nothing is redacted.
        for ordinary in [
            r#"["git","commit","-m","fix the task-runner"]"#,
            r#"["git","rebase","--continue"]"#,
            r#"["cargo","test","--workspace"]"#,
        ] {
            assert_eq!(text(ordinary), ordinary);
        }
    }

    #[test]
    fn detection_and_redaction_agree() {
        assert!(carries_secret(KEY));
        assert!(!carries_secret("git status"));
        // Anything reported as carrying a secret must actually change.
        assert_ne!(text(KEY), KEY);
    }

    #[test]
    fn redacting_twice_changes_nothing_the_second_time() {
        let once = text(KEY);
        assert_eq!(text(&once), once, "a credential survived the first pass");
        assert!(!carries_secret(&once));
    }
}
