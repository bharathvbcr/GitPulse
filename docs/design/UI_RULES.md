# UI rules for every agent

These rules bind every agent and every person who changes GitPulse's interface: the Svelte components, `src/app.css`, the harness pages, and any user-visible string. The reasoning behind each rule is in [DESIGN_LANGUAGE.md](DESIGN_LANGUAGE.md) (the "Plate" language). The order the codebase moves onto them is in [OVERHAUL_PLAN.md](OVERHAUL_PLAN.md). When this file and the code disagree, the code is what ships. Fix whichever one is wrong in the same change, and say which one it was.

Status: **proposed 2026-11-09, not yet adopted.** These rules become binding when the owner approves the plan. Existing code then migrates wave by wave. Until `scripts/design-lint.mjs` lands, the rules are checked in review. After it lands, the lint checks them against a baseline that may only go down.

> **Not in the code yet. Do not write these names before their wave lands.** Most of the "Instead" and "Use" names below are introduced by Wave 0 or Wave 1 of the plan. A class that resolves to nothing fails silently: `gp-pill` was written at a dozen call sites before anyone defined it. Until the owner exists, use today's fallback.
>
> | Arrives in | Name | Today's fallback |
> |---|---|---|
> | Wave 1 (`app.css`) | `text-micro`, `text-meta` | Existing size; add no new `text-[Npx]` |
> | Wave 1 | `text-ok`/`warn`/`danger`/`info`/`unknown`, `text-faint`, `--c-on-accent` | Existing classes; add no new palette colors |
> | Wave 1 | `--gp-r-*`, `--gp-space-*`, `--gp-row-h`, `--gp-header-h`, `--gp-edge*`, `--gp-dur-*`, `--gp-ease-*` | Existing Tailwind radius and spacing; `transitions.ts` constants |
> | Wave 1 | `gp-btn` `data-variant`/`data-size` | `gp-btn`, `gp-btn-primary`, `gp-btn-danger`, `gp-btn-success` |
> | Wave 1 | `gp-chip` (tones), `gp-header`, `gp-row`, `gp-label`, `gp-slide-docked`, pill `gp-select` | `gp-pill` / existing `gp-chip`, `gp-surface-header` + `gp-section-edge`, existing recipe, existing label style, the sidebar's current width transition, `gp-select` |
> | Wave 1 | `LAYERS.TOAST`, `STICKY`, `SCROLL_CUE` | Nearest existing `LAYERS` tier |
> | Wave 0 (built and reviewed; on branch `overhaul/wave-0`, **not on main**) | `src/lib/motion/tokens.ts` + `springs.generated.ts`; `src/lib/ui/color/{oklch,guard,derive}.ts`; `src/lib/ui/appearance/themeSchema.ts`; `src/lib/ui/copy/{vocabulary,describeFailure,sentenceCase}.ts`; `scripts/design-lint.mjs` + `bun run design:lint`; `harness/frame-rate.html` | `transitions.ts`, `accents.contract.test.ts`, the glossary in §3, `formatError`, review |
> | Wave 2–3 | `Toggle.svelte`, `Slider.svelte`, `Dialog.svelte`, `Splash.svelte` | `SettingToggle`, the existing range styles, `gp-scrim` + `gp-card` + `focusTrap`, none |
>
> This table is updated as each wave merges. A row is removed once its name exists on main.

## 0. Before you touch UI

