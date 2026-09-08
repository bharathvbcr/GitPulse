# Runtime harness

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

Open `/harness/terminal.html` on the development server and run **Run terminal
checks**, followed by **Run input stress checks**. This mounts the real dock,
panels, sessions and xterm with Tauri's official mock IPC; no shell commands
execute. The first suite exercises layout, Find, focus, two repositories, global
capacity, tabs and split panes, including a 200px dock. The input suite checks an
exact 300,000-byte Unicode paste and atomic oversize rejection. Visible counters
report spawns, kills, writes, resizes, runtime errors and individual assertions.

Native PTY stress tests are separate. See [the terminal audit](../docs/TERMINAL_AUDIT.md)
for commands, ownership contracts, reproduced failures and platform limits.
