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

function isWordChar(ch: string): boolean {
  return /[A-Za-z0-9_]/.test(ch);
}

function computeLcsTable(a: string[], b: string[]): number[][] {
  const m = a.length;
  const n = b.length;
  const table: number[][] = Array.from({ length: m + 1 }, () => Array(n + 1).fill(0));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      table[i][j] = a[i - 1] === b[j - 1] ? table[i - 1][j - 1] + 1 : Math.max(table[i - 1][j], table[i][j - 1]);
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
  type: "add" | "del" | "ctx" | "hdr";
  content: string;
  /** Line number in the old file, when the hunk header knows it. */
  oldNo?: number;
  /** Line number in the new file, when the hunk header knows it. */
  newNo?: number;
  segments?: DiffSegment[];
}

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

/**
 * Parses a unified diff into light rows without any intra-line work.
 *
 * Word-diffing every changed pair up front is quadratic work on an agent's
 * massive commits; the renderer calls [`annotateRange`] for just the visible
 * window instead. Hunk headers are tracked here, so line numbers come from
 * git rather than being faked with row indices.
 */
export function parseUnifiedDiff(raw: string): AnnotatedDiffLine[] {
  const out: AnnotatedDiffLine[] = [];
  let oldNo = 0;
  let newNo = 0;
  for (const line of (raw || "").split("\n")) {
    if (line.startsWith("@@")) {
      const match = HUNK_RE.exec(line);
      if (match) {
        oldNo = parseInt(match[1], 10);
        newNo = parseInt(match[2], 10);
      }
      out.push({ type: "hdr", content: line });
      continue;
    }
    if (line.startsWith("+++") || line.startsWith("---")) {
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
      // "\ No newline at end of file"
      out.push({ type: "hdr", content: line });
    } else {
      out.push({
        type: "ctx",
        content: line,
        oldNo: oldNo || undefined,
        newNo: newNo || undefined,
      });
      if (oldNo) oldNo += 1;
      if (newNo) newNo += 1;
    }
  }
  return out;
}

/**
 * Annotates one window of parsed lines with word-diff segments.
 *
 * Only pairs fully inside `[start, end)` are computed, so the cost scales
 * with what is on screen rather than with the size of the diff. Returns the
 * same array object when the range holds no adjacent del/add pairs.
 */
export function annotateRange(
  lines: AnnotatedDiffLine[],
  start: number,
  end: number
): AnnotatedDiffLine[] {
  const lo = Math.max(0, start);
  const hi = Math.min(lines.length, end);
  const slice = lines.slice(lo, hi);
  for (let i = 0; i < slice.length - 1; i++) {
    const cur = slice[i];
    const next = slice[i + 1];
    if (cur.type === "del" && next.type === "add" && !cur.segments && !next.segments) {
      const diff = computeWordDiff(cur.content.slice(1), next.content.slice(1));
      cur.segments = diff.original_segments;
      next.segments = diff.modified_segments;
      i += 1;
    }
  }
  return slice;
}

export function isImagePath(path: string | null | undefined): boolean {
  if (!path) return false;
  return /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(path);
}

/**
 * Annotates an entire diff in one pass. Only for small diffs: large ones must
 * go through `parseUnifiedDiff` plus per-window `annotateRange` calls.
 */
export function annotateUnifiedDiff(raw: string): AnnotatedDiffLine[] {
  const parsed = parseUnifiedDiff(raw);
  return annotateRange(parsed, 0, parsed.length);
}
