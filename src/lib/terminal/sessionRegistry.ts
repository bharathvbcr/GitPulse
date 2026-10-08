import { writable } from "svelte/store";
import { currentSessionLimit } from "./sessionLimit";
import { identityKey, isCaseInsensitiveFs, type PathIdentityOptions } from "../repos/paths";
import type { LauncherKind } from "./tabs";

export interface TerminalSessionRecord {
  key: string;
  repoPath: string;
  label: string;
  /**
   * What the process is: a shell or an agent CLI. `label` is that kind as
   * display text; surfaces that decide on it (an agent chip) read this.
   * Absent only on a record a caller built without one.
   */
  launcher?: LauncherKind;
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
  /**
   * Set when the process continues an ended attempt's conversation. It names
   * that attempt so the task can list the session, and nothing more: unlike
   * `taskRunId`, closing it ends no attempt.
   */
  continuesRunId?: string;
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
 * Session counts per checkout, looked up by checkout identity.
 *
 * `get` and `has` accept any spelling of a checkout — a different case on a
 * case-insensitive volume, a trailing or doubled separator — because the
 * strip and the Agents plane treat those as one checkout (`identityKey`), and
 * a badge that did not would count one checkout as two, or miss it. The
 * entry is named by the first spelling seen, so `keys()` stays a real path.
 */
class RepoSessionCounts extends Map<string, number> {
  private readonly spellings = new Map<string, string>();

  constructor(private readonly options: PathIdentityOptions) {
    super();
  }

  add(path: string): void {
    this.set(path, (this.get(path) ?? 0) + 1);
  }

  override set(path: string, count: number): this {
    const identity = identityKey(path, this.options);
    // A blank or unreadable path names no checkout, so it is not counted.
    if (!identity) return this;
    const spelling = this.spellings.get(identity) ?? path;
    this.spellings.set(identity, spelling);
    return super.set(spelling, count);
  }

  override get(path: string): number | undefined {
    const spelling = this.spellings.get(identityKey(path, this.options));
    return spelling === undefined ? undefined : super.get(spelling);
  }

  override has(path: string): boolean {
    return this.get(path) !== undefined;
  }

  override delete(path: string): boolean {
    const identity = identityKey(path, this.options);
    const spelling = this.spellings.get(identity);
    if (spelling === undefined) return false;
    this.spellings.delete(identity);
    return super.delete(spelling);
  }

  override clear(): void {
    this.spellings.clear();
    super.clear();
  }
}

/**
 * How many live sessions each repository checkout holds.
 *
 * Matched by checkout identity, the rule the repository store and tab strip
 * use (`repos/paths.ts::identityKey`), with the same `caseInsensitive`
 * default the store takes when none is injected. It used to be exact string
 * equality on the claim that every producer hands out one spelling; a task
 * launch's checkout, an adopted session's host path and a tab's path do not
 * always agree, and the badge then counted the wrong repository.
 *
 * Pure and separate from the store so the tab bar's badge can be tested
 * without a PTY, and so "no sessions" is a value rather than a rendering
 * accident: a checkout with none has no entry, never a 0.
 */
export function sessionsByRepo(
  records: readonly Pick<TerminalSessionRecord, "repoPath">[],
  options: PathIdentityOptions = { caseInsensitive: isCaseInsensitiveFs() },
): Map<string, number> {
  const counts = new RepoSessionCounts(options);
  for (const record of records) counts.add(record.repoPath);
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

/**
 * Stops a session from outside its tab — the Sessions list, a task's Agents
 * pane — asking the tab's own question first. Resolves to whether it was
 * stopped, so a declined question is not reported as a stop.
 */
export async function closeWithConfirmation(
  record: Pick<TerminalSessionRecord, "close" | "confirmClose">,
): Promise<boolean> {
  if (!(await (record.confirmClose?.() ?? true))) return false;
  await record.close();
  return true;
}

/**
 * A reservation refused because every session slot is taken. Its own type so
 * a caller that can wait (a queued task terminal) tells it apart from a
 * reservation that can never succeed, without matching the sentence.
 */
export class SessionCapacityError extends Error {
  constructor(readonly limit: number) {
    super(`All ${limit} terminal sessions are in use across repositories`);
    this.name = "SessionCapacityError";
  }
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
      const limit = currentSessionLimit();
      if (records.size >= limit) throw new SessionCapacityError(limit);
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
