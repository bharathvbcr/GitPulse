/**
 * Pure state machine for conversation timeline auto-scroll pinning and the
 * "Latest" jump pill.
 *
 * Design goals:
 * - Unpin immediately on any upward scroll, upward wheel/key input, or active
 *   pointer/scrollbar drag away from the bottom, and hold a 180 ms cooldown so
 *   trailing trackpad/rubber-band events never snap back to pinned.
 * - Re-pin only when scrolling downward into the <= 12 px bottom zone after
 *   the cooldown has expired, or when an explicit jump-to-latest finishes at
 *   the bottom.
 * - Use a 72 px hysteresis deadband for `pillVisible`: show only when
 *   unpinned, not mid-jump, overflowing, and > 96 px from bottom; hide only
 *   when pinned, jumping, non-overflowing, or <= 24 px from bottom. Between
 *   24 px and 96 px `pillVisible` preserves its previous value.
 * - Suppress programmatic scroll events briefly so `onscroll` never mistakes
 *   our own `scrollTop` snap for user movement.
 * - Extend active smooth-jump timeout on downward scroll progress so long
 *   smooth scrolls on WKWebView do not time out mid-animation.
 */

export const REPIN_THRESHOLD_PX = 12;
export const PILL_SHOW_THRESHOLD_PX = 96;
export const PILL_HIDE_THRESHOLD_PX = 24;
export const UPWARD_SCROLL_EPSILON_PX = 2;
export const USER_SCROLL_COOLDOWN_MS = 180;
export const JUMP_TIMEOUT_MS = 650;
export const JUMP_PROGRESS_EXTEND_MS = 350;
export const PROGRAMMATIC_SUPPRESS_MS = 80;
export const OVERFLOW_EPSILON_PX = 1;

export interface ScrollMetrics {
  readonly scrollTop: number;
  readonly scrollHeight: number;
  readonly clientHeight: number;
}

export type UserScrollIntent =
  | { readonly kind: "wheel"; readonly deltaY: number }
  | { readonly kind: "key"; readonly key: string }
  | { readonly kind: "pointer" }
  | { readonly kind: "touch" };

export interface ScrollPinState {
  readonly pinned: boolean;
  readonly jumping: boolean;
  readonly pointerActive: boolean;
  readonly pillVisible: boolean;
  readonly cooldownUntil: number;
  readonly jumpExpiresAt: number;
  readonly lastScrollTop: number;
  readonly lastScrollHeight: number;
  readonly lastClientHeight: number;
  readonly distanceFromBottom: number;
  readonly overflows: boolean;
  readonly programmaticSuppressUntil: number;
}

const UPWARD_KEYS = new Set(["PageUp", "ArrowUp", "Home"]);

function normalizeMetrics(metrics: ScrollMetrics): {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  maxScrollTop: number;
  distanceFromBottom: number;
  overflows: boolean;
} {
  const scrollHeight = Math.max(0, metrics.scrollHeight);
  const clientHeight = Math.max(0, metrics.clientHeight);
  const maxScrollTop = Math.max(0, scrollHeight - clientHeight);
  const scrollTop = Math.min(maxScrollTop, Math.max(0, metrics.scrollTop));
  const distanceFromBottom = Math.max(0, maxScrollTop - scrollTop);
  const overflows = maxScrollTop > OVERFLOW_EPSILON_PX;
  return { scrollTop, scrollHeight, clientHeight, maxScrollTop, distanceFromBottom, overflows };
}

function nextPillVisible(
  prev: boolean,
  params: {
    pinned: boolean;
    jumping: boolean;
    overflows: boolean;
    distanceFromBottom: number;
  },
): boolean {
  if (params.pinned || params.jumping || !params.overflows || params.distanceFromBottom <= PILL_HIDE_THRESHOLD_PX) {
    return false;
  }
  if (params.distanceFromBottom > PILL_SHOW_THRESHOLD_PX) {
    return true;
  }
  return prev;
}

