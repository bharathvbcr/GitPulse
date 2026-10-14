import {
  invoke as tauriInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from "@tauri-apps/api/core";
import {
  MAX_DEFERRED_RETRIES,
  isDeferredUnderLoad,
  jitteredRetryDelayMs,
} from "../async/deferral";
import { withDeadline } from "../async/deadline";
import { whenDocumentShown } from "../runtime/foreground";
import { activity, observeSettled } from "../diagnostics/activity";
import { formatError } from "../ui/formatError";

/**
 * The one IPC entry point the frontend uses.
 *
 * Its job is the deferral (see ../async/deferral.ts). Under load the backend's
 * spawn gate declines to start git and says so; nothing ran, so the same call
 * is worth asking again a few seconds later, never at once. That used to be
 * done for the repository snapshot alone, so every other panel that was
 * declined at launch kept its failure until something else happened to ask
 * again.
 *
 * Retrying here is safe because of a backend guarantee, not a guess: a
 * command that reached the mutation guard never returns the deferral marker
 * (`run_command_scope` in src-tauri/src/engine/git_cli.rs rewrites it), so a
 * deferred call is one that changed nothing.
 *
 * While a call is waiting to retry, an identical call (same command, same
 * arguments) joins it instead of starting a second chain, so a poller cannot
 * stack retries behind one another.
 *
 * Two more things keep retries from feeding the load that caused them. Each
 * delay is jittered upward (`jitteredRetryDelayMs`), so the calls one storm
 * declined do not all come back on the same millisecond into the same empty
 * budget. And a retry waits while the window is hidden: nobody can see its
 * answer, and the spawn budget it would spend is shared with every other
 * GitPulse process of this user — the agent servers (`gitpulse-mcp`), the
 * hooks and `gitpulsed` draw from the same per-user record
 * (`engine/git_cli/shared_budget.rs`). Agent sessions themselves do not: the
 * `git` an agent runs on its own never passes through this budget.
 */
type Raw = <T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions) => Promise<T>;

/**
 * How long one caller waits, and what ends the wait early. Carried in the
 * options argument beside Tauri's own (`headers`), stripped before the call
 * is forwarded, so the backend never sees them and a call that carries only
 * these is still retried and joined like any other.
 *
 * The deadline covers the whole call as this caller sees it, deferral
 * backoff included. It stops the wait, not the native operation, so a write
 * that times out may still have landed: report it as unknown, not as failed.
 */
export type IpcBound = {
  /** Rejects with a `DeadlineError` after this many ms; `Infinity` waits unbounded. */
  deadlineMs?: number;
  /** Rejects with the signal's reason at once, and never sends the call if already aborted. */
  signal?: AbortSignal;
};
export type IpcOptions = Partial<InvokeOptions> & IpcBound;
/** What the backend is sent: exactly what the caller passed after `cmd`, with the same arity. */
type Rest = [args?: InvokeArgs, options?: InvokeOptions];
type Given = [args?: InvokeArgs, options?: IpcOptions];

/**
 * Commands that shell out to `gh` over the network and so can hang on a dead
 * connection, bounded by default so a panel shows a message instead of a
 * spinner for good. `cmd_github_context` runs four `gh` round trips (see
 * WorkspaceView), hence the longer bound. A caller's `deadlineMs` overrides.
 */
export const COMMAND_DEADLINES_MS: Readonly<Record<string, number>> = {
  cmd_github_context: 60_000,
  cmd_github_runs: 30_000,
  cmd_github_workflows: 30_000,
};

export type DeferralRetryOptions = {
  /** Stand-in for `setTimeout`-based sleeping, for tests. */
  sleep?: (ms: number) => Promise<void>;
  /** Jitter source in [0, 1); `() => 0` gives the bare backoff schedule. */
  random?: () => number;
  /** Resolves when a retry may run; defaults to "the window is shown". */
  whenShown?: () => Promise<void>;
};

/**
 * The commands whose identical in-flight calls are answered with one
 * (`sharedReads.ts`). Empty until a window registers its set.
 *
 * Injected rather than imported: the list is a set of `cmd_*` literals, and
 * this module is reached by every window's bundle. The status popover's ACL
 * grant is derived from the commands its import graph names, so importing the
 * list here would credit the popover with nine commands it never calls (and
 * would have it grant them). Only the main window's entry registers the set,
 * which is also the only window that does snapshot reads worth joining.
 */
let sharedReads: ReadonlySet<string> = new Set();

/** Registers the commands whose identical in-flight calls are joined. Replaces any earlier set. */
export function configureSharedReads(commands: ReadonlySet<string>): void {
  sharedReads = commands;
}

