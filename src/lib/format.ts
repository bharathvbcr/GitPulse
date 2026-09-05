/**
 * Shared display formatters. These are ports of the per-component helpers
 * they replaced; the exact output strings (including thresholds and the
 * empty-string fallbacks) are intentional and covered by format.test.ts.
 */

/** `timestampSec`/`nowSec` are unix epoch SECONDS, matching git timestamps. */
export function formatRelativeTime(timestampSec: number, nowSec?: number): string {
  if (!timestampSec) return "";
  const now = nowSec ?? Math.floor(Date.now() / 1000);
  const diff = Math.max(0, now - timestampSec);
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  if (diff < 2592000) return `${Math.floor(diff / 86400)}d ago`;
  return `${Math.floor(diff / 2592000)}mo ago`;
}

/** Locale date-time for a unix epoch SECONDS timestamp; empty for falsy input. */
export function formatDate(ts: number): string {
  if (!ts) return "";
  return new Date(ts * 1000).toLocaleString();
}

/** Safe truncation for commit hashes; tolerates missing input. */
export function shortHash(hash: string | null | undefined, len = 7): string {
  if (!hash) return "";
  return hash.slice(0, len);
}

/**
 * "1 repository" / "3 repositories" — a count with the right noun form.
 *
 * Canonical for the app: declared once here because two independent copies
 * (fleet/aggregate.ts, repos/wipSummary.ts) had already appeared, and a
 * pluralisation rule spelled twice can be corrected on one side only.
 */
export function plural(count: number, singular: string, pluralForm = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : pluralForm}`;
}