export function createScrollPinState(options: {
  autoScroll?: boolean;
  initialMetrics?: ScrollMetrics;
} = {}): ScrollPinState {
  const autoScroll = options.autoScroll ?? true;
  const m = options.initialMetrics
    ? normalizeMetrics(options.initialMetrics)
    : { scrollTop: 0, scrollHeight: 0, clientHeight: 0, maxScrollTop: 0, distanceFromBottom: 0, overflows: false };

  const pillVisible = nextPillVisible(false, {
    pinned: autoScroll,
    jumping: false,
    overflows: m.overflows,
    distanceFromBottom: m.distanceFromBottom,
  });

  return {
    pinned: autoScroll,
    jumping: false,
    pointerActive: false,
    pillVisible,
    cooldownUntil: 0,
    jumpExpiresAt: 0,
    lastScrollTop: m.scrollTop,
    lastScrollHeight: m.scrollHeight,
    lastClientHeight: m.clientHeight,
    distanceFromBottom: m.distanceFromBottom,
    overflows: m.overflows,
    programmaticSuppressUntil: 0,
  };
}

export function isUpwardIntent(intent: UserScrollIntent): boolean {
  if (intent.kind === "wheel") return intent.deltaY < 0;
  if (intent.kind === "key") return UPWARD_KEYS.has(intent.key);
  return false;
}

export function onUserIntent(
  state: ScrollPinState,
  intent: UserScrollIntent,
  now: number,
): ScrollPinState {
  const upward = isUpwardIntent(intent);
  const pointerDown = intent.kind === "pointer" || intent.kind === "touch";
  if (!upward && !pointerDown && !state.jumping && state.programmaticSuppressUntil === 0) {
    return state;
  }
  const pinned = upward ? false : state.pinned;
  const jumping = false;
  const pillVisible = nextPillVisible(state.pillVisible, {
    pinned,
    jumping,
    overflows: state.overflows,
    distanceFromBottom: state.distanceFromBottom,
  });
  return {
    ...state,
    pinned,
    jumping,
    pointerActive: pointerDown ? true : state.pointerActive,
    pillVisible,
    jumpExpiresAt: 0,
    programmaticSuppressUntil: 0,
    cooldownUntil: upward ? Math.max(state.cooldownUntil, now + USER_SCROLL_COOLDOWN_MS) : state.cooldownUntil,
  };
}

export function onPointerEnd(
  state: ScrollPinState,
  metrics: ScrollMetrics | null,
  now: number,
  autoScroll: boolean,
): ScrollPinState {
  if (!state.pointerActive && !metrics) return state;
  const m = metrics ? normalizeMetrics(metrics) : null;
  const distanceFromBottom = m ? m.distanceFromBottom : state.distanceFromBottom;
  const overflows = m ? m.overflows : state.overflows;
  let pinned = !autoScroll ? false : state.pinned;
  let cooldownUntil = state.cooldownUntil;
  if (
    autoScroll &&
    m &&
    !pinned &&
    m.distanceFromBottom <= REPIN_THRESHOLD_PX &&
    m.scrollTop >= state.lastScrollTop
  ) {
    pinned = true;
    cooldownUntil = 0;
  }
  const pillVisible = nextPillVisible(state.pillVisible, {
    pinned,
    jumping: state.jumping,
    overflows,
    distanceFromBottom,
  });
  return {
    ...state,
    pinned,
    pointerActive: false,
    pillVisible,
    lastScrollTop: m ? m.scrollTop : state.lastScrollTop,
    lastScrollHeight: m ? m.scrollHeight : state.lastScrollHeight,
    lastClientHeight: m ? m.clientHeight : state.lastClientHeight,
    distanceFromBottom,
    overflows,
    cooldownUntil: cooldownUntil > now ? cooldownUntil : cooldownUntil,
  };
}

export function startJump(state: ScrollPinState, now: number): ScrollPinState {
  return {
    ...state,
    jumping: true,
    pillVisible: false,
    jumpExpiresAt: now + JUMP_TIMEOUT_MS,
    cooldownUntil: 0,
    programmaticSuppressUntil: 0,
  };
}

