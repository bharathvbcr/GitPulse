# Control-Plane Migration Plan

**Goal:** consolidate GitPulse, DevCouncil, and Manvi into one agentic Git control plane by assigning a single owner per shared artifact and making the other two repos consumers. No rewrites; ownership decisions plus six shippable phases, each a user-visible feature on its own. Grounded in two source-level audits of all three codebases (September 2026).

**The one rule:** every shared artifact (verdict schema, task store, code graph, event ledger, verification gates) gets exactly one canonical owner. Consumers link, exec, or speak a versioned contract. Parity is machine-enforced — extend the existing Manvi ↔ DevCouncil generated parity-test pattern (256 command cases, 775 fnmatch cases) to GitPulse.

---

## 1. Artifact Ownership Map

| Artifact | Canonical owner | Consumers | Today | Action |
|---|---|---|---|---|
| **Policy verdict schema** (RuleID × Severity × outcome + grants) | Manvi — `manvi/policy/decision.go`, `manvi/gate/gate.go` | GitPulse (`harness/policy.rs` via sidecar), DevCouncil | DevCouncil has its own allow/warn/deny in `execution/policy_engine.py`; parity tests already generated | Freeze Manvi's schema as `verdict.schema.json` v1; DevCouncil emits it; GitPulse maps 1:1 (already 5-state) |
| **Task + lease store** | DevCouncil — `.devcouncil/state.sqlite`, schema owned by Manvi's `crates/dc-store` | Manvi (links dc-store), GitPulse (new) | Manvi's Rust reader is stricter than DevCouncil's Python writer; GitPulse has nothing | Extract `dc-store` as a standalone crate; GitPulse links it directly (both Rust); migrations move to dc-store; Python keeps SQLModel as a consumer |
| **Code graph / repo map** | DevCouncil — rust-port `devmap` | Manvi (already execs `devmap`), GitPulse (new) | Three implementations: Python graph, rust-port, Manvi bridge | GitPulse **links the crates in-process** (§8); Python graph frozen to maintenance once devmap reaches parity on top languages. **No indexing code in GitPulse, ever** — there will be no fourth implementation |
| **Event ledger** | GitPulse (new, Rust) — it owns the resident watcher + UI | Manvi session logs and DevCouncil `traces.jsonl` feed it via importers | GitPulse: 200-entry browser array (`src/lib/agents/activity.ts`); Manvi: `session/log.go` invariant; DevCouncil: `telemetry/traces.py` | Define `event.schema.json` v1; SQLite ledger in Rust (Phase 1); importers (Phase 3) |
| **Verification / rigor gates** | Manvi — `crates/dc-verify` (Rust) | DevCouncil (its 50-module Python suite becomes the spec), GitPulse | Duplicated Python/Rust | Port remaining checks Python→dc-verify incrementally; both products exec the same binary |
| **Grants ledger** | Manvi — `grants/grants.go` | GitPulse UI (new) | GitPulse never shows grants | Add `grants.list` op to the serve protocol; render in the Manvi view (Phase 4) |

