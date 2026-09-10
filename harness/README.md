# Runtime harness

## Uncommitted previews

Run `npm run test:browser -- --harness uncommitted` or
`npm run test:webkit -- --harness uncommitted`. For interactive inspection,
open `/harness/uncommitted.html` on the development server.

This mounts the existing status bar, sidebars, view/repository tabs, Workspace
Overview, workspace status, worktree list, Fleet, and `DiffViewer`. Click checks
start from an old commit and require the requested worktree's changed-file rail
and actual modifications. They also cover both sides of a partially staged
file, reopening the collapsed rail, and the diff's change picker. Unexpected
commands and browser runtime errors fail the run. IPC fixtures do not verify
the installed native app; store tests separately cover failed loads and late
repository, view, and diff responses.

## Command palette

Run `npm run test:browser -- --harness palette`, or
`npm run test:webkit -- --harness palette` on macOS. The palette harness mounts the
production palette and prompt components with explicit repository/code-intelligence
fixtures. It tests help-mode transitions, stale responses, failed and partial
searches, registry-resolved workspace navigation, file paging, keyboard/IME behavior,
focus handoff, unavailable actions and duplicate execution. Missing completion and
runtime errors fail the gate. Open `/harness/palette.html` on the development server
for manual inspection; `?theme=light` selects light appearance. No Git mutation is
performed by these fixtures. See `docs/COMMAND_PALETTE.md` for contracts and limits.

## Tasks page and sheets

Run `npm run test:browser -- --harness tasks` or
`npm run test:webkit -- --harness tasks` on macOS. Both CI browser jobs and
`ci:local` include this gate. For interactive inspection, open
`/harness/tasks.html`; `?theme=light` selects light appearance.

The fixture mounts the production board and sheets with explicit simulated IPC.
It exercises repository switching, workspace defaults, task/workspace draft
protection, in-flight and uncertain saves, malformed save receipts, idempotent
retry, custom types, incremental pagination, status changes and conflicts,
search races, failure/recovery, and keyboard focus. Unexpected requests and
runtime errors fail the run. These checks do not launch an agent or verify
native IPC. The real-store enhancement fixture below tests that separate path.

The sheet keeps Save visible above its scrolling content. Core fields are title,
status, priority, type, primary repository and description; optional fields,
repository links, AI preferences, notifications and agent runs have named groups.
Custom type names remain supported. Cards expose status choices alongside drag
and arrow-key movement. Adding from a column uses that status. Load more keeps
existing cards and displays the loaded/total count. Repository switches preserve
open drafts, including their original repository selection.

## Agentic task boards

Run `node scripts/workbench-preview.mjs /absolute/path/to/dcstore` using the built
Manvi `dcstore` binary. The script prints an ephemeral loopback URL ending in
`/harness/workbench.html`. It creates a disposable profile database with three
repositories, a shared workspace and six tasks through the public workbench API;
normal shutdown removes that fixture. It does not open or mutate user repositories.

The page mounts the production `TaskBoard`, task editor and workspace editor.
Check global/workspace/repository counts, create a task with multiple repository
links, edit its title and status, change group membership, reload, and verify the
saved description, acceptance criteria and revisions. Use the visible runtime
error count as part of the result. Current interaction evidence is recorded in
[the implementation plan](../docs/AGENTIC_WORKSPACES_PLAN.md).

For brief checks, first enable **Capture copies in fixture**. Then open a task and
use **Copy saved brief**. The harness captures the text in its visible output
instead of writing to the OS clipboard. Check both repository references, task
revision and metadata. Unsaved edits must stay out of the captured text and the
copy status must say so. A concurrent saved update must refuse a stale copy and
leave the capture unchanged. This checks the rendered copy workflow with real
storage; it does not qualify the installed application's OS clipboard behavior.

For enhancement generation, append `/absolute/path/to/manvi` as a second binary
argument. The fixture starts that actual host with a disposable profile and a
scripted loopback model, using an isolated configuration root. No live provider
is contacted. Opening Manvi enhancements should show `local` / `fixture-model`.
The **Run enhancement checks** button creates a task through the editor and checks
generation, editing a suggestion while retaining original model text, selected
acceptance after an intentionally lost reply, undo, unsaved edit protection,
saved field locks, acknowledged cancellation, automatic generation after Save,
and persisted Stop controls that preserve ready reviews. Fixture seeding disables
automatic work; the checks explicitly enable it through the production UI.
It reports nine checks and a
visible pass/fail verdict. **Lose next acceptance reply** can also be armed
manually: the write commits, then the adapter throws a transport failure. Retry
must reconcile the original receipt and leave the task at one new revision.

