# Security

GitPulse is a **local, zero-telemetry** desktop app. It has no GitPulse backend and no analytics.

```mermaid
flowchart TD
    Webview["Tauri webview — CSP default-src 'self'"] --> IPC["IPC: cmd_* only"]
    IPC --> Engine["Rust sandbox, confined to the open repository"]
    Engine --> Gh["gh CLI / OS keychain"]
    Engine --> AI["Local LLM on 127.0.0.1 / localhost / ::1"]
    Engine --> PTY["User terminal PTY — isolated from AI and MANVI"]
```

## Guarantees

**No remote phoning home.** No network request without an explicit user action. The webview does not load CDN scripts or trackers.

**GitHub credentials.** GitPulse never requests, reads, stores, or transmits GitHub tokens. PR / Actions / Dependabot features delegate to the `gh` CLI you already authenticated.

**Local AI only.** Completions and model probes are restricted to loopback. A remote base URL is rejected at the transport layer so diffs and file contents do not leave the machine.

**Terminal isolation.** The embedded PTY (`portable-pty`) is not reachable by AI models or the MANVI sidecar. Remediation actions (`cmd_manvi_run_action`) use a command allowlist and require confirmation.

**Opt-in release checks.** Off by default. When enabled: `git ls-remote` against public tags, at most once a day. No tokens, repo paths, or hardware telemetry. Never downloads an installer.

**CSP** (from `src-tauri/tauri.conf.json`): `default-src 'self'`; `connect-src 'self' ipc: http://ipc.localhost`. Remote scripts and `eval` are disallowed.

**Policy fail-closed.** A missing MANVI harness yields **unchecked**, not **allowed**. A check that could not run must never look like a check that passed. See [[Policy and AI]].

**MCP is read-only.** `gitpulse-mcp` does not checkout, write files, or take leases. See [[MCP and Agents]].

## Reporting a vulnerability

Do not open a public issue.

**[Open a GitHub Security Advisory](https://github.com/bharathvbcr/GitPulse/security/advisories/new)**

Full policy: [docs/SECURITY.md](https://github.com/bharathvbcr/GitPulse/blob/main/docs/SECURITY.md).
