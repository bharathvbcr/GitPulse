/**
 * The side-by-side row model, derived from the unified line list.
 *
 * Split view used to build its own rows by carrying ONE pending deletion:
 * a block of D deletions followed by A additions came out as D-1 rows with an
 * empty right column, one paired row, then A-1 rows with an empty left column.
 * The two sides were vertically offset by the size of the block, which is the
 * one thing a side-by-side diff exists to prevent — and on a 2000/2000
 * replacement it produced 3,997 rows instead of 2,000.
 *
 * It also paired the LAST deletion with the FIRST addition for intra-line
 * highlighting, while the unified view's [`annotateRange`] pairs `del[k]` with
 * `add[k]`. Both mutate the same parsed line objects and both skip a line that
 * already carries segments, so the highlight a reader saw depended on which
 * view they had opened first.
 *
 * So the pairing lives here, once, and matches `annotateRange` exactly:
 * within a replacement block, `del[k]` faces `add[k]`, and the longer side
 * spills into rows whose other column is empty. Unified keeps rendering the
 * flat line list; split renders these rows; neither owns the decision alone.
 *
 * Every row carries indices back into the source lines, so a split row can do
 * everything a unified row can — line selection, hunk staging, jumping to a
 * search hit — instead of being a read-only rendering.
 */

import type { AnnotatedDiffLine } from "./wordDiff";

/** Chrome that belongs to neither side: file headers, hunk headers, metadata. */
export interface SplitSpanRow {
  kind: "span";
  line: AnnotatedDiffLine;
  /** Index of `line` in the source list. */
  index: number;
}

/** One line of the old file beside its counterpart in the new file. */
export interface SplitCodeRow {
  kind: "code";
  left: AnnotatedDiffLine | null;
  right: AnnotatedDiffLine | null;
  /** Index of `left` in the source list; -1 when this row has no left side. */
  leftIndex: number;
  /** Index of `right` in the source list; -1 when this row has no right side. */
  rightIndex: number;
}

export type SplitRow = SplitSpanRow | SplitCodeRow;

export interface SplitModel {
  rows: SplitRow[];
  /**
   * Source line index → split row index, so a position in one view maps to
   * the same code in the other. Toggling Unified/Split keeps the reader's
   * place, and a search hit found on the line list can be scrolled to in
   * either view. -1 for a line that reached no row (impossible today, but
   * the array is sized for every line so lookups never need a guard).
   */
  lineToRow: Int32Array;
}

export const EMPTY_SPLIT_MODEL: SplitModel = {
  rows: [],
  lineToRow: new Int32Array(0),
};

/** True for the row kinds that carry file content rather than chrome. */
function isCodeLine(line: AnnotatedDiffLine | undefined): boolean {
  return !!line && (line.type === "add" || line.type === "del" || line.type === "ctx");
}

/**
 * Builds the side-by-side rows for one parsed diff.
 *
 * Context lines occupy both columns as the same object — identity, not a
 * copy, so a caller can compare `left === right` to recognise unchanged rows
 * without re-reading the type.
 */
export function buildSplitRows(lines: readonly AnnotatedDiffLine[]): SplitModel {
  const rows: SplitRow[] = [];
  const lineToRow = new Int32Array(lines.length).fill(-1);

  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.type === "ctx") {
      lineToRow[i] = rows.length;
      rows.push({ kind: "code", left: line, right: line, leftIndex: i, rightIndex: i });
      i += 1;
      continue;
    }
    if (line.type === "del" || line.type === "add") {
      // One replacement block: every deletion, then every addition. This is
      // the same extent `replacementBlockBounds` computes, so the pairing
      // below cannot disagree with the unified view's word-diff pairing.
      const delStart = i;
      while (i < lines.length && lines[i].type === "del") i += 1;
      const addStart = i;
      while (i < lines.length && lines[i].type === "add") i += 1;
      const delCount = addStart - delStart;
      const addCount = i - addStart;
      const height = Math.max(delCount, addCount);
      for (let k = 0; k < height; k += 1) {
        const leftIndex = k < delCount ? delStart + k : -1;
        const rightIndex = k < addCount ? addStart + k : -1;
        if (leftIndex >= 0) lineToRow[leftIndex] = rows.length;
        if (rightIndex >= 0) lineToRow[rightIndex] = rows.length;
        rows.push({
          kind: "code",
          left: leftIndex >= 0 ? lines[leftIndex] : null,
          right: rightIndex >= 0 ? lines[rightIndex] : null,
          leftIndex,
          rightIndex,
        });
      }
      continue;
    }
    lineToRow[i] = rows.length;
    rows.push({ kind: "span", line, index: i });
    i += 1;
  }

  return { rows, lineToRow };
}

/**
 * The split row showing `lineIndex`, or the nearest one at or before it.
 *
 * A line that reached no row cannot happen for a well-formed model, but a
 * caller mapping an arbitrary scroll anchor may land on one, and answering
 * "nowhere" would throw the reader to the top of the file. Walking backwards
 * keeps the mapping monotonic, which is what a scroll position needs.
 */
