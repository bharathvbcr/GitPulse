# Header redesign: repository tabs above views (lane I)

Status: **final, approved by the user 2026-11-10** (D1–D3 and E1–E6 as proposed). Design consult by Fable; source claims spot-checked by the orchestrator (marked). Amends DESIGN_LANGUAGE §12 and OVERHAUL_PLAN §7 lane I, whose "move Fleet and Agents to the view strip" assumed views stay on top.

## 1. Why the rows swap

- `globalSurface` is single-select (`repository | fleet | tasks | agents`), and clicking any view forces it back to `repository` (App.svelte, ViewTabBar.svelte:69). Fleet, Agents and Tasks are therefore peers of a repository tab, not of a view. Views only mean something inside a repository. (verified by Fable against the wave-2 tree)
- Attention badges on views are per repository, so they belong under the tab they describe.
- The row that changes least sits highest: browser tabs over the toolbar; Safari's tabs in the macOS title bar are the native precedent.

## 2. Row 1: the title bar (surfaces), `<header>`, 48 px on macOS, 40 px elsewhere

One `role="tablist" aria-label="Workspace"` holds the surfaces and the repositories, with one `aria-selected` and a roving tab stop.

| Order | Control | Scope | Notes |
|---|---|---|---|
| 0 | Traffic-light inset | — | 76 px windowed; 12 px gutter in full screen (§4) |
| 1 | Logo mark, non-interactive | — | The "GitPulse" wordmark leaves the chrome (decision D2) |
| 2 | Fleet · Agents n · Tasks n | global | Become `role="tab"` in the same tab shell as repositories; close × stays beside the tab. `data-testid`s unchanged |
| 3 | `gp-separator` | — | The one sanctioned hairline |
| 4 | Repository tabs | per tab | All behaviour unchanged: pin, groups, stacks, dirty dot, conflicts, terminal count, preview, close, drag, keys, context menus, parked. Adds the overflow menu (§5). Each repository tab is a 28 px pill (`h-7`, 128 px minimum, 12 px icon), the height the stack and group heads already had; it used to shrink to its 18 px text line (decision D5) |
| 5 | Window-drag spacer | — | `data-drag-zone`, `flex-1`, 48 px minimum (E3). Open no longer follows the last tab, so the free space sits before it |
| 6 | + Open ▾ | global | Split pill: Open / Clone or recent. The first item of the global cluster, so its position is fixed (decision D4). Its recents menu aligns to the trigger's end edge |
| 7 | Fetch all · workspace status | global | `WorkspaceActions`, still hidden with one tab or fewer |
| 8 | Manvi chip | host-wide | `HarnessBadge` collapses to one `gp-chip` (tone = permission mode). Click opens a `gp-menu` holding the model `<select>` and the refresh-AI button. A `blocked` or `unchecked` verdict stays inline: a fired rule must never hide behind a click |
| 9 | Diagnostics + error badge | host-wide | Last, so the badge sits at the window corner |

## 3. Row 2: the view bar (lenses on the selected repository), `<nav aria-label="Views">`, 36 px

