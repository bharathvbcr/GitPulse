import { writable } from "svelte/store";
import { MAX_TERMINAL_TABS } from "./tabs";

export interface TerminalSessionRecord {
  key: string;
  repoPath: string;
  label: string;
  status: string;
  close: () => Promise<void>;
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
          records.set(record.key, { ...record, status });
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
