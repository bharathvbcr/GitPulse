# Dependency health remediation — 2026-09-13

Source review began at `bbafcb892e8a3c77d9fe800acfa42fbf1f516f63` in the canonical GitPulse checkout.

## Resolution

- Updated `@lucide/svelte` from 1.44.0 to 1.45.0 in the manifest, lockfile and installed tree. No new dependency.
- CodeQL alert [70](https://github.com/bharathvbcr/GitPulse/security/code-scanning/70) was dismissed as a false positive. Its assertion lives under `#[cfg(test)]` (`terminal/mod.rs:2213,2392`); the value is a synthetic process-ID/counter routing identifier (`686–687`), not a credential. The temporary test fixture and production-log regression both passed unchanged. No security boundary or production logging behavior changed.
- The ecosystem inventory retains 24 display paths, but Cargo audit targets are collected independently before that cut. Added explicit `inventory_only` cap provenance. Target/file/finding caps, missing scanners, failed scanners and legacy notices still prevent a complete-audit claim. The retained/observed counts remain visible.
- Replaced a literal NUL in `tests/hook_protocol_stress.rs` with a Rust byte escape. The emitted payload is byte-for-byte identical. Extended the existing searchable-source contract to native tests and benches. Rebuilding DevMap now parses the previously refused file.
- Health now preserves DevMap's `walk_incomplete` evidence in both the panel and copied remediation report. Entries are candidates requiring caller verification, including when every candidate fits the output budget. An incomplete empty walk never produces a dead-code all-clear.

## All 57 supplied dead-code entries

Every supplied entry has a source caller, callback registration, FFI invocation, or constructed scope guard. No listed code was deleted. These are **verified static references**, not proof that every platform or runtime path was exercised. The list below preserves each of the 57 inputs individually.

Initially DevMap's generation was fresh but partial: one NUL-containing test file was not parsed, and unresolved call attribution remained. After the source-escape fix and rebuild, generation 711 has zero parse failures and reports 57/57 candidates with no output truncation. Its query still discloses unresolved attribution (235518 of 402852 sites after excluding 167334 classified sites). Those DevCouncil inference limits are not repaired by this GitPulse change, and no candidate is hidden or suppressed. Source evidence, rather than a 40–66% graph score, determines retention.

| Reported symbol | Definition file | Verified retention evidence |
| --- | --- | --- |
| `run_tree_killer` | `src-tauri/src/procguard/mod.rs` | [caller/registration](../src-tauri/src/procguard/mod.rs#L915) — Windows taskkill cleanup; also exercised by a timeout test. |
| `CachedProbe.fresh` | `src-tauri/src/ai/mod.rs` | [caller/registration](../src-tauri/src/ai/mod.rs#L273) — Cache freshness filter and retention. |
| `ToolProbe.version` | `src-tauri/src/analyzer/deps.rs` | [caller/registration](../src-tauri/src/analyzer/deps.rs#L399) — Tool availability/version probes. |
| `Wire.send` | `src-tauri/src/bin/gitpulse-mcp.rs` | [caller/registration](../src-tauri/src/bin/gitpulse-mcp.rs#L182) — MCP response writer. |
| `verdict` | `src-tauri/src/commands/mod.rs` | [caller/registration](../src-tauri/src/commands/mod.rs#L2712) — Unit-test verdict factory. |
| `a_newly_opened_repository_publishes_its_map_without_a_manual_build.ResetCooldowns` | `src-tauri/src/devmap/live.rs` | [caller/registration](../src-tauri/src/devmap/live.rs#L1412) — Constructed test-scope guard; Drop restores shared state. |
| `failed_build_storm_enters_the_same_cooldown.ResetCooldowns` | `src-tauri/src/devmap/live.rs` | [caller/registration](../src-tauri/src/devmap/live.rs#L1186) — Constructed test-scope guard; Drop restores shared state. |
| `unchanged_echo_cooldown_blocks_activation_when_status_is_stale.ResetCooldowns` | `src-tauri/src/devmap/live.rs` | [caller/registration](../src-tauri/src/devmap/live.rs#L1244) — Constructed test-scope guard; Drop restores shared state. |
| `unchanged_echo_cooldown_still_rebuilds_obsolete_payload_on_activation.ResetCooldowns` | `src-tauri/src/devmap/live.rs` | [caller/registration](../src-tauri/src/devmap/live.rs#L1300) — Constructed test-scope guard; Drop restores shared state. |
| `unchanged_repo_changed_storm_is_bounded_by_cooldown.ResetCooldowns` | `src-tauri/src/devmap/live.rs` | [caller/registration](../src-tauri/src/devmap/live.rs#L1097) — Constructed test-scope guard; Drop restores shared state. |
| `IndexTransaction.prepare` | `src-tauri/src/diff/conflict_session.rs` | [caller/registration](../src-tauri/src/diff/conflict_session.rs#L864) — Conflict resolution stages the prepared index. |
| `IndexTransaction.publish` | `src-tauri/src/diff/conflict_session.rs` | [caller/registration](../src-tauri/src/diff/conflict_session.rs#L898) — Conflict resolution atomically publishes the index. |
| `Drained.append` | `src-tauri/src/engine/git_cli.rs` | [caller/registration](../src-tauri/src/engine/git_cli.rs#L1943) — Captured output drain. |
| `InputFeed.descriptor` | `src-tauri/src/engine/git_cli/pipe_drain.rs` | [caller/registration](../src-tauri/src/engine/git_cli/pipe_drain.rs#L207) — Method reference passed to optional stdin mapping. |
| `PipeDrain.descriptor` | `src-tauri/src/engine/git_cli/pipe_drain.rs` | [caller/registration](../src-tauri/src/engine/git_cli/pipe_drain.rs#L199) — poll descriptor construction. |
| `Worker.take` | `src-tauri/src/engine/git_cli/thread_io.rs` | [caller/registration](../src-tauri/src/engine/git_cli/thread_io.rs#L197) — Collects worker output and completion state. |
| `cancel_until` | `src-tauri/src/engine/git_cli/thread_io.rs` | [caller/registration](../src-tauri/src/engine/git_cli/thread_io.rs#L194) — Worker cancellation deadline. |
| `git_text` | `src-tauri/src/engine/git_writer.rs` | [caller/registration](../src-tauri/src/engine/git_writer.rs#L2811) — Test-local Git helper for upstream assertion; distinct from production import. |
| `Lcg.next_u64` | `src-tauri/src/github/mod.rs` | [caller/registration](../src-tauri/src/github/mod.rs#L3126) — Adversarial parser-test generator. |
| `Segment.alloc_until` | `src-tauri/src/graph/lane_solver.rs` | [caller/registration](../src-tauri/src/graph/lane_solver.rs#L504) — Graph-lane interval allocation. |
| `ScopeFailure.verdict` | `src-tauri/src/harness/mod.rs` | [caller/registration](../src-tauri/src/harness/mod.rs#L100) — Policy failure conversion. |
| `a_bound_worktree_fails_closed_when_its_task_scope_disappears.Clear` | `src-tauri/src/harness/mod.rs` | [caller/registration](../src-tauri/src/harness/mod.rs#L755) — Constructed test-scope guard; Drop restores shared state. |
| `a_bound_worktree_sends_its_scope_and_keeps_the_task.Clear` | `src-tauri/src/harness/mod.rs` | [caller/registration](../src-tauri/src/harness/mod.rs#L560) — Constructed test-scope guard; Drop restores shared state. |
| `a_linked_worktree_gate_row_is_visible_once_from_the_family_ledger.Clear` | `src-tauri/src/harness/mod.rs` | [caller/registration](../src-tauri/src/harness/mod.rs#L701) — Constructed test-scope guard; Drop restores shared state. |
| `Slot.ensure` | `src-tauri/src/harness/sidecar.rs` | [caller/registration](../src-tauri/src/harness/sidecar.rs#L1096) — Acquired sidecar slot starts/reuses host. |
| `ActorKind.as_str` | `src-tauri/src/ledger/mod.rs` | [caller/registration](../src-tauri/src/ledger/mod.rs#L936) — Ledger insert serializes actor kind. |
| `Outcome.as_str` | `src-tauri/src/ledger/mod.rs` | [caller/registration](../src-tauri/src/ledger/mod.rs#L943) — Ledger insert serializes outcome. |
| `FileSink.tail` | `src-tauri/src/logging.rs` | [caller/registration](../src-tauri/src/logging.rs#L796) — Diagnostics tail reader. |
| `FileSink.write_line` | `src-tauri/src/logging.rs` | [caller/registration](../src-tauri/src/logging.rs#L674) — Production log writer. |
| `SlowCommands.observe` | `src-tauri/src/logging/performance.rs` | [caller/registration](../src-tauri/src/logging/performance.rs#L112) — Performance guard records command duration. |
| `Cleaner.perform` | `src-tauri/src/storage/hygiene/global.rs` | [caller/registration](../src-tauri/src/storage/hygiene/global.rs#L590) — Global cleanup execution. |
| `WatcherState.begin_watch_slot` | `src-tauri/src/watcher/mod.rs` | [caller/registration](../src-tauri/src/watcher/mod.rs#L935) — Test-only watcher capacity helper. |
| `WatcherState.is_watching` | `src-tauri/src/watcher/mod.rs` | [caller/registration](../src-tauri/src/watcher/mod.rs#L891) — Test-only watcher registration helper. |
| `WatcherState.watch_count` | `src-tauri/src/watcher/mod.rs` | [caller/registration](../src-tauri/src/watcher/mod.rs#L890) — Test-only watcher count helper. |
| `WorkbenchState.reserve` | `src-tauri/src/workbench.rs` | [caller/registration](../src-tauri/src/workbench.rs#L506) — Workbench queue reservations. |
| `WorkbenchState.with_run_receipt` | `src-tauri/src/workbench.rs` | [caller/registration](../src-tauri/src/workbench/terminal_run.rs#L156) — Terminal-run receipt access. |
| `read.CloseHandle` | `src-tauri/src/workbench/process_birth.rs` | [caller/registration](../src-tauri/src/workbench/process_birth.rs#L103) — Windows FFI handle cleanup. |
| `read.GetProcessTimes` | `src-tauri/src/workbench/process_birth.rs` | [caller/registration](../src-tauri/src/workbench/process_birth.rs#L100) — Windows FFI process birth time. |
| `trackedRequests.next` | `src/lib/components/RepoMapPanel.svelte` | [caller/registration](../src/lib/components/RepoMapPanel.svelte#L243) — Repository-map request generation. |
| `reveal` | `src/lib/components/TerminalPanel.svelte` | [caller/registration](../src/lib/components/CoverageAgentPrompt.svelte#L118) — Registered session reveal callback invoked by agent-session action. |
| `createLifecycle.exit` | `src/lib/components/TerminalSession.svelte` | [caller/registration](../src/lib/terminal/sessionLifecycle.ts#L126) — Session lifecycle callback. |
| `createLifecycle.output` | `src/lib/components/TerminalSession.svelte` | [caller/registration](../src/lib/terminal/sessionLifecycle.ts#L120) — Session lifecycle callback. |
| `createLifecycle.reset` | `src/lib/components/TerminalSession.svelte` | [caller/registration](../src/lib/terminal/sessionLifecycle.ts#L171) — Session lifecycle callback. |
| `createLifecycle.started` | `src/lib/components/TerminalSession.svelte` | [caller/registration](../src/lib/terminal/sessionLifecycle.ts#L118) — Session lifecycle callback. |
| `createLifecycle.state` | `src/lib/components/TerminalSession.svelte` | [caller/registration](../src/lib/terminal/sessionLifecycle.ts#L48) — Session lifecycle callback. |
| `createLifecycle.warning` | `src/lib/components/TerminalSession.svelte` | [caller/registration](../src/lib/terminal/sessionLifecycle.ts#L115) — Session lifecycle callback. |
| `refresh.collisions` | `src/lib/components/WorkView.svelte` | [caller/registration](../src/lib/work/refresh.ts#L37) — Work refresh observer callback. |
| `refresh.finished` | `src/lib/components/WorkView.svelte` | [caller/registration](../src/lib/work/refresh.ts#L43) — Work refresh observer callback. |
| `refresh.projection` | `src/lib/components/WorkView.svelte` | [caller/registration](../src/lib/work/refresh.ts#L31) — Work refresh observer callback. |
| `guide.destroy` | `src/lib/components/onboarding/ProductTour.svelte` | [caller/registration](../src/lib/components/onboarding/ProductTour.svelte#L96) — Svelte action registration; framework invokes destroy on teardown. |
| `createPaneDetails.register.dispose` | `src/lib/diagnostics/paneCrash.ts` | [caller/registration](../src/lib/components/RepoMapPanel.svelte#L217) — Pane diagnostics cleanup registered with Svelte. |
| `createPaneDetails.register.update` | `src/lib/diagnostics/paneCrash.ts` | [caller/registration](../src/lib/components/RepoMapPanel.svelte#L200) — Pane diagnostics context update. |
| `createSessionLifecycle.launch.onError` | `src/lib/terminal/sessionLifecycle.ts` | [caller/registration](../src/lib/terminal/ptyBus.ts#L72) — PTY bus callback dispatch. |
| `createSessionLifecycle.launch.onExit` | `src/lib/terminal/sessionLifecycle.ts` | [caller/registration](../src/lib/terminal/ptyBus.ts#L139) — PTY bus callback dispatch. |
| `createSessionLifecycle.launch.onOutput` | `src/lib/terminal/sessionLifecycle.ts` | [caller/registration](../src/lib/terminal/ptyBus.ts#L118) — PTY bus callback dispatch. |
| `createSessionRegistry.reserve.release` | `src/lib/terminal/sessionRegistry.ts` | [caller/registration](../src/lib/terminal/sessionLifecycle.ts#L53) — Registry reservation release. |
| `createSessionRegistry.reserve.update` | `src/lib/terminal/sessionRegistry.ts` | [caller/registration](../src/lib/terminal/sessionLifecycle.ts#L47) — Registry reservation status update. |

## Verification

- Before the reporting fix: 75 frontend baseline tests and 63 dependency-scanner tests passed; three new report regressions and the new Rust inventory-cap regression failed on the original implementations.
- The expanded searchable-source contract failed on the literal NUL before replacement. Comparing the original raw byte-string contents to the escaped byte literal established identical payload bytes.
- `npm test`: 6,750 passed across 489 files. After the final fixture/source-escape adjustments, the affected frontend/contract selection passed 197 tests across nine files; the searchable-source and fixture contracts also passed 36 tests.
- `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib analyzer::deps::tests -- --test-threads=1`: 64 passed.
- `cargo test --manifest-path src-tauri/Cargo.toml --locked --test hook_protocol_stress -- --test-threads=1`: all five passed. The hostile-payload test also passed before the source-escape change.
- Terminal fixture `a_terminal_still_opens_under_a_sticky_world_writable_holder` and `log_sites_do_not_interpolate_session_identifiers`: both passed unchanged (one test collected per exact invocation).
- `npm run check`, `npm run check:types`, `npm run check:ipc`, `npm run build`, and `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`: passed.
- `npm audit --ignore-scripts --json`: zero vulnerabilities, 175 dependencies. `cargo audit --deny warnings --file src-tauri/Cargo.lock`: passed with 550 dependencies and no advisories/warnings. These are the two tracked audit lockfiles; neither command uses the Health inventory display cap. `npm outdated --json`: `{}`.
- GitHub readback: alert 70 is dismissed as `false positive`; code-scanning open-alert list is empty. Dependabot open-alert list is also empty.
- Rendered the actual HealthPanel in the in-app browser using the documented `health-evidence=rows` and `health-evidence=empty` fixtures. Both visibly preserved incomplete-call warnings. The real Copy report button produced text with the warning and 24/783 counts; the populated result retained candidate/exemption labels, and the empty result contained no dead-code all-clear. The fixture captures clipboard writes for inspection without changing the system clipboard.

This is local source/build/browser-fixture validation. No commit, push, application installation, release packaging, full native/coverage CI run, Windows execution, or Linux execution was performed. DevMap's unresolved receiver/call attribution remains explicitly disclosed.
