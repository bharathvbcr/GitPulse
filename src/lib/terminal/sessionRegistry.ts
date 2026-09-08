import { writable } from "svelte/store";
import { MAX_TERMINAL_TABS } from "./tabs";

export interface TerminalSessionRecord {
  key: string;
  repoPath: string;
  label: string;
  status: string;
  close: () => Promise<void>;
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
