//! Codex Tier 1 `codex app-server` JSON-RPC stdio adapter inside `gitpulsed`.
//!
//! Hosting `codex app-server` threads directly in `gitpulsed` gives Codex the
//! highest-fidelity control-plane channel of any harness (Opus §4):
//! - **Item-ID-bound approvals with no hook timeout:** server→client JSON-RPC
//!   requests (`item/commandExecution/requestApproval`,
//!   `item/fileChange/requestApproval`) stay open until resolved.
//! - **True idle wake & active steering:** `turn/start` wakes an idle thread
//!   cleanly; `turn/interrupt` stops an active turn.
//! - **Strict remote safety invariants:**
//!   1. Remote callers may NEVER set or relax `approval_policy` or
//!      `sandbox_mode` (any attempt is refused loudly before hitting stdin).
//!   2. Approval decisions accept ONLY `AllowOnce` / `Deny` / `DenyAndInterrupt`
//!      (never `updatedInput` or `updatedPermissions`, Opus P0-4).
//!   3. GitPulse never mutates Codex's trust store.

use crate::procguard::{self, Registration};
use crate::supervisor;
use crate::remote::pty_gate::sha256_hex;
use gitpulse_protocol::canonical::canonical_json_string;
use gitpulse_protocol::wire::ApprovalDecisionKind;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, oneshot};

/// Forbidden parameter keys that remote callers must NEVER supply or relax.
const FORBIDDEN_REMOTE_POLICY_KEYS: &[&str] = &[
    "approval_policy",
    "approvalPolicy",
    "askForApproval",
    "ask_for_approval",
    "sandbox_mode",
    "sandboxMode",
    "sandbox",
    "sandbox_policy",
    "sandboxPolicy",
    "bypassApprovals",
    "bypass_approvals",
    "dangerouslyBypassApprovalsAndSandbox",
];

/// Host-owned Codex approval policy (immutable from remote).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexApprovalPolicy {
    Untrusted,
    OnRequest,
    OnFailure,
    Never,
}

impl CodexApprovalPolicy {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            CodexApprovalPolicy::Untrusted => "untrusted",
            CodexApprovalPolicy::OnRequest => "on-request",
            CodexApprovalPolicy::OnFailure => "on-failure",
            CodexApprovalPolicy::Never => "never",
        }
    }

    /// Higher number = stricter policy.
    fn strictness_rank(self) -> u8 {
        match self {
            CodexApprovalPolicy::Untrusted => 3,
            CodexApprovalPolicy::OnRequest => 2,
            CodexApprovalPolicy::OnFailure => 1,
            CodexApprovalPolicy::Never => 0,
        }
    }

    pub fn is_relaxation_from(self, current: CodexApprovalPolicy) -> bool {
        self.strictness_rank() < current.strictness_rank()
    }
}

/// Host-owned Codex sandbox mode (immutable from remote).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            CodexSandboxMode::ReadOnly => "read-only",
            CodexSandboxMode::WorkspaceWrite => "workspace-write",
            CodexSandboxMode::DangerFullAccess => "danger-full-access",
        }
    }

    fn strictness_rank(self) -> u8 {
        match self {
            CodexSandboxMode::ReadOnly => 2,
            CodexSandboxMode::WorkspaceWrite => 1,
            CodexSandboxMode::DangerFullAccess => 0,
        }
    }

    pub fn is_relaxation_from(self, current: CodexSandboxMode) -> bool {
        self.strictness_rank() < current.strictness_rank()
    }
}

/// Launch specification for a `codex app-server` child process.
#[derive(Debug, Clone)]
pub struct CodexLaunchConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub approval_policy: CodexApprovalPolicy,
    pub sandbox_mode: CodexSandboxMode,
    pub rpc_timeout: Duration,
}

impl CodexLaunchConfig {
    pub fn default_for_repo(cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: PathBuf::from("codex"),
            args: vec!["app-server".to_string()],
            cwd: cwd.into(),
            approval_policy: CodexApprovalPolicy::OnRequest,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            rpc_timeout: Duration::from_secs(15),
        }
    }
}

/// Pending item-ID-bound approval request pushed by `codex app-server`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexApprovalRequest {
    pub rpc_id: Value,
    pub item_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub method: String,
    pub summary: String,
    /// Canonical JSON bytes of the server's approval params (rendered & hashed
    /// on mobile per Opus M1/M3).
    pub canonical_bytes: String,
    pub input_sha256: String,
}

/// Server notification emitted by `codex app-server`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexNotification {
    pub method: String,
    pub params: Value,
}

/// Errors from the Codex Tier 1 `app-server` adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexError {
    SpawnFailed(String),
    Io(String),
    RpcTimeout(String),
    RpcError { code: i64, message: String },
    PolicyRelaxationForbidden { field: String },
    ApprovalNotFoundOrResolved { item_id: String },
    Closed,
}

