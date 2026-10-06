/**
 * The harness's one owner for "a person is looking at this window".
 *
 * The product's polls stop while `readBackgroundDocument()` is true, and on
 * WKWebView that follows window focus, not visibility: `hasFocus()` is false
 * with `visibilityState` still "visible". The WebKit runner
 * (scripts/webkit-regressions.swift) is an accessory app whose activation is
 * cooperative. Its window may never become key while another app is in
 * front, or it becomes key and then loses it to another app mid-run. Either
 * way every poll-gated wait times out, and only in WebKit: headless Chrome
 * always reports focus.
 *
 * `holdForeground` covers both cases. It records focus through the product's
 * own record before mount (a dispatched `focus` straight after `mount()`
 * would be lost, because the component's effects have not attached their
 * listeners yet). After a real window blur it hands focus back on the next
 * tick. The blur still reaches every listener first, and this only undoes
 * it. A startup record alone is not enough: a real blur overwrites it and
 * nothing restores it, so the next poll-only wait stalls for good.
 *
 * `describeForeground` puts the product's background verdict, the inputs it
 * reads, the focus events seen, and the drift of a 25 ms timer on one line
 * for a timeout message. The runner hands back only the verdict, never the
 * console, so the timeout message is the only channel for this.
 */
import { noteForegroundFocus, readBackgroundDocument } from "../src/lib/runtime/foreground";

const PROBE_MS = 25;
const seen = { blurs: 0, focuses: 0, reasserts: 0 };
const probe = { ticks: 0, totalGapMs: 0, maxGapMs: 0, last: 0 };
let watching = false, holding = false;

/** Count window focus changes and measure timer drift. Call it before
 * `mount`, so a blur that lands during startup is counted. Idempotent. */
export function watchForeground(): void {
  if (watching) return;
  watching = true;
  window.addEventListener("blur", event => { if (event.target === window) seen.blurs++; });
  window.addEventListener("focus", event => { if (event.target === window) seen.focuses++; });
  probe.last = performance.now();
  const tick = () => {
    const now = performance.now(), gap = now - probe.last;
    probe.last = now;
    probe.ticks++;
    probe.totalGapMs += gap;
    probe.maxGapMs = Math.max(probe.maxGapMs, gap);
    setTimeout(tick, PROBE_MS);
  };
  setTimeout(tick, PROBE_MS);
}

/**
 * Model a person looking at the window for the whole run: record focus now,
 * and give it back after every real window blur. Call before `mount`. A
 * check that needs the background sets `document.hidden`, which outranks
 * both. Idempotent.
 */
export function holdForeground(): void {
  watchForeground();
  noteForegroundFocus(true);
  if (holding) return;
  holding = true;
  window.addEventListener("blur", event => {
    if (event.target !== window) return;
    setTimeout(() => {
      seen.reasserts++;
      // The record first, so the answer does not depend on a product
      // listener being attached at this instant. The event then wakes
      // whatever paused on the blur.
      noteForegroundFocus(true);
      window.dispatchEvent(new Event("focus"));
    }, 0);
  });
}

/**
 * Window focus events seen so far, real or re-asserted. A focus change wakes
 * the product's paused polls, which refresh at once, so a check that counts
 * fetches over a window compares this before and after to tell "idle" from
 * "woken by the window".
 */
export function focusChanges(): number {
  return seen.blurs + seen.focuses;
}

/** One line: the product's background verdict, the inputs it reads, the focus
 * events seen, and how late a 25 ms timer has been running. */
export function describeForeground(): string {
  let focused: string;
  try { focused = String(document.hasFocus()); } catch (error) { focused = `threw ${error}`; }
  const mean = probe.ticks ? (probe.totalGapMs / probe.ticks).toFixed(1) : "n/a";
  return [
    `background=${readBackgroundDocument()}`,
    `hasFocus=${focused}`,
    `hidden=${document.hidden}`,
    `visibility=${document.visibilityState}`,
    `blurs=${seen.blurs}`,
    `focuses=${seen.focuses}`,
    `reasserts=${holding ? seen.reasserts : "off"}`,
    watching ? `timer${PROBE_MS}ms ticks=${probe.ticks} meanGap=${mean}ms maxGap=${probe.maxGapMs.toFixed(0)}ms` : "timer probe not started",
  ].join(" ");
}
