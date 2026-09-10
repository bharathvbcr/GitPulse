# Agentic workspaces implementation contract

This document preserves the complete user-approved objective. Partial implementation
does not constitute completion. Evidence below must identify the exact check and
its scope; unit tests do not prove native desktop behavior or provider integration.

Design status: complete for the agreed scope. Implementation status: in progress.
The six required areas below remain the acceptance contract; this is not a claim
that all planned capabilities have shipped.

The current user workflow is documented in [Tasks and workspaces](TASKS_AND_WORKSPACES.md).
The source now includes board/list layouts, filters over loaded cards, multi-selection,
context actions, bounded task deletion, notes-to-draft editing, inline Manvi
title/description review, Quick Enhance and saved/draft agent copy. These additions
do not close the broader acceptance contract below. Their helper and component
tests establish local logic and wiring, not installed-provider or physical UI proof.

## Original implementation checkouts and ownership

The paths and starting commits below record the original development worktrees;
they are historical provenance, not required locations for a fresh checkout.

- GitPulse: `/Users/bharath/.codex/worktrees/agentic-workspaces/GitPulse`, branch
  `codex/agentic-workspaces`, starting commit `2fb8efc`.
- Manvi: `/Users/bharath/.codex/worktrees/agentic-workspaces/Manvi`, same branch name,
  starting commit `6fc0faf`.
- Preserve unrelated changes in the canonical checkouts. Manvi owns task storage,
  enhancement, scheduling, attention and review. GitPulse owns presentation and
  native repository, terminal, notification and review adapters.

## Required end state

### 1. Persistent groups and workspaces

- Create, rename, reorder, pin, archive and delete flat named workspaces with
  descriptions, icons and colors. Repositories may belong to multiple groups.
- Add through pickers and drag-and-drop; dragging adds membership, while Move
  explicitly removes the source membership. Include Ungrouped and Recent views.
- Stable repository, checkout and worktree identities; support relinking moved
  paths, unavailable checkouts and remote-only repositories. Never auto-merge
  distinct clones solely because their remotes match.
- Membership survives closing tabs, application restarts and group changes during
  execution. Deleting a group preserves repositories, tasks and history.
- Navigator and workspace overview expose Fleet, Tasks, Agent Activity, Search and
  repository relationships. Group actions report successes, failures and skips.
- Seed Current workspace from existing open tabs on first enablement. Group
  membership must not be bounded by the 24-open-tab limit or start every watcher.
- Reuse DevMap federation with durable membership; retain Open repositories as a
  separate transient scope. Suggested repository relationships require acceptance.

### 2. Three boards over one task store

- Global: all tasks. Workspace: home-workspace or member-repository tasks.
  Repository: linked tasks. Deduplicate every board/count by task identity.
- Types: issue, bug, feature, improvement, maintenance, research, documentation and
  custom. Title, description, priority, severity, owner, labels, dates, criteria,
  checklists, attachments, source links and parent/blocking/related/duplicate links.
- Saved tasks require at least one repository and a primary repository; unlinked
  captures are drafts. Support per-repository objectives and optional home group.
- Inbox, Backlog, Ready, In Progress, Review, Done. Keep run, verification, remote
  issue and publication states separate. An agent cannot accept its own work.
- Board/list, keyboard and accessible drag operations, bulk edits/undo, swimlanes,
  saved views, WIP limits, and Details/Plan/Context/Runs/Review inspector.

### 3. Manvi intelligence

- Separate title/description rewrite diffs with selected acceptance, edit, dismiss,
  field locks and undo. Preserve identifiers, constraints, quoted errors and intent;
  invented causes/results fail quality evaluation. Additions remain identifiable.
- Propose on save after durable persistence and one-second debounce. No model call
  on card moves, selections or recursive acceptance. Use configured provider/model;
  overrides explicit, no automatic cloud fallback or model download.
- Grounding with citations; gap questions and criteria; competing bug hypotheses;
  lexical duplicate candidates plus optional reranking; task DAGs, readiness,
  explainable prioritization, review preparation and accepted compact knowledge.
- Capture GitHub, CI, terminal, diagnostics, Health/Coverage, source and diff evidence
  with revisions and deduplication. Cache selectively; no embedding daemon.

### 4. Managed agents and recovery

- Copy details/brief, terminal handoff, Manvi, Claude Code and Codex runs from one
  versioned brief. Native Manvi loop, Codex App Server, documented Claude CLI host.
- Version/capability negotiation; truthful unsupported controls. External terminal
  sessions stay user-controlled. Typed argv/cwd and bounded prompts, no shell
  interpolation or terminal-text approval scraping.
- Managed launch identity includes canonical repo/worktree IDs, cwd and effective
  root overrides. Normalize conflicting inherited root and permission settings
  for the selected run, then verify the effective configuration without logging
  credentials. A correct cwd alone does not prove which repository an agent uses.
- Inspect/plan, ask, scoped build/test, preapproved-only, provider auto-review and
  explicit advanced per-run bypass. Bypass never silently propagates to retries,
  children, reviewers or defaults. Display actual sandbox/network/filesystem access.
- Full host bypass widens process access: worktrees are not security sandboxes and
  GUI publication rules cannot contain an unrestricted child. UI push/merge/deploy,
  external writes and human acceptance remain explicit actions.
- Durable questions, approvals, plan reviews, change reviews and blocked states.
  Approval binds payload digest, run attempt, session, request, repository, cwd and
  policy revision. Validate before consuming; persist before delivery; expire at
  min(five minutes, provider deadline), never permit on timeout or replay.
- Steering, interruption, continuation, takeover after writer termination, and
  provider handoff as a new attempt. Separate liveness from useful progress.
- Immutable review bundles and separate critic runs; accepting changed code requires
  new review. Preserve failed/unavailable/skipped verification distinctions.
- Cross-repository snapshot, worktree preparation, ordered lease acquisition,
  dependency execution and combined verification. Preserve partial results and
  block dependents; never describe cross-repository publication as atomic.
- Integration queue before explicit publication. Defaults: two global agent jobs
  (configurable to four), one writer workflow per repository, two repair attempts,
  depth two, 50 plan nodes, 45 active minutes and two elapsed hours. Report limits
  that an adapter cannot observe or enforce.
- Two-way GitHub issue sync, PR/review/CI links, explicit outbound preview, durable
  outbox and three-way conflicts; reconcile uncertain delivery before retry.

### 5. Native notifications

- Native delivery for actionable agent questions/permissions, plans/reviews,
  completion/failure/stalls/budgets, optional enhancements, task reminders,
  workspace results/conflicts, GitHub/CI changes and background recovery.
- One durable coordinator, global/workspace/repository/task settings, private
  previews, quiet hours/sound/snooze, grouped results and cross-group deduplication.
- Exact task/run/review activation through startup; validate stale requests.
  Open/Review/View request/Snooze actions where supported. Permission grants and
  bypass changes require the application view, never a stale notification action.
- Inbox persists independently of OS banners; dismissed is not resolved, submitted
  is not seen. Respect OS permission/Focus. Background delivery requires background
  mode. No per-card timer or inference; burst coalescing and durable control events.
- Desktop-specific activation handlers: Tauri's actions API is mobile-only.
  Test real installed app identity, restart activation and unsupported Linux actions.

### 6. Architecture, performance and proof

- One Manvi workbench per GitPulse profile, shared across windows/groups, with
  SQLite row transactions, versioned revisions, idempotency and cursor events.
  Retain the existing per-repository execution tasks and leases as a separate model.
- Typed APIs for workspaces/repositories/items, context/enhancements/plans,
  runs/attention/reviews, events/sync/knowledge. Scoped agent CLI/MCP cannot grant
  itself authority. Preserve GitPulse MCP's existing read-only contract.
- Indexed paginated queries and backend counts; selective subscriptions. Maximum
  200 mounted cards; lazy editors/transcripts/diffs/terminals/graphs. Persist once
  per drop. Closed repositories do not spawn sessions/watchers automatically.
- One lazy worker and shared resource pools; one automatic enhancement at a time,
  20 per hour/profile. Prioritize control events over logs; stop optional work under
  pressure; preserve unresolved decisions and unreviewed worktrees.
- Opt-in background worker with visible activity and Stop all. Otherwise checkpoint
  or cancel on quit, bounded shutdown and process-tree reconciliation.
