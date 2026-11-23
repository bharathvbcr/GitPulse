/**
 * Thread records: what the page remembers about a Mavi thread across
 * launches, and the status a thread shows in the rail.
 *
 * A record outlives the agent process. The host's session ends with the app
 * (or when stopped); the record keeps the conversation id, so the thread can
 * be resumed with `--resume`. The timeline itself is not persisted: it is the
 * agent's view of the person's files, and Claude Code keeps the real
 * transcript. A resumed thread says so instead of pretending to remember.
 */
import type { MaviPhase, MaviSummary, MaviTarget, ThreadState } from "./types";

export const THREADS_STORAGE_KEY = "gitpulse.mavi.threads.v1";
export const MAX_THREAD_RECORDS = 60;

export interface ThreadRecord {
  /** Stable across resumes; the host's session id changes on each start. */
  readonly key: string;
  sessionId: string | null;
  title: string;
  readonly target: MaviTarget;
  mode: string;
  model: string | null;
  conversationId: string | null;
  readonly createdAt: number;
  lastActivityAt: number;
  /** When the person last had this thread open. */
  seenAt: number;
  /**
   * When the conversation was moved to a terminal tab ("Continue in
   * terminal"). While set, Mavi does not start an agent for it on its own:
   * the conversation belongs to the terminal until it is brought back.
   */
  movedToTerminalAt: number | null;
}

type StorageLike = Pick<Storage, "getItem" | "setItem">;

function isTarget(value: unknown): value is MaviTarget {
  if (!value || typeof value !== "object") return false;
  const t = value as Record<string, unknown>;
  if (t.kind === "local") return typeof t.root === "string" && t.root.length > 0;
  if (t.kind === "ssh") return typeof t.host === "string" && typeof t.dir === "string";
  return false;
}

/** Field-wise defensive parse: an unusable record is dropped, not repaired. */
export function readThreadRecords(storage: StorageLike | null): ThreadRecord[] {
  if (!storage) return [];
  try {
    const raw = storage.getItem(THREADS_STORAGE_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const out: ThreadRecord[] = [];
    for (const item of parsed) {
      if (!item || typeof item !== "object") continue;
      const r = item as Record<string, unknown>;
      if (typeof r.key !== "string" || !isTarget(r.target)) continue;
      out.push({
        key: r.key,
        // Kept so a reloaded page reattaches to an agent that is still running;
        // `MaviStore.init` drops it when the host no longer has that session
        // (it never survives an app relaunch).
        sessionId: typeof r.sessionId === "string" && r.sessionId.startsWith("mavi-") ? r.sessionId : null,
        title: typeof r.title === "string" ? r.title.slice(0, 200) : "Untitled thread",
        target: r.target,
        mode: typeof r.mode === "string" ? r.mode : "ask",
        model: typeof r.model === "string" ? r.model : null,
        conversationId: typeof r.conversationId === "string" && /^[0-9a-f-]{36}$/i.test(r.conversationId) ? r.conversationId : null,
        createdAt: typeof r.createdAt === "number" ? r.createdAt : 0,
        lastActivityAt: typeof r.lastActivityAt === "number" ? r.lastActivityAt : 0,
        seenAt: typeof r.seenAt === "number" ? r.seenAt : 0,
        movedToTerminalAt: typeof r.movedToTerminalAt === "number" ? r.movedToTerminalAt : null,
      });
      if (out.length >= MAX_THREAD_RECORDS) break;
    }
    return out;
  } catch {
    return [];
  }
}

export function writeThreadRecords(storage: StorageLike | null, records: readonly ThreadRecord[]): void {
  if (!storage) return;
  try {
    const kept = [...records].sort((a, b) => b.lastActivityAt - a.lastActivityAt).slice(0, MAX_THREAD_RECORDS);
    storage.setItem(THREADS_STORAGE_KEY, JSON.stringify(kept));
  } catch {
    /* storage full or disabled: the rail still works for this run */
  }
}

/** A short title from the first message. */
export function titleFrom(text: string): string {
  const line = text.split("\n").map((l) => l.trim()).find(Boolean) ?? "";
  const clean = line.replace(/\s+/g, " ");
  return clean.length > 64 ? `${clean.slice(0, 63)}…` : clean || "Untitled thread";
}

export function targetLabel(target: MaviTarget): string {
  if (target.kind === "ssh") return `${target.host}:${target.dir}`;
  const parts = target.root.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? target.root;
}

/**
 * What the rail shows for a thread, most urgent first. One status per thread;
 * the order is the order a person should attend to them.
 */
export type ThreadStatus =
  | "needs_you"
  | "working"
  | "failed"
  | "review"
  | "done"
  | "idle"
  | "background"
  | "not_running";

export const STATUS_PRIORITY: readonly ThreadStatus[] = [
  "needs_you",
  "working",
  "failed",
  "review",
  "done",
  "background",
  "idle",
  "not_running",
];

export const STATUS_LABEL: Record<ThreadStatus, string> = {
  needs_you: "Needs you",
  working: "Working",
  failed: "Failed",
  review: "Ready for review",
  done: "Done",
  background: "Background task",
  idle: "Idle",
  not_running: "Not running",
};

export function threadStatus(
  record: Pick<ThreadRecord, "seenAt">,
  summary: Pick<MaviSummary, "phase" | "last_activity_ms" | "background_tasks" | "exit_ok"> &
    Partial<Pick<MaviSummary, "last_turn">> | null,
  state: Pick<ThreadState, "items"> | null,
  open: boolean,
): ThreadStatus {
  if (!summary) return "not_running";
  const phase: MaviPhase = summary.phase;
  if (phase === "approval") return "needs_you";
  if (phase === "working") return "working";
  const unseen = !open && summary.last_activity_ms > record.seenAt;
  if (phase === "ended") {
    return !summary.exit_ok && unseen ? "failed" : "not_running";
  }
  const lastEnd = state ? [...state.items].reverse().find((i) => i.kind === "turn_end") : undefined;
  if (unseen && lastEnd && lastEnd.kind === "turn_end") {
    if (!lastEnd.ok && !lastEnd.interrupted) return "failed";
    if (lastEnd.summary?.category === "review_ready" || lastEnd.summary?.needsAction) return "review";
    return "done";
  }
  // A thread no page has replayed this run: the host's own account of the turn.
  if (unseen && !lastEnd && summary.last_turn === "failed") return "failed";
  if (unseen && phase === "idle") return "done";
  if (summary.background_tasks > 0) return "background";
  return "idle";
}

/** Status tone for the dot. */
export function statusTone(status: ThreadStatus): "warn" | "accent" | "danger" | "ok" | "muted" {
  switch (status) {
    case "needs_you":
      return "warn";
    case "working":
    case "background":
      return "accent";
    case "failed":
      return "danger";
    case "review":
    case "done":
      return "ok";
    default:
      return "muted";
  }
}