export function splitRowForLine(model: SplitModel, lineIndex: number): number {
  if (model.rows.length === 0) return 0;
  const clamped = Math.max(0, Math.min(model.lineToRow.length - 1, Math.trunc(lineIndex)));
  for (let i = clamped; i >= 0; i -= 1) {
    const row = model.lineToRow[i];
    if (row >= 0) return row;
  }
  return 0;
}

/** The source line index a split row anchors on, for the reverse mapping. */
export function lineForSplitRow(model: SplitModel, rowIndex: number): number {
  const row = model.rows[Math.max(0, Math.min(model.rows.length - 1, Math.trunc(rowIndex)))];
  if (!row) return 0;
  if (row.kind === "span") return row.index;
  if (row.leftIndex >= 0) return row.leftIndex;
  return Math.max(0, row.rightIndex);
}

/**
 * Row tone, used by the minimap and by the "next change" stepper.
 *
 * A `Uint8Array` rather than a string list: a 300k-line diff builds this on
 * every parse, and eighty tick buckets have to scan all of it.
 */
export const TONE_NONE = 0;
export const TONE_CTX = 1;
export const TONE_ADD = 2;
export const TONE_DEL = 3;
export const TONE_HUNK = 4;
export const TONE_FILE = 5;
/** A split row that replaces one line with another: both sides changed. */
export const TONE_MOD = 6;

export function lineTones(lines: readonly AnnotatedDiffLine[]): Uint8Array {
  const tones = new Uint8Array(lines.length);
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    switch (line.type) {
      case "add":
        tones[i] = TONE_ADD;
        break;
      case "del":
        tones[i] = TONE_DEL;
        break;
      case "ctx":
        tones[i] = TONE_CTX;
        break;
      case "hdr":
        tones[i] = line.content.startsWith("@@") ? TONE_HUNK : TONE_FILE;
        break;
      case "meta":
        tones[i] = line.content.startsWith("diff --git ") ? TONE_FILE : TONE_NONE;
        break;
      default:
        tones[i] = TONE_NONE;
    }
  }
  return tones;
}

/** Tones for the split rows, so the minimap is calibrated to what is drawn. */
export function splitTones(model: SplitModel): Uint8Array {
  const tones = new Uint8Array(model.rows.length);
  for (let i = 0; i < model.rows.length; i += 1) {
    const row = model.rows[i];
    if (row.kind === "span") {
      const content = row.line.content;
      if (row.line.type === "hdr") {
        tones[i] = content.startsWith("@@") ? TONE_HUNK : TONE_FILE;
      } else if (row.line.type === "meta" && content.startsWith("diff --git ")) {
        tones[i] = TONE_FILE;
      } else {
        tones[i] = TONE_NONE;
      }
      continue;
    }
    // A replacement row is both an addition and a deletion. Calling it one or
    // the other would make the minimap report a rewrite as pure growth, so it
    // gets its own tone.
    if (row.left && row.right && row.left !== row.right) tones[i] = TONE_MOD;
    else if (row.right && row.right.type === "add") tones[i] = TONE_ADD;
    else if (row.left && row.left.type === "del") tones[i] = TONE_DEL;
    else tones[i] = TONE_CTX;
  }
  return tones;
}

/** True when a tone marks a line the commit actually changed. */
export function isChangeTone(tone: number): boolean {
  return tone === TONE_ADD || tone === TONE_DEL || tone === TONE_MOD;
}

/**
 * The next row at or after `from` that starts a new run of changes.
 *
 * "Next change" means the next block, not the next changed line: stepping
 * line by line through a 400-line rewrite is the scrolling it was meant to
 * replace. From inside a block, forward leaves it and backward returns to its
 * first line — the two together let a reader walk to the top of the change
 * they are in the middle of and then keep going.
 *
 * Returns null at either end rather than wrapping, so the control can disable
 * instead of silently sending the reader back to the top.
 */
export function nextChangeRow(tones: Uint8Array, from: number, delta: 1 | -1): number | null {
  const total = tones.length;
  if (total === 0) return null;
  const isBlockStart = (i: number): boolean =>
    isChangeTone(tones[i]) && (i === 0 || !isChangeTone(tones[i - 1]));
  const start = Math.max(-1, Math.min(total, Math.trunc(from)));
  if (delta > 0) {
    for (let i = start + 1; i < total; i += 1) {
      if (isBlockStart(i)) return i;
    }
    return null;
  }
  for (let i = Math.min(total, start) - 1; i >= 0; i -= 1) {
    if (isBlockStart(i)) return i;
  }
  return null;
}

/** Whether a line list holds anything a reader would call content. */
export function hasContentLines(lines: readonly AnnotatedDiffLine[]): boolean {
  for (const line of lines) {
    if (isCodeLine(line)) return true;
  }
  return false;
}
