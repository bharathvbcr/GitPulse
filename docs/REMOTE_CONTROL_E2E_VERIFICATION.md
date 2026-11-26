# Remote Control E2E & Empirical Host Verification

This document records:
1. The live end-to-end integration test (`src-tauri/tests/remote_relay_e2e.rs`) exercising the real Go `gitpulse-relay` binary against Rust `wayloded`, `waylode-protocol`, `HostNoiseManager`, `RevocationCache`, `ApprovalBroker`, and `SteeringIngressGate` over real loopback TCP/HTTP/WebSockets.
2. Read-only, non-destructive empirical host probes conducted on this macOS workstation for `codex`, `claude` CLI + Claude Desktop (`claude-desktop`), and `agy` CLI + Antigravity Desktop (`Antigravity.app`).

---

## 1. Live End-to-End Integration Test (`src-tauri/tests/remote_relay_e2e.rs`)

### Command
```bash
cargo test --manifest-path src-tauri/Cargo.toml --test remote_relay_e2e -- --nocapture
```

### Configuration & Environment
- **Relay source directory**: `GITPULSE_RELAY_DIR` (defaults to `/Users/bharath/Code/devtools/gitpulse-relay.remote-control`).
- **Graceful skip policy**: Skips cleanly ONLY if the `go` toolchain is unavailable in `PATH` or the relay source checkout is absent. On developer/CI machines with Go and the relay checkout present, the test builds the real `gitpulse-relay` binary (`go build ./cmd/gitpulse-relay`) into a temporary directory and runs it live on `127.0.0.1:0` with an isolated temporary SQLite data directory and `--paywall-mode enforced`.
- **Hermeticity**: Declares `mod common;` (isolated per-process `HOME`, trust store, and `GIT_CONFIG_GLOBAL`) while preserving `GOPATH`/`GOMODCACHE`/`GOCACHE` for offline Go builds.

### End-to-End Flow Exercised
1. **Free-tier gating (host + relay)**:
   - Runs the compiled `wayloded --relay --once --config-dir <free-dir>` binary against `tier: "free"` (`remote_control_enabled: true`) and asserts exit code **`4`** (`EXIT_CODE_FREE_OR_DISABLED`) before creating `wayloded-relay.lock`.
   - Signs in via `POST /v1/auth/github` (against an in-process loopback mock GitHub `/user` server), verifies `tier == "free"`, `paywall_mode == "enforced"`, and absence of `remote_control`.
   - Registers desktop (`kind: "desktop"`) and mobile (`kind: "mobile"`) devices via `POST /v1/devices` and asserts that `GET /v1/tunnel/ws?role=host` is rejected pre-upgrade with HTTP **`403`** and error code **`feature_required`**.
2. **Tier upgrade, enrollment, and revocation list**:
   - Runs `gitpulse-relay grant --login e2e-operator --tier pro --data-dir <dir> --store-backend sqlite` and verifies `GET /v1/entitlements` returns `tier == "pro"` with `remote_control` and `device_sync`.
   - Issues Account `IdentityKey`-signed `DeviceCertificate` v2 records for desktop (`DeviceKind::Desktop`) and mobile (`DeviceKind::Mobile` with enrolled SEC1 P-256 approval public key).
   - Completes `PUT /v1/identity`, `PUT /v1/devices/me/enc-key` (both devices), `POST /v1/enrolments`, `POST /v1/enrolments/{mobile_id}/approve`, and `GET /v1/enrolments/self`.
   - Publishes `SignedRevocationList` v1 via `PUT /v1/identity/revocations`, fetches it back live via `GET /v1/identity/revocations`, and installs it into host `RevocationCache` and `TrustedApprovalKeyStore`.
3. **Signed WebSocket tunnels + role/kind enforcement**:
   - Opens live host (`role=host`) and mobile (`role=client`) tunnels to `/v1/tunnel/ws` authenticated with Ed25519 request-signing headers from `waylode_protocol::crypto::sign_relay_request`.
   - Asserts a mobile device attempting `role=host` is rejected pre-upgrade with HTTP **`403`** (`role_device_kind_mismatch`).
4. **Live Noise `IK` handshake over relay**:
   - Exchanges empty-payload Noise `IK` (`Noise_IK_25519_ChaChaPoly_SHA256`) handshake `msg1` and `msg2` wrapped in `GPTF` v1 `OuterFrame`s over the live relay WebSockets (`NoiseHandshake::new_initiator` <-> `HostNoiseManager::accept_mobile`), pinning peer static X25519 keys against verified `DeviceCertificate` v2 records and the live `RevocationCache`.
