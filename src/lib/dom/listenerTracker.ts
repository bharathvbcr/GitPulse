/**
 * Ownership for unlisten functions whose registration is asynchronous.
 *
 * The race this closes: a listener promise resolves AFTER its owner's
 * cleanup already ran (e.g. a fast tab switch remounts a panel). Pushing
 * the resolved unlisten into an array the cleanup already drained leaks
 * the listener for the webview lifetime. A tracker invoked after dispose
 * calls the unlisten immediately instead of storing it, so late arrivals
 * self-unregister.
 */

export interface ListenerTracker {
  /**
   * Records an unlisten for later unwind, or — if the owner was already
   * disposed — invokes it immediately so nothing outlives the owner.
   */
  track(unlisten: () => void): void;
  /** Unwinds every tracked unlisten LIFO, then refuses further tracking. */
  dispose(): void;
  /** How many unlisten fns are currently held (0 once disposed). */
  readonly size: number;
  /** True after the first dispose(); never flips back. */
  readonly disposed: boolean;
}

export function createListenerTracker(): ListenerTracker {
  let fns: Array<() => void> = [];
  let disposed = false;
  return {
    track(unlisten) {
      if (disposed) {
        // Teardown won the race against this registration: undo it now.
        try {
          unlisten();
        } catch {
          /* a dead listener must not break its owner's lifecycle */
        }
        return;
      }
      fns.push(unlisten);
    },
    dispose() {
      disposed = true;
      // LIFO: release newest first, mirroring reverse-registration order.
      while (fns.length > 0) {
        const unlisten = fns.pop();
        try {
          unlisten?.();
        } catch {
          /* one dead listener must not strand the rest of the unwind */
        }
      }
    },
    get size() {
      return fns.length;
    },
    get disposed() {
      return disposed;
    },
  };
}
