/**
 * The page's stylesheet, as a string the shell injects once.
 *
 * Kept in TypeScript so the bundle is one file and nothing needs a CSS loader.
 * Light and dark come from `prefers-color-scheme`; every colour is a custom
 * property declared once per scheme, and the canvas theme reads the same
 * properties (`themeFromCss`). Surfaces are told apart by tonal steps rather
 * than hairline borders, controls are pills, labels are sentence case, motion
 * is transform and opacity only (menus fade and slide in), and `prefers-reduced-motion` turns it off.
 * No network, no web fonts: the system stack only.
 */

export const PAGE_CSS = `
:root {
  color-scheme: light dark;
  --cm-bg: #f5f6f8;
  --cm-surface: #ffffff;
  --cm-surface-2: #eceff4;
  --cm-surface-3: #e1e6ee;
  --cm-text: #1a2130;
  --cm-muted: #596378;
  --cm-accent: #2160d9;
  --cm-accent-soft: rgba(33, 96, 217, 0.14);
  --cm-warn: #9a5b00;
  --cm-shadow: 0 8px 28px rgba(20, 30, 50, 0.16);
  --cm-canvas: #ffffff;
  --cm-plate: #ffffff;
  --cm-label: rgba(26, 33, 48, 0.9);
  --cm-link: rgba(60, 80, 120, 0.4);
  --cm-link-muted: rgba(60, 80, 120, 0.3);
  --cm-field: #5b6b8a;
  --cm-hover: #b8700a;
  --cm-dead: #c93545;
  --cm-unwired: #b8700a;
  --cm-entry: #12875f;
  --cm-font: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  --cm-mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
@media (prefers-color-scheme: dark) {
  :root {
    --cm-bg: #0f131a;
    --cm-surface: #171c26;
    --cm-surface-2: #1f2633;
    --cm-surface-3: #293245;
    --cm-text: #e7ebf3;
    --cm-muted: #98a3b8;
    --cm-accent: #6aa6ff;
    --cm-accent-soft: rgba(106, 166, 255, 0.18);
    --cm-warn: #f0b24f;
    --cm-shadow: 0 8px 28px rgba(0, 0, 0, 0.5);
    --cm-canvas: #131822;
    --cm-plate: #171e2b;
    --cm-label: rgba(205, 213, 230, 0.94);
    --cm-link: rgba(150, 165, 195, 0.5);
    --cm-link-muted: rgba(150, 165, 195, 0.32);
    --cm-field: #94a3b8;
    --cm-hover: #f0ad4e;
    --cm-dead: #ef6b7a;
    --cm-unwired: #f0ad4e;
    --cm-entry: #34d399;
  }
}
* { box-sizing: border-box; }
html, body { height: 100%; }
body {
  margin: 0;
  font: 13px/1.5 var(--cm-font);
  background: var(--cm-bg);
  color: var(--cm-text);
  overflow: hidden;
}
button, input, select { font: inherit; color: inherit; }
button { cursor: pointer; }
button:disabled { cursor: default; opacity: 0.45; }
:focus-visible { outline: 2px solid var(--cm-accent); outline-offset: 2px; }
[hidden] { display: none !important; }
h1, h2, h3, p, ul { margin: 0; }
ul { padding: 0; list-style: none; }
.cm-muted { color: var(--cm-muted); font-size: 12px; }
.cm-grow { flex: 1; min-width: 0; }
.cm-trunc { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.cm-count { opacity: 0.6; font-variant-numeric: tabular-nums; }

/* Page frame */
#app { height: 100%; display: flex; min-width: 0; }
.page { flex: 1; display: flex; min-width: 0; min-height: 0; }
.page-side { width: min(420px, 38%); flex: none; display: flex; flex-direction: column; min-height: 0; background: var(--cm-surface); }
.page-main { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
.side-scroll { flex: 1; min-height: 0; overflow: auto; padding: 4px 16px 16px; }
.side-head { padding: 16px 16px 8px; }
.side-head h1 { font-size: 16px; font-weight: 650; }
.side-block { margin-top: 16px; }
.side-block > h2 { font-size: 12px; font-weight: 600; color: var(--cm-muted); margin-bottom: 6px; }
.side-foot { padding: 10px 16px; background: var(--cm-surface-2); font-size: 12px; display: flex; justify-content: space-between; gap: 8px; }
.side-foot a { color: var(--cm-accent); text-decoration: none; }
.side-foot a:hover { text-decoration: underline; }
.stats { display: grid; grid-template-columns: repeat(4, 1fr); gap: 6px; margin-top: 10px; }
.stat { background: var(--cm-surface-2); border-radius: 10px; padding: 8px 6px; text-align: center; }
.stat b { display: block; font-size: 15px; font-variant-numeric: tabular-nums; }
.stat span { font-size: 11px; color: var(--cm-muted); }
.about { margin-top: 12px; background: var(--cm-surface-2); border-radius: 10px; padding: 8px 12px; color: var(--cm-muted); font-size: 12px; }
.about summary { cursor: pointer; color: var(--cm-text); font-weight: 600; }
.about p { margin-top: 8px; }
.langbar { display: flex; height: 10px; border-radius: 6px; overflow: hidden; gap: 2px; background: var(--cm-surface-2); }
.langbar span { display: block; height: 100%; min-width: 2px; }
.langlegend { display: flex; flex-wrap: wrap; gap: 4px 6px; margin-top: 10px; }
.langchip { display: inline-flex; align-items: center; gap: 6px; padding: 2px 10px 2px 8px; border: 0; border-radius: 999px; background: var(--cm-surface-2); font-size: 12px; }
.langchip:hover { background: var(--cm-surface-3); }
.langchip[aria-pressed="true"] { background: var(--cm-accent-soft); }
.langchip .pct { color: var(--cm-muted); font-variant-numeric: tabular-nums; }
.dot-swatch { width: 9px; height: 9px; border-radius: 50%; flex: none; }
.dot-swatch.unofficial { box-shadow: inset 0 0 0 1px var(--cm-muted); }
.note { margin-top: 8px; font-size: 12px; color: var(--cm-muted); }
.note strong { color: var(--cm-text); font-variant-numeric: tabular-nums; }
.badges { display: flex; flex-wrap: wrap; gap: 6px; }
.badge { display: inline-flex; gap: 5px; padding: 2px 10px; border-radius: 999px; background: var(--cm-surface-2); font-size: 12px; color: var(--cm-muted); font-variant-numeric: tabular-nums; }
.badge b { color: var(--cm-text); font-weight: 600; }
.badge.warn { color: var(--cm-warn); }

/* Navigator */
.nav { display: flex; flex-direction: column; gap: 10px; }
.nav-side { display: flex; flex-direction: column; gap: 8px; }
.nav-filter, .cm-search-input { width: 100%; min-width: 0; padding: 6px 12px; border: 0; border-radius: 999px; background: var(--cm-surface-2); }
.nav-filter::placeholder, .cm-search-input::placeholder { color: var(--cm-muted); }
.nav-tree { max-height: 34vh; overflow: auto; display: flex; flex-direction: column; gap: 1px; padding: 2px; }
.nav-row { display: flex; align-items: flex-start; gap: 4px; padding-block: 6px; padding-inline-end: 8px; border-radius: 8px; cursor: pointer; color: var(--cm-muted); }
.nav-row:hover { background: var(--cm-surface-2); }
.nav-row.is-on { background: var(--cm-accent-soft); color: var(--cm-text); }
.nav-chevron { width: 14px; flex: none; text-align: center; font-size: 11px; line-height: 20px; }
.nav-row-copy { min-width: 0; flex: 1; display: flex; flex-direction: column; }
.nav-row-line { display: flex; gap: 6px; align-items: center; }
.nav-row-label { font-family: var(--cm-mono); font-size: 12px; min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.nav-row-summary { font-size: 11px; opacity: 0.8; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.nav-badge { font-size: 11px; padding: 0 6px; border-radius: 999px; background: var(--cm-surface-3); }
.nav-empty { padding: 8px; color: var(--cm-muted); }
.nav-detail { display: flex; flex-direction: column; gap: 14px; padding-top: 6px; }
.nav-detail-head { display: flex; gap: 10px; align-items: flex-start; justify-content: space-between; }
.nav-detail-title { min-width: 0; }
.nav-detail-title h2 { font-family: var(--cm-mono); font-size: 13px; overflow-wrap: anywhere; }
.nav-h { font-size: 12px; font-weight: 600; color: var(--cm-muted); margin-bottom: 4px; }
.nav-list, .nav-plain { display: flex; flex-direction: column; gap: 3px; font-size: 12px; }
.nav-plain li { overflow-wrap: anywhere; }
.nav-chips { display: flex; flex-wrap: wrap; gap: 4px; }
.nav-path { display: inline-block; max-width: 100%; padding: 2px 10px; border: 0; border-radius: 999px; background: var(--cm-surface-2); font: 12px var(--cm-mono); text-align: start; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.nav-path:hover { background: var(--cm-surface-3); }
.nav-chip-off { display: inline-block; padding: 2px 10px; border-radius: 999px; font: 12px var(--cm-mono); color: var(--cm-muted); box-shadow: inset 0 0 0 1px var(--cm-surface-3); }
.nav-strip { display: flex; height: 8px; border-radius: 5px; overflow: hidden; gap: 1px; margin-bottom: 4px; background: var(--cm-surface-2); }
.nav-strip span { display: block; height: 100%; min-width: 2px; }

/* Map */
.cm { display: flex; flex-direction: column; flex: 1; min-width: 0; min-height: 0; background: var(--cm-bg); }
.cm-toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; padding: 14px 20px 10px; }
.cm-heading { min-width: 0; margin-inline-end: auto; }
.cm-caption { display: block; font-size: 12px; font-weight: 500; color: var(--cm-muted); }
.cm-title { font-size: 15px; font-weight: 650; overflow-wrap: anywhere; }
.cm-counts { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin-top: 3px; font-size: 12px; color: var(--cm-muted); }
.cm-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; min-width: 0; }
.cm-search { display: flex; align-items: center; min-width: 0; }
.cm-search-input { width: 200px; }
.cm-btn { padding: 4px 12px; border: 0; border-radius: 999px; background: var(--cm-surface-2); }
.cm-btn:hover:not(:disabled) { background: var(--cm-surface-3); }
.cm-btn.is-on { background: var(--cm-accent-soft); color: var(--cm-accent); }
.cm-btn-small { padding: 1px 10px; font-size: 12px; }
.cm-icon-btn { width: 28px; height: 28px; border: 0; border-radius: 999px; background: transparent; font-size: 15px; line-height: 1; }
.cm-icon-btn:hover:not(:disabled) { background: var(--cm-surface-2); }
.cm-segmented { display: inline-flex; padding: 2px; border-radius: 999px; background: var(--cm-surface-2); }
.cm-seg { padding: 2px 12px; border: 0; border-radius: 999px; background: transparent; color: var(--cm-muted); }
.cm-seg.is-on { background: var(--cm-surface); color: var(--cm-text); box-shadow: 0 1px 3px rgba(0, 0, 0, 0.18); }
.cm-filters { position: relative; }
.cm-menu { position: absolute; z-index: 20; right: 0; top: calc(100% + 6px); width: 264px; max-height: 70vh; overflow-y: auto; padding: 6px; border-radius: 14px; background: var(--cm-surface); box-shadow: var(--cm-shadow); font-size: 12px; }
.cm-menu-item { display: flex; align-items: center; gap: 8px; width: 100%; padding: 6px 10px; border: 0; border-radius: 8px; background: transparent; text-align: start; cursor: pointer; }
.cm-menu-item:hover { background: var(--cm-surface-2); }
.cm-menu-item[aria-pressed="true"] { color: var(--cm-accent); }
.cm-menu-sep { height: 6px; }
.cm-note { padding: 0 20px 8px; color: var(--cm-warn); font-size: 12px; }
.cm-ring-legend { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 14px; padding: 0 20px 8px; font-size: 12px; }
.cm-ring-item { display: inline-flex; align-items: center; gap: 6px; color: var(--cm-muted); }
.cm-ring { width: 10px; height: 10px; border: 2px solid; border-radius: 50%; }
.cm-group-row { display: flex; min-width: 0; }
.cm-chips { display: flex; flex: 1; min-width: 0; gap: 6px; overflow-x: auto; padding: 0 20px 10px; scrollbar-width: thin; }
.cm-chip { display: inline-flex; flex: none; max-width: 220px; align-items: center; gap: 6px; padding: 2px 12px; border: 0; border-radius: 999px; background: transparent; color: var(--cm-muted); font-size: 12px; }
.cm-chip:hover { background: var(--cm-surface-2); }
.cm-chip.is-on { background: var(--cm-surface-2); color: var(--cm-text); }
.cm-pager { display: flex; flex: none; align-items: center; gap: 4px; padding: 0 12px 10px 0; font-size: 12px; color: var(--cm-muted); }
.cm-stage { position: relative; flex: 1; min-height: 240px; overflow: hidden; background: var(--cm-canvas); }
.cm-canvas { position: absolute; inset: 0; width: 100%; height: 100%; touch-action: none; cursor: grab; }
.cm-canvas:active { cursor: grabbing; }
.cm-panel { position: absolute; top: 8px; max-height: calc(100% - 72px); overflow: auto; padding: 8px; border-radius: 14px; background: var(--cm-surface); box-shadow: var(--cm-shadow); font-size: 12px; }
.cm-results { left: 16px; width: min(320px, calc(100% - 32px)); }
.cm-neighbors { right: 16px; width: 308px; }
.cm-panel-head { padding: 6px 8px; font-size: 12px; color: var(--cm-muted); }
.cm-between { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.cm-panel-note { padding: 6px 8px; font-size: 12px; color: var(--cm-muted); }
.cm-trace { display: flex; gap: 6px; padding: 0 8px; }
.cm-select { flex: 1; min-width: 0; padding: 3px 8px; border: 0; border-radius: 999px; background: var(--cm-surface-2); font-size: 12px; }
.cm-row { display: flex; width: 100%; align-items: flex-start; gap: 8px; padding: 6px 8px; border: 0; border-radius: 8px; background: transparent; text-align: start; }
.cm-row:hover, .cm-row.is-on { background: var(--cm-surface-2); }
.cm-row-copy { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.cm-row-copy strong { font-weight: 500; }
.cm-row-copy strong, .cm-row-copy small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.cm-row-copy small { color: var(--cm-muted); font-size: 11px; }
.cm-dot { width: 8px; height: 8px; margin-top: 5px; border-radius: 50%; flex: none; display: inline-block; }
.cm-pagination { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 6px 4px; color: var(--cm-muted); }
.cm-empty { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 6px; color: var(--cm-muted); pointer-events: none; text-align: center; padding: 24px; }
.cm-zoom { position: absolute; right: 16px; bottom: 16px; display: flex; align-items: center; gap: 2px; padding: 4px; border-radius: 999px; background: var(--cm-surface); box-shadow: var(--cm-shadow); }
.cm-zoom-readout { min-width: 44px; text-align: center; font-size: 12px; color: var(--cm-muted); font-variant-numeric: tabular-nums; }
.cm-help { position: absolute; left: 20px; bottom: 22px; font-size: 12px; color: var(--cm-muted); pointer-events: none; }
.cm-live { position: absolute; left: -9999px; }
.cm-inspector { display: flex; align-items: center; flex-wrap: wrap; gap: 6px 12px; min-height: 52px; padding: 10px 20px; background: var(--cm-surface); font-size: 12px; }
.cm-inspector .cm-dot { margin-top: 0; }
.cm-inspector-copy { display: flex; flex: 1; flex-direction: column; min-width: 160px; }
.cm-inspector-copy strong { font-weight: 500; }
.cm-inspector-copy strong, .cm-inspector-copy span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.cm-inspector-copy span { color: var(--cm-muted); font-size: 11px; }
.cm-detail { max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.page-error { margin: auto; max-width: 520px; padding: 32px; color: var(--cm-muted); text-align: center; }
.page-error strong { color: var(--cm-text); display: block; margin-bottom: 6px; font-size: 15px; }

/* One column on a narrow window */
@media (max-width: 860px) {
  body { overflow: auto; }
  .page { flex-direction: column; }
  .page-side { width: auto; max-height: 46vh; }
  .page-main { min-height: 70vh; }
  .cm-help { display: none; }
  .cm-neighbors { left: 16px; width: auto; max-height: 38%; }
}
@media (prefers-reduced-motion: no-preference) {
  .cm-menu, .cm-panel { animation: cm-pop 120ms ease-out; }
}
@keyframes cm-pop { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: none; } }
`;
