# UI/UX overhaul plan

Goal: one consistent interface, the **Plate** design language ([DESIGN_LANGUAGE.md](DESIGN_LANGUAGE.md)), with every existing feature retained. That means:
- liquid-glass materials, Material-style tone, shape and motion
- fluid compositor-only animation
- a loading screen and sliding panels
- no stray separators
- one copy vocabulary
- a full Appearance customization section (theme import, fonts, color, material, blur, background)

Rules for every agent: [UI_RULES.md](UI_RULES.md).

Status: **planned 2026-11-09, not started.** Wave 0 can begin on approval. Wave 1 and later wait on the preconditions in §6.

## 1. How this plan was made

Four read-only audits ran in parallel on 2026-11-09 against `55d605b8` plus the uncommitted tree:
- visual consistency
- copy and terminology
- information architecture with the theming pipeline
- external research on GPU rendering, Liquid Glass, Material 3 and UX principles

The advisor model then designed the language from that evidence. Counts are `rg` line counts over `src/**/*.svelte`. They are upper bounds and include script and comments; they are not occurrence counts. DevMap was used for structure, but it cannot see CSS classes or Tailwind strings, so it answered none of the visual counts. Labels: **verified** means read or run; **inferred** means a judgment.

## 2. What is wrong today (verified unless marked)

**Visual**
- **Type has no scale.** `text-[9/10/11px]` literals appear on 1,570 lines, against 315 `text-xs`. The utilities disagree with each other: buttons are 12px, chips 11px, pills 10px.
- **Status color is unmanaged.**
  - 889 raw Tailwind palette lines, with "warning" split across amber, yellow and orange.
  - Two competing light-mode mechanisms: a class-attribute remap in `app.css` and 277 `dark:` pairs.
  - `--c-status-*` exists only for the light theme and is used in 10 files.
- **Button dialects.** `gp-btn*` covers about 600 lines in 104 files. Around 10 private scoped button classes exist alongside it, plus link-button recipes in 40+ files. `gp-btn-success` is missing from all four macOS press/reduced-motion selector lists (`app.css:1336,1342,1531,1544`).
- **Separators.** 211 `border-b`/`border-t` lines use 9 different opacity stops. The shared header utility `gp-surface-header` is used in only 2 files.
- **Controls.**
  - `SettingToggle` is used in 7 files, against 82 raw checkbox/switch lines.
  - About half of 65 `<select>`s are unstyled.
  - Segmented controls are hand-rolled in 19 files.
  - Selects are `rounded-lg` beside pill fields.
- **Loading and empty states.**
  - `Skeleton` is used in 8 files; spinners in 48; "Loading…" text in 38.
  - `EmptyState` is used in 19 files; ad-hoc "No …" text in 30+.
  - **No boot screen.** Cold start paints an empty `#app`.
- **Motion.**
  - The terminal dock opens with no animation.
  - The sidebar animates `width`, a layout property.
  - CoachMark and Toast use literal timings.
  - Some modals have no exit transition.
  - Durations range from 100 to 500 ms.
- **Already good, keep:**
  - Tooltips: one singleton upgrades every `title=` (`Tooltip.svelte:14`).
  - Modals: `gp-scrim` + `focusTrap` + `gp-card`.
  - The reduced-motion plumbing.
  - The accent contrast contract.
  - The float-only blur rule and its performance reasoning (`docs/MACOS_APPEARANCE.md`).

**Copy**
- More than 99% of strings are inline. The i18n catalogue is a 12-key pilot.
- One concept has five names: DevMap / devmap / code graph / repo map / Map.
- Manvi is also written MANVI.
- "Harness" is never explained.
- Retry / Refresh / Reload / Rescan / Recheck / Try again are used interchangeably.
- Delete / Discard / Remove / Archive / Clear / Dismiss are mixed, sometimes on one surface (`TaskBoard.svelte:2752–2805`).
- Title Case and sentence case are mixed on the same menus.
- British and American spelling are mixed.
- Internals leak into the UI: "min rung", "ledger records", "generation N", `is_fresh`, "MANVI sidecar", "posture".

**Accessibility bug (verified by probe).** WebKit does not implement `prefers-reduced-transparency`. The probe ran in a bare `WKWebView` on macOS 27.2: both `reduce` and `no-preference` evaluate false, the same as an unknown feature, while the `prefers-contrast`, `prefers-reduced-motion` and `forced-colors` controls evaluate correctly. GitPulse's only handling is `@media (prefers-reduced-transparency: reduce)` at `app.css:1349` and `StatusPopover.svelte:418`, so **the system Reduce Transparency setting is ignored today.** The fix is a native bridge; see Wave 1, lane B.

## 3. Feature inventory: the retain-everything checklist

Every lane checks the surfaces it touches against this list before merge.

- **Shell**
  - Title bar: traffic-light spacer, view tab bar, diagnostics, refresh, checks badge.
  - Repository strip: repository tabs, Fleet and Agents pills, grouping and collapse.
  - Sidebar: branches, commit composer, agent commit menu, worktrees, trust banner, language logo; collapse and resize.
  - Workspace plate with terminal dock (tabs, resize, keep-alive).
  - Status bar: cadence, language segment, modes.
  - Toasts and native/session notification bridges.
  - Welcome card: open, clone, recents.
  - Coach marks, product tour, setup wizard, drag-and-drop overlay.
- **Views (4 × 18 sections)**
  - **Work:** overview, resolve, remote (GitHub), policy (Manvi), tasks.
  - **Code:** explorer, blame, search, map.
  - **History:** graph, diff, reflog, suspects.
  - **Insights:** pulse, coverage, health, secrets, storage.