1. Run `gitpulse_insights` on the absolute repository path. If another worktree or session holds a file you are about to edit, run `gitpulse_collision_risk` on it, and claim it with `gitpulse_worktree_claim` if you are in a lane. `src/app.css`, `src/App.svelte` and `src/lib/stores/interfaceStore.ts` are hot files: one lane at a time.
2. Find the owner before you write anything. Use `devmap_search` / `devmap_explore` with `repo_path` for the component or utility, then the table in §2 below. DevMap cannot see CSS classes or Tailwind strings (css and html are "blind to calls" in its status), so class-usage counts come from `rg`. Say which tool answered.
3. Read the contract tests that pin what you are changing. The ones a visual change most often breaks:
   - `scripts/mac-material-contract.test.ts`
   - `src/lib/ui/cssTokens.contract.test.ts`
   - `src/lib/ui/themeContrast.test.ts`
   - `src/lib/ui/themeAccessibility.contract.test.ts`
   - `src/lib/ui/accents.contract.test.ts`
   - `src/lib/ui/macAppearance.test.ts`
   - `src/lib/ui/transitions.test.ts`
   - `src/lib/motion/motionPreference.test.ts`
   - `src/lib/components/SettingsModal.test.ts`
   - `src/lib/ui/settingsCatalog.test.ts`
   - `src/lib/ui/settingsSections.test.ts`

   **Update a pinned expectation in the same commit that changes it. Never delete or weaken one.** If a test encoded the old design, say so in the commit and rewrite its assertion to pin the new design.

## 1. Hard rules (checked by `design-lint`)

| # | Never | Instead |
|---|---|---|
| 1 | `text-[9px]`, `text-[10px]`, `text-[11px]`, any `text-[Npx]` | `text-micro` (10), `text-meta` (11), `text-xs` (12, body), `text-sm` (13), `text-base` (15), `text-lg` (18), `text-display` (24, metric numerals only) |
| 2 | `font-bold`, `font-weight: 650`, `uppercase` + `tracking-*` labels | `font-normal`/`font-medium`/`font-semibold` only. Section labels use `gp-label` (sentence case, `text-meta font-medium`, muted) |
| 3 | Tailwind palette colors (`text-amber-400`, `bg-rose-500/20`, `border-emerald-…`, `zinc`, `slate`…) and new `dark:` color pairs | Role tokens: `text-ok`/`warn`/`danger`/`info`/`unknown`, `bg-warn/15`, `text-accent`, `text-textMuted`, `text-faint` |
| 4 | `#hex` or `rgb(12 34 56)` literals in `.svelte` | A `--c-*` token. The only exceptions are the xterm and canvas adapters, which convert tokens at their boundary |
| 5 | `rounded-[Npx]`, `rounded-3xl`, a radius picked by eye | `--gp-r-xs/sm/md/lg/xl/pane/full` through the component that owns the shape. One-line controls are pills; anything that can grow is a rounded rectangle |
| 6 | `z-50`, `z-30`, `z-[N]`, a literal `z-index` | `LAYERS` from `src/lib/ui/layers.ts`. Add a named tier there if none fits |
| 7 | `border-t`/`border-b`/`border-l`/`border-r` or `divide-*` to separate regions | Spacing, then a tonal step (`bg` → `surface`), then the scroller's own `[data-gp-fade]` edge. Hairlines are allowed only inside `gp-menu-sep`, `gp-separator`, control outlines, `gp-card`'s edge, and forced-colors restorations |
| 8 | Border opacity stops other than three | `--gp-edge-soft` (0.35), `--gp-edge` (0.65), `--gp-edge-strong` (0.9) |
| 9 | `transition-all`, `transition-[width]`, `[height]`, `top`/`left`/`margin`/`padding`/`filter`/`box-shadow`/`backdrop-filter` transitions | Animate `transform` and `opacity` only. Color transitions are allowed at `--gp-dur-instant` on small controls. The single exception is the `gp-slide-docked` utility (one-shot ≤200 ms, never during a drag) |
| 10 | Duration or easing literals (`duration: 180`, `duration-500`, `cubic-bezier(...)` in a component) | `src/lib/motion/tokens.ts` / `--gp-dur-*`, `--gp-ease-*`, and the helpers in `src/lib/ui/transitions.ts` |
| 11 | `backdrop-filter` or `backdrop-blur-*` anywhere except the one float rule in `app.css` | Wear `shadow-pop`/`shadow-float` or `gp-menu`: the float tier blurs you. Chrome, plate and content never blur |
| 12 | A scoped `<style>` class that is a button, link, chip, pill, toggle or tab (`.link`, `.text-action`, `.icon-button`, `.close-alert`, `.chip-remove`…) | The owner in §2 |
| 13 | `...` in a user-visible string | `…` |
| 14 | Banned terms in UI strings (§3): `BANNED_UI_TERMS` in `src/lib/ui/copy/vocabulary.ts`, checked in the same `.svelte` user-copy positions as rule 13: markup text, display attributes (`title`, `aria-label`, `placeholder`, `alt`, `label`, `data-tip`) and string literals inside `{…}`. Not checked: `<script>` strings, `.ts` files, `src/lib/ui/copy/` itself. The linter header lists the exact positions and the blind spots. The retired verbs (`RETIRED_VERB_TERMS`) and voice checks (`VOICE_TERMS`) are separate lists, added to the lint in Wave 2 once each view has cleared them. | The glossary word |
| 15 | An animated class missing from the reduced-motion lists, or a translucent surface missing from the reduced-transparency block | Add it to **both** lists in the same commit. The paired tests pin them equal |
| 16 | A `gp-separator` directly under a section heading | The heading already separates its group: put `mb-2` on the heading row. A separator that divides one group from the next (a menu group, a heading that follows it) is fine |

