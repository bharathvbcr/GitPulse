/**
 * Formatting helpers for the Storage view. Pure functions so they are
 * unit-testable and reusable from the copy-as-text renderer.
 */

/**
 * Human-readable byte size, du-style: 1024-based units with decimal labels.
 * Guards non-finite/negative inputs by rendering them as "—".
 */
export function humanBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  const units = ["KB", "MB", "GB", "TB", "PB"];
  let value = bytes;
  let unit = -1;
  do {
    value /= 1024;
    unit += 1;
  } while (value >= 1024 && unit < units.length - 1);
  const digits = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(digits)} ${units[unit]}`;
}

/** Share of `part` in `total`, 0–100. Zero/invalid totals yield 0. */
export function pctOf(part: number, total: number): number {
  if (!Number.isFinite(part) || !Number.isFinite(total) || total <= 0) return 0;
  const pct = (part / total) * 100;
  if (!Number.isFinite(pct)) return 0;
  return Math.min(100, Math.max(0, pct));
}

/**
 * Signed byte delta for history rows: "+1.5 MB", "−320 KB", or "no change".
 */
export function formatDelta(deltaBytes: number): string {
  if (!Number.isFinite(deltaBytes) || deltaBytes === 0) return "no change";
  const sign = deltaBytes > 0 ? "+" : "−";
  return `${sign}${humanBytes(Math.abs(deltaBytes))}`;
}

/** CSS class for a signed delta: growth reads as warning, shrink as good. */
export function deltaClass(deltaBytes: number): string {
  if (!Number.isFinite(deltaBytes) || deltaBytes === 0) return "text-textMuted";
  return deltaBytes > 0 ? "text-amber-300" : "text-emerald-300";
}

const ABSOLUTE_TIME = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** Absolute short timestamp ("Aug 25, 1:42 PM") for snapshot tables. */
export function formatSnapshotTime(epochMs: number): string {
  if (!Number.isFinite(epochMs) || epochMs <= 0) return "—";
  try {
    return ABSOLUTE_TIME.format(new Date(epochMs));
  } catch {
    return "—";
  }
}

/**
 * Coarse relative age ("4m ago", "3h ago", "12d ago"), falling back to the
 * absolute stamp once past a month.
 */
export function formatAge(epochMs: number, nowMs: number = Date.now()): string {
  if (!Number.isFinite(epochMs) || epochMs <= 0) return "—";
  const seconds = Math.round((nowMs - epochMs) / 1000);
  if (seconds < 45) return "just now";
  if (seconds < 90) return "1m ago";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 31) return `${days}d ago`;
  return formatSnapshotTime(epochMs);
}
