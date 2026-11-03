import { WorkbenchError, request, type ChildProgress, type TaskCard } from "./client";

/**
 * Where a board card sits in a task tree: its parent, and how many of its
 * subtasks are done. The card list the store returns omits `links` (a card is
 * the small projection), so the host answers this with one read over the
 * parent links (`items.lineage`), the way it answers verdicts.
 */
export interface CardLineage {
  /** The live task this one is a subtask of; null when it has none. */
  parent_item_id: string | null;
  children: ChildProgress;
}

/** The host's cap on one batched read (`workbench/lineage.rs` `MAX_BATCH`); a board page is 30 cards in each of 7 columns. */
export const MAX_LINEAGE_BATCH = 256;
/** Where `getBoardLineage` stops splitting: 16 reads, 4,096 cards. */
export const MAX_LINEAGE_READS = 16;

const ID = /^[A-Za-z0-9_-]{1,128}$/;

function count(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new WorkbenchError("protocol_error", "Task storage returned an invalid task lineage.");
  }
  return value;
}

function decode(value: unknown): CardLineage {
  const raw = typeof value === "object" && value !== null ? (value as Record<string, unknown>) : null;
  const kids = typeof raw?.children === "object" && raw.children !== null ? (raw.children as Record<string, unknown>) : null;
  const parent = raw?.parent_item_id;
  if (!raw || !kids || (parent !== null && !(typeof parent === "string" && ID.test(parent)))) {
    throw new WorkbenchError("protocol_error", "Task storage returned an invalid task lineage.");
  }
  const total = count(kids.total), done = count(kids.done);
  if (done > total) throw new WorkbenchError("protocol_error", "Task storage returned an invalid task lineage.");
  return { parent_item_id: parent, children: { total, done } };
}

/**
 * The lineage of every named card in one read. A card the store does not hold
 * is absent from the answer, which the board shows as unknown. A read that
 * fails throws, so the caller can say so instead of drawing every card as
 * having no subtasks.
 */
export async function getLineage(ids: readonly string[]): Promise<Map<string, CardLineage>> {
  const unique = [...new Set(ids)];
  const read = new Map<string, CardLineage>();
  if (!unique.length) return read;
  if (unique.length > MAX_LINEAGE_BATCH) throw new WorkbenchError("invalid_input", "Too many tasks for one lineage read.");
  const raw = await request("items.lineage", { ids: unique });
  const items = typeof raw === "object" && raw !== null && (raw as Record<string, unknown>).ok === true ? (raw as Record<string, unknown>).items : undefined;
  if (typeof items !== "object" || items === null) throw new WorkbenchError("protocol_error", "Task storage returned an invalid task lineage.");
  for (const [id, value] of Object.entries(items)) {
    if (unique.includes(id)) read.set(id, decode(value));
  }
  return read;
}

/**
 * `getLineage` for a board that holds more cards than one read takes, in
 * `MAX_LINEAGE_BATCH` reads one after another. Past `MAX_LINEAGE_READS` it
 * refuses, as one oversized read does.
 */
export async function getBoardLineage(ids: readonly string[]): Promise<Map<string, CardLineage>> {
  const unique = [...new Set(ids)];
  if (unique.length > MAX_LINEAGE_BATCH * MAX_LINEAGE_READS) {
    throw new WorkbenchError("invalid_input", "Too many tasks on the board to read their subtasks.");
  }
  const read = new Map<string, CardLineage>();
  for (let start = 0; start < unique.length; start += MAX_LINEAGE_BATCH) {
    for (const [id, lineage] of await getLineage(unique.slice(start, start + MAX_LINEAGE_BATCH))) read.set(id, lineage);
  }
  return read;
}

/** Whether two reads say the same about every card; a board keeps the one it shows when they do. */
export function sameLineage(a: ReadonlyMap<string, CardLineage>, b: ReadonlyMap<string, CardLineage>): boolean {
  if (a.size !== b.size) return false;
  for (const [id, next] of b) {
    const shown = a.get(id);
    if (!shown || shown.parent_item_id !== next.parent_item_id
      || shown.children.total !== next.children.total || shown.children.done !== next.children.done) return false;
  }
  return true;
}

/**
 * The card with its lineage filled in. A card the read did not name keeps no
 * lineage fields at all, so it is unknown rather than a task with no subtasks.
 */
export function withLineage(card: TaskCard, read: ReadonlyMap<string, CardLineage>): TaskCard {
  const lineage = read.get(card.id);
  return lineage ? { ...card, parent_item_id: lineage.parent_item_id, children: lineage.children } : card;
}

/** `2 of 3 subtasks done`; null for a card with no subtasks, or one whose subtasks were not read. */
export function childProgressLabel(children: ChildProgress | undefined): string | null {
  if (!children || children.total === 0) return null;
  return `${children.done} of ${children.total} ${children.total === 1 ? "subtask" : "subtasks"} done`;
}
