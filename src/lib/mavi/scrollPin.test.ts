import { describe, expect, it } from "vitest";
import {
  JUMP_PROGRESS_EXTEND_MS,
  JUMP_TIMEOUT_MS,
  PILL_HIDE_THRESHOLD_PX,
  PILL_SHOW_THRESHOLD_PX,
  PROGRAMMATIC_SUPPRESS_MS,
  REPIN_THRESHOLD_PX,
  USER_SCROLL_COOLDOWN_MS,
  createScrollPinState,
  markProgrammaticScroll,
  onJumpTimeout,
  onPointerEnd,
  onScroll,
  onScrollEnd,
  onUserIntent,
  setAutoScroll,
  shouldAutoScrollOnResize,
  shouldShowLatestPill,
  startJump,
  syncMetrics,
} from "./scrollPin";

const bottom = (scrollHeight = 2000, clientHeight = 600) => ({
  scrollTop: scrollHeight - clientHeight,
  scrollHeight,
  clientHeight,
});

describe("scrollPin", () => {
  it("unpins immediately on upward scroll and enforces cooldown against rubber-band re-pin", () => {
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    expect(s.pinned).toBe(true);

    // User scrolls up by 6 px (within 12 px).
    s = onScroll(s, { scrollTop: 1394, scrollHeight: 2000, clientHeight: 600 }, 1000, true);
    expect(s.pinned).toBe(false);
    expect(s.cooldownUntil).toBe(1000 + USER_SCROLL_COOLDOWN_MS);

    // Rubber-band bounce back downward to distance 4 px within cooldown must NOT re-pin.
    s = onScroll(s, { scrollTop: 1396, scrollHeight: 2000, clientHeight: 600 }, 1050, true);
    expect(s.pinned).toBe(false);

    // Still within cooldown at 179 ms -> stays unpinned even at distance 0.
    s = onScroll(s, { scrollTop: 1400, scrollHeight: 2000, clientHeight: 600 }, 1179, true);
    expect(s.pinned).toBe(false);

    // Later, user scrolls up slightly to 1385 (at t=1200, setting cooldownUntil=1380),
    // then after cooldown expires (t=1400), scrolling downward into <= 12 px re-pins.
    s = onScroll(s, { scrollTop: 1385, scrollHeight: 2000, clientHeight: 600 }, 1200, true);
    expect(s.pinned).toBe(false);
    s = onScroll(s, { scrollTop: 1392, scrollHeight: 2000, clientHeight: 600 }, 1400, true);
    expect(s.distanceFromBottom).toBe(8);
    expect(s.pinned).toBe(true);
  });

  it("ignores tiny <= 2 px jitter and clamps on content shrink while pinned", () => {
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });

    // 2 px jitter does not unpin.
    s = onScroll(s, { scrollTop: 1398, scrollHeight: 2000, clientHeight: 600 }, 100, true);
    expect(s.pinned).toBe(true);

    // Content shrinks from 2000 to 1920 while pinned at bottom (browser clamps scrollTop to 1320).
    s = onScroll(s, { scrollTop: 1320, scrollHeight: 1920, clientHeight: 600 }, 150, true);
    expect(s.pinned).toBe(true);
    expect(s.distanceFromBottom).toBe(0);

    // clientHeight grows (e.g. composer closes, 600 -> 640) while scrollHeight stays 1920:
    // maxScrollTop shrinks (1320 -> 1280) and browser clamps scrollTop to 1280 -> stays pinned!
    s = onScroll(s, { scrollTop: 1280, scrollHeight: 1920, clientHeight: 640 }, 200, true);
    expect(s.pinned).toBe(true);
    expect(s.distanceFromBottom).toBe(0);
    expect(s.lastClientHeight).toBe(640);
  });

  it("unpins immediately on upward wheel and keyboard intents", () => {
    for (const intent of [
      { kind: "wheel", deltaY: -4 } as const,
      { kind: "key", key: "PageUp" } as const,
      { kind: "key", key: "ArrowUp" } as const,
      { kind: "key", key: "Home" } as const,
    ]) {
      let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom() });
      s = onUserIntent(s, intent, 500);
      expect(s.pinned).toBe(false);
      expect(s.cooldownUntil).toBe(500 + USER_SCROLL_COOLDOWN_MS);
    }

    // Downward wheel does not unpin by itself.
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom() });
    s = onUserIntent(s, { kind: "wheel", deltaY: 12 }, 500);
    expect(s.pinned).toBe(true);
  });

  it("tracks pointer/scrollbar drag: blocks auto-scroll while dragging and unpins when dragged away from bottom", () => {
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    s = onUserIntent(s, { kind: "pointer" }, 100);
    expect(s.pointerActive).toBe(true);
    expect(shouldAutoScrollOnResize(s, true, true, true)).toBe(false);

    // Scrollbar drag moves > 12 px away from bottom -> unpins immediately.
    s = onScroll(s, { scrollTop: 1350, scrollHeight: 2000, clientHeight: 600 }, 150, true);
    expect(s.pinned).toBe(false);
    expect(s.pointerActive).toBe(true);
    expect(shouldAutoScrollOnResize(s, true, true, true)).toBe(false);

    // Releasing pointer above 12 px clears pointerActive and leaves pinned=false.
    s = onPointerEnd(s, { scrollTop: 1350, scrollHeight: 2000, clientHeight: 600 }, 200, true);
    expect(s.pointerActive).toBe(false);
    expect(s.pinned).toBe(false);

    // Dragging scrollbar down to <= 12 px and releasing re-pins immediately and clears cooldown.
    s = onUserIntent(s, { kind: "pointer" }, 220);
    s = onScroll(s, { scrollTop: 1392, scrollHeight: 2000, clientHeight: 600 }, 240, true);
    s = onPointerEnd(s, { scrollTop: 1392, scrollHeight: 2000, clientHeight: 600 }, 260, true);
    expect(s.pointerActive).toBe(false);
    expect(s.pinned).toBe(true);
    expect(s.cooldownUntil).toBe(0);
  });

  it("clamps macOS bottom rubber-band overscroll so settling back to maxScrollTop never unpins", () => {
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    // Overscroll past maxScrollTop (1400) to 1432, then settle back to 1400.
    s = onScroll(s, { scrollTop: 1432, scrollHeight: 2000, clientHeight: 600 }, 100, true);
    expect(s.pinned).toBe(true);
    expect(s.lastScrollTop).toBe(1400);
    s = onScroll(s, { scrollTop: 1400, scrollHeight: 2000, clientHeight: 600 }, 150, true);
    expect(s.pinned).toBe(true);
    expect(s.distanceFromBottom).toBe(0);
  });

  it("re-pins only when scrolling downward inside <= 12 px", () => {
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    s = onScroll(s, { scrollTop: 1300, scrollHeight: 2000, clientHeight: 600 }, 100, true);
    expect(s.pinned).toBe(false);

    // Downward to 20 px from bottom -> still unpinned (threshold is 12 px).
    s = onScroll(s, { scrollTop: 1380, scrollHeight: 2000, clientHeight: 600 }, 500, true);
    expect(s.pinned).toBe(false);

    // Downward inside <= 12 px after cooldown -> re-pins.
    s = onScroll(s, { scrollTop: 1389, scrollHeight: 2000, clientHeight: 600 }, 600, true);
    expect(s.distanceFromBottom).toBe(11);
    expect(s.pinned).toBe(true);
  });

  it("enforces 96px/24px deadband hysteresis on pillVisible", () => {
    // Does not overflow -> never shows pill.
    let fit = createScrollPinState({ autoScroll: true, initialMetrics: { scrollTop: 0, scrollHeight: 500, clientHeight: 600 } });
    fit = onUserIntent(fit, { kind: "wheel", deltaY: -10 }, 100);
    expect(fit.overflows).toBe(false);
    expect(fit.pillVisible).toBe(false);
    expect(shouldShowLatestPill(fit)).toBe(false);

    // Overflows, unpinned, at 96 px from bottom -> pill stays hidden (must exceed 96 px).
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    s = onScroll(s, { scrollTop: 1400 - PILL_SHOW_THRESHOLD_PX, scrollHeight: 2000, clientHeight: 600 }, 100, true);
    expect(s.pinned).toBe(false);
    expect(s.distanceFromBottom).toBe(PILL_SHOW_THRESHOLD_PX);
    expect(s.pillVisible).toBe(false);
    expect(shouldShowLatestPill(s)).toBe(false);

    // Crosses 97 px from bottom -> pill becomes visible.
    s = onScroll(s, { scrollTop: 1400 - (PILL_SHOW_THRESHOLD_PX + 1), scrollHeight: 2000, clientHeight: 600 }, 150, true);
    expect(s.pillVisible).toBe(true);
    expect(shouldShowLatestPill(s)).toBe(true);

    // Scrolling back down into deadband (e.g. 50 px, or 25 px > 24 px) keeps pill visible (no flicker).
    s = onScroll(s, { scrollTop: 1400 - 50, scrollHeight: 2000, clientHeight: 600 }, 400, true);
    expect(s.pillVisible).toBe(true);
    s = onScroll(s, { scrollTop: 1400 - (PILL_HIDE_THRESHOLD_PX + 1), scrollHeight: 2000, clientHeight: 600 }, 450, true);
    expect(s.pillVisible).toBe(true);

    // Crossing <= 24 px hides pill cleanly.
    s = onScroll(s, { scrollTop: 1400 - PILL_HIDE_THRESHOLD_PX, scrollHeight: 2000, clientHeight: 600 }, 500, true);
    expect(s.pillVisible).toBe(false);
    expect(shouldShowLatestPill(s)).toBe(false);

    // Moving back up into deadband (e.g. 80 px) keeps pill hidden until > 96 px again.
    s = onScroll(s, { scrollTop: 1400 - 80, scrollHeight: 2000, clientHeight: 600 }, 550, true);
    expect(s.pillVisible).toBe(false);
  });

  it("handles jump() completion, user cancellation, scrollend, timeout, and long jump progress extension", () => {
    // Complete at bottom via scroll event.
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom(4000, 600) });
    s = onScroll(s, { scrollTop: 800, scrollHeight: 4000, clientHeight: 600 }, 100, true);
    expect(shouldShowLatestPill(s)).toBe(true);
    s = startJump(s, 200);
    expect(s.jumping).toBe(true);
    expect(s.pillVisible).toBe(false);
    expect(shouldShowLatestPill(s)).toBe(false);
    expect(s.jumpExpiresAt).toBe(200 + JUMP_TIMEOUT_MS);

    // Downward progress at t=600 extends jumpExpiresAt so long WKWebView smooth scrolls don't abort at 650 ms.
    s = onScroll(s, { scrollTop: 1800, scrollHeight: 4000, clientHeight: 600 }, 600, true);
    expect(s.jumping).toBe(true);
    expect(s.jumpExpiresAt).toBe(600 + JUMP_PROGRESS_EXTEND_MS);

    // Even at t=900 (past initial 850 ms), jump is still active because progress extended it to 950 ms.
    s = onScroll(s, { scrollTop: 2800, scrollHeight: 4000, clientHeight: 600 }, 900, true);
    expect(s.jumping).toBe(true);
    expect(s.jumpExpiresAt).toBe(900 + JUMP_PROGRESS_EXTEND_MS);

    // Lands within 12 px -> finishes jump and pins.
    s = onScroll(s, { scrollTop: 3400 - REPIN_THRESHOLD_PX, scrollHeight: 4000, clientHeight: 600 }, 1000, true);
    expect(s.jumping).toBe(false);
    expect(s.pinned).toBe(true);

    // Cancel via pointerdown / touchstart / wheel / keydown mid-jump.
    for (const intent of [
      { kind: "pointer" } as const,
      { kind: "touch" } as const,
      { kind: "wheel", deltaY: 5 } as const,
      { kind: "key", key: "ArrowDown" } as const,
    ]) {
      let j = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
      j = onScroll(j, { scrollTop: 800, scrollHeight: 2000, clientHeight: 600 }, 100, true);
      j = startJump(j, 200);
      j = onUserIntent(j, intent, 250);
      expect(j.jumping).toBe(false);
      expect(j.pinned).toBe(false);
      expect(shouldShowLatestPill(j)).toBe(true);
    }

    // Cancel via scrollend even if content grew during scroll so we didn't reach <= 12 px.
    let endCase = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    endCase = onScroll(endCase, { scrollTop: 800, scrollHeight: 2000, clientHeight: 600 }, 100, true);
    endCase = startJump(endCase, 200);
    endCase = onScrollEnd(endCase, { scrollTop: 1200, scrollHeight: 2000, clientHeight: 600 }, true);
    expect(endCase.jumping).toBe(false);
    expect(endCase.pinned).toBe(false);
    expect(shouldShowLatestPill(endCase)).toBe(true);

    // Cancel via 650 ms safety timeout when stalled.
    let timeoutCase = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    timeoutCase = onScroll(timeoutCase, { scrollTop: 800, scrollHeight: 2000, clientHeight: 600 }, 100, true);
    timeoutCase = startJump(timeoutCase, 200);
    timeoutCase = onJumpTimeout(timeoutCase, { scrollTop: 1000, scrollHeight: 2000, clientHeight: 600 }, 200 + JUMP_TIMEOUT_MS - 1, true);
    expect(timeoutCase.jumping).toBe(true);
    timeoutCase = onJumpTimeout(timeoutCase, { scrollTop: 1000, scrollHeight: 2000, clientHeight: 600 }, 200 + JUMP_TIMEOUT_MS, true);
    expect(timeoutCase.jumping).toBe(false);
    expect(timeoutCase.pinned).toBe(false);
    expect(shouldShowLatestPill(timeoutCase)).toBe(true);
  });

  it("suppresses programmatic scroll events briefly and never unpins on content growth", () => {
    let s = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    s = markProgrammaticScroll(s, 1450, 1000);
    expect(s.programmaticSuppressUntil).toBe(1000 + PROGRAMMATIC_SUPPRESS_MS);

    // Browser fires scroll event for our programmatic scroll.
    s = onScroll(s, { scrollTop: 1450, scrollHeight: 2050, clientHeight: 600 }, 1016, true);
    expect(s.pinned).toBe(true);
    expect(s.programmaticSuppressUntil).toBe(0);

    // Content grows while pinned before toBottom() runs: scroll event at same scrollTop must NOT unpin.
    s = onScroll(s, { scrollTop: 1450, scrollHeight: 2200, clientHeight: 600 }, 1100, true);
    expect(s.pinned).toBe(true);
  });

  it("respects autoScroll=false and ResizeObserver gating", () => {
    let s = createScrollPinState({ autoScroll: false, initialMetrics: { scrollTop: 0, scrollHeight: 2000, clientHeight: 600 } });
    expect(s.pinned).toBe(false);
    expect(shouldShowLatestPill(s)).toBe(true);
    expect(shouldAutoScrollOnResize(s, false, true, true)).toBe(false);

    // Scrolling to bottom with autoScroll=false does not pin.
    s = onScroll(s, bottom(2000, 600), 200, false);
    expect(s.pinned).toBe(false);

    // Clicking Latest works cleanly with autoScroll=false and hides pill at bottom.
    s = onScroll(s, { scrollTop: 800, scrollHeight: 2000, clientHeight: 600 }, 300, false);
    s = startJump(s, 400);
    s = onScroll(s, bottom(2000, 600), 500, false);
    expect(s.jumping).toBe(false);
    expect(s.pinned).toBe(false);
    expect(shouldShowLatestPill(s)).toBe(false);

    // New content pushes distance > 96 while autoScroll=false -> pill reappears without jumping.
    s = syncMetrics(s, { scrollTop: 1400, scrollHeight: 2200, clientHeight: 600 }, false);
    expect(s.distanceFromBottom).toBe(200);
    expect(shouldShowLatestPill(s)).toBe(true);

    // ResizeObserver gating when autoScroll=true:
    const pinnedState = createScrollPinState({ autoScroll: true, initialMetrics: bottom(2000, 600) });
    expect(shouldAutoScrollOnResize(pinnedState, true, false, false)).toBe(false);
    expect(shouldAutoScrollOnResize(pinnedState, true, true, false)).toBe(true);
    expect(shouldAutoScrollOnResize(pinnedState, true, false, true)).toBe(true);

    // Toggling autoScroll dynamically:
    const scrolledUp = onScroll(pinnedState, { scrollTop: 800, scrollHeight: 2000, clientHeight: 600 }, 700, true);
    expect(setAutoScroll(scrolledUp, true, { scrollTop: 800, scrollHeight: 2000, clientHeight: 600 }).pinned).toBe(false);
    expect(setAutoScroll(scrolledUp, true, bottom(2000, 600)).pinned).toBe(true);
  });
});
