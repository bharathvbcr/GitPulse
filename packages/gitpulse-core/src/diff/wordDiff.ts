/**
 * Pure TypeScript unified diff parser and bounded intra-line word diff engine.
 * Extracted from `src/lib/diff/wordDiff.ts` with zero Svelte/DOM/Tauri imports.
 */

export type DiffChunkKind = "Equal" | "Added" | "Removed";

export interface DiffSegment {
  kind: DiffChunkKind;
  text: string;
}

export interface IntraLineDiff {
  original_segments: DiffSegment[];
  modified_segments: DiffSegment[];
}

const MAX_TOKENS = 500;
const MAX_LINE_CHARS = 50_000;

function isWordChar(ch: string): boolean {
  return /[A-Za-z0-9_]/.test(ch);
}

function tokenizeLine(line: string, maxTokens: number): string[] {
  const tokens: string[] = [];
  let i = 0;
  while (i < line.length) {
    if (tokens.length >= maxTokens) {
      tokens.push(line.slice(i));
      break;
    }
    const start = i;
    const ch = line[i];
    const isAlnum = isWordChar(ch);
    i += 1;
    while (i < line.length && isWordChar(line[i]) === isAlnum) {
      i += 1;
    }
    tokens.push(line.slice(start, i));
  }
  return tokens;
}

function computeLcsTable(a: string[], b: string[]): number[][] {
  const m = a.length;
  const n = b.length;
  const table: number[][] = Array.from({ length: m + 1 }, () => Array(n + 1).fill(0));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      table[i][j] =
        a[i - 1] === b[j - 1]
          ? table[i - 1][j - 1] + 1
          : Math.max(table[i - 1][j], table[i][j - 1]);
    }
  }
  return table;
}

function mergeConsecutive(segments: DiffSegment[]): DiffSegment[] {
  const merged: DiffSegment[] = [];
  for (const seg of segments) {
    const last = merged[merged.length - 1];
    if (last && last.kind === seg.kind) {
      last.text += seg.text;
    } else {
      merged.push({ ...seg });
    }
  }
  return merged;
}

export function computeWordDiff(oldLine: string, newLine: string): IntraLineDiff {
  if (oldLine === newLine) {
    return {
      original_segments: [{ kind: "Equal", text: oldLine }],
      modified_segments: [{ kind: "Equal", text: newLine }],
    };
  }
  if (oldLine.length > MAX_LINE_CHARS || newLine.length > MAX_LINE_CHARS) {
    return {
      original_segments: [{ kind: "Removed", text: oldLine }],
      modified_segments: [{ kind: "Added", text: newLine }],
    };
  }

  const oldTokens = tokenizeLine(oldLine, MAX_TOKENS);
  const newTokens = tokenizeLine(newLine, MAX_TOKENS);
  const lcs = computeLcsTable(oldTokens, newTokens);

  let i = oldTokens.length;
  let j = newTokens.length;
  const origRev: DiffSegment[] = [];
  const modRev: DiffSegment[] = [];

  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && oldTokens[i - 1] === newTokens[j - 1]) {
      origRev.push({ kind: "Equal", text: oldTokens[i - 1] });
      modRev.push({ kind: "Equal", text: newTokens[j - 1] });
      i -= 1;
      j -= 1;
    } else if (j > 0 && (i === 0 || lcs[i][j - 1] >= lcs[i - 1][j])) {
      modRev.push({ kind: "Added", text: newTokens[j - 1] });
      j -= 1;
    } else if (i > 0) {
      origRev.push({ kind: "Removed", text: oldTokens[i - 1] });
      i -= 1;
    }
  }

  origRev.reverse();
  modRev.reverse();
  return {
    original_segments: mergeConsecutive(origRev),
    modified_segments: mergeConsecutive(modRev),
  };
}

export interface AnnotatedDiffLine {
  type: "add" | "del" | "ctx" | "hdr" | "meta" | "binary";
  content: string;
  oldNo?: number;
  newNo?: number;
  segments?: DiffSegment[];
  noNewline?: boolean;
}

