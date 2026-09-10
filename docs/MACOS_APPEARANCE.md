# macOS appearance

GitPulse automatically applies a Mac appearance profile through `isMacOS()`
and the `html.macos` class. iPhones, iPads, and desktop-mode iPads are excluded.
Windows and Linux keep the standard appearance and transition timings.

## Materials and layout

The title bar, repository strip, sidebar, status bar, workspace plate,
floating menus, dialogs, and welcome card use a shared glass material. A
static hue field — four wide, saturated radial blobs spanning the whole shell
— is what the translucent surfaces show. The main workspace has a 16px rounded
outline and an 8px inset. Traffic-light spacing and native window dragging are
retained.

The Tasks board shares these materials in both global and repository views.
The navigator, toolbar, columns, task and workspace editors, and inbox use the
chrome tier. Cards, fields, notification settings, enhancement controls and
agent run details use the shared fills. The floating tier blurs the pointer
drag preview, automatic settings popup and sticky editor headers where they
overlap content. Cards do not each create a filter. Scope selection uses the
shared liquid transition, including reduced-motion preferences. Accessibility
fallbacks override both dark and light theme tokens.

Open `/harness/task-materials.html` with `npm run dev` for a disposable Tasks
preview with in-memory transport. **Check all Tasks surfaces** exercises both
themes, scope selection, editors, enhancements, run controls, automatic settings
and the inbox. `?repository=1` checks the repository mount. Browser media
emulation can exercise reduced transparency, increased contrast, forced colors
and reduced motion; the Platform control previews standard material fills.
The fixture does not access real task storage or launch agents.

Run `npm run test:browser -- --harness task-materials` for Chromium or
`npm run test:webkit -- --harness task-materials` for native macOS WKWebView.
Both run the same surface and interaction checks automatically.

The field sits over the real desktop rather than replacing it: window
transparency is on (see below), so what shows through a surface is the
blurred desktop plus this tint, not the tint alone.

### Two tiers, and why only one of them is blurred

| Tier | Surfaces | Backdrop filter |
| --- | --- | --- |
| Chrome | title bar, repository strip, sidebar, status bar, workspace plate | none |
| Float | anything wearing `shadow-pop` or `shadow-float`, plus `.gp-menu` | 34px blur, saturation and brightness boost |

The float tier is the **shadow**, not the card class. Written as
`.gp-card.shadow-float` it named the dialogs and silently missed every other
float: the commit tooltip carries `shadow-pop`, and the toasts, the coach mark
and the go-to-line popover carry `shadow-float` without `.gp-card`. The tooltip
was the one float in the application compositing with no blur at all, which
looks identical to a blur that is working until content moves behind it. Those
surfaces keep `bg-surface` rather than the thinner glass fill — a dialog card
sits on a dimmed scrim, while a tooltip sits directly on commit rows and
carries denser text.

A backdrop filter is paid per filtered surface at roughly area × radius, on
every composited frame. Chrome is laid out *beside* and *above* the content
panes and never over them, so the only thing in a chrome surface's backdrop is
the hue field — and blurring a smooth gradient returns the same smooth
gradient. Those four always-on, full-size filters were therefore paying for a
difference nobody can point at; the saturation they applied is baked into the
field's own colours instead, at no per-frame cost. Float surfaces do sit over
commit rows, diffs and tables, so for them the radius is the effect, and they
are the only tier that is filtered. Nothing is filtered until a floating surface or sticky editor header is
actually on screen.

### Stacking, and what is not stacking

Surfaces compound: a card on a pane on the plate inside the veil ends up denser
than a lone panel, and that is deliberate — depth does work a single global
alpha cannot. Repainting the *same* colour is not depth. A container that
paints `bg-background` inside another that already paints it is putting the
base colour on top of the base colour, which costs nothing while both are
opaque and darkens visibly once neither is.

