/**
 * The diff minimap: where the changes are, where you are, and where a click
 * should take you.
 *
 * The old strip got two things wrong and had no way to notice either. It
 * built its ticks from the unified line list and then wrote the resulting
 * scroll offset into whichever pane was on screen — in split view the content
 * is a different list of a different length, so every tick pointed somewhere
 * else and a click landed off by the ratio between the two. And it mapped a
 * click to `ratio × contentHeight`, which puts the target at the TOP of the
 * viewport: clicking the last tick scrolls past the end and clamps, so the
 * thing you aimed at is off screen.
 *
 * Both come from the same missing idea — that the map is a projection of one
 * specific list — so this module takes the tones of the list actually being
 * drawn and answers in that list's coordinates. It also draws the viewport
 * band, which is what turns a decoration into a map.
 */

export interface MinimapTick {
  key: string;
  topPct: number;
  heightPct: number;
  /** A `TONE_*` value from `rowModel`. */
  tone: number;
}

export interface ViewportBand {
  topPct: number;
  heightPct: number;
}

import {
  TONE_ADD,
  TONE_DEL,
  TONE_FILE,
  TONE_HUNK,
  TONE_MOD,
  TONE_NONE,
} from "./rowModel";

/**
 * How many buckets the strip is divided into.
 *
 * The strip is a few hundred pixels tall, so more ticks than this cannot be
 * told apart; fewer, and a single-line change in a large diff disappears.
 */
export const MAX_TICKS = 160;

/** The smallest tick that is still visible on a 400px strip. */
const MIN_TICK_PCT = 0.9;

/**
 * Buckets `tones` into ticks.
 *
 * A bucket holding both additions and deletions is a rewrite, and reporting
 * it as one or the other is how a minimap comes to claim a refactor was pure
 * growth. Buckets with nothing but context produce no tick at all, so the
 * strip reads as "here are the changes" rather than as a solid bar.
 */
export function buildTicks(tones: Uint8Array, maxTicks: number = MAX_TICKS): MinimapTick[] {
  const total = tones.length;
  if (total === 0 || maxTicks <= 0) return [];
  const step = Math.max(1, Math.ceil(total / maxTicks));
  const ticks: MinimapTick[] = [];
  for (let start = 0; start < total; start += step) {
    const end = Math.min(total, start + step);
    let adds = 0;
    let dels = 0;
    let mods = 0;
    let hunks = 0;
    let files = 0;
    for (let i = start; i < end; i += 1) {
      switch (tones[i]) {
        case TONE_ADD:
          adds += 1;
          break;
        case TONE_DEL:
          dels += 1;
          break;
        case TONE_MOD:
          mods += 1;
          break;
        case TONE_HUNK:
          hunks += 1;
          break;
        case TONE_FILE:
          files += 1;
          break;
        default:
          break;
      }
    }
    let tone = TONE_NONE;
    if (mods > 0 || (adds > 0 && dels > 0)) tone = TONE_MOD;
    else if (adds > 0) tone = TONE_ADD;
    else if (dels > 0) tone = TONE_DEL;
    else if (files > 0) tone = TONE_FILE;
    else if (hunks > 0) tone = TONE_HUNK;
    if (tone === TONE_NONE) continue;
    ticks.push({
      key: `t${start}`,
      topPct: (start / total) * 100,
      heightPct: Math.max(MIN_TICK_PCT, ((end - start) / total) * 100),
      tone,
    });
  }
  return ticks;
}

/** The largest scroll offset that still shows content, never negative. */
export function maxScroll(rowCount: number, rowHeight: number, viewportHeight: number): number {
  if (!Number.isFinite(rowCount) || !Number.isFinite(rowHeight)) return 0;
  const content = Math.max(0, rowCount) * Math.max(0, rowHeight);
  const viewport = Number.isFinite(viewportHeight) ? Math.max(0, viewportHeight) : 0;
  return Math.max(0, content - viewport);
}

/**
 * Where to scroll so the row at `ratio` through the list sits in the middle
 * of the viewport.
 *
 * Centring rather than top-aligning is the whole difference between a map and
 * a scrollbar: aiming at the last tick has to show the last tick, and a
 * top-aligned jump clamps it off the bottom of the screen.
 */
export function scrollForRatio(
  ratio: number,
  rowCount: number,
  rowHeight: number,
  viewportHeight: number,
): number {
  const clamped = Math.max(0, Math.min(1, Number.isFinite(ratio) ? ratio : 0));
  const viewport = Number.isFinite(viewportHeight) ? Math.max(0, viewportHeight) : 0;
  const target = clamped * Math.max(0, rowCount) * Math.max(0, rowHeight);
  return Math.max(0, Math.min(maxScroll(rowCount, rowHeight, viewport), target - viewport / 2));
}

/** Scroll offset that centres one row, for "go to this line" jumps. */
export function scrollForRow(
  rowIndex: number,
  rowCount: number,
  rowHeight: number,
  viewportHeight: number,
): number {
  const total = Math.max(0, rowCount);
  if (total === 0) return 0;
  const index = Math.max(0, Math.min(total - 1, Math.trunc(rowIndex)));
  const viewport = Number.isFinite(viewportHeight) ? Math.max(0, viewportHeight) : 0;
  const target = index * Math.max(0, rowHeight) - viewport / 2 + Math.max(0, rowHeight) / 2;
  return Math.max(0, Math.min(maxScroll(total, rowHeight, viewport), target));
}

/**
 * The band marking what is currently on screen, or null when everything is.
 *
 * Null rather than a full-height band: a band covering the whole strip says
 * nothing and only adds a wash of colour over the ticks.
 */
export function viewportBand(
  scrollTop: number,
  viewportHeight: number,
  rowCount: number,
  rowHeight: number,
): ViewportBand | null {
  const content = Math.max(0, rowCount) * Math.max(0, rowHeight);
  const viewport = Number.isFinite(viewportHeight) ? Math.max(0, viewportHeight) : 0;
  if (content <= 0 || viewport <= 0 || viewport >= content) return null;
  const top = Math.max(0, Math.min(content - viewport, Number.isFinite(scrollTop) ? scrollTop : 0));
  return {
    topPct: (top / content) * 100,
    heightPct: Math.max(MIN_TICK_PCT, (viewport / content) * 100),
  };
}

/** Vertical position of a pointer within a strip, as a 0–1 ratio. */
export function ratioFromPointer(clientY: number, top: number, height: number): number {
  if (!Number.isFinite(height) || height <= 0) return 0;
  const offset = (Number.isFinite(clientY) ? clientY : 0) - (Number.isFinite(top) ? top : 0);
  return Math.max(0, Math.min(1, offset / height));
}

/**
 * Where each file starts, as percentages down the strip.
 *
 * A commit diff is a stack of files, and without these marks the strip says
 * "changes are spread evenly" for a change that rewrote one file and touched
 * a comma in forty others.
 */
export function fileMarks(
  startIndices: readonly number[],
  rowCount: number,
  mapRow?: (lineIndex: number) => number,
): number[] {
  const total = Math.max(0, rowCount);
  if (total === 0) return [];
  const marks: number[] = [];
  for (const index of startIndices) {
    const row = mapRow ? mapRow(index) : index;
    if (!Number.isFinite(row) || row < 0) continue;
    const pct = (Math.min(row, total - 1) / total) * 100;
    // The first file always starts at the top; a mark there is a line under
    // the strip's own edge and reads as a rendering artifact.
    if (pct <= 0) continue;
    marks.push(pct);
  }
  return marks;
}