export function onScroll(
  state: ScrollPinState,
  metrics: ScrollMetrics,
  now: number,
  autoScroll: boolean,
): ScrollPinState {
  const m = normalizeMetrics(metrics);

  // Expire a stale smooth jump so it can never trap subsequent scrolls.
  let jumping = state.jumping;
  let jumpExpiresAt = state.jumpExpiresAt;
  if (jumping && now >= jumpExpiresAt) {
    jumping = false;
    jumpExpiresAt = 0;
  }

  if (jumping) {
    const reachedBottom = m.distanceFromBottom <= REPIN_THRESHOLD_PX;
    const pinned = reachedBottom ? autoScroll : state.pinned;
    const stillJumping = !reachedBottom;
    // Extend jump window while downward smooth-scroll frames are progressing.
    const nextExpires = stillJumping
      ? m.scrollTop > state.lastScrollTop
        ? Math.max(jumpExpiresAt, now + JUMP_PROGRESS_EXTEND_MS)
        : jumpExpiresAt
      : 0;
    const pillVisible = nextPillVisible(state.pillVisible, {
      pinned,
      jumping: stillJumping,
      overflows: m.overflows,
      distanceFromBottom: m.distanceFromBottom,
    });
    return {
      ...state,
      pinned,
      jumping: stillJumping,
      pillVisible,
      jumpExpiresAt: nextExpires,
      cooldownUntil: reachedBottom ? 0 : state.cooldownUntil,
      lastScrollTop: m.scrollTop,
      lastScrollHeight: m.scrollHeight,
      lastClientHeight: m.clientHeight,
      distanceFromBottom: m.distanceFromBottom,
      overflows: m.overflows,
    };
  }

  if (now < state.programmaticSuppressUntil && !state.pointerActive) {
    const nonUpward = m.scrollTop >= state.lastScrollTop - UPWARD_SCROLL_EPSILON_PX;
    if (nonUpward && m.distanceFromBottom <= REPIN_THRESHOLD_PX) {
      const pillVisible = nextPillVisible(state.pillVisible, {
        pinned: state.pinned,
        jumping: false,
        overflows: m.overflows,
        distanceFromBottom: m.distanceFromBottom,
      });
      return {
        ...state,
        jumping: false,
        pillVisible,
        jumpExpiresAt: 0,
        programmaticSuppressUntil: 0,
        lastScrollTop: m.scrollTop,
        lastScrollHeight: m.scrollHeight,
        lastClientHeight: m.clientHeight,
        distanceFromBottom: m.distanceFromBottom,
        overflows: m.overflows,
      };
    }
  }

  // Dragging the scrollbar or touch-dragging away from bottom unpins at once.
  if (state.pointerActive && m.distanceFromBottom > REPIN_THRESHOLD_PX) {
    const pillVisible = nextPillVisible(state.pillVisible, {
      pinned: false,
      jumping: false,
      overflows: m.overflows,
      distanceFromBottom: m.distanceFromBottom,
    });
    return {
      ...state,
      pinned: false,
      jumping: false,
      pillVisible,
      jumpExpiresAt: 0,
      programmaticSuppressUntil: 0,
      cooldownUntil: Math.max(state.cooldownUntil, now + USER_SCROLL_COOLDOWN_MS),
      lastScrollTop: m.scrollTop,
      lastScrollHeight: m.scrollHeight,
      lastClientHeight: m.clientHeight,
      distanceFromBottom: m.distanceFromBottom,
      overflows: m.overflows,
    };
  }

  const scrolledUp = m.scrollTop < state.lastScrollTop - UPWARD_SCROLL_EPSILON_PX;
  // When content shrinks OR clientHeight grows (e.g. composer closing) while
  // pinned at bottom, the browser clamps scrollTop downward while
  // distanceFromBottom stays ~0; that is not a user scroll up.
  const prevMaxScrollTop = Math.max(0, state.lastScrollHeight - state.lastClientHeight);
  const maxScrollTopShrank = m.maxScrollTop < prevMaxScrollTop;
  const layoutShrinkAtBottom =
    state.pinned && !state.pointerActive && maxScrollTopShrank && m.distanceFromBottom <= 2;

  if (scrolledUp && !layoutShrinkAtBottom) {
    const pillVisible = nextPillVisible(state.pillVisible, {
      pinned: false,
      jumping: false,
      overflows: m.overflows,
      distanceFromBottom: m.distanceFromBottom,
    });
    return {
      ...state,
      pinned: false,
      jumping: false,
      pillVisible,
      jumpExpiresAt: 0,
      programmaticSuppressUntil: 0,
      cooldownUntil: Math.max(state.cooldownUntil, now + USER_SCROLL_COOLDOWN_MS),
      lastScrollTop: m.scrollTop,
      lastScrollHeight: m.scrollHeight,
      lastClientHeight: m.clientHeight,
      distanceFromBottom: m.distanceFromBottom,
      overflows: m.overflows,
    };
  }

  const scrolledDown = m.scrollTop > state.lastScrollTop;
  const cooldownExpired = now >= state.cooldownUntil;
  let pinned = state.pinned;
  if (!autoScroll) {
    pinned = false;
  } else if (!pinned && scrolledDown && m.distanceFromBottom <= REPIN_THRESHOLD_PX && cooldownExpired) {
    pinned = true;
  }

  const pillVisible = nextPillVisible(state.pillVisible, {
    pinned,
    jumping: false,
    overflows: m.overflows,
    distanceFromBottom: m.distanceFromBottom,
  });

  return {
    ...state,
    pinned,
    jumping: false,
    pillVisible,
    jumpExpiresAt: 0,
    lastScrollTop: m.scrollTop,
    lastScrollHeight: m.scrollHeight,
    lastClientHeight: m.clientHeight,
    distanceFromBottom: m.distanceFromBottom,
    overflows: m.overflows,
  };
}

