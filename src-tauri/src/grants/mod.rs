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
//! share. Manvi has an internal ledger revocation method, but its current CLI
//! and serve protocol expose no revocation command; GitPulse therefore reports
//! that limitation and the ledger's location without offering an unsafe second
//! write path.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
    pub id: String,
}

/// What a grant covers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GrantScope {
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub once: bool,
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

/// The shared verdict contract is the canonical rule/severity table for every
/// GitPulse consumer. MANVI treats a rule absent from this table as hard, so a
/// future or misspelled rule can never become grantable by being unknown here.
fn rule_severities() -> Result<&'static HashMap<String, String>, String> {
    static SEVERITIES: OnceLock<Result<HashMap<String, String>, String>> = OnceLock::new();
    match SEVERITIES.get_or_init(|| {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../contracts/verdict.schema.json"))
                .map_err(|error| format!("shared verdict contract is unreadable: {error}"))?;
        let properties = schema
            .pointer("/$defs/severityByRule/properties")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "shared verdict contract has no rule severity table".to_string())?;
        let mut severities = HashMap::with_capacity(properties.len());
        for (rule, descriptor) in properties {
            let severity = descriptor
                .get("const")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("shared verdict contract has no severity for {rule}"))?;
            severities.insert(rule.clone(), severity.to_string());
        }
        Ok(severities)
    }) {
        Ok(severities) => Ok(severities),
        Err(error) => Err(error.clone()),
    }
}

/// Returns why MANVI's restore boundary would refuse the policy-independent
/// shape of this grant. Config-dependent checks (reason requirement, TTL
/// ceilings, and optional agent command grants) stay with MANVI, which owns
/// that live policy; these invariants hold under every configuration.
fn semantic_refusal(grant: &Grant, severities: &HashMap<String, String>) -> Option<String> {
    if grant.scope.rules.is_empty() {
        return Some("names no rules; a rule-less grant covers every soft rule".to_string());
    }
    for rule in &grant.scope.rules {
        match severities.get(rule).map(String::as_str) {
            Some("hard") => {
                return Some(format!(
                    "carries hard rule {rule}, which no grant may clear"
                ))
            }
            Some("soft" | "warn") => {}
            Some(severity) => {
                return Some(format!("rule {rule} has non-grantable severity {severity}"))
            }
            None => {
                return Some(format!(
                    "carries unknown rule {rule}, which is hard by default"
                ))
            }
        }
    }
    match grant.grantor.authority.as_str() {
        "agent" if grant.scope.task_id.trim().is_empty() => Some(
            "is an agent grant that names no task; an agent may only grant within its own task"
                .to_string(),
        ),
        "agent" | "human" => None,
        authority => Some(format!("has unknown authority {authority:?}")),
    }
}

