/**
 * Honesty helpers for the Code → Map navigator.
 *
 * Capped samples must never read as complete inventories: every list render
 * pairs shown length with `liveness_meta` / `role_file_counts` totals.
 */

import type {
  DevmapStatusPayload,
  RepoMapDocument,
  RepoMapSubsystem,
  RepoMapUnwiredMeta,
} from "./types";
import { dedupePreserveOrder } from "../ui/eachKeys";

export function formatCap(shown: number, total: number, truncated: boolean): string {
  if (truncated || shown < total) {
    return `${shown} of ${total}`;
  }
  return String(shown);
}

export function coverageGapSummary(payload: DevmapStatusPayload | null): string | null {
  const gaps = payload?.coverage_gaps;
  if (!gaps || typeof gaps !== "object") return null;
  const parts: string[] = [];
  for (const [key, value] of Object.entries(gaps)) {
    const n = coverageGapCount(value);
    if (n === "unavailable") parts.push(`${key}: unavailable`);
    else if (n > 0) parts.push(`${key}: ${n}`);
  }
  return parts.length > 0 ? parts.join(" · ") : null;
}

function coverageGapCount(value: unknown): number | "unavailable" {
  if (Array.isArray(value)) return value.length;
  if (isCount(value)) return value;
  if (!value || typeof value !== "object") return "unavailable";

  // Current devmap envelopes carry a capped sample beside the complete total.
  // Presence is decisive: a malformed `total` is producer drift, not permission
  // to reinterpret the envelope's four structural keys as four gaps.
  if ("total" in value) {
    const total = value.total;
    return isCount(total) ? total : "unavailable";
  }

  // Legacy producers exposed array-like objects with a numeric length.
  if ("length" in value) {
    const length = value.length;
    return isCount(length) ? length : "unavailable";
  }
  return "unavailable";
}

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
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
  // Producer samples can repeat an id (measured on DevCouncil/Manvi maps).
  // RepoMapPanel keys `{#each}` on these strings; a duplicate throws
  // `each_key_duplicate` and takes the Code → Map pane down.
  return {
    unwired: dedupePreserveOrder(map.unwired_candidates ?? []),
    deadSymbols: dedupePreserveOrder(map.dead_symbol_candidates ?? []),
    unreachable: unreachableSuppressed
      ? []
      : dedupePreserveOrder(map.unreachable_files ?? []),
    unreachableSuppressed,
  };
}

/**
 * The "Excluded:" line under the unwired list, as `label count` phrases.
 *
 * Every counter is optional and every one means something different when it is
 * missing rather than zero: a map written before the kernel gained the
 * file-liveness rule carries no `excluded_not_code` key at all, and rendering
 * that as `not code 0` would claim the producer looked and found none. An
 * absent counter contributes no phrase; a present zero contributes none either,
 * because "excluded nothing" is what an empty line already says.
 *
 * `excluded_directory_unit` is deliberately rendered *inside* the exempt
 * phrase rather than beside it. It is a subset of `excluded_exempt`, and two
 * numbers side by side in one list read as two populations — a reader adding
 * them up would double-count every Terraform file in the repository.
 */
export function unwiredExclusionSummary(meta: RepoMapUnwiredMeta): string[] {
  const phrases: string[] = [];
  const push = (label: string, count: number | undefined, suffix = "") => {
    if (!isCount(count) || count === 0) return;
    phrases.push(`${label} ${count}${suffix}`);
  };
  push("coverage loss", meta.excluded_coverage_loss);
  push("import-blind", meta.excluded_import_blind);
  push("not code", meta.excluded_not_code);
  const directoryUnit = meta.excluded_directory_unit;
  push(
    "exempt",
    meta.excluded_exempt,
    isCount(directoryUnit) && directoryUnit > 0
      ? ` (${directoryUnit} whose unit is a directory)`
      : "",
  );
  return phrases;
}
