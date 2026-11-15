/**
 * Dismiss and restore for the Tasks inbox. The store owns what a dismissal means
 * (`attention.update`: Active and Unread leave dismissed notices out, All keeps
 * them), so this module only sends the write and folds its receipt into the page
 * the reader is looking at, without re-reading and losing their place.
 */
import { attentionWrite, updateAttention, WorkbenchError, type AttentionFilter, type AttentionRow, type AttentionWrite, type Page } from "./client";

export type DismissAction = "dismiss" | "restore";

/** Codes after which the write may have landed: keep its request id so a retry is the same write. */
const UNCERTAIN = new Set(["transport_error", "worker_error", "store_error", "protocol_error", "timeout"]);
const uncertain = (cause: unknown) => !(cause instanceof WorkbenchError) || UNCERTAIN.has(cause.code);

export function createAttentionDismiss(write: (input: AttentionWrite) => Promise<AttentionRow> = updateAttention) {
  const pending = new Map<string, AttentionWrite>();
  return {
    async apply(item: Pick<AttentionRow, "id" | "revision">, action: DismissAction): Promise<AttentionRow> {
      const key = `${item.id}:${action}`;
      // A kept write is replayed only for the revision it was made against: once the row has moved on (the uncertain
      // write landed and was read back, or someone else changed it), replaying would return a receipt the store no
      // longer holds, so the reader's current row gets a write of its own.
      const kept = pending.get(key);
      const input = kept && kept.expected_revision === item.revision ? kept : attentionWrite(item, action);
      pending.set(key, input);
      try {
        const result = await write(input);
        pending.delete(key);
        return result;
      } catch (cause) {
        if (!uncertain(cause)) pending.delete(key);
        throw cause;
      }
    },
  };
}

/** Whether a row belongs in a page read with `filter`, by the same rules `attention.list` applies (store times are ms). */
function listed(row: AttentionRow, filter: AttentionFilter, now: number): boolean {
  if (filter === "all") return true;
  if (row.dismissed_at !== null) return false;
  if (row.snoozed_until !== null && row.snoozed_until > now) return false;
  return filter === "active" || row.read_at === null;
}

/**
 * The page with `updated` in place of its row, or without it when the write took
 * it out of this filter. Counts move with the rows so the page stays truthful.
 */
export function withAttentionUpdate(page: Page<AttentionRow>, updated: AttentionRow, filter: AttentionFilter, now = Date.now()): Page<AttentionRow> {
  const index = page.items.findIndex((row) => row.id === updated.id);
  if (index < 0) return page;
  if (listed(updated, filter, now)) return { ...page, items: page.items.map((row, at) => (at === index ? updated : row)) };
  const items = page.items.filter((_, at) => at !== index);
  return { ...page, items, shown: items.length, total: Math.max(items.length, page.total - 1) };
}
