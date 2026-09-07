/**
 * Honesty helpers for the Code → Map navigator.
 *
 * Capped samples must never read as complete inventories: every list render
 * pairs shown length with `liveness_meta` / `role_file_counts` totals.
 */

import type { RepoMapDocument, RepoMapSubsystem } from "./types";

export function formatCap(shown: number, total: number, truncated: boolean): string {
  if (truncated || shown < total) {
    return `${shown} of ${total}`;
  }
  return String(shown);
}

export function roleSample(
  subsystem: RepoMapSubsystem,
  role: string,
): { paths: string[]; total: number; truncated: boolean } {
  const paths = subsystem.role_files[role] ?? [];
  const total = subsystem.role_file_counts[role] ?? paths.length;
  return { paths, total, truncated: total > paths.length };
}

/**
 * Prefer unwired / dead-symbol candidates. Ignore `unreachable_files` when
 * `liveness_unreachable_unreliable` is set.
 */
export function preferredDeadLists(map: RepoMapDocument): {
  unwired: string[];
  deadSymbols: string[];
  unreachable: string[];
  unreachableSuppressed: boolean;
} {
  const unreachableSuppressed = map.liveness_unreachable_unreliable === true;
  return {
    unwired: map.unwired_candidates ?? [],
    deadSymbols: map.dead_symbol_candidates ?? [],
    unreachable: unreachableSuppressed ? [] : (map.unreachable_files ?? []),
    unreachableSuppressed,
  };
}
