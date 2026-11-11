# GitPulse design language: "Plate" — spec v0.1

Status: **proposed, 2026-11-09**. Designed by the advisor model from four read-only audits (visual, copy, architecture/theming, external research), orchestrated and spot-checked by the planning session. Companion files: [UI_RULES.md](UI_RULES.md) (the rules every agent follows), [OVERHAUL_PLAN.md](OVERHAUL_PLAN.md) (waves, lanes, verification).

Evidence labels, kept on purpose: **[verified]** = read in the source at the cited location (line numbers are against the dirty tree of 2026-11-09 on top of `55d605b8` and will drift); **[brief]** = from the external research and **not** re-verified — do not copy a `[brief]` number into code as a rule without checking it; **[inferred]** = design judgment. Line numbers are pointers, not contracts.

Sources read for this spec: `docs/MACOS_APPEARANCE.md`, `src/app.css` (lines 1–1600), `src/lib/ui/{accents,transitions,layers,platformCopy,formatError,settingsSections}.ts`, `src/lib/motion/{easing,motionPreference}.ts`, `src/lib/stores/{interfaceStore,densityStore,themeStore}.ts`, `src/lib/views/viewRegistry.ts`, the Skeleton/EmptyState/SettingToggle/SettingSegment components, and the test names in `scripts/mac-material-contract.test.ts`.

## Name and one-paragraph thesis

**Plate.** The window is a translucent *plate* standing on a static *field*; everything the user reads sits on tonal steps of that plate, nothing is boxed with lines, and the only things that blur are the few surfaces that genuinely float over content. "Liquid glass" is where the window meets the desktop (native, free) and where a float meets content (CSS, bounded). "Material" is the tonal/elevation/shape/motion grammar in between. The name is also the test: if a component cannot say which plate tier it sits on, it is not finished.

---

## 1. Principles (each with a testable rule)

1. **Tone separates, lines do not.** Rule: no `border-t`/`border-b`/`border-l`/`border-r` on a layout seam outside the allowed utilities in §3; the CI grep in §13 counts them and the count may only go down.
2. **Only floats blur.** Rule: `backdrop-filter` appears in app.css only inside the `@supports` float rule (already pinned by `mac-material-contract` "filters float surfaces only, never always-on chrome" [verified: scripts/mac-material-contract.test.ts:93]) and never in a `.svelte` file.
3. **Motion is compositor-only.** Rule: no `transition`/`animation` whose property list includes `width|height|top|left|right|bottom|margin|padding|backdrop-filter|filter|box-shadow` in a `.svelte` file (one exception class, §8.5, with a 200 ms cap). Durations and easings come from `motion/tokens.ts`, never literals.
4. **One owner per shape.** Rule: every button, field, select, chip, toggle, segmented control, menu, dialog, toast, panel header, empty state and skeleton renders through the named `gp-*` utility or component in §9; a scoped `<style>` block defining `.btn|.link|.icon-button|.chip|.pill|.toggle` fails the lint.
5. **Tokens, never literals.** Rule: in `.svelte`/`.ts` UI code, no `text-[Npx]`, no `rounded-[Npx]`, no `#hex`, no `z-[N]`/`z-50`, no Tailwind palette color class (`amber-400`, `rose-500`…). The @theme scale and `--gp-*` tokens are the only vocabulary.
6. **Contrast is a gate, not a review.** Rule: every color pair the app can produce (presets, custom accent, imported theme, every material-intensity setting) passes WCAG 2 AA 4.5:1 text / 3:1 non-text against the worst surface it can land on, enforced at runtime by the same function the contract tests use (`accents.contract.test.ts` thresholds [verified: lines 82, 89]).
7. **Honesty survives styling.** Rule: a state that *could not be determined* never renders with the success tone or the empty-state component; it renders the "unknown" tone (§4) and the `gp-notice` with the reason. (This is the existing honesty invariant, restated as a visual rule so a restyle cannot break it.)
8. **System accessibility wins.** Rule: system Reduce Motion, Reduce Transparency, Increase Contrast and Forced Colors always override the app's own settings; app settings are one-way (they can only add reduction), mirroring `motion/easing.ts` [verified: lines 36–50].

---

## 2. Surface model

Six tiers, bottom to top. Each names what it paints, whether it may blur, and how it is elevated.

| Tier | What it is | Paints | Blur | Elevation |
| --- | --- | --- | --- | --- |
| **Field** | Behind everything: the blurred desktop (native `NSVisualEffectView underWindowBackground`, window-server cost = 0 to the app) plus the 4-blob hue field on `.gp-shell` and the veil [verified app.css:981–997] | `rgb(var(--c-bg) / var(--mac-shell-veil))` + radial gradients | Native only | none |
| **Chrome** | Title bar, repo strip, sidebar card, status bar | `--mac-glass-fill` (0.40 dark / 0.58 light), sheen + tint gradients, 1px `--mac-glass-edge` | **Never** (pinned) | Edge highlight + `--shadow-card` |
| **Plate** | The workspace card the view renders into [verified app.css:1208–1219] | `--mac-plate-fill` (0.46 / 0.60) | Never | Radius `--gp-pane-radius` 14px + `--shadow-card` |
| **Content** | Panels, cards, rows, fields inside the plate | `--mac-fill-surface` (0.50 / 0.60), `--mac-fill-bg` (0.42 / 0.52), recess `--mac-recess` (a *shade*, not a fill) | Never | **Tone only**: three steps (`bg`, `surface`, `surface-hover`) plus one shade. No shadow on content unless it is a `gp-card` (which carries `--shadow-card`) |
| **Float** | Menus, popovers, tooltips, toasts, coach marks, sticky editor headers, dialog cards | `bg-surface`, `--shadow-pop` or `--shadow-float` | **Yes**: `blur(var(--mac-glass-blur-float)) var(--mac-glass-boost)`, the one filtered rule [verified app.css:1040–1043] | Shadow + sheen |
| **Overlay** | Scrims | Dim + hue; **no filter on macOS** (one blur per dialog) [verified app.css:1053–1058] | No | n/a |

**How "Liquid Glass" and "Material" coexist without breaking Apple's "no glass in content" rule.** Glass = Field (native) + Float (CSS). Apple's guidance [brief] puts glass on the controls/navigation layer floating above content, never on the content layer. Plate's content tier is *translucent material*, not glass: it has no blur, no specular sheen, no saturation boost, only alpha and tone, which is exactly the HIG's "sidebars more opaque / content opaque" reading translated into a translucent-shell app. Chrome is the ambiguous case; it is glass in *look* (sheen, edge) without the filter, because its backdrop is a smooth gradient and blurring it is a no-op (the existing argument [verified MACOS_APPEARANCE.md:58–66]).

