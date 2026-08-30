/**
 * Shared reading of scan limit notices.
 *
 * Both scanners bound their work and report what they kept. A renderer that
 * headlines the retained count presents a capped sample as complete coverage
 * ("showing 30 of 141" for a repo where 12,873 files were seen), so every
 * bounded section asks this module for the observed total instead of counting
 * the rows it happens to hold.
 *
 * Structurally typed on purpose: `analyzer::deps::ScanLimitNotice` and
 * `analyzer::coverage::CoverageScanLimit` are separate wire types with the
 * same shape and the same meaning, and each keeps its own IPC mirror.
 */
export interface LimitNotice {
  resource: string;
  kept: number;
  total: number;
}

export interface LimitBearingReport {
  limit_notices?: LimitNotice[] | null;
}

function usableTotal(value: unknown): number | null {
  // Notices cross IPC from a producer we do not control. A non-finite,
  // negative or fractional total is not evidence of anything, and must not
  // be able to make a section claim fewer rows than it is about to print.
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return null;
  // Clamp to the safe-integer range for the same reason `safeCount` does:
  // 1e308 is finite and positive, and rendering it as "of 1e+308" is not a
  // count anyone can read. Matches the coverage renderer's own ceiling.
  return Math.min(Number.MAX_SAFE_INTEGER, Math.trunc(value));
}

/**
 * How many of a bounded collection the scan actually observed, as opposed to
 * how many survived the cap.
 *
 * Falls back to the retained count whenever no usable notice exists — an
 * absent notice means "nothing was dropped", never "unknown". A notice whose
 * total is below the retained count is discarded rather than believed: the
 * observed total can never be smaller than what we still hold.
 */
export function observedTotal(
  report: LimitBearingReport | null | undefined,
  resource: string,
  retained: number,
): number {
  const floor = usableTotal(retained) ?? 0;
  const notices = report?.limit_notices;
  if (!Array.isArray(notices)) return floor;
  const match = notices.find(
    (notice) => !!notice && typeof notice === "object" && notice.resource === resource,
  );
  const total = usableTotal(match?.total);
  return total === null ? floor : Math.max(total, floor);
}

/**
 * Suffix disclosing that a section is showing fewer rows than were observed.
 * Empty when nothing was dropped, so complete sections read unchanged.
 */
export function cappedSuffix(observed: number, shown: number): string {
  return observed > shown ? `; showing ${shown}` : "";
}