- Normal fixture: 10k tasks/100 repos/100k events; stress: 100k tasks. Cold board
  <=1.5s, warm switch <=300ms, edit/search p95 <=100ms, drag p95 <=16.7ms,
  additional idle memory <=100MiB, idle CPU <0.5% of one core over five minutes,
  eight-hour bounded soak. Document hardware; report whole process tree and agents.
- Regression/adversarial checks for stale edits/approval/review, duplicate events,
  unavailable/moved repos, membership mutation, concurrent leases, expired live
  writers, lingering children/PID reuse, partial repo failure, protocol/version
  changes, provider outages/quotas, prompt injection, output floods, disk/database
  failure, GitHub races and native notification delivery/activation.
- Required gates: GitPulse `npm run ci:local`, Manvi `verify.sh` plus race and
  relevant adversarial checks, disposable-repository real integration and native
  macOS/Windows/Linux qualification. No dependency additions without approval.

## Implementation evidence

| Area | Current evidence | Remaining |
|---|---|---|
| Workspaces and boards | Manvi profile storage and Go host API; GitPulse lazy native adapter, persistent workspace editor, multi-repository task editor and global/workspace/repository boards implemented. Four native tests passed, including real Git worktree/clone identity checks. Browser interaction verified task edits, shared membership, scoped counts and persistence through reload | Full task fields, checkout identities and relinking; group import/reorder, board/list/bulk/undo/accessibility interactions and native installed-app qualification |
| Manvi intelligence and orchestration | Durable proposals, field locks, editable suggestions preserving original model text, selected acceptance/undo and worker ownership implemented. Schema four adds transactional on-save queueing and preparation through the existing proposal slot/quota. The worker now wakes after text saves, resumes prepared work after restart, and exposes profile settings/status in GitPulse. Nine real-store browser checks passed, including automatic generation after Save and persisted Stop | Complete semantic quality evaluation, context/planning/orchestration and installed native qualification |
| Agent supervision and review | Canonical versioned briefs, durable attempts and checkout/branch checks; Runs panel launches private-file briefs through the existing PTY manager. Schema eight adds durable permission/question bindings, one-use delivery claims and an inspector that distinguishes saved decisions from provider confirmation. Schema nine adds managed Codex with verified configuration, native process activation, durable callback delivery and output. The real read-only native/provider path and 28 browser handoff/review checks pass | Real-account approvals and remaining callback types, managed Claude, launch-time fencing, code review, crash recovery and installed-app qualification |
| Native notifications | Schema seven adds settings, quiet hours, scoped mutes and durable delivery/activation records to the schema-six inbox. GitPulse implements the macOS adapter, opt-in preferences and recovered current/stale task review; eight canonical store tests, twenty frontend tests, two native boundary tests and nineteen real-store browser checks verify these paths | Installed OS delivery/activation, callback crash-window qualification, native Snooze/withdrawal, remaining event producers, other platforms and native resource measurements. [Native adapter contract](NATIVE_NOTIFICATIONS_ADAPTER.md) |
| Performance | Indexed scope counts, bounded body projection and shared FTS hit sets implemented; 93 store tests pass. [Normal and stress benchmarks](AGENTIC_WORKSPACES_BENCHMARK.md) completed with 10k/100k tasks and 100,110 initial events each. Normal scoped search is 14.18/36.77 ms p95; edits are 3.151/2.693 ms for normal/stress | Stress broad global/workspace search is 484.5/527.8 ms p95 and misses the 100 ms target. Profile large matching sets and metadata reads; native rendering, cold/warm navigation, idle CPU/memory and eight-hour soak remain |

Implementation has begun; the full goal remains incomplete.

### Managed startup recovery checkpoint

- **Verified:** schema ten and preparation protocol version two record native
  process identity before the provider handshake. No task turn starts before
  effective settings are verified. Old/future host receipts and unavailable
  processes are refused before activation, with cleanup requested for the helper.
- **Verified:** known pre-spawn failures finish as failed and release repository
  capacity. Generic startup errors remain unresolved; typed nil error sessions
  cannot crash the host. Failed initialization retains native process evidence
  without fabricating a provider thread. Confirmed child reaping and provider
  failure are recorded separately; uncertain reaping retains the reservation.
- **Verified:** the board accepts uninitialized failure records and the durable
  inbox emits a failure notice even when the helper exits with code zero. A failure
  does not change task status/revision, start a replacement, or inherit bypass.
- **Regression evidence:** pre-fix missing-executable capacity retention and a nil
  session panic (`/tmp/manvi-recovery-before.log`); provider readiness accepted
  before native birth (`/tmp/manvi-recovery-schema-before.log`); missing failure
  receipt support (`/tmp/manvi-recovery-finish-before.log`); decoder refusal
  (`/tmp/gitpulse-recovery-client-before.log`); and unsafe old-host activation
  (`/tmp/gitpulse-recovery-native-before.log`). Retained tests pass after the fixes.
- **Verified:** canonical dc-store tests and Clippy; Go protocol/host/CLI race
  suites; full Go vet/tests (4,107 passed, seven skipped, 45 tested packages);
  112 workbench frontend tests; 28 native workbench tests (three separately opt-in);
  Svelte/TypeScript checks. The installed Codex read-only native integration test
  passed against rebuilt Manvi/schema-ten binaries and left the task in Inbox.
  Logs: `/tmp/manvi-recovery-store.log`, `/tmp/manvi-recovery-race.log`,
  `/tmp/manvi-recovery-go.jsonl`, `/tmp/gitpulse-recovery-native.log`,
  `/tmp/gitpulse-recovery-native-live.log`.
- **Verified browser:** all 30 handoff/review/recovery checks passed with zero
  recorded runtime errors. Initial runs were interrupted by development reloads
  during report/document writes; the complete settled run is saved in
  `/tmp/gitpulse-recovery-browser.json`. Simulated terminal events still emit
  Tauri callback warnings. This fixture does not qualify OS notification delivery.
- **Full gates:** GitPulse `ci:local` exited zero: 5,304 frontend tests (one
  skipped), 2,090 Rust tests (twelve ignored), 24 standard browser regressions,
  contracts/types, formatting, Clippy, production build and coverage floors.
  Coverage was 96.14% frontend lines, 88.94% frontend branches and 84.13% Rust
  lines. The selected dc-store vendor matches canonical Manvi; unrelated DevMap
  upstream drift remains explicitly reported by the existing allow-drift check.
- Manvi `verify.sh` exited zero with a **qualified pass**: 233 Rust tests,
  4,114 discovered Go tests (seven skipped), 81.6% Go coverage and 869 benchmark
  checks. RustSec and the configured local-provider wire check passed. Missing
  golangci-lint/debt ratchet, govulncheck, nilaway, the legacy DevCouncil environment,
  and live Anthropic/Gemini/xAI wire checks remain unverified. The flat graph export
  is still generation 1; refreshed DevMap stores reached Manvi 20 and GitPulse 18,
  with incomplete walks. Logs: `/tmp/manvi-recovery-verify.log`,
  `/tmp/gitpulse-recovery-ci.log`, `/tmp/gitpulse-recovery-vendor.log`.
- **Open:** unactivated-helper and legacy host-crash reconciliation, escaped
  descendants, writer fencing, managed Claude, real-account approvals, richer
  review and orchestration, the remaining board/workspace features, installed
  notification delivery and native resource/performance qualification. This
  checkpoint adds no dependency and changes no global permission preference.

### Managed Codex checkpoint

- **Verified:** schema nine separates terminal and managed run claims and retains
  immutable provider settings/thread/turn identity. Native launch accepts a run ID,
  revalidates its checkout and observes the actual process creation identity
  before Manvi sends one task turn. The renderer cannot forge lifecycle receipts.
- **Verified:** the stdio adapter captures complete command/file/question requests,
  validates decisions before consuming delivery claims, observes buffered provider
  resolutions, and never retries an uncertain response. Secret questions, broad
  grants and unknown callback methods fail without authorization.
- **Verified:** managed start/retry/stop, effective-settings and output views are
  connected to task details. Question sets have independent answer fields and
  suggested choices. History excludes output/settings; those load on demand.
