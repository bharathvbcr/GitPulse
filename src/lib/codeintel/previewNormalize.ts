/**
 * Normalize a `devmap preview` JSON report into the typed PreviewReport shape.
 *
 * The CLI driver returns `report` as raw JSON (`serde_json::Value`). The
 * broken_callers envelope uses the engine Response wire (`resolution`,
 * `hidden`, …) rather than GitPulse's `CodeintelResponse`, so callers must
 * not treat the raw object as already typed.
 */

import {
  parseSourceFreshness,
  unverifiedSourceFreshness,
  type CodeintelResponse,
  type DevmapPreviewCaller,
  type DevmapPreviewFileResult,
  type DevmapPreviewReport,
} from "./types";
import {
  boundText,
  summarizeWalkIncomplete,
  WALK_INCOMPLETE_MAX_CHARS,
} from "./walkIncomplete";

/**
 * Engine prose reaching a panel is bounded here, beside the walk fold.
 *
 * `reason` and `degraded_reason` come from the same driver as
 * `walk_incomplete` and carry no documented length limit, so a panel that
 * renders one verbatim has exactly the unbounded-essay problem the fold next
 * to it exists to prevent. Clipping rather than folding keeps a short exact
 * phrase exact; the ellipsis is what says a tail was cut.
 */
function boundEngineText(value: unknown): string | null {
  return typeof value === "string" ? boundText(value, WALK_INCOMPLETE_MAX_CHARS) : null;
}

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
      source_freshness: unverifiedSourceFreshness("broken_callers missing from preview report"),
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
  let reason: string | null = boundEngineText(rec.reason);
  if (resolution && typeof resolution === "object" && !Array.isArray(resolution)) {
    const res = resolution as Record<string, unknown>;
    if ("Unavailable" in res) {
      available = false;
      const detail = res.Unavailable;
      if (typeof detail === "string") reason = boundEngineText(detail);
      else if (detail && typeof detail === "object" && "reason" in detail) {
        reason = boundEngineText(
          asString((detail as { reason?: unknown }).reason, reason ?? ""),
        );
      }
    }
  }

  const items = Array.isArray(rec.items)
    ? (rec.items as DevmapPreviewCaller[])
    : [];
  const foldedWalk =
    typeof rec.walk_incomplete === "string"
      ? summarizeWalkIncomplete([rec.walk_incomplete])
      : null;
  const source_freshness = rec.source_freshness
    ? parseSourceFreshness(rec.source_freshness, "preview")
    : unverifiedSourceFreshness();
  return {
    source_freshness,
    available,
    reason,
    items,
    total: asNumber(rec.total, items.length),
    shown: asNumber(rec.shown, items.length),
    truncated: asBool(rec.truncated, false),
    ...(foldedWalk ? { walk_incomplete: foldedWalk } : {}),
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
    // `parse_status_name` emits one of five short tokens — clean | partial |
    // fallback | failed | skipped — so this clip never fires on real data.
    // It is still an unconstrained String on the wire, and the commit
    // composer's details view prints it verbatim. Bounding is safe precisely
    // because reliability is not decided by this field alone: the engine also
    // sets degraded_reason / delta_available, which is what honestyFromReport
    // actually reads.
    parse_status: boundText(asString(rec.parse_status, "Unknown"), WALK_INCOMPLETE_MAX_CHARS),
    delta_available: asBool(rec.delta_available, false),
    file_is_indexed: asBool(rec.file_is_indexed, false),
    compared_against: asString(rec.compared_against, "unknown"),
    degraded_reason: boundEngineText(rec.degraded_reason),
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