**Native glass vs CSS glass decision.** 
- Keep the current native `underWindowBackground` as the baseline on every macOS version.
- `window-vibrancy`'s `apply_liquid_glass` (NSGlassEffectView, macOS 26+, private API) [brief]: do **not** adopt in the first wave. It would replace the Field's native blur, not the Float's CSS blur, so its visible win is the window edge and the sidebar/title bar meeting the desktop; its cost is a private API on top of the private API already forfeiting MAS, plus an unmeasured interaction with a transparent WKWebView. Gate it behind a human decision (§14) and a measurement: compare frame time of the shell idle + a menu open on macOS 26 with and without it.
- **The reduced-transparency bug [verified 2026-11-09].** Probed in a bare `WKWebView` on macOS 27.2 (build 26B123): `(prefers-reduced-transparency: reduce)` and `(prefers-reduced-transparency: no-preference)` *both* evaluate false — the same answer as `(made-up-feature: x)` — while the controls `(prefers-contrast: no-preference)`, `(prefers-reduced-motion: no-preference)` and `(forced-colors: none)` evaluate true. So WebKit does not implement the feature, and the system "Reduce transparency" setting never reaches `app.css:1349` or `StatusPopover.svelte:418`; Increase Contrast and forced colors *do* work through their own queries. Fix with a native bridge that mirrors the motion pattern: Rust reads `NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency`, observes `NSWorkspace.accessibilityDisplayOptionsDidChangeNotification`, emits an event on change, TS stamps `html[data-transparency="reduced"]`, and the existing `@media (prefers-reduced-transparency: reduce), (prefers-contrast: more), (forced-colors: active)` block [verified app.css:1349–1398] gains `html[data-transparency="reduced"]` as an equivalent selector (both the main window and the status popover). A sibling of `motionPreference.test.ts` pins the two selector lists equal. The attribute is one-way: present only to add reduction.

---

## 3. Separator policy

**Default: no line.** Seams are made by (in order of preference):

1. **Spacing**: the 4px grid (§7); a header is separated from its list by `--gp-space-2` (8px) of nothing.
2. **Tonal step**: the header sits on `surface`, the list on `bg` (one rung), never the same color twice (the "repaint is not depth" rule [verified app.css:1122–1144]).
3. **Scroll-edge fade**: the scroller masks its own edge via `[data-gp-fade]` [verified app.css:187–217]; a sticky header over a scroller gets the float treatment only if it genuinely overlaps content.
4. **Hairline, last resort**, only in these utilities: `gp-menu-sep` (inside menus), `gp-separator` (the 40%-wide centred pill), the `gp-field`/`gp-btn`/`gp-chip` control outline, `gp-card`'s edge, `gp-floor` (the dashed lower-bound mark, which is semantics not decoration), and forced-colors restorations.

**Border-opacity stops: exactly three.** `--gp-edge-soft: rgb(var(--c-border) / 0.35)` (separators, skeleton edges), `--gp-edge: rgb(var(--c-border) / 0.65)` (control outlines, cards), `--gp-edge-strong: rgb(var(--c-border) / 0.9)` (menus, focused/hovered fields). The nine current stops (`/40 … /90`) collapse onto these; the migration is mechanical (`/40,/45,/50 → soft`, `/60,/65,/70 → edge`, `/80,/90 → strong`).

`gp-section-edge` [verified app.css:265–275] stays as the transitional shim that makes an existing `border-b` transparent; the end state deletes both the `border-b` and the shim from call sites and the lint counts `border-[tblr]` lines down to the allow-list.

---

## 4. Color system

### 4.1 Role tokens (the only names components may use)

Keep the existing `--c-*` RGB-triplet format because Tailwind's opacity modifiers need channels [verified app.css:409–413] and xterm needs hex at its boundary [verified MACOS_APPEARANCE.md:128–133]. Add roles; do not rename what exists.

Surfaces (exist): `--c-bg`, `--c-surface`, `--c-surface-hover`, `--c-border`.
Ink (exist): `--c-text`, `--c-text-muted`. Add `--c-text-faint` (disabled/placeholder, light-mode remap-safe, replaces every `text-textMuted/40..80`).
Accent (exist): `--c-accent`. Add `--c-on-accent` (text on an accent fill; white in dark, computed in light).
Status (new, both themes, replaces the light-only `--c-status-*` zoo [verified app.css:458–472] and the 889 raw palette lines): `--c-ok`, `--c-warn`, `--c-danger`, `--c-info`, `--c-unknown` (the "could not determine" tone: neutral slate, deliberately *not* grey-as-disabled), each with a paired `--c-on-ok`… for filled chips. Mapping of today's zoo: emerald/green/teal → ok; amber/yellow/orange → warn; rose/red → danger; sky/cyan/blue/indigo → info; purple → accent or info (decide per site); zinc/slate → unknown.
Focus (exists): `--ring-focus`, derived from accent.
Brand (exists): `--brand-gradient`; **restrict to the primary button, the slider thumb and the splash**. Nothing else is gradient.

Tailwind side: `@theme` gains `--color-ok: rgb(var(--c-ok))` etc., so components write `text-ok`, `bg-warn/15`, `border-danger/40`. The light-mode `[class~=…]` remap block [verified app.css:502–653] and the 277 `dark:` pairs are retired **last**, once no component names a palette color (migration order §13).

### 4.2 Deriving the palette from user input

Model: **Linear-style three inputs** [brief] in OKLCH, computed in TypeScript, emitted as RGB triplets into the existing names.

Inputs: `base` (a hex: the surface hue and lightness), `accent` (a hex), `contrast` (0–1, how far apart the surface steps sit). Theme mode decides whether the ladder climbs or descends.

Derivation (dark; light mirrors with L inverted):
- `bg = base`; `surface = base + ΔL 4%`; `surface-hover = base + ΔL 9%`; `border = base + ΔL 16%`, each scaled by `contrast` (0.5 = shipped ladder, which is `11/14/23 → 20/26/41 → 31/39/58 → 44/54/78` [verified app.css:418–421]; the "+4/+9/+16" L steps are **[inferred]** by eye from those RGB values, and the derivation script computes the real OKLCH deltas from the shipped triplets so 0.5 reproduces them exactly).
- Hue of every surface step = hue of `base` (chroma capped at 0.03 so a vivid base still yields a usable grey).
- `text = L 93%`, `text-muted = L 68%`, `text-faint = L 52%`, hue = base hue, chroma 0.02.
- `accent` = user's hex, then pushed through the contrast guard (§4.3).
- Status hues are fixed (ok 150°, warn 75°, danger 20°, info 240°, unknown base hue chroma 0); only L/C adapt per theme through the same guard.
- Presets become **named triplets of inputs** rather than hand-tuned RGB: the six existing accents stay as `accent` inputs with `base` = shipped; their current per-theme RGB values [verified accents.ts:37–68] are kept as the "shipped" column so the contract test's cases still derive from the list.