- **Verified live:** installed Codex 0.153.4 completed an ephemeral read-only
  handshake, a direct marker turn and the native GitPulse → Manvi → Codex → store
  flow in a disposable checkout. The native flow retained task revision 1/Inbox
  and replaying launch returned the same completed attempt. Evidence:
  `/tmp/manvi-managed-live-handshake.log`, `/tmp/manvi-managed-live-turn.log`,
  `/tmp/gitpulse-managed-native-live.log`.
- **Verified with scripted providers:** protocol and Go host race suites, 111
  workbench frontend tests, 27 native workbench tests and 28 browser interaction
  checks pass. The browser fixture had zero recorded runtime errors and emitted
  Tauri callback warnings during simulated terminal events. It is not an OS or
  live-account approval test.
- **Verified regression:** a 160-frame output burst reproduced an incorrect
  cancellation when the 64-frame buffer filled. The reader now waits for its
  bounded consumer; it retains frame/total-output caps and cancellation. The
  regression and full coding-protocol/host race suites passed after the fix.
- GitPulse `ci:local` passed: **5,303 frontend tests** (one skipped), **2,089 Rust
  tests** (twelve ignored), **24 standard browser regressions**, type/contract/
  vendor/schema checks, Clippy, formatting, production build and coverage floors.
  Coverage: frontend lines **96.14%**, branches **88.92%**, Rust lines **84.11%**.
  Evidence: `/tmp/gitpulse-managed-ci.log`.
- Manvi `verify.sh` completed with a **qualified pass**: 231 Rust tests, 4,104
  discovered Go tests (seven skipped), 81.5% Go coverage and 869 benchmark checks.
  After the final output-buffer fix, fresh `go vet ./...` and `go test ./...`
  passed: **4,098 tests passed**, seven skipped, 45 packages; the protocol/host
  race suites also passed. The two installed-Codex opt-in tests skipped by the
  general suite were executed successfully in the separate live checks above.
  Logs: `/tmp/manvi-managed-verify-final.log`,
  `/tmp/manvi-managed-go-final.jsonl`, `/tmp/manvi-managed-backpressure-after.log`.
- Unrun verifier gates remain explicit: golangci-lint/debt ratchet, govulncheck,
  nilaway, legacy DevCouncil environment interop, and live Anthropic/Gemini/xAI
  wire checks. RustSec and the configured local-provider wire check passed.
  The flat graph export remains generation 1; refreshed DevMap stores are
  GitPulse generation 17 and Manvi generation 19. Graph walks remain incomplete.
- **Open:** real-account approval modes and additional protocol callbacks, managed
  Claude, process-tree termination/recovery, launch fencing, richer reviews,
  installed notifications, workspace/board completion and performance targets.
  Direct-child reaping alone is not proof that escaped descendants stopped.
- No new dependency, release, installation, commit or global permission change
  belongs to this checkpoint. Existing per-attempt bypass acknowledgment remains.

### Structured agent request checkpoint

- Manvi schema eight captures one immutable permission request or question with
  a run, task/repository revision, owner/session, provider thread/turn/request,
  complete payload and SHA-256 digest. The Go client hashes exact captured and
  retained payloads. Pending requests expire within five minutes; each run is
  bounded to 32 unresolved requests and 2048 total. Migration from seven is atomic.
- Human response, delivery claim and provider resolution are separate states.
  A saved response cannot be delivered twice by replaying its claim after a lost
  reply or restart. Stale run/task/repository/identity/payload bindings are refused.
  Questions accept an answer or denial; they cannot grant a tool permission.
  GitPulse's renderer cannot capture callbacks, consume claims or forge resolution.
- The run inspector exposes one request panel, 30 records per page and 180 loaded
  records maximum, reusing existing visible-run refresh. It hashes the request
  before saving, preserves the exact captured text, and retains the original
  request after an uncertain save. Opening/reviewing a notice never accepts work.
- Permission/question notices use private generic titles. Their native delivery
  eligibility and inspector validity share the same store predicate. The
  repository-change regression failed with `current` instead of `changed` before
  this was unified; it passes afterward. A browser regression also reproduced
  JSON reformatting losing duplicate fields and large-number precision. The
  inspector now displays the original payload bytes as text without reformatting.
- Verified: all **139 dc-store tests**, including nine decision cases, and
  all-target Clippy passed. The Go decision/restart test passed against real
  `dcstore` child processes under the race detector. These are storage/transport
  binding tests, not proof of a managed provider callback.
- Verified in a disposable real profile: Allow once, Deny and a typed answer
  persisted at decision revision two while task revision one and Inbox status
  remained unchanged. Changing the repository removed all pending approval
  actions. The retained browser handoff fixture passed **21 checks** with zero
  runtime errors, including exact payload display, lost-reply retry, stale
  controls, reopening receipts and per-attempt bypass acknowledgment. Its process
  and provider responses are simulated. Evidence:
  `/tmp/gitpulse-decisions-browser-evidence.json`.
- Manvi `env -u DEVCOUNCIL_ROOT ./verify.sh` returned a qualified pass with
  **230 Rust tests**, 4090 Go cases (five skipped), 82.1% Go statement coverage,
  and 53 real-binary store tests. Missing golangci-lint/lint-debt, govulncheck and
  nilaway checks, absent sibling Python lease environment, the generation-one
  flat graph versus generation-fifteen index, and scripted cloud-provider wire
  checks remain explicit gaps. RustSec audit and the local provider wire probe
  passed. Log: `/tmp/manvi-decisions-verify.log`.
- GitPulse validation passed **5301 frontend tests** (one skipped), **2087 Rust
  tests** (eleven ignored), and **24 existing browser regressions**, including
  the native test refusing forged renderer callbacks/claims. Type checks, IPC,
  vendor/schema contracts, formatting, Clippy and production build passed.
  `ci:local` itself exited with a missing frontend coverage file when a later
  UI verification overlapped its coverage gate. After that run finished, the
  gate was rerun serially and passed: frontend lines **96.13%**, branches
  **88.96%**, Rust lines **84.24%**. The final exact-payload UI change also passed
  fresh type checks, all frontend tests/coverage and a production build. Logs:
  `/tmp/gitpulse-decisions-ci.log`, `/tmp/gitpulse-decisions-ui-final.log` and
  `/tmp/gitpulse-decisions-coverage-final.log`. The full-profile browser reload
  retained all three saved responses, removed stale approval controls and
  displayed the exact stored payload, with zero runtime errors.
- DevMap impact answers remain incomplete lower bounds; GitNexus did not resolve
  the newly added symbols/fixture. Source and execution tests were used directly.
  No dependency, installed application, global permission preference, commit or
  release was changed by this checkpoint.
- At this earlier schema-eight checkpoint, managed Codex and Claude adapters still needed effective
  policy, capture live callbacks, consume a decision only while its callback is
  open, confirm provider resolution and recover owned processes. Existing launch
  buttons at that checkpoint were terminal handoffs with terminal-owned permissions. Installed
  native notifications, task planning/orchestration and performance goals remain
  part of the full objective.

### Native notifications checkpoint

- Verified in source and tests: one profile coordinator applies saved opt-in
  preferences, private previews, local quiet hours and workspace/repository/task
  mutes. Disabled delivery has no periodic wake; enabled delivery checks every
  five seconds in batches of three. There is no per-card timer, model or watcher.
  Performance limits are structural bounds, not measured idle CPU/memory results.
- A durable claim is committed before OS submission. Replays cannot submit again;
  missing callbacks remain uncertain, while definite callbacks record submitted
  or failed. Renderer requests cannot author claim, finish or activation records.
  Native Open task activation validates the saved profile and delivery identity,
  then opens current task data with an explicit stale-target explanation.
  Acknowledging the review view does not accept a proposal or complete a task.
- The bounded activation queue can lose a click if the process crashes before
  its record is saved. Saved activations recover through reload/restart; this
  does not prove receipt of every OS callback. The inbox remains available.
  Persist-before-OS-acknowledgment and installed click-after-quit qualification
  remain explicit work in the [adapter contract](NATIVE_NOTIFICATIONS_ADAPTER.md).
- The final deleted-mute regression first failed with
  `invalid_input: mute scope does not exist`. The canonical fix accepts known
  tombstone identities while preserving unknown-ID refusal and current-target
  delivery checks. The UI can clear saved mutes without changing sound or quiet
  hours, and the action remains a draft until Save.
