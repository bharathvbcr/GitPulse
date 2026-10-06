/**
 * When a `repo-changed` event may cost a refresh.
 *
 * Every open tab is watched, and a refresh is a full snapshot — about eight
 * git children, plus branch churn when tips moved. Refreshing every tab on its
 * own events, as this store used to, let two repositories with an agent
 * editing them spend the app's whole spawn budget (8 children a second,
 * shared with every GitPulse process of this user) on tabs nobody was looking
 * at. The visible tab's own reads were then deferred behind them, and that is
 * what the user saw as slowness.
 *
 * The rule now:
 *
 * - The **active tab of a shown window** refreshes on its events after the
 *   storm debounce, as before. Unfocused is still shown: someone watching
 *   GitPulse on a second display while an agent works must see it live.
 * - A **background tab** only has to keep its tab-strip badges (dirty dot,
 *   counts, branch) roughly current, because activating it hydrates it in
 *   full anyway. Its events coalesce into at most one refresh per
 *   {@link BACKGROUND_REFRESH_MIN_MS}, and background refreshes across ALL
 *   tabs are spaced {@link BACKGROUND_REFRESH_SPACING_MS} apart, so their cost
 *   is bounded by a constant rather than by how many tabs are busy.
 * - While the window is **hidden** (minimised, another Space), nothing is
 *   refreshed. The event is remembered and paid once when it is shown again.
 *
 * Pure apart from the injected clock, timers and predicates, so the schedule
 * can be driven by a test without a store or a DOM.
 */

/** The storm debounce the active tab has always had. */
export const WATCHER_REFRESH_DEBOUNCE_MS = 200;
/** Least time between two refreshes of one background tab. */
export const BACKGROUND_REFRESH_MIN_MS = 30_000;
/** Least time between any two background refreshes, across every tab. */
export const BACKGROUND_REFRESH_SPACING_MS = 5_000;

export interface WatcherRefreshDeps {
  now(): number;
  setTimer(run: () => void, delayMs: number): unknown;
  clearTimer(handle: unknown): void;
  /** Whether `key` is the active tab's repository. */
  isActive(key: string): boolean;
  /** Whether the window is hidden — not merely unfocused. */
  isHidden(): boolean;
  /** Runs the full refresh of `key`'s session. */
  refresh(key: string): void;
}

export interface WatcherRefreshPolicy {
  /** A settled `repo-changed` event for `key`. */
  onChange(key: string): void;
  /**
   * `key` is about to be hydrated in full by its activation. Called before
   * the hydrate, not after: an event arriving while it runs may describe a
   * change the hydrate already read past, and must arm a fresh refresh
   * rather than be cancelled by a stamp landing after it. The cost of that
   * order is that a refused activation hydrate still anchors the background
   * floor; the tab is active then, and its next event takes the 200 ms path.
   */
  onActivated(key: string): void;
  /** The window may have been shown or hidden. */
  onVisibilityChange(): void;
  /** `key`'s tab closed: drop everything held for it. */
  forget(key: string): void;
  /** Cancels every timer; nothing runs afterwards. */
  dispose(): void;
  /** For tests and diagnostics: keys with a refresh owed or armed. */
  owed(): string[];
}

interface Entry {
  timer: unknown;
  /** When the armed timer fires, for a background timer; null otherwise. */
  backgroundDueAt: number | null;
  /** When this key last had a full refresh (ours or an activation's). */
  lastRunAt: number | null;
  /** An event arrived while hidden; pay it when shown. */
  owedWhileHidden: boolean;
}

