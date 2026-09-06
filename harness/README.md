# Runtime harness

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
  `TerminalPanel` | `LoopCanary`
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
