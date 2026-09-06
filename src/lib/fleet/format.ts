/**
 * Rendering a change in a Fleet cell.
 *
 * One owner for the delta chip, because "how did this move?" is asked of four
 * columns whose units all differ — lines, bytes, a count, a percentage — and
 * four separate spellings of the same idea is how one of them ends up saying
 * "0" for a repository that has only ever been measured once.
 *
 * Two rules, and they are the reason this is a module rather than markup:
 *
 * 1. **No baseline means no chip.** `deltaFrom` returns `undefined` rather than
 *    a zero, and nothing here manufactures one.
 * 2. **Down is not always good.** Fewer vulnerabilities is an improvement;
 *    less coverage is not. The direction a column *wants* is stated per column
 *    rather than assumed from the sign.
 */

import { humanBytes } from "../storage/format";
import type { CellDelta } from "./types";

/** Which direction of change a column treats as an improvement. */
export type DeltaGoal = "lower" | "higher" | "neutral";

/** How a delta is spelled, per column. */
export type DeltaUnit = "count" | "bytes" | "percent";

function magnitude(change: number, unit: DeltaUnit): string {
  const size = Math.abs(change);
  if (unit === "bytes") return humanBytes(size);
  if (unit === "percent") return `${size.toFixed(1)}pp`;
  return size.toLocaleString();
}

/**
 * The chip's text: a signed magnitude, or "" when nothing moved.
 *
 * An unchanged measurement returns the empty string rather than "+0", so the
 * only thing a reader ever sees is an actual change. That keeps the chip's
 * presence meaningful: it appears exactly when something happened.
 */
export function formatDelta(delta: CellDelta, unit: DeltaUnit): string {
  if (!Number.isFinite(delta.change) || delta.change === 0) return "";
  const sign = delta.change > 0 ? "+" : "−";
  return `${sign}${magnitude(delta.change, unit)}`;
}

/** Tone for the chip: green for the direction the column wants, amber against. */
export function deltaTone(delta: CellDelta, goal: DeltaGoal): string {
  if (!Number.isFinite(delta.change) || delta.change === 0 || goal === "neutral") {
    return "text-textMuted";
  }
  const improved = goal === "lower" ? delta.change < 0 : delta.change > 0;
  return improved
    ? "text-emerald-600 dark:text-emerald-400"
    : "text-amber-600 dark:text-amber-400";
}

/**
 * The full sentence behind the chip, for its tooltip.
 *
 * Always names the baseline day. A delta whose baseline is unstated invites the
 * reader to assume it means "since yesterday", which it usually does not:
 * families are scanned independently, so a coverage baseline can be months
 * older than a storage one on the same row.
 */
export function describeDelta(delta: CellDelta, unit: DeltaUnit, label: string): string {
  const from = unit === "bytes"
    ? humanBytes(delta.from)
    : unit === "percent"
      ? `${delta.from.toFixed(1)}%`
      : delta.from.toLocaleString();
  if (!Number.isFinite(delta.change) || delta.change === 0) {
    return `${label} is unchanged since ${delta.day}, when it was ${from}.`;
  }
  const direction = delta.change > 0 ? "up" : "down";
  return `${label} is ${direction} ${magnitude(delta.change, unit)} since ${delta.day}, when it was ${from}.`;
}
