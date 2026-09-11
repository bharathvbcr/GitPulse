# Global repository hygiene audit

Scope: GitPulse's Storage/Fleet/settings and native cleanup paths, plus the
portable Rust policy and managed guide writer in DevCouncil. Verified locally
on macOS on 2026-09-09. This is an implementation audit with finite regression
coverage, not a claim that every repository or operating system has been tested.

## Findings reproduced and fixed

| Finding | Fix and retained regression |
| --- | --- |
| Storage treated ignored environments, agent state and scratch as safe reclaim. | Preserved categories require review; `hygiene_regression` proves they cannot be offered as safe cleanup. |
| The 64-artifact cap could look like a complete inventory. | Storage now propagates truncation; a 70-artifact fixture proves the global cleaner cannot interpret the capped sample as complete. |
| Permission failures in a storage subtree did not make global discovery incomplete. | Global inventory propagates permission failures and refuses cleanup. A real inaccessible-directory fixture reproduces the omission. |
| Credentials with different letter case and additional model/data formats escaped preservation checks. | Canonical case-insensitive policy and protected-entry checks, tested with `.ENV.production`, ONNX, PTH and Parquet fixtures. |
| The reusable eligibility API accepted parent-directory traversal and missed Windows-style protected paths. | Bounded literal-relative paths and separator-aware preservation, tested directly in DevCouncil. |
| Corrupt lock paths looked like an active cleaner; unfinished runs could continue to display as running after a crash. | Lock errors are unavailable, not busy; orphaned runs report interrupted without replay. |
| Closing a lock descriptor did not release ownership while a fork/duplicate retained the file description. | Explicit unlock in the shared lock guard. Both a duplicate-descriptor regression and the concurrent full suite pass. |
| A background registration failure could leave an enabled policy if its recovery write hit a busy state lock. | Save disabled authority before OS work; enable only after successful setup. Registration failure, interrupted setup and busy-writer tests prove refusal. |
| Storage inspection could execute a configured Git filesystem-monitor hook. | Reuse lightweight worktree listing and disable fsmonitor in index/ignore/status checks. A hook fixture verifies neither inventory nor preflight runs it. Worktree-list failures now propagate instead of becoming empty inventory. |
| Native browser tests stalled when their webview became inactive. | The ephemeral WKWebView disables inactive scheduling suspension; all original and new assertions remain. Both hygiene and existing diagnostics pass. Production webview preferences are unchanged. |

These failures were observed before their corresponding fixes. The additional
suite exercises path replacement, symlinks, new files appearing during deletion,
tracked/index/ignore changes after preview and during policy review, cancellation,
expired/foreign/reused plans, protected nested repositories, oversized state,
unsupported policy versions, depth limits, exclusions and dirty worktrees.

## Scheduling and mutation checks

- Sixteen simultaneous schedule claims produce one durable winner. Missed
  intervals are coalesced; a stale UI cannot restore an old due time.
- Multiple service instances share file locks and cancellation/revision checks.
  Editing exclusions inside the execution gate preserves real generated files.
- The real global worker discovers two temporary repositories, journals before
  deletion, removes only the reviewed Python bytecode directory within its byte
  budget and preserves source. Only activity and the external policy verdict
  are injected in this fixture; filesystem, Git, discovery, snapshots, policy
  revision, limits, journal and exact-entry removal are real.
- Corrupt task state and active leases refuse global cleanup. Incomplete scans
  invoke no mutation gate. Twenty-four successive runs retain at most 20 records.
- launchd fixtures validate plist syntax using macOS `plutil`, path escaping,
  fixed headless arguments, foreign-job preservation, unavailable launchctl,
  idempotent disable and durable revocation before unloading. No user LaunchAgent
  was installed by these tests.
- Native Go validation builds into a disposable cache, runs the production
  prepared-operation executor and checks that source and the executable remain.

## Verified results

| Check | Result |
| --- | --- |
| Installed compiler and Cargo | `rustc 1.98.0`, `cargo 1.98.0`, `stable-aarch64-apple-darwin`; this checkout's native build used this toolchain. No repo toolchain pin. |
| Frontend unit tests and coverage | 5,184 passed; one existing absent `PROMO.md` draft check skipped. 96% lines, 94% statements, 88.62% branches, 95.57% functions. |
| GitPulse Rust library | 1,542 passed; three opt-in/manual/network tests ignored by the default run. The Go test passed separately. |
| Tauri bridge and storage integrations | 31 bridge tests, 28 storage stress tests and two hygiene regressions passed. |
| Rendered hygiene UI | 58/58 Chromium and 58/58 macOS WKWebView assertions, including global settings and cancellation. Standalone global layout also inspected visually. |
| Existing WKWebView diagnostics | 24/24 passed after the test-view scheduling correction. |
| DevCouncil embedding configuration | `devmap-query --no-default-features`: 159 library tests and 23 integration tests passed; all-target Clippy passed. |
| Contracts and build | 196 commands; 52 contracts / 948 fields; Svelte/TypeScript, workflow/release/schema checks, vendor integrity, Rust formatting, all-target Clippy and frontend production build passed. |
| Headless entry point | Built and ran `gitpulse --cleaner-due`; exited successfully with no configured schedule and no window. |

The production build retains its existing large-chunk advisory. GitNexus
reported low risk for the indexed changes, but it omits new untracked code and
its symbol mapping is incomplete. DevMap also reports missing call edges.
Source call-site inspection, Git diffs, full native tests and integration tests
supply evidence the indexes cannot provide; graph output alone is not complete
impact proof.

## Explicit remaining gates

The feature is implemented in the GitPulse worktree and the isolated DevCouncil
`codex/global-hygiene` worktree; the DevCouncil policy is re-vendored into this
GitPulse build. Concurrent edits in the canonical checkouts were preserved.
No dependencies were added. Nothing has been committed, installed or released.

Installed/signed application registration with launchd, logout/login, sleep/wake,
upgrades and actual unattended deletion have not been exercised against a user's
real projects. Linux execution and Windows support are not verified; Windows
cleanup is deliberately unavailable. Real shared npm/uv/pnpm cache mutations,
remote CI and Rust coverage collection were not run. Native Go was exercised
only with temporary data.

Activity checks observe external processes; they cannot freeze an unrelated
builder that starts after the final check. Deletion is permanent and may be
partial on cancellation or concurrent changes. Sizes are logical bytes, not a
promise of physical APFS space reclaimed. Unknown producers, dependency stores,
virtual environments, source/index changes and shared downloads are preserved
or require manual review. The adapter catalog is extensible; unsupported
languages or output layouts are not silently treated as safe.

Change footprint: GitPulse adds 5,101 lines and removes 44 across 44 files; the isolated DevCouncil change adds 319 lines across six files. These totals include tests, fixtures, documentation and the vendored policy copy.
