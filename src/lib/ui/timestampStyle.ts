import { formatDate, formatRelativeTime } from "../format";

/**
 * How commit times read across the app.
 *
 * `relative` ("3d ago") is what GitPulse has always shown and stays the
 * default. `absolute` is `YYYY-MM-DD`: fixed width, sortable and unambiguous
 * between locales, which matters in a column that has to line up and in a
 * tool where 06/09 means two different days depending on who is reading.
 *
 * One owner so the choice cannot be honoured in the commit list and ignored
 * in the tooltip beside it.
 */
export type TimestampStyle = "relative" | "absolute";

export const TIMESTAMP_STYLES: readonly TimestampStyle[] = ["relative", "absolute"];

export function isTimestampStyle(value: unknown): value is TimestampStyle {
  return value === "relative" || value === "absolute";
}

/** `timestampSec` is unix epoch SECONDS, matching git and `format.ts`. */
export function formatAbsoluteDate(timestampSec: number): string {
  if (!timestampSec) return "";
  const date = new Date(timestampSec * 1000);
  if (Number.isNaN(date.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/**
 * The commit time in the reader's chosen style.
 *
 * Returns "" for a falsy timestamp in both styles, matching
 * `formatRelativeTime`, so a caller's `|| fallback` keeps working when the
 * preference flips.
 */
export function formatTimestamp(
  timestampSec: number,
  style: TimestampStyle,
  nowSec?: number,
): string {
  return style === "absolute"
    ? formatAbsoluteDate(timestampSec)
    : formatRelativeTime(timestampSec, nowSec);
}

/**
 * The other style, for the `title` attribute.
 *
 * Whichever form is on screen, hovering gives the one it is not: the full
 * local date and time under a relative label, and how long ago under a date.
 * Neither style loses information the other carried.
 */
export function timestampTitle(
  timestampSec: number,
  style: TimestampStyle,
  nowSec?: number,
): string {
  if (!timestampSec) return "";
  return style === "absolute"
    ? formatRelativeTime(timestampSec, nowSec)
    : formatDate(timestampSec);
}