## 2. Use this, never that

| Need | Use | Not |
|---|---|---|
| Button | `gp-btn` + `data-variant="primary\|secondary\|ghost\|danger\|success"` + `data-size="sm\|md"` | `gp-btn-*` siblings (being folded in), bare `text-accent hover:underline` buttons, scoped `.link` |
| Icon-only button | `gp-icon-btn`, `aria-label` required, target ≥ 24×24 | `p-0.5` icon with no label |
| Count, label, status tag | `gp-chip` + `data-tone` + `data-kind="count\|label\|removable"` | `gp-pill` (being folded in), `!bg-*` overrides |
| Text input | `gp-field` (pill) / `gp-field-multi` (rounded rect) | bare styled `<input>` |
| Select | `gp-select` (pill, same metrics as `gp-field`) | unstyled `<select>` |
| On/off (a setting that takes effect) | `Toggle.svelte` (generalised `SettingToggle`) | raw `type="checkbox"` / `role="switch"` |
| Pick items from a list, or confirm a statement before an action | `Checkbox.svelte` (native checkbox, `gp-check` look; its unchecked edge is `--c-text-faint`, not `--gp-edge`, because a control edge needs 3:1 and a separator hairline doesn't reach it) | a `Toggle` used as a row selector, a raw unstyled `type="checkbox"` |
| Choice of 2–5 | `SettingSegment` / `gp-segmented` + `gp-seg-btn` + `gp-liquid-tabs` on tablists | hand-rolled segmented thumbs |
| Range | `Slider.svelte` (the existing range styles) | custom slider |
| Menu / popover | `gp-menu`, `gp-menu-item`, `gp-menu-sep`, positioned by `src/lib/ui/popover.ts`, which also owns the exit: put `out:popoverExit` beside `use:popover` | a `gp-card` used as a menu |
| Dialog / sheet | `gp-scrim` + `gp-card` + `focusTrap` + `backdropFade`/`cardScale` in **and** out (→ `Dialog.svelte`) | a dialog without an outro, `rounded-3xl` |
| Toast | `ToastContainer` on `LAYERS.TOAST` | literal `fly`/`fade` timings |
| Panel header | `gp-header` (title and actions slots, tonal step) | `border-b` headers, hand-picked `px-3 py-2` |
| List row | `gp-row` (`--gp-row-h` from density) | per-list padding recipes |
| Nothing exists yet | `EmptyState.svelte` (title says what would be here, hint says how it comes to exist) | ad-hoc "No …" text |
| Couldn't determine | `gp-notice` with tone `unknown` and the reason, plus a Rescan action | `EmptyState`, a green tick, or "0". **A check that could not run never looks like one that passed** |
| Loading longer than ~1 s | `Skeleton.svelte`, shown after 300 ms and held for at least 400 ms | `animate-spin` + "Loading…". A spinner is allowed only inside a button or chip that is busy |
| Tooltip | `title=`, which the global `Tooltip.svelte` upgrades. Use `data-tip-guide` for view and section tabs | a new tooltip component |
| Error text | `describeFailure(action, err)` (wraps `formatError`) → "Couldn't <verb> <object>. <Reason>." | raw `e.message`, "Failed to", "Unable to", backend text as the headline |
| Shortcut text | `shortcutKeyLabel` / `shortcutTextLabel` / `platformChord` from `platformCopy.ts` | hard-coded "Cmd/Ctrl+…" or ⌘ glyphs |
| OS words (Finder, System Settings) | `platformCopy.ts` | literals |
| Stacking | `LAYERS.*` | z literals |

## 3. Copy

- **Voice.** Say what happened, then what the user can do. Name things the way the user does, never by module. Use one idea per label. Something that wasn't checked is "Couldn't check", never "clean", "none" or "0". No exclamation marks, "please", "oops" or "successfully".
- **Sentence case** everywhere: buttons, menus, tabs, headings, tooltips, and command-palette labels ("Fetch all remotes", "Stash working tree"). Proper nouns (GitPulse, GitHub, Manvi, DevMap) keep their case.
  - **Surface names are proper nouns:** Settings, Tasks, Fleet, Agents, Work, Code, History and Insights. The glossary already writes Settings that way. So "Open Tasks", "Open Settings", "Open Fleet", never "Open tasks" when the words name the surface. A lowercase "tasks" is a plural noun: "Open tasks" in `aria-label="Open tasks"` on a strip of task tabs is correct.
  - **The native macOS menu keeps Title Case** ("Stash Working Tree", "Interactive Rebase…"). That is the platform's convention (Apple's HIG), and `menu.rs` and `menuState.ts` follow it. Do not sentence-case a native menu item to match the palette.
  - **The palette and the menu may differ in case, never in wording.** The same action has the same words in both: "Stash working tree" in the palette is "Stash Working Tree" in the menu. The palette may add a leading "Open " or "Use ", and may say more after the menu's words ("Fetch all remotes" for "Fetch"). `catalog.test.ts` ("palette labels agree with the native menu") reads `menu.rs` and fails when they diverge.
