# macOS menus and status icon

Audit date: 2026-09-09. Baseline: `3d61e2e` in the task checkout.

**Verified in source:** GitPulse already had a native application menu. This change expands it and adds a separate, optional icon on the right of the macOS menu bar. The foreground application's menus and its background status icon are separate surfaces. No installed application has been replaced.

## Compact status icon

Enable **Settings → Layout → Menu bar status icon**. The menu bar shows one monochrome pulse icon, with no repository name, count or other text beside it.

Click the icon to open a compact popover. Its monochrome header shows the repository switcher and branch. Three equal cards show **Changed**, **Staged** and **Conflicts** in large type; nonzero counts open the relevant view. A short upstream row shows ahead/behind counts and an explicit **last fetch** caption. One blue primary action changes with context: **Review changes**, **Resolve conflicts**, **View history** or **Try again**. Active Git work and parked operations remain visible in the headline. Empty and unavailable states avoid misleading zero counts. Names truncate in the compact view and remain available in tooltips/details. Escape collapses the switcher or details first, then dismisses the panel; `R` refreshes, `1`–`3` open nonzero cards and `4` opens listed stashes in Work.

**Details** contains the stash count, command palette, workspace insights, path, branch, changed/staged/conflict counts, upstream, watcher state, parked operations and running Git actions. It also exposes History, Pulse, Fleet and Terminal, plus copy branch, reveal folder, remote website and appearance shortcuts. A bounded, keyboard-scrollable area keeps the footer reachable. **Refresh**, appearance, copy and repository switching stay in the popover. Reveal and Remote dismiss the panel without bringing GitPulse forward. The footer contains **Open GitPulse**, **Settings** and **Quit**. Right-clicking the icon keeps the native Open/Settings/Quit escape path and adds the current primary action and Refresh when available. Light/dark appearance, reduced transparency and reduced motion follow the main app's preferences and system settings. The popover still cannot run Git mutations; those stay on the application menu and in the main window.

On macOS, the status window uses the same native `UnderWindowBackground` material as the main app. The panel and cards carry translucent tints with a restrained highlight; the native window supplies desktop blur without a second CSS filter. Its 360px content width and 18px corners match the panel, and reopening preserves short connecting/error heights so material does not extend beyond the footer. Other native platforms retain opaque surfaces. Reduced transparency, increased contrast and forced colors restore opaque surfaces and remove highlights.

The icon reuses the current workspace state; enabling it starts no additional polling, provider calls or background services. It is off by default. While enabled, closing the main window hides it and preserves its session. The popover dismisses on Escape, focus loss or closing its window. Disabling the icon reveals the main window first. Quit retains draft/save checks and waits for tracked Git work, with a deadline that keeps the app open on failure.

## Application menu inventory

Menu order: **GitPulse · File · Edit · View · Go · Repository · Window · Help**.

| Menu | Available commands |
| --- | --- |
| GitPulse | About, Settings, Check for Updates, Services, Hide, Hide Others, Show All, Quit. Update checks report available/current/failed and do not install anything. |
| File | Open/Clone Repository, Open Recent, Clear Recent Repositories, Open Repositories, Close/Reopen Repository Tab, Next/Previous Repository Tab, Close Window. Recent history is capped at 12; identical names display distinguishing paths. Clearing history preserves repositories, tabs and closed-tab recovery. |
| Edit | Native Undo, Redo, Cut, Copy, Paste, Select All for the focused control. |
| View | Work, Code, History, Insights, Fleet, Show/Hide Terminal, Search Commits, Command Palette, Refresh, System/Light/Dark Appearance, Toggle Dark/Light, Full Screen, Zoom In, Zoom Out, Actual Size. |
| Go → Work | Overview, Resolve, Remote, Stack, Policy. |
| Go → Code | Explorer, Blame, Map. |
| Go → History | Graph, Diff, Reflog. |
| Go → Insights | Pulse, Coverage, Health, Storage. |
| Repository | Fetch, Pull, Push, Stash Working Tree, Pop Stash, Stage All, Unstage All, Quick Commit, Interactive Rebase, Create Branch, Rename Current Branch, Continue/Abort/Skip Operation, Copy Repository Path, Copy Branch, Copy Selected Commit SHA or HEAD SHA, Reveal in Finder, Open Remote Website. |
| Window | Minimize, maximize/zoom, Close Window. |
| Help | GitPulse Help, Keyboard Shortcuts, Diagnostics, Set Up Optional Tools, Release Notes, Report an Issue. Opening setup or an issue page does not install tools or submit an issue. |

