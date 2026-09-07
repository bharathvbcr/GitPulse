/**
 * Honesty-first summaries of `devmap preview` for the commit composer and
 * diff rail. A Fallback / unreadable parse must never render as "nothing
 * breaks" — that is the precise failure the report's fields exist to prevent.
 */

import type { DevmapPreviewFileResult, DevmapPreviewReport } from "./types";

/** Per-file marker for the diff rail, sourced from the shared preview call. */
export type PreviewMarkerKind =
  | "breaks"
  | "unreliable"
  | "clean"
  | "unavailable";

export interface PreviewMarker {
  kind: PreviewMarkerKind;
  /** Short label for the rail (e.g. "3", "!", "?"). */
  label: string;
  /** Full honesty line for a tooltip. */
  title: string;
  brokenTotal: number;
}

export interface PreviewFileHonesty {
  file_path: string;
  available: boolean;
  reason: string | null;
  parse_status: string;
  degraded_reason: string | null;
  compared_against: string;
  file_is_indexed: boolean;
  delta_available: boolean;
  bodies_not_compared: number;
  ambiguous_callers: number;
  broken_total: number;
  broken_shown: number;
  broken_truncated: boolean;
  walk_incomplete: string | null;
  /** True when the parse cannot support a "nothing breaks" claim. */
  unreliable: boolean;
  /** Safe to claim zero breakage for this file. */
  claim_clean: boolean;
}

export interface PreviewCommitSummary {
  fileCount: number;
  availableFiles: number;
  unreliableFiles: number;
  unavailableFiles: number;
  brokenCallerTotal: number;
  filesWithBreaks: number;
  claimCleanCount: number;
  cancelled: boolean;
  outcomeReason: string | null;
  files: PreviewFileHonesty[];
  /** Headline the composer shows; never "nothing breaks" when unreliable. */
  headline: string;
}

const UNRELIABLE_PARSE = /fallback|unreadable|error|unknown/i;

export function isUnreliableParseStatus(parseStatus: string): boolean {
  return UNRELIABLE_PARSE.test(parseStatus.trim());
}

export function fileHonesty(result: DevmapPreviewFileResult): PreviewFileHonesty {
  const report = result.report;
  if (!result.available || !report) {
    return {
      file_path: result.file_path,
      available: false,
      reason: result.reason ?? "preview unavailable",
      parse_status: report?.parse_status ?? "Unavailable",
      degraded_reason: report?.degraded_reason ?? null,
      compared_against: report?.compared_against ?? "—",
      file_is_indexed: report?.file_is_indexed ?? false,
      delta_available: report?.delta_available ?? false,
      bodies_not_compared: report?.bodies_not_compared ?? 0,
      ambiguous_callers: report?.ambiguous_callers ?? 0,
      broken_total: report?.broken_callers.total ?? 0,
      broken_shown: report?.broken_callers.shown ?? 0,
      broken_truncated: report?.broken_callers.truncated ?? false,
      walk_incomplete: report?.broken_callers.walk_incomplete ?? null,
      unreliable: true,
      claim_clean: false,
    };
  }
  return honestyFromReport(report, result.reason ?? null);
}

export function honestyFromReport(
  report: DevmapPreviewReport,
  reason: string | null = null,
): PreviewFileHonesty {
  const unreliable =
    isUnreliableParseStatus(report.parse_status) ||
    Boolean(report.degraded_reason) ||
    !report.file_is_indexed ||
    !report.delta_available ||
    !report.broken_callers.available;

  const brokenTotal = report.broken_callers.total;
  const claim_clean =
    !unreliable &&
    brokenTotal === 0 &&
    report.bodies_not_compared === 0 &&
    !report.broken_callers.truncated &&
    !report.broken_callers.walk_incomplete;

  return {
    file_path: report.file_path,
    available: true,
    reason,
    parse_status: report.parse_status,
    degraded_reason: report.degraded_reason ?? null,
    compared_against: report.compared_against,
    file_is_indexed: report.file_is_indexed,
    delta_available: report.delta_available,
    bodies_not_compared: report.bodies_not_compared,
    ambiguous_callers: report.ambiguous_callers,
    broken_total: brokenTotal,
    broken_shown: report.broken_callers.shown,
    broken_truncated: report.broken_callers.truncated,
    walk_incomplete: report.broken_callers.walk_incomplete ?? null,
    unreliable,
    claim_clean,
  };
}

