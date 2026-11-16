/**
 * Typed frontend client for the host-coordinated cross-repository execution
 * and integration queue (`queue.*`).
 */

import { WorkbenchError, newID } from "./client";
import { hostRequest } from "./hostRequest";

export type QueueState =
  | "queued"
  | "leased"
  | "verifying"
  | "passed"
  | "failed"
  | "cancelled"
  | "timed_out";

export type QueueMemberState =
  | "queued"
  | "leased"
  | "running"
  | "verifying"
  | "passed"
  | "failed"
  | "blocked"
  | "cancelled"
  | "timed_out";

export interface QueueMemberInput {
  member_id: string;
  repository_id: string;
  repo_path: string;
  task_id?: string;
  depends_on?: readonly string[];
}

export interface MemberVerification {
  member_id: string;
  repository_id: string;
  ok: boolean;
  head_oid: string | null;
  command: string | null;
  exit_code: number | null;
  summary: string;
  verified_at: number;
}

export interface QueueMember {
  member_id: string;
  repository_id: string;
  git_dir: string;
  base_oid: string;
  task_id: string | null;
  run_id: string | null;
  position: number;
  state: QueueMemberState;
  lease_held: boolean;
  depends_on: string[];
  started_at: number | null;
  active_seconds: number;
  last_active_at: number | null;
  verification: MemberVerification | null;
}

export interface CombinedVerification {
  verified_at: number;
  ok: boolean;
  publication: "manual_per_repository";
  results: MemberVerification[];
}

export interface WorkQueueEntry {
  id: string;
  revision: number;
  title: string;
  state: QueueState;
  concurrency_limit: number;
  active_timeout_seconds: number;
  elapsed_timeout_seconds: number;
  lease_order: string[];
  leased_at: number | null;
  finished_at: number | null;
  publication: "manual_per_repository";
  reason: string;
  verification: CombinedVerification | null;
  created_at: number;
  updated_at: number;
  members: QueueMember[];
}

export interface WorkQueuePage {
  items: WorkQueueEntry[];
  total: number;
  has_more: boolean;
  next_cursor: string | null;
}

function expectItem(response: Record<string, unknown>, method: string): WorkQueueEntry {
  const item = response.item;
  if (typeof item !== "object" || item === null || Array.isArray(item)) {
    throw new WorkbenchError("protocol_error", `${method} omitted the queue entry.`);
  }
  return item as WorkQueueEntry;
}

export async function enqueueWorkQueue(input: {
  id?: string;
  request_id?: string;
  expected_revision?: number;
  title: string;
  concurrency_limit?: number;
  active_timeout_seconds?: number;
  elapsed_timeout_seconds?: number;
  members: readonly QueueMemberInput[];
}): Promise<WorkQueueEntry> {
  const payload: Record<string, unknown> = {
    id: input.id ?? newID(),
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision ?? 0,
    title: input.title,
    members: input.members.map((m) => ({
      member_id: m.member_id,
      repository_id: m.repository_id,
      repo_path: m.repo_path,
      ...(m.task_id !== undefined ? { task_id: m.task_id } : {}),
      ...(m.depends_on !== undefined ? { depends_on: [...m.depends_on] } : {}),
    })),
  };
  if (input.concurrency_limit !== undefined) payload.concurrency_limit = input.concurrency_limit;
  if (input.active_timeout_seconds !== undefined) {
    payload.active_timeout_seconds = input.active_timeout_seconds;
  }
  if (input.elapsed_timeout_seconds !== undefined) {
    payload.elapsed_timeout_seconds = input.elapsed_timeout_seconds;
  }
  return expectItem(await hostRequest("queue.enqueue_host", payload), "queue.enqueue_host");
}

export async function acquireWorkQueue(input: {
  id: string;
  request_id?: string;
  expected_revision: number;
}): Promise<WorkQueueEntry> {
  return expectItem(
    await hostRequest("queue.acquire", {
      id: input.id,
      request_id: input.request_id ?? newID(),
      expected_revision: input.expected_revision,
    }),
    "queue.acquire",
  );
}

export async function getWorkQueue(id: string): Promise<WorkQueueEntry> {
  return expectItem(await hostRequest("queue.get", { id }), "queue.get");
}

export async function listWorkQueue(filter: {
  limit?: number;
  cursor?: string | null;
  state?: QueueState;
} = {}): Promise<WorkQueuePage> {
  const payload: Record<string, unknown> = {};
  if (filter.limit !== undefined) payload.limit = filter.limit;
  if (filter.cursor !== undefined && filter.cursor !== null) payload.cursor = filter.cursor;
  if (filter.state !== undefined) payload.state = filter.state;
  const response = await hostRequest("queue.list", payload);
  if (!Array.isArray(response.items)) {
    throw new WorkbenchError("protocol_error", "queue.list omitted the items array.");
  }
  return {
    items: response.items as WorkQueueEntry[],
    total: typeof response.total === "number" ? response.total : response.items.length,
    has_more: response.has_more === true,
    next_cursor: typeof response.next_cursor === "string" ? response.next_cursor : null,
  };
}

export async function transitionQueueMember(input: {
  id: string;
  request_id?: string;
  expected_revision: number;
  member_id: string;
  state: "running" | "verifying" | "passed" | "failed";
  run_id?: string;
}): Promise<WorkQueueEntry> {
  const payload: Record<string, unknown> = {
    id: input.id,
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision,
    member_id: input.member_id,
    state: input.state,
  };
  if (input.run_id !== undefined) payload.run_id = input.run_id;
  return expectItem(
    await hostRequest("queue.member.update", payload),
    "queue.member.update",
  );
}

export async function verifyWorkQueueCombined(input: {
  id: string;
  request_id?: string;
  expected_revision: number;
  checkouts?: Record<string, string>;
  results?: readonly {
    member_id: string;
    ok: boolean;
    head_oid?: string;
    command?: string;
    exit_code?: number;
    summary?: string;
  }[];
}): Promise<WorkQueueEntry> {
  const payload: Record<string, unknown> = {
    id: input.id,
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision,
    publication: "manual_per_repository",
  };
  if (input.checkouts !== undefined) payload.checkouts = input.checkouts;
  if (input.results !== undefined) payload.results = input.results.map((r) => ({ ...r }));
  return expectItem(
    await hostRequest("queue.verify_combined", payload),
    "queue.verify_combined",
  );
}

export async function cancelWorkQueue(input: {
  id: string;
  request_id?: string;
  expected_revision: number;
  reason?: string;
}): Promise<WorkQueueEntry> {
  const payload: Record<string, unknown> = {
    id: input.id,
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision,
  };
  if (input.reason !== undefined) payload.reason = input.reason;
  return expectItem(await hostRequest("queue.cancel", payload), "queue.cancel");
}

export async function sweepWorkQueue(input: {
  id: string;
  request_id?: string;
  expected_revision: number;
}): Promise<WorkQueueEntry> {
  return expectItem(
    await hostRequest("queue.sweep", {
      id: input.id,
      request_id: input.request_id ?? newID(),
      expected_revision: input.expected_revision,
    }),
    "queue.sweep",
  );
}