Go uses the existing view registry and per-repository section persistence. Navigation closes Fleet so its destination is visible. Help remains available without a repository. Zoom uses the existing persisted scale and limits (`⌘=`, `⌘-`, `⌘0`). Keyboard Shortcuts uses `⌘/`; `?` remains available. Native shortcut ownership prevents duplicate execution in the webview.

## State, targeting and recovery

- Availability follows loaded repository state, bare repositories, conflicts, stash-read failures, supported parked-operation actions and running Git work. Refresh remains available for recovery from a failed load. Stage All can stage conflict resolutions; Continue waits until conflicts are resolved.
- Checkmarks reflect the active view, section, repository, Terminal and explicit appearance preference. Labels show Show/Hide Terminal and work in progress. Native checkmarks are reconciled after clicks and recent-menu rebuilds.
- Native events capture the repository path. The frontend rejects stale events and rechecks path, generation, branch and operation after dialogs. Bulk staging captures the original repository session for every file. Quick Commit cannot retarget another repository when its dialog completes.
- Git mutations reuse the existing guarded store and backend commands. They report actual outcomes, including partial bulk failures. Interactive rebase now uses the same mutation/activity owner. Abort/Skip retain consequence confirmations.
- Copy HEAD resolves a current full object ID. Remote Website uses the default/origin remote, rejects ambiguous or truncated-only choices and unsupported URLs, and strips embedded credentials before opening the browser. Repository reveal uses the existing repository resolver.
- Native presentation updates are serialized and coalesced to the latest state, with bounded retries. GUI operations run on the main thread. Unknown IDs and oversized or inconsistent payloads are rejected. A failed update is never recorded as successfully applied.

## Ownership

`src-tauri/src/desktop/actions.rs` owns native IDs and the native projection of the 16 sections. `menu.rs` constructs and updates the application menu; `tray.rs` owns the icon and native right-click menu; `popover.rs` owns the lazy status window, positioning, dismissal and restricted action bridge. `state.rs` validates presentation. `desktop/mod.rs` owns event routing, main-thread updates and window/exit behavior.

`src/lib/desktop/menuState.ts` derives presentation from repository, interface, theme and mutation state. `menuStateStore.ts` connects those stores, `menuSync.ts` serializes delivery, and `menuCommands.ts` routes actions into existing handlers. `nativeActions.ts` validates dispatch. `repoStore.ts` remains the Git mutation owner. Wire contracts cover the new state structs and event repository identity.

`status.html` and `src/status.ts` form a separate lightweight entry point. `StatusApp.svelte` uses `statusConnection.ts` to subscribe before its initial read, retry failed subscriptions and reject responses superseded by live updates; it sends actions through the native bridge; `StatusPopover.svelte` renders the panel. It does not mount App or create another repository store. Native and frontend routing both reject stale repository actions. Resize delivery reuses the serialized update mechanism, and native placement selects the display from physical monitor bounds and clamps to its work area. This avoids mixing Retina tray coordinates with the logical-point macOS display lookup.

The existing Tauri dependency enables its already-locked `tray-icon` feature. Six IPC commands are added: menu presentation, repository reveal, current HEAD ID, status snapshot, restricted status actions and status sizing. The status window capability grants only core event listen/unlisten permissions; its three commands also verify the calling window label. Its action allowlist contains section navigation, workspace controls, copy/reveal/remote utilities, refresh and existing app controls, with no direct Git mutation commands. No new package dependency, automatic update check or provider integration is introduced.

## Verification

Regression tests reproduce the original repository-retargeting bugs in bulk staging and Quick Commit before the fixes. Additional regressions reproduce a dropped status update after the final retry and immediate-idle quit subscription failure. Tests cover cancellation, failed commands, duplicate invocations, stale dialogs/events, absent and failed state, URL normalization, bounds, checkmarks and menu rebuilding.

Native main-thread tests construct actual macOS menu objects using Tauri's MockRuntime. Desktop integration tests exercise native event emission. Frontend tests and the WebKit harness exercise their respective layers; neither substitutes for a packaged-app click.

