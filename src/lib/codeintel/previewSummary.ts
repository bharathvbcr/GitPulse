/**
 * Honesty-first summaries of `devmap preview` for the commit composer and
 * diff rail. A Fallback / unreadable parse must never render as "nothing
 * breaks" — that is the precise failure the report's fields exist to prevent.
 */

import type { DevmapPreviewFileResult, DevmapPreviewReport } from "./types";
import {
  boundText,
  firstClause,
  summarizeWalkIncomplete,
  tooltipWalkIncomplete,
} from "./walkIncomplete";

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

/**
 * Longest phrase a sidebar row will render.
 *
 * Every string interpolated into a glance comes from the engine and none of
 * them carry a documented bound, so the bound is applied here rather than
 * hoped for upstream.
 */
const GLANCE_MAX_CHARS = 72;

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
      walk_incomplete: summarizeWalkIncomplete([
        report?.broken_callers.walk_incomplete,
      ]),
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
    walk_incomplete: summarizeWalkIncomplete([report.broken_callers.walk_incomplete]),
    unreliable,
    claim_clean,
  };
}

export function markerForHonesty(h: PreviewFileHonesty): PreviewMarker {
  if (!h.available) {
    return {
      kind: "unavailable",
      label: "?",
      title: boundText(`Preview unavailable: ${h.reason ?? "unknown"}`),
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
    if (h.walk_incomplete) {
      bits.push(
        `walk incomplete: ${tooltipWalkIncomplete([h.walk_incomplete]) ?? h.walk_incomplete}`,
      );
    }
    return {
      kind: "unreliable",
      label: "!",
      title: boundText(`Preview unreliable — not "nothing breaks". ${bits.join("; ")}`),
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
  // Zero broken callers found is not the same as zero broken callers.
  //
  // `unreliable` above asks whether the PARSE could be trusted; it says
  // nothing about whether the caller search finished. So a file with a clean
  // parse whose walk stopped early — or whose bodies were never compared —
  // fell through to "clean" and rendered in the rail as a bare `0` titled "No
  // broken callers detected". `claim_clean` is the field that already knows
  // better, and it is the one that decides here.
  if (!h.claim_clean) {
    const why = h.walk_incomplete
      ? `walk incomplete: ${tooltipWalkIncomplete([h.walk_incomplete]) ?? h.walk_incomplete}`
      : h.broken_truncated
        ? "the broken-caller list was truncated"
        : `${h.bodies_not_compared} body/bodies were not compared`;
    return {
      kind: "unreliable",
      label: "!",
      title: boundText(`Preview incomplete — not "nothing breaks". ${why}`),
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

/**
 * One short phrase saying what this file's preview actually found.
 *
 * The commit composer used to render the report struct field by field —
 * `parse=Unavailable · against=— · indexed=no · delta=no ·
 * bodies_not_compared=0 · ambiguous_callers=0 · broken=0/0` — six lines of 9px
 * monospace per file, most of it zeroes. Spending the reader's attention to
 * say "nothing happened" is what buried the one line that did say something.
 *
 * So: name the first real finding and stop. The order is by severity, not by
 * struct order, and every branch states a fact the fields actually carry —
 * "unreliable" never degrades into a quiet "clean".
 */
export function fileGlance(h: PreviewFileHonesty): string {
  if (!h.available) {
    // `firstClause` answers null for a clause too long to sit on a line, and
    // falling back to the raw reason would put the whole engine string in a
    // sidebar row — which is the defect this function replaced, re-entering
    // through the error path.
    const reason = firstClause(h.reason) ?? boundText(h.reason ?? "", GLANCE_MAX_CHARS);
    return reason ? `not previewed — ${reason}` : "not previewed";
  }
  if (h.degraded_reason) {
    return (
      firstClause(h.degraded_reason) ??
      boundText(h.degraded_reason, GLANCE_MAX_CHARS) ??
      "preview degraded"
    );
  }
  if (!h.file_is_indexed) return "not in the index — callers unknown";
  if (isUnreliableParseStatus(h.parse_status)) {
    // parse_status is engine-supplied and has no documented length bound.
    return `could not be parsed (${boundText(h.parse_status.trim().toLowerCase(), 24)})`;
  }
  if (!h.delta_available) return "no before/after to compare";
  if (h.broken_total > 0) {
    const n = h.broken_total.toLocaleString();
    const caller = h.broken_total === 1 ? "caller" : "callers";
    return h.broken_truncated
      ? `at least ${n} ${caller} would break`
      : `${n} ${caller} would break`;
  }
  if (h.walk_incomplete) return "search did not finish — cannot say it is clean";
  if (h.bodies_not_compared > 0) {
    const n = h.bodies_not_compared.toLocaleString();
    return `${n} ${h.bodies_not_compared === 1 ? "body" : "bodies"} not compared`;
  }
  if (h.ambiguous_callers > 0) {
    const n = h.ambiguous_callers.toLocaleString();
    return `${n} ambiguous ${h.ambiguous_callers === 1 ? "caller" : "callers"}`;
  }
  if (h.claim_clean) return "no callers break";
  // Reliable-looking but not clean-claimable: say so rather than round down.
  return "incomplete — cannot claim it is safe";
}

export function summarizePreview(
  files: DevmapPreviewFileResult[],
  opts: {
    cancelled?: boolean;
    reason?: string | null;
    filesOmitted?: number;
    filesTotal?: number;
  } = {},
): PreviewCommitSummary {
  const honesty = files.map(fileHonesty);
  const unreliableFiles = honesty.filter((h) => h.unreliable || !h.available).length;
  const unavailableFiles = honesty.filter((h) => !h.available).length;
  const filesWithBreaks = honesty.filter((h) => h.broken_total > 0).length;
  const brokenCallerTotal = honesty.reduce((n, h) => n + h.broken_total, 0);
  const claimCleanCount = honesty.filter((h) => h.claim_clean).length;
  const availableFiles = honesty.filter((h) => h.available).length;
  const filesOmitted = opts.filesOmitted ?? 0;
  const filesTotal = opts.filesTotal ?? files.length;

  let headline: string;
  if (opts.reason && files.length === 0) {
    headline = `Preview failed — not a clean bill of health (${opts.reason})`;
  } else if (files.length === 0) {
    headline = "No staged files to preview";
  } else if (opts.cancelled) {
    headline = "Preview cancelled before every staged file finished";
  } else if (filesOmitted > 0) {
    headline = `Preview fan-out capped: ${filesOmitted} of ${filesTotal} file(s) not previewed — not a clean bill of health`;
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
