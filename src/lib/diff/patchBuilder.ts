import type { AnnotatedDiffLine } from "./wordDiff";

export type DiffLineType = "Context" | "Addition" | "Deletion";

export interface UnifiedDiffLine {
  line_type: DiffLineType;
  old_line_no?: number;
  new_line_no?: number;
  content: string;
  is_selected: boolean;
}

export interface UnifiedDiffHunk {
  old_start: number;
  old_lines: number;
  new_start: number;
  new_lines: number;
  header: string;
  lines: UnifiedDiffLine[];
}

export interface FilePatch {
  old_path: string;
  new_path: string;
  hunks: UnifiedDiffHunk[];
}

const HUNK_HEADER_RE = /^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@/;

/**
 * Parses a raw `@@ -X,Y +A,B @@` header string to extract starting numbers and line counts.
 */
export function parseHunkHeaderNumbers(header: string): {
  old_start: number;
  old_lines: number;
  new_start: number;
  new_lines: number;
} {
  const match = HUNK_HEADER_RE.exec(header);
  if (!match) {
    return { old_start: 1, old_lines: 0, new_start: 1, new_lines: 0 };
  }
  return {
    old_start: parseInt(match[1], 10),
    old_lines: match[2] !== undefined ? parseInt(match[2], 10) : 1,
    new_start: parseInt(match[3], 10),
    new_lines: match[4] !== undefined ? parseInt(match[4], 10) : 1,
  };
}

/**
 * Builds a `FilePatch` targeting specific line indices.
 */
export function buildFilePatchFromLines(
  lines: AnnotatedDiffLine[],
  filePath: string,
  selectedLineIndices: Set<number>
): FilePatch | null {
  const cleanPath = filePath.replace(/^[ab]\//, "");
  const hunks: UnifiedDiffHunk[] = [];

  let currentHunk: UnifiedDiffHunk | null = null;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    if (line.type === "hdr" && line.content.startsWith("@@")) {
      if (currentHunk && currentHunk.lines.length > 0) {
        hunks.push(currentHunk);
      }
      const nums = parseHunkHeaderNumbers(line.content);
      currentHunk = {
        old_start: nums.old_start,
        old_lines: nums.old_lines,
        new_start: nums.new_start,
        new_lines: nums.new_lines,
        header: line.content,
        lines: [],
      };
      continue;
    }

    if (!currentHunk) {
      continue;
    }

    if (line.type === "ctx" || line.type === "add" || line.type === "del") {
      let lineType: DiffLineType = "Context";
      if (line.type === "add") lineType = "Addition";
      else if (line.type === "del") lineType = "Deletion";

      // Strip leading +, -, or space for the backend payload
      const lineContent = line.content.length > 0 ? line.content.slice(1) : "";
      const isSelected = selectedLineIndices.has(i);

      currentHunk.lines.push({
        line_type: lineType,
        old_line_no: line.oldNo,
        new_line_no: line.newNo,
        content: lineContent,
        is_selected: isSelected,
      });
    }
  }

  if (currentHunk && currentHunk.lines.length > 0) {
    hunks.push(currentHunk);
  }

  const hasAnySelected = hunks.some((h) =>
    h.lines.some(
      (l) => l.is_selected && (l.line_type === "Addition" || l.line_type === "Deletion")
    )
  );

  if (!hasAnySelected) {
    return null;
  }

  return {
    old_path: cleanPath,
    new_path: cleanPath,
    hunks,
  };
}

/**
 * Builds a `FilePatch` selecting all addition/deletion lines within a specific hunk.
 */
export function buildFilePatchForHunk(
  lines: AnnotatedDiffLine[],
  filePath: string,
  hunkHeaderIndex: number
): FilePatch | null {
  const selectedIndices = new Set<number>();
  let inTargetHunk = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (i === hunkHeaderIndex) {
      inTargetHunk = true;
      continue;
    }
    if (line.type === "hdr" && line.content.startsWith("@@")) {
      if (inTargetHunk) break;
    }
    if (inTargetHunk && (line.type === "add" || line.type === "del")) {
      selectedIndices.add(i);
    }
  }

  return buildFilePatchFromLines(lines, filePath, selectedIndices);
}
