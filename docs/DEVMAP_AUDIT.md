# DevMap audit and hardening

Scope: GitPulse's file, symbol, subsystem, and document maps, the shared DevCouncil visualization payload, and the browser preview. This records implementation and bounded local verification, not release or universal correctness certification.

## Required outcomes

| Area | Required behavior | Evidence / status |
| --- | --- | --- |
| Relationships | File view projects real cross-file calls, references, and type relationships; symbol view retains symbol relationships. Ownership must come from indexed nodes, with no invented endpoints. | Fixed at the DevCouncil owner and synchronized. MarkDev preview: 5,094 links, all 226 Swift files connected, 19 isolated files; 13 unresolvable source records explicitly reported. The 5 initial projection regressions failed before the change and pass after. |
| Payload integrity | Invalid or ambiguous identities and endpoints cannot crash the viewer or invent edges. Counts, omissions, confidence, and caps remain explicit. | Backend rejects ambiguous IDs/ownership, aggregates unique evidence conservatively, and caps rendered links at 50,000. Frontend validates unknown payloads and coverage metadata, bounds processing/rendered samples, and reports rejected rows and link-only truncation. Adversarial browser checks and 300 deterministic malformed-payload fuzz cases pass. |
| Layout | Dependencies influence placement; disconnected nodes remain legible; coordinates are deterministic, bounded, and stable through refresh. | Dependency-based slot swaps reduce total squared edge length while retaining non-overlapping positions. Eight passes and two million neighbor visits bound refinement. Row-order stability, spacing, extreme supplied coordinates, and viewport fitting have regression coverage. This is a bounded heuristic, not a claim of globally optimal layout. |
| Exploration | Directed neighbor lists, bounded multi-hop tracing, language/test/generated/isolated filters, and keyboard-accessible node browsing. | Implemented and tested: 30-node pages, 20-connection pages, compound filters, one-to-three-hop tracing capped at 1,000 nodes / 50,000 edge visits. Undirected subsystem neighbors traverse either way and receive no dependency arrowheads. All 5,000 groups remain reachable through 40-group pages. |
| State | Same-repository refresh preserves valid selection, filters, and camera; switching repositories resets context. Hover and inspector agree. | Browser checks pass for refresh, selection removal, repository switching, compound-filter persistence, and camera preservation. RepoMapPanel provides a stable repository + view key. Selected nodes take precedence over hovered nodes in both the inspector and trace. |
| Rendering | Correct viewport culling and label priorities, theme contrast, usable targets, empty/error/truncated explanations, and narrow-panel behavior. | Offscreen geometry and labels are culled; labels and group controls are bounded. Hard-filtered nodes cannot be picked. Fit view calculates graph bounds instead of merely resetting offsets. Controls use the component's measured width, and long node text is clipped. Light/dark rendering and browser activation checked; compiler reports no map accessibility warnings. |
| Scale | Measure real browser layout, paint, and interaction with dense and sparse maximum-size samples, not just model construction. Bound expensive work and retain regressions. | Six real-browser cases pass: 5,000 nodes with 0 / 5,000 / 50,000 links at 320 / 1,440 px. Final measured load/settle times 47–532 ms; maximum measured interaction frames 17–30 ms. These are local observations, not cross-device guarantees. Rust stress retains 122,150 total source edges while rendering 50,000. |
| Integration | Canonical upstream changes synchronized through vendoring; file/symbol/doc/subsystem adapters, native loading, and standalone HTML remain coherent. | Upstream 10 visualization + 6 projection tests and 9 native loader tests pass. Native reads cap input at 128 MiB, reject malformed shapes and incomplete legacy exports, and retain valid empty graphs. Scoped vendoring matches devmap-query and preserves unrelated drift. Installed native application remains unverified. |
| Final gates | Focused regressions, frontend checks/build, native tests, browser interactions, and relevant wider checks pass; any unavailable gate is named. | 83 focused frontend tests, 3 scheduler tests, frontend check/typecheck, production build, 9 native loader tests, native Clippy with warnings denied, strict browser functional checks, and strict browser stress matrix pass. The wider checkpoint had 3 concurrent ConflictEditor failures; all affected tests pass in the final 24-test targeted recheck. The entire suite was not rerun after that recheck. |

## Workspace boundaries

- GitPulse and DevCouncil contain concurrent edits. Preserve unrelated work.
- The initial vendor check reports pre-existing drift in devmap-extract (`src/cache.rs`, `src/treesitter.rs`, `build.rs`); the devmap-query copy initially matches upstream.
- GitPulse MCP's code-intelligence facet cannot read schema 19 with its installed binary. Its worktree/collision facets succeeded: one scanned worktree, no cross-worktree overlap. Source and GitNexus are used for this audit instead.

