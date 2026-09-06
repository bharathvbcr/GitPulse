# macOS appearance

GitPulse automatically applies a Mac appearance profile through `isMacOS()`
and the `html.macos` class. iPhones, iPads, and desktop-mode iPads are excluded.
Windows and Linux keep the standard appearance and transition timings.

## Materials and layout

The title bar, repository strip, sidebar, status bar, workspace plate,
floating menus, dialogs, and welcome card use a shared glass material. A
static hue field — four wide, saturated radial blobs spanning the whole shell
— is what the translucent surfaces show. The main workspace has a 16px rounded
outline and an 8px inset; code, diffs, tables, and graph canvases retain their
opaque backgrounds and existing virtualization. Traffic-light spacing and
native window dragging are retained.

The field sits over the real desktop rather than replacing it: window
transparency is on (see below), so what shows through a surface is the
blurred desktop plus this tint, not the tint alone.

### Two tiers, and why only one of them is blurred

| Tier | Surfaces | Backdrop filter |
| --- | --- | --- |
| Chrome | title bar, repository strip, sidebar, status bar, workspace plate | none |
| Float | menus, popovers, toasts, dialog cards | 34px blur, saturation and brightness boost |

A backdrop filter is paid per filtered surface at roughly area × radius, on
every composited frame. Chrome is laid out *beside* and *above* the content
panes and never over them, so the only thing in a chrome surface's backdrop is
the hue field — and blurring a smooth gradient returns the same smooth
gradient. Those four always-on, full-size filters were therefore paying for a
difference nobody can point at; the saturation they applied is baked into the
field's own colours instead, at no per-frame cost. Float surfaces do sit over
commit rows, diffs and tables, so for them the radius is the effect, and they
are the only tier that is filtered. Nothing is filtered until a menu, toast or
dialog is actually on screen.

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
dilutes the field and thickens the fill. The dark blobs are deep and saturated
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
| `macos-private-api` feature | `Cargo.toml`, macOS target only | The WKWebView stays opaque and covers the material |
| `"transparent": true` | `tauri.macos.conf.json` | Nothing to see through |
| `windowEffects` | `tauri.macos.conf.json` | The desktop shows through unblurred |

The feature gate is deeper than it looks: `tauri/macos-private-api` forwards
`wry/transparent`, and wry compiles its `setOpaque(false)` call only behind
that feature. The vibrancy itself is public API — `tauri::vibrancy` is not
feature-gated — but with an opaque webview on top there is no way to see it.

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

`scripts/mac-material-contract.test.ts` pins both material rules against the
source: every full-screen scrim routes through `.gp-scrim` and none re-adds a
`backdrop-blur-*` utility beside it, and the only filtered selector inside the
`@supports` block is the float tier. Its first assertion checks that the scrim
discovery found anything at all — written without it, the scrim rules passed
against zero matches the moment the dialogs adopted the class.

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
