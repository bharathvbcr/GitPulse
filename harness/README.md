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

- `c` — `PulseView` | `StoragePanel` | `HealthPanel` | `CoverageViewer` | `LoopCanary`
- `scenario` — `mount` | `churn` | `switch` | `storm` | `remount` | `chaos`
- `tabs`, `cycles`

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