export function onScrollEnd(
  state: ScrollPinState,
  metrics: ScrollMetrics,
  autoScroll: boolean,
): ScrollPinState {
  const m = normalizeMetrics(metrics);
  const reachedBottom = m.distanceFromBottom <= REPIN_THRESHOLD_PX;
  let pinned = state.pinned;
  let cooldownUntil = state.cooldownUntil;
  if (state.jumping && reachedBottom) {
    pinned = autoScroll;
    cooldownUntil = 0;
  } else if (!autoScroll) {
    pinned = false;
  }
  const pillVisible = nextPillVisible(state.pillVisible, {
    pinned,
    jumping: false,
    overflows: m.overflows,
    distanceFromBottom: m.distanceFromBottom,
  });
  return {
    ...state,
    pinned,
    jumping: false,
    pillVisible,
    jumpExpiresAt: 0,
    cooldownUntil,
    programmaticSuppressUntil: 0,
    lastScrollTop: m.scrollTop,
    lastScrollHeight: m.scrollHeight,
    lastClientHeight: m.clientHeight,
    distanceFromBottom: m.distanceFromBottom,
    overflows: m.overflows,
  };
}

export function onJumpTimeout(
  state: ScrollPinState,
  metrics: ScrollMetrics | null,
  now: number,
  autoScroll: boolean,
): ScrollPinState {
  if (!state.jumping || now < state.jumpExpiresAt) return state;
  if (!metrics) {
    const pillVisible = nextPillVisible(state.pillVisible, {
      pinned: state.pinned,
      jumping: false,
      overflows: state.overflows,
      distanceFromBottom: state.distanceFromBottom,
    });
    return {
      ...state,
      jumping: false,
      pillVisible,
      jumpExpiresAt: 0,
    };
  }
  const m = normalizeMetrics(metrics);
  const reachedBottom = m.distanceFromBottom <= REPIN_THRESHOLD_PX;
  const pinned = reachedBottom ? autoScroll : state.pinned;
  const pillVisible = nextPillVisible(state.pillVisible, {
    pinned,
    jumping: false,
    overflows: m.overflows,
    distanceFromBottom: m.distanceFromBottom,
  });
  return {
    ...state,
    pinned,
    jumping: false,
    pillVisible,
    jumpExpiresAt: 0,
    cooldownUntil: reachedBottom ? 0 : state.cooldownUntil,
    lastScrollTop: m.scrollTop,
    lastScrollHeight: m.scrollHeight,
    lastClientHeight: m.clientHeight,
    distanceFromBottom: m.distanceFromBottom,
    overflows: m.overflows,
  };
}

