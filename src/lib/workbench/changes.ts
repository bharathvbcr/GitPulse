/**
 * The one subscription to the host's `workbench-changed` event.
 *
 * Four surfaces (board, archive dock, attention inbox, Manvi ops panel) used
 * to listen for it themselves, each ignoring the payload, and each answered
 * every event with a full reload. The host also announces one write twice:
 * `{sequence}` straight from the writing command, then `{sequence: null,
 * source: "external"}` about a second later, because the watcher polls the
 * profile's `PRAGMA data_version`, which moves for this process's own commits
 * too (`src-tauri/src/workbench/external_changes.rs`).
 *
 * This hub listens once and fans out. It drops a repeated `sequence`, and the
 * first external event within `ECHO_WINDOW_MS` of an in-process one, which is
 * the echo of it. One echo is owed per burst of local writes (the watcher
 * coalesces them into one poll), so a second external event is a real one.
 * The cost of the heuristic is stated, not hidden: a write by another process
 * landing in the same poll tick as a local one is folded into the echo; the
 * window coming forward still refreshes every surface.
 */

/** About three poll ticks: the watcher polls every second while the window is shown. */
export const ECHO_WINDOW_MS = 3_000;
/** How many recent sequences are remembered to drop a repeat; bounded, oldest first. */
const SEEN_SEQUENCES = 64;

export interface WorkbenchChange {
  /** The store's commit sequence, or null for a change another process made. */
  sequence: number | null;
  source: "local" | "external";
}

export interface WorkbenchSubscriber {
  changed: (change: WorkbenchChange) => void;
  /** The subscription could not be made; the surface should say live updates are off and offer Refresh. */
  unavailable: (message: string) => void;
}

type Listen = (event: string, handler: (event: { payload: unknown }) => void) => Promise<() => void>;

export interface WorkbenchChangesDeps {
  listen: Listen;
  now?: () => number;
  echoWindowMs?: number;
  explain?: (cause: unknown) => string;
}

function readChange(payload: unknown): WorkbenchChange {
  const raw = typeof payload === "object" && payload !== null ? payload as Record<string, unknown> : {};
  const sequence = typeof raw.sequence === "number" && Number.isSafeInteger(raw.sequence) && raw.sequence >= 0 ? raw.sequence : null;
  return { sequence, source: sequence !== null && raw.source !== "external" ? "local" : "external" };
}

export function createWorkbenchChanges(deps: WorkbenchChangesDeps) {
  const now = deps.now ?? (() => Date.now());
  const echoWindowMs = deps.echoWindowMs ?? ECHO_WINDOW_MS;
  const explain = deps.explain ?? ((cause: unknown) => (cause instanceof Error ? cause.message : String(cause)));
  const subscribers = new Set<WorkbenchSubscriber>();
  let stop: (() => void) | null = null;
  /** A listen in flight; bumped on teardown so a late answer is released instead of kept. */
  let attempt = 0;
  let listening = false;
  let failure: string | null = null;
  let seen: number[] = [];
  let echoOwedUntil = 0;

  function deliver(change: WorkbenchChange): void {
    for (const subscriber of [...subscribers]) subscriber.changed(change);
  }

  function onEvent(event: { payload: unknown }): void {
    const change = readChange(event.payload);
    if (change.source === "local" && change.sequence !== null) {
      if (seen.includes(change.sequence)) return;
      seen = [...seen.slice(-(SEEN_SEQUENCES - 1)), change.sequence];
      echoOwedUntil = now() + echoWindowMs;
    } else if (echoOwedUntil > 0) {
      const echo = now() <= echoOwedUntil;
      echoOwedUntil = 0;
      if (echo) return;
    }
    deliver(change);
  }

  function start(): void {
    listening = true;
    failure = null;
    const mine = ++attempt;
    deps.listen("workbench-changed", onEvent).then((unlisten) => {
      if (mine !== attempt) { unlisten(); return; }
      stop = unlisten;
    }).catch((cause) => {
      if (mine !== attempt) return;
      listening = false;
      failure = explain(cause);
      for (const subscriber of [...subscribers]) subscriber.unavailable(failure);
    });
  }

  function teardown(): void {
    attempt++;
    listening = false;
    failure = null;
    seen = [];
    echoOwedUntil = 0;
    stop?.();
    stop = null;
  }

  return {
    /** Subscribe; the returned function unsubscribes, and the last one out releases the listener. */
    subscribe(subscriber: WorkbenchSubscriber): () => void {
      subscribers.add(subscriber);
      if (failure !== null) subscriber.unavailable(failure);
      else if (!listening) start();
      let done = false;
      return () => {
        if (done) return;
        done = true;
        subscribers.delete(subscriber);
        if (subscribers.size === 0) teardown();
      };
    },
  };
}
