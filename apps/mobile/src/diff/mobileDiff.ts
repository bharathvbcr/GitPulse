/**
 * Pure TypeScript flat-span diff helpers for virtualized FlashList `<Text>` rendering.
 * Mirrors GitPulse's `src/lib/diff/wordDiff.ts` / `@gitpulse/core` segment model so mobile
 * renders 120fps diffs without WebViews.
 */

export type DiffChunkKind = "Equal" | "Added" | "Removed";

export interface DiffSegment {
  readonly kind: DiffChunkKind;
  readonly text: string;
}

export interface IntraLineDiff {
  readonly original_segments: readonly DiffSegment[];
  readonly modified_segments: readonly DiffSegment[];
}

export interface AnnotatedDiffLine {
  readonly id: string;
  readonly type: "add" | "del" | "ctx" | "hdr" | "meta";
  readonly content: string;
  readonly oldNo?: number;
  readonly newNo?: number;
  readonly segments?: readonly DiffSegment[];
}

const MAX_TOKENS = 400;
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
    const isWord = isWordChar(line[i] ?? "");
    i += 1;
    while (i < line.length && isWordChar(line[i] ?? "") === isWord) {
      i += 1;
    }
    tokens.push(line.slice(start, i));
  }
  return tokens;
}

function mergeSegments(segments: DiffSegment[]): DiffSegment[] {
  const merged: DiffSegment[] = [];
  for (const seg of segments) {
    const prev = merged[merged.length - 1];
    if (prev && prev.kind === seg.kind) {
      merged[merged.length - 1] = { kind: prev.kind, text: prev.text + seg.text };
    } else {
      merged.push(seg);
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

  const a = tokenizeLine(oldLine, MAX_TOKENS);
  const b = tokenizeLine(newLine, MAX_TOKENS);
  const m = a.length;
  const n = b.length;
  const table: number[][] = Array.from({ length: m + 1 }, () => new Array<number>(n + 1).fill(0));

  for (let i = 1; i <= m; i++) {
    const row = table[i]!;
    const prevRow = table[i - 1]!;
    for (let j = 1; j <= n; j++) {
      row[j] =
        a[i - 1] === b[j - 1]
          ? (prevRow[j - 1] ?? 0) + 1
          : Math.max(prevRow[j] ?? 0, row[j - 1] ?? 0);
    }
  }

  let i = m;
  let j = n;
  const origRev: DiffSegment[] = [];
  const modRev: DiffSegment[] = [];

  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && a[i - 1] === b[j - 1]) {
      const tok = a[i - 1]!;
      origRev.push({ kind: "Equal", text: tok });
      modRev.push({ kind: "Equal", text: tok });
      i -= 1;
      j -= 1;
    } else if (j > 0 && (i === 0 || (table[i]?.[j - 1] ?? 0) >= (table[i - 1]?.[j] ?? 0))) {
      modRev.push({ kind: "Added", text: b[j - 1]! });
      j -= 1;
    } else if (i > 0) {
      origRev.push({ kind: "Removed", text: a[i - 1]! });
      i -= 1;
    }
  }

  origRev.reverse();
  modRev.reverse();
  return {
    original_segments: mergeSegments(origRev),
    modified_segments: mergeSegments(modRev),
  };
}

const HUNK_HEADER_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export function parseUnifiedDiffToFlatLines(
  filePath: string,
  unifiedDiff: string,
): AnnotatedDiffLine[] {
  const rawLines = unifiedDiff.split("\n");
  const parsed: AnnotatedDiffLine[] = [];
  let oldLineNo = 0;
  let newLineNo = 0;

  for (let idx = 0; idx < rawLines.length; idx++) {
    const line = rawLines[idx] ?? "";
    const id = `${filePath}:${idx}`;

    if (line.startsWith("@@")) {
      const match = HUNK_HEADER_RE.exec(line);
      if (match) {
        oldLineNo = Number.parseInt(match[1] ?? "1", 10);
        newLineNo = Number.parseInt(match[2] ?? "1", 10);
      }
      parsed.push({ id, type: "hdr", content: line });
      continue;
    }
    if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ")) {
      parsed.push({ id, type: "meta", content: line });
      continue;
    }
    if (line.startsWith("-")) {
      parsed.push({
        id,
        type: "del",
        content: line.slice(1),
        oldNo: oldLineNo,
      });
      oldLineNo += 1;
      continue;
    }
    if (line.startsWith("+")) {
      parsed.push({
        id,
        type: "add",
        content: line.slice(1),
        newNo: newLineNo,
      });
      newLineNo += 1;
      continue;
    }
    parsed.push({
      id,
      type: "ctx",
      content: line.startsWith(" ") ? line.slice(1) : line,
      oldNo: oldLineNo > 0 ? oldLineNo : undefined,
      newNo: newLineNo > 0 ? newLineNo : undefined,
    });
    if (oldLineNo > 0) oldLineNo += 1;
    if (newLineNo > 0) newLineNo += 1;
  }

  // Pair adjacent del/add lines for intra-line word diff highlights
  for (let i = 0; i < parsed.length - 1; i++) {
    const cur = parsed[i]!;
    const next = parsed[i + 1]!;
    if (cur.type === "del" && next.type === "add") {
      const intra = computeWordDiff(cur.content, next.content);
      parsed[i] = { ...cur, segments: intra.original_segments };
      parsed[i + 1] = { ...next, segments: intra.modified_segments };
      i += 1;
    }
  }

  return parsed;
}
