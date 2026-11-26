import { createListenerTracker } from "../dom/listenerTracker";
import { bindForegroundChanges, readBackgroundDocument, readHiddenDocument } from "../runtime/foreground";
import { subscribeWorkbenchChanges } from "./client";

export interface LiveLoaderOptions<Mode> {
  /** False while the surface is inactive or its window is in the background; a load then does nothing. */
  canRun(): boolean;
  /**
   * One read. `isCurrent()` turns false once the surface is reset or disposed,
   * so an answer that arrives after that must be discarded, not applied.
   */
  read(mode: Mode | undefined, isCurrent: () => boolean): Promise<void | "failed">;
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
  /**
   * Opt-in backoff for a source that is failing. A read that resolves
   * `"failed"` (or throws) is retried by the loader after `baseMs`, then
   * twice that, doubling up to `maxMs`, until a read succeeds: asking a busy
   * store again every `minGapMs` is what kept it busy. A change that arrives
   * meanwhile is held to the same schedule; `load()` never is. Without this
   * option a failed read is not retried and nothing here changes.
   */
  retry?: { baseMs: number; maxMs: number };
}

/**
 * Whether a live surface should keep itself current: true while the window is
 * on screen, focused or not. Lost focus is not "in the background" here. The
 * board beside another app's terminal, where an agent is filing tasks, is the
 * commonest moment a change arrives, and the host announces external changes
 * exactly while the window is on screen (`external_changes.rs`), so pausing on
 * blur dropped those changes until the next click into the window. Work that
 * spawns git keeps the stricter focus gate (`readBackgroundDocument`).
 */
export function liveOnScreen(): boolean {
  return !readHiddenDocument();
}

/**
 * Tells, for each foreground event, whether a live surface should re-read: the
 * window came back on screen, or took focus. A blur while it stays on screen
 * is not one. Changes keep arriving as change events then, and re-reading on
 * the blur too doubled the reads of every switch to another app.
 */
export function createLiveReturn(): (onScreen: boolean) => boolean {
  let was = liveOnScreen();
  return (onScreen) => {
    const back = onScreen && (!was || !readBackgroundDocument());
    was = onScreen;
    return back;
  };
}

/** Changes reach a surface within this long, however often they come. */
export const LIVE_MIN_GAP_MS = 1_500;

/** The longest a failing live read waits before it is asked again (see `retry`). */
export const LIVE_RETRY_MAX_MS = 15_000;

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
  /** Reads in a row that failed; only counted when `retry` is set. */
  let failures = 0;
  /** How long the next change-driven read waits for the failures so far; 0 when the last one landed. */
  const backoffMs = () => (options.retry && failures > 0 ? Math.min(options.retry.maxMs, options.retry.baseMs * 2 ** (failures - 1)) : 0);

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
    let failed = false;
    try {
      failed = (await options.read(mode, isCurrent)) === "failed";
    } catch (cause) {
      failed = true;
      if (isCurrent()) options.onError(cause);
    } finally {
      loading = false;
      lastReadAt = Date.now();
      // A read that a reset orphaned says nothing about the source now.
      if (options.retry && isCurrent()) failures = failed ? failures + 1 : 0;
      if (again) {
        again = false;
        owed = false;
        void load();
      } else if (owed || (failed && options.retry && isCurrent())) {
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
    // A change after the whole backoff has passed with nothing scheduled finds a source that may have recovered.
    if (failures > 0 && Date.now() - lastReadAt >= backoffMs()) failures = 0;
    const gap = Math.max(lastReadAt + (options.minGapMs ?? LIVE_MIN_GAP_MS), lastReadAt + backoffMs()) - Date.now();
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
      failures = 0;
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
     * Subscribes to workbench changes and to the window's hide, show, focus
     * and blur. `onVisibility` receives whether the window is on screen
     * (`liveOnScreen`); a refresh follows only a return (`createLiveReturn`).
     * The disposer releases both.
     */
    connect(handlers: {
      onVisibility(visible: boolean): void;
      onUnavailable(message: string): void;
    }): () => void {
      const listeners = createListenerTracker();
      const returned = createLiveReturn();
      listeners.track(subscribeWorkbenchChanges({
        changed: schedule,
        unavailable: (message) => {
          if (!disposed) handlers.onUnavailable(message);
        },
      }));
      listeners.track(bindForegroundChanges(document, typeof window === "undefined" ? null : window, () => {
        const visible = liveOnScreen();
        handlers.onVisibility(visible);
        if (returned(visible)) schedule();
      }));
      return () => listeners.dispose();
    },
  };
}
