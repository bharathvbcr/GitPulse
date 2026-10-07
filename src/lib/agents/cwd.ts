/**
 * Where a live terminal is, when the OS will say.
 *
 * The session registry does not carry a directory. Binding a process to the
 * repository root because the directory is unknown would put it in a checkout
 * it was never seen in. A missing, empty, or rejected answer stays unknown.
 * One session's failure does not drop the others, and a sweep that has used
 * its time does not start another read.
 */

import { readable, type Readable } from "svelte/store";
import { mapWithConcurrency } from "../async/pool";
import { invoke } from "../ipc/invoke";
import { identityKey, type PathIdentityOptions } from "../repos/paths";
import { parseTerminalContext } from "../terminal/sessionContext";
import { terminalSessions } from "../terminal/sessionRegistry";

/** How long a directory sweep may keep starting reads. */
export const AGENT_CWD_DEADLINE_MS = 4_000;
/** Reads in flight at once. Matches the rest of the workspace fan-out. */
export const AGENT_CWD_CONCURRENCY = 4;
/** Sessions past this are left unknown rather than queued. */
export const MAX_AGENT_CWD_READS = 64;

export interface AgentCwdDeps {
  readonly read?: (sessionId: string) => Promise<unknown>;
  readonly now?: () => number;
}

export interface AgentDirectorySweep extends Readable<ReadonlyMap<string, string>> {
  /** Reads every session again, for a shell that changed directory. */
  refresh(): void;
}

type DirectoryRecord = { sessionId?: string | null; label: string };

/**
 * The directories of the open sessions, by session id, read whenever the set
 * of sessions changes and someone is subscribed.
 *
 * One sweep feeds the tab bar's live-agent chip and the Agents plane, so a
 * shell sitting in an agent checkout counts the same in both whether or not
 * the plane has been opened. A title or status change leaves the set equal
 * and starts nothing; an older sweep that finishes after a newer one started
 * is dropped. A session the sweep has not read is absent: unknown, not the
 * repository root.
 */
export function createAgentDirectorySweep(
  sessions: Readable<readonly DirectoryRecord[]>,
  read: (sessionIds: readonly string[]) => Promise<Map<string, string>> = (ids) => readAgentCwds(ids),
): AgentDirectorySweep {
  let generation = 0;
  let targets: string | null = null;
  let publish: ((value: ReadonlyMap<string, string>) => void) | null = null;

  function sweep(): void {
    const set = publish;
    if (!set) return;
    const ticket = ++generation;
    const ids = targets ? targets.split("\n") : [];
    if (ids.length === 0) {
      set(new Map());
      return;
    }
    void read(ids).then(
      (found) => {
        if (ticket === generation) set(found);
      },
      () => {
        // The last answer stays. A failed sweep is not an empty one.
      },
    );
  }

  const store = readable<ReadonlyMap<string, string>>(new Map(), (set) => {
    publish = set;
    const stop = sessions.subscribe((records) => {
      const next = agentCwdTargets(records).join("\n");
      if (next === targets) return;
      targets = next;
      sweep();
    });
    return () => {
      stop();
      publish = null;
      targets = null;
      generation += 1;
    };
  });

  return { subscribe: store.subscribe, refresh: sweep };
}

/**
 * The tab chip and the Agents plane read the same sweep.
 *
 * Driving it from the chip means sessions are read when one opens, closes
 * or is relabelled, even with the plane closed. That costs one bounded sweep
 * per change to the set of sessions, never one per output line.
 */
export const agentDirectories = createAgentDirectorySweep(terminalSessions);

/**
 * The sessions whose directory is worth a read, agents first.
 *
 * A shell matters only when it sits in an agent checkout, and the cap is the
 * same for both, so shells take what the agents leave. Only the id and the
 * launcher name decide the list, so a title or status change keeps it equal.
 */
export function agentCwdTargets(
  records: readonly { sessionId?: string | null; label: string }[],
): string[] {
  const agents: string[] = [];
  const shells: string[] = [];
  for (const record of records) {
    const id = record.sessionId?.trim() ?? "";
    if (!id) continue;
    const label = record.label.trim().toLowerCase();
    (label === "" || label === "shell" ? shells : agents).push(id);
  }
  return uniqueIds([...agents, ...shells]);
}

/**
 * The entry for `path` or its nearest ancestor in an index keyed by
 * `identityKey`. A worktree holds every directory under it, so a shell in
 * `src/lib` of a checkout is in that checkout. Ancestors are cut at a
 * separator, so `/repo/alphabet` is never inside `/repo/alpha`.
 */
export function nearestContaining<T>(
  index: ReadonlyMap<string, T>,
  path: string,
  paths: PathIdentityOptions,
): T | undefined {
  let key = identityKey(path, paths);
  while (key) {
    const found = index.get(key);
    if (found !== undefined) return found;
    const cut = key.lastIndexOf("/");
    if (cut <= 0) return undefined;
    key = key.slice(0, cut);
    if (key === "/") return undefined;
  }
  return undefined;
}

function uniqueIds(sessionIds: readonly string[]): string[] {
  const seen = new Set<string>();
  const ids: string[] = [];
  for (const id of sessionIds) {
    const trimmed = id.trim();
    if (!trimmed || seen.has(trimmed)) continue;
    seen.add(trimmed);
    ids.push(trimmed);
    if (ids.length >= MAX_AGENT_CWD_READS) break;
  }
  return ids;
}

/**
 * Directories the OS reported for these sessions.
 *
 * The map contains only a non-empty directory from a payload
 * `parseTerminalContext` accepted. Anything else is absent, which the caller
 * must keep as unknown.
 */
export async function readAgentCwds(
  sessionIds: readonly string[],
  deps: AgentCwdDeps = {},
): Promise<Map<string, string>> {
  const read = deps.read ?? ((sessionId: string) => invoke<unknown>("cmd_terminal_context", { sessionId }));
  const now = deps.now ?? (() => Date.now());
  const ids = uniqueIds(sessionIds);
  const started = now();
  const found = new Map<string, string>();
  await mapWithConcurrency(ids.length, AGENT_CWD_CONCURRENCY, async (index) => {
    if (now() - started >= AGENT_CWD_DEADLINE_MS) return;
    const id = ids[index];
    try {
      const cwd = parseTerminalContext(await read(id))?.cwd?.trim() ?? "";
      if (cwd) found.set(id, cwd);
    } catch {
      // Unknown stays unknown. The next session still gets its own read.
    }
  });
  return found;
}