export function syncMetrics(
  state: ScrollPinState,
  metrics: ScrollMetrics,
  autoScroll: boolean,
): ScrollPinState {
  const m = normalizeMetrics(metrics);
  const pinned = autoScroll ? state.pinned : false;
  const prevMaxScrollTop = Math.max(0, state.lastScrollHeight - state.lastClientHeight);
  const lastScrollTop = m.maxScrollTop < prevMaxScrollTop ? m.scrollTop : state.lastScrollTop;
  const pillVisible = nextPillVisible(state.pillVisible, {
    pinned,
    jumping: state.jumping,
    overflows: m.overflows,
    distanceFromBottom: m.distanceFromBottom,
  });
  if (
    pinned === state.pinned &&
    pillVisible === state.pillVisible &&
    lastScrollTop === state.lastScrollTop &&
    m.scrollHeight === state.lastScrollHeight &&
    m.clientHeight === state.lastClientHeight &&
    m.distanceFromBottom === state.distanceFromBottom &&
    m.overflows === state.overflows
  ) {
    return state;
  }
  return {
    ...state,
    pinned,
    pillVisible,
    lastScrollTop,
    lastScrollHeight: m.scrollHeight,
    lastClientHeight: m.clientHeight,
    distanceFromBottom: m.distanceFromBottom,
    overflows: m.overflows,
  };
}

export function setAutoScroll(
  state: ScrollPinState,
  autoScroll: boolean,
  metrics?: ScrollMetrics,
): ScrollPinState {
  const m = metrics ? normalizeMetrics(metrics) : null;
  const distanceFromBottom = m ? m.distanceFromBottom : state.distanceFromBottom;
  const overflows = m ? m.overflows : state.overflows;
  const pinned = autoScroll ? distanceFromBottom <= REPIN_THRESHOLD_PX : false;
  const pillVisible = nextPillVisible(state.pillVisible, {
    pinned,
    jumping: state.jumping,
    overflows,
    distanceFromBottom,
  });
  return {
    ...state,
    pinned,
    pillVisible,
    lastScrollTop: m ? m.scrollTop : state.lastScrollTop,
    lastScrollHeight: m ? m.scrollHeight : state.lastScrollHeight,
    lastClientHeight: m ? m.clientHeight : state.lastClientHeight,
    distanceFromBottom,
    overflows,
  };
}

export function markProgrammaticScroll(
  state: ScrollPinState,
  targetScrollTop: number,
  now: number,
  metrics?: ScrollMetrics,
): ScrollPinState {
  const top = Math.max(0, targetScrollTop);
  return {
    ...state,
    lastScrollTop: top,
    lastScrollHeight: metrics ? Math.max(0, metrics.scrollHeight) : state.lastScrollHeight,
    lastClientHeight: metrics ? Math.max(0, metrics.clientHeight) : state.lastClientHeight,
    distanceFromBottom: 0,
    pillVisible: false,
    programmaticSuppressUntil: now + PROGRAMMATIC_SUPPRESS_MS,
  };
}

export function shouldShowLatestPill(state: ScrollPinState): boolean {
  return state.pillVisible && !state.pinned && !state.jumping && state.overflows;
}

export function shouldAutoScrollOnResize(
  state: ScrollPinState,
  autoScroll: boolean,
  working: boolean,
  tailRecentlyGrew: boolean,
): boolean {
  return autoScroll && state.pinned && !state.pointerActive && !state.jumping && (working || tailRecentlyGrew);
}
