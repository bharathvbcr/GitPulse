/**
 * The row model the delivery timeline draws, and the arithmetic behind it.
 *
 * One model for both sources: a workflow run and an App Hosting rollout are
 * the same question asked twice — did this attempt at delivery start, how long
 * did it take, and how did it end. The vocabularies that produce a phase stay
 * separate; this is where they meet.
 *
 * Every number here can be absent, and absent is never zero. A run with no
 * start time has an unknown duration, not a zero-second one; a bar with no
 * length is drawn as no bar rather than as an instant success.
 */
import { isInFlight, isVerdict, type MonitorPhase } from "./phase";

export interface TimelineRow {
  id: string;
  /** Primary label: a run's title, a rollout's id. */
  label: string;
  /** Secondary label: a workflow name, a backend id. Empty when there is none. */
  sublabel: string;
  phase: MonitorPhase;
  /** The state word from the source, shown verbatim so it can be searched for. */
  stateLabel: string;
  /** ISO 8601, or empty when the source reported none. */
  startedAt: string;
  /** ISO 8601 of the last change, or empty. */
  endedAt: string;
  /** Commit this attempt delivered, empty when unreported. */
  commitSha: string;
  /** Branch, empty when unreported. */
  branch: string;
  /** What triggered it, empty when unreported. */
  trigger: string;
  /** Link to the source's own page, empty when there is none. */
  url: string;
}

/**
 * Longest label this model will carry.
 *
 * A run's title is a commit subject and a rollout's sublabel is a commit
 * message line, both of which are bounded only by what someone typed. Two
 * hundred characters is far past the width of any column here, so the cap
 * changes nothing a reader would have seen while keeping a pathological
 * message out of the DOM and out of a `title` attribute.
 */
export const MAX_LABEL_CHARS = 200;

/**
 * Detail rows drawn before the reader asks for the rest.
 *
 * The outcome strip and the sample figures stay on the full listing: this
 * only decides how many duration rows are on screen. Three is short enough
 * that the rest of the CI rail stays reachable in a narrow column. Shared
 * with App Hosting rollouts through DeliveryTimeline, not a GitHub-only
 * constant — a second count is how "show all" would split in two.
 */
export const TIMELINE_PREVIEW_COUNT = 3;

/**
 * How many characters a glance tile may show for identity.
 *
 * The duration card is not the place for a commit subject. Sixteen code
 * points is a workflow name or a short rollout id; anything longer is
 * truncated, and the full string stays on the tile's `title`.
 */
export const GLANCE_NAME_CHARS = 16;

/**
 * The identity a glance tile shows.
 *
 * Actions puts the commit subject in `label` and the workflow name in
 * `sublabel`. App Hosting inverts that: the rollout id is the label and the
 * commit subject is the sublabel. Preferring the shorter non-empty of the
 * two is how both sources land on the scannable name rather than the essay.
 */
export function glanceName(row: Pick<TimelineRow, "label" | "sublabel">): string {
  const primary = displayText(row.label);
  const secondary = displayText(row.sublabel);
  const len = (value: string) => [...value].length;
  let chosen = primary;
  if (secondary && (!primary || len(secondary) <= len(primary))) chosen = secondary;
  const chars = [...chosen];
  if (chars.length <= GLANCE_NAME_CHARS) return chosen;
  return `${chars.slice(0, GLANCE_NAME_CHARS).join("")}…`;
}

/**
 * The verdict a glance tile shows.
 *
 * Phase, not the source's sentence: "Completed (no conclusion reported)" is
 * a search term for the tooltip, not a cell. Unknown is an em dash, never
 * a word that could be mistaken for a pass.
 */
export function glanceState(phase: MonitorPhase): string {
  if (phase === "settled_ok") return "Pass";
  if (phase === "settled_bad") return "Fail";
  if (phase === "in_flight") return "Live";
  return "—";
}

/** Full identity for the tooltip; every field is already bounded. */
export function glanceTitle(row: TimelineRow): string {
  return [row.label, row.stateLabel, row.branch, row.trigger].filter((part) => part.length > 0).join(" · ");
}

/**
 * One line of display text, bounded.
 *
 * Newlines are collapsed rather than trusted to CSS. Every label today lands
 * in an element with `truncate` — which implies `nowrap` — but that makes the
 * invariant a property of each call site rather than of the model, and the
 * first consumer to render a label without it gets a row that pushes every
 * later row down the page.
 */
export function displayText(value: unknown): string {
  if (typeof value !== "string") return "";
  // Every Unicode line terminator, written as code-point escapes: U+2028 and
  // U+2029 are themselves line terminators in JavaScript source, so a literal
  // one inside this regex would end the literal and not compile.
  const flattened = value.replace(/[\u000A\u000D\u2028\u2029]+/g, " ").trim();
  if (flattened.length <= MAX_LABEL_CHARS) return flattened;
  // Slice by code point, not by UTF-16 unit: cutting mid-surrogate produces a
  // lone half that renders as a replacement character.
  return `${[...flattened].slice(0, MAX_LABEL_CHARS).join("")}…`;
}

