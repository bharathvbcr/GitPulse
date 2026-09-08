import { computeWordDiff, type DiffSegment } from "./wordDiff";

export interface ConflictComparisonRow { ours: DiffSegment[]; theirs: DiffSegment[] }
/** Highlight small blocks. Large blocks retain their complete text in a plain
 * view, avoiding a quadratic word diff or one DOM node per token/line. */
export function conflictComparisonRows(ours: string, theirs: string): ConflictComparisonRow[] | null {
  if (ours.length + theirs.length > 40_000) return null;
  const left = ours.split("\n"); const right = theirs.split("\n");
  if (Math.max(left.length, right.length) > 200) return null;
  return Array.from({ length: Math.max(left.length, right.length) }, (_, index) => {
    const oldLine = left[index] ?? ""; const newLine = right[index] ?? "";
    const diff = oldLine.length + newLine.length <= 2000
      ? computeWordDiff(oldLine, newLine)
      : { original_segments: [{ kind: "Removed" as const, text: oldLine }], modified_segments: [{ kind: "Added" as const, text: newLine }] };
    return { ours: diff.original_segments, theirs: diff.modified_segments };
  });
}