**As built (Wave 0 lane C, `src/lib/ui/color/derive.ts`) [verified].** Where the build differs from the sketch above, the build wins:
- **Exact reproduction.** At `contrast` 0.5 with `base` = shipped `--c-bg`, `deriveTokens` returns every shipped triplet exactly in both themes: bg, surface, surface-hover, border, text and text-muted. `derive.test.ts` reads `src/app.css` on every run. When the overhaul's refactor changes those triplets, the test fails loudly. That is intended: the derivation must be re-measured, not the test loosened.
- **Measured steps replace the guesses.** Dark surface, hover and border sit at ΔL +0.054, +0.109 and +0.170 from bg. Text and muted use measured offsets (L 94.6 and 70.9), not the 93/68 above. Each step keeps the small hue drift the shipped ladder has.
- **The chroma cap applies to `base` only.** The shipped hover and border measure C 0.037 and 0.045, so capping every step would break reproduction.
- **`--c-border` is not contrast-guarded.** The shipped dark border is far below 3:1 against hover. That is fine, because it is a tonal edge, not a control boundary that must be seen to be operated (WCAG 1.4.11). So the 3.0 minimum in §4.3 applies to `ring`, chip fills and focus indicators, not to `--c-border`. `NON_TEXT_MIN` is exported for those callers.
- **`on-*` ink is white when it reaches 4.5:1 on the fill, and near-black otherwise.** So the default dark accent gets dark ink; white would only reach about 2.7:1.
- **Refusals name the key and the ratio** (`{ ok: false, failures }`). A mid-lightness dark base is refused by design.

### 4.3 Contrast guard (hard gate)

One function, `ui/color/guard.ts`, used by the contract test *and* the runtime: `ensureContrast(fg, bgWorst, min)` nudges OKLCH L of `fg` away from `bgWorst` in 1% steps until `contrast ≥ min`, preserving hue; if it hits L 5% or 95% without passing it returns `{ok:false}` and the caller refuses the color (custom accent: snaps to nearest preset with a notice; imported theme: import fails with the offending key named). Minima: 4.5 for `text*`, `accent`-as-text, status-as-text; 3.0 for `ring`, chip fills and focus indicators (`--c-border` is a tonal edge and is not guarded, see §4.2 "As built"). Worst surface per theme is the same `WORST_SURFACE[theme]` the existing test uses [verified accents.contract.test.ts:35]. The guard also runs when the material intensity setting (§11) changes, because thinner surfaces change the effective ground; its result is rendered in Settings as a live "AA passes / adjusted / refused" line, never silent.

---

## 5. Shape

Radius tokens (`--gp-r-*`, 4px grid, concentric-friendly):

| Token | px | Use |
| --- | --- | --- |
| `--gp-r-xs` | 4 | keycaps, inline code, skeleton bars |
| `--gp-r-sm` | 8 | menu items, list-row hover, chips' inner elements |
| `--gp-r-md` | 12 | menus/popovers, fields (multiline), selects, notices |
| `--gp-r-lg` | 16 | cards, panels, toasts |
| `--gp-r-xl` | 20 | dialogs, sheets, the welcome card (currently 28 [verified app.css:1194]: bring to 20) |
| `--gp-r-pane` | 14 | sidebar and workspace cards (exists as `--gp-pane-radius` [verified app.css:1205]; alias it) |
| `--gp-r-full` | 9999 | pills: buttons, single-line fields, chips, toggles, segmented |

**Concentric rule:** inner radius = outer radius − padding, clamped to ≥ `--gp-r-xs`. A `gp-card` (16) with 8px padding holds `gp-r-sm` (8) children; a `gp-menu` (12) with 4px padding holds `gp-r-sm` items. Implemented as `--gp-r-inner: max(var(--gp-r-xs), calc(var(--gp-r-outer) - var(--gp-pad)))` set by the container utilities so children inherit it.

**Pill vs rounded-rect decision:** controls that are *one line tall* are pills (button, icon button, single-line field, chip, toggle, segmented, keycap stays xs). Anything that can grow taller than one line is a rounded rect (multiline field `gp-field-multi` → md). **Select becomes a pill**: it is one line tall and sits beside pill fields, so the current `rounded-lg` on `gp-select` [verified app.css:374] is the inconsistency, not the field. Dialogs: `rounded-3xl` outliers → `--gp-r-xl`. Window: 12px is the macOS native corner [brief]; the plate's 14px sits inside it concentrically with the 8px inset. `--gp-radius-scale` (0.5–1.25, §11 "corner style") multiplies every token except `full` and `pane`.

---

## 6. Type

Scale (7 steps, rem so `uiFontScale`/webview zoom carries it). Naming constraint [verified by count this session, non-test `.svelte`/`.ts`]: `text-xs` 315 uses, `text-sm` 42, `text-base` 15, `text-lg` 0, `text-xl` 3, `text-2xl` 9, and the `gp-btn`/`gp-field`/`gp-notice`/`gp-seg-btn`/`gp-select` utilities all `@apply text-xs` [verified app.css:53, 67, 297, 309, 328]. So **`text-xs` keeps Tailwind's 12px** (it already is the body size) and is not redefined; the two smaller steps get new names; `sm`/`base`/`lg` are redefined because their use is small and visibly reviewed by the codemod.