/**
 * The join generation for identical in-flight reads (`sharedReads.ts`). A call
 * joins a running read only if both started in the same generation.
 *
 * Module-wide, not per `withDeferralRetry` instance: what ends a generation
 * (a write through any entry point, a watcher event) is a fact about the
 * repository, not about one wrapper.
 */
let readGeneration = 0;

/**
 * Ends the current join generation: reads already running will not be joined
 * by a call made from now on. For the one place that learns the repository
 * changed outside this window's own commands (`repoStore.handleRepoChanged`,
 * where watcher events enter), called BEFORE the refresh is requested, so the
 * refresh reads after the change instead of riding a read that began before it.
 */
export function invalidateSharedReads(): void {
  readGeneration += 1;
}

/**
 * A key for calls whose arguments are plain data. `null` for anything else —
 * a `Channel`, a byte buffer, a class instance — which is neither coalesced
 * nor retried: re-sending a streaming channel or a large payload is not
 * obviously the same call.
 */
function retryKey(cmd: string, args: InvokeArgs | undefined): string | null {
  if (args === undefined) return JSON.stringify([cmd]);
  if (isBinary(args)) return null;
  let plain = true;
  const text = JSON.stringify([cmd, args], (_key, value: unknown) => {
    if (value !== null && typeof value === "object") {
      const proto = Object.getPrototypeOf(value);
      if (proto !== Object.prototype && proto !== Array.prototype && proto !== null) plain = false;
      // Canonical: the same arguments written in another key order are the
      // same call, and a key that differed only in order would let two
      // identical reads past each other.
      if (proto === Object.prototype || proto === null) {
        return Object.fromEntries(
          Object.entries(value as Record<string, unknown>).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
        );
      }
    } else if (typeof value === "function" || typeof value === "bigint" || typeof value === "symbol") {
      plain = false;
    }
    return value;
  });
  return plain ? text : null;
}

function isBinary(args: InvokeArgs | undefined): boolean {
  return args instanceof ArrayBuffer || ArrayBuffer.isView(args);
}

/**
 * `retryKey` that never throws. Arguments that cannot be serialized (a cycle,
 * a `bigint`) are simply not retryable: the call goes out as given, and a
 * deferral is its answer rather than a serializer error in its place.
 */
