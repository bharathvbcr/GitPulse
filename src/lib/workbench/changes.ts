import { describeError } from "../ui/copy/describeFailure";
/**
 * The one subscription to the host's `workbench-changed` event.
 *
 * Four surfaces (board, archive dock, attention inbox, and the issue import now on Remote) used
 * to listen for it themselves, each ignoring the payload, and each answered
 * every event with a full reload. This hub listens once, fans out, and drops a
 * repeated `sequence`: the same in-process write announced twice.
 *
 * It does not drop the external event that follows an in-process write. The
 * host announces one write twice — `{sequence}` from the writing command,
 * then `{sequence: null, source: "external"}` about a second later, because
 * the watcher polls the profile's `PRAGMA data_version`, which moves for this
 * process's own commits too (`src-tauri/src/workbench/external_changes.rs`) —
 * but that second event cannot say whether an agent's write landed in the
 * same poll tick. Dropping it as an echo lost that write until the next commit
 * or a click into the window. Delivering it costs one more read per burst of
 * local writes, which each surface's live loader spaces (`liveLoader.ts`).
 */

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
  explain?: (cause: unknown) => string;
}

function readChange(payload: unknown): WorkbenchChange {
  const raw = typeof payload === "object" && payload !== null ? payload as Record<string, unknown> : {};
  const sequence = typeof raw.sequence === "number" && Number.isSafeInteger(raw.sequence) && raw.sequence >= 0 ? raw.sequence : null;
  return { sequence, source: sequence !== null && raw.source !== "external" ? "local" : "external" };
}

export function createWorkbenchChanges(deps: WorkbenchChangesDeps) {
  const explain = deps.explain ?? ((cause: unknown) => (describeError(cause, "workbench")));
  const subscribers = new Set<WorkbenchSubscriber>();
  let stop: (() => void) | null = null;
  /** A listen in flight; bumped on teardown so a late answer is released instead of kept. */
  let attempt = 0;
  let listening = false;
  let failure: string | null = null;
  let seen: number[] = [];

  function deliver(change: WorkbenchChange): void {
    for (const subscriber of [...subscribers]) subscriber.changed(change);
  }

  function onEvent(event: { payload: unknown }): void {
    const change = readChange(event.payload);
    if (change.source === "local" && change.sequence !== null) {
      if (seen.includes(change.sequence)) return;
      seen = [...seen.slice(-(SEEN_SEQUENCES - 1)), change.sequence];
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