- Verified: the final Manvi Rust workspace has **221 passing tests**, including
  eight notification regressions; workspace all-target Clippy and formatting pass.
  Logs: `/tmp/manvi-notifications-rust-tests-final.log` and
  `/tmp/manvi-notifications-rust-clippy-final.log`. The earlier full
  `env -u DEVCOUNCIL_ROOT ./verify.sh` returned a qualified pass: 218 Rust tests at
  that checkpoint, 4089 Go tests with five skipped, 82.1% Go coverage and 52
  real-binary store tests. The final Rust checks include the three subsequently
  added regressions; the Go implementation did not change afterward.
- The full Manvi gate did not cover golangci-lint/lint-debt ratcheting,
  govulncheck or nilaway because those tools were unavailable; incumbent Python
  lease interop lacked the sibling virtual environment; the flat navigation
  graph was generation 1 versus DevMap generation 14. Anthropic/Gemini/xAI wire
  checks used scripted servers. RustSec audit and a local provider wire request
  did pass. These limitations are recorded in `/tmp/manvi-notifications-verify.log`.
  Focused `go test -race ./dc/store ./serve ./cmd/manvi` also passed with CGO enabled
  and the inherited root override removed (`/tmp/manvi-notifications-race.log`).
- Verified on the final implementation: GitPulse `npm run ci:local` passed with
  **5283 frontend tests passed, one skipped; 2086 Rust tests passed, eleven
  ignored; and 24/24 browser regressions passed**. Svelte/TypeScript, IPC/type
  contracts, vendor/schema checks, production build, Rust formatting and
  all-target Clippy passed. Coverage floors held at **96.12% frontend lines,
  88.96% frontend branches and 84.22% Rust lines**. Log:
  `/tmp/gitpulse-notifications-complete-ci.log`. Documentation was reconciled
  afterward; its eight Manvi contract checks and both repository whitespace
  checks passed (`/tmp/manvi-notifications-docs-final.log`).
- Verified: nineteen browser checks used a disposable real profile, including
  quiet-hour/mute persistence, current/stale activation recovery, unchanged
  task/proposal state, draft-only mute clearing and bounded inbox layout. The
  final runtime error count was zero. The fixture explicitly reports unavailable
  OS delivery and event updates; seeded activation records are not OS evidence.
  Evidence: `/tmp/gitpulse-native-notifications-browser-evidence.json`.
- Unverified: installed bundle identity/permission/Focus behavior, real banners
  and action clicks, click-after-quit, Windows/Linux native delivery and measured
  native idle resources. Managed coding-agent permissions/review, the remaining
  event producers, stress-search optimization and the broader plan remain open.

### Durable activity inbox checkpoint

- One canonical transaction records each eligible control event, its inbox entry
  and mutation receipt. Replay cannot create a duplicate. Failure to write the
  notice rolls the source outcome back instead of silently losing attention.
  Only small control metadata is projected into generic private preview text,
  including when a valid source/result body exceeds 256 KiB.
- Global, workspace, repository and task queries use indexed, bounded pages with
  exact counts. Shared repositories do not duplicate notices. Current target and
  task revisions are resolved on reads; deleted or changed targets stay explicit.
- Read/unread, dismiss/restore and one-hour UI snooze affect only the inbox. The
  store accepts bounded snooze durations and revision/idempotency checks. These
  APIs cannot approve provider requests, accept proposals or mark tasks Done.
- GitPulse mounts 30 notices only when the inbox is open. Updates use coalesced
  event/focus hints and existing automatic-worker updates. No notice owns a
  timer, model call, process or repository watcher. This earlier checkpoint
  preceded the native adapter and preference controls described above.
- Verification and OS adapter qualification are recorded separately. The user
  approved the four macOS bindings, which are now declared with selected features
  and existing locked versions. All-target Clippy, formatting and the dependency
  comparison passed. Native delivery remains to be implemented; details are in
  [the adapter contract](NATIVE_NOTIFICATIONS_ADAPTER.md).
- Verified: nine new Rust inbox tests cover atomic outcome/notice/receipt writes,
  replay, scopes and pagination, revision conflicts, snooze boundaries, stale and
  deleted targets, schema migration and a result larger than 256 KiB. Nineteen
  frontend tests cover metadata validation, bounded requests and uncertain-reply
  retry identity. Eleven real-store browser checks cover the three board scopes,
  read/filter/snooze/dismiss/restore, task opening and state persistence through
  reload. The proposal remains `ready` and the linked task remains revision one
  after all those actions. The browser adapter has no native event bus or optional
  model host; it reports those limits, and uses manual refresh for this check.
- Manvi `env -u DEVCOUNCIL_ROOT ./verify.sh` passed with stated exclusions:
  213 Rust tests, 82.0% Go coverage, 4,088 collected Go tests (five skipped),
  51 real-binary store tests and a successful local-provider wire request.
  Focused `go test -race ./dc/store ./serve ./cmd/manvi` passed. Missing optional
  analysis tools, incumbent lease interop, stale flat navigation and live cloud
  providers retain the preceding checkpoint's unverified status. Logs:
  `/tmp/manvi-attention-verify.log`, `/tmp/manvi-attention-race.log`.
- GitPulse `npm run ci:local` passed: 5,262 frontend tests across 402 files
  (one skipped), 2,084 Rust test passes across 60 result groups (11 ignored),
  type checks, production build, formatting and all-target Clippy. The final
  coverage gate reported 96.13% frontend lines, 88.98% frontend branches and
  84.70% Rust lines, all above the required floors. Log:
  `/tmp/gitpulse-attention-validated-ci.log`. These local checks do not qualify
  native OS delivery, live provider permissions or the outstanding performance
  stress and installed-platform gates.
- Browser evidence: `/tmp/gitpulse-attention-browser-evidence.json`. The preview
  uses a disposable Vite cache to avoid collisions with concurrent verification.
  An existing signal-cleanup regression exposed cancelled optimizer writes after
  `server.close`; eager cleanup plus final cleanup after the event loop drains
  now passes all four preview lifecycle tests, including failed startup.
  The existing CSS token contract caught and prevented undefined theme tokens.

### Terminal handoff and lifecycle checkpoint

- Task details now expose repository/checkout, Claude Code or Codex, and six
  explicit permission modes. A prepared attempt retains the canonical saved brief;
  private temporary files keep large task text out of argv. Native capability
  probes require the exact option and requested value in that option's help block.
  Bypass requires acknowledgment on that attempt and resets to Ask afterward.
- The existing PTY manager owns execution. Its run observer claims once just
  before spawning, records the actual PID and OS creation identity before output,
  and writes the process outcome without accepting the task. A lost launch reply
  can reconnect through the native run binding without launching a second child.
  Frontend reconnect preserves the process and scrollback; a new task terminal
  does not also create a default shell. Ended attempts require a new explicit run.
- Task children normalize conflicting Git root variables and child-only
  `DEVCOUNCIL_ROOT`, preserving authentication configuration and ordinary terminal
  settings. Unit coverage checks this command environment; it does not establish
  a live provider's complete effective configuration.
- Reaping evidence is distinct from exit code. Failed waits retain uncertainty.
  Shutdown refuses new requests while allowing already-owned observers to write
  final receipts into the existing store. Session cleanup waits for the callback
  before releasing its slot. The shutdown regression failed against the prior
  code with a reaped child still recorded as `running`, then passed after the fix.
- Run history pages newest-first with deterministic ties and exact counts. The
  inspector polls active attempts only while visible. The browser fixture passed
  all 14 checks with three simulated task processes, four launch requests, zero
  default shells, zero kills during reconnect and zero runtime errors. It covers
  lost preparation/launch replies, duplicate opens, acknowledgment reset and
  paused/resumed polling; its native transport is simulated.
- Verified focused evidence: 20 native workbench tests passed (one existing live
  host check ignored), 184 frontend workbench/terminal tests passed before the
  additional reaping-boundary regression, and all 18 PTY event-bus tests passed
  after that regression. Explicit installed-provider help probing passed for both
  providers and all six modes. No live coding task was sent to either CLI.
- Evidence: `/tmp/gitpulse-handoff-shutdown-before.log`,
  `/tmp/gitpulse-handoff-shutdown-after.log`,
  `/tmp/gitpulse-handoff-reconnect-before.log`,
  `/tmp/gitpulse-handoff-frontend-tests.log`,
  `/tmp/gitpulse-handoff-capability-before.log`,
  `/tmp/gitpulse-handoff-installed-help.log`, and
  `/tmp/gitpulse-handoff-reap-wire-after.log`. The final browser result is saved
  in `/tmp/gitpulse-handoff-browser-passed.json`.
