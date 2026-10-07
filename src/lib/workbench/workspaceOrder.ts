/**
 * The order of workspaces in the task navigator, and moving one within it.
 *
 * The store sorts workspaces by `position`; the navigator draws pinned ones
 * first, then by position. Both rules live here so the drawn order and the
 * order a move is computed against cannot disagree.
 *
 * A move is one write when there is room between the new neighbours, which
 * is the usual case: new workspaces take `Date.now()` as their position, so
 * the gaps are wide. Only when two neighbours sit on adjacent integers is the
 * whole group re-spaced, one revision-checked write per workspace — the store
 * has no batch write, so that is not atomic, and the result names what did
 * not move rather than calling a half-done order done.
 */
import { insertionPosition } from "./boardDrag";
import { explainError, getWorkspace, newID, putWorkspace, workspaceDraft, WorkbenchError, type Workspace, type WorkspaceCard } from "./client";

type Ordered = Pick<WorkspaceCard, "id" | "position" | "pinned">;

/** Pinned first, then by position, then by id — the navigator's order. */
export function navigatorOrder<T extends Ordered>(groups: readonly T[]): T[] {
  return [...groups].sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.position - b.position || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

export interface PositionWrite { id: string; revision: number; position: number }

/** Spacing used when a group has to be re-numbered. */
export const WORKSPACE_SPACING = 1_048_576;

/**
 * The position writes that move `id` one place up (-1) or down (+1).
 *
 * A move never crosses between pinned and unpinned workspaces: pinning is
 * what decides that boundary, and a reorder that silently changed it would
 * be a second meaning for one gesture. Null when there is nowhere to go.
 */
export function moveWrites(groups: readonly (Ordered & { revision: number })[], id: string, delta: -1 | 1): PositionWrite[] | null {
  const ordered = navigatorOrder(groups);
  const self = ordered.find((group) => group.id === id);
  if (!self) return null;
  const siblings = ordered.filter((group) => group.pinned === self.pinned);
  const from = siblings.indexOf(self);
  const to = from + delta;
  if (to < 0 || to >= siblings.length) return null;
  const rest = siblings.filter((group) => group.id !== id);
  const before = rest[to - 1] ?? null;
  const after = rest[to] ?? null;
  const position = insertionPosition(before?.position ?? null, after?.position ?? null);
  const fits = (before === null || position > before.position) && (after === null || position < after.position) && position >= 0;
  if (fits) return [{ id, revision: self.revision, position }];
  rest.splice(to, 0, self);
  return rest
    .map((group, index) => ({ id: group.id, revision: group.revision, position: (index + 1) * WORKSPACE_SPACING }))
    .filter((write) => write.position !== groups.find((group) => group.id === write.id)?.position);
}

export interface MoveIO { get: (id: string) => Promise<Workspace>; put: (input: Record<string, unknown>) => Promise<Workspace> }
const defaultIO: MoveIO = { get: getWorkspace, put: putWorkspace };

export interface MoveResult { written: string[]; failed: { id: string; error: string }[] }

/**
 * Write a move. Each workspace is read first and written against the
 * revision the navigator showed, so a workspace edited elsewhere in the
 * meantime is refused instead of having its edit replaced.
 */
export async function applyMove(writes: readonly PositionWrite[], io: MoveIO = defaultIO): Promise<MoveResult> {
  const result: MoveResult = { written: [], failed: [] };
  for (const write of writes) {
    try {
      const full = await io.get(write.id);
      if (full.id !== write.id || full.revision !== write.revision) throw new WorkbenchError("revision_conflict", "This workspace changed elsewhere. Refresh and move it again.");
      const saved = await io.put({ ...workspaceDraft(full), id: full.id, expected_revision: full.revision, request_id: newID(), position: write.position });
      if (saved.id !== write.id || saved.position !== write.position) throw new WorkbenchError("protocol_error", "Workspace move confirmation does not match the request.");
      result.written.push(write.id);
    } catch (cause) {
      result.failed.push({ id: write.id, error: explainError(cause) });
    }
  }
  return result;
}
