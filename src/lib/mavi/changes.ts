/**
 * What a thread changed, per file: the "agent + live file changes" view.
 *
 * Three sources, in the order a turn produces them:
 * 1. **Proposed** — an edit waiting for permission. Its hunk is built from
 *    the request's own input (`old_string` → `new_string`, or a Write's
 *    content), so the person sees the change before approving it.
 * 2. **Applied** — an edit the agent made with its own tools. Its exact hunks
 *    come from `structuredPatch` in the tool result, attributed to that call.
 * 3. **Other changes during this turn** — paths whose worktree state moved
 *    between the host's before/after fingerprints but that no edit tool
 *    names: a shell command, a formatter, a generator, or another program.
 *    The view never claims those were the agent's; it opens their patch
 *    against HEAD on request.
 */
import type { MaviHunk, ThreadState, ToolItem } from "./types";
import { isEditTool, shortPath, toolPath } from "./rows";

/**
 * `not_applied`: the call never ran (stopped, or its request was voided when
 * the turn or the agent ended). Distinct from `failed`, which ran and erred.
 */
export type ChangeState = "proposed" | "applying" | "applied" | "denied" | "failed" | "not_applied";

export interface ChangeEdit {
  readonly toolId: string;
  readonly toolName: string;
  readonly turn: number;
  readonly state: ChangeState;
  readonly hunks: readonly MaviHunk[];
  /** True when the hunks were built from the request input, not reported by the agent. */
  readonly fromInput: boolean;
  readonly requestId: string | null;
}

export interface FileChange {
  readonly path: string;
  readonly edits: readonly ChangeEdit[];
  readonly added: number;
  readonly removed: number;
  /** Named by an edit tool (true) or only by the worktree fingerprint (false). */
  readonly attributed: boolean;
  /** Porcelain code from the turn's fingerprint, when the path moved. */
  readonly code: string | null;
  readonly turns: readonly number[];
  readonly latestState: ChangeState | "moved";
}

const MAX_INPUT_LINES = 400;

function splitLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

function capLines(lines: string[], prefix: string): string[] {
  const out = lines.slice(0, MAX_INPUT_LINES).map((l) => prefix + l);
  if (lines.length > MAX_INPUT_LINES) out.push(` … ${lines.length - MAX_INPUT_LINES} more lines`);
  return out;
}

/** Hunks built from an edit request's input (no line numbers are known yet). */
export function hunksFromInput(tool: Pick<ToolItem, "name" | "input">): MaviHunk[] {
  const input = tool.input;
  const one = (oldText: string, newText: string): MaviHunk => {
    const oldLines = oldText ? splitLines(oldText) : [];
    const newLines = newText ? splitLines(newText) : [];
    // Lines the edit keeps at either end are context, not a removal and an
    // addition of the same text.
    let head = 0;
    while (head < oldLines.length && head < newLines.length && oldLines[head] === newLines[head]) head += 1;
    let tail = 0;
    while (
      tail < oldLines.length - head &&
      tail < newLines.length - head &&
      oldLines[oldLines.length - 1 - tail] === newLines[newLines.length - 1 - tail]
    ) tail += 1;
    const removed = oldLines.slice(head, oldLines.length - tail);
    const added = newLines.slice(head, newLines.length - tail);
    return {
      oldStart: 0,
      oldLines: oldLines.length,
      newStart: 0,
      newLines: newLines.length,
      lines: [
        ...capLines(oldLines.slice(0, head), " "),
        ...capLines(removed, "-"),
        ...capLines(added, "+"),
        ...capLines(oldLines.slice(oldLines.length - tail), " "),
      ],
    };
  };
  if (tool.name === "Edit" && typeof input.new_string === "string") {
    return [one(typeof input.old_string === "string" ? input.old_string : "", input.new_string)];
  }
  if (tool.name === "MultiEdit" && Array.isArray(input.edits)) {
    return input.edits
      .map((e) => (e && typeof e === "object" ? (e as Record<string, unknown>) : null))
      .filter((e): e is Record<string, unknown> => !!e && typeof e.new_string === "string")
      .map((e) => one(typeof e.old_string === "string" ? e.old_string : "", e.new_string as string));
  }
  if (tool.name === "Write" && typeof input.content === "string") {
    return [one("", input.content)];
  }
  if (tool.name === "NotebookEdit" && typeof input.new_source === "string") {
    return [one("", input.new_source)];
  }
  return [];
}

export function countLines(hunks: readonly MaviHunk[]): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const hunk of hunks) {
    for (const line of hunk.lines) {
      if (line.startsWith("+")) added += 1;
      else if (line.startsWith("-")) removed += 1;
    }
  }
  return { added, removed };
}