- Manvi `verify.sh` reached a qualified PASS on the current store revision:
  204 Rust tests, 82.1% Go coverage, 4,087 collected Go tests with five skipped,
  869 benchmark-instrument checks and a real local-provider wire request. Focused
  Go race checks for `./dc/store ./serve ./cmd/manvi` passed. Missing golangci-lint
  and its lint-debt check, govulncheck, nilaway, incumbent DevCouncil lease interop,
  stale flat navigation output and live cloud-provider checks remain unverified.
  Logs: `/tmp/manvi-handoff-final-verify.log` and
  `/tmp/manvi-handoff-final-race.log`. The initial verifier invocation incorrectly
  supplied a root override to the entire test process and failed disposable-root
  tests; the corrected invocation removes that override. Its misdirected
  `hello.txt` fixture was preserved outside the worktree.
- The final GitPulse `npm run ci:local` gate passed: 5,243 frontend tests
  (one skipped), 2,084 Rust tests across 60 reported groups (11 ignored),
  24 browser diagnostics, production build, formatting and all-target Clippy.
  Coverage floors passed at 96.12% frontend lines, 88.95% frontend branches
  and 84.70% Rust lines. IPC checked 190 registered handlers; the type contract
  checked 884 fields. Log: `/tmp/gitpulse-handoff-validated-ci.log`.
- Remaining: structured provider approval and review channels, observed effective
  account/configuration policy, startup recovery and process-tree reconciliation,
  durable retry of failed receipt writes, host-session namespaces across process
  restarts, temporary-file cleanup after crashes,
  termination proof for uncertain starts, notifications, and installed macOS,
  Windows and Linux qualification. Worktrees are not security sandboxes. CLI help
  and scripted PTY tests do not prove live provider policy enforcement.

### Durable attempts and native preparation checkpoint

- Schema five persists run metadata and one immutable canonical source brief.
  Preparation, lifecycle changes, events and receipts share the canonical
  transaction. A consumed claim cannot be replayed as a second launch, including
  across process restart and concurrent store connections. Deleted tasks retain
  their run history; terminal outcomes leave task state unchanged.
- Native `runs.prepare_terminal` observes a real working checkout before calling
  the store. Native `runs.claim` compares cwd, Git directories, branch reference
  and commit, then consumes the canonical claim. Tests reject missing checkouts,
  changed sources and branch switches at the same commit; an unsuccessful probe
  does not consume the claim. Quoted paths and trailing spaces are preserved.
- Verified: 112 canonical store tests, all-target store clippy, and Go race tests
  for `./dc/store ./serve ./cmd/manvi`. Manvi `verify.sh` reached a qualified PASS:
  203 Rust tests, 82.1% Go coverage, 4,087 Go tests collected with five skipped,
  869 benchmark-instrument checks, and a real local provider wire request.
  Missing golangci-lint/lint-debt checks, govulncheck, nilaway, incumbent
  `../DevCouncil/.venv`, stale flat navigation export (generation 1 versus index
  12), and live cloud-provider qualification remain explicit gaps.
- Verified: GitPulse `npm run ci:local` passed with 5,230 frontend tests and one
  skip, 24 browser regressions, and 2,074 Rust tests with ten ignored. That run
  measured frontend lines/branches at 96.06%/88.85% and Rust lines at 84.72%.
  A subsequent quoted/trailing-space path regression exposed whitespace trimming
  in the native Git probe; the correction passed all 13 native workbench tests
  (one existing live-host test ignored), format check and all-target clippy.
  Frontend source was unchanged after the full gate; the full gate was not
  repeated for that final path correction.
- Evidence logs: `/tmp/manvi-runs-headref-store.log`,
  `/tmp/manvi-native-runs-race.log`, `/tmp/manvi-native-runs-verify.log`,
  `/tmp/gitpulse-native-runs-ci.log`, `/tmp/gitpulse-native-path-before.log`,
  `/tmp/gitpulse-native-path-fixed.log`, and `/tmp/gitpulse-native-path-clippy.log`.
- Remaining: actual private-file/PTY handoff and its UI, verified effective CLI
  policies and child environment, process identity and startup/exit ordering,
  termination proof for uncertain starts, and native notifications. Current Git
  identity checks are observations, not locks against external writers or proof
  against an otherwise identical clone being substituted at the same path.
  No coding agent was launched and no installed-platform behavior was qualified
  by these preparation tests. The full six-area objective remains active.

### Canonical brief and copy checkpoint

- Manvi owns `items.brief.get`: one read transaction captures the exact saved
  task, every ordered repository reference and the optional home workspace.
  The caller supplies the saved task revision. Repository/workspace revisions
  remain explicit even when their changes do not change the task revision.
- Markdown preserves task instructions and criteria and includes metadata and
  primary-repository identity. Repository lookup identities and remote URLs are
  omitted. Stale/deleted tasks, inconsistent links and oversized output fail
  explicitly; no event, receipt, model worker or partial export is produced.
- GitPulse replaces its frontend brief formatter with the canonical read and a
  typed response validator. **Copy saved brief** identifies excluded unsaved
  edits, refuses stale revisions, and is disabled during uncertain saves or
  active writes in the editor. Closing an editor prevents a pending read from
  later copying its result.
- Verified before implementation: three new store tests failed with
  `unknown_method`; the native regression also failed against the old vendored
  store. Seven brief tests now pass, including unchanged-read determinism,
  repository renames, deletion, corrupt links, and a Unicode/escaped-text brief
  larger than 256 KiB with every criterion preserved. All 100 store tests pass.
- Verified Go race tests cover persistent-child transport, restart byte equality
  and typed stale refusal. The initial combined command incorrectly named
  `./cli`; store/serve passed, and the CLI run at `./cmd/manvi` subsequently passed.
- Verified native test: reopened storage returns the same brief, includes both
  linked repository names, refuses a stale revision, and starts no model worker.
  Five frontend boundary tests cover complete ordered vectors, format/record
  mismatches, invalid text, response identity and error propagation.
- Verified rendered browser workflow with real disposable storage: both linked
  repositories appear in captured output, unsaved text stays excluded with an
  explicit status, a concurrent saved edit refuses copying while preserving the
  previous capture and unsaved draft, and reopening the latest revision restores
  copying. The fixture captures text inside the page; this does not qualify the
  installed OS clipboard. Initial preview startup hit a stale Vite dependency
  import during concurrent CI startup; restarting the disposable preview resolved
  it. The completed interaction checks reported zero runtime errors.
- GitPulse `npm run ci:local` passes: 5,230 frontend tests (one skipped), 24 browser
  regressions and 2,069 Rust tests (ten ignored), plus type/build/format/Clippy and
  coverage gates. Frontend lines/branches are 96.06%/88.85%; Rust lines 84.65%.
  The ten optional native suites remain unexecuted by this gate.
- Manvi `./verify.sh` passes with recorded gaps: 82.1% Go coverage, 4,086 collected
  Go tests (five skipped), 191 Rust tests and 869 benchmark-instrument checks.
  The local provider wire probe passed. Missing lint/nil/vulnerability tools,
  incumbent lease interoperability, stale flat graph versus index generation,
  and live cloud-provider tests remain unverified. These are not rewrite-quality
  or whole-application performance results.
- No dependency, schema migration, permission change or additional worker was
  introduced. Terminal brief files, checkout validation, managed execution,
  review, notifications and the other remaining contract areas are unfinished.

### Query performance checkpoint

- Verified regressions: selective counts no longer scan unrelated task rows;
  search selects a bounded page before formatting bodies. A new tripwire failed
  before the fix when a discarded candidate's body was read, and now passes.
- A first attempt to optimize scoped search was falsified by the expanded
  benchmark: repeatedly opening MATCH per member cost 233.4 ms p95 for only
  100 repository matches. A separate retained regression reproduced 12.47
  seconds. One shared hit set now replaces those repeated evaluations.
- SQLite VM steps exclude virtual-table callback work. Four original selective
  count bounds remain unchanged; scoped common-term cases now budget one FTS
  hit-set pass and have separate elapsed-time and host benchmark coverage.
- The complete store suite passes 93 tests, including an independent reference
  across 64 scope/status/search combinations, pagination on both search paths,
  overlapping membership, changed links, renamed text and deletion. Clippy passes.
