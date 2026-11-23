/**
 * Mavi threads as rows of the Agents window.
 *
 * The plane's rows come from terminals, worktrees and task runs through a
 * closed projection (`agents/plane.ts`) whose invariants are about processes
 * GitPulse hosts in PTYs. A Mavi thread is neither, so it is not forced into
 * that union: it gets rows of its own, in the same columns, under the same
 * filter and repository scope, so the window counts every agent at work.
 *
 * Filter meanings, matched to the plane's:
 * - **Needs action**: waiting for an answer, failed, or ready for review.
 * - **Live**: its agent process is running.
 * - **Parallel**: never. One live thread per worktree is the host's rule, and
 *   a thread on an SSH host shares no checkout here.
 */
import type { AgentFilter } from "../agents/plane";
import type { MaviSummary } from "./types";
import { STATUS_LABEL, STATUS_PRIORITY, targetLabel, type ThreadRecord, type ThreadStatus } from "./threads";

export interface MaviAgentRow {
  readonly key: string;
  readonly title: string;
  /** The repository (local) or `host:dir` (SSH) it runs in. */
  readonly where: string;
  readonly root: string | null;
  readonly ssh: boolean;
  /** Whether its agent process is running now. */
  readonly live: boolean;
  readonly presence: string;
  readonly status: ThreadStatus;
  /** "" when nothing is asked of the person. */
  readonly attention: string;
  readonly needsAction: boolean;
  /** Files the agent edited with its own tools; null when no host session is known. */
  readonly edited: number | null;
  readonly lastActivityAt: number;
}

const ACTION: ReadonlySet<ThreadStatus> = new Set(["needs_you", "failed", "review"]);

function presenceOf(record: ThreadRecord, summary: Pick<MaviSummary, "phase"> | null): string {
  if (record.movedToTerminalAt) return "In a terminal tab";
  switch (summary?.phase) {
    case "working":
      return "Working";
    case "approval":
      return "Waiting for you";
    case "idle":
    case "ready":
      return "Running, idle";
    default:
      return record.conversationId ? "Not running (can resume)" : "Not running";
  }
}

export function maviAgentRows(
  records: readonly ThreadRecord[],
  summaryOf: (record: ThreadRecord) => Pick<MaviSummary, "phase" | "touched"> | null,
  statusOf: (record: ThreadRecord) => ThreadStatus,
  options: { filter: AgentFilter; scope: string | null; sameRepo: (a: string, b: string) => boolean },
): MaviAgentRow[] {
  const rows: MaviAgentRow[] = [];
  for (const record of records) {
    const summary = summaryOf(record);
    const root = record.target.kind === "local" ? record.target.root : null;
    if (options.scope && (!root || !options.sameRepo(root, options.scope))) continue;
    const status = statusOf(record);
    const live = !!summary && summary.phase !== "ended";
    const needsAction = ACTION.has(status);
    if (options.filter === "parallel") continue;
    if (options.filter === "live" && !live) continue;
    if (options.filter === "attention" && !needsAction) continue;
    rows.push({
      key: record.key,
      title: record.title,
      where: targetLabel(record.target),
      root,
      ssh: record.target.kind === "ssh",
      live,
      presence: presenceOf(record, summary),
      status,
      attention: needsAction ? STATUS_LABEL[status] : "",
      needsAction,
      edited: summary ? summary.touched.length : null,
      lastActivityAt: record.lastActivityAt,
    });
  }
  return rows.sort(
    (a, b) => STATUS_PRIORITY.indexOf(a.status) - STATUS_PRIORITY.indexOf(b.status) || b.lastActivityAt - a.lastActivityAt,
  );
}

/** The headline's clause for Mavi threads, or "" when there are none. */
export function maviHeadline(rows: readonly MaviAgentRow[]): string {
  if (rows.length === 0) return "";
  const live = rows.filter((r) => r.live).length;
  const waiting = rows.filter((r) => r.status === "needs_you").length;
  const parts = [`${rows.length} Mavi ${rows.length === 1 ? "thread" : "threads"}`];
  if (live > 0) parts.push(`${live} running`);
  if (waiting > 0) parts.push(`${waiting} waiting for you`);
  return parts.join(", ");
}