function editOf(tool: ToolItem): ChangeEdit | null {
  if (!isEditTool(tool.name)) return null;
  let state: ChangeState;
  if (tool.approval?.state === "open") state = "proposed";
  else if (tool.status === "denied" || tool.approval?.state === "denied") state = "denied";
  else if (tool.approval?.state === "void" || tool.status === "interrupted") state = "not_applied";
  else if (tool.status === "error") state = "failed";
  else if (tool.status === "ok") state = "applied";
  else state = "applying";
  const reported = tool.patch && tool.patch.length ? tool.patch : null;
  return {
    toolId: tool.id,
    toolName: tool.name,
    turn: tool.turn,
    state,
    hunks: reported ?? hunksFromInput(tool),
    fromInput: !reported,
    requestId: tool.approval?.requestId ?? null,
  };
}

export type ChangeScope = { readonly kind: "turn"; readonly turn: number } | { readonly kind: "thread" };

/**
 * Every file the scope changed, attributed edits first (by first touch), then
 * other moved paths (alphabetical). Denied and failed edits are listed so a
 * refused change is visible, but they do not count toward +/−.
 */
export function collectChanges(state: ThreadState, root: string | null, scope: ChangeScope): FileChange[] {
  const inScope = (turn: number) => scope.kind === "thread" || turn === scope.turn;
  const byPath = new Map<string, { edits: ChangeEdit[]; turns: Set<number> }>();
  for (const item of state.items) {
    if (item.kind !== "tool" || !inScope(item.turn)) continue;
    const edit = editOf(item);
    const path = toolPath(item);
    if (!edit || !path) continue;
    const key = shortPath(path, root);
    const entry = byPath.get(key) ?? { edits: [], turns: new Set<number>() };
    entry.edits.push(edit);
    entry.turns.add(item.turn);
    byPath.set(key, entry);
  }
  const out: FileChange[] = [];
  const codes = new Map<string, string>();
  for (const [turnText, files] of Object.entries(state.turnFiles)) {
    if (!inScope(Number(turnText))) continue;
    for (const moved of files.moved) codes.set(moved.path, moved.code);
  }
  for (const [path, entry] of byPath) {
    const counted = entry.edits.filter((e) => e.state !== "denied" && e.state !== "failed" && e.state !== "not_applied");
    const { added, removed } = countLines(counted.flatMap((e) => e.hunks));
    out.push({
      path,
      edits: entry.edits,
      added,
      removed,
      attributed: true,
      code: codes.get(path) ?? null,
      turns: [...entry.turns].sort((a, b) => a - b),
      latestState: entry.edits[entry.edits.length - 1]!.state,
    });
  }
  const others: FileChange[] = [];
  for (const [turnText, files] of Object.entries(state.turnFiles)) {
    const turn = Number(turnText);
    if (!inScope(turn)) continue;
    for (const moved of files.moved) {
      if (byPath.has(moved.path) || others.some((o) => o.path === moved.path)) continue;
      others.push({
        path: moved.path,
        edits: [],
        added: 0,
        removed: 0,
        attributed: false,
        code: moved.code,
        turns: [turn],
        latestState: "moved",
      });
    }
  }
  others.sort((a, b) => a.path.localeCompare(b.path));
  return [...out, ...others];
}

/** The turns that changed anything, newest first. */
export function changedTurns(state: ThreadState): number[] {
  const turns = new Set<number>();
  for (const item of state.items) {
    if (item.kind === "tool" && isEditTool(item.name)) turns.add(item.turn);
  }
  for (const [turn, files] of Object.entries(state.turnFiles)) {
    if (files.moved.length) turns.add(Number(turn));
  }
  return [...turns].sort((a, b) => b - a);
}

/** Totals for a turn's footer chip. */
export function turnTotals(state: ThreadState, root: string | null, turn: number): { files: number; added: number; removed: number } {
  const changes = collectChanges(state, root, { kind: "turn", turn });
  return {
    files: changes.length,
    added: changes.reduce((n, c) => n + c.added, 0),
    removed: changes.reduce((n, c) => n + c.removed, 0),
  };
}

/** A porcelain code as a word. */
export function describeCode(code: string | null): string {
  if (!code) return "";
  const c = code.trim();
  if (c === "") return "Back to HEAD";
  if (c === "??") return "New";
  if (c.includes("D")) return "Deleted";
  if (c.includes("A")) return "Added";
  if (c.includes("R")) return "Renamed";
  if (c.includes("U")) return "Conflict";
  return "Modified";
}

/** Hunks of a unified patch for one file (the host's `git diff` output). */
export function parsePatch(patch: string): { hunks: MaviHunk[]; binary: boolean } {
  const hunks: MaviHunk[] = [];
  let current: { oldStart: number; oldLines: number; newStart: number; newLines: number; lines: string[] } | null = null;
  let binary = false;
  for (const line of patch.split("\n")) {
    const header = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    if (header) {
      current = {
        oldStart: Number(header[1]),
        oldLines: header[2] === undefined ? 1 : Number(header[2]),
        newStart: Number(header[3]),
        newLines: header[4] === undefined ? 1 : Number(header[4]),
        lines: [],
      };
      hunks.push(current);
      continue;
    }
    if (/^Binary files /.test(line)) binary = true;
    if (!current) continue;
    if (line.startsWith("+") || line.startsWith("-") || line.startsWith(" ")) current.lines.push(line);
    else if (line.startsWith("\\")) continue;
  }
  return { hunks, binary };
}