impl std::fmt::Display for CodexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodexError::SpawnFailed(m) => write!(f, "failed to spawn codex app-server: {m}"),
            CodexError::Io(m) => write!(f, "codex app-server stdio error: {m}"),
            CodexError::RpcTimeout(m) => write!(f, "codex app-server RPC timeout: {m}"),
            CodexError::RpcError { code, message } => {
                write!(f, "codex app-server RPC error ({code}): {message}")
            }
            CodexError::PolicyRelaxationForbidden { field } => write!(
                f,
                "remote attempt to set or relax Codex policy field '{field}' is strictly forbidden"
            ),
            CodexError::ApprovalNotFoundOrResolved { item_id } => {
                write!(f, "Codex approval item '{item_id}' not found or already resolved")
            }
            CodexError::Closed => write!(f, "codex app-server connection closed"),
        }
    }
}

impl std::error::Error for CodexError {}

/// Recursively inspects a JSON object supplied by a remote caller and rejects
/// ANY key that attempts to specify or relax `approval_policy` or `sandbox_mode`.
pub fn reject_remote_policy_overrides(value: &Value) -> Result<(), CodexError> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if FORBIDDEN_REMOTE_POLICY_KEYS
                    .iter()
                    .any(|forbidden| k.eq_ignore_ascii_case(forbidden))
                {
                    return Err(CodexError::PolicyRelaxationForbidden { field: k.clone() });
                }
                reject_remote_policy_overrides(v)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_remote_policy_overrides(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Explicitly refuses any remote policy change request that would relax
/// `approval_policy` or `sandbox_mode` (or attempts any remote override at all).
pub fn refuse_remote_policy_change(
    current_approval: CodexApprovalPolicy,
    requested_approval: Option<CodexApprovalPolicy>,
    current_sandbox: CodexSandboxMode,
    requested_sandbox: Option<CodexSandboxMode>,
) -> Result<(), CodexError> {
    if let Some(next) = requested_approval {
        if next != current_approval || next.is_relaxation_from(current_approval) {
            return Err(CodexError::PolicyRelaxationForbidden {
                field: format!(
                    "approval_policy ({} -> {})",
                    current_approval.as_wire_str(),
                    next.as_wire_str()
                ),
            });
        }
    }
    if let Some(next) = requested_sandbox {
        if next != current_sandbox || next.is_relaxation_from(current_sandbox) {
            return Err(CodexError::PolicyRelaxationForbidden {
                field: format!(
                    "sandbox_mode ({} -> {})",
                    current_sandbox.as_wire_str(),
                    next.as_wire_str()
                ),
            });
        }
    }
    Ok(())
}

impl Drop for CodexAppServerClient {
    fn drop(&mut self) {
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((mut child, registration)) = child {
            reap(&mut child, &registration);
        }
    }
}

/// Stops the child's whole process group and reaps it.
fn reap(child: &mut Child, registration: &Registration) {
    let _ = registration.stop_and_reap(child, Duration::ZERO);
}

/// In-flight JSON-RPC calls, keyed by request id, each waiting for its reply.
type PendingCalls = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, CodexError>>>>>;

/// Active `codex app-server` JSON-RPC stdio client owned by `gitpulsed`.
pub struct CodexAppServerClient {
    config: CodexLaunchConfig,
    child: Mutex<Option<(Child, Registration)>>,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending_calls: PendingCalls,
    pending_approvals: Arc<Mutex<HashMap<String, CodexApprovalRequest>>>,
    approvals_rx: tokio::sync::Mutex<mpsc::Receiver<CodexApprovalRequest>>,
    notifications_rx: tokio::sync::Mutex<mpsc::Receiver<CodexNotification>>,
}

impl CodexAppServerClient {
    /// Spawns `codex app-server` (or configured mock binary) with piped stdio
    /// and starts the background NDJSON JSON-RPC reader task.
    pub fn spawn(config: CodexLaunchConfig) -> Result<Self, CodexError> {
        let mut cmd = Command::new(&config.program);
        cmd.args(&config.args)
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        // Started through procguard, like every long-lived child: trust
        // admission, its own process group, and the quit sweep.
        let (mut child, registration) = procguard::spawn(&mut cmd, "codex app-server")
            .map_err(|e| CodexError::SpawnFailed(format!("{}: {e}", config.program.display())))?;
        let Some(stdin) = child.stdin.take() else {
            reap(&mut child, &registration);
            return Err(CodexError::SpawnFailed("child stdin unavailable".into()));
        };
        let Some(stdout) = child.stdout.take() else {
            reap(&mut child, &registration);
            return Err(CodexError::SpawnFailed("child stdout unavailable".into()));
        };
        let stdin = match ChildStdin::from_std(stdin) {
            Ok(stdin) => stdin,
            Err(e) => {
                reap(&mut child, &registration);
                return Err(CodexError::SpawnFailed(format!("child stdin: {e}")));
            }
        };

        let pending_calls: PendingCalls = Arc::new(Mutex::new(HashMap::new()));
        let pending_approvals: Arc<Mutex<HashMap<String, CodexApprovalRequest>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (approvals_tx, approvals_rx) = mpsc::channel(128);
        let (notifications_tx, notifications_rx) = mpsc::channel(256);

        let calls_reader = Arc::clone(&pending_calls);
        let approvals_reader = Arc::clone(&pending_approvals);

        let reader = move || {
            let lines = std::io::BufReader::new(stdout).lines();
            for line in lines {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };

                let has_method = msg.get("method").and_then(Value::as_str).is_some();
                let has_id = msg.get("id").is_some();

                if has_method && has_id {
                    // Server -> client request (e.g. item-ID-bound approval request).
                    let method = msg["method"].as_str().unwrap_or_default().to_string();
                    let rpc_id = msg["id"].clone();
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    if is_approval_method(&method) {
                        let req = parse_approval_request(rpc_id, method, params);
                        approvals_reader
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(req.item_id.clone(), req.clone());
                        let _ = approvals_tx.blocking_send(req);
                    }
                } else if has_id {
                    // Response to our client -> server request.
                    if let Some(id_u64) = msg.get("id").and_then(Value::as_u64) {
                        let waiter = calls_reader
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&id_u64);
                        if let Some(tx) = waiter {
                            if let Some(err) = msg.get("error") {
                                let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
                                let message = err
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown rpc error")
                                    .to_string();
                                let _ = tx.send(Err(CodexError::RpcError { code, message }));
                            } else {
                                let result = msg.get("result").cloned().unwrap_or(Value::Null);
                                let _ = tx.send(Ok(result));
                            }
                        }
                    }
                } else if has_method {
                    // Server notification (turn/started, turn/completed, etc.).
                    let method = msg["method"].as_str().unwrap_or_default().to_string();
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    let _ = notifications_tx.blocking_send(CodexNotification { method, params });
                }
            }

            // Fail any remaining pending callers on EOF.
            let drained: Vec<_> = calls_reader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .drain()
                .collect();
            for (_, tx) in drained {
                let _ = tx.send(Err(CodexError::Closed));
            }
        };
        if let Err(e) = supervisor::spawn_once("codex-app-server-reader", "codex app-server reader", reader) {
            reap(&mut child, &registration);
            return Err(CodexError::SpawnFailed(format!("reader thread: {e}")));
        }

        Ok(Self {
            config,
            child: Mutex::new(Some((child, registration))),
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            next_id: AtomicU64::new(1),
            pending_calls,
            pending_approvals,
            approvals_rx: tokio::sync::Mutex::new(approvals_rx),
            notifications_rx: tokio::sync::Mutex::new(notifications_rx),
        })
    }

    pub fn approval_policy(&self) -> CodexApprovalPolicy {
        self.config.approval_policy
    }

    pub fn sandbox_mode(&self) -> CodexSandboxMode {
        self.config.sandbox_mode
    }

    /// Performs the JSON-RPC `initialize` handshake + `initialized` notification.
    pub async fn initialize(&self) -> Result<Value, CodexError> {
        let resp = self
            .call(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "gitpulsed",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .await?;
        self.send_json(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }))
        .await?;
        Ok(resp)
    }

    /// Starts a new Codex thread using ONLY the host-configured, immutable
    /// `approval_policy` and `sandbox_mode`.
    pub async fn thread_start(&self) -> Result<String, CodexError> {
        let result = self
            .call(
                "thread/start",
                json!({
                    "cwd": self.config.cwd.to_string_lossy(),
                    "approvalPolicy": self.config.approval_policy.as_wire_str(),
                    "sandbox": self.config.sandbox_mode.as_wire_str(),
                }),
            )
            .await?;
        extract_id(&result, &["threadId", "thread_id", "id"])
            .ok_or_else(|| CodexError::Io(format!("thread/start missing threadId: {result}")))
    }

    /// Starts a new turn on `thread_id` — wakes an idle thread cleanly (**true
    /// idle wake**) or steers an existing thread.
    ///
    /// Strictly refuses any `remote_extra_params` attempting to set or relax
    /// `approval_policy` or `sandbox_mode`.
    pub async fn turn_start(
        &self,
        thread_id: &str,
        prompt: &str,
        remote_extra_params: Option<&Value>,
    ) -> Result<String, CodexError> {
        let mut params = serde_json::Map::new();
        if let Some(extra) = remote_extra_params {
            reject_remote_policy_overrides(extra)?;
            if let Some(obj) = extra.as_object() {
                for (k, v) in obj {
                    params.insert(k.clone(), v.clone());
                }
            }
        }
        params.insert("threadId".into(), json!(thread_id));
        params.insert(
            "input".into(),
            json!([{ "type": "text", "text": prompt }]),
        );

        let result = self.call("turn/start", Value::Object(params)).await?;
        extract_id(&result, &["turnId", "turn_id", "id"])
            .ok_or_else(|| CodexError::Io(format!("turn/start missing turnId: {result}")))
    }

    /// Interrupts an in-flight turn on `thread_id`.
    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<Value, CodexError> {
        self.call(
            "turn/interrupt",
            json!({
                "threadId": thread_id,
                "turnId": turn_id,
            }),
        )
        .await
    }

    /// Receives the next item-ID-bound approval request pushed by `codex app-server`.
    pub async fn next_approval_request(&self) -> Option<CodexApprovalRequest> {
        self.approvals_rx.lock().await.recv().await
    }

    /// Receives the next server notification (`turn/started`, `turn/completed`, etc.).
    pub async fn next_notification(&self) -> Option<CodexNotification> {
        self.notifications_rx.lock().await.recv().await
    }

    /// Lists all currently pending item-ID-bound approvals (for `inbox.snapshot`).
    pub fn open_approvals(&self) -> Vec<CodexApprovalRequest> {
        self.pending_approvals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    /// Resolves a pending item-ID-bound approval request using ONLY the minimal
    /// Opus P0-4 decision surface (`AllowOnce` -> `"accept"`, `Deny` -> `"decline"`,
    /// `DenyAndInterrupt` -> `"cancel"`). Never sends `updatedInput` or
    /// `updatedPermissions`.
    pub async fn resolve_approval(
        &self,
        item_id: &str,
        decision: ApprovalDecisionKind,
    ) -> Result<(), CodexError> {
        let req = self
            .pending_approvals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(item_id)
            .ok_or_else(|| CodexError::ApprovalNotFoundOrResolved {
                item_id: item_id.to_string(),
            })?;

        let wire_decision = match decision {
            ApprovalDecisionKind::AllowOnce => "accept",
            ApprovalDecisionKind::Deny => "decline",
            ApprovalDecisionKind::DenyAndInterrupt => "cancel",
        };

        let response = json!({
            "jsonrpc": "2.0",
            "id": req.rpc_id,
            "result": {
                "decision": wire_decision,
            }
        });
        self.send_json(&response).await
    }

    /// Terminates the child `codex app-server` cleanly.
    pub async fn shutdown(&self) {
        let child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((mut child, registration)) = child {
            reap(&mut child, &registration);
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, CodexError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, tx);

        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(err) = self.send_json(&req).await {
            self.pending_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
            return Err(err);
        }

        match tokio::time::timeout(self.config.rpc_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CodexError::Closed),
            Err(_) => {
                self.pending_calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&id);
                Err(CodexError::RpcTimeout(method.to_string()))
            }
        }
    }

    async fn send_json(&self, value: &Value) -> Result<(), CodexError> {
        let mut line = serde_json::to_string(value).map_err(|e| CodexError::Io(e.to_string()))?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| CodexError::Io(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| CodexError::Io(e.to_string()))
    }
}

