/**
 * How the Tasks board is drawn: layout, density, which columns are on screen,
 * and which chips a card carries.
 *
 * These are reader preferences, so they live beside the other preference
 * vocabularies (`accents`, `codeDisplay`, `statusBarMode`) rather than in the
 * board component — `interfaceStore` persists them and the board reads them.
 *
 * Every reader here treats stored input as hostile: an array from
 * `localStorage` may have been written by another build, hand-edited, or
 * truncated mid-write. The sanitizers below never throw and never return a
 * shape the board cannot render.
 *
 * One rule runs through the whole file: **hiding is cosmetic, never silent.**
 * A hidden column still exists, still holds tasks, and still accepts a Move
 * from the context menu. `hiddenColumnReport` exists so the board can say how
 * much is off screen instead of letting a reader mistake a filtered board for
 * an empty one.
 */

import { STATUSES, STATUS_LABELS, asTaskStatus, type TaskStatus } from "../workbench/vocabulary";

export type BoardLayout = "board" | "list";
export type TaskDensity = "comfortable" | "compact";

/**
 * Optional chips on a board card. Title and priority are not in this list:
 * a card without a title is not a card, and the priority pip is the only
 * urgent/high signal on the board.
 */
export const TASK_CARD_FIELDS = ["repo", "type", "owner", "due", "labels"] as const;
export type TaskCardField = (typeof TASK_CARD_FIELDS)[number];

export const TASK_CARD_FIELD_LABELS: Record<TaskCardField, string> = {
  repo: "Repository",
  type: "Type",
  owner: "Owner",
  due: "Due date",
  labels: "Labels",
};

/** What a fresh profile shows: everything the previous board showed. */
export const DEFAULT_TASK_CARD_FIELDS: readonly TaskCardField[] = TASK_CARD_FIELDS;

export function isBoardLayout(value: unknown): value is BoardLayout {
  return value === "board" || value === "list";
}

export function isTaskDensity(value: unknown): value is TaskDensity {
  return value === "comfortable" || value === "compact";
}

export function isTaskCardField(value: unknown): value is TaskCardField {
  return TASK_CARD_FIELDS.some((field) => field === value);
}

/**
 * Card fields from storage, in the canonical order.
 *
 * Order comes from `TASK_CARD_FIELDS`, not from the stored array, so a
 * reordered or duplicated list cannot change how a card reads.
 *
 * Three stored shapes mean three different things, and collapsing them is how
 * a reader ends up staring at cards that carry nothing but a title with no
 * way to tell whether they asked for that:
 *
 * * not an array — nothing was ever written. Use the defaults.
 * * an empty array — the reader turned every chip off. Honour it.
 * * a non-empty array in which nothing is recognizable — written by a build
 *   whose field names this one does not have. That is drift, not a choice, so
 *   it falls back to the defaults rather than silently blanking every card.
 *
 * A partially recognizable array keeps the part this build understands.
 */
export function sanitizeCardFields(value: unknown): TaskCardField[] {
  if (!Array.isArray(value)) return [...DEFAULT_TASK_CARD_FIELDS];
  if (value.length === 0) return [];
  const chosen = new Set(value.filter(isTaskCardField));
  if (chosen.size === 0) return [...DEFAULT_TASK_CARD_FIELDS];
  return TASK_CARD_FIELDS.filter((field) => chosen.has(field));
}

/**
 * Hidden statuses from storage.
 *
 * Refuses to hide every column: a board with no columns has no drop target,
 * no "new task in column" action and nothing to read, and a stored value that
 * produced one would be unrecoverable from the board itself. Hiding five of
 * six is allowed; hiding all six falls back to hiding none.
 */
export function sanitizeHiddenStatuses(value: unknown): TaskStatus[] {
  if (!Array.isArray(value)) return [];
  const hidden = new Set<TaskStatus>();
  for (const entry of value) {
    const status = asTaskStatus(entry);
    if (status) hidden.add(status);
  }
  if (hidden.size >= STATUSES.length) return [];
  return STATUSES.filter((status) => hidden.has(status));
}