- **Global surfaces:** repository, Fleet, Tasks, Agents.
- **Dialogs and sheets:**
  - Rebase, Clone, Settings, Shortcuts, Command palette, Branch cleanup, Diagnostics, Prompt, Setup wizard, Product tour.
  - Inferred from file names: Task action, Task merge, Merge branch, Task handoff, Quick enhance.
- **Command palette:**
  - Modes: commands, files, repositories, commits, branches, symbols, workspace, help.
  - Every view and section.
  - Git actions: fetch, pull, pull-rebase, push, rebase, stash/pop, branch new/rename/clean, quick commit.
  - Repository-tab operations, theme, zoom, avatars, Model Context Protocol (MCP) and tool setup.
- **Settings:** appearance, layout, views, graph, diff, analysis, agents, hygiene, updates. Each setting is declared in `settingsCatalog.ts` and pinned against the DOM by `SettingsModal.test.ts`.

Sub-panels inside each section were not enumerated. A lane that touches a section lists that section's sub-panels in its report before and after the change.

## 4. Smoothness: what the research says, and what we will do

- **No toolkit rewrite.** GPU-native toolkits were assessed and rejected for this codebase:
  - GPUI is pre-1.0 with breaking changes.
  - Blitz is beta; Xilem/Vello are experimental; Slint is mature.
  - None of them replaces xterm.js, Markdown rendering, IME and the accessibility tree.
  - The largest smoothness gain for the least risk is **compositor discipline inside the webview we already have.**
- **Compositor discipline**, which is the core of the design language:
  - Animate only `transform` and `opacity`.
  - Svelte transitions use the `css` form, which runs on the Web Animations API off the main thread.
  - The sidebar and dock slide by transform as overlays, instead of animating `width`.
  - Blur is limited to the float tier, at a bounded radius.
  - No ancestor of a float may create a backdrop root.
  - Springs are baked into `linear()` easing curves. WebKit has supported `linear()`, `@starting-style` and `allow-discrete` since Safari 17.4–17.5.
- **Native glass is free.** The window's desktop blur is `NSVisualEffectView`, which the window server composites at no per-frame cost to the app.
  - macOS 26+ adds `NSGlassEffectView`, exposed by `tauri-apps/window-vibrancy` `apply_liquid_glass`. It is public AppKit API (in the macOS 26 SDK headers), not private; the app already builds with Tauri's `macos-private-api` for the transparent window. Built in Wave 4 lane G; see docs/MACOS_APPEARANCE.md.
  - This becomes an off-by-default flag, decided only after a frame-time measurement (Wave 4).
- **Frame rate (measured by Wave 0 lane M, `harness/frame-rate.html`, 2026-11-09).** The "uncapped on macOS 26+" claim does not hold here.
  - **Capped at 60 Hz [verified].** On macOS 27.2 with a 120 Hz built-in display, WKWebView delivers a 60.0 Hz mean `requestAnimationFrame` cadence. That holds in the harness runner and in a visible, opaque, frontmost window.
  - **The cap is one WebKit feature flag [verified].** It is `PreferPageRenderingUpdatesNear60FPSEnabled`, on by default and listed in `WKPreferences._features`. Turning it off through the private `_setEnabled:forFeature:` gave a 120 Hz mean in two runs. No display-link workaround or `tauri-plugin-macos-fps` is needed.
  - **Not yet known [unverified].** These runs were not inside the Tauri app, and nobody has checked whether wry exposes the WKWebView configuration early enough to set the flag. This is decision 4 in §5.
  - **Float blur costs no frames [verified].** The `blur` phase (34px plus saturate over a moving panel) matched `transform` at both 60 Hz and 120 Hz, with zero long frames.
  - **`width` animation does cost frames [verified, in WebKit].** It lost 7–10 frames per 3 s at 60 Hz, with p99 48–66 ms, and did worse when uncapped. `transform` and `opacity` held the full rate. The fixture's 400-row flex relayout sets the size of the loss; the direction is general. This is direct evidence for decision 1's overlay slide.

## 5. Decisions you make

These four change what gets built. Each has a recommendation.

