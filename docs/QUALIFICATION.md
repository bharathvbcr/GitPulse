# Open qualification

Dated audit ledgers are preserved at [commit 16483b7d](https://github.com/bharathvbcr/GitPulse/blob/16483b7de2b5c74003c7e7af046f7142ece7699b/docs/archive/README.md). This file is the
tracked follow-up list. The acceptance contract for the workbench remains
[AGENTIC_WORKSPACES_PLAN.md](AGENTIC_WORKSPACES_PLAN.md) — partial
implementation is not completion.

## Remaining (from the live contract)

Extracted from the plan's remaining column (2026-09-11). Re-read the plan
before treating a row as still open.

| Area | Remaining |
|------|-----------|
| Workspaces and boards | The task fields the store cannot hold yet (see [Task fields](#task-fields-against-the-plan)); registering a remote-only repository from the board (they are shown and can be linked to a checkout, but only another host creates one); the [early board gaps](#early-board-gaps-plan889-894) still open; installed-app qualification. Board/list multi-select with bulk status/label/archive, undo (a deferred window for deletion), keyboard operation, group import from tab groups, group reorder and relinking a moved or re-cloned checkout shipped 2026-10-07; swimlanes (status, owner, first label), saved named views and optional WIP limits per board, unsaved-suggestion-edit guards on every board route, the latest active suggestion read across paginated history, missing-checkout and remote-only marks before any launch or relink, list-layout paging and a repository board's link to the global board shipped 2026-10-08 — `harness/tasks.html` checks each in Chrome and WKWebView |
| Manvi intelligence | Semantic quality evaluation, context/planning/orchestration, installed native qualification |
| Agent supervision | Approve/deny round-trip through the managed lane on real Claude Code and Codex accounts (only controlled providers so far). Codex mid-turn driver kill with a tool running or an approval pending (its configured model is refused on the measuring machine; the other mid-turn cases are measured, see ARCHITECTURE.md). Closed 2026-10-08 (task ft-723b56f2): a refused provider callback (Claude `request_user_dialog`/`elicitation`, an unhandled Codex method) is driven end to end through `manvi serve` and lands as the run's recorded reason (Manvi `6df0909`, which also fixed seven Codex kinds that were recorded as "no matching active turn"); the hook's scope fence reads the files `sed -i`, `tee`, `cp` and `mv` write through their arguments (DevCouncil `673073d3`; installed with Manvi `ed6e8aa`, and `tests/hook_scope_fence.rs` passes against that installed harness), the file-tool hook judges scope through the same `harness::scope_for` owner as the command gate, and an untrusted repository's answer says `unscoped: repository not trusted`; the planned-file redirect refusal is gone with Manvi `6ebee98` installed (063632f); agent-output review is built behind a per-repository opt-in ([AGENT_OUTPUT_REVIEW.md](AGENT_OUTPUT_REVIEW.md)); an attempt worktree is released only once no process works in it. The argument fence reaches *release* builds: Manvi `main` pins DevCouncil `9add99c2` in `scripts/module-pins.txt`, and `673073d3` is an ancestor of it. **Still open:** other commands that write through arguments (`rm`, `truncate`, `touch`, `dd of=`, `perl -i`, `install`, `ln`, `rsync`, `patch`, `git apply`, sed's `w` command, `xargs`/`find -exec` operands, wrappers with options such as `sudo -u`); a run in the person's own checkout is not scanned for escaped descendants; nothing fences a second GitPulse profile or an agent started outside GitPulse (Git identity checks are observations, not locks); Grok and Antigravity are terminal-only ([why](TASKS_AND_WORKSPACES.md#providers-with-a-managed-lane)); a managed Claude run records its build identity, not a model, and is supervised rather than sandboxed. Approve/deny round-trip through the managed lane on real Claude Code and Codex accounts (only controlled providers so far). Codex mid-turn driver kill with a tool running or an approval pending (its configured model is refused on the measuring machine; the other mid-turn cases are measured, see ARCHITECTURE.md). Installed-app qualification |
| Native notifications | Installed OS delivery/activation (now including the banner Snooze action and withdrawal, built and store-tested 2026-10-07 but never observed in an installed bundle), callback crash-window, burst/grouping behaviour, native resource measurements, producers blocked on other workflows (CI, reminders, GitHub, workspace results, agent output review), Windows and Linux bindings (candidates already in Cargo.lock; approval needed) — see [NATIVE_NOTIFICATIONS_ADAPTER.md](NATIVE_NOTIFICATIONS_ADAPTER.md) |
| Performance | Stress broad global/workspace search meets the 100 ms target on the committed benchmark (44.6/70.5 ms p95, interleaved A/B, 2026-10-07 in [the archived benchmark](https://github.com/bharathvbcr/GitPulse/blob/16483b7de2b5c74003c7e7af046f7142ece7699b/docs/archive/AGENTIC_WORKSPACES_BENCHMARK.md)). GitPulse has vendored it since `0af6bff7`. The vendored source (DevCouncil `c17fd379`) measured 55.5/90.2 ms p95, minimum of five interleaved runs on a heavily loaded host, with 10–30% on scoped search over the measured commit unexplained. Release build in an isolated profile (ten of the user's repositories, 2026-10-09/10, [PERFORMANCE.md](PERFORMANCE.md)): spawn to first meaningful paint 360–414 ms over five launches; four warm repository switches settled at p50 549 / max 762 ms (`performance:nav`). **Not measured:** cold navigation, view switches, frame pacing (the window was focused only for seconds), idle CPU/memory (the only idle sample ran with every repository refused for trust; the trusted run was quit from the keyboard first) and the eight-hour soak (`scripts/native-sample.mjs`). Each needs a person at the Mac to keep the test window on stage |

### Early board gaps (plan:889-894)

Re-checked 2026-10-08 against the code, item by item.

| Gap | State | Evidence |
| --- | --- | --- |
| Efficient repository lookup beyond the bounded registration scan | **Open** | `intake::find` (`src-tauri/src/workbench/intake.rs`) still pages `repositories.list` 200 at a time, up to 50 pages, and refuses past 10,000 with `registry_limit`. An indexed identity lookup needs a dc-store query, so it is a DevCouncil change and re-vendor, not a board change |
| Global navigation from repository tabs | Closed 2026-10-08 | A repository's board (no navigator) has an **All tasks** link to the global Tasks surface; the repository pane is hidden, not unmounted, so its open task survives |
| Consistent unsaved-change handling | Closed inside the board 2026-10-08; **open** outside it | Unsaved suggestion edits made every board route refuse silently (they held the assist busy); they now ask, through the same `canLeave` as task edits, in the task sheet and in Quick Enhance. Still open: leaving a repository's Tasks section or switching repository tabs unmounts that board (`{#key currentPath}` and the `{#if}` view chain in `App.svelte`, `{#if section === "tasks"}` in `WorkspaceView.svelte`), dropping its unsaved edits without a prompt |
| Recovery from an uncertain delete | Closed | A confirmed delete waits out an undo window, then runs as a `TaskBatch` whose uncertain rows reopen the action dialog with an exact retry under the same request id. Across a restart nothing local claims a result: the board re-reads the store, which holds whatever committed |
| Full pagination navigation | Closed 2026-10-08 | The list layout had no paging, so a task past a column's first page was unreachable from it; both layouts now show "N of M", Load more and First page per column |
| Syncing when the Go host changes data while the app has focus | Closed | `src-tauri/src/workbench/external_changes.rs` (642c5d5b) polls SQLite `data_version` while the window is visible and emits `workbench-changed`, which the board debounces into a refresh |

### Task fields against the plan

Checked 2026-10-08 against the plan's section 2 and against what the store
accepts (the field count re-read 2026-10-23): `put_item` in
`src-tauri/vendored/dc-store/src/workbench/mod.rs` takes exactly 24 named fields
(that list includes the request envelope `request_id`, `id` and
`expected_revision`; schema 11 added `archived`, `checklist` and `links`) and
refuses any other, so a field the store does not name cannot ride
along in a task — not even as extra JSON.

| Plan field | Store | Task sheet | State |
| --- | --- | --- | --- |
| Type: issue, bug, feature, improvement, maintenance, research, documentation, custom | `kind`, free text up to 64 | Seven presets plus Custom… | Closed |
| Title, description | Yes | Yes | Closed |
| Priority, severity, owner, labels | Yes | Yes | Closed |
| Acceptance criteria | `acceptance_criteria`, up to 128 lines | Yes | Closed |
| Linked repositories and a primary one | `repository_ids`, `primary_repository_id` | Repository picker | Closed |
| Optional home group | `home_workspace_id` | Home workspace select (it was set once, at creation, and could not be changed) | Closed 2026-10-07 |
| Dates | `due_at` only; `updated_at` is the store's | Due | Due is closed. A start date is **declined**: no field holds it, and when a task changed is already in its revision history |
| Checklists | `checklist`: up to 128 `{text, done}` entries, kept when a write omits it (schema 11) | Checklist with checkboxes (`TaskRelations.svelte`); the brief's `## Checklist` | Closed 2026-10-08 |
| Parent, blocking, related and duplicate links | `links` → `work_item_links` (`parent`, `blocks`, `related`, `duplicate_of`), up to 64; one parent, no parent cycle, a new link must name a live task (schema 11) | Linked tasks with a task search (`TaskRelations.svelte`); the brief's `## Linked tasks`, read from both ends | Closed 2026-10-08 |
| Completion time | `completed_at`, the store's own: set entering Done, cleared leaving it (schema 11) | Archive sorts by it and stamps each row | Closed 2026-10-08 |
| Attachments | None | None | **Declined** — needs a file store, not a field: a path would point outside the profile and break on another machine, and copying files into SQLite is a storage decision this board should not make on its own |
| Source links | None; a GitHub issue is linked by an `issue-N` label | Through labels | **Declined** — the `issue-N` label already round-trips with the GitHub panel; a second field would be a second answer to "which issue is this", and the two would drift |
| Per-repository objectives | None: `work_item_repositories` holds only the link and its order | None; intake folds per-repository detail into the description | **Declined** — no consumer: neither the brief nor any launch reads per-repository text separately, so a column would be written and never read; revisit when a multi-repository launch needs one |

Checklists, links and completion time shipped as dc-store schema 11, made in
DevCouncil first (field whitelist, `put_item` body, brief format), re-vendored,
then surfaced in the sheet — see [ARCHIVE_SEPARATION.md](ARCHIVE_SEPARATION.md).
A declined row stays out rather than being emulated: a reserved label or
description heading would look like a field and silently drop on the next
edit by a host that does not know the convention.

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

### Windows status (2026-10)

| Surface | State | Evidence |
| --- | --- | --- |
| `cargo clippy --all-targets -- -D warnings` for `x86_64-pc-windows-msvc` | **Clean** in a metadata-only cross-check from macOS; unix-only items are `cfg(unix)`-gated rather than allowed | Recipe below. A check only: nothing links and no test runs. |
| Repository trust gate, Windows arm (`admit_directory_windows`, `pin_dir_chain`) | **Tests written, never run** | `repository_trust::tests::a_substituted_directory_cannot_replace_an_admitted_one_while_the_guard_is_held` (rename, ancestor rename and removal refused while the guard lives, allowed after) and `a_junction_is_refused_by_the_pin_and_resolved_before_admission`. They compile under the Windows target; the first Windows CI run is their first execution. |
| The 31 Rust lib test failures and 6 Vitest failures on the Windows leg (CI run 37875243743, 2026-10-09) | **Diagnosed from the logs and fixed in source; not yet run on Windows** | Causes: git refuses a `\\?\` path as an argument (`worktree add`/`remove` now take `plain_path_text`; `ref_cache`, `repo_op`, `worktree`, churn test fixtures use `canonicalize_plain`); `*` and `"` are not Windows filenames (`stash_with_paths`, blame rename test); a system `pull.rebase=false`; a Unix-literal `cwd`; the brief path is JSON-quoted in the prompt; `read_bounded_nofollow` could not open a directory and followed links on Windows; the raw watcher reports the `target` directory entry itself; vendored-manifest and Vitest paths used `\`. Each is a first-CI-run prediction until the Windows leg is green. |
| Managed Codex/Claude runs, terminal handoffs, Secrets `\\?\` paths, git separator/case handling | **Unverified** | See the list above; needs a Windows machine |
| Repository hygiene removal | **Refused**, with the reason shown in the Cleaner panel | `storage/hygiene/global.rs` reports `supported: cfg!(unix)`; the backend refuses `Cleanup is unavailable on this platform` |

Windows clippy recipe from macOS (the C dependencies' build scripts are given
fake `cc`/`ar`/`rc` that only create the files the build script expects, so
`cargo clippy` type-checks without linking): set `CC_x86_64_pc_windows_msvc`,
`AR_x86_64_pc_windows_msvc` and `RC` to such scripts and run
`cargo clippy --all-targets --target x86_64-pc-windows-msvc -- -D warnings`
in `src-tauri`. Without them the build stops in `libsqlite3-sys`, as below.

A Windows regression cannot be caught from a macOS checkout even at compile
time: the `x86_64-pc-windows-msvc` target fails to cross-check here because
`libsqlite3-sys` needs a Windows toolchain. Platform-specific code is therefore
written with `cfg` inversion — a `not(unix)` arm compiled on macOS — rather than
trusted to a build nobody ran.

### Native PTY, Linux and macOS hardware runs

| Surface | State | Evidence |
| --- | --- | --- |
| A real shell command round-tripping through the PTY (input in, output event out, exit event) | **Works on macOS**; runs on Linux in CI | `terminal::tests::a_real_shell_command_round_trips_through_the_pty` (`#[cfg(unix)]`, so it is part of the `ci.yml` Rust suite on the Linux and macOS legs). It passed locally on macOS; its first Linux run is the next `ci.yml` run. |
| Linux Wayland session | **Unverified** | Manual hardware run; no known defect |
| Linux physical global shortcuts | **Unverified** | Manual hardware run; no known defect |
| Linux x86_64 packaging and signing | **Unverified** | Manual hardware run; no known defect |
| macOS sleep/wake and display changes | **Unverified** | Manual hardware run; no known defect |

An unverified row is not a claim that it works. Record the result here as
works, broken or unverified when someone runs it.

## Live product docs (not archived)

Architecture, Features, Terminal, Tasks and workspaces, Module integration,
Command palette, Repository hygiene, Security, Coverage, Performance,
macOS menus/appearance, Dependency health, Overview hardening contracts,
Native notifications adapter, Good first issues, Agentic workspaces plan.