| Order | Control | Notes |
|---|---|---|
| 1 | View switcher | Four pills; the active one expands into its section tabs, inactive ones keep the hover flyout. DOM contract unchanged |
| 2 | `ViewContextBar` content, folded inline | Only at ≥ 1600 px; below that it keeps its own 40 px row. History's list segment (Commits / HEAD moves) and Work's conflict count; the slot is content-sized and its own background drags the window |
| 2a | **Page search** (`PageSearch.svelte`) | The one search field, for whatever page is on screen (decision D7). It takes three shares of the free width to the drag spacer's one (spacer floor 48 px), 56 rem cap. Its floor is 160 px from a 1400 px row and 96 px below, so at the default 1280 px window the view switcher keeps its full width (Work's is the widest). Focus does not repaint it; ⌘F focuses it from anywhere on the repository surface |
| 2b | **Glance** (`ViewBarGlance.svelte`) | After the drag spacer, four chips, each absent when it has nothing to say (decision D8): files changed in more than one worktree (opens Agents), the branch's pull request and its checks (opens Work › Remote), the last fetch's age (warns past a day; click fetches), the stash count. Each comes from a read already made, so the glance asks the host nothing. It gives way before the switcher: a chip that does not fit is hidden whole and inert, the last first. Its own lazy chunk |
| 3 | **Agents pill (this repository)** | Shows the most urgent attention among this repository's agents, ranked by the agent plane's `ATTENTION_REASONS` (needs-you › error › pending/blocked › finished), then the live count. Reads the plane through `scopeToRepository` and the per-session attention from `terminal/sessionActivity.ts` (`ATTENTION_EVENTS`); no second attention model. Click opens the Agents surface narrowed to this repository with `setAgentsRepositoryScope` (as Fleet's agent count already does). An unscanned or unprobed plane reads "unknown", never "0". Hidden when the repository has no agents and nothing unknown |
| 4 | **Terminal pill** | The terminal toggle moves here from the status bar: the most-used control gets the most-seen row. It is the *same* control, not a copy: it renders `terminalPillView` (`terminal/statusPill.ts`, the session summary and hover menu another session is building in the main checkout right now) and calls `repoStore.toggleTerminal()`. Lane I relocates it after that work lands; it does not rebuild it. Shortcut label from `platformCopy` |
| 5 | Uncommitted chip | Moves out of the switcher into the trailing cluster: the switcher says "where am I", the trailing cluster says "what state is this repository in" |
| 6 | Refresh | Per repository, last |

Trailing order, left to right: the glance, Agents, Terminal, uncommitted, Refresh, so the two "come here" controls sit nearest the content and the passive state sits at the edge.

Under Fleet, Agents or Tasks the row stays mounted at the same height, with the switcher `hidden`/`inert` and the surface's name in its place, so the workspace never jumps. On the welcome screen (no repository) row 2 is not rendered.

Branch and ahead/behind are not in the header. Readers: the status bar's branch pill; the tab's branch label at ≥ 1280 px; the sidebar pulse strip, which owns per-repository fetch/pull/push/stash. The glance shows the last fetch's age and the stash count as facts; only its fetch chip acts, and the stash's actions stay in the sidebar.

## 4. Windowed and full screen

- **Detection** (verified in source, not yet in the running app): Tauri has no full-screen event, but tao calls `emit_resize_event()` from both `windowDidEnterFullScreen:` and `windowDidExitFullScreen:` (src-tauri/framework/tao/src/platform_impl/macos/window_delegate.rs:587, 612), and `isFullscreen()` exists (`@tauri-apps/api` window.d.ts:465). So the existing `onResized` subscription (src/lib/desktop/windowState.ts:238) calls `isFullscreen()` and stamps `html[data-fullscreen]`. If the timing proves racy in the app, fall back to an objc2 observer of `NSWindowDidEnterFullScreenNotification`.
- **Reclaiming the inset:** `html[data-fullscreen]` collapses the 76 px spacer to a 12 px gutter. When the pointer touches the top edge, macOS slides the menu bar and traffic lights over the top-left band, so only the non-interactive logo mark sits there; nothing clickable does.
- **Windows and Linux:** the Overlay title bar is macOS-only in the vendored runtime, so there is no inset and row 1 is the 40 px strip.
- **The rows never merge**, at any width. Repository tabs have unbounded width, and the two rows have different keyboard models. Width is spent on labels instead.

### Breakpoints (container queries per row; chosen so no shape change lands on the 1400 px regression viewport)

| Width | Row 1 | Row 2 |
|---|---|---|
| ≥ 1600 | Everything labelled; Manvi shows its model | Section labels; context bar folds inline |
| 1440–1599 | Manvi model name | Section labels; context bar keeps its row |
| 1280–1439 | Branch names on tabs; "Fetch all", "Open", chip labels | Section labels on the active view |
| 1100–1279 | Tabs lose branch names | Section labels |
| 900–1099 | Fleet/Agents/Tasks → icon only, with the count as a small badge on the icon (never a bare number; the `aria-label` keeps name and count, e.g. "Agents, 2 running"); Fetch all and status → icons; Open → "+"; Manvi → shield; the uncommitted chip → icon with badge | Sections icon-only, active label kept |

Collapse order as the window narrows: context-bar fold → Manvi model → tab branch names → section labels → chip and button labels → tabs compress to 112 px and scroll. View labels never collapse. `minWidth` is 900 (tauri.conf.json, verified).

## 5. Overflow and density

### Edge-to-edge tabs with a diffuse blur edge

The tab strip runs edge to edge between the surfaces and the drag spacer, which sits before + Open. Where it hides tabs, the edge goes soft and then disappears, rather than clipping hard.
- **Base, already shipped:** `ScrollCue` writes `data-gp-fade` and the scroller masks its own edge over 2rem (app.css `[data-gp-fade]`). Kept.
- **New, the diffuse layer:** on each edge that still hides content, `ScrollCue` (new opt-in prop `diffuse`) renders two stacked strips with `backdrop-filter`: a light blur over 40 px and a heavier one over the outer 20 px, each masked from opaque at the edge to transparent inward. Together they form a progressive blur, with the opacity mask on top. Strips enter at 140 ms and exit at 70 ms (opacity) when the overflow state changes, and stay static while scrolling.
- **Settings:** strength follows the float-blur setting from lane S. Blur at 0 px, Reduce Transparency, or `transparency: reduced` leaves only the mask fade.
- **Chevrons:** shown on pointer hover only; keyboard users move with the roving tab stop and the overflow menu.
- **Rendering (unverified until measured):** the title bar is already a backdrop root, so the strips blur the bar's own content (the tabs), which is the intent. Measure scroll frame time at 120 Hz in WKWebView with and without the strips; ship only if no frame is added past 8.3 ms, and otherwise keep mask-only.

Scroll with the existing `ScrollCue`, plus a trailing "N more ▾" menu listing off-screen units with their dirty, conflict and terminal marks (IntersectionObserver, no per-frame layout reads). It renders nothing when the strip fits. Collapsed groups and folded stacks count as one unit. Tabs keep a 112 px minimum and are never ellipsised. Row 2 pill height follows the density setting (26 px comfortable, 24 px compact); row 1 height is fixed by the traffic lights.

## 5a. Further enhancements (proposed in the same lane)

- **E1 Attention on background tabs.** A repository tab shows a "needs you" mark when one of its agents is waiting (same derivation as the row-2 Agents pill), so an agent in a repository you are not looking at still reaches you. The mark is a different shape from the dirty dot, and the overflow menu lists it. Unknown stays unknown.
- **E2 Tab hover card.** After the 500 ms intent delay, a card (positioned by `ui/popover.ts`) shows path, branch with ahead/behind, uncommitted count, agents and terminals with their attention, and last fetch. It replaces the native `title` tooltip.
- **E3 A window-drag zone that cannot be squeezed out.** The spacer between the tab strip and + Open is the drag region with a 48 px minimum; the tab strip scrolls before it shrinks. Empty space in row 2 drags too.
- **E4 Jump to repository N.** ⌘1–⌘9 (Ctrl elsewhere) select repository tab N; ⌘9 selects the last, as browsers do. Next/previous repository already exist in the menu (`next-repo-tab`, `prev-repo-tab`). Labels come from `platformCopy`; check for a clash with existing shortcuts before assigning. Settled (lane K): these are native File-menu accelerators (`repo-tab-1`…`repo-tab-9`), so they work with a terminal or editor focused, and the views moved off ⌘1–3 to ⌥⌘1–3 on macOS and Ctrl+Shift+1–3 on Windows and Linux (Ctrl+Alt+digit is AltGr on many layouts; ⌘⇧3/4/5 are the macOS screenshot shortcuts).
- **E5 Fetch-all progress in its own pill.** A progress ring and Stop inside the pill while it runs; the result reads in the workspace status popover, not a toast.
- **E6 A slimmer status bar.** With the terminal moved to row 2, the status bar carries branch, sync and cadence only. Hiding it (`statusBarMode`) no longer hides the terminal control.

## 6. Motion (tokens 80/140/220/320/480; exits ≤ 70 ms; transform and opacity only)

- **View switch:** pill crossfade; old sections exit 70 ms, new ones enter 140 ms; the pane rises 6 px over 220 ms.
- **Repository switch:** row-1 pill crossfade; sidebar, status bar and workspace stay the same DOM nodes (the `tab-switch` harness pins this), so content dips opacity 70 ms out, 140 ms in.
- **Full screen:** the inset collapse happens inside the system's own animation; newly unlocked labels fade in at 140 ms. Leaving: labels fade out in 70 ms first, then the inset returns. No width transitions.
- **Opening Fleet, Agents or Tasks:** pill crossfade; the row-2 switcher exits 70 ms, the surface label enters 140 ms; the plate rises over 220 ms. Closing reverses it.
- Every new animated class joins both reduced-motion lists in the same commit.

## 7. Accessibility

Landmarks: `header` (row 1), `nav "Views"` (row 2), `main`, `aside`, `footer`. The status bar becomes `contentinfo` with inner `role="status"` spans instead of a live region wrapping buttons. Row 2 under a global surface is `hidden`/`inert`, not faded. Targets stay ≥ 24 px; icon-only states keep their `aria-label`s.

## 8. Contracts to rewrite in the same commit (never delete)

- `harness/tab-switch.html`: boot selectors, same-node probes, exactly one tablist whose label ends in "sections" (so the new one is "Workspace"), flyout intent/grace and unchanged group lefts, ArrowDown/Up into sections, the Graph list group, and the page search (one field; filter on Graph, find on Diff with the standing filter chip, the palette's results under it on Work; it fits at 900 px), and the glance (its order, tones and labels; at 1400 and 1280 px every view's switcher whole beside it, a chip that does not fit hidden whole and inert, the last first; its chips' destinations; no chip when there is nothing to say).
- `harness/repo-tabs.html`: no scroll or cue when it fits; close controls outside tabs with one roving stop; whole names; Open is a labelled pill at 1400 px, pinned right (the drag zone between the tab list and Open, its left edge unmoved when a tab opens or closes, its recents menu inside the window at 1400 and 900 px); `tasks-tab-close` lifecycle.
- `RepoTabBar.test.ts` (testids, Fleet-before-Agents order), `App.test.ts` (the `<RepoTabBar …/>` literal and the title-bar scroller row), `scripts/mac-material-contract.test.ts` (`.gp-glass.gp-repo-tabs` and its reduced-transparency override; the view row needs its own measured fill), `Sidebar.test.ts` if decision D3 retires the sidebar's Open.
- Both rows' `border-b` separators go (rule 7).

## 9. Decisions (approved 2026-11-10)

- **D1 Native window tabs.** `tabbingIdentifier` (tauri.conf.json) lets macOS draw a second, native tab bar under the repository row when "prefer tabs" is on. Decided: remove `tabbingIdentifier` from tauri.conf.json (lane I).
- **D2 Wordmark.** Drop "GitPulse" from the title bar (the menu bar and window title already say it; the welcome card keeps it). Decided: drop.
- **D3 Sidebar Open.** With Open in the title row, the sidebar's Open icon is a third copy. Decided: retire it, rewriting the Sidebar.test.ts pin in the same commit.
- **D4 Open placement.** + Open ▾ followed the last tab like a browser's new-tab control, so it moved whenever a tab opened or closed. Decided 2026-11-10 by the user: Open pinned right for a fixed position. It is the first control of the global cluster, after the drag spacer.

- **D5 Repository tab height (2026-11-11, user).** "Increase the tab pill size." Repository tabs are 28 px pills, matching the stack and group heads.
- **D6 One band, and the search takes the space (2026-11-11, user).** "Remove the separator line above, 2 headers should blend in." Rows 1 and 2 are one band of glass on macOS: where they meet, row 1 drops its bottom shade and drop shadow, row 2 drops its top sheen, and both drop their diagonal sheen (`app.css`, after the reduced-transparency rules; `--shadow-card-drop` is the card shadow without its top highlight). The search fills row 2's free width up to 56 rem (§3, D7). Its focus does not repaint the pill: no accent border, no ring; the caret marks the field. `harness/tab-switch.html` checks the join and the folded width.

- **D7 One search, adaptive to the page (2026-11-11, user).** "Make the top search universal and page adaptive (like a repository-specific command palette too)." Row 2 holds one field, `PageSearch.svelte`, routed by the view and section on screen through `search/pageSearch.svelte.ts`:
  - History › Graph: it is the commit filter (`filterStore`), the branch a chip inside it. The filter stays applied when the field loses focus.
  - History › Diff and Code › Explorer: the open viewer registers a find (count, Aa, .*, previous, next; Enter and Shift+Enter step, Escape clears). The viewer draws no Find of its own there; mounted anywhere else (a stash preview) it keeps `FindField`.
  - On a page with nothing of its own (Work, Insights, Code › Map, History › Suspects): the command palette for this repository, prefixes and all (`>` `/` `@` `#` `:` `%` `?`), its results dropping under the field from the palette's own engine, loaded on first use. Where the page has a search, typed text is that search, literally (`#include`, `// TODO`, `#123`); ⌘K opens the palette from every page. Entry cost: about 6.5 KB; the build gate read 636,823 of its 640,000 bytes at this change (the emitted file is about 6.9 KB larger than what the gate measures).
  - Each place keeps its own query, so a diff's find never becomes the commit filter. Where the field is not the commit filter but the filter narrows the list on screen (Diff, HEAD moves), the filter shows as a chip with its own clear.
  - Not in v1: Work's conflict list and the code map keep their own controls.

- **D8 A glance in row 2 (2026-11-11, user).** "Make this space useful with some info, instead of blank: cool at-a-glance." Four facts the app already holds, drawn by `ViewBarGlance.svelte` from `ui/viewBarGlance.ts`:
  - Overlap: the collision facet of the insights snapshot the Agents pill's sweep already took. Shown only when the scan found overlap; a partial scan shows its count as a floor (`2+`), and a failed one shows nothing, because the Agents pill already turns unknown for it. The sweep is not polled (row 2 asks once per repository; Agents asks when it opens), so the chip names the read's age (`AgentPlaneState.readAt`), as the pull request chip does.
  - Pull request: the last `cmd_github_context` any panel read for this repository (`github/pullRequestReads.ts`, fed by Work's load, the Remote panel and the Policy panel; a failed read never replaces a good one). The chip names the age of that read. No panel has read it: no chip, and no `gh` call of its own.
  - Last fetch: the metadata facet's `cmd_last_fetch_at`; neutral under a day, warn from a day. No fetch recorded: no chip.
  - Stash: the metadata facet's stash list; a capped list is a floor, a failed read no chip (the sidebar says why).
  - Width: Work's switcher is the longest. Measured in `harness/tab-switch.html` with four chips to show: with no Agents pill, Work keeps three at a 1400 px row and two at 1280 px, Code and History all four; with the Agents pill up in its widest (labelled, unknown) state, Work keeps only the overlap chip at both widths, Code all four, History all four at 1400 px and three at 1280 px. The strip's flex-shrink outweighs the switcher's a thousandfold, and a ResizeObserver hides the chips its width does not hold. Every run's widths are in the page's diagnostics line.
  - Cost: its own 4.6 kB chunk, loaded when row 2 mounts; the entry chunk would not hold it under the 640,000-byte gate.

## 10. Unverified until the app runs

The resize-event timing around the native full-screen animation; the menu-bar reveal overlay; whether any harness besides `repo-tabs.html` asserts `aria-pressed` on the surface chips; the view row's glass fill for 4.5:1 at 13 px.
