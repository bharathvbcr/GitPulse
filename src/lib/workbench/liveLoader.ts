import { createListenerTracker } from "../dom/listenerTracker";
import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
import { subscribeWorkbenchChanges } from "./client";

export interface LiveLoaderOptions<Mode> {
  /** False while the surface is inactive or its window is in the background; a load then does nothing. */
  canRun(): boolean;
  /**
   * One read. `isCurrent()` turns false once the surface is reset or disposed,
   * so an answer that arrives after that must be discarded, not applied.
   */
  read(mode: Mode | undefined, isCurrent: () => boolean): Promise<void>;
  /** A read that rejected while still current. */
  onError(cause: unknown): void;
  /** How long a change burst is coalesced before one re-read. */
  delayMs?: number;
  /**
   * The least time between the starts of two change-driven reads. A store
   * that commits every second sends a change every second; without a floor
   * each one was a full re-read. `load()` itself is never held back.
   */
  minGapMs?: number;
}

/** Changes reach a surface within this long, however often they come. */
export const LIVE_MIN_GAP_MS = 1_500;

/**
 * The load/coalesce/refresh loop the Task archive and the Attention inbox
 * both had, line for line: one read at a time (a load that arrives mid-read
 * is remembered and re-run after it), an epoch so a reset orphans the read in
 * flight, a short debounce for workbench change events, and a refresh when the
 * window returns. The surface keeps what differs: what a read fetches, and
 * what its answer does to the screen.
 */
export function createLiveLoader<Mode = never>(options: LiveLoaderOptions<Mode>) {
  let generation = 0;
  let disposed = false;
  let loading = false;
  let again = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** When the last read started or, once it has, ended; the gap for change-driven reads counts from it. */
  let lastReadAt = -Infinity;
  /** A change-driven read came due while a read was running; it is scheduled once that read ends. */
  let owed = false;

  async function load(mode?: Mode): Promise<void> {
    if (!options.canRun() || disposed) return;
    if (loading) {
      again = true;
      return;
    }
    const epoch = generation;
    const isCurrent = () => epoch === generation && !disposed;
    loading = true;
    lastReadAt = Date.now();
    try {
      await options.read(mode, isCurrent);
    } catch (cause) {
      if (isCurrent()) options.onError(cause);
    } finally {
      loading = false;
      lastReadAt = Date.now();
      if (again) {
        again = false;
        owed = false;
        void load();
      } else if (owed) {
        owed = false;
        schedule();
      }
    }
  }

  /**
   * One re-read for a burst of changes. A pending re-read is kept, not pushed
   * back: restarting the timer on each change put the read off for as long
   * as changes kept arriving faster than the delay.
   */
  function schedule(): void {
    if (!options.canRun() || disposed || timer !== undefined) return;
    if (loading) { owed = true; return; }
    const gap = lastReadAt + (options.minGapMs ?? LIVE_MIN_GAP_MS) - Date.now();
    timer = setTimeout(() => {
      timer = undefined;
      // A read began meanwhile (an explicit load): this one waits for it.
      if (loading) owed = true;
      else void load();
    }, Math.max(options.delayMs ?? 200, gap));
  }

  function cancelScheduled(): void {
    clearTimeout(timer);
    timer = undefined;
    owed = false;
  }

  return {
    load,
    schedule,
    /** The list is a different list now: orphan the read in flight and any pending refresh. */
    reset(): void {
      generation += 1;
      cancelScheduled();
    },
    /** For callbacks guarded by "still mounted". */
    isDisposed: () => disposed,
    dispose(): void {
      disposed = true;
      generation += 1;
      cancelScheduled();
    },
    /**
     * Subscribes to workbench changes and to the window returning to the
     * front. `onVisibility` receives the new state before a refresh is
     * scheduled for a return. The disposer releases both.
     */
    connect(handlers: {
      onVisibility(visible: boolean): void;
      onUnavailable(message: string): void;
    }): () => void {
      const listeners = createListenerTracker();
      listeners.track(subscribeWorkbenchChanges({
        changed: schedule,
        unavailable: (message) => {
          if (!disposed) handlers.onUnavailable(message);
        },
      }));
      listeners.track(bindForegroundChanges(document, typeof window === "undefined" ? null : window, () => {
        const visible = !readBackgroundDocument();
        handlers.onVisibility(visible);
        if (visible) schedule();
      }));
      return () => listeners.dispose();
    },
  };
}