- The real Go client/debug Rust child benchmark completed 100 samples for each
  of ten operations on both the 10k-task and 100k-task fixtures. It checks exact
  counts/page sizes and one event per committed edit. The historical 171.4 ms
  global search result did not reproduce in the fresh baseline (21.93 ms); final
  normal global search is 23.93 ms. Do not claim that case improved.
- All measured normal-fixture operations are under 100 ms p95. Stress-scale
  search remains above target. Benchmark PASS denotes execution/correctness;
  it does not certify latency, native UI, installed-app or process-tree budgets.
- GitPulse vendors the same canonical store implementation. No new dependency,
  schema migration, worker, commit, release or installation is part of this change.
- Manvi `./verify.sh` completed with a qualified pass: 82.1% Go coverage, 4,085
  collected Go tests (five skipped), 184 Rust tests and 869 benchmark-instrument
  checks. The live local-provider wire probe passed. Missing lint/nil/vulnerability
  tools, incumbent lease interoperability, the generation-one flat navigation
  artifact versus index ten, and live cloud-provider coverage remain unverified.
  The focused Go store/serve/CLI race suites also passed.
- GitPulse `npm run ci:local` passed: 5,225 frontend tests (one skipped), 24/24
  browser regressions and 2,068 Rust tests (ten explicitly ignored), plus build,
  type, formatting, Clippy and coverage gates. Frontend line/branch coverage is
  96.06%/88.82%; Rust line coverage is 84.64%. The ignored native-to-Go profile
  test was then run explicitly with freshly built binaries and passed, covering
  shared revisions/settings, stale-generation refusal and shutdown. Remaining
  opt-in timing, deep-fuzz, live-update and real-repository suites were not run
  by this checkpoint. No installed desktop or live rewrite-quality result is claimed.

### Automatic worker and board controls checkpoint

- Verified source and regression: manual preparation supersedes only its task's
  queued suggestion. Two tests failed before the fix. Transaction rollback
  preserves queued work, and receipt replay leaves later text saves queued.
- Verified Go race tests: committed text saves wake one coordinator; card moves
  do not repeat inference. Prepared automatic proposals resume after restart;
  already claimed attempts and provider failures are never automatically replayed.
  Missing model selection preserves queued work. Competing hosts, disable during
  inference, shutdown and maximum-length proposal identities are covered.
- GitPulse saves through its existing native store and delivers a coalesced wake
  hint afterward. Wake failure is visible separately from the successful save.
  Active board startup resumes queued work. Profile controls expose enablement,
  inherited/explicit provider and model, queue count, stop and resume.
- Visible boards share one status timer while work is active; idle, paused and
  hidden boards use none. Newest-first history and worker updates expose automatic
  suggestions without replacing the user's selected review or edited proposal.
- Verified native boundary: a new regression found positional JSON arrays were
  accepted as struct input. Host controls now require objects before process or
  storage I/O. A real native-to-Go regression first failed with `unsupported
  profile worker operation`; the existing profile transport now admits wake/status
  and the test passes for shared settings, stale-generation refusal and shutdown.
- Verified browser: all nine production-component checks passed with zero runtime
  errors using the actual Go host, canonical store and a scripted loopback model.
  Automatic generation followed Save without a Generate click, honored the saved
  field lock, left task text unchanged, and appeared for review. Stop persisted
  through settings reload and retained a ready suggestion.
- GitPulse `npm run ci:local` passed for this checkpoint: 5,225 frontend tests
  (one skipped), 24 browser regressions, 95.71% frontend function coverage and
  84.62% Rust line coverage, plus build, type, formatting and Clippy gates. The
  real native-to-Go worker test passed separately. Direct fixture database reads
  confirmed one ready automatic proposal, unchanged task revision five, disabled
  profile settings at revision four and an empty queue. The fixture host exited
  successfully and its temporary directory was removed.
- Manvi `./verify.sh` finished with a qualified pass: 82.1% Go statement coverage,
  4,085 collected Go tests (five skipped), and 869 benchmark-instrument checks.
  Its local-provider wire probe passed. Missing lint/nil/vulnerability tools,
  incumbent Python lease interoperability, the stale flat graph artifact
  (generation one versus index eight), and live cloud-provider checks remain
  unverified. These are verification gaps, not successful checks.
- This proves automatic workflow wiring and controlled provider behavior. It
  does not prove live model semantic quality, installed desktop notification/UI
  behavior, the 100k-task stress target or whole-process resource/soak targets.
  Older evidence is retained as explicitly historical checkpoints.

### Durable automatic scheduling checkpoint

- Verified source: task text saves enqueue work atomically after a one-second
  debounce. Board moves preserve the deadline; queue reads use current task
  revisions, including workspace changes. Acceptance and undo never enqueue work.
- Verified source: schema-four settings and preparation reuse the existing
  proposal lifecycle, active slot and hourly quota. Disabling automation dismisses
  pending automatic work and requests cancellation of running automatic work;
  worker acknowledgement remains required. Claims bind the settings revision.
- Verified tests: the four original scheduling tests failed before implementation.
  Three additional regressions then reproduced disabled/changed-settings launch
  races and now pass. All 13 automatic-store tests pass, including concurrent
  hosts, restart, exact receipt replay, pagination, quota and injected transaction
  failures. The Go store/serve/CLI tests pass under the race detector; the new
  process test proves durable queue/settings and persistent child reuse.
- GitPulse's vendored canonical store was refreshed from the isolated Manvi
  checkout. Both hosts must use schema-four-compatible store artifacts. GitPulse
  `ci:local` passed: 5,217 frontend tests (one skipped), 24 browser regressions,
  function coverage 95.67%, Rust line coverage 84.65%, and the build, format,
  Clippy and coverage gates. The explicit real-native-host test passed separately.
- Manvi `verify.sh` completed with a qualified pass at this checkpoint: 82.2%
  Go statement coverage and 176 Rust tests across the workspace. Five Go tests
  were skipped; lint/nil/vulnerability tools, incumbent Python lease interop,
  the stale flat navigation graph and live cloud-provider checks remain
  unverified. This gate predates the subsequent preservation-prompt change.
- All seven browser enhancement checks passed again using the schema-four store
  and actual host with a scripted model. A direct queue read confirmed the UI's
  saves were durable. The browser fixture and host shut down and were removed.
- At this earlier checkpoint, worker wake-up, restart processing and GitPulse
  settings/status controls were incomplete. The worker checkpoint above supersedes
  that limitation; live-provider quality and installed native behavior remain open.

### Live local enhancement and preservation checkpoint

- An explicit disposable fixture selected the local `qwen3.8:27b-mlx` model at
  the endpoint confirmed by `verify.sh`. The first live attempt was refused with
  `model altered a protected constraint in description`; the saved task remained
  unchanged. No automatic retry or alternative provider was used.
- Source inspection found that validation preserves complete constraint lines
  while the prompt did not make that boundary explicit. Two regressions failed
  before the change. Generation now includes the exact per-field literals and
  constraint lines from the same extractor used by validation, and explains when
  an entire field must remain unchanged. Existing output refusal checks remain.
  Guard extraction is bounded and runs before provider resolution. Go serve/CLI
  tests passed with the race detector after this change.
- A new explicit live attempt reached `ready` in 27.205 seconds. It changed
  `Investigate slow task search (E42)` to
  `Investigate slow task search performance (E42)` and preserved the complete
  description, including the 100 ms p95 target and unknown-cause constraint.
  The task stayed unchanged until selected title acceptance; identical receipt
  replay applied once, undo restored the source, and neither operation enqueued
  another enhancement. The host exited successfully and the fixture was removed.
- This is one reviewed live-model example through durable preparation and an
  explicit generation call. It does not prove automatic worker scheduling,
  general semantic rewrite quality, native UI/provider integration or cloud
  behavior. Full Manvi verification passed again with the same qualifications
  after the prompt change: 82.2% Go statement coverage, five skipped cases out
  of 4,073 Go tests, and 176 Rust tests across the workspace. Missing analysis
  tools, Python lease interop, the stale flat navigation graph (generation one
  versus index seven), and live cloud-provider checks remain unverified.
  The existing output-boundary fuzz target passed 9,658 executions in a bounded
  20-second run. These checks do not constitute a semantic quality corpus.

