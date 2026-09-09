//! Native preparation for a task-linked terminal attempt. The canonical store
//! owns the snapshot and capacity transaction; this adapter supplies observed
//! checkout identity. Preparation neither spawns a process nor grants access.

use super::{query, WorkbenchError, WorkbenchState};
use crate::engine::git_cli::{git_captured, resolve_git_common_dir, resolve_git_dir, resolve_repo};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preparation {
    id: String,
    request_id: String,
    task_id: String,
    source_revision: u64,
    repository_id: String,
    repository_revision: u64,
    provider: String,
    permission_mode: String,
    #[serde(default)]
    acknowledge_bypass: bool,
    repo_path: String,
}

fn parse(input: &str) -> Result<Preparation, WorkbenchError> {
    if input.len() > 16_384 || !input.trim_start().starts_with('{') {
        return Err(WorkbenchError::new(
            "invalid_input",
            "Terminal preparation must be a JSON object of at most 16 KiB.",
        ));
    }
    // Struct deserialization rejects duplicate and unknown fields. Validate all
    // user-controlled metadata before filesystem probing or database creation.
    let request: Preparation = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    let ids = [
        &request.id,
        &request.request_id,
        &request.task_id,
        &request.repository_id,
    ];
    if ids.iter().any(|id| {
        id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) || [request.source_revision, request.repository_revision]
        .iter()
        .any(|r| *r == 0 || *r > 9_007_199_254_740_991)
        || !["codex", "claude"].contains(&request.provider.as_str())
        || ![
            "inspect",
            "ask",
            "edit",
            "auto_review",
            "preapproved",
            "bypass",
        ]
        .contains(&request.permission_mode.as_str())
        || (request.permission_mode == "bypass") != request.acknowledge_bypass
        || !valid_path(&request.repo_path)
    {
        return Err(WorkbenchError::new("invalid_input", "Terminal preparation requires valid identities, exact revisions, a supported mode and an absolute checkout path. Bypass acknowledgment applies only to bypass mode."));
    }
    Ok(request)
}

fn valid_path(path: &str) -> bool {
    path.len() <= 4096 && Path::new(path).is_absolute() && !path.chars().any(char::is_control)
}

fn unavailable(message: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new("repository_unavailable", message)
}

fn native_path(path: &Path) -> Result<String, WorkbenchError> {
    path.to_str()
        .filter(|s| valid_path(s))
        .map(String::from)
        .ok_or_else(|| unavailable("The resolved checkout path is unsupported or exceeds 4 KiB."))
}

fn observed(repo: &Path, args: &[&str]) -> Result<(i32, String), WorkbenchError> {
    let output = git_captured(repo, args)
        .and_then(|r| r.require_complete("Terminal checkout probe"))
        .map_err(unavailable)?;
    if output.cancelled {
        return Err(unavailable("Checkout validation was cancelled."));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| unavailable("Git returned an invalid Unicode checkout identity."))?;
    // Remove Git's line ending, not meaningful whitespace in a native path.
    Ok((
        output.status_code,
        text.trim_end_matches(['\r', '\n']).to_owned(),
    ))
}

fn head(repo: &Path) -> Result<Option<String>, WorkbenchError> {
    let (status, oid) = observed(repo, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
    if status == 0 && [40, 64].contains(&oid.len()) && oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(Some(oid));
    }
    if status == 1 {
        // A failed HEAD lookup alone does not establish an unborn branch. Its
        // symbolic branch must be valid and demonstrably absent from the refs.
        let (symbolic_status, branch) = observed(repo, &["symbolic-ref", "--quiet", "HEAD"])?;
        if symbolic_status == 0 && branch.starts_with("refs/heads/") {
            let (ref_status, _) = observed(repo, &["show-ref", "--verify", "--quiet", &branch])?;
            if ref_status == 1 {
                return Ok(None);
            }
        }
    }
    Err(unavailable(
        "Cannot establish a commit or an unborn branch for the selected checkout.",
    ))
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
struct Checkout {
    cwd: String,
    git_dir: String,
    git_common_dir: String,
    head_oid: Option<String>,
    head_ref: Option<String>,
}

fn checkout(repo_path: &str) -> Result<Checkout, WorkbenchError> {
    let resolved = resolve_repo(repo_path).map_err(unavailable)?;
    if resolved.is_bare {
        return Err(unavailable(
            "A terminal task requires a working checkout; this repository is bare.",
        ));
    }
    let cwd = Path::new(&resolved.path);
    let cwd_text = native_path(cwd)?;
    let (status, top) = observed(cwd, &["rev-parse", "--show-toplevel"])?;
    if status != 0
        || Path::new(&top)
            .canonicalize()
            .map_err(|e| unavailable(e.to_string()))?
            != cwd
    {
        return Err(unavailable(
            "The selected directory is not the checkout root.",
        ));
    }
    let git_dir = native_path(&resolve_git_dir(cwd).map_err(unavailable)?)?;
    let git_common_dir = native_path(&resolve_git_common_dir(cwd).map_err(unavailable)?)?;
    let head_oid = head(cwd)?;
    let (status, branch) = observed(cwd, &["symbolic-ref", "--quiet", "HEAD"])?;
    let head_ref = match status {
        0 if branch.starts_with("refs/heads/") => Some(branch),
        1 if head_oid.is_some() => None,
        _ => return Err(unavailable("Cannot establish the checkout branch.")),
    };
    Ok(Checkout {
        cwd: cwd_text,
        git_dir,
        git_common_dir,
        head_oid,
        head_ref,
    })
}

pub(super) fn prepare(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    prepare_kind(state, input, "external_terminal")
}

pub(super) fn prepare_managed(
    state: &WorkbenchState,
    input: &str,
) -> Result<Value, WorkbenchError> {
    prepare_kind(state, input, "managed")
}

fn prepare_kind(state: &WorkbenchState, input: &str, kind: &str) -> Result<Value, WorkbenchError> {
    let input = parse(input)?;
    if kind == "managed" && (input.provider != "codex" || input.id.len() > 100) {
        return Err(WorkbenchError::new(
            "unsupported_operation",
            "Managed launches require Codex and an attempt identity of at most 100 characters.",
        ));
    }
    let checkout = checkout(&input.repo_path)?;
    let body = json!({
        "id":input.id, "request_id":input.request_id, "expected_revision":0,
        "kind":kind,
        "task_id":input.task_id, "source_revision":input.source_revision,
        "repository_id":input.repository_id, "repository_revision":input.repository_revision,
        "provider":input.provider, "permission_mode":input.permission_mode,
        "acknowledge_bypass":input.acknowledge_bypass, "cwd":checkout.cwd,
        "git_dir":checkout.git_dir, "git_common_dir":checkout.git_common_dir,
        "head_oid":checkout.head_oid, "head_ref":checkout.head_ref,
    });
    state.with_store(|store| query(store, "runs.prepare", &body.to_string()))
}

pub(super) fn revalidate(saved: &Value) -> Result<(), WorkbenchError> {
    let recorded: Checkout = serde_json::from_value(saved["item"].clone())
        .map_err(|e| WorkbenchError::new("protocol_error", e.to_string()))?;
    if saved["item"].get("head_ref").is_none() || checkout(&recorded.cwd)? != recorded {
        return Err(WorkbenchError::new("checkout_changed", "The checkout, Git directories, branch or commit changed after preparation. Prepare a new attempt."));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    id: String,
    request_id: String,
    expected_revision: u64,
    owner_id: String,
    session_id: String,
}

pub(super) fn claim(state: &WorkbenchState, input: &str) -> Result<Value, WorkbenchError> {
    if input.len() > 4096 || !input.trim_start().starts_with('{') {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A launch claim must be a JSON object of at most 4 KiB.",
        ));
    }
    let claim: Claim = serde_json::from_str(input)
        .map_err(|e| WorkbenchError::new("invalid_input", e.to_string()))?;
    if [
        &claim.id,
        &claim.request_id,
        &claim.owner_id,
        &claim.session_id,
    ]
    .iter()
    .any(|id| {
        id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) || claim.expected_revision == 0
        || claim.expected_revision > 9_007_199_254_740_991
    {
        return Err(WorkbenchError::new(
            "invalid_input",
            "A launch claim requires valid identities and an exact revision.",
        ));
    }
    let saved =
        state.with_store(|store| query(store, "runs.get", &json!({"id":claim.id}).to_string()))?;
    if saved["item"]["state"] == "prepared" {
        // Native launch requires an observed branch field. Older, manually
        // supplied records cannot silently imply a detached checkout.
        revalidate(&saved)?;
    }
    // Preserve the exact request identity and the canonical one-use receipt.
    // CAS also closes source/revision races while filesystem probing ran.
    // Filesystem probes are observations, not locks against external git tools.
    state.with_store(|store| query(store, "runs.claim", input))
}

#[cfg(test)]
mod tests {
    use super::super::{Inner, WorkbenchState};
    use crate::engine::git_cli::{git_global, git_text};
    use serde_json::{json, Value};
    use std::path::Path;
    use std::sync::Arc;

    fn host(path: &Path) -> WorkbenchState {
        WorkbenchState(Arc::new(Inner {
            path: Some(path.into()),
            ..Inner::default()
        }))
    }

    fn init(root: &Path) {
        git_global(&["init", root.to_str().unwrap()]).unwrap();
    }

    fn commit(root: &Path) {
        git_text(
            root,
            &[
                "-c",
                "user.name=Workbench Test",
                "-c",
                "user.email=workbench@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
    }

    fn prepare(root: &Path) -> Value {
        json!({"id":"run","request_id":"prepare","task_id":"task","source_revision":1,"repository_id":"repo","repository_revision":1,"provider":"codex","permission_mode":"inspect","acknowledge_bypass":false,"repo_path":root})
    }

    fn seed(state: &WorkbenchState, root: &Path) {
        state
            .register(root.to_str().unwrap(), "repo", "register")
            .unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"task","expected_revision":0,"title":"Preserve E42","description":"Exact saved instructions","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
    }

    #[test]
    fn native_preparation_binds_real_worktree_identity_and_preserves_source_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo with spaces");
        let linked = dir.path().join(if cfg!(unix) {
            "linked 'quoted' worktree "
        } else {
            "linked 'quoted' worktree"
        });
        let clone = dir.path().join("different clone");
        init(&root);
        commit(&root);
        git_text(
            &root,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        )
        .unwrap();
        git_global(&[
            "clone",
            "--local",
            root.to_str().unwrap(),
            clone.to_str().unwrap(),
        ])
        .unwrap();
        let db = dir.path().join("profile.sqlite");
        let state = host(&db);
        seed(&state, &root);
        let wrong = state
            .request("runs.prepare_terminal", &prepare(&clone).to_string())
            .unwrap_err();
        assert_eq!(wrong.code, "repository_mismatch");
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 0);
        let input = prepare(&linked).to_string();
        let result = state.request("runs.prepare_terminal", &input).unwrap();
        assert_eq!(result["item"]["state"], "prepared");
        assert_eq!(result["item"]["cwd"], json!(linked.canonicalize().unwrap()));
        assert_eq!(
            result["item"]["git_common_dir"],
            json!(root.join(".git").canonicalize().unwrap())
        );
        assert_ne!(result["item"]["git_dir"], result["item"]["git_common_dir"]);
        assert_eq!(
            result["item"]["head_oid"],
            git_text(&linked, &["rev-parse", "HEAD"]).unwrap().trim()
        );
        assert_eq!(
            state.request("runs.prepare_terminal", &input).unwrap(),
            result
        );
        assert_eq!(
            state.request("items.get", r#"{"id":"task"}"#).unwrap()["item"]["revision"],
            1
        );
        assert!(state.0.worker.get().is_none());
        drop(state);
        let restored = host(&db).request("runs.get", r#"{"id":"run"}"#).unwrap();
        assert_eq!(
            restored["item"]["brief"]["task"]["description"],
            "Exact saved instructions"
        );
    }

    #[test]
    fn native_preparation_rejects_malformed_input_before_filesystem_or_profile_io() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("absent/profile.sqlite");
        let state = host(&db);
        let base = prepare(&dir.path().join("missing repo"));
        for input in [
            "null".into(),
            "[]".into(),
            "{".into(),
            " ".repeat(16_385),
            base.to_string()
                .replacen("\"id\":\"run\"", "\"id\":\"run\",\"id\":\"again\"", 1),
        ] {
            assert_eq!(
                state
                    .request("runs.prepare_terminal", &input)
                    .unwrap_err()
                    .code,
                "invalid_input",
                "{input}"
            );
        }
        for (key, value) in [
            ("extra", json!(true)),
            ("id", json!("../../run")),
            ("source_revision", json!(0)),
            ("source_revision", json!(9_007_199_254_740_992_u64)),
            ("source_revision", json!(1.5)),
            ("repository_revision", json!(0)),
            ("provider", json!("shell")),
            ("permission_mode", json!("unknown")),
            ("permission_mode", json!("bypass")),
            ("acknowledge_bypass", json!(true)),
            ("repo_path", json!("relative")),
            ("repo_path", json!("/bad\npath")),
            ("repo_path", json!("/bad\0path")),
        ] {
            let mut input = base.clone();
            input[key] = value;
            assert_eq!(
                state
                    .request("runs.prepare_terminal", &input.to_string())
                    .unwrap_err()
                    .code,
                "invalid_input",
                "{input}"
            );
        }
        assert!(!db.parent().unwrap().exists());
        assert!(state.0.worker.get().is_none());
    }

    #[test]
    fn native_preparation_distinguishes_unborn_head_from_broken_or_unavailable_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("unborn");
        init(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        let result = state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        assert!(result["item"]["head_oid"].is_null());
        state
            .request(
                "runs.cancel",
                r#"{"id":"run","request_id":"cancel","expected_revision":1}"#,
            )
            .unwrap();
        std::fs::write(root.join(".git/HEAD"), "invalid head\n").unwrap();
        assert_eq!(
            state
                .request("runs.prepare_terminal", &prepare(&root).to_string())
                .unwrap_err()
                .code,
            "repository_unavailable"
        );
        let bare = dir.path().join("bare");
        git_global(&["init", "--bare", bare.to_str().unwrap()]).unwrap();
        assert_eq!(
            state
                .request("runs.prepare_terminal", &prepare(&bare).to_string())
                .unwrap_err()
                .code,
            "repository_unavailable"
        );
        assert_eq!(
            state
                .request(
                    "runs.prepare_terminal",
                    &prepare(&dir.path().join("missing")).to_string()
                )
                .unwrap_err()
                .code,
            "repository_unavailable"
        );
        assert_eq!(state.request("runs.list", "{}").unwrap()["total"], 1);
    }

    #[test]
    fn native_claim_refuses_checkout_drift_without_consuming_its_claim() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        let original_branch = git_text(&root, &["symbolic-ref", "--short", "HEAD"]).unwrap();
        let claim = r#"{"id":"run","request_id":"claim","expected_revision":1,"owner_id":"host","session_id":"session"}"#;
        git_text(&root, &["switch", "-c", "same-commit-other-branch"]).unwrap();
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "checkout_changed"
        );
        commit(&root);
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "checkout_changed"
        );
        assert_eq!(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"]["state"],
            "prepared"
        );
        // A rejected probe does not consume the request ID. Restore the exact
        // disposable checkout and prove that this claim can be consumed once.
        git_text(&root, &["switch", original_branch.trim()]).unwrap();
        assert_eq!(
            state.request("runs.claim", claim).unwrap()["item"]["state"],
            "starting"
        );
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "claim_consumed"
        );
    }

    #[test]
    fn native_claim_refuses_replaced_checkout_and_changed_task() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init(&root);
        commit(&root);
        let state = host(&dir.path().join("profile.sqlite"));
        seed(&state, &root);
        state
            .request("runs.prepare_terminal", &prepare(&root).to_string())
            .unwrap();
        let claim = r#"{"id":"run","request_id":"claim","expected_revision":1,"owner_id":"host","session_id":"session"}"#;
        std::fs::rename(&root, dir.path().join("moved")).unwrap();
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "repository_unavailable"
        );
        init(&root);
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "checkout_changed"
        );
        std::fs::rename(&root, dir.path().join("replacement")).unwrap();
        std::fs::rename(dir.path().join("moved"), &root).unwrap();
        state.request("items.put", &json!({"id":"task","request_id":"edit","expected_revision":1,"title":"Changed instructions","repository_ids":["repo"],"primary_repository_id":"repo"}).to_string()).unwrap();
        assert_eq!(
            state.request("runs.claim", claim).unwrap_err().code,
            "revision_conflict"
        );
        assert_eq!(
            state.request("runs.get", r#"{"id":"run"}"#).unwrap()["item"]["state"],
            "prepared"
        );
    }
}