The scripted response deliberately marks itself as test output. These checks
establish workflow behavior, not semantic rewrite quality. Startup failure and
normal shutdown close the model listener and Manvi child; process-lifecycle
regressions also run in the ordinary unit suite.

This harness sends requests to a real Manvi store through a bounded local HTTP
adapter. Storage calls create a CLI process per request; generation/configuration
reuse one real Manvi host. This is not a native IPC or
performance benchmark. Native folder picking and event delivery are unavailable
here; a live-update-unavailable message is expected. Native registration is
covered separately by Rust tests using disposable Git repositories. Installed
desktop activation, notifications and managed-agent execution need their own
qualification.

To verify the native adapter against real Manvi binaries, set
`GITPULSE_WORKBENCH_TEST_MANVI` and `GITPULSE_WORKBENCH_TEST_DCSTORE` to their
absolute built paths and run:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib \
  real_profile_host_shares_native_revisions_and_refuses_a_stale_generation -- --ignored
```

This explicit integration test is ignored in the ordinary suite because it
requires separately built Manvi artifacts. When invoked, absent artifacts fail
the test. It uses a disposable profile and a child-scoped wrapper; configuration
must not open storage, and Manvi must observe a native task revision change and
refuse a stale generation before provider resolution. It does not launch the
installed desktop app or evaluate a live model.

## Code diagnostics

Run `npm run test:browser` for the automated Chrome gate, or
`npm run test:webkit` on macOS for the system `WKWebView` gate. Chrome must
already be installed (`CHROME_BIN` can select its executable); WebKit uses
the installed Xcode command-line tools. Neither command installs dependencies.
Each run starts an isolated loopback Vite server and temporary browser profile,
requires all 24 assertions, and fails on missing completion, browser exit,
or the 60-second deadline. CI runs Chrome on Linux and WebKit on macOS;
`ci:local` includes Chrome. WebKit briefly opens a dedicated test window.

For interactive inspection:

Run `node node_modules/vite/bin/vite.js --config vite.config.ts --host 127.0.0.1 --port 5189 --strictPort`
and open `http://127.0.0.1:5189/harness/diagnostics.html`. This harness needs the
production Vite configuration so Tailwind compiles the real layout styles.

The page automatically mounts the production Blame, Map, Markdown, and Diagnostics
components with explicit IPC fixtures. It checks unrelated store publications,
cleared selections, disappearing build failures, out-of-order searches and graph/document requests, repeated
backlinks and capped broken-link lists, and 2,000 hostile keys across 200
reconciliations. An intentional duplicate-key canary must fail and produce one
contextual stack report. It also exercises repeated same-status refreshes,
coalesced Blame loads, A–B–clear–A selection races, background index publications,
Map request context, and storage failure/retry in the actual Diagnostics window.
Any other pane failure or unconfigured command fails
the run. The visible result and `window.__gpResult` must show every check passing.

These gates verify Svelte's actual browser lifecycle, including native WebKit
when selected. The fixtures do not verify
native IPC, the installed app, or a specific user's repository; run the Rust
Git integration tests separately for native blame behavior.

## DevMap canvas

Run `npm run dev -- --port 5191` and open
`http://localhost:5191/harness/devmap.html`. This mounts the production canvas
with a deterministic 288-node fixture, including sparse connections and a
truncated-payload warning. The harness controls are outside the production UI.

`Run runtime checks` verifies same-count/same-generation canvas replacement,
refresh and repository isolation, fitting supplied coordinates, combined filters,
paged browsing, directed neighbors, keyboard file activation, removed selection,
malformed-payload containment, and empty/unavailable states in the actual Svelte
client runtime. The repaint check fails against the original cached canvas with
`Same-count payload did not repaint`. The check waits for layout and paint to
settle before comparing the images.