- **US spelling**: color, organize, behavior.
- **Ellipsis** `…` only when the control opens a further step, or for in-progress text.
- **One verb per meaning:**
  - Refresh re-reads local state.
  - Fetch contacts the remote.
  - Rescan re-runs a scanner.
  - Rebuild regenerates an index.
  - Try again only after a failure.
  - Delete destroys.
  - Discard throws away uncommitted changes.
  - Remove takes something out of a list.
  - Archive is a task's Done.
  - Clear empties a field or filter.
  - Dismiss closes a notice.
  - Open navigates.
  - Show / Hide toggles visibility.
  - Run / Stop controls a process.

  Retired: Reload, Recheck, Retry, Launch, Organise.
- **Templates:**
  - Error: `Couldn't <verb> <object>. <Reason>. [Try again]`
  - Destructive confirm: title `Delete <object>?`, one sentence of consequence, `[Delete] [Cancel]`, with the destructive button `data-variant="danger"` and never the primary gradient.
  - Empty: title is what would be here, hint is how it comes to exist.
  - Unknown: `Couldn't check <object>` + reason + `[Rescan]`.
- **Glossary.** The owner will be `src/lib/ui/copy/vocabulary.ts` (Wave 0).

  | Concept | Say | Never say |
  |---|---|---|
  | The indexed code graph | **code map** (the Code → Map section stays "Map"; DevMap only names the CLI in Settings → Agents) | code graph, repo map, devmap (lowercase), codeintel |
  | The policy tool | **Manvi** (as in "Manvi policy") | MANVI |
  | GitPulse's own gate | **checks** ("GitPulse's checks") | harness, sidecar, posture |
  | One execution of a task | **run** | attempt |
  | A visible terminal or agent process | **session** | — |
  | Linked checkout | **worktree** | lane, checkout |
  | Board item | **task** (card only for the physical card) | ticket |
  | Repository | **repository** | repo, project |
  | Ref | **branch** (ref only in Refs/Reflog) | — |
  | Commit | **commit** ("changes" only for uncommitted edits) | — |
  | App settings | **Settings** | Preferences, Options |