/// Reads the grant ledger for a repository.
pub fn view(repo_path: &str) -> GrantView {
    let path = grants_path(repo_path);
    let shown = path.display().to_string();
    // A relative MANVI_STATE_DIR is repository-controlled; an absolute one is
    // explicit operator configuration and intentionally allowed outside the
    // checkout. Both still reject a symlink at the state directory or file.
    let repo_root = match std::env::var_os("MANVI_STATE_DIR") {
        Some(dir) if Path::new(&dir).is_absolute() => None,
        _ => Some(PathBuf::from(repo_path)),
    };
    match crate::ledger::read_checked_state_file(&path, repo_root.as_deref()) {
        Ok(None) => GrantView {
            available: false,
            path: shown,
            grants: Vec::new(),
            error: String::new(),
        },
        Ok(Some(raw)) => match serde_json::from_str::<Vec<Grant>>(&raw) {
            Ok(grants) => match rule_severities() {
                Ok(severities) => {
                    let mut accepted = Vec::with_capacity(grants.len());
                    let mut refused = Vec::new();
                    for grant in grants {
                        if let Some(why) = semantic_refusal(&grant, severities) {
                            refused.push(format!("{}: {why}", grant.id));
                        } else {
                            accepted.push(grant);
                        }
                    }
                    GrantView {
                        available: true,
                        path: shown,
                        grants: accepted,
                        error: if refused.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "grant ledger contains refused entries: {}",
                                refused.join("; ")
                            )
                        },
                    }
                }
                Err(error) => GrantView {
                    available: true,
                    path: shown,
                    grants: Vec::new(),
                    error,
                },
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
            error: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn real_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .status()
            .expect("run git init");
        assert!(status.success(), "git init failed");
        dir
    }

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
    fn reads_a_ledger_in_manvis_persisted_format() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), include_str!("fixtures/manvi-ledger.json"));
        let v = view(dir.path().to_str().unwrap());
        assert!(v.available, "{}", v.error);
        assert_eq!(v.grants.len(), 1);
        let g = &v.grants[0];
        assert_eq!(g.id, "G-1");
        assert_eq!(g.grantor.id, "bharath");
        assert_eq!(g.grantor.authority, "human");
        assert_eq!(g.scope.rules, ["scope.unplanned", "task.forbidden_change"]);
        assert_eq!(g.scope.paths, ["src/a.rs", "docs/**"]);
        assert_eq!(g.scope.task_id, "TASK-1");
        assert!(g.scope.once);
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
        write_ledger(
            dir.path(),
            r#"[{"id":"G-2",
                 "grantor":{"authority":"human","future_grantor_field":"still-compatible"},
                 "reason":"operator reviewed",
                 "scope":{"rules":["scope.unplanned"],"future_scope_field":{"version":2}},
                 "future_grant_field":42}]"#,
        );
        let v = view(dir.path().to_str().unwrap());
        assert!(v.error.is_empty(), "{}", v.error);
        assert_eq!(v.grants.len(), 1);
        assert_eq!(v.grants[0].id, "G-2");
        assert!(v.grants[0].grantor.id.is_empty());
        assert_eq!(v.grants[0].scope.rules, ["scope.unplanned"]);
        assert!(v.grants[0].scope.paths.is_empty());
        assert!(!v.grants[0].scope.once);
        assert!(!v.grants[0].consumed);
    }

    #[cfg(unix)]
    #[test]
    fn repository_state_symlink_cannot_redirect_the_grant_view() {
        use std::os::unix::fs::symlink;

        let repo = real_repo();
        let outside = tempfile::tempdir().expect("outside dir");
        std::fs::write(
            outside.path().join("harness-grants.json"),
            include_str!("fixtures/manvi-ledger.json"),
        )
        .expect("outside ledger");
        symlink(outside.path(), repo.path().join(".devcouncil")).expect("plant state symlink");

        let view = view(repo.path().to_str().unwrap());
        assert!(view.available, "an unsafe existing ledger is not absence");
        assert!(
            view.grants.is_empty(),
            "the repository redirected the reader to grants outside its boundary"
        );
        assert!(
            view.error.contains("symlink") || view.error.contains("outside"),
            "the unsafe state path was not diagnosed: {}",
            view.error
        );
    }

    #[cfg(unix)]
    #[test]
    fn grant_file_symlink_cannot_redirect_the_view() {
        use std::os::unix::fs::symlink;

        let repo = real_repo();
        let state = repo.path().join(".devcouncil");
        std::fs::create_dir(&state).expect("state dir");
        let outside = tempfile::NamedTempFile::new().expect("outside ledger");
        std::fs::write(outside.path(), include_str!("fixtures/manvi-ledger.json"))
            .expect("outside ledger body");
        symlink(outside.path(), state.join("harness-grants.json")).expect("plant ledger symlink");

        let view = view(repo.path().to_str().unwrap());
        assert!(view.available, "an unsafe existing ledger is not absence");
        assert!(view.grants.is_empty(), "a symlinked grant file was read");
        assert!(view.error.contains("symlink"), "{}", view.error);
    }

    #[test]
    fn semantically_invalid_grants_are_refused_like_manvi_restore() {
        let cases = [
            (
                "missing authority",
                r#"{"id":"G-1","grantor":{"id":"operator"},"reason":"r","scope":{"rules":["scope.unplanned"]}}"#,
                "authority",
            ),
            (
                "unknown authority",
                r#"{"id":"G-2","grantor":{"authority":"root","id":"operator"},"reason":"r","scope":{"rules":["scope.unplanned"]}}"#,
                "authority",
            ),
            (
                "empty rules",
                r#"{"id":"G-3","grantor":{"authority":"human","id":"operator"},"reason":"r","scope":{"rules":[]}}"#,
                "no rules",
            ),
            (
                "unscoped agent",
                r#"{"id":"G-4","grantor":{"authority":"agent","id":"executor"},"reason":"r","scope":{"rules":["scope.unplanned"]}}"#,
                "no task",
            ),
            (
                "blank agent task",
                r#"{"id":"G-4B","grantor":{"authority":"agent","id":"executor"},"reason":"r","scope":{"task_id":"   ","rules":["scope.unplanned"]}}"#,
                "no task",
            ),
            (
                "hard rule",
                r#"{"id":"G-5","grantor":{"authority":"human","id":"operator"},"reason":"r","scope":{"rules":["path.secret"]}}"#,
                "hard rule",
            ),
            (
                "unknown rule",
                r#"{"id":"G-6","grantor":{"authority":"human","id":"operator"},"reason":"r","scope":{"rules":["future.invented"]}}"#,
                "unknown rule",
            ),
        ];

        for (name, grant, diagnostic) in cases {
            let dir = tempfile::tempdir().unwrap();
            write_ledger(dir.path(), &format!("[{grant}]"));
            let view = view(dir.path().to_str().unwrap());
            assert!(view.available, "{name}: the ledger exists");
            assert!(
                view.grants.is_empty(),
                "{name}: a grant MANVI would refuse was presented as valid"
            );
            assert!(
                view.error.contains(diagnostic),
                "{name}: missing diagnostic {diagnostic:?}: {}",
                view.error
            );
        }
    }

    #[test]
    fn one_refused_record_does_not_hide_valid_grant_history() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(
            dir.path(),
            r#"[
              {"id":"GOOD","grantor":{"authority":"agent","id":"executor"},"reason":"r",
               "scope":{"task_id":"TASK-1","rules":["scope.unplanned"]}},
              {"id":"BAD","grantor":{"authority":"human","id":"operator"},"reason":"r",
               "scope":{"rules":["command.force_push"]}}
            ]"#,
        );

        let view = view(dir.path().to_str().unwrap());
        assert!(view.available);
        assert_eq!(
            view.grants.len(),
            1,
            "valid history was hidden: {}",
            view.error
        );
        assert_eq!(view.grants[0].id, "GOOD");
        assert!(
            view.error.contains("BAD"),
            "refusal was silent: {}",
            view.error
        );
    }

    #[test]
    fn the_path_follows_the_harness_state_directory_convention() {
        let p = grants_path("/repo");
        assert!(p.ends_with("harness-grants.json"));
        assert!(p.to_string_lossy().contains(".devcouncil"));
    }
}