`Run stress checks` exercises 5,000-node sparse, dense, and isolated graphs
(0 / 5,000 / 50,000 links) at 320 and 1,440 px. It reports mounted load/settle
time and the maximum of eight keyboard pan/zoom frame measurements per case,
and rejects unbounded group controls. Both checks fail if browser errors or
unhandled promise rejections occur during the run. Avoid editing imported source during
checks: hot reload can replace the harness and discard its verdict.

Also check search → Enter → selection, Open file (the harness records the
callback path), group highlighting, dragging without losing selection,
pointer-centered wheel zoom, keyboard pan/zoom/reset, and light/dark themes.
For a local payload, place a `GraphVizPayload` JSON file at
`harness/.devmap-preview.json` and use `?real=1`. This is an optional inspection
fixture, not a source artifact. `?theme=light` selects the light preview.

The September 2026 redesign was initially checked with MarkDev's 288 files,
36 file-import edges, and 27 communities. The hardened cross-file projection
now supplies 5,094 relationships from that source graph, including calls,
references, and inheritance. All 226 Swift files have displayed connections.
The preview explicitly reports 13 unresolvable graph records.
See `docs/DEVMAP_AUDIT.md` for the current verification ledger and open gates.
Browser callback verification does
not verify opening a file inside the installed Tauri application.

`npm test` **cannot** catch a Svelte reactive loop. `vitest.config.ts` sets
`environment: "node"`, where `$effect` compiles out entirely — which is why
every Svelte test in this repo is source-text or `render()` from
`svelte/server`, and why `effect_update_depth_exceeded` reached a release.

`scripts/effect-loop-contract.test.ts` guards the *shape* statically. This
harness is how the *behaviour* gets checked, in a real browser with the real
Svelte client runtime.

```bash
npx vite --config vite.harness.config.ts
# then open, e.g.:
#   http://localhost:5188/harness/stress.html?c=PulseView&tabs=5&scenario=chaos&cycles=45
```

`window.__gpResult` holds the run's verdict; `window.__gpDepth` is published
every 250 ms so a run that wedges the renderer still reports (a severe loop
never reaches the final assignment — that is how the StoragePanel defect
presented).

- `c` — `PulseView` | `StoragePanel` | `HealthPanel` | `CoverageViewer` | `FleetView` |
  `TerminalPanel` | `ManviOpsPanel` | `LoopCanary`
- `scenario` — `mount` | `churn` | `switch` | `storm` | `remount` | `chaos` | `termtabs`
- `termtabs` drives the terminal strip through the real DOM (open / switch / close),
  because that state is internal to the component. Pair it with `TerminalPanel`:
  every open mounts an xterm into a visible box and hides the previous one, which
  is the only place the reveal effects can form a loop. Named apart from the
  `tabs` count parameter below, which means repositories, not terminal sessions.
- `tabs`, `cycles`
- `css=1` — load the real stylesheet, for *looking* at a component instead of
  only stressing it. Off by default: a behaviour run should not depend on
  Tailwind having compiled.

Two rules learned the hard way:

1. **Check `otherCrashes` before believing `depthExceeded: 0`.** A component
   that throws mid-render tears down the effects below it, so an incomplete
   fixture turns a loop into a false clean. Every fixture here is shaped from
   the real interface in `src/lib/**/types.ts` for that reason.
2. **Run `LoopCanary` first.** It reproduces the defect on purpose, so a clean
   sweep can be told apart from a harness that detects nothing. And pick a
   scenario that can actually arm the bug: StoragePanel's loop needs the effect
   to *re-run* against a cached measurement, so `mount` reports a false clean
   and `switch` is what catches it.


## `settings.html` — the settings page's document-side behaviour

Three of the Settings preferences do not live in a component at all: the accent
writes an inline `--c-accent` on `<html>`, "reduce motion" stamps `data-motion`,
and the tab width publishes `--gp-tab-size`. None of that is reachable from
`environment: "node"` — there is no stylesheet, no cascade and no computed
style — so the unit tests can only check that the appliers *would* write the
right thing.

```bash
npx vite --config vite.harness.config.ts
# then open:
#   http://localhost:5188/harness/settings.html
```