Additional Manvi crates GitPulse should reuse directly rather than reinvent: **`dc-grep`** (repo search on ripgrep's own libraries — ignore-aware, reports `files_searched`/`skipped`, no `rg` binary dependency; a search feature GitPulse lacks entirely) and **`dc-glob`** (Python-fnmatch semantics, needed to evaluate `planned_files` scopes identically to the policy engine).

---

## 2. Validation spike (before Phase 0, ~1 week)

The plan is grounded in two source-level audits; the remaining unknowns are empirical, not readable. Four spikes de-risk the whole sequence before any contract work starts. Throwaway code — none of it ships.

| # | Spike | Effort | What it proves | Acceptance |
|---|---|---|---|---|
| S1 | **devmap link test** | 1 day | `devmap-query` + `devmap-store` as path dependencies in `src-tauri`, one `impact` query answered from a Tauri command. License (rust-port is MIT at workspace level) and edition (both 2021) are verified compatible; what reading can't tell you is whether transitive deps (rusqlite versions, tree-sitter linkage) collide with Tauri's tree — only `cargo build` answers that | Workspace compiles; `impact` on a real file returns edges in-process |
| S2 | **Ledger throughput** | ½ day | Write the Phase 1 schema, replay a busy day of synthetic `Guarded<T>` events through WAL SQLite | Append latency invisible next to git subprocess cost (<1 ms p99) |
| S3 | **Transcript parse** | 1 day | Throwaway parser over real `~/.claude/projects/` JSONL: what fraction of tool-use events map cleanly to worktree paths | ≥90% of Edit/Write/Bash events attributable to a repo path; validates or kills Phase 3's attribution assumptions |
| S4 | **Protocol round-trip** | ½ day | Send a `scope` object through `policy.check.command` to a patched `manvi serve` (4-line op change per the audit) and watch `scope.unplanned` fire | A GitPulse-originated mutation outside `planned_files` returns the rule verdict end-to-end |

If a spike fails, it changes the plan cheaply now instead of expensively mid-phase: S1 failure → fall back to the IPC daemon (§8.2); S3 failure → attribution degrades to reflog + watcher only; S4 failure → the `task_id` wiring moves from Phase 2 to a Manvi-side workstream first.

**Deferred audit that IS still worth doing:** a security review of the ledger/ingest design before Phase 3 ships — transcripts are a secrets-bearing input crossing a trust boundary. Premature today; it reviews a design that doesn't exist yet.

---

## 3. Phase 0 — Contracts (prerequisite, ~1 week)

Create a `contracts/` home (suggest: top-level dir in DevCouncil, vendored into the other two repos).

1. `verdict.schema.json` v1 — transcribed from `manvi/policy/decision.go` + the five gate outcomes (`Passed / Blocked / Granted / Demoted / Degraded`) + `checked` flag semantics from GitPulse's `harness/policy.rs` (the "allowed vs. nobody-checked" distinction is kept).
2. `event.schema.json` v1 — the ledger event (Phase 1).
3. `lease.schema.md` — documents `state.sqlite` tables as implemented in `crates/dc-store/src/schema.rs`, including the `ux_task_leases_active` partial unique index Manvi verifies at runtime.
4. Contract tests in all three repos asserting serializers against these files. GitPulse already has the muscle: point `scripts/policy-status-contract.test.ts` / `command-policy-contract.test.ts` at the shared schema instead of local copies.

Rules: schemas are versioned and additive-only; breaking changes require a new version consumed side-by-side.

Two GitPulse conventions to respect throughout the whole plan:

- **Wire-type discipline is enforced:** `scripts/wire-type-locality-contract.test.ts` fails the build on any snake_case interface declared inside a `.svelte` file (empty allowlist). All new payload types — badges, annotations, run timelines, task bindings — go in `src/lib/**/types.ts` modules.
- **New views are genuinely one registry entry:** `VIEW_REGISTRY` + a `ViewTab` union member auto-registers the header tab, native menu item, ⌘N accelerator, and palette command (guarded by `view-menu-contract.test.ts`). Adding **Work** / **Runs** surfaces is cheap. Counterpoint: the command palette has **no** `registerCommand()` API — commands are two hardcoded arrays in `CommandPalette.svelte`; build a small registry before integrations start wanting palette entries.

**Done when:** all three repos' CI fails if any serializer drifts from the shared schema.

---

## 4. Phase 1 — Durable ledger in GitPulse (the unlock, ~2–3 weeks)

The Rust backend currently persists nothing — no SQLite, no app-data dir. Provenance is a 200-entry array in browser memory. This phase turns the UI into a projection of a disk-backed, append-only event ledger.

### Storage

- `rusqlite` (WAL). Per-repo ledger at `.devcouncil/ledger.sqlite` (joins the existing shared-state convention) plus a small global DB in app-data for cross-repo queries. Single writer: the GitPulse process; importers go through the same writer.

### Write path — hook the Rust seam, not `runMutating()`

`repoStore.runMutating()` is the canonical frontend mutation seam but covers only ~80% of writes. Audit-confirmed bypasses that never flow through it: worktree add/remove/lock/unlock/prune (`WorktreesPanel`), `cmd_write_file_content` (FileTreePanel, CodeViewer, **and** ConflictEditor), `cmd_restack` (CodeStackViewer manually duplicates the verdict/journal calls — visible evidence of the cost of bypassing), `cmd_rebase_interactive`, `cmd_github_checkout_pr`, `cmd_manvi_run_action`, and the interactive PTY (deliberately ungated, documented at `TerminalPanel.svelte:110-115`).

**Therefore:** append events in Rust wherever a `Guarded<T>{policy, output}` envelope is produced — `harness/mod.rs::guard_command` / `guard_file` and `engine/git_writer.rs`. The verdict vocabulary is already contract-checked; the ledger just makes it durable. **Redaction at write time, not display time:** port Manvi's credential regexes from `dc-verify/src/rigor.rs` into `ledger/redact.rs`, applied to `argv_json` and `detail_json` before insert.

### Read path (replaces the ring buffer)

New `ledger-appended` Tauri event + `cmd_ledger_tail(cursor, limit)`. `harnessStore.actions` becomes a projection; delete `MAX_AGENT_ACTIONS = 200` as a data cap (keep a display cap only). This is Manvi's `session/invariant.go` property applied to the UI — everything visible is a projection of the log — and the pattern that stops the 103-command IPC surface from doubling: prefer ledger projections over new commands.

### Schema (v1)

```sql
CREATE TABLE events (
  id             INTEGER PRIMARY KEY,   -- monotonic cursor
  ulid           TEXT NOT NULL UNIQUE,
  ts_utc         TEXT NOT NULL,
  schema_version INTEGER NOT NULL DEFAULT 1,
  repo_path      TEXT NOT NULL,
  worktree_path  TEXT,
  actor_kind     TEXT NOT NULL CHECK (actor_kind IN ('human','agent','system')),
  actor_id       TEXT,          -- 'claude-code', 'codex', 'bharath', ...
  session_id     TEXT,          -- agent session / transcript id
  task_id        TEXT,          -- DevCouncil task id, else NULL
  action         TEXT NOT NULL, -- 'git.commit','git.push','file.edit','ci.step',...
  object         TEXT,          -- ref / file / argv digest
  argv_json      TEXT,          -- REDACTED argv
  outcome        TEXT NOT NULL CHECK (outcome IN ('ok','failed','blocked')),
  verdict_json   TEXT,          -- {rule_id,severity,outcome,checked,grant_id}
  before_ref     TEXT,
  after_ref      TEXT,
  duration_ms    INTEGER,
  detail_json    TEXT           -- REDACTED, per-action payload
);
CREATE INDEX idx_events_repo_ts  ON events(repo_path, ts_utc);
CREATE INDEX idx_events_task     ON events(task_id)    WHERE task_id IS NOT NULL;
CREATE INDEX idx_events_session  ON events(session_id) WHERE session_id IS NOT NULL;
```

**Early feature this enables — F2, commit-readiness gate:** `dcverify` is a standalone JSON-on-stdio binary needing no Manvi state: unified diff in, structured findings out (secrets / stubs matched against **added lines only**, so pre-existing TODOs don't fail a diff / scope classification / coverage intersection). An unparseable diff is exit 2, never an empty result — "could not run" is never readable as "clean". Run it on the staged diff at commit time; findings render inline in the DiffViewer gutter (Phase 2's annotation channel).

**Done when:** kill the app mid-rebase; on restart the full action history, with verdicts, is intact and continues appending.

---

## 5. Phase 2 — Wire `task_id` for real (~1–2 weeks)

The protocol already anticipates this: `harness/protocol.rs:81` carries `task_id` in every `RawDecision`, and GitPulse's `PolicyVerdict` conversion **drops it on the floor** (`policy.rs:229` fabricates `"host-scope"`). Only the request side and the UI join are new work.

1. **Protocol:** add an optional `scope` object to `policy.check.*` requests — `{"task_id","lease","worktree"}`. Manvi's `serve/policy.go` threads it into the gate, activating rules that exist today but never fire from GitPulse: `task.absent`, `scope.unplanned`, `scope.read_only`, `task.forbidden_change`.
2. **Binding:** on `cmd_add_worktree`, optionally bind a checked-out DevCouncil task (read leases + planned files via the linked dc-store crate). Record the binding as a ledger event; every later mutation in that worktree resolves its `task_id` from it.
3. **UI:** `WorktreesPanel` shows bound task, lease TTL, planned-scope summary, verdict counts. A worktree stops being a directory and becomes a task in flight — the first change-centric surface.

**Features this enables (built on Tier-0 file reads, §8.3):**

- **F1 — Run Review surface.** DevCouncil's checkpoints are **real git refs** (`refs/devcouncil/tasks/<task>/before|after|attempts/<n>`), run manifests are `.devcouncil/runs/<id>/agent-run.json`, and trace events are tailable JSONL at `.devcouncil/logs/traces.jsonl`. GitPulse can render a full agent-run timeline with revertible diffs using its existing diff machinery plus two JSON reads — and surface the supervise verdict (`keep / revert / repair`) with one-click revert via `dev runs revert`.
- **F4 — Review-cards inbox.** Glob `.devcouncil/live/cards/CARD-*.json` (schema `devcouncil.critique_card.v1`); blocking cards badge the relevant branch/commit; resolve via `dev watch resolve <id>`.

**Done when:** an agent edit outside `planned_files` in a task-bound worktree produces a visible `scope.unplanned` verdict, and the ledger row carries the task id.

---

## 6. Phase 3 — Passive attribution + catch-up (~3 weeks)

Provenance without requiring any agent to integrate — and derived-from-observation attribution is more trustworthy than self-reported.

1. **Transcript ingestion:** new `src-tauri/src/ingest/` watching `~/.claude/projects/<slug>/*.jsonl`; parse tool-use events (Edit/Write/Bash) with timestamps and paths. Version-gate the parser; unknown format degrades to "unattributed", never errors. Other agents' logs join behind the same trait. Note: Manvi session files are whole-document-per-generation with checksums — **not externally tailable**; ingest them at save time or via a future `session.tail` serve op (poll-shaped), never by watching partial writes.
2. **Join:** transcript events × existing watcher events × `git reflog` × worktree paths → `session.*`, `file.attributed`, `commit.attributed` ledger events.
3. **Reflog catch-up:** on repo open, scan the reflog since the last ledger cursor and synthesize events for everything that happened while GitPulse was closed. Correctness never depends on residency — Git itself is the recovery source.
4. **"While you were gone":** the first ledger-projection screen — sessions, commits, verdicts, moved refs since last close, per repo and across repos.
5. **Commit trailers** (cheap, do here): `GitPulse-Actor`, `GitPulse-Session`, `DevCouncil-Task` appended by `git_writer` for bound sessions.
6. **F8 — Audit timeline:** join reflog × ledger in the Reflog view (git's record vs GitPulse's verdicts — two datasets minutes apart that never meet today), and add "restore to here". The Reflog view is currently a flat 132-line read-only table; this is its purpose.

**Done when:** a Claude Code session run with GitPulse closed appears fully attributed — files, commits, diffs — on next open, summarized in the catch-up screen.

---

## 7. Phase 4 — Close the gate bypasses, surface grants, host agents (~2–3 weeks)

If the positioning is "trust boundary between agents and the repo," there can be no privileged unlogged execution path. The Phase 1 audit produced the concrete checklist: `ci_local.rs`, `cmd_manvi_run_action`, the seven `runMutating` bypasses, and the PTY.

1. **`ci_local.rs`:** currently bypasses the MANVI gate by design (it fails closed on non-git commands). Manvi's command ladder already handles arbitrary argv — route each CI step through `policy.check.command` with an explicit `ci.local` allow rule instead of a bypass. Every step logged with its verdict. Same treatment for `cmd_manvi_run_action`.
2. **Grants surfaced:** add `grants.list` / `grants.revoke` ops to the serve protocol; the Manvi view renders grantor, reason, expiry; every Granted/Demoted ledger event links its `grant_id`.
3. **F6 — Agent launcher.** Two modes, both nearly free:
   - *Interactive:* `spawn_session` hardcodes `$SHELL` with no args — **parameterize program/argv/env (one parameter)** and GitPulse hosts agent CLIs (Claude Code in a tab). The backend session registry is already a `HashMap` (supports N sessions); App-level PTY keep-alive already exists. Keep the ungated-PTY labeling honest; `terminal-isolation-contract.test.ts` polices this area.
   - *Headless:* `manvi run --json` emits the same typed NDJSON event stream the TUI renders (`session.start, tool.start, approval.request, policy.decision, lease.change, turn.usage, run.report, …`), credential-scrubbed, per-sub-agent attribution, 6-value exit status. Spawn it and render the stream in the Manvi view. One Manvi enhancement makes GitPulse a real operator: headless mode currently uses `DenyAll` for soft blocks — **add `--approver stdio`** so `approval.request` blocks awaiting a JSON decision on stdin, answered from GitPulse's approval UI.
4. **F5 — Model manager.** Add a `local.scan` serve op: `llm/local/endpoints.go::Scan` already probes well-known endpoints concurrently (~2 s), identifies the runtime (ollama / llama.cpp / vllm / lm-studio) *by asking, never by port*, and lists usable models — none of it is on the wire today (`capability.probe` requires already knowing the answer). Combine with probe-style "tool calling actually works" verdicts as badges in the AI settings.

**Serve-protocol mechanics (applies to 2–4):** the op table is a hardcoded `switch` (`serve/server.go:269`); adding an op is a 4-line change, `ProtocolVersion = 1` explicitly permits additive ops, and `hello` returns `ops[]` for feature detection. Constraints: dispatch is **serial** (a slow op blocks all others) and there is deliberately **no streaming line** — event-shaped data must be poll-based or a second child process. `serve` imports nothing from `grants`, `session`, or `agents`; each new op crosses a documented scope boundary — decide deliberately.

**Approval-UI semantics to port from Manvi's TUI** (decisions, not code): approvals are modal on purpose (a queued keystroke must not answer one) and **queued, never stacked**; a hard rule gets *acknowledge*, never a fake allow; `Subject` distinguishes path/command/question; `Decision.Answered()` is separate from `Allow` so an unreachable seam is never recorded as a human choice. Plus: a persistent status bar (posture, weakened safety flags, lease-expiry countdown) rather than toasts, and a `manvi doctor`-style system panel — *effective* flags with origin layers, dependency reachability (dcstore/dcverify/devmap), credential presence (source + length, never the key), and a `WEAKENED` block listing every safety default that's off.

**Done when:** the ledger shows a verdict for every process GitPulse ever spawned; active grants are visible; an agent CLI runs in a tab; a headless Manvi run renders live with GitPulse answering approvals.

---

## 8. Phase 5 — devmap integration + Git-native provenance (~3–4 weeks)

### 8.1 devmap: link the crates, skip the daemon

GitPulse is Rust and DevCouncil's `rust-port/` workspace is MIT-licensed. **Depend on `devmap-query` + `devmap-store` directly** and call `StoreQueryEngine::new(&store)` in-process: `search / dependencies / impact / trace / trace_between / dead_symbols`. This eliminates the socket, the daemon lifecycle, and the 30-min idle self-retirement entirely. Incremental updates: GitPulse already runs a `notify` watcher per repo — on debounced change, shell `devmap build --affected <paths> --deleted <paths>`. GitPulse becomes devmap's keeper for repos DevCouncil isn't driving.

### 8.2 If the IPC daemon is ever used instead — the traps (all confirmed in source)

Newline-delimited JSON over a Unix socket, one request per connection, 6 read ops (`status, search, deps, impact, trace, dead`), `Response<T>` envelope with `tokens_used` and `resolution`.

1. **Two socket-path conventions.** Rust default: `$TMPDIR/devmap-{fnv1a64(root)}/ipc.sock`. Python client: `/tmp/devmap-{sha256(root)[:16]}.sock`. A daemon DevCouncil started lives at the sha256 path. Spawn your own with explicit `--socket` to own the endpoint identity.
2. **The `--db` default is wrong for the Rust store** — it points at `index.sqlite` (Python schema, rejected with "unsupported schema version 2"). Always pass `--db .devcouncil/codeintel/devmap.sqlite`. This exact bug (SC23) silently disabled DevCouncil's own hybrid path for months.
3. **`devmap manifest` writes empty freshness stamps** (`indexed_hash: ""`, `content_fingerprint: ""`; the Python layer patches them afterwards). If GitPulse drives `devmap manifest`, it must stamp all three fingerprints (head SHA, sorted-tracked-files sha1, size/mtime_ns sha1) or every consumer reads the map as permanently stale.
4. **Check `map_engine` before trusting empty fields.** The Rust engine emits `neighbors: []`, `role_files: {}`, `handoff_paths: []`, `unwired_candidates: []` — empty means *not computed*, not "computed zero". Only the Python mapper populates them.
5. **Call-graph coverage:** solid for C-family, JS/TS, Python, Go, Rust; ~26 other grammars give declarations + imports only. Degrade features per language honestly — mirror the health panel's `scanners_ran` pattern.

### 8.3 Tier-0: DevCouncil artifacts readable with no process at all

`.devcouncil/repo_map.json` (subsystems, entry_roots, dependents with truncation marker `dependents_total`), `.devcouncil/graph/code_graph.json` (check `meta.compatibility_export_tier`; if `stub`, use SQLite), live cards, run manifests, traces JSONL, and the checkpoint git refs (§5). Staleness is computable in Rust with the three-fingerprint rule — no Python. One live warning from the audit: DevCouncil's own `repo_map.json` and `code_graph.json` currently come from *different builds*. Never assume the two artifacts agree; prefer the SQLite/crate path for queries and treat the JSON as display-grade.

**Concurrency etiquette:** against DevCouncil's MCP server or state, GitPulse is a **read-only consumer**. Never call `checkout_task`, `write_file`, `apply_patch`, or `graph_ingest` from the UI process — those contend with an active agent's writer lease and task leases. Safe set: `status`, `repo_map`, `impact`, `code_*`, `live_*`, `run_timeline`, `run_supervise`, `list_agent_runs`.

### 8.4 Features the devmap link enables

- **F3 — Impact-aware diff:** blast radius + affected tests per changed file in the diff header; "run affected tests" through the gated runner; per-hunk **enclosing symbol** from devmap spans (byte offsets → line map).
- **F7 — Structure health:** dead symbols, cycles, god nodes, and unused-dependency detection (manifest deps × import edges) piped into the Health view's existing `HealthIssue {severity, code, message, path?}` shape — new `code` values render with zero UI work. Keep the "could not check ≠ clean" discipline.
- **F9 — Repo search:** dc-grep-backed search across worktrees, palette-integrated (any time; independent of devmap).

### 8.5 Git-native provenance

SQLite is the index; Git is the durable substrate. Provenance survives GitPulse's absence and travels with clones.

1. **Verification artifacts:** an in-toto-style attestation per verification run (local CI, dc-verify, DevCouncil `verify_task`) — subject commit/tree, target base, commands, results, env — stored under `refs/notes/gitpulse/verification`, indexed in the ledger. One format, emitted by dc-verify, consumed by both products.
2. **Confidence decay:** staleness = distance between the attestation's target base and current main, weighted by whether moved commits touch the verified diff (blast-radius via the devmap link; path overlap until then). Rendered as a freshness badge on branches and PRs.
3. **Episode index:** session summaries — observable actions + agent-provided summary, never chain-of-thought — under `refs/notes/gitpulse/sessions`, keyed by commit.
4. **Model-as-judge, if ever used:** adopt Manvi's verdict contract (`verdict.go`) — no line is no judgement, unreadable is no judgement, disagreement is no judgement, PASS-with-findings downgrades. No judgement is never a pass.

**Done when:** a fresh clone on a second machine shows verification badges and session provenance from refs alone; the diff header shows blast radius and affected tests.

---

## 9. Phase 6 — Headless core + MCP (only after 1–5, ~4+ weeks)

Read-only MCP 2.0 (`2026-07-28`) and the Agent Plugins 1.0 package now exist: `gitpulse_insights`, `gitpulse_change_context`, `gitpulse_active_changes`, `gitpulse_collision_risk`, plus ledger / codeintel / provenance. Mutating tools (`checkpoint_workspace`, `report_blocker`) are still out — they must go through `harness::guard_command`.

1. **Extract `gitpulse-core`:** `engine/`, `harness/`, `watcher/`, `diff/`, ledger into a crate; the Tauri app links it; a thin `gitpulsed` serves it headless. Reuse the NDJSON-over-stdio transport from `manvi serve` — don't invent a new one.
2. **MCP server exposing context, not Git wrappers:** `get_change_context`, `get_active_changes`, `checkpoint_workspace`, `report_blocker`, `get_collision_risk`, `prepare_review`. *(The first three read-only names shipped as `gitpulse_*`.)*
3. **Proxy, don't duplicate:** DevCouncil already exposes ~78 MCP tools including `checkout_task` and `verify_task`. GitPulse's MCP proxies those and adds only what is uniquely its own: worktree fabric, ledger queries, diff and verification context.
4. **F10 — Work view:** the change-centric surface from the original vision — tasks (dc-store) × worktrees × PRs × runs × verdicts in one screen. Everything above feeds it; it lands last because it is a pure projection. *(Shipped.)*

**Done when:** with the desktop app closed, an agent calls `get_change_context` and receives task scope, drift, verdicts, and parallel-work warnings; opening the app later shows everything the agent did.

---

## 10. Per-view feature matrix

Each row: view → the seam that exists today → the feature to build (phase in parentheses).

| View | Existing seam | Feature |
|---|---|---|
| **Graph** | `refsByCommit: Map<id, RefItem[]>` decoration pattern; `CommitGraphPayload.refs?` is the additive-optional model to copy; `cmd_get_commit_graph` already runs 3 concurrent fetches in `thread::scope` — a 4th drops in | Commit provenance badges (task/agent/run/verdict) as a parallel `badgesByCommit` map. DOM chips in `CommitRow` = hours; canvas badges = days (the tile cache `computeCacheKey` must include them or they go stale) — DOM first (P3) |
| **Diff** | `selectedDiff: string` is the bottleneck; `DiffPayload` + `normalizeDiffPayload()` in graphStore is the **half-built richer transport**; `AnnotatedDiffLine` is mutable (lazy `segments` precedent); `railTicks` minimap | Migrate to `DiffPayload`; add `annotations?: DiffAnnotation[]` per line (provenance, dcverify findings, impact); plot risk/coverage on the rail; per-hunk enclosing symbol (P1–P5) |
| **Blame** | Coverage-per-line overlay already proven (`hitBadgeClass` gutter badge) | Third gutter column: actor/task per line from the ledger — the highest-leverage single field in the app; symbol-level blame via `contains` edges (P3, P5) |
| **Conflict** | `ConflictChunk` has line ranges + `{Custom: string}` resolution variant | `enclosing_symbol` per chunk ("conflict inside `LaneSolver::solve`"); side attribution (which task/agent wrote ours/theirs); record provenance of `Custom` bodies (P3–P5) |
| **Health** | `HealthIssue` is a generic finding shape; strict honesty contract (`scanners_ran`, `audit_complete`) | F7 structure health (P5) |
| **Coverage** | `CoveredLine {line_no, hits}`; churn exists separately (`cmd_branch_stats`) | Test→symbol edges via `affected_tests`; churn × coverage risk ranking; "you changed X → run these N tests" (P5) |
| **Stack** | `StackedBranchNode` is pure topology; `PullRequestInfo.head_ref` loaded in the same app | **PR ↔ branch join is pure client-side, zero backend — do it first.** Then task binding per node turns Stack into the work queue; restack-conflict prediction via symbol overlap (P2, P5) |
| **Storage** | `WorktreeUsage {path, name, branch, bytes}` | Worktree ↔ task/lease binding; "safe to reclaim" from lease expiry instead of merge heuristics; artifact provenance (P2) |
| **Reflog** | Flat read-only table, no actions | F8 audit timeline + "restore to here" (P3) |
| **Manvi** | `AgentActionEntry` ring buffer; designated agent surface, currently thinnest | Run timeline (F1), lease table, grants ledger, rule catalog with "why was this blocked" drill-down (P2–P4) |
| **GitHub** | `head_ref`/`base_ref` are display text only | PR ↔ local branch join (ahead/behind, "this PR is your checkout") — client-side, zero backend; prerequisite for task ↔ PR linkage (P2) |
| **Files** | `FileRow` badge path exists (git status); `FileBlob` has no structure | Symbol outline rail; per-file fan-in/fan-out + dead-code badges; lease badges; subsystem overlay from `repo_map.subsystems` (P2, P5) |
| **Terminal** | `portable_pty` fully wired; `spawn_session` hardcodes `$SHELL`; backend registry already `HashMap` | F6 agent launcher — one parameter (P4) |
| **AI features** | `run(Feature, repo, selection, |budget| -> Turn)` closure is the single context seam; `Feature` keys the token-calibration ledger | Context enrichment: callers of changed symbols, affected tests, parallel-task warnings. **New context shape = new `Feature` variant** — mutating an existing one poisons calibration (P5) |

---

## 11. Feature index

| # | Feature | Phase |
|---|---|---|
| F1 | Run Review surface — timeline, checkpoint-ref diffs, supervise verdict, one-click revert | 2 |
| F2 | Commit-readiness gate — dcverify on the staged diff, findings in the gutter | 1–2 |
| F3 | Impact-aware diff — blast radius + affected tests per change | 5 |
| F4 | Review-cards inbox — critique cards badge branches/commits | 2 |
| F5 | Model manager — `local.scan` discovery + tool-calling verdict badges | 4 |
| F6 | Agent launcher — CLIs in PTY tabs + headless `manvi run --json` with GitPulse as approver | 4 |
| F7 | Structure health — dead code, cycles, god nodes, unused deps | 5 |
| F8 | Audit timeline — reflog × ledger with restore-to-here | 3 |
| F9 | Repo search — dc-grep-backed, palette-integrated | any |
| F10 | Work view — tasks × worktrees × PRs × runs × verdicts | 6 |

---

## 12. Explicitly frozen

- **No fourth code graph.** No AST/indexing work in GitPulse; DevCouncil's Python graph frozen once devmap reaches parity on top languages.
- **No CoW filesystems, GPU, swarm scheduling, or CRDTs** until Phase 6 ships. Normal worktrees + shared package caches are enough.
- **No 14-state ChangeIntent lifecycle.** Change identity is *inferred* (worktree + branch + session cluster + optional DevCouncil task) before it is ever declared. Manual curation nobody feeds is a Jira clone.
- Housekeeping: prune the stale worktree at `.claude/worktrees/agentic-git-repo-system-8540d4`.

## 13. Risks

| Risk | Mitigation |
|---|---|
| Schema drift across three repos | Phase 0 contracts + generated parity tests, extending the existing Manvi pattern to GitPulse |
| `state.sqlite` dual-writer corruption | dc-store owns migrations; Manvi-style index verification (assert the partial unique index exists, don't trust DDL); Python writes move behind the dc-store CLI over time |
| Transcript format changes upstream | Version-gated parsers; unknown format → "unattributed", never an error |
| Secrets in the ledger | Write-time redaction reusing the dc-verify credential scan |
| devmap artifact drift | Prefer crate/SQLite queries over the JSON exports; check `map_engine` + `compatibility_export_tier`; stamp fingerprints when driving `devmap manifest` |
| Serve-protocol scope creep | Serial dispatch + no streaming: keep new ops fast and poll-shaped; each op crossing a package boundary is a deliberate decision |
| Solo bandwidth | Every phase independently shippable and user-visible; stopping after any phase still leaves the product better than before |

## 14. Sequencing

| Phase | Ships as | Size |
|---|---|---|
| S · Validation spike | S1–S4 de-risk results; go/no-go on crate link, ledger perf, attribution, protocol | ~1 wk |
| 0 · Contracts | CI drift protection across all three repos | ~1 wk |
| 1 · Ledger | Durable, crash-safe action history + F2 commit gate | 2–3 wk |
| 2 · task_id | Task-aware worktrees + F1 run review + F4 cards + PR↔branch join | 1–2 wk |
| 3 · Attribution | "While you were gone" + agent attribution + F8 audit timeline | ~3 wk |
| 4 · Gates + agents | Complete trust story + grants UI + F5 models + F6 launcher | 2–3 wk |
| 5 · devmap + Git-native | F3 impact diff + F7 structure health + verification badges | 3–4 wk |
| 6 · Headless + MCP | Automation surface + F10 Work view | 4+ wk |

**The wedge is Phases 1–3:** durable, attributed, task-aware repo state — the demo no competitor currently has. GitKraken and GitButler manage agent sessions; none can answer, from deterministic local state, *who changed what, under which task, with which verdict, while you were away.* And the second audit's net effect is that Phases 2–5 got cheaper: checkpoints are already git refs, cards and runs are already JSON files, the verdict already carries `task_id`, PR↔branch is a client-side join, the PTY needs one parameter, and devmap is linkable in-process rather than daemon-managed.