export function createWatcherRefreshPolicy(deps: WatcherRefreshDeps): WatcherRefreshPolicy {
  const entries = new Map<string, Entry>();
  /** When the last background refresh actually ran, across all keys. */
  let lastBackgroundRunAt = -Infinity;
  let disposed = false;

  /**
   * Earliest start for a newly armed background refresh: spaced after the
   * last one that ran and after every one still armed. Derived rather than
   * kept as a running reservation, so a timer cancelled by activation or a
   * closed tab gives its slot back instead of delaying everyone after it.
   */
  const nextBackgroundSlot = (): number => {
    let latest = lastBackgroundRunAt;
    for (const entry of entries.values()) {
      if (entry.timer !== null && entry.backgroundDueAt !== null) {
        latest = Math.max(latest, entry.backgroundDueAt);
      }
    }
    return latest + BACKGROUND_REFRESH_SPACING_MS;
  };

  const entryFor = (key: string): Entry => {
    let entry = entries.get(key);
    if (!entry) {
      entry = { timer: null, backgroundDueAt: null, lastRunAt: null, owedWhileHidden: false };
      entries.set(key, entry);
    }
    return entry;
  };

  const disarm = (entry: Entry) => {
    if (entry.timer !== null) {
      deps.clearTimer(entry.timer);
      entry.timer = null;
    }
    entry.backgroundDueAt = null;
  };

  const fire = (key: string) => {
    const entry = entries.get(key);
    if (!entry || disposed) return;
    const wasBackground = entry.backgroundDueAt !== null;
    entry.timer = null;
    entry.backgroundDueAt = null;
    // Re-decided at fire time: the window may have been hidden since.
    if (deps.isHidden()) {
      entry.owedWhileHidden = true;
      return;
    }
    const now = deps.now();
    entry.lastRunAt = now;
    if (wasBackground) lastBackgroundRunAt = now;
    deps.refresh(key);
  };

  const armActive = (key: string, entry: Entry) => {
    // A re-armed trailing window, exactly the debounce this replaced: a storm
    // becomes one refresh after it settles.
    disarm(entry);
    entry.timer = deps.setTimer(() => fire(key), WATCHER_REFRESH_DEBOUNCE_MS);
  };

  const armBackground = (key: string, entry: Entry) => {
    // Already armed: this event rides on the refresh already owed. An armed
    // ACTIVE debounce for a tab that has since gone to the background keeps
    // its short timer; it was owed before the switch.
    if (entry.timer !== null) return;
    const now = deps.now();
    const due = Math.max(
      now + WATCHER_REFRESH_DEBOUNCE_MS,
      entry.lastRunAt === null ? -Infinity : entry.lastRunAt + BACKGROUND_REFRESH_MIN_MS,
      nextBackgroundSlot(),
    );
    entry.backgroundDueAt = due;
    entry.timer = deps.setTimer(() => fire(key), due - now);
  };

  const schedule = (key: string) => {
    const entry = entryFor(key);
    if (deps.isHidden()) {
      // Nobody can see any tab. Hold the debt instead of a timer, so a window
      // hidden for an hour costs one refresh per tab when it returns, not one
      // per event.
      disarm(entry);
      entry.owedWhileHidden = true;
      return;
    }
    entry.owedWhileHidden = false;
    if (deps.isActive(key)) armActive(key, entry);
    else armBackground(key, entry);
  };

  return {
    onChange(key) {
      if (disposed) return;
      schedule(key);
    },
    onActivated(key) {
      if (disposed) return;
      const entry = entryFor(key);
      disarm(entry);
      entry.owedWhileHidden = false;
      entry.lastRunAt = deps.now();
    },
    onVisibilityChange() {
      if (disposed || deps.isHidden()) return;
      // The active tab first, so its refresh is not queued behind background
      // slots the others claim.
      const owed = [...entries.entries()].filter(([, entry]) => entry.owedWhileHidden);
      owed.sort(([a], [b]) => Number(deps.isActive(b)) - Number(deps.isActive(a)));
      for (const [key] of owed) schedule(key);
    },
    forget(key) {
      const entry = entries.get(key);
      if (!entry) return;
      disarm(entry);
      entries.delete(key);
    },
    dispose() {
      disposed = true;
      for (const entry of entries.values()) disarm(entry);
      entries.clear();
    },
    owed() {
      return [...entries.entries()]
        .filter(([, entry]) => entry.timer !== null || entry.owedWhileHidden)
        .map(([key]) => key);
    },
  };
}