### Editable suggestion checkpoint

- Verified source: `enhancements.revise` edits a ready proposal without changing
  the task. It preserves the original model text, records which fields differ,
  checks proposal revisions and current task locks, and reuses the existing
  idempotency receipt and transactional history. Acceptance and undo use the
  revised proposal through the existing operations.
- Verified tests at this checkpoint: the two new store regressions failed before
  implementation. The complete 72-test Rust store suite then passed, along with
  Clippy and the Go store, serve and CLI tests under the race detector. GitPulse's
  34 workbench tests and Svelte/type checks passed.
- Verified browser checkpoint: all seven enhancement checks passed with zero
  runtime errors against the real store and host with a scripted model. Editing
  retained the original suggestion, left the task unchanged until acceptance,
  reconciled an intentionally lost acceptance reply once, and supported undo.
  This is workflow evidence, not live-model quality or installed native UI proof.
- The four originally failing scheduling tests were subsequently implemented
  and expanded in the durable automatic scheduling checkpoint above. The full
  CI evidence below predates the editable-suggestion and scheduling changes.
- Remaining review interaction work includes guarding every board navigation
  route against unsaved proposal edits and selecting/polling the latest active
  proposal when history spans multiple pages.

### Native enhancement review checkpoint

- Verified source: the task editor exposes provider/model selection, requested
  fields, bounded proposal history, original/suggestion review, selected
  acceptance, undo, dismissal, cancellation and explicit expired-attempt recovery.
  Opening the panel reads configuration and history without starting inference.
  Status polling runs only for an open, visible panel with an active proposal.
- Verified source: the native profile connection reuses the existing sidecar
  transport and its fault/backoff behavior. It starts lazily with the exact
  profile database, negotiates supported methods and isolates profile requests
  from the policy host. Validation rejects malformed IDs, duplicate keys,
  unexpected controls and invalid Unicode before creating a profile directory or
  starting the process. Quit closes the profile worker with a bounded grace period.
- Verified tests: 19 sidecar tests, five native workbench tests and Clippy passed
  during transport integration. The sidecar fixture proves lazy startup, reuse,
  unsupported-operation refusal and closed-connection refusal. It is a scripted
  process, not proof that the installed native application integrates with Manvi.
- Verified separately against actual built Manvi and dcstore binaries:
  `real_profile_host_shares_native_revisions_and_refuses_a_stale_generation`
  passed. The native adapter read configured selection without opening storage,
  wrote a task and proposal, then changed the task revision. The real Manvi host
  observed that same profile and rejected generation with `revision_conflict`,
  leaving the proposal pending and unchanged. Shutdown refused further profile
  calls. The explicit integration test is ignored in the default suite; it was
  invoked with `--ignored` and both required artifact paths for this checkpoint.
  This proves native adapter/host/storage integration, not installed-app UI or
  provider inference.
- A subsequent shutdown regression reproduced a late request opening a lazy
  profile after shutdown. The shared host now records closure before stopping
  its worker, refuses new reservations/registration/requests, checks closure
  again after the store lock, and closes a connection initialized concurrently
  with shutdown before it can spawn. Six native workbench tests and the explicit
  real-host test pass after this correction; formatting and Clippy pass. The
  full `ci:local` result below predates this final native guard change.
- Verified Go host tests with the race detector: configuration is advertised
  only when supplied; empty requests return selection metadata; unknown controls,
  duplicate decoded keys and arrays are refused. The test uses a missing store
  binary and a provider resolver that must never run: reading configuration does
  not open the database or resolve a model. CLI selection tests preserve explicit
  overrides and do not inherit a local model for a cloud provider.
- Verified production UI against a disposable real Manvi CLI and database with
  a scripted loopback model: all six checks in `harness/workbenchChecks.ts` pass.
  Generation leaves the task unchanged; a deliberately lost acceptance reply
  pauses task editing and reconciles the original receipt; only selected fields
  change; undo preserves unrelated content; unsaved edits block generation;
  saved locks exclude fields; cancellation waits for worker acknowledgement.
  The browser reported zero runtime errors. A direct history read after the
  lost-reply retry showed exactly two task revisions, with description, criteria,
  status and repository links preserved.
- The browser regression first failed because accepted review marked an
  unaccepted description as accepted. The checkbox now reflects the stored
  accepted fields; the same regression passes. Rendered review layout was also
  inspected. Reloading the page preserved the proposal and saved task revision.
- The disposable preview accepts an optional real Manvi binary, uses a scripted
  model and bounds process I/O. Lifecycle tests verify that a failed startup
  closes the model listener and removes its disposable profile. A direct startup
  failure check with the real Manvi binary also exited and removed the fixture.
  Normal termination initially left a temporary database because Vite's own exit
  handler could bypass worker cleanup. The preview now owns its HTTP server and
  uses Vite middleware mode. Four lifecycle tests pass, including a delayed host
  shutdown. After rerunning all six browser checks, actual termination exited
  with code zero, stopped both processes and removed the temporary profile.
- Verification: `npm run ci:local` passed with 5,214 frontend tests (one skipped),
  24 existing browser regressions, 95.66% function coverage, a production build,
  Rust formatting/Clippy, native tests and coverage floors (84.72% Rust lines).
  The subsequent preview startup-cleanup test and explicit real-host test were
  added after that full run; all four preview lifecycle tests, TypeScript,
  the real-host test, formatting and Clippy passed separately. Manvi's focused
  serve/CLI race suites passed. Its earlier broad verification limitations remain;
  `verify.sh` was not repeated for the configuration-only host addition.
- Remaining: automatic on-save generation, editing a suggestion before acceptance,
  full semantic quality evaluation, installed native IPC/provider execution and
  all other open requirements in the six-area contract. The scripted model is
  workflow evidence only. The preview intentionally lacks native event delivery,
  folder picking and notification activation; their absence is shown explicitly.

### GitPulse board checkpoint

- Verified source: `src-tauri/src/workbench.rs` delegates to the vendored Manvi
  store with one lazy connection, eight bounded requests and blocking work off
  the UI thread. IPC resolves the profile path itself and returns structured
  errors. The frontend validates the JSON envelope and decoded record fields.
- Verified source: global surface selection is a single repository/Fleet/Tasks
  state. The global board lives outside the repository-keyed subtree; repository
  Tasks uses the view registry. Cards are paginated to 30 per column, at most 180
  mounted cards. Task descriptions and criteria load only when needed. No model
  or execution worker starts for board browsing or editing.
- Verified focused tests: four native adapter tests passed; 26 frontend boundary
  tests passed. Transport tests exercise malformed/oversized envelopes, typed
  failures, pagination, complete-record replacement and identical receipt reuse
  after an uncertain write. The latter proves client transport behavior, not
  recovery for every editor operation.
- Verified browser interaction through the production board and real disposable
  Manvi database: changed a task title and status, retained its description and
  criteria at revision two, renamed a workspace and added a shared membership,
  reloaded to confirm persistence, then created a task linked to GitPulse and
  Manvi and checked the repository-scoped count. The harness uses a local HTTP
  adapter; it does not prove native IPC, OS notifications or provider execution.
- Verification checkpoint: the first `ci:local` run passed 24 existing browser
  regressions and 5,183 tests (one skipped), but stopped at 94.85% function coverage
  against the 95% requirement. Transport tests were added without changing the
  threshold. The rerun passed 5,205 tests (one skipped) and 95.64% function coverage;
  Svelte/TypeScript checks, the production build, Rust formatting and Clippy passed.
  The native workspace coverage run and final coverage check then passed (Rust
  lines 84.72%, required 80%). This completed the board checkpoint's `ci:local`
  sequence. It predates the schema-two enhancement and field-lock changes; the
  subsequent integration checks for those changes are recorded below.
- Known remaining board gaps: path relocation and stable checkout records,
  efficient repository lookup beyond the bounded registration scan, global
  navigation from repository tabs, consistent unsaved-change handling, durable
  uncertain deletion recovery, complete pagination navigation, and synchronization
  when the separate Go host changes data while the app stays focused. These are
  implementation work, not waived requirements.

### Enhancement checkpoint

- Manvi schema two adds complete source snapshots, pending/ready/failed proposals,
  acceptance of selected fields, dismissal and undo. Task and proposal revisions,
  events and receipts commit together. The native store uses this same vendored
  implementation; no second SQL or inference owner was added.