function lazyRetryKey(cmd: string, args: InvokeArgs | undefined): string | null {
  try {
    return retryKey(cmd, args);
  } catch {
    return null;
  }
}

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Wraps `raw` so a deferral is retried with the shared backoff. */
export function withDeferralRetry(raw: Raw, options: DeferralRetryOptions = {}) {
  const sleep = options.sleep ?? defaultSleep;
  const random = options.random ?? Math.random;
  const whenShown = options.whenShown ?? whenDocumentShown;
  const retrying = new Map<string, Promise<unknown>>();
  /** Allowlisted reads in flight, by command and canonical arguments; see `sharedReads.ts`. */
  const sharing = new Map<string, { promise: Promise<unknown>; generation: number }>();

  /**
   * One call to `raw`, always as a promise. Tauri's own `invoke` returns one;
   * a stand-in that returns a value or throws synchronously is normalised
   * here rather than breaking the `.catch` below. `Promise.resolve` hands a
   * native promise back unchanged, so this adds no microtask.
   */
  function call<T>(cmd: string, rest: Rest): Promise<T> {
    try {
      return Promise.resolve(raw<T>(cmd, ...rest));
    } catch (error) {
      return Promise.reject(error);
    }
  }

  async function retry<T>(cmd: string, rest: Rest, first: unknown): Promise<T> {
    let last = first;
    for (let attempt = 1; attempt <= MAX_DEFERRED_RETRIES; attempt += 1) {
      await sleep(jitteredRetryDelayMs(attempt, random));
      await whenShown();
      try {
        return await call<T>(cmd, rest);
      } catch (error) {
        if (!isDeferredUnderLoad(formatError(error))) throw error;
        last = error;
      }
    }
    // Still declined after the whole backoff: the deferral is the answer, and
    // the caller reports it (as a warning — see diagnostics and toastStore).
    throw last;
  }

  // Forwarded with the caller's own arity: Tauri treats an explicit
  // `undefined` like an absent argument, but a caller's test asserting the
  // exact call does not.
  //
  // Not an `async` function: each extra `await` layer delays every answer by
  // microtasks, and callers that publish an answer to a store and are read in
  // the same turn saw the delay. A call known up front not to be retryable
  // (options, a byte buffer) adds nothing here; any other adds a single
  // `.catch`, and is checked for plain data only if it is actually deferred.
  // The exported `invoke` sits one microtask above `tauriInvoke`, for the
  // stall-attribution note below.
  function dispatch<T>(cmd: string, rest: Rest): Promise<T> {
    // An identical read already in flight is the answer to this one. The
    // promise is shared as it is: each caller's deadline or abort signal wraps
    // it in `bound`, so one caller giving up detaches that caller and leaves
    // the call running for the others.
    if (sharedReads.has(cmd)) {
      if (rest.length <= 1 && !isBinary(rest[0])) {
        const key = lazyRetryKey(cmd, rest[0]);
        if (key !== null) {
          const live = sharing.get(key);
          // Only a call that started in the same generation: anything that
          // may have changed the repository since (a write, an unknown
          // command, a watcher event) makes the running read a pre-change
          // answer, which a caller asking now must not be handed.
          if (live && live.generation === readGeneration) return live.promise as Promise<T>;
          const shared = dispatchOne<T>(cmd, rest);
          sharing.set(key, { promise: shared, generation: readGeneration });
          const clear = () => {
            if (sharing.get(key)?.promise === shared) sharing.delete(key);
          };
          shared.then(clear, clear);
          return shared;
        }
      }
      return dispatchOne<T>(cmd, rest);
    }
    // Not a named read: it may change what a read would answer, so it ends
    // the generation twice, when it is sent and when it settles. A read that
    // started before either is never joined by one that starts after.
    readGeneration += 1;
    const call = dispatchOne<T>(cmd, rest);
    const settled = () => {
      readGeneration += 1;
    };
    call.then(settled, settled);
    return call;
  }

  function dispatchOne<T>(cmd: string, rest: Rest): Promise<T> {
    // A call carrying options (request headers, for raw-body commands) is
    // treated like one with non-plain arguments: neither retried nor joined.
    // So is a byte buffer; both are known without serializing anything.
    if (rest.length > 1 || isBinary(rest[0])) return call<T>(cmd, rest);
    // The key exists only to join a call already waiting to retry. With none
    // waiting there is nothing to join, so it is not computed up front: every
    // file save and terminal write would otherwise serialize its whole payload
    // for nothing. It is computed below instead, at the moment this call is
    // deferred and registers its own retry, so a later identical call still
    // finds and joins it.
    let key: string | null | undefined;
    if (retrying.size > 0) {
      key = lazyRetryKey(cmd, rest[0]);
      if (key === null) return call<T>(cmd, rest);
      const pending = retrying.get(key);
      if (pending) return pending as Promise<T>;
    }
    return call<T>(cmd, rest).catch((error: unknown) => {
      if (!isDeferredUnderLoad(formatError(error))) throw error;
      if (key === undefined) key = lazyRetryKey(cmd, rest[0]);
      // Not plain data: the deferral is the answer, exactly as if the call
      // had never been eligible for a retry.
      if (key === null) throw error;
      const settledKey = key;
      const joined = retrying.get(settledKey);
      if (joined) return joined as Promise<T>;
      const chain = retry<T>(cmd, rest, error).finally(() => {
        if (retrying.get(settledKey) === chain) retrying.delete(settledKey);
      });
      retrying.set(settledKey, chain);
      return chain;
    });
  }

  return function invoke<T>(cmd: string, ...given: Given): Promise<T> {
    if (given.length < 2) return bound(cmd, given.length === 0 ? [] : [given[0]], COMMAND_DEADLINES_MS[cmd], undefined);
    const [args, options] = given;
    if (options === undefined) return bound(cmd, [args], COMMAND_DEADLINES_MS[cmd], undefined);
    const { deadlineMs, signal, headers } = options;
    const rest: Rest = headers !== undefined ? [args, { headers }] : args === undefined ? [] : [args];
    return bound(cmd, rest, deadlineMs ?? COMMAND_DEADLINES_MS[cmd], signal);
  };

  /** `dispatch` under the caller's wait bound; a call with neither adds nothing. */
  function bound<T>(cmd: string, rest: Rest, deadlineMs: number | undefined, signal: AbortSignal | undefined): Promise<T> {
    const limited = deadlineMs !== undefined && Number.isFinite(deadlineMs);
    if (!limited && !signal) return dispatch<T>(cmd, rest);
    return withDeadline(() => dispatch<T>(cmd, rest), limited ? deadlineMs : Infinity, {
      signal,
      message: `${cmd} timed out after ${Math.round((deadlineMs ?? 0) / 1000)} s without an answer`,
    });
  }
}

// Each answer is noted for UI stall attribution (diagnostics/activity.ts).
export const invoke = withDeferralRetry(
  observeSettled(tauriInvoke, (cmd) => activity.noteCommandSettled(cmd)),
);
