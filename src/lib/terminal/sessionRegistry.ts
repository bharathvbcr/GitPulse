import { writable } from "svelte/store";
import { MAX_TERMINAL_TABS } from "./tabs";

export interface TerminalSessionRecord {
  key: string;
  repoPath: string;
  label: string;
  status: string;
  /**
   * The backend's own id for the PTY, once one has started.
   *
   * Absent while a session is still spawning, and absent for good if it never
   * did. It is what a native notification is keyed by, so this is the only
   * bridge from a banner the user clicked back to the tab that raised it —
   * `key` is a renderer invention the backend has never seen.
   */
  sessionId?: string;
  /**
   * What the session is about, when it is about something: the task title of
   * an attempt's terminal or a resumed conversation. "Claude" alone does not
   * say which of three Claude sessions is the one waiting on you.
   */
  title?: string;
  /** Set when the process is a task attempt's, which a close ends for good. */
  taskRunId?: string;
  /** Stops the process without asking; the tab stays, showing that it ended. */
  close: () => Promise<void>;
  /**
   * Asks the panel's question — the one the tab's × asks — and resolves to
   * whether to go ahead: true at once when closing would interrupt nothing,
   * otherwise the person's answer. The Sessions list asks this before
   * `close`, so stopping a working agent from there is never silent. Absent
   * only for a record whose panel wired none.
   */
  confirmClose?: () => Promise<boolean>;
  /**
   * Brings this session on screen inside its own panel: selects its tab in
   * that repository's strip and focuses the terminal.
   *
   * Owned by the panel, not the session, because focusing an xterm that sits
   * behind a hidden tab shows the user nothing. Optional so a record whose
   * panel has not wired one is still listed — a session you cannot jump to is
   * better than a session you cannot see.
   */
  reveal?: () => void;
}

/**
 * How many live sessions each repository holds, keyed by the exact
 * `repoPath` the session was started with.
 *
 * Exact string equality, not path identity: every producer of a `repoPath`
 * here — the dock's `tab.path`, a task launch's resolved `onReady` path —
 * already hands out the canonical path the repository store resolved, and
 * `TerminalPanel` matches launch requests the same way. Normalising again
 * here would invent a second identity rule for one badge.
 *
 * Pure and separate from the store so the tab bar's badge can be tested
 * without a PTY, and so "no sessions" is a value rather than a rendering
 * accident.
 */
export function sessionsByRepo(
  records: readonly Pick<TerminalSessionRecord, "repoPath">[],
): Map<string, number> {
  const counts = new Map<string, number>();
  for (const record of records) {
    if (!record.repoPath) continue;
    counts.set(record.repoPath, (counts.get(record.repoPath) ?? 0) + 1);
  }
  return counts;
}

/**
 * The tab that owns a backend PTY id, or null.
 *
 * Null for an id nothing is running — a banner for a session that has since
 * ended is the ordinary case, not an error, and the caller shows the terminal
 * rather than inventing a tab.
 */
export function sessionByNativeId(
  records: readonly TerminalSessionRecord[],
  sessionId: string,
): TerminalSessionRecord | null {
  if (!sessionId) return null;
  return records.find((record) => record.sessionId === sessionId) ?? null;
}

/** Capacity belongs to the app, including starts and closes still in flight. */
export function createSessionRegistry() {
  const records = new Map<string, TerminalSessionRecord>();
  const store = writable<TerminalSessionRecord[]>([]);
  const publish = () => store.set([...records.values()]);
  return {
    subscribe: store.subscribe,
    reserve(record: TerminalSessionRecord) {
      if (records.has(record.key)) throw new Error("This terminal already owns a session slot");
      if (records.size >= MAX_TERMINAL_TABS) throw new Error(`All ${MAX_TERMINAL_TABS} terminal sessions are in use across repositories`);
      records.set(record.key, record);
      publish();
      let released = false;
      return {
        update(status: string) {
          if (released) return;
          const current = records.get(record.key) ?? record;
          records.set(record.key, { ...current, status });
          publish();
        },
        /**
         * Records the backend id once the PTY has one.
         *
         * One record per native session: any other record holding this id
         * gives way. A session a reloaded page left running is adopted as a
         * record of its own, and a task tab can take that same process over
         * through its own launch path ("Open terminal"), never through the
         * adopted record. Without this the session was listed twice, held
         * two slots against the shared limit, and the stale row's Close
         * stopped the live tab's process.
         */
        identify(sessionId: string) {
          if (released || !sessionId) return;
          const current = records.get(record.key) ?? record;
          if (current.sessionId === sessionId) return;
          for (const [key, other] of records) {
            if (key !== record.key && other.sessionId === sessionId) records.delete(key);
          }
          records.set(record.key, { ...current, sessionId });
          publish();
        },
        release() {
          if (released) return;
          released = true;
          records.delete(record.key);
          publish();
        },
      };
    },
  };
}
export const terminalSessions = createSessionRegistry();