1. **Sidebar and terminal dock default:**
   - **Overlay slide** (transform-only and smoothest; the panel floats over content and blurs).
   - **Docked reflow** (today's layout, with a one-shot 200 ms width animation).
   - *Recommended:* overlay for the toggle and shortcut, docked while drag-resizing. Both stay selectable in Layout.
2. **Wallpaper or background image in v1:**
   - Ship now with the veil forced to ≥ 0.7.
   - Or defer until contrast over arbitrary images is measured.
   - *Recommended:* defer. The recolorable hue field (hue + intensity, off / subtle / vivid) delivers the same feel at zero contrast risk.
3. **Native Liquid Glass (macOS 26+, public AppKit `NSGlassEffectView`):**
   - Build it behind a flag.
   - Or stay on `underWindowBackground` only.
   - *Recommended:* build the flag, ship it off, and decide after the Wave 4 measurement.
4. **120 Hz rendering (private WebKit flag, added after the Wave 0 measurement):**
   - Turn off `PreferPageRenderingUpdatesNear60FPSEnabled` on GitPulse's WKWebView so ProMotion displays get 120 Hz.
   - Or stay at 60 Hz.
   - *Costs:* it is private API that a WebKit update can rename or remove. It roughly doubles frame work during animation. It widens the cost of any layout-animating code, since `width` lost more frames when uncapped.
   - *Recommended:* build it as an off-by-default setting next to the Liquid Glass flag. Do this only after a spike confirms wry lets us set it, and after Wave 2 has removed the layout animations. Fail closed: if the selector is missing, stay at 60 Hz and say so in Diagnostics.

Also settled by recommendation, and overridable:
- US spelling.
- Sentence case.
- "code map" as the user-facing name for DevMap output.
- Selects become pills.
- The type scale is 10 / 11 / 12 / 13 / 15 / 18.
- Three densities: compact, comfortable, spacious.

## 6. Preconditions and coordination (read before launching any lane)

Taken from `gitpulse_insights` on 2026-11-09:

- **The main checkout is mid-refactor.** 65 uncommitted files belong to another live session rebuilding the view tab bar:
  - `ViewSectionBar` and `HeaderRepoMenu` are deleted; `ViewSectionTabs` and `ViewContextBar` are new.
  - The touched files include `app.css`, `App.svelte`, `ViewTabBar`, `SettingsModal`, `interfaceStore`, `settingsCatalog` and `mac-material-contract.test.ts`.
  - These are the overhaul's own files. **Wave 1+ does not start until that refactor is committed.** The commit it lands as is the overhaul's **base SHA**, recorded here when known: `BASE_SHA = 2c72b869`.
  - The refactor landed on 2026-11-10 as merge 8c6d3d03 (`ui/streamlined-nav`). Four follow-ups came after it, including 693c5849, which retires header icons. `BASE_SHA` is the tip after those follow-ups, not the merge itself.
  - Line numbers in the audits and the language spec are against that dirty tree and will drift.
- **Another feature is still uncommitted in the main checkout.** As of `BASE_SHA`, 26 dirty files belong to a different feature: `FileTreeColumn`, `statusPill` and `shortcutGuide`.
  - It edits `WorkView`, `StatusBar`, `TerminalDock`, `RepoTabBar`, `ShortcutsModal`, `FileViewer`, `RepoMapPanel` and `WorkspaceActions`.
  - None of those are Wave 1 files, but all of them are Wave 2 lane files. Before launching Wave 2, check whether that work has landed. If it has, move the wave branch onto it, the same way Wave 1 waited for the refactor.
- **Sibling worktrees:**
  - `browser-m0` (`browser/terminal-session`, 3 ahead, 24 dirty) holds `interfaceStore.ts`, `TerminalPanel.svelte`, `src-tauri/src/lib.rs` and `capabilities/default.json`.
  - `manvi-mode` (`feat/manvi-mode`, 26 dirty) holds `src-tauri/src/lib.rs` and `capabilities/default.json`.
  - Wave 3, lane D (the dock slide) and anything that registers a Tauri command (Wave 1, lane B) must wait for those branches or coordinate through `gitpulse_worktree_send_message` before editing them.
- **Lint baselines are taken at `BASE_SHA`,** never against a dirty tree. Another session's half-finished work must not become the floor.
- **Lanes run in their own worktrees** (`gitpulse_worktree_create` or Agent `isolation: "worktree"`), claim their files with `gitpulse_worktree_claim`, and never edit the main checkout.
- **Disk.** Frontend lanes need no Rust build. A lane that builds Rust (Wave 1, lane B) uses its own target directory, about 20 GB, and cleans it on finish. In a past run, four lanes with private targets filled the disk.
- **New worktrees.** A symlinked `node_modules` is untracked, and `git add -A` stages it. Stage by path.

## 7. Waves and lanes

- **Model routing:** Sonnet agents implement each lane; Opus orchestrates, reviews every diff and runs the merge verification; Fable advises on design questions a lane raises.
- **Partitioning:** lanes are split **by file ownership, not by concern.** No two lanes in a wave touch the same file. In Wave 2, each lane applies every rule migration to the files it owns, so a file is opened once.

### Wave 0: additive foundations

Wave 0 creates new files only and touches no dirty path, so it can start now from `55d605b8`.

| Lane | Delivers (new files) | Tests |
|---|---|---|
| **T** tokens-as-code | `src/lib/motion/tokens.ts` (durations, easing names); `scripts/bake-springs.mjs`, which bakes M3 spring parameters into `linear()` curves at ≤ 1% error; the generated output committed | Unit test that the baked curves settle; parity with the `transitions.ts` constants (out < in) |
| **C** color engine | `src/lib/ui/color/oklch.ts`, `guard.ts` (`ensureContrast`), `derive.ts` (three-input base/accent/contrast → `--c-*` triplets that reproduce the shipped ladder at contrast 0.5), `src/lib/ui/appearance/themeSchema.ts` (`gitpulse-theme/1` validator) | Guard thresholds shared with `accents.contract.test.ts`; derive reproduces the shipped dark and light ladders exactly; the validator rejects CSS strings, `url()`, filters and z-index |
| **L** design lint | `scripts/design-lint.mjs` covering the 15 rules in UI_RULES §1, a `--baseline` mode writing `scripts/design-lint.baseline.json`, and a `design:lint` script in `package.json` (reported only, not yet in `ci:local`) | Fixture tests per rule; the baseline is taken at `BASE_SHA` when known |
| **V** vocabulary | `src/lib/ui/copy/vocabulary.ts` (glossary, banned terms, verb map), `src/lib/ui/copy/describeFailure.ts` wrapping `formatError` | Unit tests; the banned-term list feeds lint rule 14 |
| **M** measurement | A harness page `harness/frame-rate.html` measuring rAF cadence, plus a scratch report of the rate in GitPulse's real window on this Mac (macOS 27.2) | Report only |

Wave 0 exits when all five are merged to main and the full suite is green.

### Wave 1: the foundation in `app.css` (after `BASE_SHA`)

| Lane | Owns | Delivers |
|---|---|---|
| **A** foundation (one lane, serial; it is the only writer of `app.css`, `layers.ts` and `transitions.ts`) | `src/app.css`, `src/lib/ui/layers.ts`, `src/lib/ui/transitions.ts`, `scripts/mac-material-contract.test.ts`, `src/lib/ui/themeContrast.test.ts`, `src/lib/ui/macAppearance.test.ts` | `@theme` additions: radius, space, density, motion vars; `text-micro`/`text-meta`; status roles for **both** themes; `text-faint`; `--c-on-accent`; three edge stops. Utilities: `gp-btn` with `data-variant`/`data-size` (the `gp-btn-*` classes kept as aliases until Wave 2 empties them), `gp-chip`, `gp-header`, `gp-row`, `gp-label`, `gp-select` as a pill, `gp-slide-docked`. LAYERS: `TOAST 65`, `STICKY 10`, `SCROLL_CUE 20`. Every contract test that pins these is updated in the same commit, never deleted. |
| **B** transparency bridge | new `src-tauri/src/desktop/accessibility.rs`, new `src/lib/ui/transparencyPreference.ts`, `src/lib/desktop/StatusPopover.svelte` | Native read and observer for Reduce Transparency, an event that stamps `html[data-transparency="reduced"]`, and the paired test. Lane A adds the matching selector to the `app.css` block. Registering the event touches `src-tauri/src/lib.rs`, which `browser-m0` and `manvi-mode` also hold, so lane B coordinates or waits. Check the IPC doc-count sites (a new command trips about 5 count guards across 6 files). |

**Wave 1 status (2026-11-10):** integrated on `overhaul/wave-1` at 51888211.
- The tip is merge d30c37c6 (lane A), stacked on lane B's merge and Wave 0's merge, with the baseline re-stamped on top.
- Rust tests: 9087/9087 pass. Coverage floors hold.
- Harnesses: 39/39 pass in Chromium and in WebKit.
- Design-lint rule 15 is at 0.

**Wave 1.5, lane P (shared primitives).** Some primitives that §2 of UI_RULES tells Wave 2 to use did not exist at the end of Wave 1:
- `Toggle.svelte`, `Dialog.svelte` and `Slider.svelte`;
- the `gp-notice` and `gp-field-multi` styles.

Lane P builds these before any Wave 2 lane starts, so that no lane hand-rolls its own. `SettingToggle` becomes a thin wrapper over `Toggle`, with the same API.

### Wave 2: migrate every component (parallel, partitioned by files)

Each lane applies, in its own files:
- the type scale
- status roles
- one button grammar
- `gp-header` instead of `border-b`
- edge stops
- `Toggle`/`gp-select`/`gp-segmented`
- `Skeleton`/`EmptyState`/`gp-notice`
- LAYERS
- transitions with outros
- the **copy pass** (sentence case, glossary, verbs, `describeFailure`, `…`)

| Lane | Files (and their tests) |
|---|---|
| **W1** Tasks | `TaskBoard`, `Task*`, `RepositoryTasks`, `AttentionInbox`, `WorkspaceEditor`, `WorkspaceActions`, `QuickEnhanceSheet`, `AutomaticEnhancements`, `ScanProposals`, `TuningApply`, `LabelInput` |
| **W2** Agents and Manvi | `AgentsView`, `AgentInbox`, `AgentDecisions`, `AgentIntegrationPanel`, `AgentCommitMenu`, `AttemptReview`, `Manvi*` (except `ManviClaudeModel`), `RunActivity`, `CardVerdict`, `VerificationChip`, `SendToAgent`, `HarnessBadge`, `HookRunNotice`, `DevcouncilSuitePanel` |
| **W3** History | `HistoryView`, `Commit*`, `DiffViewer`, `DiffFileRail`, `DiffHistoryGraph`, `ImageDiffViewer`, `BlameViewer`, `ReflogViewer`, `RegressionSuspectsPanel`, `GraphNotices`, `GraphNodeTooltip`, `GraphClipNotice`, `ConflictEditor`, `ConflictComparison`, `RebaseModal`, `MergeBranchDialog`, `ChurnBar`, `FreshnessBadge` |
| **W4** Code | `CodeView`, `files/*`, `FileViewer`, `ContentSearch`, `CodeGraphCanvas`, `CodeStackViewer`, `RepoMapPanel`, `BlastRadiusPanel`, `RungFilterControl`, `MarkdownBody`, `MarkdownContent`, `LanguageLogo`, `LanguageSegment` |
| **W5** Insights | `InsightsView`, `pulse/*`, `health/*`, `HealthPanel`, `CoverageViewer`, `Coverage*`, `SecretsPanel`, `StoragePanel`, `Fleet*`, `DeliveryTimeline` |
| **W6** Work and remote | `WorkView`, `WorkspaceView`, `GitHubPanel`, `github/*`, `FirebasePanel`, `WorktreesPanel`, `BranchList`, `BranchHealthDot`, `BranchCleanupModal`, `HygienePanel`, `GlobalCleaner`, `OperationBanner`, `WavePanel`, `RepoPanel`, `RepoPaneRecovery`, `TrustExtensionBanner`, `IdentityPrompt` |
| **W7** Shell, dialogs, shared primitives | `src/App.svelte`, `Sidebar`, `StatusBar`, `RepoTabBar`, `ViewTabBar`, `ViewSectionTabs`, `ViewContextBar`, `ViewSectionPanel`, `ViewGuideCard`, `ViewGuideArt`, `CommandPalette`, `ShortcutsModal`, `DiagnosticsModal`, `PromptModal`, `CloneModal`, `ToastContainer`, `CoachMark`, `EmptyState`, `Skeleton`, `ScrollCue`, `Tooltip`, `VirtualList`, `LazyMount`, `LazyView`, `FilterBar`, `Logo`, `NativeNotificationBridge`, `SessionNotificationBridge`, `onboarding/*` |

**Wave 2 status (2026-11-10):**
- **Integrated:** `overhaul/wave-2` is at b376ae4a. It holds P2, W2, W5, W3 and W1, with the baseline re-stamped.
- **No more deferral.** The user said not to defer anything, so W4, W6, W7 and Wave 3 lane S started at b376ae4a without waiting for the main checkout's uncommitted FileTreeColumn/statusPill work. When that work lands, expect conflicts in `WorkView`, `StatusBar`, `RepoTabBar`, `ShortcutsModal`, `FileViewer`, `RepoMapPanel`, `WorkspaceActions` and `TerminalDock`. Lanes keep those files to class, token and copy swaps to keep the conflicts small.
- **Ownership closed at b376ae4a.** Every `.svelte` file is in a lane, and every remaining design-lint hit has an owner:
  - `BlameViewer`, `fileStatus.ts` and `syntaxHighlight.ts` → W4.
  - `WorkspaceActions`, `deadbranch.ts`, `rolloutState.ts`, `runLifecycle.ts` and `runActions.ts` → W6.
  - `StatusPopover` and `BrowserSession` → W7.
  - `TerminalPanel`, `TerminalSession` and `TerminalDock` (151 hits) → Wave 3 lane D. Its `browser-m0` blocker is gone: that branch landed in 2c72b869.
  - D and I start from the tip after W7 merges, because they share `Sidebar`, `LazyView`, `ViewTabBar` and `RepoTabBar` with W7.
- **Another session's branch already holds part of the overhaul.** A consolidation session is collecting branches on `feat/policy-resolve`. That branch already contains `overhaul/wave-1` and `8d6251d8`.
  - A trial merge of b376ae4a onto it (`git merge-tree`, 2026-11-10 12:48) conflicts in 3 files: `harness/branches.html`, `src/lib/components/TaskManviAssist.svelte` (the manvi-mode work) and `src/lib/ui/formatError.sweep.test.ts`.
  - This overhaul does not merge that branch, and does not merge into it.

**Handoffs between lanes, recorded as they come up:**
- **Deferred to a late pass (2026-11-10):** `WorkspaceActions` (from W1) and `BlameViewer` (from W3). Another session has uncommitted edits to them in the main checkout. W1 and W3 started without them, and they will be done after that work lands. W4, W6 and W7 have too many of those files to start early.
- **W3 → Wave 3 lane D.** `CommitTable`'s loading strip still uses `animate-[gp-slide_…]`. This was true before the overhaul.
  - `gp-slide` isn't in either reduced-motion list, and design-lint can't see arbitrary-value animations.
  - Lane D fixes this together with the Skeleton timing work. The Skeleton has no 300 ms show delay or 400 ms hold yet; W3 and W5 both reported that.
- **W3 → follow-up.** `CommitDetails` still hand-writes its "Couldn't …" headline around `formatError`.
  - `formatError.sweep.test.ts` now accepts `describeFailure` as a seam, added in W3's 177b04a5.
  - So `CommitDetails` can now move to `describeFailure`.
- **W1 → W7 (shared primitives).** `Checkbox.svelte` has no `data-testid` pass-through. Because `tasksChecks.js` and `proposalsUi.test.ts` select on one, the three part checkboxes in `TaskRefine` stay native `gp-check` inputs. Adding the pass-through lets them move.
- **W1 → follow-up (copy).** Some strings outside W1's files still use the old wording:
  - `taskTerminal.ts:542` still says "Start or recover managed launch" and "attempt", but the button now says "run".
  - `taskHandoff.ts` and `taskSessions.ts` say "Failed to start".
  - `sessionContext.ts` says "attempt".
  - `SWIMLANE_LABELS` say "lane".
  - `TaskBoard`'s `explainError` read-notes and `TaskRefine`'s composed messages don't go through `describeFailure` yet.
- **W1 → manvi-mode.** The uncommitted work in the `manvi-mode` worktree (`feat/manvi-mode`) edits `TaskManviAssist.svelte`, which W1 also changed. Expect a conflict when either one merges.
- **Wave 2 merge (b376ae4a): a pre-existing WebKit flake → W7.** During the full WebKit run, `tab-switch` failed "switcher: leaving the pill closes the flyout after its grace". It passed when rerun alone (109/109).
  - The touch-flyout failure right after it is a knock-on effect: the flyout was still open from the previous check.
  - `ViewTabBar.svelte` is unchanged since 2c72b869. The flyout closes on a 220 ms `setTimeout` with no exit transition. The check waits up to 1000 ms.
  - So the timer fired more than 780 ms late, which fits WebKit throttling timers in a window that isn't focused (inferred).
  - W7 owns `ViewTabBar`. It should make the check wait on the close condition rather than wall time, or prove the timer clamp. The log is at `tmp/overhaul/wave2-merge-webkit.log`.
- **W1, unverified.** `harness/workbench.html` got 4 assertion edits, but it isn't a registered harness, so nothing ran them.
- **W4 → merge (copy pin).** `RepoMapPanel` keeps "Could not check links." because `harness/diagnostics.html:311` pins it. Change both together at the wave-2 merge, to "Couldn't check links." with a reason.
- **W4 → lane D (loading states).** `BlameViewer`, `CodeStackViewer` and `LivePulseDashboard` still use text or pulse cards for loading. They move to `Skeleton` with lane D's timing (300 ms delay, 400 ms hold).
- **W4 allowed exceptions.**
  - `MarkDevLogo.svelte`: 9 rule-4 brand-art colour literals, with no token to map them to.
  - `syntaxHighlight.ts`: 11 rule-3 palette classes. `app.css` remaps exactly these classes for light-theme contrast, so they move only together with the Wave 4 retirement of the light-theme remap. Add both files to the design-lint allow-list at that point.
- **W7 → Wave 4 (StatusPopover palette).** `StatusPopover.svelte` keeps 96 hits: 65 rule-4 hex literals, 29 rule-12 scoped `.chip`/`.primary` classes and 2 rule-11. It is the native menu-bar popover, with its own palette tuned for contrast over any wallpaper and pinned by `harness/statusChecks.ts`. Retune it onto tokens only with a measured contrast check, in Wave 4.
- **W7 allowed exceptions.** `Logo.svelte` keeps 8 brand-art hex stops, as `MarkDevLogo` does. `Sidebar.svelte` keeps `transition-[width]` until lane D's slide replaces it.
- **W7 → lane I and the copy follow-up.** Title-case labels remain: "Clone Repository…" in the palette catalog, `RepoTabBar` and `ShortcutsModal`. `harness/repo-tabs.html` pins the text, so lane I changes both together.
- **W7, unverified visually.** Dropping the `border-r` hairline leaves the sidebar's right edge with no line wherever `gp-section-edge` is absent. Check it in the integration screenshots: the tonal step must still separate the sidebar from the workspace.
- **W7 tab-switch flake.** The flyout-close check now waits up to 5 s for "closed" (it was 1 s), because an unfocused WebKit window clamps timers. It still fails a flyout that never closes, but no longer catches one that closes slowly; the 220 ms grace is pinned only by `ViewTabBar`'s constant.
- **Host load (2026-11-10).** The 1-minute load average sat at 160–190, mostly from other sessions' profiling binaries and Go tests. Lane harnesses time out at the 60 s cold start under that load. So the suite and `:all` runs are classified once, on the integrated tree, against b376ae4a, rather than per lane.
- **W5 → W6.** `DeliveryTimeline` gained an optional `onRescan` prop (in `cd049338` on `overhaul/wave-2-w5`). Its only parents are `GitHubPanel` and `FirebasePanel`, which W6 owns. W6 should pass each panel's existing run-listing refresh into that prop.
- **W5 → W7 or Wave 4.** The `gp-field` style still has no invalid or disabled state. Only `gp-field-multi` has them.
- **Open design question: multi-select lists.** W2 and W5 followed UI_RULES §2 ("On/off → `Toggle`") and turned row-selection checkboxes into switches:
  - branch and tag cleanup in `ManviOpsPanel`;
  - the coverage focus in `CoverageAgentPrompt`.

  A switch announces "on/off". Choosing items from a list is a checkbox job.
  - **Decided 2026-11-10:** add `Checkbox.svelte` (lane P2), and add a UI_RULES §2 row that separates "pick items or confirm a statement" from "on/off".
  - P2 moves the four sites W2 and W5 converted back onto `Checkbox`:
    - the ManviOpsPanel branch and tag cleanup;
    - the ManviOpsPanel release confirmation;
    - the CoverageAgentPrompt focus.

These are left to Wave 3:
- **Lane S:** `SettingsModal` and every settings component (`Setting*`, `*Setting`, `*Settings`, `AgentDefaultsSettings`, `AgentLimitSetting`, `AgentModelSettings`, `AgentSettingSourcesSetting`, `Lappi*`, `ManviClaudeModel`, `HygieneDefaultsPanel`, `ExternalToolsPanel`).
- **Lane D:** the terminal files (`TerminalDock`, `TerminalPanel`, `TerminalSession`), after `browser-m0`.

The lane list is a starting partition. Before launch, `rg --files src/lib/components` is diffed against the union of the lanes so no file is unowned or owned twice; the 279-plus files there include tests. Wave 2 exits when the design-lint counts for rules 1–8 and 12–14 reach zero outside the allow-list, and the full suite plus every harness passes in Chromium and WebKit.

### Wave 3: experience

| Lane | Owns | Delivers |
|---|---|---|
| **S** Settings → Appearance | `SettingsModal.svelte`, `settingsCatalog.ts`, `settingsSections.ts`, `Setting*.svelte`, the new `src/lib/ui/appearance/apply.ts`, `interfaceStore.ts` (after `browser-m0`), `densityStore.ts`, density typing in `src/lib/canvas/GraphRenderer.ts` | Appearance is regrouped into Theme, Colors, Type, Shape, Material, Motion and Density. The new settings: <br>• theme import/export (`gitpulse-theme/1`) <br>• accent preset + custom color through the guard <br>• UI and code font (local families only) <br>• density merge <br>• corner style <br>• material intensity <br>• float blur 0–40 px (0 → `none`) <br>• background field off/subtle/vivid + hue <br>• transparency auto/reduced <br>On Windows and Linux, the macOS-only material settings are hidden by a backend capability. |
| **D** motion and panels | `Sidebar`, `TerminalDock`, `TerminalPanel` (after `browser-m0`), `LazyView`, `index.html`, new `Splash.svelte`, `public/theme-boot.js` | Overlay slide for the sidebar and dock, with docked one-shot as the alternative; the boot splash in inline critical CSS plus a 480 ms hand-off; skeleton timing (300 ms delay, 400 ms hold); view and section choreography on the motion tokens. Watch the hidden-pane ResizeObserver class: VirtualList renders only its overscan band when its pane is hidden. |
| **I** header and IA | `App.svelte` header block, `RepoTabBar`, `ViewTabBar`, `ViewSectionTabs`, `ViewContextBar`, `WorkspaceActions` placement, `HarnessBadge`, `StatusBar` landmark, `src/lib/desktop/windowState.ts` (full-screen stamp), `harness/tab-switch.html`, `harness/repo-tabs.html`, `scripts/mac-material-contract.test.ts`. `ScrollCue` (the `diffuse` edge). Starts from the post-W7 tip, after lane D (it shares `Sidebar` for D3), and after the main checkout's in-flight status-bar terminal pill (`terminal/statusPill.ts`, `StatusBar.svelte`) is committed, because lane I moves that pill to row 2 | The header redesign in [HEADER_SPEC.md](HEADER_SPEC.md) (2026-11-10), which **supersedes** the earlier "Fleet and Agents move to the view strip": repository tabs and global surfaces in the title bar, views in a second row, full-screen inset reclaimed, container-query breakpoints, overflow menu. Also: section labels at ≥ 1100 px, and a collapsed sidebar becomes a 44 px icon rail. D1–D3 and E1–E6 approved 2026-11-10; lane I also owns `src-tauri/tauri.conf.json` (`tabbingIdentifier` only). |

### Wave 4: enforcement and native

- `design:lint` moves into `ci:local` as an error, with the baseline at zero for the migrated rules.
- Retire the light-theme class remap block and the remaining `dark:` color pairs. This goes last, and `themeContrast.test.ts` re-derives its cases.
- The native Liquid Glass flag, built with `apply_liquid_glass` behind an off-by-default setting. Measure shell-idle and menu-open frame time with and without it on macOS 26+, then decide.
- Re-run the four audits with the same prompts and compare the numbers against §2.

**Wave 3 status (2026-11-10, evening).** The user said to work on everything and leave no gaps, so nothing waits on the main checkout any more.
- **Base.** `overhaul/wave-3-base` starts from the `overhaul/wave-2` tip cf0b0b7e and adds:
  - W1b and S, merged;
  - a snapshot commit of the owner's uncommitted terminal pill, shortcut guide and header edits, frozen from main at 16:15;
  - the design docs, committed.
- **Main's files are untouched.** When the owner commits that work, run `git merge-tree` on that commit against the overhaul tip before anything lands on main.
- **Not in the snapshot.** Main's `TerminalDock`, tasks harness, `BranchList`, `CommitComposer` and proposals edits were still changing at 15:47, so expect conflicts there.
- **Lanes D and I ran in parallel.** D3 moved to lane D, and lane F took every outstanding handoff.

**Decided 2026-11-10 (user): E4.** ⌘1–9 switch repository tabs, and ⌘9 selects the last tab.
- The views move off ⌘1–3, to ⌥⌘1–3 on macOS and Ctrl+Shift+1–3 on Windows and Linux. That avoids AltGr and the macOS screenshot chords.
- Wave 4 lane K implemented this.

**Decided 2026-11-10 (user): Open pinned right.** The "+ Open ▾" split pill is the first control of the right-hand cluster, after the window-drag zone and before Fetch all, so it no longer moves as tabs open and close.
- The drag zone keeps its 48 px floor (E3).
- The recents menu aligns to the pill's end edge.
- Wave 4 lane O implemented this, and it is recorded as HEADER_SPEC decision D4.

**Decided 2026-11-10 (user): three-line branch rows.** The sidebar branch row has three lines:
1. name and chips;
2. churn, divergence, files and freshness;
3. last-commit age (tinted when stale) and author.

`three-line` is the default, and a stored `two-line` migrates to it once. Two-line and one-line stay selectable. Measured heights are 62 px spacious and 56 px compact. Wave 4 lane T implemented this.

**Decided 2026-11-11 (orchestrator, not yet confirmed by the user).**
- **Docked sidebar.** It stays the default and now slides on transform and opacity, reflowing the workspace once rather than every frame (Wave 5 lane B). The alternative was to make overlay the default.
- **Surface names are proper nouns:** Settings, Tasks, Fleet, Agents, Work, Code, History and Insights.
- **Case.** Command-palette labels use sentence case. Native macOS menus keep title case (Apple HIG). The two may differ in case, never in wording, and a test enforces that.
- **Metric numerals** use a new 24 px `--text-display` step.
- **`--text-xs` line height** stays 1.333, not 1.45, so 315 uses don't reflow.

### Wave 5: closing every gap the final audits found

The two final audits (`tmp/overhaul/audit-final/`) found about 30 gaps at the Wave 4 tip. Nothing was worse than at the start, but the design-lint read only `.svelte` markup, so it reported zero while gaps remained.

**Lint first.** Lane L2 widened the lint before anyone fixed anything:
- It now reads `app.css`, `<style>` blocks and `.ts` user strings.
- It found a walker that had skipped every `coverage/` directory.
- It added rules 17 (an undefined `gp-*` class) and 18 (an unused one).

**Fixing lanes, each owning its own files:**
- **A (shell and styles):** the type tokens and the display step; rule 1 also reads rem/em/% and the `font:` shorthand; the "3 more" tint box; the dock exit; the boot colour contract; the drop overlay.
- **B (sidebar):** the transform-only docked slide, the rail clip, the scroll fade and the light floating edge.
- **C (copy):** glossary copy across the `.ts` files.
- **E (settings and dialogs):** the "Header button labels" setting restored, the scroll-edge fade on dialogs, the dialog exits, and a lazy notification bridge (−2,688 B).
- **F (panels):** selects, checkboxes, skeletons and empty states.
- **S (task surfaces):** type scale and sweep.
- **Z:** no idle render loop; a capture artifact, plus idle-stability checks.

**Residue lanes and the final pass:**
- **R1, R2, R3 and the final lane** cleared the remaining 201 off-scale sizes, the remaining copy, and "could not", which is now retired by rule 14.
- **The code map fit.** The never-registered `devmap` harness, once registered, exposed a real code-map bug: the first fit ignored the toolbar growing, leaving the graph 28 px off-centre.
- **Exits.** Every placed popover now leaves through one shared 70 ms exit.

**Final tip:** `overhaul/wave-5-final`.
- 0 deferred lint entries, with 6 reasoned `allow` entries.
- 909/909 test files and 13,687 tests pass.
- The entry chunk is 634,421 B against the 640,000 budget.

**Second consistency audit (2026-11-11).** A live sweep of 731 controls on 42 harness pages found five classes; each was fixed at its owner and given a lint guard:
- **A. Corner style missed most corners.** Tailwind's `--radius-*` were never mapped onto `--gp-r-*`, so ~260 `rounded-*` uses, `gp-menu` and `gp-card` ignored Appearance → Corner style. Mapped in `@theme inline`; `rounded-md` (6px, off the ladder) is now 8px.
- **B. Scoped CSS beat the primitives.** The `gp-*` primitives are layered utilities and component CSS is not, so any scoped rule setting padding, height, border or radius on a primitive won outright. The primitives gained `gp-btn data-size="inline"` and `data-size="sm"` on `gp-field`/`gp-select`; rule 12 flags the override.
- **C. Heights.** *Orchestrator decision, not yet confirmed by the owner:* an unmarked `gp-btn`, `gp-icon-btn`, `gp-field` and `gp-select` is the md size, 28px (it was an accidental 26px); `data-size="sm"` is 24px; every standalone `gp-chip` is 20px, and a chip set inside a line of text wears `data-size="inline"` and takes that line's height (the branch-row tags, which fix C had grown and the branches harness caught); `gp-seg-btn` is at least 24px. The 80 forced `py-*!`/`p-*!`/`h-*` overrides became `data-size`. Flipping the default to 24px is one line in each utility plus this note.
- **D.** The last five hand-styled selects and eight raw checkboxes use `gp-select` and `Checkbox`; rule 12 flags both.
- **E.** 48 literal `border-radius` values in `<style>` and `app.css` moved onto the ladder; rule 5 now reads them.

## 8. Lane brief template (Sonnet)

Each lane gets this brief, filled in:

> You are lane `<ID>` of the GitPulse UI overhaul. Work only in your worktree `<path>`, branched from `<BASE_SHA>`.
> **You own exactly these files:** `<list>`. Edit nothing else; if you need to, stop and report.
> Read first: `docs/design/UI_RULES.md` (binding), the sections of `docs/design/DESIGN_LANGUAGE.md` named here: `<§>`, and `docs/MACOS_APPEARANCE.md` if you touch materials.
> Navigate with DevMap first (`devmap_search` / `devmap_explore` / `devmap_impact` / `devmap_affected_tests`, absolute `repo_path`, and check `repository.root`, `truncated` and `walk_incomplete`). Run `gitpulse_insights` at the start and `gitpulse_collision_risk` before each file, then claim your files. DevMap cannot see CSS or Tailwind strings: use `rg` for those and say so.
> Before you change a surface, list its features and sub-panels. After, prove each still exists (test or harness).
> Done means:
> - the full `bun run test` passes
> - `bun run check` passes
> - `node scripts/design-lint.mjs` shows no new violations in your files
> - the harnesses for your surfaces pass in Chromium and WebKit (`npm --prefix <wt> run test:browser|test:webkit -- --harness <name>`)
> - screenshots of both themes are saved under `tmp/overhaul/<ID>/`
>
> Update pinned contract expectations in the same commit; never delete or weaken a test. Commit on your branch with path-scoped staging and do not push. Report what you verified, what you did not, and every file you touched.

## 9. Merge protocol

1. Opus reviews each lane's diff against UI_RULES and the feature inventory. It runs `devmap_impact` / `devmap_affected_tests` on the changed symbols.
2. A lane merges into an integration branch (`overhaul/wave-N`), never straight to main.
3. Before a wave lands on main:
   - Run the **full** suite on the integration branch: `bun run test`, `bun run check`, `bun run ci:local` and the browser and WebKit harnesses. Targeted tests have missed regressions before.
   - Check the main reflog first, in case another session has moved main.
   - Build from a pinned SHA.
4. The installed app is rebuilt by the user. Agents do not write to `/Applications`.

## 10. Risks

- **Light theme over a dark desktop is unmeasured**, so the material intensity floor is only known safe over white. Lane S measures it before shipping the slider.
- **Overlay panels change layout behaviour.** A hidden pane under an overlay can stop its ResizeObserver from firing. Lane D adds a harness for it.
- **Density becomes a CSS variable** and reaches the canvas graph renderer and the task view types.
- **About 17 contract tests pin `app.css` text.** Every wave pays this cost on purpose: update, never delete.
- **Copy changes break tests that assert strings.** They are expected; each one is reviewed, not bulk-updated.
- **Private APIs** (Liquid Glass, the 120 Hz unlock) are opt-in only, with a fallback.
