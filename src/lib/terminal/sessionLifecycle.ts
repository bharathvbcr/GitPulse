import { mergeTerminalNotice, type PtyBus, type TerminalExitEvent } from "./ptyBus";
import type { TerminalSpawned } from "./runResult";
import { INPUT_HARD_MS, TerminalTimeoutError, createTerminalInput, sessionIsGone, terminalDeadline } from "./inputQueue";
import { SessionCapacityError, type createSessionRegistry } from "./sessionRegistry";
import type { LauncherKind } from "./tabs";

type Slot = ReturnType<ReturnType<typeof createSessionRegistry>["reserve"]>;

export interface SessionTransport {
  spawn(): Promise<TerminalSpawned>;
  write(id: string, data: string, binary: boolean): Promise<void>;
  resize(id: string, rows: number, cols: number): Promise<void>;
  kill(id: string): Promise<void>;
  /**
   * Leave the process running with no view (`cmd_terminal_detach`). Resolves
   * whether there was a live session to keep.
   */
  detach(id: string): Promise<boolean>;
}
export interface SessionHooks {
  state(status: "starting" | "running" | "exited" | "error", message?: string): void;
  started(session: TerminalSpawned): void;
  output(data: string, id: string, reservedBytes?: number): void;
  exit(event: TerminalExitEvent): void;
  reset(): void | Promise<void>;
  warning(message: string): void;
  /**
   * Typed input is queued behind a send the backend has not answered yet. Called
   * with the notice, and with null when that send settles. Not a warning: it
   * retracts itself and must not clear one the view is showing for another
   * reason.
   */
  slow?(message: string | null): void;
  /**
   * Output was skipped for this view (`terminal-loss`). What it had painted
   * may end mid escape sequence; the view must leave that sequence before
   * the next chunk, or the emulator can swallow what follows.
   */
  skipped?(): void;
  /** This tab now holds a session slot; called once per slot taken. */
  admitted?(): void;
  /**
   * Every session slot was taken when this tab asked for one. Return true to
   * take the refusal back — the tab will be retried later, so nothing is
   * shown — or false to have it reported as the tab's error.
   */
  refused?(message: string): boolean;
}