const HUNK_RE = /^@@\s+-(\d+)(?:,\d+)?\s+\+(\d+)(?:,\d+)?\s+@@/;

const META_PREFIXES = [
  "diff --git ",
  "index ",
  "new file mode ",
  "deleted file mode ",
  "old mode ",
  "new mode ",
  "rename from ",
  "rename to ",
  "copy from ",
  "copy to ",
  "similarity index ",
  "dissimilarity index ",
];

const PATCH_BODY_PREFIX_RE = /^[-+ \\@]/;

export function classifyMetaLine(line: string): "meta" | "binary" | null {
  if (!line) return null;
  if (line.startsWith("Binary files ") || line.startsWith("GIT binary patch")) return "binary";
  if (META_PREFIXES.some((prefix) => line.startsWith(prefix))) return "meta";
  return PATCH_BODY_PREFIX_RE.test(line) ? null : "meta";
}

export function parseUnifiedDiff(raw: string | null | undefined): AnnotatedDiffLine[] {
  const out: AnnotatedDiffLine[] = [];
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;
  let inBinaryPayload = false;
  const rows = (raw || "").split("\n");
  if (rows.length > 0 && rows[rows.length - 1] === "") rows.pop();

  for (const line of rows) {
    if (inBinaryPayload) {
      if (line.startsWith("diff --git ")) {
        inBinaryPayload = false;
      } else {
        out.push({ type: "binary", content: line });
        continue;
      }
    }
    if (line.startsWith("@@")) {
      const match = HUNK_RE.exec(line);
      if (match) {
        oldNo = parseInt(match[1], 10);
        newNo = parseInt(match[2], 10);
      } else {
        oldNo = 0;
        newNo = 0;
      }
      out.push({ type: "hdr", content: line });
      inHunk = true;
      continue;
    }
    const metaKind = classifyMetaLine(line);
    if (metaKind) {
      if (line.startsWith("diff --git ")) inHunk = false;
      out.push({ type: metaKind, content: line });
      if (line === "GIT binary patch") inBinaryPayload = true;
      continue;
    }
    if (!inHunk && (line.startsWith("+++") || line.startsWith("---"))) {
      out.push({ type: "hdr", content: line });
      continue;
    }
    if (line.startsWith("+")) {
      out.push({ type: "add", content: line, newNo: newNo || undefined });
      if (newNo) newNo += 1;
    } else if (line.startsWith("-")) {
      out.push({ type: "del", content: line, oldNo: oldNo || undefined });
      if (oldNo) oldNo += 1;
    } else if (line.startsWith("\\")) {
      if (inHunk) {
        const prev = out[out.length - 1];
        if (prev && (prev.type === "add" || prev.type === "del" || prev.type === "ctx")) {
          prev.noNewline = true;
        }
      }
      out.push({ type: "hdr", content: line });
    } else if (inHunk) {
      out.push({
        type: "ctx",
        content: line,
        oldNo: oldNo || undefined,
        newNo: newNo || undefined,
      });
      if (oldNo) oldNo += 1;
      if (newNo) newNo += 1;
    } else {
      out.push({ type: "meta", content: line });
    }
  }
  return out;
}

export interface ParseCache {
  parse(input: string | null): AnnotatedDiffLine[];
}

export function createParseCache(): ParseCache {
  let lastInput: string | null = null;
  let lastOutput: AnnotatedDiffLine[] = [];
  return {
    parse(input: string | null): AnnotatedDiffLine[] {
      if (input === lastInput) return lastOutput;
      lastInput = input;
      lastOutput = parseUnifiedDiff(input ?? "");
      return lastOutput;
    },
  };
}

export interface ReplacementBlock {
  start: number;
  addStart: number;
  end: number;
}

export function replacementBlockBounds(
  lines: AnnotatedDiffLine[],
  index: number,
): [number, number] | null {
  const block = replacementBlock(lines, index);
  return block ? [block.start, block.end] : null;
}

