import {
  invoke as tauriInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from "@tauri-apps/api/core";
import {
  MAX_DEFERRED_RETRIES,
  deferredRetryDelayMs,
  isDeferredUnderLoad,
} from "../async/deferral";

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
 */
type Raw = <T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions) => Promise<T>;
/** Exactly what the caller passed after `cmd`, forwarded with the same arity. */
type Rest = [args?: InvokeArgs, options?: InvokeOptions];

export type DeferralRetryOptions = {
  /** Stand-in for `setTimeout`-based sleeping, for tests. */
  sleep?: (ms: number) => Promise<void>;
};

function messageOf(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

/**
 * A key for calls whose arguments are plain data. `null` for anything else —
 * a `Channel`, a byte buffer, a class instance — which is neither coalesced
 * nor retried: re-sending a streaming channel or a large payload is not
 * obviously the same call.
 */
function retryKey(cmd: string, args: InvokeArgs | undefined): string | null {
  if (args === undefined) return JSON.stringify([cmd]);
  if (args instanceof ArrayBuffer || ArrayBuffer.isView(args)) return null;
  let plain = true;
  const text = JSON.stringify([cmd, args], (_key, value: unknown) => {
    if (value !== null && typeof value === "object") {
      const proto = Object.getPrototypeOf(value);
      if (proto !== Object.prototype && proto !== Array.prototype && proto !== null) plain = false;
    } else if (typeof value === "function" || typeof value === "bigint" || typeof value === "symbol") {
      plain = false;
    }
    return value;
  });
  return plain ? text : null;
}

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Wraps `raw` so a deferral is retried with the shared backoff. */
export function withDeferralRetry(raw: Raw, options: DeferralRetryOptions = {}) {
  const sleep = options.sleep ?? defaultSleep;
  const retrying = new Map<string, Promise<unknown>>();

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
      await sleep(deferredRetryDelayMs(attempt));
      try {
        return await call<T>(cmd, rest);
      } catch (error) {
        if (!isDeferredUnderLoad(messageOf(error))) throw error;
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
  // the same turn saw the delay. A call that cannot be retried is the raw
  // promise itself; one that can adds a single `.catch`.
  return function invoke<T>(cmd: string, ...rest: Rest): Promise<T> {
    // A call carrying options (request headers, for raw-body commands) is
    // treated like one with non-plain arguments: neither retried nor joined.
    const key = rest.length > 1 ? null : retryKey(cmd, rest[0]);
    if (key === null) return call<T>(cmd, rest);
    const pending = retrying.get(key);
    if (pending) return pending as Promise<T>;
    return call<T>(cmd, rest).catch((error: unknown) => {
      if (!isDeferredUnderLoad(messageOf(error))) throw error;
      const joined = retrying.get(key);
      if (joined) return joined as Promise<T>;
      const chain = retry<T>(cmd, rest, error).finally(() => {
        if (retrying.get(key) === chain) retrying.delete(key);
      });
      retrying.set(key, chain);
      return chain;
    });
  };
}

export const invoke = withDeferralRetry(tauriInvoke);