/**
 * Milliseconds for an ISO timestamp, or null when it is not one.
 *
 * `Date.parse` returns NaN for junk and for the empty string, and NaN
 * propagates silently through every subtraction downstream — a duration of NaN
 * renders as an empty bar that looks exactly like a fast one. Converting to
 * null here forces every caller to handle the absence.
 */
export function parseInstant(iso: string): number | null {
  if (typeof iso !== "string" || iso.trim() === "") return null;
  const ms = Date.parse(iso);
  return Number.isFinite(ms) ? ms : null;
}

/**
 * How long this attempt took, in milliseconds, or null when unknowable.
 *
 * Three ways to be unknowable, and all three must stay distinct from zero:
 *
 *  - never started (queued): no start instant to measure from;
 *  - still running: measured against `now`, so it grows — but only when `now`
 *    is actually after the start;
 *  - the clock disagrees with itself: an end before its start is skew, a
 *    suspended machine, or a timezone bug upstream. Clamping it to zero would
 *    publish "0s" for a run that took ten minutes. Null says so instead.
 */
export function durationMs(row: TimelineRow, now: number): number | null {
  const started = parseInstant(row.startedAt);
  if (started === null) return null;
  if (isInFlight(row.phase)) {
    if (!Number.isFinite(now)) return null;
    const elapsed = now - started;
    return elapsed >= 0 ? elapsed : null;
  }
  const ended = parseInstant(row.endedAt);
  if (ended === null) return null;
  const span = ended - started;
  return span >= 0 ? span : null;
}

/**
 * The longest known duration in the sample, or null when none is known.
 *
 * This is the bar scale's denominator, which is the whole reason it returns
 * null rather than 0: dividing by a zero maximum yields Infinity, and a bar
 * width of `Infinity%` clamps to full width, drawing every unknown row as the
 * slowest run in the list.
 */
export function longestDurationMs(rows: readonly TimelineRow[], now: number): number | null {
  let longest: number | null = null;
  for (const row of rows) {
    const ms = durationMs(row, now);
    if (ms === null) continue;
    if (longest === null || ms > longest) longest = ms;
  }
  return longest;
}

/**
 * A bar's width as a percentage of the widest in the sample.
 *
 * Returns null when there is nothing to scale against, and clamps into
 * [0, 100] so a duration that arrives longer than the sample maximum — which
 * happens whenever an in-flight row keeps growing between the scale being
 * computed and the row being drawn — cannot overflow its track.
 *
 * A known-but-tiny duration floors at a visible sliver rather than zero: a
 * three-second run is a real row, and a zero-width bar reads as missing data,
 * which is the one thing it is not.
 */
export function barWidthPct(ms: number | null, longestMs: number | null): number | null {
  if (ms === null || longestMs === null) return null;
  if (!Number.isFinite(ms) || !Number.isFinite(longestMs) || longestMs <= 0) return null;
  const pct = (ms / longestMs) * 100;
  if (!Number.isFinite(pct)) return null;
  return Math.max(1.5, Math.min(100, pct));
}

/**
 * A duration for humans: `1.4s`, `2m 05s`, `1h 12m`.
 *
 * Null in, em dash out. The dash is the point — it is visibly not a number, so
 * an unknown duration cannot be misread as a fast one.
 */
export function formatDuration(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const totalSeconds = Math.floor(ms / 1000);
  if (totalSeconds < 60) {
    // One decimal below a minute: the difference between 3s and 3.4s is
    // meaningful when comparing two runs of the same workflow.
    return `${(ms / 1000).toFixed(1)}s`;
  }
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes < 60) return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
}

/** Short SHA for display only, matching git's own default abbreviation. */
export function shortCommit(sha: string): string {
  return typeof sha === "string" ? sha.slice(0, 7) : "";
}

/**
 * Median of the known durations among rows that carry a verdict.
 *
 * In-flight rows are excluded because their duration is still growing, and a
 * median that includes them drifts every time the poll fires. Unjudgeable rows
 * are excluded for the same reason they are excluded from a pass rate.
 *
 * Returns null on an empty sample rather than 0, and reports `sample` so the
 * figure is never shown without the count behind it.
 */
export function medianSettledDurationMs(
  rows: readonly TimelineRow[],
  now: number,
): { medianMs: number | null; sample: number } {
  const durations: number[] = [];
  for (const row of rows) {
    if (!isVerdict(row.phase)) continue;
    const ms = durationMs(row, now);
    if (ms !== null) durations.push(ms);
  }
  if (durations.length === 0) return { medianMs: null, sample: 0 };
  durations.sort((a, b) => a - b);
  const mid = Math.floor(durations.length / 2);
  const medianMs =
    durations.length % 2 === 1 ? durations[mid] : (durations[mid - 1] + durations[mid]) / 2;
  return { medianMs, sample: durations.length };
}