The interactive fixture at `harness/status.html` renders the production popover with sample data and a labeled simulated backdrop. Its Material control switches between Glass and Opaque. Run `npm run test:webkit -- --harness status` for **51 real WebKit checks** covering the three-card layout and native window bounds, Details and its preserved actions, repository switching, keyboard navigation, loading/error/empty states, conflicts, long names, dark appearance, text/semantic-color contrast over extreme backdrops, live accessibility fallback changes, native padding/filter rules, active work and parked operations. These fixture checks validate the rendered component and action dispatch, not the native tray anchor or IPC transport.

The reference-layout revision passed Svelte/TypeScript checks with zero errors or warnings, 203 desktop and material/harness-contract tests across 19 files, and the 51 WebKit checks. A separately identified **GitPulse Status Preview.app** debug bundle was built and its ad-hoc signature verified, using the same `status.html` production entry point. It does not replace the installed release; manual tray clicks and outside-click dismissal remain a separate check.

The blur revision also passes **27 desktop Rust tests**. `cargo test --manifest-path src-tauri/Cargo.toml --test native_status_material --locked` runs the real production popover constructor on AppKit's main thread and checks the attached `NSVisualEffectView`, `UnderWindowBackground` material, behind-window blending, 18px corners, native width, autoresizing and short-height reopen behavior. It first failed on this 2x Retina screen with `Status icon display is unavailable`; selecting the icon's monitor in physical coordinates fixes that failure. The test requires an interactive macOS display and reports a skip on other platforms. Browser fallback checks activate the production CSS media rule within the test fixture; they do not change the user's OS preferences. The app and native-test Clippy check passed with warnings denied, the native main-thread menu checks passed, and the final debug bundle was rebuilt and ad-hoc signed. Existing vendored-framework and STATIC_VCRUNTIME deprecation warnings remain. Manual tray clicks, outside-click dismissal and mixed-scale physical display transitions were not exercised.

The following results describe the earlier full menu/status implementation, rather than checks rerun for the presentation-only revision.

Verified checks for the popover: the final full frontend suite passed **5,275 tests across 400 files**, with one skipped check for an absent optional promotional document. Svelte/TypeScript reports zero errors or warnings; all **193 IPC handlers** and **51 wire contracts / 910 fields** match. The final focused frontend run passed **169 tests across 16 files**, including the presentation component, stale snapshot protection, subscription recovery, coalescing and accessibility/token contracts. Native checks passed **27 desktop unit tests**, **5 event integration tests** and the main-thread menu checks. The popover-close regression first failed by emitting an app-quit request; it now passes while retaining the main-window guard. A separate attempt to drive MockRuntime's whole macOS event loop crashed with SIGSEGV during setup; it was replaced with direct tests of the production close-routing owner. Native placement tests cover screen edges, negative origins and 1x/2x scale. Rust formatting and Clippy with warnings denied pass; the frontend production build and isolated macOS debug bundle build pass. Existing bundle-size and STATIC_VCRUNTIME deprecation warnings remain.

Physical observation in the separately named debug bundle previously verified the eight application menus, actual enabled/disabled Repository entries and the Settings toggle. Closing the main window and reactivating the app preserved the same process and repository session. The installed copy was not replaced. Physical tray anchoring, outside-click dismissal, macOS Help search, alternate keyboard layouts and Windows/Linux appearance remain unverified. Native geometry and routing tests do not substitute for physical clicks.

## Separate product extensions

The current implementation covers the six agreed enhancement groups: live availability, live selection/labels, repository switching/history, Git actions, utility actions and the status icon. Further independent features include named workspace files, repository initialization, focused-editor Find/Replace/Save menus, customizable shortcuts, navigation history, advanced remote/tag/stash pickers, multiple independent windows, launch at login and provider-backed CI summaries. Those require their own interaction and lifecycle contracts and are not implied by a compact status menu.

Change review: individual tray/event lifecycle impact checks were HIGH and received source and native-test review. Graph results include incomplete callback edges and do not fully represent new untracked files; those files are reviewed directly and included in the applicable checks. DevMap's bounded results were not used to waive tests.

Combined menu/status implementation and documentation: **3,676 lines added / 173 removed**, across **39 modified and 28 new files**. This includes the earlier application-menu work in this task.