- Eight Rust lifecycle tests passed, including source conflicts, locks, concurrent
  acceptance, receipt replay after restart, failed-transaction rollback, migration,
  expiry and automatic-start limits. The Go host lifecycle test and store/serve/CLI
  race suites passed. The original five lifecycle cases failed on the old source
  with `unknown_method`, and the frontend lock regression failed before its fix.
- The task editor now exposes title/description enhancement locks; older hosts
  that omit the new field preserve existing locks. This is advisory task metadata,
  independent of coding-agent permission or sandbox policy.
- Verified in the browser against a disposable Manvi store: saved a title lock,
  reloaded and reopened the task at revision two, edited the locked title as a
  human, and saved revision three with the lock, description and criteria intact.
  Runtime error count stayed zero. The preview process and tab were closed.
- The full GitPulse `ci:local` run passed on the schema-two/field-lock changes:
  5,206 frontend tests passed (one skipped), 24 browser regressions passed,
  function coverage was 95.65%, and native coverage floors passed (84.67% Rust
  lines). Four native adapter tests also passed separately. Manvi's complete
  `dc-store` suite and Clippy passed; store/serve/CLI Go race suites passed. The
  broad Manvi `verify.sh` was not rerun for this checkpoint; its earlier limitations
  remain, and no model, installed notification or whole-app performance proof is
  implied by these gates.
- This schema-two checkpoint preceded generation. The worker integration and
  schema-three evidence are recorded below. Proposal diff/acceptance UI and
  complete semantic quality evaluation remain required. Provider/model metadata
  in storage alone do not prove that inference took place.

### Generation checkpoint

- Verified source: Manvi's Go host advertises `work.enhancements.generate` when
  the profile module has a runner. The CLI resolves the explicitly selected
  provider/model using its existing factory and credentials. A durable claim
  precedes one asynchronous, tool-free inference call. CRUD and board navigation
  do not start inference; generation never accepts its own proposal.
- Schema three binds completion to a worker. Running claims remain exclusive
  after their deadline. Dismissal requests cancellation; only actual worker return
  acknowledges it. Explicit expired-attempt recovery preserves outcome uncertainty
  and rejects late completion. GitPulse's vendored store was refreshed through
  the existing selective vendor script so both hosts share these rules.
- Bounds: one active enhancement per profile, 32 KiB intact source context,
  110-second inference context, at most 8,192 output tokens/events and 256 KiB
  streamed/final content. Shutdown waits up to five seconds; an uncooperative
  provider leaves an unresolved claim. No automatic provider retry or fallback.
- Verified: 70 tests in the complete Manvi Rust store suite, including ten
  enhancement lifecycle cases, and Clippy passed. Go store/serve/CLI race suites
  passed. Real-store worker tests check host responsiveness, cross-host repeat
  starts, cancellation acknowledgement, failure redaction and restart persistence.
- Verified: a real CLI child reached a scripted local HTTP model through the
  configured adapter, answered board requests while inference was blocked and
  made exactly one call after repeated starts. The proposal persisted separately
  from the unchanged task. This is transport proof, not live model quality proof.
- Output tests reject malformed/duplicate fields, lost protected literals and
  constraints, unrequested edits, output floods, tool requests and incomplete
  results. Two failing tests exposed invented success claims in the explanation
  and invalid failure diagnostics; both passed after correction. The output fuzz
  target passed 198,929 executions. Literal/phrase checks are bounded heuristics,
  not complete intent-preservation or hallucination detection.
- Outstanding: the generation/review UI, automatic on-save scheduling and default
  provider selection, complete semantic evaluations, context/planning, agent
  supervision, notifications and whole-application performance qualification.
- Verified on this checkpoint: GitPulse `npm run ci:local` passed, including 5,206
  frontend tests (one skipped), 24 browser regressions, 95.65% function coverage,
  native tests and coverage floors (84.71% Rust lines). Manvi `./verify.sh`
  completed with a qualified pass and 82.2% Go statement coverage. Its full Go run
  reported five skipped cases: three requirement interoperability cases and two
  terminal rendering cases. Missing golangci-lint, govulncheck and nilaway checks,
  incumbent Python lease interoperability and live cloud-provider qualification
  remain uncovered. The live local-provider wire probe passed; this is separate
  from task-enhancement semantic quality.
- The broad Manvi navigation gate also reported a stale artifact: it reads
  `.devcouncil/code_graph.json` at generation one, while the rebuilt index was at
  generation four. The DevMap build emitted its graph under
  `.devcouncil/graph/code_graph.json`. That path mismatch remains a navigation
  qualification gap; the successful generation tests do not qualify it.

### Storage checkpoint

- Canonical owner: Manvi `crates/dc-store/src/workbench/`; source copied into
  GitPulse through `vendor-crates.mjs --crate=dc-store`. No dependency was added.
- The Go host enables `work.*` only with an explicit absolute profile DB path,
  preserves typed revision/missing-record errors, and closes its lazy pool on exit.
- GitPulse can use the already-linked Manvi Rust core for native board storage;
  the Go host uses the same store for managed workflows. Model workers need not
  start to browse/edit tasks. The native adapter and basic board UI are wired;
  managed workflows and the full task inspector are still outstanding.
- Workspace deletion preserves tasks/repositories and records affected task
  revisions. Its affected-ID sample is capped at 200 and includes a total and
  truncation flag. Board pagination is consistent within a page, not a snapshot
  spanning edits; consumers must invalidate/restart after relevant events.
- Read the Manvi `docs/WORKBENCH.md` contract before UI integration: `.put` is a
  replacement, task cards omit description/criteria, and profile records must
  use a separate database file from repository execution leases.
- The first `./verify.sh` run reported a qualified pass and 82.2% Go statement
  coverage, with unavailable analysis/interop/provider gates explicitly listed.
  A subsequent run was invalid because a global `DEVCOUNCIL_ROOT` override also
  changes CLI root discovery. The override was removed, affected CLI tests pass,
  and the normal-environment rerun finished with a qualified pass. Missing
  golangci-lint, govulncheck and nilaway gates did not run; incumbent Python lease
  interop was unavailable, and Anthropic/Gemini/xAI used scripted servers only.
  The three requirement interop tests passed in
  their own narrowly scoped invocation. No native runtime qualification,
  managed-provider integration or whole-application performance result is claimed.

### Native integration sequence

1. Add the profile adapter around Manvi's already-linked `dc-store` core. Keep
   database creation lazy, off the UI thread, and the queue bounded. Resolve the
   profile path in native code, preserve structured errors and emit change cursors
   only after commit. Do not add a second implementation of the SQL or schema.
2. Finish the repository/checkout identity contract before importing open tabs:
   separate repository IDs from checkout paths, represent unavailable paths, and
   support explicit relinking without matching clones solely by remote URL. Use
   the existing Git root/common-dir resolver and keep membership independent from
   watcher registration. Persist first-enable seeding as an idempotent operation.
3. Extend the frontend's single global-surface state rather than adding competing
   `fleetOpen`/`tasksOpen` booleans. `src/App.svelte` currently keeps Fleet outside
   the repository-keyed subtree; the global board and workspace navigator need
   that same lifetime. Retain the terminal dock's existing lifetime across scope
   switches. `WorkspaceView.svelte`/`viewRegistry.ts` supply the repo Tasks section.
4. Supply one typed workbench client/store to every board. Reject malformed
   responses; reset stale cursor queries after relevant events. Load card summaries
   first, full records on inspector open. Selection and drag must not instantiate
   editors, terminals, graphs or model clients for every card.
5. Add native move/partial-update contracts before optimizing interactive edits.
   The current `.put` replaces a full record, so a card summary cannot be sent back
   as an edit without losing omitted details. Until partial updates land, fetch
   the full version and use revision CAS. Resolve drop anchors/order changes in
   one transaction and preserve unrelated title/description edits.
6. Wire workspace CRUD, shared memberships, global/workspace/repo boards and task
   inspectors with actual native/browser tests. Verify unavailable, empty, loading,
   failed, stale and retry states independently. Then connect accepted enhancement
   diffs and managed runs to these durable task IDs and immutable source revisions.
7. Add the durable attention/inbox coordinator before OS banners. Agent question,
   permission, review and completion events must survive process/app restart before
   native activation is wired. A native banner is a delivery attempt, not proof
   that a request was seen or resolved.
