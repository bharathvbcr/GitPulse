/**
 * Typed frontend client for two-way GitHub issue synchronization (`github_sync.*`)
 * and the opt-in, explicitly confirmed durable outbox (`outbox.*`).
 */

import { WorkbenchError, newID } from "./client";
import { hostRequest } from "./hostRequest";

export type SyncField = "title" | "body" | "state" | "labels";

export interface IssueSnapshot {
  title: string;
  body: string;
  state: "open" | "closed";
  labels: string[];
  updated_at?: string | null;
}

export interface InboundProposal {
  observed_at: number;
  remote: IssueSnapshot;
  remote_digest: string;
  changed_fields: SyncField[];
  locked_skipped: SyncField[];
  applicable_fields: SyncField[];
  conflicting_fields: SyncField[];
  conflicts: Record<string, unknown>;
}

export interface IssueSyncLink {
  id: string;
  revision: number;
  task_id: string;
  repo_slug: string;
  issue_number: number;
  issue_url: string | null;
  outbound_opt_in: boolean;
  last_seen: IssueSnapshot | null;
  pending_proposal: InboundProposal | null;
  last_dismissed_digest: string | null;
  created_at: number;
  updated_at: number;
}

export interface IssueSyncPage {
  items: IssueSyncLink[];
  total: number;
  has_more: boolean;
  next_cursor: string | null;
}

export type OutboxOp = "create_issue" | "update_issue";

export type OutboxState =
  | "pending"
  | "delivering"
  | "delivered"
  | "outcome_unknown"
  | "conflict"
  | "failed"
  | "cancelled";

export interface OutboxConflict {
  detected_at: number;
  fields: SyncField[];
  details: Record<string, unknown>;
  remote: IssueSnapshot;
}

export interface OutboxPayload {
  op: OutboxOp;
  task_id: string;
  repo_slug: string;
  issue_number: number | null;
  fields: SyncField[];
  title: string;
  body: string;
  state: "open" | "closed";
  labels: string[];
}

export interface OutboxPreview {
  sync_id: string | null;
  op: OutboxOp;
  repo_slug: string;
  issue_number: number | null;
  outbound_opt_in: boolean;
  can_enqueue: boolean;
  payload: OutboxPayload;
  payload_digest: string;
  conflicts: Record<string, unknown>;
}

export interface OutboxEntry {
  id: string;
  revision: number;
  sync_id: string | null;
  task_id: string;
  op: OutboxOp;
  repo_slug: string;
  issue_number: number | null;
  state: OutboxState;
  confirmed: boolean;
  outbound_opt_in: boolean;
  idempotency_key: string;
  payload_digest: string;
  payload: OutboxPayload;
  attempts: number;
  max_attempts: number;
  next_attempt_at: number;
  last_preflight_at: number | null;
  delivered_at: number | null;
  last_error: string | null;
  conflict: OutboxConflict | null;
  created_at: number;
  updated_at: number;
}

export interface OutboxPage {
  items: OutboxEntry[];
  total: number;
  has_more: boolean;
  next_cursor: string | null;
}

function expectObject<T>(value: unknown, label: string): T {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new WorkbenchError("protocol_error", `${label} returned an invalid payload.`);
  }
  return value as T;
}

export async function linkGitHubIssue(input: {
  id?: string;
  request_id?: string;
  expected_revision?: number;
  task_id: string;
  repo_slug: string;
  issue_number: number;
  issue_url?: string;
  outbound_opt_in?: boolean;
  last_seen?: IssueSnapshot;
}): Promise<IssueSyncLink> {
  const payload: Record<string, unknown> = {
    id: input.id ?? newID(),
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision ?? 0,
    task_id: input.task_id,
    repo_slug: input.repo_slug,
    issue_number: input.issue_number,
  };
  if (input.issue_url !== undefined) payload.issue_url = input.issue_url;
  if (input.outbound_opt_in !== undefined) payload.outbound_opt_in = input.outbound_opt_in;
  if (input.last_seen !== undefined) payload.last_seen = input.last_seen;
  const response = await hostRequest("github_sync.link", payload);
  return expectObject<IssueSyncLink>(response.item, "github_sync.link");
}

export async function getGitHubIssueSync(
  query: { id: string } | { task_id: string } | { repo_slug: string; issue_number: number },
): Promise<IssueSyncLink> {
  const response = await hostRequest("github_sync.get", { ...query });
  return expectObject<IssueSyncLink>(response.item, "github_sync.get");
}

export async function listGitHubIssueSync(filter: {
  limit?: number;
  cursor?: string | null;
  only_pending?: boolean;
  task_id?: string;
} = {}): Promise<IssueSyncPage> {
  const payload: Record<string, unknown> = {};
  if (filter.limit !== undefined) payload.limit = filter.limit;
  if (filter.cursor !== undefined && filter.cursor !== null) payload.cursor = filter.cursor;
  if (filter.only_pending !== undefined) payload.only_pending = filter.only_pending;
  if (filter.task_id !== undefined) payload.task_id = filter.task_id;
  const response = await hostRequest("github_sync.list", payload);
  if (!Array.isArray(response.items)) {
    throw new WorkbenchError("protocol_error", "github_sync.list omitted the items array.");
  }
  return {
    items: response.items as IssueSyncLink[],
    total: typeof response.total === "number" ? response.total : response.items.length,
    has_more: response.has_more === true,
    next_cursor: typeof response.next_cursor === "string" ? response.next_cursor : null,
  };
}

