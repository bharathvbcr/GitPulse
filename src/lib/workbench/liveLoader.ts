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
}

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

  async function load(mode?: Mode): Promise<void> {
    if (!options.canRun() || disposed) return;
    if (loading) {
      again = true;
      return;
    }
    const epoch = generation;
    const isCurrent = () => epoch === generation && !disposed;
    loading = true;
    try {
      await options.read(mode, isCurrent);
    } catch (cause) {
      if (isCurrent()) options.onError(cause);
    } finally {
      loading = false;
      if (again) {
        again = false;
        void load();
      }
    }
  }

  function schedule(): void {
    if (!options.canRun() || disposed) return;
    clearTimeout(timer);
    timer = setTimeout(() => void load(), options.delayMs ?? 200);
  }

  return {
    load,
    schedule,
    /** The list is a different list now: orphan the read in flight and any pending refresh. */
    reset(): void {
      generation += 1;
      clearTimeout(timer);
    },
    /** For callbacks guarded by "still mounted". */
    isDisposed: () => disposed,
    dispose(): void {
      disposed = true;
      generation += 1;
      clearTimeout(timer);
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