5. **Zero-knowledge E2EE control plane, P-256 approval & steering verification**:
   - Registers a real `Bash` permission ticket in `ApprovalBroker` (away presence, trusted hermetic repo, live `TrustedApprovalKeyStore`).
   - Host encrypts and sends `ControlPlaneEvent::InboxSnapshot` and `ControlPlaneEvent::ApprovalCreated` over host WS -> relay -> mobile WS.
   - Asserts raw wire JSON seen by the relay contains `ciphertext_b64` and **zero** cleartext leakage (no command body, sentinel string, session ID, tool name, repository path, or inner event type).
   - Mobile decrypts both frames, signs P-256 `SignedApprovalDecision` (`allow_once`) and `SignedSteeringConfirm`, encrypts both into `OuterFrame`s, and sends over mobile WS -> relay -> host WS (again asserting zero wire plaintext leakage).
   - Host decrypts via `HostNoiseManager::open_from_mobile`, verifies the approval via `ApprovalBroker::resolve_signed` (unblocking the waiting hook thread with `HookBrokerReply::Decision { decision: AllowOnce }`), verifies steering via `SteeringIngressGate::verify_strict` (`SteeringIngressVerdict::HumanConfirmed`), and asserts replay rejection (`BrokerError::NonceReplayed`, `ProtocolError::ReplayDetected`).
6. **Immediate revocation close (`4003`) + v2 revocation list enforcement**:
   - Calls `DELETE /v1/devices/{mobile_id}` on the live relay (`204 No Content`) and asserts the mobile WebSocket immediately receives close code **`4003`** (`WS_CLOSE_REVOKED_OR_DOWNGRADED`).
   - Publishes and fetches `SignedRevocationList` v2 revoking `mobile_id`, asserting `HostNoiseManager::evict_revoked` drops the mobile session and `ApprovalBroker::resolve_signed` returns `Err(BrokerError::DeviceRevoked(_))`.

### Verification Run Evidence

