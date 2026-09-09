import { STATUSES, type TaskCard, type TaskStatus } from "./client";

/** Pointer movement (px²) before a press becomes a card drag. */
export const TASK_DRAG_THRESHOLD_PX = 6;

export const PRIORITY_LABELS = ["Urgent", "High", "Normal", "Low"] as const;

export function dragExceeded(dx: number, dy: number, threshold = TASK_DRAG_THRESHOLD_PX): boolean {
  return dx * dx + dy * dy >= threshold * threshold;
}

export function parseColumnStatus(value: string | null | undefined): TaskStatus | null {
  return STATUSES.find((status) => status === value) ?? null;
}

export function neighborStatus(status: TaskStatus, delta: -1 | 1): TaskStatus | null {
  const index = STATUSES.indexOf(status) + delta;
  if (index < 0 || index >= STATUSES.length) return null;
  return STATUSES[index];
}

/** Cross-column status change. Same-column reorder uses `shouldCommitMove`. */
export function dropTargetStatus(
  from: TaskStatus,
  over: TaskStatus | null,
  options: { moving: boolean },
): TaskStatus | null {
  if (options.moving || over == null || over === from) return null;
  return over;
}

/**
 * Integer position for `items.put`. `items.list` is `ORDER BY position,id`.
 * When both neighbors exist the result is strictly between them; it is never
 * a `Date.now()` append in that case. Empty-column / end-of-column may use
 * `now` or `before + 1`.
 */
export function insertionPosition(
  before: number | null,
  after: number | null,
  now = Date.now(),
): number {
  if (before == null && after == null) return now;
  if (before == null) return after != null && after > 0 ? Math.floor(after / 2) : 0;
  if (after == null) return before + 1;
  if (after > before + 1) return Math.floor((before + after) / 2);
  return before;
}

export function insertionNeighbors(
  items: readonly { id: string; position: number }[],
  draggedId: string,
  insertIndex: number,
): { before: number | null; after: number | null } {
  const rest = items.filter((item) => item.id !== draggedId);
  const index = Math.max(0, Math.min(insertIndex, rest.length));
  return {
    before: index === 0 ? null : rest[index - 1].position,
    after: index >= rest.length ? null : rest[index].position,
  };
}

/** Mid-Y of each remaining card, top to bottom; returns the insert index. */
export function insertIndexFromY(mids: readonly number[], y: number): number {
  for (let i = 0; i < mids.length; i++) {
    if (y < mids[i]) return i;
  }
  return mids.length;
}

export function shouldCommitMove(
  from: TaskStatus,
  over: TaskStatus | null,
  options: { moving: boolean; fromIndex: number; insertIndex: number },
): over is TaskStatus {
  if (options.moving || over == null) return false;
  if (over !== from) return true;
  return options.insertIndex !== options.fromIndex;
}

/** Occupied columns while idle; every status (empty drop targets) while dragging. */
export function visibleStatuses(
  counts: Partial<Record<TaskStatus, number>>,
  dragging: boolean,
): TaskStatus[] {
  const occupied = STATUSES.filter((status) => (counts[status] ?? 0) > 0);
  if (dragging || occupied.length === 0) return [...STATUSES];
  return occupied;
}

export interface CardFace {
  title: string;
  pip: 0 | 1 | null;
  repo: string | null;
  labels: string[];
}

export function cardFace(
  card: Pick<TaskCard, "title" | "priority" | "repository_ids" | "labels">,
  repoName: (id: string) => string | undefined,
  labelCap = 2,
): CardFace {
  const id = card.repository_ids[0];
  return {
    title: card.title,
    pip: card.priority === 0 || card.priority === 1 ? card.priority : null,
    repo: id ? (repoName(id) ?? id) : null,
    labels: card.labels.slice(0, labelCap),
  };
}
