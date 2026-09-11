# GitPulse Security Model & Policy

GitPulse is a **local-first, zero-telemetry** developer desktop application.
Repository and task state stay in local stores. Git remotes, GitHub operations,
optional tool installation and configured agent providers can use the network.

**Product stack.** DevCouncil is components and modules. Manvi wraps them.
GitPulse uses Manvi for policy, workbench, and agent hosting, and selected
DevCouncil modules for code intelligence. Take or update only the modules this
app needs.

```mermaid
flowchart TD
    subgraph Boundary["Security & Isolation Boundary"]
        Webview["Tauri Webview<br/>(Strict CSP: default-src 'self')"]
        IPCBoundary["Tauri IPC Seam<br/>(Policy-Checked Custom cmd_* Handlers)"]
        LocalEngine["Rust Core<br/>(Validated paths and command gates)"]
        
        Webview -->|IPC Only| IPCBoundary
        IPCBoundary --> LocalEngine
    end

    subgraph ExternalSurfaces["External Surface Isolation"]
        LocalGH["Local <code>gh</code> CLI<br/>(Uses existing local keychain)"]
        LocalAI["Local LLM Server<br/>(Loopback 127.0.0.1 / localhost Only)"]
        PTY["User Shell / Explicit Agent PTYs<br/>(Separate sessions)"]
    end

    LocalEngine --> LocalGH
    LocalEngine --> LocalAI
    LocalEngine --> PTY
    LocalEngine --> ProfileManvi["Profile Manvi Host<br/>(Configured provider / managed runs)"]
```

---

## 1. Core Security Guarantees

### Zero Telemetry & No Remote Phoning Home
- GitPulse has no centralized backend or analytics tracking.
- Network-capable features run through user actions or explicitly enabled settings,
  including scheduled release checks, automatic task suggestions, and the
  default-on GitHub alert scan (Dependabot and code scanning via the local `gh`
  CLI when a repository opens). A failed GitHub check does not toast.
- The webview does not load external CDN scripts, styles, or telemetry trackers.

### Local `gh` Credential Safety
- GitPulse never requests, reads, stores, or transmits your GitHub personal access tokens or passwords.
- All GitHub operations (PR inspection, workflow dispatch, Dependabot and code scanning queries) delegate exclusively to your locally installed and authenticated `gh` CLI.

### Loopback-Only Local AI Transport
- Built-in local AI completions (commit messages, explanations and branch names)
  and local model probes restrict their transport to loopback addresses
  (`127.0.0.1`, `localhost`, `[::1]`); remote base URLs are rejected on that path.
- Task enhancement and agent execution use the separately configured profile Manvi
  provider or agent CLI. Their network and filesystem access follow that provider
  and the selected run settings. The local-completion transport restriction does
  not establish containment for an explicitly launched agent.

### Terminal & Process Isolation
- The embedded interactive terminal (`src-tauri/src/terminal/`) runs user shell processes directly via `portable-pty`.
- Local AI suggestions and the policy sidecar do not read/write ordinary shell
  sessions. Explicit Claude, Manvi, Codex and task launches create dedicated PTY
  sessions; the launched process receives that session's input.
- Terminal handoffs remain user-controlled. Managed Codex runs use the separate
  Manvi configuration/decision protocol. A worktree is not an OS sandbox, and
  native launch validation alone does not prove the provider's effective policy.
  See [Tasks and workspaces](TASKS_AND_WORKSPACES.md) for the verification limits.
- Model-assisted remediation actions (`cmd_manvi_run_action`) are restricted to a strict command allowlist and require explicit user confirmation.

### GitHub Alert Scans
- Opening a repository fetches Dependabot and code scanning alerts through the
  locally authenticated `gh` CLI. This is on by default under **Settings →
  Analysis**. GitPulse still does not read or store GitHub tokens.
- Only critical and high findings raise a warning. A check that could not run
  (missing `gh`, not a GitHub remote, API error) is listed in Health and recorded
  in diagnostics, never presented as an all-clear.
- **Scan local** on the Health panel does not call GitHub. The **Check GitHub
  alerts** button remains a manual refresh.

### Opt-In Release Checks
- Automatic application release checks are off by default; GitPulse does not
  automatically install application updates.
- When explicitly enabled under **Settings → Updates**, GitPulse compares public release tags once a day via `git ls-remote` against the upstream repository.
- No user tokens, repository paths, or hardware telemetry are ever sent.
- Release checks only notify with a link to GitHub releases. Optional-tool setup
  is a separate, explicitly requested install/update workflow.

### Webview Content Security Policy (CSP)
The webview operates under a strict CSP configured in `src-tauri/tauri.conf.json`:
- `default-src 'self'`
- `connect-src 'self' ipc: http://ipc.localhost`
- Remote scripts and inline `eval` are strictly disallowed.

---

## 2. Reporting a Vulnerability

If you discover a security vulnerability in GitPulse, please report it via GitHub Security Advisories rather than filing a public issue:

👉 **[Open a Security Advisory](https://github.com/bharathvbcr/GitPulse/security/advisories/new)**

We take security issues seriously and will respond promptly to investigate and patch confirmed vulnerabilities.

## 3. Unresolved Dependency Advisory

As of 2026-09-08, **GHSA-wrw7-89jp-8q8g / RUSTSEC-2024-0429 remains
unresolved** in `src-tauri/Cargo.lock` (`glib 0.18.5`). The upstream advisory
describes undefined behavior in `VariantStrIter` and identifies `0.20.0` as the
first fixed version. See the [RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2024-0429.html)
and [upstream fix](https://github.com/gtk-rs/gtk-rs-core/pull/1343).

**Verified:** Tauri's Linux GTK3/WebKitGTK stack resolves `gtk 0.18.2` and
`webkit2gtk 2.0.2`, which require `glib 0.18` and `^0.18.0` respectively.
`0.18.5` is the latest published release on that compatible line. Cargo rejects
the suggested `0.20.0` update with `failed to select a version for the requirement
glib = "^0.18"`. Reproduce without changing the lockfile:

```sh
cargo tree --manifest-path src-tauri/Cargo.toml --locked --target all -i glib@0.18.5
cargo update --manifest-path src-tauri/Cargo.toml -p glib@0.18.5 --precise 0.20.0 --dry-run
```

The Apple Silicon macOS dependency graph has no `glib` edge; this does not clear
the advisory for Linux builds. The [Wry GTK4 port](https://github.com/tauri-apps/wry/pull/1767)
and [Tauri GTK4 port](https://github.com/tauri-apps/tauri/pull/14684) were still
unmerged when checked. A resolution requires either a compatible maintained
backport of the upstream fix or a compatible migration of the parent stack.
Adding `glib 0.20` directly cannot replace GTK3's incompatible dependency.

**Unverified:** reachability of the affected iterator in a running Linux build.
The alert remains open; no advisory suppression or version override is applied.
