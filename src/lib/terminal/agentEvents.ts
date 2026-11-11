import { listen } from "@tauri-apps/api/event";
import { isTauri } from "../platform";
import type { EventListen } from "./ptyBus";

/** Emitted by `instance::Request::AgentSessionMeta` when `SessionStart` reports session metadata. */
export interface TerminalAgentMetaPayload {
  id: string;
  agent: string;
  conversation_id: string;
  artifact_dir: string | null;
}

/** Emitted when a parent agent session spawns a scoped child PTY (`gitpulse_terminal_spawn`). */
export interface TerminalChildSpawnedPayload {
  id: string;
  parent_session_id: string;
  repo_path: string;
  cwd: string;
  shell: string;
  launcher: string;
  title: string | null;
  split: boolean;
}

/** Emitted when a live agent session requests opening a repository file in a split pane. */
export interface TerminalOpenFileRequestPayload {
  caller_session_id: string;
  repo_path: string;
  path: string;
  line: number | null;
  column: number | null;
}

export interface PanelEventHandlers {
  onChildSpawned?: (payload: TerminalChildSpawnedPayload) => void;
  onOpenFile?: (payload: TerminalOpenFileRequestPayload) => void;
}

export const MAX_CACHED_AGENT_META = 64;

function validAgentMeta(raw: unknown): TerminalAgentMetaPayload | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    !r.id ||
    typeof r.agent !== "string" ||
    typeof r.conversation_id !== "string" ||
    !r.conversation_id
  ) {
    return null;
  }
  return {
    id: r.id,
    agent: r.agent,
    conversation_id: r.conversation_id,
    artifact_dir: typeof r.artifact_dir === "string" && r.artifact_dir.length > 0 ? r.artifact_dir : null,
  };
}

function validChildSpawned(raw: unknown): TerminalChildSpawnedPayload | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    !r.id ||
    typeof r.parent_session_id !== "string" ||
    !r.parent_session_id ||
    typeof r.repo_path !== "string" ||
    !r.repo_path
  ) {
    return null;
  }
  return {
    id: r.id,
    parent_session_id: r.parent_session_id,
    repo_path: r.repo_path,
    cwd: typeof r.cwd === "string" ? r.cwd : r.repo_path,
    shell: typeof r.shell === "string" ? r.shell : "",
    launcher: typeof r.launcher === "string" && r.launcher ? r.launcher : "shell",
    title: typeof r.title === "string" && r.title.length > 0 ? r.title : null,
    split: Boolean(r.split),
  };
}

function validOpenFileRequest(raw: unknown): TerminalOpenFileRequestPayload | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.caller_session_id !== "string" ||
    !r.caller_session_id ||
    typeof r.repo_path !== "string" ||
    !r.repo_path ||
    typeof r.path !== "string" ||
    !r.path
  ) {
    return null;
  }
  return {
    caller_session_id: r.caller_session_id,
    repo_path: r.repo_path,
    path: r.path,
    line: typeof r.line === "number" && Number.isInteger(r.line) && r.line > 0 ? r.line : null,
    column: typeof r.column === "number" && Number.isInteger(r.column) && r.column > 0 ? r.column : null,
  };
}

/**
 * Shared O(1) Tauri event router for terminal agent metadata and scoped child
 * terminal orchestration events. Buffers up to `MAX_CACHED_AGENT_META` early
 * `terminal-agent-meta` payloads so a fast `SessionStart` hook is never lost
 * while a session's `spawn` IPC is still returning.
 */
export function createAgentEventBus(listenFn: EventListen) {
  const metaSubscribers = new Map<string, Set<(payload: TerminalAgentMetaPayload) => void>>();
  const cachedMeta = new Map<string, TerminalAgentMetaPayload>();
  const panelSubscribers = new Set<PanelEventHandlers>();
  let unlisteners: Array<() => void> = [];
  let attaching: Promise<void> | null = null;

  const wanted = () => metaSubscribers.size > 0 || panelSubscribers.size > 0;

  function cacheMeta(payload: TerminalAgentMetaPayload) {
    if (cachedMeta.has(payload.id)) cachedMeta.delete(payload.id);
    while (cachedMeta.size >= MAX_CACHED_AGENT_META) {
      const oldest = cachedMeta.keys().next();
      if (oldest.done) break;
      cachedMeta.delete(oldest.value);
    }
    cachedMeta.set(payload.id, payload);
  }

  function ensureAttached(): void {
    if (unlisteners.length > 0 || attaching || !wanted()) return;
    attaching = Promise.allSettled([
      listenFn<unknown>("terminal-agent-meta", (event) => {
        const parsed = validAgentMeta(event.payload);
        if (!parsed) return;
        cacheMeta(parsed);
        const set = metaSubscribers.get(parsed.id);
        if (set) for (const fn of set) fn(parsed);
      }),
      listenFn<unknown>("terminal-child-spawned", (event) => {
        const parsed = validChildSpawned(event.payload);
        if (!parsed) return;
        for (const sub of panelSubscribers) sub.onChildSpawned?.(parsed);
      }),
      listenFn<unknown>("terminal-open-file-request", (event) => {
        const parsed = validOpenFileRequest(event.payload);
        if (!parsed) return;
        for (const sub of panelSubscribers) sub.onOpenFile?.(parsed);
      }),
    ])
      .then((results) => {
        const releases = results.flatMap((r) => (r.status === "fulfilled" ? [r.value] : []));
        if (!wanted()) {
          for (const release of releases) {
            try {
              release();
            } catch {
              /* ignore dead unlisten */
            }
          }
        } else {
          unlisteners = releases;
        }
      })
      .finally(() => {
        attaching = null;
      });
  }

  function maybeDetach(): void {
    if (wanted()) return;
    const active = unlisteners;
    unlisteners = [];
    for (const release of active) {
      try {
        release();
      } catch {
        /* ignore dead unlisten */
      }
    }
  }

  return {
    subscribeAgentMeta(
      terminalId: string,
      handler: (payload: TerminalAgentMetaPayload) => void,
    ): () => void {
      let set = metaSubscribers.get(terminalId);
      if (!set) {
        set = new Set();
        metaSubscribers.set(terminalId, set);
      }
      set.add(handler);
      ensureAttached();
      const cached = cachedMeta.get(terminalId);
      if (cached) queueMicrotask(() => {
        if (metaSubscribers.get(terminalId)?.has(handler)) handler(cached);
      });
      return () => {
        const current = metaSubscribers.get(terminalId);
        if (current) {
          current.delete(handler);
          if (current.size === 0) metaSubscribers.delete(terminalId);
        }
        maybeDetach();
      };
    },
    forgetAgentMeta(terminalId: string): void {
      cachedMeta.delete(terminalId);
    },
    subscribePanelEvents(handlers: PanelEventHandlers): () => void {
      panelSubscribers.add(handlers);
      ensureAttached();
      return () => {
        panelSubscribers.delete(handlers);
        maybeDetach();
      };
    },
  };
}

export const agentEvents = createAgentEventBus((event, handler) =>
  isTauri() ? listen(event, handler) : Promise.resolve(() => {}),
);