The graph gutter was that case: it carried `bg-background` three lines below
the pane that already carried it, so it composited 8.2% of the desktop against
the commit list's 14.1% beside it, and read as a hard rectangle in the middle
of the glass. A nested base plate is therefore transparent on macOS, with one
exception — a `sticky`, `absolute` or `fixed` element sharing its parent's
colour is not repainting the ground, it is covering content that scrolls under
it. The diff's sticky gutter is exactly that. The exclusion list is derived
from the components rather than written down, so a new occluder idiom fails the
contract test instead of going transparent unnoticed.

The author-chosen alphas are the same arithmetic one rung down.
`bg-background/50` and `/60` are recesses — the base colour thinned against the
`--c-surface` panel around them, which opaque is a nine-unit colour step and
translucent is half a layer of coverage on top of three others. Measured over a
white desktop, the commit-details preview sat at `rgb(22, 26, 36)` against the
panel beside it at `rgb(34, 37, 50)`. Those become a shade, so the recess costs
a ninth of the coverage rather than half; every one of them keeps a border or a
layout edge doing the rest of the separating. `/80` and `/90` keep their fill,
because those are controls and floating fields where covering what is behind is
the point.

Overflow cues follow the same rule. `from-background to-transparent` is a
full-alpha stop, so the fades at the edges of a scroller were opaque bands on a
translucent pane — and Tailwind's `to-transparent` is `rgb(0 0 0 / 0)`, so the
ramp travelled through black and fringed on light themes. `.gp-edge-fade` fades
one colour to its own zero, and on macOS fades a shade rather than a colour,
because the gutter deliberately stopped painting a colour to fade from.

### The two surfaces CSS cannot reach

Everything above is one file: `src/app.css` owns the material, and every rule
here lands in it. Two surfaces paint themselves outside CSS and each needs a
token there plus a one-line adapter at its own boundary.

The **graph canvas** is a 2D context: `--bg-main` resolving to `transparent`
tells the renderer to take an alpha backing store, clear each frame rather than
fill it, and cut node haloes with `destination-out`.

The **terminal** is xterm.js, which paints its own background from a theme
object. `--bg-terminal` resolving to `transparent` makes it paint nothing, so
the surface is the plate its mount div already carries (`bg-surface`, which
thins and goes opaque with every other panel) — a second translucent fill from
the terminal would be the same double coverage the panes were cured of. The
token is deliberately not `--bg-surface`, which has a second runtime reader:
the graph's node *stroke*, and a stroke is not a fill.

Three things are load-bearing at that boundary, and none of them is cosmetic:

- `allowTransparency` must be set before `open()` and cannot be changed later.
- The colour must be **hex**. `css.toColor` in @xterm/xterm 6.0.0 parses
  `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa` itself and pushes everything else
  through a canvas probe that *throws* when the sampled alpha is not 255 —
  measured in WebKit, `rgba(20, 26, 41, 0.5)` samples alpha 128 and the
  `transparent` keyword samples 0, while `getComputedStyle` hands back exactly
  those forms. `hexColor()` re-spells them at the call site.