## Completed earlier in this task

- Language colors reuse the app palette and adapt to light/dark themes.
- Mixed-community labels prefer source directories; test files remain counted and visible. Duplicate inferred labels carry stable community identifiers.
- Removed stale canvas caching; same-count payload updates repaint.
- Added search, fit/zoom, selection, keyboard pan/zoom/open, drag preservation, and clear empty/unavailable states.

These earlier changes are subject to the final integration audit above.

## September 7 hardening checkpoint

- Frontend: 78 focused tests across six files pass. `npm run typecheck` and production build pass.
- Live Svelte browser: same-count repaint, malformed payload containment, empty state, and unavailable reason all pass; no browser errors reported.
- Upstream: `cargo clippy -p devmap-query --no-default-features --lib --test viz_projection --example viz_snapshot -- -D warnings` passes.
- Full `npm run check` currently fails in concurrent `src/lib/terminal/sessionLifecycle.test.ts` work: `.mock` access on a plain function type, a deferred promise/mock return mismatch, and `mockResolvedValueOnce()` without an argument. No DevMap diagnostics were reported. This is a failed workspace gate, not a pass.
- Only `devmap-query/src/viz.rs` and its manifest record changed under `src-tauri/vendored`. The scoped vendor command is `node scripts/vendor-crates.mjs --crate=devmap-query`.
- Regenerate the local preview from the canonical projection with `cargo run -p devmap-query --no-default-features --example viz_snapshot -- /absolute/path/to/code_graph.json` in DevCouncil's `rust-port`; redirect stdout to the preview JSON. `--symbols` selects the symbol projection.
- The pending implementation and browser checks listed at this checkpoint were addressed in the following pass.

## Final local hardening pass

- Reproduced before fixing: refresh lost selection; layout ignored all edges; 5,000 isolated communities created 5,001 controls; offscreen geometry still submitted draw calls; undirected subsystem neighbors acquired false direction; Fit view left distant supplied coordinates offscreen; invalid coverage displayed values such as `20 of 4` or `[object Object]`; native loaders treated malformed shapes as available and parsed oversized input before rejecting it.
- Retained regressions cover those failures. The browser harness additionally verifies combined filters, paging beyond the first 30 nodes, incoming/outgoing lists, activating a root-level Swift file with Enter, and clearing selection when its node disappears.
- `Run stress checks` mounts the production Svelte/canvas implementation and measures load plus frame completion. Final stable-server measurements: sparse 320/1440 px = 176/74 ms load, 19/17 ms maximum frame; dense = 532/347 ms load, 26/30 ms maximum frame; isolated = 87/47 ms load, 18/17 ms maximum frame. Eight keyboard pan/zoom interactions are measured per case. No hot reload was enabled for this final run.
- A final server-log audit found two `ResizeObserver loop completed with undelivered notifications` errors that the initial harness did not include in its verdict. The harness now fails on browser errors and unhandled promise rejections. That stricter check reproduced the failure before the fix. Resize measurements now use a separate frame scheduler, canceled on teardown; the strict stress matrix passes after the fix. The three scheduler regressions also pass.
- Full frontend suite at the broader checkpoint: 5,028 passed / 3 failed. The failures were two ConflictEditor source contracts and its CSS-token contract (`--font-mono` and `--side-color`). Concurrent work resolved them; the final targeted recheck passes all 24 tests in those two files. This supersedes the known failures but is not a new full-suite run.
- Latest `npm run check`: zero errors and zero warnings. Production build passes with the existing large-chunk warning. `cargo clippy --manifest-path src-tauri/Cargo.toml --lib -- -D warnings` passes.
- `node scripts/vendor-crates.mjs --check --allow-drift`: no vendor-local edits, devmap-query matches upstream. Extractor and store drift belong to concurrent upstream work and were preserved.
- The four central existing implementation files (`graphPayload.ts`, `CodeGraphCanvas.svelte`, `CodeGraphRenderer.ts`, and native `viz.rs`) have 990 added / 379 removed lines against HEAD; new navigation/test/harness files and the upstream projection are separate. This replaces the old layout and cached rendering paths rather than retaining a second renderer. No dependency was added.

## Verification limits

- The installed Tauri application was not replaced or exercised with this build. Browser callback checks and native loader tests do not prove physical editor navigation in the installed application.
- Backend projection cannot reconstruct relationships absent from the source index; unresolved evidence remains explicitly counted. Test/generated classification uses naming conventions, and connected-only / trace results describe the loaded sample.
- Performance figures cover the recorded local browser, sample shapes, widths, and interaction sequence. They are not a guarantee for every device, every graph, or assistive technology. No release, signing, or cross-platform certification was requested or performed.
