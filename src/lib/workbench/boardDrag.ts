import { STATUSES, type TaskCard, type TaskStatus } from "./client";

/** Pointer movement (px²) before a press becomes a card drag. */
export const TASK_DRAG_THRESHOLD_PX = 6;

export function dragExceeded(dx: number, dy: number, threshold = TASK_DRAG_THRESHOLD_PX): boolean {
  return dx * dx + dy * dy >= threshold * threshold;
}

/** Where a card was held, and the size its drag preview is drawn at. */
export interface GrabPoint {
  dx: number;
  dy: number;
  width: number;
  height: number;
}

const extent = (value: number) => (Number.isFinite(value) && value > 0 ? value : 0);
const within = (value: number, max: number) => (Number.isFinite(value) ? Math.min(Math.max(value, 0), max) : 0);

/**
 * The press point inside the card's box. The preview is drawn at
 * `pointer - (dx, dy)` with the card's own size, so the spot the card was
 * picked up by stays under the pointer for the whole drag. Clamped into the
 * box, so a degenerate or stale measurement can never place the preview
 * away from the pointer.
 */
export function grabPoint(
  press: { x: number; y: number },
  box: { left: number; top: number; width: number; height: number },
): GrabPoint {
  const width = extent(box.width), height = extent(box.height);
  return { dx: within(press.x - box.left, width), dy: within(press.y - box.top, height), width, height };
}

export function parseColumnStatus(value: string | null | undefined): TaskStatus | null {
  return STATUSES.find((status) => status === value) ?? null;
}

export function neighborStatus(status: TaskStatus, delta: -1 | 1): TaskStatus | null {
  const index = STATUSES.indexOf(status) + delta;
  if (index < 0 || index >= STATUSES.length) return null;
  return STATUSES[index];
}

/**
 * Integer position for `items.put`. `items.list` is `ORDER BY position,id`.
 * When both neighbors exist the result is strictly between them; it is never
 * a `Date.now()` append in that case. Empty-column / end-of-column may use
 * `now` or `before + 1`.
 *
 * `more` says the column has cards beyond the ones loaded. Then a slot with
 * no card after it is only the end of what is loaded: `before + 1` would sort
 * after page one and the card would vanish on the next read. The answer is
 * `null`, and the caller reads the rest of the column before asking again.
 */
export function insertionPosition(before: number | null, after: number | null, now?: number, more?: false): number;
export function insertionPosition(before: number | null, after: number | null, now: number | undefined, more: boolean): number | null;
export function insertionPosition(
  before: number | null,
  after: number | null,
  now = Date.now(),
  more = false,
): number | null {
  if (after == null && more) return null;
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
