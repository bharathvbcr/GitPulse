# macOS appearance

GitPulse automatically applies a Mac appearance profile through `isMacOS()`
and the `html.macos` class. iPhones, iPads, and desktop-mode iPads are excluded.
Windows and Linux keep the standard appearance and transition timings.

## Materials and layout

The title bar, repository strip, sidebar, status bar, floating menus, dialogs,
and welcome card use a shared glass material. A static blue/teal ambient
background gives the blur content to sample. Glass uses a 20px backdrop blur,
135% saturation, a subtle highlight, and separate dark/light fill tokens.
The main workspace has a 16px rounded outline and an 8px inset; code, diffs,
tables, and graph canvases retain their opaque backgrounds and existing
virtualization. Traffic-light spacing and native window dragging are retained.

This is **in-app glass**. It blurs the interface behind a surface, not the
desktop behind the window. Tauri's native window transparency requires its
macOS private API feature, which prevents Mac App Store acceptance. That
feature remains disabled. See [Tauri window configuration](https://v2.tauri.app/reference/config/#windowconfig).

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
  and ambient gradients. OS preference detection depends on the webview's
  support for these media features.
- Reduced motion disables CSS movement and returns zero durations for
  Svelte transitions, including exits. Preferences are checked for each
  transition rather than captured once at startup. The liquid selector uses
  a duration callback so an already-mounted pill also reads the latest setting.
- The selection pill is decorative and ignores pointer events. Buttons keep
  their names, selected state, and a visible keyboard focus outline.
- Existing theme text/status tokens and dense content backgrounds are retained.

## Verification

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