- **Banned in UI strings:** min rung, rung, ledger record(s), generation N, is_fresh, sidecar, posture, harness (as a noun), MANVI, snake_case identifiers, successfully, Oops, and "could not" (write "couldn't": "The log couldn't be read", not "could not be read"). The lint reads user-string positions only: wire and protocol strings, text compared against Rust, comments and fixture data that mirrors a backend message keep their words.

  "Verdict" is Manvi's own term. Change it in both apps or neither, because this is cross-app copy.

## 4. Motion and performance

- Animate `transform`/`opacity` only. Svelte transitions must use the `css` form, which runs on WAAPI off the main thread, never `tick`. `Spring`/`Tween` are only for interruptible gestures on small elements.
- Durations: `--gp-dur-instant` 80, `fast` 140, `base` 220, `slow` 320, `emphasis` 480 ms. Exit is shorter than enter: about 70 ms, opacity only.
- Easings are `linear()` polylines baked from spring parameters by `scripts/bake-springs.mjs`. Never hand-type them.
- Gate every animation on `prefersReducedMotion()` (`src/lib/motion/easing.ts`). System settings win. The in-app setting can only add reduction.
- **Blur budget.** Only floats blur, at `--mac-glass-blur-float`. The user's blur setting moves that radius (0–40 px). At 0 it must produce `backdrop-filter: none`, not `blur(0)`, because `blur(0)` still creates a backdrop root. Never put `opacity < 1`, `filter`, `mask` or `will-change: opacity|filter` on an ancestor of a float: each makes a backdrop root, and the blur goes flat.
- Sidebar and dock slide as **overlays** (`transform`). Resizing writes width live while dragging, with no transition. The docked toggle may use the one-shot `gp-slide-docked`.
- Nothing is promoted with `will-change` permanently. Add it on gesture start and remove it on end.

## 5. Accessibility

- Text contrast ≥ 4.5:1 and non-text ≥ 3:1 against the worst surface, for every theme, accent, imported theme and material setting. The runtime guard `src/lib/ui/color/guard.ts` and the contract tests use the same function.
- Targets ≥ 24×24 CSS px. Focus is always visible through `--ring-focus`.
- System Reduce Motion, Reduce Transparency (via the native bridge, because WebKit does not implement the media feature, verified 2026-11-09), Increase Contrast and forced colors override every app setting.
- Never use color alone to carry meaning. Pair a tone with an icon or a word.

## 6. Definition of done for a UI change

1. `bun run check` and `bun run test` pass. Run the **full** suite before merge, not a targeted subset.
2. `node scripts/design-lint.mjs` shows no new violations once it exists.
3. The harness page for each surface you touched passes in Chromium **and** WebKit: `npm --prefix <your-worktree> run test:browser -- --harness <name>` and `… test:webkit -- --harness <name>`. Running `node <wt>/scripts/browser-regressions.mjs` directly serves the main checkout, not yours.
4. Screenshot both themes, at default and spacious density, with Reduce Motion on and off where motion changed.
5. No feature disappeared. Check the surfaces you touched against the feature inventory in [OVERHAUL_PLAN.md](OVERHAUL_PLAN.md) §3.
6. Report what you verified and how, and what you did not.
