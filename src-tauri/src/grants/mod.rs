//! Read-only view of the harness's grant ledger.
//!
//! A grant is how a soft denial becomes an allow: someone argued for a write
//! the plan did not authorise, and the gate recorded that they did. Until now
//! GitPulse rendered the *result* — a verdict whose status is `granted` — with
//! no way to see who granted it, why, or when it expires.
//!
//! # Why a file read and not a serve op
//!
//! The migration plan proposed `grants.list` / `grants.revoke` as new
//! operations on the `manvi serve` protocol. Reading the source says otherwise
//! on both counts:
//!
//! * The serve plane builds `policy.FileGate` and `policy.CommandGate`
//!   directly, never `gate.Gate`, and the grant ledger lives on `gate.Gate`.
//!   A `grants.list` op served from that process would have no ledger to read
//!   and would return an empty list forever — which is exactly the failure this
//!   codebase exists to avoid: "nothing granted" and "this plane has no grants
//!   model" rendering identically.
//! * `serve` deliberately imports nothing from `grants`. Adding the op would
//!   cross that boundary for data already sitting in a file.
//!
//! Manvi persists the ledger to `<repo>/.devcouncil/harness-grants.json`, which
//! is the same per-repository convention as `state.sqlite` and the event
//! ledger. Reading it needs no process, no protocol change, and no boundary
//! crossing.
//!
//! # Revocation is deliberately absent
//!
//! Revoking mutates state Manvi owns, and GitPulse is a read-only consumer of
//! it — the same rule that keeps this process from taking a task lease. A
//! second writer to the grant ledger could interleave with the harness's own
//! `saveGrants`, which serialises its writes behind a mutex this process cannot
//! share. Revocation belongs to Manvi, through its CLI or a future op that
//! Manvi itself serves.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where the harness keeps its grant ledger for a repository.
///
/// Mirrors `grantLedgerPath()` in `cmd/manvi/root.go`: the state directory is
/// `MANVI_STATE_DIR` when set, otherwise `<repo>/.devcouncil`.
pub fn grants_path(repo_path: &str) -> PathBuf {
    match std::env::var_os("MANVI_STATE_DIR") {
        Some(dir) if Path::new(&dir).is_absolute() => Path::new(&dir).join("harness-grants.json"),
        Some(dir) => Path::new(repo_path).join(dir).join("harness-grants.json"),
        None => Path::new(repo_path)
            .join(".devcouncil")
            .join("harness-grants.json"),
    }
}

/// Who issued a grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Grantor {
    #[serde(default)]
    pub authority: String,
    #[serde(default)]
    pub name: String,
}

/// What a grant covers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GrantScope {
    #[serde(default)]
    pub rule: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub task_id: String,
}

/// One recorded override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Grant {
    pub id: String,
    #[serde(default)]
    pub grantor: Grantor,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub scope: GrantScope,
    #[serde(default)]
    pub issued_at: String,
    #[serde(default)]
    pub expires_at: String,
    /// True once the grant has been spent on a decision.
    #[serde(default)]
    pub consumed: bool,
}

/// The grant ledger as GitPulse can see it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantView {
    /// False when this repository has no grant ledger — the ordinary case.
    pub available: bool,
    pub path: String,
    pub grants: Vec<Grant>,
    /// Empty when the ledger was read; otherwise why it could not be.
    ///
    /// Separate from `available` so a ledger that exists and could not be
    /// parsed never renders as a repository where nothing was ever granted.
    pub error: String,
}

/// Reads the grant ledger for a repository.
pub fn view(repo_path: &str) -> GrantView {
    let path = grants_path(repo_path);
    let shown = path.display().to_string();
    if !path.exists() {
        return GrantView {
            available: false,
            path: shown,
            grants: Vec::new(),
            error: String::new(),
        };
    }
    match std::fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<Vec<Grant>>(&raw) {
            Ok(grants) => GrantView {
                available: true,
                path: shown,
                grants,
                error: String::new(),
            },
            Err(e) => GrantView {
                available: true,
                path: shown,
                grants: Vec::new(),
                // A hand-edited or truncated ledger is reported, never treated
                // as an empty one. The harness itself refuses entries loudly on
                // load for the same reason.
                error: format!("grant ledger is unreadable: {e}"),
            },
        },
        Err(e) => GrantView {
            available: true,
            path: shown,
            grants: Vec::new(),
            error: format!("{e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_ledger(dir: &Path, body: &str) {
        let p = dir.join(".devcouncil");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("harness-grants.json"), body).unwrap();
    }

    #[test]
    fn a_repository_with_no_grants_is_unavailable_not_broken() {
        let dir = tempfile::tempdir().unwrap();
        let v = view(dir.path().to_str().unwrap());
        assert!(!v.available);
        assert!(v.error.is_empty(), "absence is not a failure");
        assert!(v.grants.is_empty());
    }

    #[test]
    fn reads_a_ledger_in_the_harness_format() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(
            dir.path(),
            r#"[{"id":"G-1",
                 "grantor":{"authority":"human","name":"bharath"},
                 "reason":"needed for the migration",
                 "scope":{"rule":"scope.unplanned","target":"src/a.rs","task_id":"TASK-1"},
                 "issued_at":"2026-09-01T12:00:00Z",
                 "expires_at":"2026-09-01T13:00:00Z",
                 "consumed":true}]"#,
        );
        let v = view(dir.path().to_str().unwrap());
        assert!(v.available, "{}", v.error);
        assert_eq!(v.grants.len(), 1);
        let g = &v.grants[0];
        assert_eq!(g.id, "G-1");
        assert_eq!(g.grantor.name, "bharath");
        assert_eq!(g.grantor.authority, "human");
        assert_eq!(g.scope.rule, "scope.unplanned");
        assert_eq!(g.reason, "needed for the migration");
        assert!(g.consumed);
    }

    #[test]
    fn an_unreadable_ledger_is_reported_not_shown_as_empty() {
        // The distinction that matters: a ledger we could not parse must never
        // render as a repository where nothing was ever granted.
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "{ not json ]");
        let v = view(dir.path().to_str().unwrap());
        assert!(v.available, "the file is there, so this is not absence");
        assert!(!v.error.is_empty());
        assert!(v.grants.is_empty());
    }

    #[test]
    fn an_empty_ledger_is_readable_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "[]");
        let v = view(dir.path().to_str().unwrap());
        assert!(v.available);
        assert!(v.error.is_empty());
        assert!(v.grants.is_empty());
    }

    #[test]
    fn missing_optional_fields_do_not_break_the_read() {
        // Forward compatibility: the harness may add fields, and an older
        // GitPulse must still show the grants rather than reporting the whole
        // ledger unreadable.
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), r#"[{"id":"G-2","future_field":42}]"#);
        let v = view(dir.path().to_str().unwrap());
        assert!(v.error.is_empty(), "{}", v.error);
        assert_eq!(v.grants.len(), 1);
        assert_eq!(v.grants[0].id, "G-2");
        assert!(!v.grants[0].consumed);
    }

    #[test]
    fn the_path_follows_the_harness_state_directory_convention() {
        let p = grants_path("/repo");
        assert!(p.ends_with("harness-grants.json"));
        assert!(p.to_string_lossy().contains(".devcouncil"));
    }
}