export function markerForHonesty(h: PreviewFileHonesty): PreviewMarker {
  if (!h.available) {
    return {
      kind: "unavailable",
      label: "?",
      title: `Preview unavailable: ${h.reason ?? "unknown"}`,
      brokenTotal: 0,
    };
  }
  if (h.unreliable) {
    const bits = [
      `parse=${h.parse_status}`,
      `indexed=${h.file_is_indexed}`,
      `delta=${h.delta_available}`,
      `against=${h.compared_against}`,
    ];
    if (h.degraded_reason) bits.push(`degraded: ${h.degraded_reason}`);
    if (h.walk_incomplete) bits.push(`walk incomplete: ${h.walk_incomplete}`);
    return {
      kind: "unreliable",
      label: "!",
      title: `Preview unreliable — not "nothing breaks". ${bits.join("; ")}`,
      brokenTotal: h.broken_total,
    };
  }
  if (h.broken_total > 0) {
    return {
      kind: "breaks",
      label: String(h.broken_total),
      title: `${h.broken_total} broken caller(s) (showing ${h.broken_shown}${
        h.broken_truncated ? ", truncated" : ""
      })`,
      brokenTotal: h.broken_total,
    };
  }
  return {
    kind: "clean",
    label: "0",
    title: "No broken callers detected for a reliable parse",
    brokenTotal: 0,
  };
}

export function summarizePreview(
  files: DevmapPreviewFileResult[],
  opts: { cancelled?: boolean; reason?: string | null } = {},
): PreviewCommitSummary {
  const honesty = files.map(fileHonesty);
  const unreliableFiles = honesty.filter((h) => h.unreliable || !h.available).length;
  const unavailableFiles = honesty.filter((h) => !h.available).length;
  const filesWithBreaks = honesty.filter((h) => h.broken_total > 0).length;
  const brokenCallerTotal = honesty.reduce((n, h) => n + h.broken_total, 0);
  const claimCleanCount = honesty.filter((h) => h.claim_clean).length;
  const availableFiles = honesty.filter((h) => h.available).length;

  let headline: string;
  if (opts.reason && files.length === 0) {
    headline = `Preview failed — not a clean bill of health (${opts.reason})`;
  } else if (files.length === 0) {
    headline = "No staged files to preview";
  } else if (opts.cancelled) {
    headline = "Preview cancelled before every staged file finished";
  } else if (unavailableFiles === files.length) {
    headline = "Could not preview staged changes — not a clean bill of health";
  } else if (unreliableFiles > 0 && brokenCallerTotal === 0) {
    headline = `${unreliableFiles} file(s) have an unreliable preview — do not treat as "nothing breaks"`;
  } else if (brokenCallerTotal > 0) {
    headline = `${brokenCallerTotal} broken caller(s) across ${filesWithBreaks} file(s)`;
  } else if (claimCleanCount === files.length) {
    headline = "No broken callers detected for staged changes";
  } else {
    headline = "Preview incomplete — cannot claim staged changes are safe";
  }

  return {
    fileCount: files.length,
    availableFiles,
    unreliableFiles,
    unavailableFiles,
    brokenCallerTotal,
    filesWithBreaks,
    claimCleanCount,
    cancelled: Boolean(opts.cancelled),
    outcomeReason: opts.reason ?? null,
    files: honesty,
    headline,
  };
}

/** Build a path → marker map from one preview batch (shared A1 path). */
export function markersByPath(
  files: DevmapPreviewFileResult[],
): Map<string, PreviewMarker> {
  const map = new Map<string, PreviewMarker>();
  for (const file of files) {
    map.set(file.file_path, markerForHonesty(fileHonesty(file)));
  }
  return map;
}
