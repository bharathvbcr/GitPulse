# MANVI Harness & Local AI Integration

GitPulse integrates with the **MANVI coding-agent harness** as an embedded sidecar process (`manvi serve`) communicating over standard I/O via NDJSON.

```mermaid
flowchart TD
    subgraph UI["GitPulse Frontend"]
        ActionReq["User Triggers Git Action / AI Request"]
        Badge["Header Badge: Policy Posture & Verdicts"]
    end

    subgraph Rust["Rust Backend IPC Seam"]
        GuardSeam["<code>guard_command()</code> / <code>guard_file()</code>"]
        AiEngine["AI Engine & Token Budgeter<br/><code>src-tauri/src/ai/</code>"]
        Allowlist["Action Execution Allowlist<br/><code>cmd_manvi_run_action</code>"]
    end

    subgraph Harness["MANVI Sidecar (<code>manvi serve</code> via NDJSON)"]
        CmdGate["Command & Write Gate"]
        Probe["Capability Probe (Context Window)"]
        Prepare["Chat Prepare (Compaction Ledger)"]
        Settle["Chat Settle (Reasoning Separation)"]
    end

    subgraph LocalLLM["Local LLM Server (Loopback Only)"]
        Ollama["Ollama / LM Studio / llama.cpp / vLLM"]
    end

    ActionReq --> GuardSeam
    GuardSeam --> CmdGate
    CmdGate -->|Verdict Ladder| GuardSeam
    GuardSeam --> Badge

    ActionReq --> AiEngine
    AiEngine --> Probe
    AiEngine --> Prepare
    AiEngine -->|Direct HTTP POST /chat/completions| LocalLLM
    LocalLLM --> Settle
    Settle --> AiEngine
    AiEngine --> UI

    ActionReq --> Allowlist
    Allowlist --> CmdGate
    CmdGate -->|Gated Execution| Allowlist
```

---

## 1. Policy Verdict Ladder

Every mutating Git operation (commit, push, rebase, branch deletion, worktree prune) passes through the MANVI policy gate. Verdicts fall onto a strict 5-stage ladder:

```mermaid
stateDiagram-v2
    [*] --> PolicyEvaluation
    
    state PolicyEvaluation {
        direction TB
        Evaluate: Inspect Command & Context
    }

    PolicyEvaluation --> Allowed: Safe & Conforming Action
    PolicyEvaluation --> Demoted: Downgraded to Safe Variant (e.g. non-destructive)
    PolicyEvaluation --> Warned: Potential Risk / Caution Advised
    PolicyEvaluation --> Blocked: Hostile, Destructive or Forbidden
    PolicyEvaluation --> Unchecked: Harness Not Installed (Asymmetric Degradation)

    Allowed --> ExecuteAction: Proceed with Mutation
    Demoted --> ExecuteAction: Proceed with Downgraded Action
    Warned --> ExecuteAction: Proceed with Warning Logged
    Blocked --> RefuseAction: Reject Action Loudly
    Unchecked --> ExecuteAction: Proceed with Explicit 'Unchecked' Verdict
```

### Verdict Definitions
- **`Allowed`**: Operation complies with security policy and repo bounds.
- **`Demoted`**: Operation is transformed or downgraded to a safer variant.
- **`Warned`**: Operation proceeds with an explicit advisory logged in the agent activity journal.
- **`Blocked`**: Operation is rejected. Refusal explanation is presented in the UI modal.
- **`Unchecked`**: Harness binary is absent. GitPulse records the verdict explicitly as *unchecked* (never falsely reported as "allowed").

### Asymmetric Degradation Design
- **Missing Harness**: Mutating commands proceed with explicit `unchecked` status.
- **Wedged or Unreachable Harness**: If MANVI is installed but encounters errors or drops connection, mutations are **refused immediately** rather than silently falling back to unchecked execution.

---

## 2. On-Device AI Engine & Budget Planning

GitPulse features on-device AI assistance for commit messages, commit explanations, branch naming, and remediation plans for dependency health and test coverage.

```mermaid
sequenceDiagram
    participant UI as GitPulse UI
    participant AI as Rust AI Engine
    participant Harness as MANVI Sidecar
    participant LLM as Local LLM Server

    UI->>AI: Request Commit Message / Remediation
    AI->>Harness: <code>capability.probe</code> (Fetch context dimensions)
    Harness-->>AI: Context window & token limits
    AI->>Harness: <code>chat.prepare</code> (Calibrate prompt against window)
    Harness-->>AI: Token budget & compaction plan
    AI->>LLM: POST /chat/completions (Strict Loopback Only)
    LLM-->>AI: Raw completion with reasoning & text
    AI->>Harness: <code>chat.settle</code> (Strip <think> tags & validate output)
    Harness-->>AI: Cleaned text & reasoning stream
    AI-->>UI: Display structured output & budget metrics
```

### Self-Calibrating Token Ledger
The AI engine maintains an in-memory token calibration ledger scoped per feature (`commit-message`, `explain-commit`, `branch-name`, `health-fix`, `coverage-report`) and repo path. Observed prompt tokens reported by the local server feed back into the compaction estimator to prevent context-window overflow.

### Loopback-Only Transport Security
All HTTP requests to local model endpoints are hard-coded to loopback interfaces (`127.0.0.1`, `localhost`, `[::1]`). Requests to remote IP addresses or external hostnames are strictly rejected at the transport layer, ensuring diffs and repository context never leave the machine.

---

## 3. Scoped Remediation Allowlist (`cmd_manvi_run_action`)

Remediation scripts proposed by AI for coverage generation or dependency vulnerabilities are executed through a purpose-limited command allowlist:

```mermaid
flowchart LR
    ScriptReq["Model Proposes Remediation Script"] --> ValidateInput["Input Validation<br/>(No shell syntax, no URLs, no path escapes)"]
    ValidateInput --> CheckAllowlist["Allowlist Verification<br/>(Ecosystem-specific runner)"]
    CheckAllowlist --> PolicyGate["MANVI Command Gate Verification"]
    PolicyGate --> UserConfirm["Explicit User Click / Confirmation"]
    UserConfirm --> SafeExec["Bounded Subprocess Execution<br/>(Hard timeout & capped output)"]
```

- **Strict Executables**: Only known ecosystem tools (`npm`, `cargo`, `pip`, `pytest`, `go`, `swift`, `dart`, `mvn`, `gradle`, etc.) are permitted.
- **Direct Argv Only**: Commands are executed directly without spawning an intermediary shell (`sh -c` or `cmd.exe`).
- **Bounded Resources**: Subprocesses enforce strict timeouts (e.g. 60s) and bounded output capture buffers to prevent memory exhaustion.
- **Zero Autonomous Execution**: Nothing runs automatically; each step requires explicit user interaction.