- xterm.css hard-codes `background-color: #000` on its viewport ("On OS X this
  is required in order for the scroll bar to appear fully opaque"). That black
  slab is why a transparent theme colour alone changes nothing on screen, and
  it is overridden for the same reason the terminal is translucent at all.

### One blur per dialog

`backdrop-filter` makes its element a **backdrop root**. A full-viewport scrim
that blurs therefore leaves the dialog card inside it sampling the scrim's own
flat wash rather than the application — the card pays for a filter whose input
is a solid colour. Every dialog scrim routes through the shared `.gp-scrim`
class, and on macOS that class turns its own filter off, so the card is the
only blurred surface and it blurs the real application behind it. Each dialog
keeps its own dim and alignment.

Verified in Chromium 148 with an `invert(1)` probe: an ancestor with
`transform: translateZ(0)`, `isolation: isolate`, `contain: layout paint` or
`will-change: transform` passes the backdrop through, while an ancestor with
`backdrop-filter`, `opacity < 1` or `filter` cuts it off. `WKWebView`'s
`takeSnapshot` does not composite `backdrop-filter` at all, so it cannot be
used to check this on WebKit; the equivalence was not re-measured there.

### Contrast

Thinner surfaces mean the field, not the surface colour, becomes the ground
that 11px muted text sits on, so the field is tuned against measured contrast
rather than by eye. Rendered in WKWebView at 1280×840 and sampled on empty
probe rects, `--c-text-muted` holds **4.86:1 – 6.82:1** on dark chrome and
**4.58:1 – 4.90:1** on light chrome, with body text at 11:1 or better on both.
Light is the tighter of the two — dark text on a near-white ground has less
headroom than light text on a near-black one — which is why the light theme
dilutes the field and thickens the fill.

**Both measurements were taken over a white desktop, which is the worst case
for the dark theme and the best case for the light one.** The light theme over
a *dark* desktop has not been measured. A CSS-only model of the stack puts
muted text at 3.5:1 on light chrome there, but that model omits both the
`NSVisualEffectView` and the hue field, each of which only adds opacity, so it
is a lower bound rather than a result: it can fail to prove safety and cannot
establish a failure. The same model puts dark chrome at 2.9:1 where the running
app measures 4.74:1, which is the size of the gap. Settling the light case
needs a measurement, not a tighter model. The dark blobs are deep and saturated
rather than pastel for the same reason: chroma reads as glass without raising
the luminance that muted text is measured against.

### Native window transparency

The window itself is transparent and an `NSVisualEffectView`
(`underWindowBackground`, `followsWindowActiveState`) blurs the **desktop**
behind it. The blur is done by the window server, so it costs the application
nothing per frame — unlike a CSS `backdrop-filter`, which the app pays for.

Three settings have to be present together, and any one alone is inert:

| Setting | Where | Without it |
| --- | --- | --- |
| `macos-private-api` feature | `Cargo.toml` **and** `macOSPrivateApi` in the **base** `tauri.conf.json` | The WKWebView stays opaque and covers the material |
| `"transparent": true` | `tauri.macos.conf.json` | Nothing to see through |
| `windowEffects` | `tauri.macos.conf.json` | The desktop shows through unblurred |

The feature gate is deeper than it looks: `tauri/macos-private-api` forwards
`wry/transparent`, and wry compiles its `setOpaque(false)` call only behind
that feature. The vibrancy itself is public API — `tauri::vibrancy` is not
feature-gated — but with an opaque webview on top there is no way to see it.

The pair has to be declared **symmetrically, on every platform**, which is why
`macOSPrivateApi` sits in the base config beside macOS-only neighbours. On each
platform it builds, `tauri-build` re-derives the Cargo features the merged
config implies and aborts when they differ from the ones declared on the
`[dependencies] tauri` entry — and it reads the first dependency table naming
the crate, so a target-scoped entry cannot make the manifest side vary either.
Put the key in `tauri.macos.conf.json` and macOS agrees with itself while Linux
and Windows fail at the build script; that is invisible to a macOS `ci:local`
run, and it cost the v0.0.6 release a pre-flight. Off macOS the feature changes
nothing — every guard in `tauri` reads `any(not(target_os = "macos"), feature =
"macos-private-api")`, and the feature body resolves to `wry`'s empty
`transparent` — so declaring it everywhere is free. `mac-material-contract`
re-derives that comparison per platform.

**This forfeits Mac App Store acceptance**, which is a product decision rather
than an implementation detail. It was previously disabled for exactly that
reason. See [Tauri window configuration](https://v2.tauri.app/reference/config/#windowconfig).

macOS-only settings live in `src-tauri/tauri.macos.conf.json` so Windows and
Linux builds keep the window configuration and wry feature set they had. Tauri
merges that file with JSON Merge Patch (RFC 7396), which **replaces** arrays
rather than merging them, so the file has to restate the whole `windows` entry
— `scripts/mac-material-contract.test.ts` derives the expected object from the
base config so the copy cannot drift.

### The veil

Once the desktop is the backdrop, the ground under 11px muted text is a
wallpaper nobody has seen. `--mac-shell-veil` keeps the shell at partial
opacity instead of fully clear, so the worst case is bounded rather than
unknown. Measured on the running app over a **pure white** full-screen
backdrop, dark theme: `--c-text-muted` held **4.74:1 – 7.23:1** and body text
**10.5:1 or better**. The `underWindowBackground` material darkens heavily in
dark appearance, and the veil finishes the job.

## Motion and rendering

| Interaction | Mac behavior |
| --- | --- |
| Main view selection | A decorative pill crossfades between buttons over 280ms; labels and focus targets stay stationary |
| Dialog entrance | 260ms cubic easing, starting at 98.5% scale |
| Dialog exit | Existing 60ms exit, with a matching 98.5% scale |
| View/popover entrance | 260ms easing; animation releases its transform after completion |
| Button press | 180ms transition, 1px depression and 97% scale |

Svelte's built-in crossfade owns shared-element measurement, interruption,
and cleanup. The new animated movement uses transform and opacity, which
allow the webview compositor to accelerate it. Blur radii are static; there
is no new canvas, animation loop, dependency, or global `will-change` rule.
The Mac selection pill also reuses the existing `gp-gpu` compositing hint;
that layer is limited to the small decorative selection surface.
The application does not force a GPU driver or claim that hardware
acceleration is available on every machine. Chromium's existing launch flags
are Windows-specific and do not configure WKWebView.

The repository pane is not re-keyed on every view switch: that would
recreate the content and replay a full-pane fade. Modal exits remain shorter
than entrances to avoid stacking dimmed backdrops when rapidly reopened.
See [Svelte transitions](https://svelte.dev/docs/svelte/transition) and
[crossfade](https://svelte.dev/docs/svelte/svelte-transition#crossfade).

## Accessibility and fallbacks

- Unsupported backdrop filtering gets a solid surface by default.
- Reduced transparency, increased contrast, and forced colors remove glass
  and ambient gradients, and repaint the shell opaque. Window transparency is
  fixed at build time and cannot be withdrawn at runtime, so honouring the
  preference means the page covers the material rather than removing it. OS
  preference detection depends on the webview's support for these media
  features.
- Reduced motion disables CSS movement and returns zero durations for
  Svelte transitions, including exits. Preferences are checked for each
  transition rather than captured once at startup. The liquid selector uses
  a duration callback so an already-mounted pill also reads the latest setting.
- The selection pill is decorative and ignores pointer events. Buttons keep
  their names, selected state, and a visible keyboard focus outline.
- Existing theme text/status tokens and dense content backgrounds are retained.

## Verification

`scripts/mac-material-contract.test.ts` pins the material rules against the
source: every full-screen scrim routes through `.gp-scrim` and none re-adds a
`backdrop-blur-*` utility beside it, the only filtered selector inside the
`@supports` block is the float tier, a nested base plate is transparent, every
positioning keyword that actually appears beside `bg-background` is exempted
from that rule, and no component fades an edge from a full-alpha surface
colour. Two of those assertions are discovery-based and check that they found
anything at all — written without it, the scrim rules passed against zero
matches the moment the dialogs adopted the class.

`src/lib/ui/macAppearance.test.ts` covers platform boundaries, Mac transition
timings, changing reduced-motion preferences, and rendered selection semantics.
The iOS cases and Mac entrance/selection cases failed before implementation.
Live component verification also caught and corrected a stale outgoing
duration after changing Reduce Motion; the regression test pins late duration
resolution, and browser checks verified rapid-switch cleanup and zero selector
movement after the preference change.
`transitions.test.ts` explicitly pins the standard platform profile so host
Node versions exposing `navigator.platform` cannot change its expectations.

Run `npm run ci:local` for the full repository gate. Rendered checks should
cover dark/light themes, 900px and 1280px windows, modal reopening, keyboard
focus, rapid view selection, and supported accessibility preferences. Browser
emulation can verify CSS fallbacks; it does not prove macOS system preference
propagation or GPU frame timing on physical hardware.
