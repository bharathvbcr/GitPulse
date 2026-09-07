/**
 * Normalize a `devmap preview` JSON report into the typed PreviewReport shape.
 *
 * The CLI driver returns `report` as raw JSON (`serde_json::Value`). The
 * broken_callers envelope uses the engine Response wire (`resolution`,
 * `hidden`, …) rather than GitPulse's `CodeintelResponse`, so callers must
 * not treat the raw object as already typed.
 */

import type {
  CodeintelResponse,
  DevmapPreviewCaller,
  DevmapPreviewFileResult,
  DevmapPreviewReport,
} from "./types";

function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function asString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function asBool(value: unknown, fallback = false): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function asNumber(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function normalizeBrokenCallers(
  raw: unknown,
): CodeintelResponse<DevmapPreviewCaller> {
  const rec = asRecord(raw);
  if (!rec) {
    return {
      available: false,
      reason: "broken_callers missing from preview report",
      items: [],
      total: 0,
      shown: 0,
      truncated: false,
    };
  }

  const resolution = rec.resolution;
  let available = asBool(rec.available, true);
  let reason: string | null =
    typeof rec.reason === "string" ? rec.reason : null;
  if (resolution && typeof resolution === "object" && !Array.isArray(resolution)) {
    const res = resolution as Record<string, unknown>;
    if ("Unavailable" in res) {
      available = false;
      const detail = res.Unavailable;
      if (typeof detail === "string") reason = detail;
      else if (detail && typeof detail === "object" && "reason" in detail) {
        reason = asString((detail as { reason?: unknown }).reason, reason ?? "");
      }
    }
  }

  const items = Array.isArray(rec.items)
    ? (rec.items as DevmapPreviewCaller[])
    : [];
  return {
    available,
    reason,
    items,
    total: asNumber(rec.total, items.length),
    shown: asNumber(rec.shown, items.length),
    truncated: asBool(rec.truncated, false),
    ...(typeof rec.walk_incomplete === "string"
      ? { walk_incomplete: rec.walk_incomplete }
      : {}),
  };
}

/** Parse one file's preview report from the CLI/IPC Value. */
export function normalizePreviewReport(
  raw: unknown,
  fallbackPath = "",
): DevmapPreviewReport | null {
  const rec = asRecord(raw);
  if (!rec) return null;
  return {
    file_path: asString(rec.file_path, fallbackPath),
    parse_status: asString(rec.parse_status, "Unknown"),
    delta_available: asBool(rec.delta_available, false),
    file_is_indexed: asBool(rec.file_is_indexed, false),
    compared_against: asString(rec.compared_against, "unknown"),
    degraded_reason:
      typeof rec.degraded_reason === "string" ? rec.degraded_reason : null,
    symbols: Array.isArray(rec.symbols) ? rec.symbols : [],
    bodies_not_compared: asNumber(rec.bodies_not_compared, 0),
    ambiguous_callers: asNumber(rec.ambiguous_callers, 0),
    broken_callers: normalizeBrokenCallers(rec.broken_callers),
  };
}

/** Normalize a whole file result so UI code always sees typed reports. */
export function normalizePreviewFileResult(
  raw: DevmapPreviewFileResult,
): DevmapPreviewFileResult {
  if (!raw.report) return raw;
  const report = normalizePreviewReport(raw.report, raw.file_path);
  return { ...raw, report };
}