export async function pollGitHubIssue(
  input: ({ id: string } | { task_id: string } | { repo_slug: string; issue_number: number }) & {
    repo_path: string;
    request_id?: string;
  },
): Promise<IssueSyncLink> {
  const response = await hostRequest("github_sync.poll", {
    ...input,
    request_id: input.request_id ?? newID(),
  });
  return expectObject<IssueSyncLink>(response.item, "github_sync.poll");
}

export async function acceptGitHubProposal(input: {
  id: string;
  request_id?: string;
  expected_revision: number;
  fields?: readonly SyncField[];
  expected_task_revision?: number;
}): Promise<IssueSyncLink> {
  const payload: Record<string, unknown> = {
    id: input.id,
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision,
  };
  if (input.fields !== undefined) payload.fields = [...input.fields];
  if (input.expected_task_revision !== undefined) {
    payload.expected_task_revision = input.expected_task_revision;
  }
  const response = await hostRequest("github_sync.accept", payload);
  return expectObject<IssueSyncLink>(response.item, "github_sync.accept");
}

export async function dismissGitHubProposal(input: {
  id: string;
  request_id?: string;
  expected_revision: number;
}): Promise<IssueSyncLink> {
  const response = await hostRequest("github_sync.dismiss", {
    id: input.id,
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision,
  });
  return expectObject<IssueSyncLink>(response.item, "github_sync.dismiss");
}

export async function previewOutboxWrite(input: {
  task_id: string;
  op: OutboxOp;
  repo_slug?: string;
  fields?: readonly SyncField[];
  remote?: IssueSnapshot;
}): Promise<OutboxPreview> {
  const payload: Record<string, unknown> = {
    task_id: input.task_id,
    op: input.op,
  };
  if (input.repo_slug !== undefined) payload.repo_slug = input.repo_slug;
  if (input.fields !== undefined) payload.fields = [...input.fields];
  if (input.remote !== undefined) payload.remote = input.remote;
  const response = await hostRequest("outbox.preview", payload);
  return expectObject<OutboxPreview>(response.preview, "outbox.preview");
}

export async function enqueueOutboxWrite(input: {
  id?: string;
  request_id?: string;
  expected_revision?: number;
  task_id: string;
  op: OutboxOp;
  repo_slug?: string;
  fields?: readonly SyncField[];
  outbound_opt_in: boolean;
  confirmed: boolean;
  confirm_payload_digest: string;
  idempotency_key: string;
  max_attempts?: number;
}): Promise<OutboxEntry> {
  if (input.outbound_opt_in !== true) {
    throw new WorkbenchError(
      "outbound_disabled",
      "Outbound GitHub writes require explicit opt-in.",
    );
  }
  if (input.confirmed !== true || !input.confirm_payload_digest.trim()) {
    throw new WorkbenchError(
      "confirmation_required",
      "Outbound GitHub writes require explicit confirmation and a previewed payload digest.",
    );
  }
  const payload: Record<string, unknown> = {
    id: input.id ?? newID(),
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision ?? 0,
    task_id: input.task_id,
    op: input.op,
    outbound_opt_in: true,
    confirmed: true,
    confirm_payload_digest: input.confirm_payload_digest,
    idempotency_key: input.idempotency_key,
  };
  if (input.repo_slug !== undefined) payload.repo_slug = input.repo_slug;
  if (input.fields !== undefined) payload.fields = [...input.fields];
  if (input.max_attempts !== undefined) payload.max_attempts = input.max_attempts;
  const response = await hostRequest("outbox.enqueue", payload);
  return expectObject<OutboxEntry>(response.item, "outbox.enqueue");
}

export async function deliverOutboxEntry(input: {
  id: string;
  repo_path: string;
  request_id?: string;
}): Promise<OutboxEntry> {
  const response = await hostRequest("outbox.deliver", {
    id: input.id,
    repo_path: input.repo_path,
    request_id: input.request_id ?? newID(),
  });
  return expectObject<OutboxEntry>(response.item, "outbox.deliver");
}

export async function getOutboxEntry(id: string): Promise<OutboxEntry> {
  const response = await hostRequest("outbox.get", { id });
  return expectObject<OutboxEntry>(response.item, "outbox.get");
}

export async function listOutboxEntries(filter: {
  limit?: number;
  cursor?: string | null;
  state?: OutboxState;
  task_id?: string;
  due_only?: boolean;
} = {}): Promise<OutboxPage> {
  const payload: Record<string, unknown> = {};
  if (filter.limit !== undefined) payload.limit = filter.limit;
  if (filter.cursor !== undefined && filter.cursor !== null) payload.cursor = filter.cursor;
  if (filter.state !== undefined) payload.state = filter.state;
  if (filter.task_id !== undefined) payload.task_id = filter.task_id;
  if (filter.due_only !== undefined) payload.due_only = filter.due_only;
  const response = await hostRequest("outbox.list", payload);
  if (!Array.isArray(response.items)) {
    throw new WorkbenchError("protocol_error", "outbox.list omitted the items array.");
  }
  return {
    items: response.items as OutboxEntry[],
    total: typeof response.total === "number" ? response.total : response.items.length,
    has_more: response.has_more === true,
    next_cursor: typeof response.next_cursor === "string" ? response.next_cursor : null,
  };
}

export async function cancelOutboxEntry(input: {
  id: string;
  request_id?: string;
  expected_revision: number;
  reason?: string;
}): Promise<OutboxEntry> {
  const payload: Record<string, unknown> = {
    id: input.id,
    request_id: input.request_id ?? newID(),
    expected_revision: input.expected_revision,
  };
  if (input.reason !== undefined) payload.reason = input.reason;
  const response = await hostRequest("outbox.cancel", payload);
  return expectObject<OutboxEntry>(response.item, "outbox.cancel");
}