export function replacementBlock(
  lines: AnnotatedDiffLine[],
  index: number,
): ReplacementBlock | null {
  const line = lines[index];
  if (!line || (line.type !== "del" && line.type !== "add")) return null;
  let start = index;
  if (line.type === "add") {
    while (start > 0 && lines[start - 1].type === "add") start -= 1;
  }
  while (start > 0 && lines[start - 1].type === "del") start -= 1;
  let addStart = start;
  while (addStart < lines.length && lines[addStart].type === "del") addStart += 1;
  let end = addStart;
  while (end < lines.length && lines[end].type === "add") end += 1;
  return { start, addStart, end };
}

export const WORD_DIFF_PAIR_HALO = 24;
export const WORD_DIFF_CELL_BUDGET = 1_000_000;

function pairCellCost(oldLen: number, newLen: number): number {
  return (Math.min(oldLen, MAX_TOKENS + 1) + 1) * (Math.min(newLen, MAX_TOKENS + 1) + 1);
}

export function annotateAround(
  lines: AnnotatedDiffLine[],
  index: number,
  block: ReplacementBlock | null = replacementBlock(lines, index),
  halo: number = WORD_DIFF_PAIR_HALO,
): void {
  if (!block) return;
  const pairCount = Math.min(block.addStart - block.start, block.end - block.addStart);
  const k = index < block.addStart ? index - block.start : index - block.addStart;
  if (k < 0 || k >= pairCount) return;
  let spent = 0;
  const annotatePair = (offset: number, force: boolean): void => {
    if (offset < 0 || offset >= pairCount) return;
    const delLine = lines[block.start + offset];
    const addLine = lines[block.addStart + offset];
    if (delLine.segments || addLine.segments) return;
    const cost = pairCellCost(delLine.content.length - 1, addLine.content.length - 1);
    if (!force && spent + cost > WORD_DIFF_CELL_BUDGET) return;
    spent += cost;
    const diff = computeWordDiff(delLine.content.slice(1), addLine.content.slice(1));
    delLine.segments = diff.original_segments;
    addLine.segments = diff.modified_segments;
  };
  annotatePair(k, true);
  for (let d = 1; d <= halo; d += 1) {
    annotatePair(k - d, false);
    annotatePair(k + d, false);
  }
}

export function annotateRange(
  lines: AnnotatedDiffLine[],
  start: number,
  end: number,
): AnnotatedDiffLine[] {
  const lo = Math.max(0, start);
  const hi = Math.min(lines.length, end);
  const slice = lines.slice(lo, hi);
  let i = 0;
  while (i < slice.length) {
    if (slice[i].type === "del") {
      const delStart = i;
      while (i < slice.length && slice[i].type === "del") i += 1;
      const addStart = i;
      while (i < slice.length && slice[i].type === "add") i += 1;
      const pairCount = Math.min(addStart - delStart, i - addStart);
      for (let k = 0; k < pairCount; k++) {
        const delLine = slice[delStart + k];
        const addLine = slice[addStart + k];
        if (!delLine.segments && !addLine.segments) {
          const diff = computeWordDiff(delLine.content.slice(1), addLine.content.slice(1));
          delLine.segments = diff.original_segments;
          addLine.segments = diff.modified_segments;
        }
      }
    } else {
      i += 1;
    }
  }
  return slice;
}

export function isImagePath(path: string | null | undefined): boolean {
  if (!path) return false;
  return /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(path);
}

export function annotateUnifiedDiff(raw: string): AnnotatedDiffLine[] {
  const parsed = parseUnifiedDiff(raw);
  return annotateRange(parsed, 0, parsed.length);
}

export function emptyDiffCopy(isMerge: boolean): { title: string; hint: string } {
  if (isMerge) {
    return {
      title: "Merge commit — no textual diff",
      hint: "Git omits the combined diff for merges that resolved cleanly. Open one of its parents or pick a changed file from the commit details.",
    };
  }
  return {
    title: "No diff selected",
    hint: "Select a changed file from the sidebar or a commit from the graph to view diffs.",
  };
}