/** One canonical owner of a tab's native process, even after its view is gone. */
export function createSessionLifecycle(options: {
  key: string;
  repoPath: string;
  /** See `TerminalSessionRecord.checkout`. */
  checkout?: string;
  label: string;
  /** See `TerminalSessionRecord.launcher`. */
  launcher?: LauncherKind;
  bus: PtyBus;
  registry: ReturnType<typeof createSessionRegistry>;
  transport: SessionTransport;
  hooks: SessionHooks;
  /** Native spawn reconnects this durable attempt; it can never create a successor. */
  singleAttempt?: boolean;
  /** Panel-owned "bring this tab on screen"; see `TerminalSessionRecord.reveal`. */
  reveal?: () => void;
  /** Panel-owned question before a close; see `TerminalSessionRecord.confirmClose`. */
  confirmClose?: () => Promise<boolean>;
  /** See `TerminalSessionRecord.title`, `.taskRunId` and `.continuesRunId`. */
  title?: string;
  taskRunId?: string;
  continuesRunId?: string;
  /**
   * Whether this session is positively idle — a plain shell at its prompt —
   * so a view going away may end it. Anything else, including "could not
   * tell", keeps the process. Absent: every session is kept.
   */
  idle?: (sessionId: string) => Promise<boolean>;
  /**
   * Prepares the Sessions record a still-running process becomes when its
   * view goes away. Called while this lifecycle still owns the slot; the
   * returned adopter takes the slot over synchronously after the host has
   * detached the session (`detachedSessions.keepDetached`). Null, a throw,
   * or absent: this lifecycle stays the owner, listed and stoppable.
   */
  keep?: (sessionId: string) => Promise<((slot: Slot) => void) | null>;
}) {
  const { bus, registry, transport, hooks } = options;
  let id: string | null = null;
  let disposed = false;
  let starting: Promise<void> | null = null;
  let restarting: Promise<void> | null = null;
  let closing: Promise<void> | null = null;
  let subscription: (() => void) | null = null;
  let slot: ReturnType<typeof registry.reserve> | null = null;
  let input: ReturnType<typeof createTerminalInput> | null = null;
  let pendingSize: { rows: number; cols: number } | null = null;
  let resizing = false;
  /** A resize failure was already reported and none has succeeded since. */
  let resizeWarned = false;
  let attemptEnded = false;
  /** Banner text already shown. A later clean exit must not drop it. */
  let shownError: string | null = null;
  /**
   * Output that was lost on this process. A new process clears it. Closing,
   * or reconnecting this same process, must not.
   */
  let lossNotice: string | null = null;

  function state(status: "starting" | "running" | "exited" | "error", message?: string) {
    if (status === "starting" || status === "running") shownError = null;
    else if (status === "error") shownError = mergeTerminalNotice(shownError, message ?? null);
    // Publish the whole notice. A second loss must not hide the one already shown,
    // and a transport failure must not hide output that was already lost.
    const published = status === "error" ? mergeTerminalNotice(shownError, lossNotice) : message;
    slot?.update(published ?? status);
    if (!disposed) hooks.state(status, published ?? undefined);
  }
  function release() {
    input?.dispose(); input = null;
    subscription?.(); subscription = null;
    slot?.release(); slot = null;
    id = null;
    resizeWarned = false;
  }
  function fail(message: string, retain = false) {
    input?.dispose();
    // Retained failures describe bytes that are already gone. Reconnecting
    // this process does not bring them back.
    if (retain) lossNotice = mergeTerminalNotice(lossNotice, message);
    state("error", message);
  }
  async function closeCurrent() {
    if (closing) return closing;
    const target = id;
    if (!target) { release(); return; }
    input?.dispose();
    const notice = mergeTerminalNotice(shownError, lossNotice);
    slot?.update(notice ?? "closing");
    closing = terminalDeadline(transport.kill(target), 5000, "Closing terminal").then(() => {
      if (id === target) {
        attemptEnded = true;
        release();
        // Kill can finish without a terminal-exit event. A bare "exited"
        // would replace the loss the reader already saw.
        if (!disposed) {
          if (notice) state("error", notice);
          else hooks.state("exited");
        }
      }
    }).catch((error: unknown) => {
      // Keep the slot and process id: a failed kill is not a closed process.
      state("error", `Close failed; use Sessions to retry. ${String(error)}`);
      throw error;
    }).finally(() => { closing = null; });
    return closing;
  }
  async function close() {
    if (starting) await starting;
    await closeCurrent();
  }
  /**
   * The view is going away and nobody asked to close it: an unmount from a
   * repository tab closing, a pane that crashed, the dock re-hosting its
   * panels. None of those is a reason to end an agent's work. The process is
   * detached and its slot handed to Sessions (`options.keep`); only a session
   * known to be an idle shell is closed, so empty prompts do not pile up.
   * A detach that fails keeps the slot and says so: never a silent kill.
   */
  async function leave() {
    if (starting) await starting;
    if (closing) { await closing; return; }
    const target = id;
    if (!target) { release(); return; }
    let idle = false;
    try {
      idle = options.idle ? await terminalDeadline(options.idle(target), 1500, "Reading terminal context") : false;
    } catch { idle = false; }
    if (id !== target) return;
    if (idle) { await closeCurrent(); return; }
    let adopt: ((slot: Slot) => void) | null = null;
    try {
      adopt = options.keep ? await terminalDeadline(options.keep(target), 3000, "Listing terminal") : null;
    } catch { adopt = null; }
    if (id !== target) return;
    input?.dispose();
    let kept = false;
    try {
      kept = await terminalDeadline(transport.detach(target), 5000, "Detaching terminal");
    } catch (error) {
      if (id === target) slot?.update(`Still running; its view closed but it could not be detached. ${String(error)}`);
      throw error;
    }
    // Not kept: it had already ended, and its exit (still subscribed) releases.
    if (id !== target || !kept) return;
    const handed = slot;
    if (adopt && handed) {
      try {
        adopt(handed);
        slot = null;
        release();
        return;
      } catch { /* the slot was not taken; this lifecycle keeps it below */ }
    }
    slot?.update("Still running — its tab was closed");
  }
  async function launch(reconnect: boolean) {
    if (disposed || (id && !reconnect)) return;
    let ready: (() => void) | null = null;
    let watchdog: ReturnType<typeof setTimeout> | null = null;
    let timedOut = false;
    try {
      if (!slot) {
        try {
          slot = registry.reserve({
            key: options.key, repoPath: options.repoPath, label: options.label, status: "starting", close,
            ...(options.checkout ? { checkout: options.checkout } : {}),
            ...(options.launcher ? { launcher: options.launcher } : {}),
            reveal: options.reveal, confirmClose: options.confirmClose,
            ...(options.title ? { title: options.title } : {}),
            ...(options.taskRunId ? { taskRunId: options.taskRunId } : {}),
            ...(options.continuesRunId ? { continuesRunId: options.continuesRunId } : {}),
          });
        } catch (error) {
          // Every slot is taken. A caller that can wait for one takes the
          // refusal back (nothing started, nothing to report); anyone else
          // sees it as this tab's error, as before.
          if (error instanceof SessionCapacityError && hooks.refused?.(error.message)) return;
          throw error;
        }
        hooks.admitted?.();
      }
      state("starting");
      ready = await bus.prepare();
      if (disposed) { release(); return; }
      // A timeout is visible, but an unresolved spawn still owns its slot. A
      // retry must not spawn a duplicate, and a late success must be reclaimed.
      watchdog = setTimeout(() => {
        timedOut = !options.singleAttempt;
        state("error", options.singleAttempt
          ? "Task terminal launch is still pending. Reconnect waits for this same attempt."
          : "Starting terminal timed out; awaiting native cleanup");
      }, 15000);
      const spawned = await transport.spawn();
      clearTimeout(watchdog); watchdog = null;
      if (id && id !== spawned.id) throw new Error("Reconnect returned a different terminal. The existing session remains owned.");
      const reattached = id === spawned.id;
      id = spawned.id;
      // Published before the session can produce anything: a notification that
      // arrives before the registry knows this id has no tab to open.
      slot?.identify(spawned.id);
      if (timedOut) {
        await closeCurrent();
        state("error", "Starting terminal timed out; the late process was closed. Retry to start again.");
        return;
      }
      input?.dispose();
      subscription?.(); subscription = null;
      // The view went away mid-spawn. `dispose` waits for this launch and
      // then decides, so the new process is left for it, not killed here —
      // and it is not shown, typed into or journaled. It is still watched
      // below: its output is acknowledged and its exit releases its slot.
      if (!disposed) {
        input = createTerminalInput((data, binary) => transport.write(spawned.id, data, binary), (message, fatal) => {
          if (fatal) fail(message);
          else if (!disposed) hooks.warning(message);
        }, { onSlow: (message) => { if (!disposed || message === null) hooks.slow?.(message); } });
        state("running");
        if (!reattached) {
          // This id belongs to a new process. Its predecessor's loss does not.
          lossNotice = null;
          hooks.started(spawned);
        } else if (lossNotice) state("error", lossNotice);
      }
      subscription = bus.subscribe(spawned.id, {
        // Dispose must not swallow output. Rust already reserved these bytes,
        // and kill has not stopped the reader yet — dropping the chunk here
        // leaves that credit outstanding until the stall timeout. The view
        // acknowledges what it can no longer paint. After release(), `id` is
        // null and a late chunk belongs to a session the reader has finished.
        onOutput(data, reserved) {
          if (id !== spawned.id) return;
          // Omit the argument when it is absent. An explicit `undefined`
          // would fail callers that assert the two-argument delivery.
          if (typeof reserved === "number") hooks.output(data, spawned.id, reserved);
          else hooks.output(data, spawned.id);
        },
        // A lost chunk is not a dead shell. The notice stays up, and the
        // keyboard stays up, until the process itself exits.
        onError(message) {
          if (id !== spawned.id) return;
          lossNotice = mergeTerminalNotice(lossNotice, message);
          state("error", message);
        },
        // Output skipped while the view lagged. The process is fine and is
        // still running, so this is a warning, never an error status.
        onNotice(message) {
          if (id !== spawned.id || disposed) return;
          hooks.skipped?.();
          hooks.warning(message);
        },
        onExit(event) {
          if (id !== spawned.id) return;
          attemptEnded = true;
          const error = mergeTerminalNotice(mergeTerminalNotice(event.error, shownError), lossNotice);
          const delivered = error && error !== event.error ? { ...event, error } : event;
          release();
          if (!disposed) { hooks.exit(delivered); hooks.state(error ? "error" : "exited", error ?? undefined); }
        },
      });
      // An exit may have been replayed synchronously by subscribe().
      if (!id) { subscription(); subscription = null; }
      else if (!disposed) void resizeLatest();
    } catch (error) {
      state("error", String(error));
      if (!id) release();
    } finally {
      if (watchdog !== null) clearTimeout(watchdog);
      ready?.();
    }
  }
  function start(reconnect = false): Promise<void> {
    if (starting) return starting;
    if (disposed || (id && !reconnect)) return Promise.resolve();
    if (options.singleAttempt && attemptEnded) {
      state("error", "This attempt ended. Launch a new attempt from the task details.");
      return Promise.resolve();
    }
    starting = launch(reconnect).finally(() => { starting = null; });
    return starting;
  }
  /**
   * Latest wins and one is in flight at a time: sizes that arrive while a resize
   * is being answered collapse into the one pending size. A slow answer is
   * awaited without a word (a window drag on a loaded host is a stream of
   * these); only a failure is said, and once per streak, because the next
   * resize carries the whole size and repairs it. A session that is gone is
   * the exit path's to report. The bound exists so that a resize which never
   * answers cannot hold `resizing` (this lifecycle's, not one session's) for
   * good; if the abandoned resize lands late it is stale by one size, which the
   * resize that followed it has already superseded or will.
   */
  async function resizeLatest() {
    if (resizing) return;
    resizing = true;
    try {
      while (pendingSize && id && !disposed) {
        const size = pendingSize, target = id;
        pendingSize = null;
        try {
          await terminalDeadline(transport.resize(target, size.rows, size.cols), INPUT_HARD_MS, "Terminal resize", { pauseWhileHidden: true });
          resizeWarned = false;
        } catch (error) {
          if (!disposed && id === target && !sessionIsGone(error) && !resizeWarned) {
            resizeWarned = true;
            hooks.warning(error instanceof TerminalTimeoutError
              ? `Resize failed: no answer from the terminal backend for ${Math.round(error.ms / 1000)} s; the host may be overloaded.`
              : `Resize failed: ${String(error)}`);
          }
        }
      }
    } finally { resizing = false; }
  }
  return {
    start,
    restart(): Promise<void> {
      if (restarting) return restarting;
      restarting = (async () => {
        if (starting) await starting;
        if (disposed) return;
        if (options.singleAttempt) { await start(true); return; }
        await closeCurrent();
        await terminalDeadline(Promise.resolve(hooks.reset()), 5000, "Draining terminal renderer");
        await start();
      })().catch((error: unknown) => state("error", String(error))).finally(() => { restarting = null; });
      return restarting;
    },
    write: (data: string, binary = false) => input?.write(data, binary) ?? false,
    resize(rows: number, cols: number) {
      if (!Number.isFinite(rows) || !Number.isFinite(cols) || rows < 1 || cols < 1) return;
      pendingSize = { rows: Math.min(1000, Math.round(rows)), cols: Math.min(1000, Math.round(cols)) };
      void resizeLatest();
    },
    fail,
    /** The user's Close: ends the process. A view going away uses `dispose`. */
    close,
    isCurrent: (sessionId: string) => !disposed && id === sessionId,
    dispose() {
      disposed = true;
      input?.dispose();
      pendingSize = null;
      // Failures keep the slot in the global registry, with an explicit retry.
      void leave().catch(() => { /* leave/closeCurrent retained and reported ownership */ });
    },
  };
}
