import type { InvokeArgs, InvokeOptions } from "@tauri-apps/api/core";

/**
 * What the renderer handled recently, so a late UI tick can name a cause.
 *
 * The responsiveness probe sees only that its timer ran late. The work that
 * made it late ran on the same thread inside that interval: the continuation
 * of an IPC answer, a `repo-changed` handler, or the visible view rendering on
 * its own. This log records the first two as they are delivered and the
 * active view as a getter; `attribute` reads one interval back.
 *
 * Attribution is correlation within the late interval, not a profile: work
 * that started just before the interval and ran through it is credited to
 * whatever was delivered beside it, or to the view. Labels are command names
 * and view names only — never arguments, paths or results.
 */

/** Delivered events retained. A burst larger than this is reported as at least this many. */
export const MAX_ACTIVITY_EVENTS = 256;
/** Commands named in one attribution; the rest are counted. */
export const MAX_NAMED_COMMANDS = 4;

type Delivered = { at: number; command: string | null };

export interface StallAttribution {
  /** `command:<name>`, `watcher-burst`, or `view:<name>`. Always set. */
  cause: string;
  view: string;
  /** Commands whose answers were handled in the interval, most frequent first. */
  commands: Array<[name: string, count: number]>;
  /** Further distinct commands not listed in `commands`. */
  otherCommands: number;
  watcherEvents: number;
  /** True when the log wrapped inside the interval, so counts are lower bounds. */
  partial: boolean;
}

export interface ActivityLog {
  noteCommandSettled(command: string): void;
  noteWatcherEvent(): void;
  attribute(from: number, to: number, view: string): StallAttribution;
}

export function createActivityLog(now: () => number): ActivityLog {
  const ring: Array<Delivered | undefined> = new Array(MAX_ACTIVITY_EVENTS);
  let next = 0;
  let written = 0;

  function push(command: string | null): void {
    ring[next] = { at: now(), command };
    next = (next + 1) % MAX_ACTIVITY_EVENTS;
    written += 1;
  }

  return {
    noteCommandSettled: (command) => push(command),
    noteWatcherEvent: () => push(null),
    attribute(from, to, view) {
      const counts = new Map<string, number>();
      let watcherEvents = 0;
      let oldestInWindow = true;
      const held = Math.min(written, MAX_ACTIVITY_EVENTS);
      for (let i = 1; i <= held; i += 1) {
        const event = ring[(next - i + MAX_ACTIVITY_EVENTS) % MAX_ACTIVITY_EVENTS]!;
        if (event.at > to) continue;
        if (event.at < from) { oldestInWindow = false; break; }
        if (event.command === null) watcherEvents += 1;
        else counts.set(event.command, (counts.get(event.command) ?? 0) + 1);
      }
      // Every retained event fell inside the interval and older ones were
      // overwritten: the interval may hold more than the ring could keep.
      const partial = oldestInWindow && written > MAX_ACTIVITY_EVENTS;
      const ranked = [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
      const settled = ranked.reduce((sum, [, n]) => sum + n, 0);
      const name = view || "unknown";
      let cause = `view:${name}`;
      if (watcherEvents > 0 && watcherEvents >= settled) cause = "watcher-burst";
      else if (ranked.length > 0) cause = `command:${ranked[0][0]}`;
      return {
        cause,
        view: name,
        commands: ranked.slice(0, MAX_NAMED_COMMANDS),
        otherCommands: Math.max(0, ranked.length - MAX_NAMED_COMMANDS),
        watcherEvents,
        partial,
      };
    },
  };
}

/** The renderer's log; the probe and the IPC entry point share it. */
export const activity: ActivityLog = createActivityLog(() => performance.now());

type Raw = <T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions) => Promise<T>;

/**
 * Wraps an IPC call so each answer is noted as it is handled.
 *
 * The answer is forwarded through the derived promise instead of observed
 * with a side `.then`: any reaction marks a promise handled, and a side
 * observer would silence the unhandled-rejection diagnostics of every caller
 * that forgot its `catch`. Forwarding costs the caller exactly one microtask
 * (pinned by activity.test.ts). Tauri's own `invoke` cannot be hooked below
 * this: it is an `async` wrapper over a non-writable `__TAURI_INTERNALS__`.
 */
export function observeSettled(raw: Raw, onSettled: (command: string) => void): Raw {
  return <T>(cmd: string, ...rest: [args?: InvokeArgs, options?: InvokeOptions]): Promise<T> => {
    let pending: Promise<T>;
    try {
      pending = Promise.resolve(raw<T>(cmd, ...rest));
    } catch (error) {
      onSettled(cmd);
      return Promise.reject(error);
    }
    return pending.then(
      (value) => { onSettled(cmd); return value; },
      (error: unknown) => { onSettled(cmd); throw error; },
    );
  };
}