| Utility | rem | px@16 | Line-height | Use | Change |
| --- | --- | --- | --- | --- | --- |
| `text-micro` | 0.625 | 10 | 1.3 | badges, keycaps, the attention count | new; 9px literals → this (nothing renders below 10) |
| `text-meta` | 0.6875 | 11 | 1.4 | metadata, chips, descriptions, table cells | new; 11px literals → this |
| `text-xs` | 0.75 | 12 | 1.333 (4/3) | **body default** (= `body { font-size: 0.75rem }` [verified app.css:710]), buttons, fields, menu items | unchanged |
| `text-sm` | 0.8125 | 13 | 1.45 | panel headers, emphasised rows | redefined 14→13 (42 sites) |
| `text-base` | 0.9375 | 15 | 1.3 | surface titles (today's `gp-surface-title` 15px [verified app.css:290]) | redefined 16→15 (15 sites) |
| `text-lg` | 1.125 | 18 | 1.25 | dialog titles, splash, empty-state title | redefined 18→18 (0 sites); a heading that was `text-xl` or `text-2xl` codemods to this; a metric numeral that was `text-2xl` or `text-3xl` goes to `text-display`; both are then banned |
| `text-display` | 1.5 | 24 | 1.1 | metric numerals only: a count on a card, a stat. Not for headings or prose | new; `--text-display` for a component `<style>`, `text-display` in markup |

**`text-xs` keeps line-height 1.333, not 1.45 [verified app.css:79-80, `--text-xs--line-height: calc(1 / 0.75)`].** Tailwind ships `text-xs` at that height, and 1.45 would change the height of every one of the 315 `text-xs` uses (rows, buttons, fields, menu items) and reflow the app for no gain. The other six steps use the heights in the table; `text-xs` is the one place the table is a decision, not a default. **`text-display` is 24 px (1.5 rem), line-height 1.1** [verified app.css:88-89]: it exists for one job, a metric numeral (a count on a card, a stat in Pulse or Work), and `text-2xl`/`text-3xl` metric numerals map to it. A heading that was `text-xl` becomes `text-lg`. The scale has seven steps in all; a font size outside them is a rule 1 failure in markup, in a scoped `<style>` and in a `.ts` class string.

The `@theme` override of `--text-sm/base/lg` and the codemod land in **the same step** (§13 step 4), never in the tokens-first step, because redefining a utility already in use is a component change. `text-[9px]`/`[10px]`/`[11px]` become lint failures with the one-to-one codemod (9→micro, 10→micro, 11→meta). Weights: **3** — `font-normal` 400, `font-medium` 500, `font-semibold` 600; `font-bold` and `font-weight: 650` literals are retired (650 → 600). Label style: **sentence case, `text-xs font-medium text-textMuted`, no uppercase, no tracking**; the 184 uppercase+tracking lines become plain section labels (a `gp-label` utility owns the style so it can be changed once). Monospace: only for code, paths, hashes, refs, keycaps and the terminal; never for numbers in prose. Line heights are fixed per step above; `leading-*` utilities are not used on text (lint).

Fonts: `--gp-font-ui` (default: the current system stack [verified app.css:857]) and `--gp-font-mono` (default: `ui-monospace, "SF Mono", Menlo, Consolas, monospace`). User-chosen families are **local font names only**, appended in front of the fallback stack, validated with `document.fonts.check()`; no URLs, no `@font-face` from settings. Two JS adapters read `--gp-font-mono` the way `--bg-terminal`/`--bg-main` are read today: xterm's `fontFamily` option (settable at runtime through `terminal.options`, unlike `allowTransparency` [inferred from xterm's API; not verified in this session]) and the canvas graph's label font via `themeFromCss`. Size stays `uiFontScale` (webview zoom) [verified uiScale.ts header]; the type scale does not get a separate size knob.

---

## 7. Spacing and density