- **Relay checkout verified against**: `/Users/bharath/Code/devtools/gitpulse-relay.remote-control` (`feat/remote-control` @ `f1654752ff456289b18bbf1f3d6234dcb3d38d07`)
- **Command**: `cargo test --manifest-path src-tauri/Cargo.toml --test remote_relay_e2e -- --nocapture`
- **Result** (executed live, NOT skipped):
```text
[remote_relay_e2e] PASS live relay=127.0.0.1:57513 relay_dir=/Users/bharath/Code/devtools/gitpulse-relay.remote-control host=dev_1c4413d8107e9cecef7c2af0e15dfad9 mobile=dev_a804fcb985272ed7cb82c241d47aa815 close_code=4003
test live_relay_free_gate_pro_noise_ik_approvals_steering_and_4003_revocation ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.46s
```
- **Hermetic test contract check (`cargo test --manifest-path src-tauri/Cargo.toml --test hermetic_contract`)**:
  - `src-tauri/tests/remote_relay_e2e.rs` introduced **0** violations across all hermetic, env-isolation, wall-clock scaling, and sleep-scanner checks. (Note: pre-existing lines in `src/alerts/approval_broker.rs`, `src/hooks/permission_bridge.rs`, and `src/remote/tests.rs` on `feat/unified-control-plane` were outside this task's file ownership and left untouched.)

---

## 2. Empirical Host Behavior Probes (Read-Only, Non-Destructive)

All probes below were executed read-only on this macOS arm64 workstation (Go `1.27.4`, Rust `1.100.0`) without modifying `~/.codex`, `~/.claude`, `~/.gemini`, or any installed desktop application settings.

### 2.1 `codex` CLI (`codex app-server` & TUI hooks)

- **Observed installation state**:
  - `which codex`: not found in default `PATH`.
  - `/Users/bharath/.local/bin/codex`: **broken symlink** targeting `/Users/bharath/.codex/packages/standalone/current/bin/codex`.
  - `/Users/bharath/.codex/packages/`: does not exist; no `/opt/homebrew/Cellar/codex` or `/Applications/Codex.app` installed.
  - Historical `~/.codex/` directory exists with `config.toml` (`project_doc_max_bytes = 65536`), `AGENTS.md -> /Users/bharath/.claude/CLAUDE.md`, `skills/`, and SQLite state files (`state_5.sqlite`, `logs_2.sqlite`, `goals_1.sqlite`, `memories_1.sqlite`, `queue_1.sqlite`).
- **What is verified**:
  - No runnable `codex` binary is currently present on this host.
  - Waylode's `src-tauri/src/remote/codex_app_server.rs` protocol adapter (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, policy-override refusal) is verified only against Rust unit tests with mock stdio streams.
- **What remains unverified empirically**:
  - Live `codex --version`, `codex app-server --help`, `codex app-server generate-json-schema`, and whether Codex TUI supports a native `PermissionRequest` hook schema cannot be empirically verified until a live `codex` binary is reinstalled.

### 2.2 `claude` CLI & Claude Desktop (`claude-desktop`)

- **Observed versions**:
  - `claude` CLI: **`2.1.331 (Claude Code)`** (versions `2.1.314`, `2.1.325`, `2.1.327`, `2.1.328`, `2.1.329`, `2.1.330`, `2.1.331` in `~/.local/share/claude/versions/`).
  - Claude Desktop: `/Applications/Claude.app` (`com.anthropic.claudefordesktop`), **`6.6395.1`**.
  - Claude Desktop's bundled Claude Code binary: `~/Library/Application Support/Claude/claude-code/2.1.326/c4434d650cb3/claude.app/Contents/MacOS/claude` — reports **`2.1.326 (Claude Code)`**.
- **Observed CLI capabilities (`claude 2.1.331 --help` & binary inspection)**:
  - Supports `--settings`, `--setting-sources`, `--plugin-dir`, `--plugin-url`, `--include-hook-events`, `--permission-mode`, `--permission-prompts`, `--permission-prompt-tool`, and `--remote-control`.
  - Both CLI `2.1.331` and Desktop-bundled `2.1.326` binaries contain hook event symbols: `PermissionRequest`, `PreToolUse`, `PostToolUse`, `Stop`, `UserPromptSubmit`, `Notification`.
  - Existing `~/.claude/settings.json` has `hooks` keys: `['Notification', 'PostToolUse', 'PostToolUseFailure', 'PreToolUse', 'SessionEnd', 'SessionStart', 'Stop', 'UserPromptSubmit']` (`PermissionRequest` is not currently configured in the user's live settings file, and was not mutated).
- **Key empirical finding on Claude Desktop (`6.6395.1`)**:
  - Inspecting live Claude Desktop child processes (`ps -ww` on active `entrypoint: "claude-desktop"` PIDs) confirms Claude Desktop launches its bundled `2.1.326` `claude` binary with:
    - `--output-format stream-json --input-format stream-json`
    - **`--setting-sources=user,project,local`** (proving Claude Desktop loads `~/.claude/settings.json` alongside project and local settings, merged with its inline `--settings` JSON for `deniedMcpServers` and `sandbox`)
    - **`--permission-prompt-tool=stdio`** (in those active sessions paired with `--permission-mode=bypassPermissions`).
- **Verified vs. still requiring interactive desktop UI test**:
  - **Verified (read-only)**: Claude Desktop runs a standard Claude Code `2.1.326` binary containing `PermissionRequest` and explicitly passes `--setting-sources=user,project,local`, so user-level hooks in `~/.claude/settings.json` are in scope.
  - **Still requires an interactive Desktop UI click test**: Whether Claude Desktop in interactive (non-bypass) mode invokes a `~/.claude/settings.json` `PermissionRequest` hook *before* its `--permission-prompt-tool=stdio` Electron dialog and cleanly honors hook stdout allow/deny decisions without UI race/freeze. Until tested interactively, Claude Desktop sessions should be verified with a live non-bypass UI prompt before promoting beyond conservative fallback.

### 2.3 `agy` CLI & Antigravity Desktop (`Antigravity.app`)

- **Observed versions**:
  - `agy` CLI: `/Users/bharath/.local/bin/agy`, version **`1.5.15`**.
  - Antigravity Desktop: `/Applications/Antigravity.app` (`com.google.antigravity`), version **`2.34.0`** (newer than the `2.32.1` reference in earlier design notes).
  - Antigravity IDE: `/Applications/Antigravity IDE.app` (`com.google.antigravity-ide`), version **`2.7.0`**.
- **Observed CLI flags & subcommands (`agy 1.5.15 --help`, `agy plugin --help`)**:
  - **Per-invocation `--settings` EXISTS**:
    `--settings`: `"Settings for this session only, never saved: a JSON object of settings.json keys, inline or as a file path"`.
  - **Per-invocation `--plugin-dir` DOES NOT EXIST**: `agy --help` has no `--plugin-dir` or `--plugin-url` flag. Plugins are managed persistently via `agy plugin {list, import, install, uninstall, enable, disable, validate, link}`.
  - Other notable flags/subcommands: `--dangerously-skip-permissions`, `--mode (accept-edits, auto, plan)`, `--sandbox`, `--remote-control`, `agy remote-control {start, status, stop}`, `agy acp` (Agent Client Protocol stdio server).
  - `~/.gemini/antigravity-cli/settings.json` top-level keys: `['model', 'permissions', 'trustedWorkspaces']`.
- **Observed hook symbols in `agy 1.5.15`**:
  - Binary string inspection shows `PreToolUse`, `PostToolUse`, `Stop`.
  - **No `PermissionRequest` hook symbol** was found in `agy 1.5.15`.
- **Architectural conclusion**:
  - Even with `Antigravity.app 2.34.0` (above `2.32.1`) and `agy 1.5.15`, `agy` still does **not** expose a `PermissionRequest` hook or a per-invocation `--plugin-dir` flag (only `--settings` and persistent `agy plugin install/enable`).
  - This empirically confirms Waylode's conservative capability modeling for Antigravity: `ApprovalChannel::NativeOnly` (`ungated: true`, `supports_remote_approval: false`), persistent plugin management via `agy plugin`, and honest `"Queued (next turn)"` steering semantics for idle sessions.
