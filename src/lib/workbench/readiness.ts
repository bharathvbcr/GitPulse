/**
 * Task DAG readiness queries backed by `dc-store`'s `items.readiness`.
 * Fail-closed: any missing or truncated entry resolves to `{ ok: false }` so
 * wave scheduling never treats an unreadable readiness check as ready.
 */

import { WorkbenchError } from "./client";
import { hostRequest } from "./hostRequest";

const MAX_READINESS_BATCH = 100;

export interface LinkedTaskRef {
  id: string;
  title: string;
  status: string;
}

export type ReadinessReason = "ready" | "blocked_by_tasks" | "open_subtasks";

export interface TaskReadiness {
  id: string;
  ready: boolean;
  ready_with_subtasks: boolean;
  readiness_reason: ReadinessReason;
  blocked_by: LinkedTaskRef[];
  blocking: LinkedTaskRef[];
  open_subtasks: LinkedTaskRef[];
  open_subtask_count: number;
  total_subtask_count: number;
}

export type ReadinessCheck =
  | { ok: true; readiness: TaskReadiness }
  | { ok: false; error: string };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseLinkedRefs(raw: unknown): LinkedTaskRef[] {
  if (!Array.isArray(raw)) return [];
  const out: LinkedTaskRef[] = [];
  for (const entry of raw) {
    if (!isRecord(entry) || typeof entry.id !== "string") continue;
    out.push({
      id: entry.id,
      title: typeof entry.title === "string" ? entry.title : "",
      status: typeof entry.status === "string" ? entry.status : "",
    });
  }
  return out;
}

function parseReadiness(raw: unknown): TaskReadiness | null {
  if (!isRecord(raw) || typeof raw.id !== "string" || typeof raw.ready !== "boolean") {
    return null;
  }
  const reason: ReadinessReason =
    raw.readiness_reason === "blocked_by_tasks" || raw.readiness_reason === "open_subtasks"
      ? raw.readiness_reason
      : "ready";
  return {
    id: raw.id,
    ready: raw.ready,
    ready_with_subtasks: typeof raw.ready_with_subtasks === "boolean" ? raw.ready_with_subtasks : raw.ready,
    readiness_reason: reason,
    blocked_by: parseLinkedRefs(raw.blocked_by),
    blocking: parseLinkedRefs(raw.blocking),
    open_subtasks: parseLinkedRefs(raw.open_subtasks),
    open_subtask_count: typeof raw.open_subtask_count === "number" ? raw.open_subtask_count : 0,
    total_subtask_count: typeof raw.total_subtask_count === "number" ? raw.total_subtask_count : 0,
  };
}

export async function getTaskReadiness(id: string): Promise<TaskReadiness> {
  const response = await hostRequest("items.readiness", { id });
  const parsed = parseReadiness(response.item);
  if (!parsed) {
    throw new WorkbenchError("protocol_error", "items.readiness returned an invalid readiness record.");
  }
  return parsed;
}

export async function checkTasksReadiness(
  ids: readonly string[],
): Promise<ReadonlyMap<string, ReadinessCheck>> {
  const uniqueIds = [...new Set(ids.filter((id) => typeof id === "string" && id.length > 0))];
  const results = new Map<string, ReadinessCheck>();
  if (uniqueIds.length === 0) return results;

  for (let offset = 0; offset < uniqueIds.length; offset += MAX_READINESS_BATCH) {
    const chunk = uniqueIds.slice(offset, offset + MAX_READINESS_BATCH);
    const response = await hostRequest("items.readiness", { ids: chunk });
    if (!Array.isArray(response.items)) {
      throw new WorkbenchError("protocol_error", "items.readiness omitted the items array.");
    }
    const returned = new Map<string, TaskReadiness>();
    for (const entry of response.items) {
      const parsed = parseReadiness(entry);
      if (parsed) returned.set(parsed.id, parsed);
    }
    const truncated = response.truncated === true;
    for (const id of chunk) {
      const readiness = returned.get(id);
      if (readiness) {
        results.set(id, { ok: true, readiness });
      } else {
        results.set(id, {
          ok: false,
          error: truncated
            ? "readiness batch response was truncated"
            : "task readiness was not returned by the store",
        });
      }
    }
  }
  return results;
}
