import {
  invoke as tauriInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from "@tauri-apps/api/core";
import { MAX_DEFERRED_RETRIES, jitteredRetryDelayMs } from "../async/deferral";
import { withDeadline } from "../async/deadline";
import { whenDocumentShown } from "../runtime/foreground";
import { activity, observeSettled } from "../diagnostics/activity";

/**
 * What a command's error is, as far as the frontend acts on it. Mirrors
 * `IpcErrorKind` in src-tauri/src/ipc_error.rs, which
 * scripts/enum-variant-contract.test.ts pins in both directions.
 */
export const IPC_ERROR_KINDS = [
  "unclassified",
  "deferred_under_load",
  "repository_trust_required",
  "identity_missing",
  "stale_stash",
  "ledger_not_initialised",
  "ledger_failed",
  "ai_endpoint_unconfirmed",
] as const;

export type IpcErrorKind = (typeof IPC_ERROR_KINDS)[number];

/**
 * A command's error: `{ kind, message, detail? }` on the wire (`IpcError` in
 * src-tauri/src/ipc_error.rs), this class once it is read.
 *
 * An `Error`, so every `err instanceof Error ? err.message : String(err)` a
 * caller already writes shows the message, and `toString` is the message
 * itself rather than `IpcError: ...`, so `String(err)` and a template literal
 * show what the string rejection showed. `message` is byte-for-byte the text
 * the backend used to send.
 */
export class IpcError extends Error {
  readonly kind: IpcErrorKind;
  readonly detail: unknown;

  constructor(kind: IpcErrorKind, message: string, detail?: unknown) {
    super(message);
    this.name = "IpcError";
    this.kind = kind;
    this.detail = detail;
  }

  override toString(): string {
    return this.message;
  }
}

/**
 * The one guard: reads a rejection as an `IpcError`, or null when it is not
 * one. A string (a command not yet typed, or a stand-in in a test), an `Error`,
 * a `{ code, message }` payload of a command with its own error type, and an
 * object naming a kind this build does not know are all null: the caller shows
 * them and does not branch on them.
 *
 * Nothing here, or anywhere else, reads inside a message. `unclassified` is a
 * kind like any other and means the message is for a person only.
 */
export function readIpcError(payload: unknown): IpcError | null {
  if (payload instanceof IpcError) return payload;
  if (payload === null || typeof payload !== "object" || payload instanceof Error) return null;
  const { kind, message, detail } = payload as Record<string, unknown>;
  if (typeof kind !== "string" || typeof message !== "string") return null;
  if (!(IPC_ERROR_KINDS as readonly string[]).includes(kind)) return null;
  return new IpcError(kind as IpcErrorKind, message, detail);
}

/** The kind of a rejection: `unclassified` for anything that is not an `IpcError`. */
export function ipcErrorKind(error: unknown): IpcErrorKind {
  return readIpcError(error)?.kind ?? "unclassified";
}

/** True when the rejection is the backend declining to start git under load. */
function isDeferred(error: unknown): boolean {
  return ipcErrorKind(error) === "deferred_under_load";
}

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
 * The reads that take a `requestId`, and the command that stops one by it
 * (`sharedReads.ts`). Injected for the same reason as `sharedReads`: the
 * command names are `cmd_*` literals this module must not carry into the
 * status popover's bundle. Empty, a call is never given an id.
 */
let cancellable: ReadonlySet<string> = new Set();
let cancelWith: CancelRead | null = null;

/** Sends the cancel for `requestId` through `send`; see `cancelRead` in `sharedReads.ts`. */
type CancelRead = (send: (cmd: string, args: { requestId: string }) => Promise<unknown>, requestId: string) => Promise<unknown>;

/** Registers the reads that can be stopped and the call that stops one. Replaces any earlier registration. */
export function configureReadCancellation(commands: ReadonlySet<string>, cancel: CancelRead | null): void {
  cancellable = commands;
  cancelWith = cancel;
}

/**
 * Ids name a read to the backend's registry, which any view of this window
 * (and, if ids collided, another window) can reach. A per-load random scope
 * plus a counter keeps one view from naming another's read by accident: ids
 * never repeat within a window and do not repeat across windows or reloads.
 * They are not a secret and need not be: cancelling a read only makes its
 * answer an error for a caller that had already given up on it.
 */
const requestScope = (() => {
  try {
    return Array.from(crypto.getRandomValues(new Uint32Array(2)), (n) => n.toString(16).padStart(8, "0")).join("");
  } catch {
    return `${Date.now().toString(16)}${Math.floor(Math.random() * 0xffffffff).toString(16)}`;
  }
})();
let requestCounter = 0;
function newRequestId(): string {
  requestCounter += 1;
  return `r${requestScope}-${requestCounter.toString(36)}`;
}

/**
 * The callers still waiting on one backend call, and what it takes to stop it.
 *
 * A Set so every place that already joins callers by adding their `Interest`
 * (the shared-read join, the retry join) registers them here without change.
 * The backend call is stopped when the last interested caller has left, and
 * never before: a caller that joined a running call keeps it alive.
 */
class Waiters extends Set<Interest> {
  /** The id the backend call was sent under; null for a command that cannot be stopped. */
  readonly id: string | null;
  /** Whether the call has been put on the wire under `id`. */
  sent = false;
  /** Whether the call has answered or failed. */
  settled = false;
  /** Whether a cancel has been sent: a caller arriving now must not join this call. */
  cancelled = false;
  private readonly stop: (id: string) => void;

  constructor(id: string | null, stop: (id: string) => void) {
    super();
    this.id = id;
    this.stop = stop;
  }

  override add(interest: Interest): this {
    super.add(interest);
    let owners = waitersOf.get(interest);
    if (!owners) waitersOf.set(interest, (owners = new Set()));
    owners.add(this);
    return this;
  }

  /** `rest` as it goes on the wire: with the id, when this call has one. */
  arm(rest: Rest): Rest {
    if (this.id === null) return rest;
    this.sent = true;
    return [{ ...(rest[0] as Record<string, unknown>), requestId: this.id }];
  }

  /** Sends the cancel when the call is on the wire and no caller is left waiting. */
  stopIfAbandoned(): void {
    if (this.id === null || !this.sent || this.settled || this.cancelled) return;
    for (const interest of this) if (interest()) return;
    this.cancelled = true;
    this.stop(this.id);
  }
}

const waitersOf = new WeakMap<Interest, Set<Waiters>>();
/** The callers that carry an abort signal, i.e. can leave. */
const signalled = new WeakSet<Interest>();

/**
 * A caller has stopped waiting. The calls it was waiting on are looked at one
 * task later, not now: the caller that replaced it (a newer click on the same
 * read) joins the running call in a microtask after being issued, and must be
 * counted before the call is judged abandoned.
 */
function callerLeft(interest: Interest): void {
  setTimeout(() => {
    for (const waiters of waitersOf.get(interest) ?? []) waiters.stopIfAbandoned();
  }, 0);
}

/** Whether `rest` is one plain-object argument, the only shape an id can be added to. */
function carriesNamedArguments(rest: Rest): boolean {
  const [args] = rest;
  return rest.length === 1 && typeof args === "object" && args !== null && Object.getPrototypeOf(args) === Object.prototype && !("requestId" in args);
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

/** Whether a caller is still waiting for the answer. */
type Interest = () => boolean;
const ALWAYS_INTERESTED: Interest = () => true;

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Wraps `raw` so a deferral is retried with the shared backoff. */
export function withDeferralRetry(raw: Raw, options: DeferralRetryOptions = {}) {
  const sleep = options.sleep ?? defaultSleep;
  const random = options.random ?? Math.random;
  const whenShown = options.whenShown ?? whenDocumentShown;
  /**
   * Calls waiting to retry, with the callers still waiting on each. A retry is
   * sent only while one of them is, so a caller that has aborted or run out of
   * time never causes another command to be sent on its account.
   */
  const retrying = new Map<string, { chain: Promise<unknown>; interests: Set<Interest> }>();
  /** Allowlisted reads in flight, by command and canonical arguments; see `sharedReads.ts`. */
  const sharing = new Map<string, { promise: Promise<unknown>; generation: number; interests: Set<Interest> }>();

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

  async function retry<T>(cmd: string, rest: Rest, first: unknown, interests: Set<Interest>): Promise<T> {
    let last = first;
    for (let attempt = 1; attempt <= MAX_DEFERRED_RETRIES; attempt += 1) {
      await sleep(jitteredRetryDelayMs(attempt, random));
      await whenShown();
      // Checked after every wait, immediately before the send: the backoff and
      // a hidden window are long, and a caller that aborted or timed out in
      // them has been answered already. Nobody left means nothing is sent.
      if (![...interests].some((interested) => interested())) throw last;
      try {
        return await call<T>(cmd, rest);
      } catch (error) {
        if (!isDeferred(error)) throw error;
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
  function dispatch<T>(cmd: string, rest: Rest, interest: Interest): Promise<T> {
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
          //
          // Not one that has been told to stop: its answer will be the
          // cancellation, which a caller still waiting must not receive.
          if (live && live.generation === readGeneration && !(live.interests instanceof Waiters && live.interests.cancelled)) {
            live.interests.add(interest);
            return live.promise as Promise<T>;
          }
          const interests = openWaiters(cmd, rest, interest);
          const shared = dispatchOne<T>(cmd, rest, interests);
          sharing.set(key, { promise: shared, generation: readGeneration, interests });
          const clear = () => {
            if (sharing.get(key)?.promise === shared) sharing.delete(key);
          };
          shared.then(clear, clear);
          return shared;
        }
      }
      return dispatchOne<T>(cmd, rest, openWaiters(cmd, rest, interest));
    }
    // Not a named read: it may change what a read would answer, so it ends
    // the generation twice, when it is sent and when it settles. A read that
    // started before either is never joined by one that starts after.
    readGeneration += 1;
    const call = dispatchOne<T>(cmd, rest, openWaiters(cmd, rest, interest));
    const settled = () => {
      readGeneration += 1;
    };
    call.then(settled, settled);
    return call;
  }

  /** The callers waiting on one new backend call, led by `interest`; see {@link Waiters}. */
  function openWaiters(cmd: string, rest: Rest, interest: Interest): Waiters {
    // Only a caller that can leave (it carries an abort signal) makes an id
    // worth sending; every other call goes out exactly as it always did.
    const id = signalled.has(interest) && cancellable.has(cmd) && cancelWith !== null && carriesNamedArguments(rest) ? newRequestId() : null;
    // Fire and forget: the cancel is an optimisation. It goes straight to
    // `call`, not through `dispatch`, so it neither ends the join generation
    // nor is retried; a lost cancel only lets the read finish.
    const cancel = cancelWith;
    const stop = (requestId: string) => {
      cancel?.((name, args) => call(name, [args]), requestId)?.catch(() => undefined);
    };
    return new Waiters(id, stop).add(interest);
  }

  function dispatchOne<T>(cmd: string, rest: Rest, interests: Set<Interest>): Promise<T> {
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
      if (pending) {
        for (const interest of interests) pending.interests.add(interest);
        return pending.chain as Promise<T>;
      }
    }
    // The key above is of the arguments as the caller gave them; the id, when
    // the call has one, is added only to what goes on the wire.
    const waiters = interests instanceof Waiters ? interests : null;
    const wire = waiters ? waiters.arm(rest) : rest;
    const result = call<T>(cmd, wire).catch((error: unknown) => {
      if (!isDeferred(error)) throw error;
      if (key === undefined) key = lazyRetryKey(cmd, rest[0]);
      // Not plain data: the deferral is the answer, exactly as if the call
      // had never been eligible for a retry.
      if (key === null) throw error;
      const settledKey = key;
      const joined = retrying.get(settledKey);
      if (joined) {
        for (const interest of interests) joined.interests.add(interest);
        return joined.chain as Promise<T>;
      }
      const chain = retry<T>(cmd, wire, error, interests).finally(() => {
        if (retrying.get(settledKey)?.chain === chain) retrying.delete(settledKey);
      });
      retrying.set(settledKey, { chain, interests });
      return chain;
    });
    if (waiters) {
      const done = () => {
        waiters.settled = true;
      };
      result.then(done, done);
    }
    return result;
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
    if (!limited && !signal) return dispatch<T>(cmd, rest, ALWAYS_INTERESTED);
    // Still waiting: not aborted and inside the deadline. The deferral retry
    // asks this before every send (see `retry`), so a caller that has been
    // answered with an `AbortError` or a `DeadlineError` is never the reason
    // another command goes out.
    const startedAt = Date.now();
    const interest: Interest = () => !signal?.aborted && !(limited && Date.now() - startedAt >= (deadlineMs as number));
    const answer = withDeadline(() => dispatch<T>(cmd, rest, interest), limited ? deadlineMs : Infinity, {
      signal,
      message: `${cmd} timed out after ${Math.round((deadlineMs ?? 0) / 1000)} s without an answer`,
    });
    if (signal) {
      signalled.add(interest);
      // An abort is this caller leaving. When it was the last one waiting on a
      // read the backend can stop, the backend is told (see `Waiters`). A
      // deadline passing is also leaving but has no event to hear: that read
      // is simply not stopped early.
      const left = () => callerLeft(interest);
      signal.addEventListener("abort", left, { once: true });
      const done = () => signal.removeEventListener("abort", left);
      answer.then(done, done);
    }
    return answer;
  }
}

// Each answer is noted for UI stall attribution (diagnostics/activity.ts), and
// a rejection leaves here as an `IpcError` when the backend sent one.
export const invoke = withDeferralRetry(
  observeSettled(
    tauriInvoke,
    (cmd) => activity.noteCommandSettled(cmd),
    (error) => readIpcError(error) ?? error,
  ),
);
