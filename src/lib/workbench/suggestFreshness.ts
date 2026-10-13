import { debounce } from "../async/debounce";
import type { Page, TaskCard, TaskStatus } from "./client";

/**
 * What a "Suggested next" answer depends on among the cards the board holds:
 * which tasks exist and each one's status and priority. A drag that only moves
 * a card inside its column, or an edit to a title, does not change it.
 */
export function suggestSignature(columns: Partial<Record<TaskStatus, Page<TaskCard>>>): string {
  const parts: string[] = [];
  for (const page of Object.values(columns)) for (const card of page?.items ?? []) parts.push(`${card.id}:${card.status}:${card.priority}`);
  return parts.sort().join("|");
}

export interface SuggestWatch {
  /**
   * Reports the signature of the board that is on screen. `key` is null while
   * that board is still loading. The first signature of a board is a baseline
   * (loading or switching boards is asked about by its own path); a later,
   * different one schedules one re-ask after the quiet window.
   */
  observe(key: string | null, signature: string): void;
  /** Drops the pending re-ask and forgets the baseline. */
  reset(): void;
}

/** Re-asks once per burst of task changes, never per change: `ask` runs on the trailing edge. */
export function createSuggestWatch(ask: () => void, waitMs: number): SuggestWatch {
  const later = debounce(ask, waitMs);
  let seen: { key: string; signature: string } | null = null;
  return {
    observe(key, signature) {
      if (key === null) return;
      if (seen === null || seen.key !== key) {
        later.cancel();
        seen = { key, signature };
        return;
      }
      if (seen.signature === signature) return;
      seen.signature = signature;
      later();
    },
    reset() {
      later.cancel();
      seen = null;
    },
  };
}