The page mounts the real modal through `SettingsHost.svelte` (which owns
`isOpen`, so the close/reopen transitions can be driven) and runs the same
three appliers `App.svelte` runs. `window.__gp` exposes the stores and
`window.__gpSetOpen(bool)` opens and closes the dialog. What it is for:

- click a swatch, then read `getComputedStyle(document.documentElement)` —
  `--c-accent`, `--accent-color` (the canvas graph's input), `--ring-focus` and
  `--shadow-glow` must all move together, and the default must *remove* the
  inline property rather than restate it;
- toggle reduce motion and read `animationName` on a `.gp-view` element;
- pick a tab width and read `tabSize` on a `<pre>`;
- type in the search box and check that a filtered-out row computes to
  `display: none` — the attribute alone is not enough, which is why app.css
  carries `[hidden] { display: none !important }`.

## Terminal

The real-store workbench fixture also exposes **Activity inbox**. Use
`node scripts/workbench-preview.mjs /absolute/path/to/dcstore` with a freshly
built Manvi store binary. The printed profile path is disposable. Create
enhancement/run outcomes through that profile's store API, then check the
global, workspace and repository scopes, read/unread filters, snooze,
dismiss/restore, linked task opening and persistence after reload. These
controls must leave the task and proposal states unchanged.

Expand **Desktop notifications** to exercise the real store settings: sound,
local quiet hours, background preference and workspace/repository/task mutes.
Open a task for its mute control. Clearing saved scope mutes edits the draft;
verify the stored settings remain unchanged until Save, and that saving retains
sound/quiet hours. The browser adapter reports native authorization and delivery
as unavailable; it never pretends that a banner was displayed.

To test recovery, use the disposable profile's public store API to enable
notifications, create a new eligible enhancement/run outcome, claim its notice
and activate the returned exact native identity. Reloading the fixture should
open a task review; change/delete the task before activation to exercise stale
target explanations. Closing acknowledges the activation only. Confirm current
task/proposal states are unchanged and the activation queue is empty. These
seeded records test durable recovery, not a real OS callback. Native permission,
banners, click-after-quit and the optional provider/event bus are separate tests.
The fixture isolates its Vite cache and cleans it after cancelled optimizer
writes drain; its lifecycle regression checks both SIGTERM and failed startup.

Open `/harness/task-runs.html` on the development server and select **Run handoff
checks** to exercise the task Runs panel, repository opening and terminal dock.
Its 21 interaction checks cover lost launch/preparation replies, same-attempt
reconnection, duplicate tab refusal, per-attempt bypass acknowledgment, suspended
polling, exact request text, decision retry after a lost reply, separate answers
and denials, stale request controls, and receipt recovery when reopening review.
This fixture simulates the native process/database transport;
it does not launch a coding agent. Real PTY/store tests live in
`src-tauri/src/workbench/terminal_run.rs`. Installed CLI help checks are explicitly
ignored by default and probe version/help only, without sending a coding task.

Open `/harness/terminal.html` on the development server and run **Run terminal
checks**, followed by **Run input stress checks**. This mounts the real dock,
panels, sessions and xterm with Tauri's official mock IPC; no shell commands
execute. The first suite exercises layout, Find, focus, two repositories, global
capacity, tabs and split panes, including a 200px dock. The input suite checks an
exact 300,000-byte Unicode paste and atomic oversize rejection. Visible counters
report spawns, kills, writes, resizes, runtime errors and individual assertions.

Native PTY stress tests are separate. See [the terminal audit](../docs/TERMINAL_AUDIT.md)
for commands, ownership contracts, reproduced failures and platform limits.

Managed Codex is also covered by `/harness/task-runs.html`: the retained checks
exercise a lost managed launch reply, one-session retry, output loaded on demand,
multiple question answers, Stop without task acceptance, and failed initialization
without a provider thread or automatic replacement (30 checks total).
The transport is simulated. For the real native/provider path, explicitly run the
ignored `installed_managed_codex_crosses_native_host_and_store_without_accepting_task`
Rust test with `GITPULSE_WORKBENCH_TEST_MANVI`,
`GITPULSE_WORKBENCH_TEST_DCSTORE` and `GITPULSE_WORKBENCH_TEST_CODEX` set to absolute
binary paths. That test sends one read-only marker turn to the installed provider.