4px grid: `--gp-space-1..8` = 4, 8, 12, 16, 20, 24, 32, 40 (Tailwind's default spacing already is this; the rule is "no `px-[Npx]`, no `gap-[Npx]`, no `h-[Npx]`").

Named recipes (utilities, so a row is one class):
- `gp-row`: `min-height: var(--gp-row-h); padding-inline: var(--gp-space-2); gap: var(--gp-space-2); border-radius: var(--gp-r-sm)`.
- `gp-header`: `height: var(--gp-header-h); padding-inline: var(--gp-space-3); gap: var(--gp-space-2)`; replaces every `border-b` header; sits on `surface` tone.
- `gp-toolbar`: same height, `gap: var(--gp-space-1)`, controls are `gp-btn` size `sm`.
- `gp-surface-header` (exists [verified app.css:282–287]) becomes `gp-header` with the `title` slot.

Density → CSS vars on `html[data-density]`:

| | compact | comfortable (default) | spacious |
| --- | --- | --- | --- |
| `--gp-row-h` | 24 | 28 | 32 |
| `--gp-header-h` | 28 | 32 | 36 |
| `--gp-pad` (card/panel inner) | 8 | 12 | 16 |
| `--gp-density` (numeric, for JS consumers) | 0.85 | 1 | 1.15 |

Ownership: **one global density**, stored in `interfaceStore.density`, replacing `densityStore` (which today is a two-value `spacious|compact` typed by `canvas/GraphRenderer` [verified densityStore.ts:2–3]). GraphRenderer and `TaskDensity` (`ui/taskView`) become *consumers* of `--gp-density`/`data-density` with their own mapping tables (the graph's row heights are canvas numbers, so it reads `--gp-row-h` through `themeFromCss` like it reads colors). Row heights that today are "not CSS" (brief) move to the vars; `sidebar/metrics.ts` keeps `contain-intrinsic-size` per row kind but derives from `--gp-row-h`. `gp-cv-row`'s static 30px fallback [verified app.css:1465] becomes `var(--gp-header-h)`.

---

## 8. Motion

### 8.1 Tokens (`src/lib/motion/tokens.ts`, mirrored as CSS vars on `:root`)

Durations: `--gp-dur-instant: 80ms` (hover tint, press), `--gp-dur-fast: 140ms` (fade, scrim), `--gp-dur-base: 220ms` (pop, menu, view switch, sidebar slide), `--gp-dur-slow: 320ms` (dialog, sheet, dock), `--gp-dur-emphasis: 480ms` (splash hand-off, theme crossfade stays 220).

Easings (CSS `linear()` polylines baked from spring parameters; the numbers are M3 Expressive's [brief, not verified here] and the baked curves must be generated by a committed script, `scripts/bake-springs.mjs`, not typed by hand):
- `--gp-ease-spatial-fast` (damping 0.6, stiffness 800) — menus, chips, segmented thumb
- `--gp-ease-spatial` (0.8, 380) — view switch, sidebar, dock, dialog
- `--gp-ease-spatial-slow` (0.8, 200) — splash, sheets
- `--gp-ease-effect` (1.0, 1600) — opacity/color (critically damped: never overshoots)
- `--gp-ease-exit: cubic-bezier(0.4, 0, 1, 1)` — every exit

The existing `--mac-liquid-ease: cubic-bezier(0.22, 1, 0.36, 1)` [verified app.css:927] is kept as the non-spring fallback and aliased to `--gp-ease-spatial` on runtimes without `linear()`.

**Baked curves are normalized to the duration token, not played at their natural settle time** [verified, Wave 0 lane T]. The springs' natural settle times are long: spatial-fast 482 ms, spatial 484 ms, spatial-slow 653 ms, effect 267 ms. A `linear()` curve is a shape over 0→1. So `--gp-ease-spatial` at `--gp-dur-base` (220 ms) plays the 484 ms spring in 220 ms, which behaves like a stiffer spring with the same damping ratio: the overshoot shape is kept and time is compressed. This is intended, because durations stay the single speed control and reduced motion still zeroes them. A pane that wants the spring's real physical feel uses the matching settle duration, which `springs.generated.ts` exports, and never a hand-typed one. The curves are within 0.008 of the analytic spring and are regenerated by `node scripts/bake-springs.mjs`. `--check` fails if the committed file drifts.

### 8.2 Asymmetry (keeps the existing invariant: OUT strictly shorter than IN [verified transitions.ts:9–16])
Enter = `dur-base`/`spatial`; exit = `dur-fast` × 0.5 ≈ 70 ms/`ease-exit`, opacity only (no reverse scale). Scrim: enter 140, exit 70. Dialog card: enter 320 from scale 0.985 (mac) / 0.97, exit 70.
As built in Wave 1 lane A: `EXIT_MS` caps every exit at `dur-fast / 2` = 70 ms, so the old 60 ms literals became 70. The card exit still reverses the scale (`cardScaleOut`), as it did before the overhaul. The opacity-only exit is Wave 3 lane D's change.

### 8.3 What may animate
`transform` and `opacity`, full stop. `background-color`/`color`/`border-color` may *transition* at `dur-instant` for hover/press (cheap repaint of a small control, not a layer). Never: width, height, inset, margin, padding, `backdrop-filter`, `filter`, `box-shadow` (shadow changes are done by crossfading two pre-painted layers if ever needed).

### 8.4 Named choreographies
- **View switch** (`.gp-view`): fade+6px rise, 220/`spatial` [today 180/260 verified app.css:1487–1506]; the liquid selection pill crossfade stays 280 (Svelte crossfade, transform/opacity).
- **Section switch**: content crossfade 140 only; no translate (same subject, different lens: §viewRegistry doc).
- **Menu/popover** (`.gp-pop`): opacity + 8px + scale .97 → 1, 220/`spatial-fast`; exit 70.
- **Toast**: enter from `translateY(8px)`, 220; exit 140 opacity.
- **Loading → content hand-off**: skeleton holds ≥ 400 ms once shown (no flash), content enters via `.gp-view` fade; skeleton exits at 70 ms. Skeleton appears only after a 300 ms delay (Doherty/0.1–1 s band [brief]).
- **Splash/boot** (§12): a static plate with the mark and the brand gradient on the `#app` background *before* JS (so cold start is never an empty `#app`), then a 480 ms opacity hand-off to the shell.

### 8.5 Sliding sidebar and terminal dock (the honest part)
A transform-only slide of a *docked* panel leaves the workspace where it was, so either the workspace also translates (and then its right edge is wrong) or the layout reflows (width = layout, not compositor). Two modes:

- **Overlay mode (recommended default when collapsing/expanding with a shortcut or the toggle):** the sidebar/dock is `position: absolute` **relative to the row container that holds both `.gp-sidebar` and `.gp-workspace`** (they are siblings [verified App.svelte:1212]; inside `.gp-pane`'s `contain: layout paint` [verified app.css:1437] it could not overlap the neighbouring card), slides with `transform: translateX(-100%) → 0` (dock: `translateY(100%) → 0`), 220/`spatial`, content behind does not reflow. The workspace keeps the collapsed layout; the open panel floats over it with the Float treatment (so it legitimately blurs). This is the "fluid" wish satisfied at compositor cost only.
- **Docked mode (today's layout, kept for drag-resize and for users who want content to reflow):** a **one-shot** `transition: width/height 200ms` on the user's gesture, inside a `contain: layout paint` pane [verified `.gp-pane` app.css:1437–1439], never during a drag (drag writes width live with no transition, as today [verified Sidebar.svelte:209–211]). This is the single sanctioned exception to §8.3, capped at 200 ms, because it is a discrete user action, not continuous motion. The dock currently has no animation at all (brief) and gets this.

Which one is default is a human decision (§14).

**Measured cost of the docked mode** [verified, Wave 0 lane M, `harness/frame-rate.html`, WKWebView on macOS 27.2]: animating `width` lost 7–10 frames per 3 s at 60 Hz (p99 48–66 ms), and lost more when uncapped to 120 Hz. `transform` and `opacity` held the full rate in every run. The fixture is a 400-row flex relayout, so the magnitude is fixture-specific; the direction is not.

### 8.6 Reduced motion
Two triggers, pinned equal by `motionPreference.test.ts` [verified app.css:1509–1546]: every new animated class (`.gp-slide`, `.gp-skeleton`, `.gp-splash`, `.gp-toast`) is added to **both** the `@media (prefers-reduced-motion: reduce)` list and the `html[data-motion="reduced"]` list in the same commit, or the test fails. Reduced motion: durations → 0 via `motionDuration`, skeleton pulse stops (glyph stays, as today), the sidebar snaps, the splash crossfade becomes a cut. A third setting value "off" is *not* added: the system preference plus the one-way in-app switch already express it, and a tri-state would reintroduce "animate anyway".

---

## 9. Component grammar

Owner column = where the one implementation lives. Every row's "don't" is a lint target in §13.

| Component | Owner | Variants / sizes | States | Do | Don't |
| --- | --- | --- | --- | --- | --- |
| **Button** | `gp-btn` utility + `data-variant="primary|secondary|ghost|danger|success"` + `data-size="sm|md"` (one base so the macOS press/reduced rules select `.gp-btn` once — today `gp-btn-success` is missing from all four [verified app.css:1336, 1342, 1531, 1544]) | md 28px pill, sm 24px pill; `ghost` is the link-button (no fill, accent text, underline on hover only) | hover tint, `:active` scale .97 (mac: +1px translate), disabled .4, loading (spinner replaces icon, label stays) | one primary per view surface; verbs from §10 | scoped `.link/.text-action/.close-alert`; raw `<button class="text-accent">` |
| **Icon button** | `gp-icon-btn` | 28 / 24; `data-tone="danger"` | same as button; **min target 24×24** (WCAG 2.2) | `aria-label` required | `p-1` icon-only buttons without a label |
| **Chip** (absorbs pill/badge) | `gp-chip` + `data-tone="neutral|ok|warn|danger|info|unknown|accent"` + `data-kind="count|label|removable"` | one size: 20px pill, `text-2xs` for counts, `text-xs` for labels | removable has the × as an inner `gp-icon-btn` sm | tone = meaning | `gp-pill` (merge into chip), `!bg-*` overrides |
| **Field** | `gp-field` (pill) / `gp-field-multi` (md) | sm/md heights = button heights | focus ring, invalid (`data-invalid` → danger edge), disabled | keep `-webkit-text-fill-color` [verified app.css:332] | bare `<input class="rounded-lg …">` |
| **Select** | `gp-select` → **pill**, same height/padding as `gp-field` | — | as field | native `<select>` kept (keyboard, a11y) | 30 un-styled selects |
| **Toggle** | `SettingToggle.svelte` (exists) generalised: `Toggle.svelte` with optional label row | 32×18 track (exists) | checked/disabled/focus | `role="switch"` | 82 raw checkbox/switch lines |
| **Segmented** | `SettingSegment.svelte` + `gp-segmented`/`gp-seg-btn`; the liquid selection pill is the active indicator everywhere, not only the header | sm/md | `aria-pressed`, `data-active` | 19 hand-rolled → this | custom thumbs |
| **Slider** | `input[type=range]` styles (exist [verified app.css:808–833]) wrapped as `Slider.svelte` with value label and keyboard step | — | — | brand gradient thumb is allowed | — |
| **Menu / popover** | `gp-menu` + `gp-menu-item` + `gp-menu-sep`; `ui/popover.ts` owns positioning (memory: 12 popovers already) | — | `.gp-pop` enter | Float tier: blurs | menus built from `gp-card` |
| **Dialog / sheet** | `gp-scrim` + `gp-card` + `focusTrap` (exist) → `Dialog.svelte` shell with title/body/footer slots; **sheet** = same shell anchored to an edge, slides with transform | md 480 / lg 720 / full-height sheet | enter 320 / exit 70; **every dialog has an out-transition** | one blur per dialog | `rounded-3xl`; missing outro |
| **Toast** | `Toast.svelte` (exists) on `LAYERS.TOAST` (new, 65) | `tone` | auto-dismiss 6 s, pause on hover | one action max | literal durations |
| **Panel header** | `gp-header` (§7) | with/without title, with actions slot | — | tone step, no line | `border-b` |
| **List row** | `gp-row` | `data-selected`, `data-active` | hover `surface-hover`, selected = accent/12 fill + accent text | 28px default | per-list hover recipes |
| **Empty state** | `EmptyState.svelte` (exists; 19 files) | compact/regular, optional action | — | **only for "nothing exists"** | ad-hoc "No …" text; using it for "could not scan" (that is `gp-notice` tone `unknown`) |
| **Skeleton** | `Skeleton.svelte` (exists) | text/rect/circle/card/tree-row/code-lines | 300 ms delay, 400 ms min hold | for >~1 s loads | `animate-spin` + "Loading…" as the default (spinner only inside a button or a chip) |
| **Notice** | `gp-notice` + tone | ok/warn/danger/info/unknown | — | the honesty surface | — |
| **Splash** | `index.html` inline CSS + `Splash.svelte` | — | 480 ms hand-off | static, no JS needed to show | spinner on boot |

---

## 10. Copy language

**Voice (5 rules):** (1) Say what happened and what the user can do, in that order. (2) Name the thing the user calls it; never the module that produced it. (3) One idea per sentence; one sentence per label. (4) A state we could not determine is said as "couldn't check", never "clean", "none" or "0". (5) No exclamation marks, no "please", no "oops", no "successfully".

**Casing:** sentence case everywhere (buttons, menus, titles, settings). Proper nouns keep their case. The view registry already is sentence case [verified viewRegistry.ts labels].
**Spelling:** **US** (recommend; `platformCopy.ts`, the registry and Tailwind vocabulary are US already; "color/organise" in copy are the outliers). Code comments may stay as written.
**Ellipsis:** the character `…`, only to mean "opens a further step" (`Rename…`) or "in progress" (`Fetching…`); never three dots.
**Numbers:** digits always; thousands with a thin space in tables, comma in prose; relative times via `timestampStyle`.

**Action verbs (one each):**
- `Refresh` = re-read from local state; `Fetch` = contact the remote; `Rescan` = run a scan again (coverage, secrets, health); `Rebuild` = regenerate an index; `Try again` = repeat the failed action (only on an error). *Reload*, *Recheck*, *Retry* are retired.
- `Delete` = destroy permanently (branch, worktree, file); `Discard` = throw away uncommitted changes; `Remove` = take out of a list without destroying (a repository from the tab strip, a pin); `Archive` = the Done state for tasks (memory: archive *is* done); `Clear` = empty a field or filter; `Dismiss` = close a notice/toast without acting. Never interchange.
- `Open` for navigation; `Show`/`Hide` for toggles; `Run` for anything that starts a process; `Stop` to end one.

**Templates:**
- Error: `Couldn't <verb> <object>. <Reason as a sentence, from formatError>. [Try again] [Details]` — `formatError` keeps passing backend text through [verified formatError.ts], a new `describeFailure(action, err)` wraps it in the template and strips the banned terms below before display.
- Confirmation (destructive): title `Delete <object>?`; body one sentence of consequence (`This removes the branch and its 3 unmerged commits.`); buttons `[Delete] [Cancel]`, destructive is `data-variant="danger"`, never the primary gradient.
- Loading: `Loading <object>…` only as the skeleton's `aria-label`; no visible "Loading…" text.
- Empty: title = what would be here (`No worktrees`), hint = how it comes to exist (`Create one from a branch to work in parallel.`), action optional.
- Unknown: `Couldn't check <object>` + reason + `[Rescan]`.

**Banned in UI strings** (lint, §13): `min rung`, `ledger record(s)`, `generation N`, `is_fresh`, `sidecar`, `posture`, `rung`, `harness` as a noun, (`verdict` is **not** banned outright: it is Manvi's own term and appears in Manvi's CLI output, so renaming it in GitPulse alone would be cross-app copy drift; check Manvi's vocabulary first and change both or neither), `MANVI` in caps, `devmap`/`DevMap` as a product name in copy (see glossary), snake_case identifiers, "successfully", "Oops".

**Glossary decisions:**
- **Code map** is the user-facing name for everything DevMap produces ("the code map is 3 commits behind", "Rebuild code map"). *DevMap* appears only in Settings → Agents where the CLI is installed, as the tool name. "repo map", "graph", "Map" (section label) → the section is renamed **Map** stays (it is the shortest) but its tooltip says "code map".
- **Manvi** — product casing, never MANVI. In copy it is "policy" (the section is already called Policy [verified viewRegistry.ts:103]); "Manvi" appears where the user installs or configures it.
- **Harness** → **"GitPulse's checks"** in copy (the badge `HarnessBadge` becomes "checked by GitPulse"); the technical term stays in code.
- **Session** = one terminal/agent process the user can see; **run** = one execution of a task by an agent (a task can have many runs); **attempt** is retired (a run that failed and was repeated is "run 2").
- **Worktree** everywhere the user sees it; **lane** is retired from copy (agents' internal term).
- **Task** everywhere; **card** only when talking about the physical card on the board ("drag the card").
- **Repository** in sentences and headings; **repo** never in copy (tooltips included). Fine in code.
- **Branch** (not ref) unless it genuinely is a tag or remote ref.

Owner: `src/lib/ui/copy/vocabulary.ts` exporting the nouns and verbs as constants (the `workbench/vocabulary.ts` pattern already exists), plus `describeFailure`. The 12-key i18n pilot is not expanded by this work; the overhaul must not block on i18n.

---

## 11. Customization model — Settings → Appearance

Regroup (keeps all 9 section ids valid for persisted selection by adding a `resolveSection`-style fallback; `SETTINGS_SECTION_IDS` is a tuple with a contract test [verified settingsSections.ts:9–19]):

- **Appearance** (grows): Theme, Colors, Type, Shape, Material, Motion, Density. Absorbs `uiFontScale` (already here), `density` (from Graph), `reduceMotion`, status-bar mode stays in Layout (it is layout).
- **Layout**: sidebar/dock mode (overlay/docked), status bar, repo strip, tab stacking.
- Others unchanged.

**Platform gating:** Material intensity, Float blur, Background field and Transparency drive `html.macos` tokens only; on Windows/Linux they are dead knobs and are hidden behind a *capability* read from the backend (the existing rule: capability command, not `isMacOS()` in the component), with the section explaining what is unavailable rather than showing inert sliders.

Every setting below: storage = `interfaceStore` (localStorage, deferred write, as accent/uiFontScale today [verified interfaceStore.ts]); theme mode stays in `themeStore`. CSS vars are written inline on `<html>` by one applier, `ui/appearance/apply.ts`, the way `applyAccent` does [verified accents.ts:112–125].

| Setting | Control | Store key → CSS var | Bounds | Perf cost | A11y precedence |
| --- | --- | --- | --- | --- | --- |
| Theme mode | segmented system/light/dark | `themeStore` → `html.dark/.light` | — | View Transition 220 ms once | reduced motion → cut |
| Theme import/export | file picker + "Export current" | `interfaceStore.customTheme` → derived `--c-*` triplets | JSON schema below | none at runtime | contrast guard may refuse |
| Accent | 6 preset swatches + custom hex | `accent: AccentId | {hex}` → `--c-accent`, `--c-on-accent` | guard §4.3 | none | Increase Contrast → ring opaque (exists) |
| UI font / Code font | two text fields with local-font validation | `fontUi`, `fontMono` → `--gp-font-ui`, `--gp-font-mono` + xterm/canvas adapters | installed local families only | reflow once | — |
| Text size | existing slider | `uiFontScale` → webview zoom | 0.75–1.5 (exists) | — | — |
| Density | segmented compact/comfortable/spacious | `density` → `html[data-density]` | 3 values | reflow once | — |
| Corner style | segmented sharp/soft/round | `radiusScale` → `--gp-radius-scale` 0.5/1/1.25 | 3 values | none | — |
| Material intensity | slider "more transparent ↔ more opaque" | `materialOpacity` 0–1 → `--mac-glass-fill`, `--mac-plate-fill`, `--mac-fill-*` alphas via `calc(base + (1-base)*t)` | **floor = shipped alphas** (0 = today's values, which are the measured AA minimum [verified MACOS_APPEARANCE.md:159–165]); 1 = opaque | none per frame | Reduce Transparency → forced 1 |
| Float blur | slider | `floatBlur` 0–40 → `--mac-glass-blur-float`; at 0 the applier also stamps `html[data-float-blur="off"]` | 0–40 px; **0 must yield `backdrop-filter: none`, not `blur(0)`** (a var inside `blur()` cannot produce `none`, and `blur(0)` still creates a backdrop root). The `[data-float-blur="off"]` rule lives *inside* the one pinned float selector block so "filters float surfaces only" [verified mac-material-contract:93] keeps passing | area × radius per float on screen; floats are small and transient | Reduce Transparency → none |
| Background field | segmented off/subtle/vivid + hue picker (rotates the 4 blobs' hues together) | `fieldGain` 0/0.5/1, `fieldHue` → `--mac-hue-gain`, `--mac-hue-*` | gain ≤ 1 (shipped dark); hue rotation keeps shipped chroma/L | none (static layer) | Reduce Transparency → `background-image: none` (exists) |
| Wallpaper image | **not in v1** (human decision §14) | would be `--gp-field-image` on `.gp-shell` | would force `--mac-shell-veil ≥ 0.7` | one static layer, cheap | contrast unmeasured → refused until measured |
| Motion | segmented full/reduced | `reduceMotion` (exists, one-way) | — | — | system reduce → reduced, cannot be undone in-app |
| Transparency | segmented auto/reduced | `reduceTransparency` (new, one-way) → `html[data-transparency="reduced"]` | — | — | system (native bridge §2) → reduced |

Theme JSON schema (`gitpulse-theme/1`): `{ "schema": "gitpulse-theme/1", "name": string, "appearance": "dark"|"light", "colors": { "base": hex, "accent": hex, "contrast"?: 0..1, "ok"?: hex, "warn"?: hex, "danger"?: hex, "info"?: hex, "text"?: hex, "textMuted"?: hex }, "field"?: { "hues": [hex×4], "gain": 0..1 }, "shape"?: { "radiusScale": 0.5..1.25 }, "fonts"?: { "ui": string, "mono": string } }`. Honored: colors (through §4.2 derivation, then the guard), field, shape, fonts (local names). **Not honored, by rule:** any CSS string, `url()`, filter/blur values, z-index, font URLs, durations. Validation: unknown keys are reported and ignored; a failing guard names the key and the achieved ratio; export writes the *resolved* values plus the inputs so a theme round-trips. Import of Zed/VS Code/Warp formats is a later adapter that maps onto this schema, not a second schema.

---

## 12. Information architecture (every feature retained)

- **Three-layer nav is right**; keep view → section → content. Changes: the inactive-pill hover flyout stays, but the *active* view's section icon-tabs gain their labels at ≥1100px (icons alone cost discoverability; the registry's `summary` is already the tooltip). Fleet/Agents pills move from the repo strip into the view strip's trailing cluster as global surfaces, so the repo strip is only repositories (and can auto-hide, as it already can).
- **Settings**: Appearance / Layout / Views / Commit graph / Diff & code / Analysis / Agents / Repo hygiene / Updates — ids unchanged, density and reduceMotion move into Appearance (§11).
- **Splash/boot**: lives in `index.html` as inline critical CSS painting the plate color and a centred mark on `#app` before any script. That block runs before `themeStore` has set `html.dark/.light`, so its colors come from `@media (prefers-color-scheme)` plus a `localStorage` read of `gitpulse_theme_preference` in a 3-line inline script, never from the theme class; then `Splash.svelte` hands off (§8.4). It is the *only* place the brand gradient appears at size.
- **Sidebar/dock behaviors**: overlay vs docked (§8.5); `⌘\` toggles the sidebar, `⌘J` the dock (if free — platformCopy owns the shortcut spelling); collapsed sidebar keeps a 44px rail with section icons rather than vanishing; the dock remembers height per repository (exists) and gains the slide.
- **Command palette**: unchanged; its 8 modes gain the glossary verbs.
- **Status bar**: unchanged structurally; its chips become `gp-chip` tones.

---

## 13. Enforcement and migration order

Mechanical checks (one script, `scripts/design-lint.mjs`, run in `ci:local`; each rule carries a baseline count that may only decrease, so existing violations do not block but new ones do):

1. `text-\[\d+px\]`, `rounded-\[`, `z-\[`, `\bz-(10|20|30|40|50)\b` in `.svelte` → 0 new.
2. Tailwind palette classes `(text|bg|border|ring|from|to)-(amber|yellow|orange|rose|red|emerald|green|teal|sky|cyan|blue|indigo|purple|zinc|slate|gray)-\d` in `.svelte` → 0 new.
3. `#[0-9a-f]{3,8}` in `.svelte` (outside `<script>` tests) → 0 new.
4. `border-[tblr]\b` in `.svelte` outside the §3 allow-list → count down.
5. `transition-\[(width|height|…)\]` or `transition-all` in `.svelte`; the §8.5 docked exception is the one `gp-slide-docked` utility → 0 elsewhere.
6. `backdrop-blur-*` or `backdrop-filter` in `.svelte` → 0 (already partially pinned).
7. Scoped `<style>` selectors matching `\.(btn|link|chip|pill|toggle|icon-button|text-action|close-\w+)\b` → 0 new.
8. `"\.\.\."` in template strings → 0; `uppercase` + `tracking-` → 0 new.
9. Button/menu labels in Title Case (heuristic: two capitalised words, not a proper noun from the glossary list) → warn.
10. Banned terms list (§10) in template text → 0.
11. New animated class names must appear in both reduced-motion lists (`motionPreference.test.ts` already; add the transparency sibling).

Agent guide ([UI_RULES.md](UI_RULES.md); not `AGENTS.md`, which agents may not edit): the component table (§9) as "use this, never that", the token names, the copy templates, and "run `bun run design:lint` before `ci:local`".

Migration order (each step updates the named contract tests rather than deleting them):
1. **Tokens first**: add `--gp-*` radius/space/density/motion vars, the two *new* type names (`text-micro`, `text-meta`) and the status color roles in `@theme`. **Not** the `--text-sm/base/lg` redefinition (that is a component change; it lands in step 4). No component changes. `cssTokens.contract.test.ts` gains the new names.
2. **Button unification**: `gp-btn` base + `data-variant`; update the four macOS press/reduced selectors to `.gp-btn` and `mac-material-contract`/`motionPreference` lists accordingly. Codemod the private dialects.
3. **Headers and separators**: `gp-header`; strip `border-b` + `gp-section-edge` from call sites; lint rule 4 baseline recorded.
4. **Type codemod** (9/10/11px → `micro`/`meta`; `sm`/`base` sites reviewed; `xl`/`2xl` → `lg`) **together with** the `@theme` redefinition of `--text-sm/base/lg`, and the label style.
5. **Chips/selects/toggles/segmented** onto owners; `mac-material-contract` "gives every gp-segmented tablist the liquid tab treatment" [verified line 605] widens to the newly converted controls — update its discovery set.
6. **Status colors**: components → `ok/warn/danger/info/unknown`; then retire the light remap block and `dark:` pairs **last**; `themeContrast.test.ts` re-derives from the new roles.
7. **Motion tokens** + sidebar/dock slide + splash + skeleton policy; both reduced-motion lists updated. Tests touched: `src/lib/ui/macAppearance.test.ts` (pins the mac 260 ms view/dialog timings that move to 220/320) and `transitions.test.ts` (pins the standard profile) [verified MACOS_APPEARANCE.md:306–314]; `mac-material-contract` "draws the sidebar and workspace as rounded cards with no gutter" [verified line 646] for the overlay-mode positioning.
8. **Appearance settings**: density merge (GraphRenderer type change), fonts, material/blur/field sliders, transparency bridge + its paired test; `SettingsModal.test.ts` and `settingsSections.test.ts` updated.
9. **Theme import/export** + contrast guard shared with `accents.contract.test.ts`.
10. **Copy pass**: vocabulary module, templates, glossary renames, banned-term lint.

---

## 14. Risks and the four human decisions

Risks:
- **Light theme over a dark desktop is unmeasured** [verified MACOS_APPEARANCE.md:167–178]; the material-intensity slider's floor is only known-safe over a white desktop. Mitigation: ship the slider with floor = shipped and measure the dark-desktop case before allowing thinner.
- **`linear()` springs on WKWebView**: supported per brief but the baked polylines must be generated and committed; hand-typed curves drift. Fallback is the existing cubic.
- **Density type change** reaches `canvas/GraphRenderer` and `ui/taskView`; a stale persisted `gitpulse_density_mode` must map `spacious→spacious`, `compact→compact`, default `comfortable`.
- **Contract test churn**: ~17 tests pin app.css text; each migration step names which ones it updates, and the rule is update-not-delete.
- **Sidebar overlay mode changes focus/measurement**: VirtualList/ResizeObserver behaviour under a hidden pane (memory) must be re-verified when the sidebar becomes absolutely positioned.
- **Private API stack**: native liquid glass would add a second private API, and the 120 Hz flag (decision 4) a third. The first already forfeits MAS.

Decisions for the human:
1. **Sidebar/dock default: overlay (fluid, compositor-only) or docked (content reflows, one-shot 200 ms layout animation)?** Recommendation: overlay for toggle/shortcut, docked while drag-resizing; both available in Layout.
2. **Wallpaper image behind the field: ship in v1 with a forced veil ≥ 0.7, or defer until contrast over arbitrary images is measured?** Recommendation: defer; the hue-rotating field delivers most of the wish at zero contrast risk.
3. **Native Liquid Glass (`apply_liquid_glass`, macOS 26+, private API) behind a flag, or stay on `underWindowBackground` only?** Recommendation: build the flag, ship it off, decide after the frame-time measurement; it is cheap to add and unmeasured to enable.
4. **120 Hz rendering: turn off WebKit's `PreferPageRenderingUpdatesNear60FPSEnabled` (private) so ProMotion displays get 120 Hz, or stay at 60?** Measured [verified, Wave 0 lane M]: WKWebView runs at 60 Hz on macOS 27.2 with a 120 Hz display, and turning the flag off gives 120 Hz. Float blur held 120 Hz; `width` animation got worse. Recommendation: an off-by-default setting beside the Liquid Glass flag. Build it only after a spike confirms wry lets us set the flag, and after Wave 2 removes layout animation. Fail closed to 60 Hz if the selector is missing.

(US vs UK spelling is recommended US and not escalated: the code already leans US.)
