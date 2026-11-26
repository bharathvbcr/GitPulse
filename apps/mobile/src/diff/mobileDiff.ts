/**
 * Pure TypeScript flat-span diff helpers for virtualized FlashList `<Text>` rendering.
 * Consumes `@waylode/core/diff` (`computeWordDiff` and segment types) directly so mobile
 * and desktop share one intra-line diff engine while rendering 120fps native diffs without WebViews.
 */

import {
  computeWordDiff,
  type DiffChunkKind,
  type DiffSegment,
  type IntraLineDiff,
} from "@waylode/core/diff";

export { computeWordDiff, type DiffChunkKind, type DiffSegment, type IntraLineDiff };

export interface AnnotatedDiffLine {
  readonly id: string;
  readonly type: "add" | "del" | "ctx" | "hdr" | "meta";
  readonly content: string;
  readonly oldNo?: number;
  readonly newNo?: number;
  readonly segments?: readonly DiffSegment[];
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
