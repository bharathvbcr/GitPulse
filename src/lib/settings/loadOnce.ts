/**
 * Runs a Settings panel's first load once, and again only when asked.
 *
 * The panels used to gate their `$effect` on a `loaded` flag that only a
 * success sets. A rejected load then cleared `busy` in `finally`, which
 * re-ran the effect while `loaded` was still false, and the loader was called
 * again as fast as each rejection returned: the error text flickered and the
 * IPC bridge was hammered for as long as the panel was open. An attempt is
 * recorded before the loader runs, so a failure stops the automatic retries
 * and leaves the panel to show the error with a Retry button (`retry`).
 */
export interface LoadOnce {
  /** Starts the load unless one has already been attempted. */
  request(): void;
  /** Starts the load again, whatever happened to the last attempt. */
  retry(): void;
  /** Whether a load has been started, successful or not. */
  readonly attempted: boolean;
}

export function loadOnce(load: () => Promise<void>): LoadOnce {
  let attempted = false;
  return {
    request() {
      if (attempted) return;
      attempted = true;
      void load();
    },
    retry() {
      attempted = true;
      void load();
    },
    get attempted() {
      return attempted;
    },
  };
}
