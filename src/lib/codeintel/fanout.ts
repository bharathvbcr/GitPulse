/**
 * Bound fan-out for layered impact and `devmap preview` batches.
 *
 * Neighbors already chunk at `MAX_NEIGHBOR_TARGETS` (16). Layered-many and
 * preview-many used to walk every path, which is what turned a 40-file
 * change-set into a 30s cancel-deadline stall. Cap, then report the omitted
 * remainder — never present a truncated walk as complete.
 *
 * Keep the number in sync with `MAX_NEIGHBOR_TARGETS` /
 * `MAX_PREVIEW_FILES` in src-tauri.
 */

import type { CodeintelLayeredImpact, DevmapPreviewFileResult } from "./types";

export const CODEINTEL_FANOUT_CAP = 16;

export const FANOUT_OMITTED_REASON =
  "layered-impact fan-out capped; this seed was not walked";

export const PREVIEW_FANOUT_OMITTED_REASON =
  "preview fan-out capped; this file was not previewed";

export interface FanoutCap<T> {
  kept: T[];
  omitted: T[];
  truncated: boolean;
  total: number;
}

export function capFanout<T>(
  items: readonly T[],
  cap = CODEINTEL_FANOUT_CAP,
): FanoutCap<T> {
  const total = items.length;
  if (total <= cap) {
    return { kept: [...items], omitted: [], truncated: false, total };
  }
  return {
    kept: items.slice(0, cap),
    omitted: items.slice(cap),
    truncated: true,
    total,
  };
}

export function omittedLayeredImpact(seed: string): CodeintelLayeredImpact {
  return {
    available: false,
    reason: FANOUT_OMITTED_REASON,
    edges: {
      available: false,
      reason: FANOUT_OMITTED_REASON,
      items: [],
      total: 0,
      shown: 0,
      truncated: false,
    },
    blast_radius: {
      seeds: [seed],
      unmatched_targets: [],
      layers: {
        available: false,
        reason: FANOUT_OMITTED_REASON,
        items: [],
        total: 0,
        shown: 0,
        truncated: false,
      },
      total_impacted: 0,
    },
  };
}

export function omittedPreviewFile(filePath: string): DevmapPreviewFileResult {
  return {
    file_path: filePath,
    available: false,
    reason: PREVIEW_FANOUT_OMITTED_REASON,
    report: null,
  };
}