/**
 * Whether hiding `status` is a move the board can honour.
 *
 * The menu asks before drawing the row, so the one column still standing is
 * marked un-hideable rather than offered and then silently refused. Un-hiding
 * is always allowed, so a status already hidden answers true.
 */
export function canHideStatus(hidden: readonly TaskStatus[], status: TaskStatus): boolean {
  const current = new Set(sanitizeHiddenStatuses(hidden));
  return current.has(status) || current.size + 1 < STATUSES.length;
}

/**
 * Toggling one column.
 *
 * Refuses the hide that would empty the board, and refuses it by *keeping the
 * reader's set unchanged*. Deferring to `sanitizeHiddenStatuses` here would
 * apply that function's storage repair — an all-hidden set falls back to
 * hiding none — to a deliberate click, so unchecking the last column would
 * turn all six back on and read as the menu undoing five earlier choices. The
 * repair is right for a value read from disk, which is unrecoverable from the
 * board itself, and wrong for a transition the reader can simply not make.
 */
export function toggleHiddenStatus(hidden: readonly TaskStatus[], status: TaskStatus): TaskStatus[] {
  const current = sanitizeHiddenStatuses(hidden);
  if (!canHideStatus(current, status)) return current;
  const next = new Set(current);
  if (next.has(status)) next.delete(status);
  else next.add(status);
  return sanitizeHiddenStatuses([...next]);
}

/**
 * Columns the board draws, in status order.
 *
 * Three inputs decide this, and they are not interchangeable:
 *
 * * `hidden` is the reader's standing choice and always wins — a hidden
 *   column is never drawn, not even mid-drag, because a drop target the
 *   reader asked not to see is a place tasks would vanish into.
 * * `counts` collapses empty columns while idle, so an untouched board is not
 *   six empty rectangles.
 * * `dragging` re-expands the empty ones, so a card can be dropped into a
 *   column that has nothing in it yet.
 *
 * When every visible column would be empty and nothing is being dragged, all
 * unhidden columns are shown: an empty board still has to look like a board.
 */
export function visibleBoardStatuses(
  hidden: readonly TaskStatus[],
  counts: Partial<Record<TaskStatus, number>>,
  dragging: boolean,
): TaskStatus[] {
  const hiddenSet = new Set(sanitizeHiddenStatuses(hidden));
  const shown = STATUSES.filter((status) => !hiddenSet.has(status));
  if (dragging) return shown;
  const occupied = shown.filter((status) => (counts[status] ?? 0) > 0);
  return occupied.length ? occupied : shown;
}

export interface HiddenColumnReport {
  /** Hidden columns that currently hold at least one task, in status order. */
  statuses: TaskStatus[];
  /** How many tasks those columns hold in total. */
  tasks: number;
  /** "12 tasks in Review and Done" — ready to put beside a Show-all action. */
  summary: string;
}

/**
 * What the reader cannot see because they hid a column.
 *
 * Returns null only when nothing is hidden *or* every hidden column is empty;
 * a hidden column holding work always produces a report. This is the same
 * rule the Fleet grid follows for a failed cell in a hidden column: hiding
 * changes the layout, never what the page is willing to admit.
 *
 * `counts` are the board's own per-column totals, so the number is the
 * server's total for that column and not just its loaded page.
 */
export function hiddenColumnReport(
  hidden: readonly TaskStatus[],
  counts: Partial<Record<TaskStatus, number>>,
): HiddenColumnReport | null {
  const hiddenSet = new Set(sanitizeHiddenStatuses(hidden));
  const statuses = STATUSES.filter((status) => hiddenSet.has(status) && (counts[status] ?? 0) > 0);
  if (!statuses.length) return null;
  const tasks = statuses.reduce((sum, status) => sum + Math.max(0, Math.trunc(counts[status] ?? 0)), 0);
  if (tasks <= 0) return null;
  return { statuses, tasks, summary: `${tasks} ${tasks === 1 ? "task" : "tasks"} in ${joinNames(statuses)}` };
}

function joinNames(statuses: readonly TaskStatus[]): string {
  const names = statuses.map((status) => STATUS_LABELS[status]);
  if (names.length === 1) return names[0];
  if (names.length === 2) return `${names[0]} and ${names[1]}`;
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}
