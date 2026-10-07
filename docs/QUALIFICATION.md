# Open qualification

Dated audit ledgers live under [archive/](archive/README.md). This file is the
tracked follow-up list. The acceptance contract for the workbench remains
[AGENTIC_WORKSPACES_PLAN.md](AGENTIC_WORKSPACES_PLAN.md) — partial
implementation is not completion.

## Remaining (from the live contract)

Extracted from the plan's remaining column (2026-09-11). Re-read the plan
before treating a row as still open.

| Area | Remaining |
|------|-----------|
| Workspaces and boards | The task fields the store cannot hold yet (see [Task fields](#task-fields-against-the-plan)); detecting an unavailable checkout before a relink, and remote-only repositories; swimlanes, saved views and WIP limits; installed-app qualification. Board/list multi-select with bulk status/label/archive, undo (a deferred window for deletion), keyboard operation, group import from tab groups, group reorder and relinking a moved or re-cloned checkout shipped 2026-10-07 — `harness/tasks.html` checks each in Chrome and WKWebView |
| Manvi intelligence | Semantic quality evaluation, context/planning/orchestration, installed native qualification |
| Agent supervision | Approve/deny round-trip through the managed lane on real Claude Code and Codex accounts (only controlled providers so far). Codex mid-turn driver kill with a tool running or an approval pending (its configured model is refused on the measuring machine; the other mid-turn cases are measured, see ARCHITECTURE.md). End-to-end test that a refused provider callback (Claude `request_user_dialog`/`elicitation`/any other subtype, Codex any unhandled method) lands as the run's visible reason — traced in code, adapter-tested, not driven through `serve`. The agent hook's scope fence covers redirection targets only: Manvi does not read writes out of a command's arguments, so `sed -i` outside the plan is still a demoted allow — and the file-tool hook (`collision-guard`, on Edit/Write) checks collisions only, so a task-bound agent's direct write outside its scope is not fenced by any hook. It applies only in repositories GitPulse is trusted in; elsewhere commands are judged unscoped, silently. A redirect into a *planned* file is refused (`scope.operation`) until Manvi's `fix/hostscope-unspecialised-write` ships in the installed harness. Code review is designed, not built ([AGENT_OUTPUT_REVIEW.md](AGENT_OUTPUT_REVIEW.md)). Installed-app qualification |
| Native notifications | Installed OS delivery/activation (now including the banner Snooze action and withdrawal, built and store-tested 2026-10-07 but never observed in an installed bundle), callback crash-window, burst/grouping behaviour, native resource measurements, producers blocked on other workflows (CI, reminders, GitHub, workspace results, agent output review), Windows and Linux bindings (candidates already in Cargo.lock; approval needed) — see [NATIVE_NOTIFICATIONS_ADAPTER.md](NATIVE_NOTIFICATIONS_ADAPTER.md) |
| Performance | Stress broad global/workspace search meets the 100 ms target upstream (44.6/70.5 ms p95, interleaved A/B on the committed benchmark, 2026-10-07 in [the archived benchmark](archive/AGENTIC_WORKSPACES_BENCHMARK.md)); GitPulse's vendored `dc-store` does not have it until re-vendored. Installed-build native rendering/frame pacing, cold/warm navigation, idle CPU/memory and the eight-hour soak are **not measured** — the release build was not installed; `scripts/native-sample.mjs` is the soak sampler. Browser canary 2026-10-07 in [PERFORMANCE.md](PERFORMANCE.md): six components clean at 12 cycles; ManviOpsPanel not clean (harness crash) and TerminalPanel/termtabs never armed |

### Task fields against the plan

Checked 2026-10-07 against the plan's section 2 and against what the store
accepts: `put_item` in `src-tauri/vendored/dc-store/src/workbench/mod.rs`
takes exactly 19 named fields and refuses any other, so a field the store does
not name cannot ride along in a task — not even as extra JSON.

| Plan field | Store | Task sheet | State |
| --- | --- | --- | --- |
| Type: issue, bug, feature, improvement, maintenance, research, documentation, custom | `kind`, free text up to 64 | Seven presets plus Custom… | Closed |
| Title, description | Yes | Yes | Closed |
| Priority, severity, owner, labels | Yes | Yes | Closed |
| Acceptance criteria | `acceptance_criteria`, up to 128 lines | Yes | Closed |
| Linked repositories and a primary one | `repository_ids`, `primary_repository_id` | Repository picker | Closed |
| Optional home group | `home_workspace_id` | Home workspace select (it was set once, at creation, and could not be changed) | Closed 2026-10-07 |
| Dates | `due_at` only; `updated_at` is the store's | Due | Due is closed. A start date is **declined**: no field holds it, and when a task changed is already in its revision history |
| Checklists | None: subtasks are plain lines in `acceptance_criteria`, with no done state | Subtasks list without checkboxes | **Declined here** — needs a store field |
| Attachments | None | None | **Declined here** — needs a store field and a file store |
| Source links | None; a GitHub issue is linked by an `issue-N` label | Through labels | **Declined here** — needs a store field |
| Parent, blocking, related and duplicate links | None; a merge records its sources as `## Merged from <id>` text in the description, not as a link | None | **Declined here** — needs a link table |
| Per-repository objectives | None: `work_item_repositories` holds only the link and its order | None; intake folds per-repository detail into the description | **Declined here** — needs a column on the link |

Each declined row is a dc-store change in DevCouncil first — the field
whitelist, the body built in `put_item`, and the brief format agents read —
then a re-vendor, then the sheet. That is a schema migration of its own, not
a board change, so it is listed here instead of being emulated: a reserved
label or description heading would look like a field and silently drop on
the next edit by a host that does not know the convention.

## Platform coverage

GitPulse builds and ships for macOS, Linux and Windows. `ci.yml` runs on all
three: the contract checks, `svelte-check`, Vitest, the Vite build, `cargo fmt`,
`cargo clippy` and the Rust unit and integration suites. Three things do not run
everywhere — the browser regressions are Linux (Chrome) and macOS (WKWebView)
only, `actionlint` and the coverage workflow are Linux only — and a Rust test
behind `#[cfg(unix)]` is compiled out on Windows rather than failing there.

What no platform but macOS gets is a person: GitPulse is developed on macOS, and
that is the only platform where features are exercised by hand against a running
app. Two different things follow, and this section keeps them apart, because
collapsing them is how a limitation turns into a surprise:

- **Known absent** — the code refuses on that platform and says so. Verified by
  reading the gate, not inferred.
- **Unverified** — nothing platform-specific is known to be wrong, and nobody
  has run it there. This is not a claim that it works.

### Known absent off macOS

| Surface | What happens | Why |
| --- | --- | --- |
| Desktop notifications (activity **and** agent sessions) | No banner is delivered; the in-app inbox and the settings counters still work, and the settings panel names the platform rather than showing a dead switch | `workbench::notifications` has a macOS implementation and a `not(target_os = "macos")` arm that returns `unsupported` |
| Agent hook reports (`gitpulse-hook notify`) | The **Accept reports from agent hooks** switch is disabled and explains why; the hook exits 0 and costs the agent's turn nothing | The bridge is a Unix domain socket. `bridge_supported` is `cfg!(unix)`, and `notify_send` has a `not(unix)` arm |
| Menu-bar status item, macOS menus and appearance, Apple Intelligence enhancements | Hidden rather than removed | Documented in [MACOS_MENUS.md](MACOS_MENUS.md) and [MACOS_APPEARANCE.md](MACOS_APPEARANCE.md) |

On Windows an agent session in a terminal tab therefore still *sounds* — the
PTY-side scanner and the CLI launch flags are platform-neutral — but nothing
turns that into a desktop banner. The unread marker on the tab is the signal.

### Unverified on Windows

- **Managed agent runs** (Codex and Claude Code). The adapters use no
  Unix-specific API, but both end-to-end tests are `#[cfg(unix)]` and have only
  ever been run on macOS. Treat managed runs on Windows as untried.
- **Terminal handoffs** beyond what the unit tests cover: provider discovery
  through `PATH`, and the launch flags each CLI advertises on that platform.
- **Insights → Secrets.** The parser and Git-location tests are platform-neutral
  and run everywhere, but `tests/secrets_stress.rs` drives stub scanners as
  shell scripts and is `#[cfg(unix)]`. Whether Kingfisher on Windows echoes
  the scan root in the form `validate_repo` returns (a `\\?\` verbatim prefix
  would make every row read "outside") has never been checked, and the
  install hint names Homebrew.
- **Anything that shells out to `git`** with paths that differ in separator or
  case sensitivity. `portable-paths.contract` catches the one class that has bitten
  CI before (a `file:` URL's `pathname` is `/D:/…` on Windows) but it is a lint,
  not a run.

A Windows regression cannot be caught from a macOS checkout even at compile
time: the `x86_64-pc-windows-msvc` target fails to cross-check here because
`libsqlite3-sys` needs a Windows toolchain. Platform-specific code is therefore
written with `cfg` inversion — a `not(unix)` arm compiled on macOS — rather than
trusted to a build nobody ran.

## Live product docs (not archived)

Architecture, Features, Terminal, Tasks and workspaces, Module integration,
Command palette, Repository hygiene, Security, Coverage, Performance,
macOS menus/appearance, Dependency health, Overview hardening contracts,
Native notifications adapter, Good first issues, Agentic workspaces plan.