fn is_approval_method(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "execCommandApproval"
            | "applyPatchApproval"
    )
}

fn parse_approval_request(rpc_id: Value, method: String, params: Value) -> CodexApprovalRequest {
    let item_id = extract_id(&params, &["itemId", "item_id", "callId", "id"])
        .unwrap_or_else(|| format!("rpc-{}", rpc_id));
    let thread_id = extract_id(&params, &["threadId", "thread_id"]).unwrap_or_default();
    let turn_id = extract_id(&params, &["turnId", "turn_id"]).unwrap_or_default();
    let summary = params
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| params.get("reason").and_then(Value::as_str))
        .unwrap_or(&method)
        .to_string();
    let canonical_bytes = canonical_json_string(&params).unwrap_or_default();
    let input_sha256 = if canonical_bytes.is_empty() {
        String::new()
    } else {
        sha256_hex(canonical_bytes.as_bytes())
    };

    CodexApprovalRequest {
        rpc_id,
        item_id,
        thread_id,
        turn_id,
        method,
        summary,
        canonical_bytes,
        input_sha256,
    }
}

fn extract_id(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(s) = value.get(*key).and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    if let Some(nested) = value.get("thread").or_else(|| value.get("turn")) {
        for key in keys {
            if let Some(s) = nested.get(*key).and_then(Value::as_str) {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}
