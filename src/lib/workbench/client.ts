import { invoke } from "@tauri-apps/api/core";

export const STATUSES = ["inbox", "backlog", "ready", "in_progress", "review", "done"] as const;
export type TaskStatus = (typeof STATUSES)[number];
export const STATUS_LABELS: Record<TaskStatus, string> = { inbox: "Inbox", backlog: "Backlog", ready: "Ready", in_progress: "In progress", review: "Review", done: "Done" };
export interface RecordVersion { id: string; revision: number; updated_at: number }
export interface Repository extends RecordVersion { name: string; identity_key: string; remote_url: string | null }
export interface WorkspaceCard extends RecordVersion { name: string; icon: string; color: string; position: number; pinned: boolean; archived: boolean; repository_count: number }
export interface Workspace extends Omit<WorkspaceCard, "repository_count"> { description: string; repository_ids: string[] }
export interface TaskCard extends RecordVersion {
  title: string; kind: string; status: TaskStatus; priority: number; severity: string | null;
  owner: string | null; due_at: number | null; labels: string[]; repository_ids: string[];
  primary_repository_id: string; home_workspace_id: string | null; position: number;
}
export type EnhancementField = "title" | "description";
export const ENHANCEMENT_STATES = ["pending", "running", "cancel_requested", "ready", "failed", "cancelled", "interrupted", "dismissed", "accepted", "undone"] as const;
export type EnhancementState = (typeof ENHANCEMENT_STATES)[number];
export interface EnhancementSummary extends RecordVersion {
  task_id: string; source_revision: number; fields: EnhancementField[]; state: EnhancementState;
  provider: string; model: string; automatic: boolean; created_at: number; expires_at: number;
  worker_id: string | null; failure: string; accepted_fields: EnhancementField[]; edited_fields: EnhancementField[]; outcome_uncertain: boolean;
}
export interface Enhancement extends EnhancementSummary {
  source: Task; proposed: Partial<Record<EnhancementField, string>>; original_proposed: Partial<Record<EnhancementField, string>> | null; rationale: string;
}
export interface EnhancementConfiguration { provider: string; model: string; model_source: string; providers: string[] }
export interface AutomationSettings extends RecordVersion { enabled: boolean; provider: string | null; model: string | null }
export const AUTOMATIC_STATES = ["not_started", "checking", "idle", "disabled", "waiting", "generating", "stopping", "paused", "stopped"] as const;
export interface AutomaticStatus { state: typeof AUTOMATIC_STATES[number]; reason: string; task_id: string; proposal_id: string; next_check_at: number }
export type EnhancementMutation = "enhancements.create" | "enhancements.generate" | "enhancements.accept" | "enhancements.dismiss" | "enhancements.undo" | "enhancements.recover" | "enhancements.revise";
export interface Task extends TaskCard { description: string; acceptance_criteria: string[]; locked_fields?: EnhancementField[] }
export interface BriefReference extends RecordVersion { name: string }
export interface TaskBrief extends RecordVersion { format_version: 1; task: Task; repositories: BriefReference[]; workspace: BriefReference | null; markdown: string }
export const RUN_STATES = ["prepared", "starting", "running", "exited", "failed", "cancelled", "unresolved"] as const;
export const PERMISSION_MODES = ["inspect", "ask", "edit", "auto_review", "preapproved", "bypass"] as const;
export type PermissionMode = typeof PERMISSION_MODES[number];
export type RunKind = "external_terminal" | "managed";
export const PROVIDER_STATES = ["ready", "running", "completed", "failed", "interrupted", "unresolved"] as const;
export interface TaskRun extends RecordVersion {
  kind: RunKind;
  provider_state?: typeof PROVIDER_STATES[number]; provider_thread_id?: string; provider_turn_id?: string | null;
  effective_configuration?: string; output?: string; output_truncated?: boolean;
  task_id: string; source_revision: number; task_title: string; repository_id: string;
  provider: "claude" | "codex"; permission_mode: PermissionMode; state: typeof RUN_STATES[number];
  cwd: string; created_at: number; expires_at: number; session_id: string | null;
  exit_code: number | null; reason: string; outcome_uncertain: boolean;
}
export interface RunPreparation {
  kind?: RunKind;
  id: string; request_id: string; task_id: string; source_revision: number;
  repository_id: string; repository_revision: number; repo_path: string;
  provider: "claude" | "codex"; permission_mode: PermissionMode; acknowledge_bypass: boolean;
}
export type TaskDraft = Omit<Task, keyof RecordVersion>;
export type WorkspaceDraft = Omit<Workspace, keyof RecordVersion>;
export interface Page<T> { items: T[]; total: number; shown: number; has_more: boolean; next_cursor: string | null }
export type Scope = { kind: "global" } | { kind: "workspace" | "repository"; id: string };
export type WorkbenchMethod = "decisions.get" | "decisions.list" | "decisions.decide" | "notifications.settings.get" | "notifications.settings.put" | "notifications.delivery.get" | "notifications.ack" | "notifications.native.pending" | "notifications.native.status" | "notifications.native.authorize" | "attention.list" | "attention.get" | "attention.update" | EnhancementMutation | "runs.prepare_managed" | "runs.launch_managed" | "runs.stop_managed" | "runs.prepare_terminal" | "runs.get" | "runs.list" | "runs.cancel" | "workspaces.list" | "workspaces.get" | "workspaces.put" | "workspaces.delete" | "repositories.list" | "repositories.get" | "repositories.put" | "items.list" | "items.get" | "items.brief.get" | "items.put" | "items.delete" | "items.history" | "events.list" | "enhancements.complete" | "enhancements.get" | "enhancements.list" | "enhancements.configuration" | "enhancements.wake" | "enhancements.worker" | "automation.get" | "automation.put" | "automation.list";

export interface WorkbenchError {
  readonly code: string;
  message: string;
}

export const ATTENTION_TITLES = {
  run_exited: "Coding agent exited", run_failed: "Coding agent failed to start",
  run_exit_failed: "Coding agent exited with an error",
  run_unresolved: "Coding agent outcome needs attention",
  enhancement_ready: "Task enhancement ready to review", enhancement_failed: "Task enhancement failed",
  enhancement_interrupted: "Task enhancement outcome is uncertain",
  decision_permission: "Coding agent needs permission", decision_question: "Coding agent needs input",
} as const;
export interface Attention extends RecordVersion {
  source_sequence: number; task_id: string; task_revision: number; target_type: "run" | "enhancement" | "decision";
  target_id: string; target_revision: number; target_status: "current" | "changed" | "task_changed" | "task_deleted" | "unavailable";
  kind: keyof typeof ATTENTION_TITLES; title: string; created_at: number;
  read_at: number | null; dismissed_at: number | null; snoozed_until: number | null;
}
export type AttentionFilter = "active" | "unread" | "all";
export type AttentionAction = "read" | "unread" | "dismiss" | "restore" | "snooze" | "unsnooze";
export interface AttentionWrite { id: string; request_id: string; expected_revision: number; action: AttentionAction; seconds?: number }
export function attention(value: unknown): Attention {
  const raw = object(value), base = version(raw);
  const kind = Object.keys(ATTENTION_TITLES).find((key): key is keyof typeof ATTENTION_TITLES => key === raw.kind);
  const target = raw.target_type, status = raw.target_status;
  const sequence = integer(raw.source_sequence), taskRevision = integer(raw.task_revision), targetRevision = integer(raw.target_revision);
  const taskID = text(raw.task_id), targetID = text(raw.target_id);
  if (!kind || !sequence || base.id !== `event-${sequence}` || !taskRevision || !targetRevision ||
      (target !== "run" && target !== "enhancement" && target !== "decision") || !kind.startsWith(`${target}_`) ||
      (status !== "current" && status !== "changed" && status !== "task_changed" && status !== "task_deleted" && status !== "unavailable") ||
      ![taskID, targetID].every((id) => /^[A-Za-z0-9_-]{1,128}$/.test(id)) || raw.title !== ATTENTION_TITLES[kind]) return invalid();
  return { ...base, source_sequence: sequence, task_id: taskID, task_revision: taskRevision, target_type: target,
    target_id: targetID, target_revision: targetRevision, target_status: status, kind, title: ATTENTION_TITLES[kind], created_at: integer(raw.created_at),
    read_at: raw.read_at === null ? null : integer(raw.read_at), dismissed_at: raw.dismissed_at === null ? null : integer(raw.dismissed_at), snoozed_until: raw.snoozed_until === null ? null : integer(raw.snoozed_until) };
}
export async function listAttention(scope: Scope, filter: AttentionFilter, cursor?: string): Promise<Page<Attention>> {
  return page(await request("attention.list", { ...scopeParams(scope), filter, limit: 30, ...(cursor ? { cursor } : {}) }), attention);
}
export async function getAttention(id: string): Promise<Attention> {
  const result = record(await request("attention.get", { id }), attention);
  return result.id === id ? result : invalid();
}
export function attentionWrite(item: Attention, action: AttentionAction): AttentionWrite {
  return { id: item.id, expected_revision: item.revision, request_id: newID(), action, ...(action === "snooze" ? { seconds: 3600 } : {}) };
}
export async function updateAttention(input: AttentionWrite): Promise<Attention> {
  const result = record(await request("attention.update", { ...input }), attention);
  if (result.id !== input.id || result.revision !== input.expected_revision + 1) return invalid();
  return result;
}
export interface AgentDecision extends RecordVersion {
  run_id: string; task_id: string; source_revision: number; repository_id: string; repository_revision: number;
  owner_id: string; session_id: string; provider_thread_id: string; provider_turn_id: string; protocol_request_id: string;
  provider: "codex" | "claude"; permission_mode: PermissionMode; policy_revision: 1; cwd: string;
  kind: "permission" | "question"; payload: string; payload_digest: string; created_at: number; expires_at: number;
  state: "pending" | "decided" | "dispatching" | "resolved" | "cancelled";
  decision: "allow_once" | "deny" | "answer" | null; answer: string | null; actionable: boolean; reason: string;
}
export interface DecisionWrite { id: string; request_id: string; expected_revision: number; payload_digest: string; decision: "allow_once" | "deny" | "answer"; answer?: string }
export function agentDecision(value: unknown): AgentDecision {
  const raw = object(value), base = version(raw);
  const id = (v: unknown) => { const s = text(v); return /^[A-Za-z0-9_-]{1,128}$/.test(s) ? s : invalid(); };
  const opaque = (v: unknown) => { const s = text(v); return s.trim() && new TextEncoder().encode(s).length <= 256 && !s.includes("\0") ? s : invalid(); };
  const positive = (v: unknown) => { const n = integer(v); return n > 0 ? n : invalid(); };
  const provider = raw.provider, kind = raw.kind, state = raw.state, decision = raw.decision, actionable = boolean(raw.actionable);
  const mode = PERMISSION_MODES.find((mode) => mode === raw.permission_mode);
  const payload = text(raw.payload), digest = text(raw.payload_digest), answer = nullableText(raw.answer), cwd = text(raw.cwd);
  const created = integer(raw.created_at), expires = integer(raw.expires_at);
  if ((provider !== "codex" && provider !== "claude") || (kind !== "permission" && kind !== "question") || !mode || raw.policy_revision !== 1 ||
      (state !== "pending" && state !== "decided" && state !== "dispatching" && state !== "resolved" && state !== "cancelled") ||
      (decision !== null && decision !== "allow_once" && decision !== "deny" && decision !== "answer") ||
      (decision === "answer" && (kind !== "question" || !answer?.trim())) || (decision === "allow_once" && kind !== "permission") ||
      (decision !== "answer" && answer !== null) || ((state === "decided" || state === "dispatching") && decision === null) ||
      (state === "pending" && decision !== null) || (actionable && state !== "pending") ||
      !/^[a-f0-9]{64}$/.test(digest) || !payload || new TextEncoder().encode(payload).length > 65536 || payload.includes("\0") ||
      (answer !== null && new TextEncoder().encode(answer).length > 16384) || expires <= created || expires > created + 300 ||
      !cwd || new TextEncoder().encode(cwd).length > 4096 || cwd.includes("\0")) return invalid();
  let parsed: unknown; try { parsed = JSON.parse(payload); } catch { return invalid(); } object(parsed);
  return { ...base, id: id(base.id), run_id: id(raw.run_id), task_id: id(raw.task_id), source_revision: positive(raw.source_revision),
    repository_id: id(raw.repository_id), repository_revision: positive(raw.repository_revision), owner_id: id(raw.owner_id), session_id: id(raw.session_id),
    provider_thread_id: opaque(raw.provider_thread_id), provider_turn_id: opaque(raw.provider_turn_id), protocol_request_id: opaque(raw.protocol_request_id),
    provider, permission_mode: mode, policy_revision: 1, cwd, kind, payload, payload_digest: digest, created_at: created, expires_at: expires,
    state, decision, answer, actionable, reason: text(raw.reason) };
}
export async function listAgentDecisions(runID: string, cursor?: string): Promise<Page<AgentDecision>> {
  const result = page(await request("decisions.list", { run_id: runID, limit: 30, ...(cursor ? { cursor } : {}) }), agentDecision);
  return result.items.every((item) => item.run_id === runID) ? result : invalid();
}

export interface DecisionQuestion { id: string; question: string; options: {label: string; description: string}[] }
export function decisionQuestions(source: AgentDecision): DecisionQuestion[] {
  if (source.kind !== "question") return [];
  const payload = object(decodeJSON(source.payload));
  if (payload.method !== "item/tool/requestUserInput") return [];
  const raw = object(payload.params).questions;
  if (!Array.isArray(raw) || raw.length < 1 || raw.length > 3) return invalid();
  const ids = new Set<string>();
  return raw.map((value: unknown) => {
    const q = object(value), key = text(q.id), question = text(q.question);
    if (!key.trim() || ids.has(key) || q.isSecret === true || !question.trim()) return invalid();
    ids.add(key);
    const options = q.options == null ? [] : q.options;
    if (!Array.isArray(options) || options.length > 32) return invalid();
    return {id:key, question, options:options.map((option: unknown) => { const row = object(option); return {label:text(row.label),description:text(row.description)}; })};
  });
}
export function structuredDecisionAnswer(source: AgentDecision, values: Record<string, string>): string {
  const questions = decisionQuestions(source);
  if (!questions.length || Object.keys(values).length !== questions.length) return invalid();
  const pairs = questions.map((q): [string, string[]] => {
    const answer = values[q.id];
    if (typeof answer !== "string" || !answer.trim()) return invalid();
    return [q.id, [answer]];
  });
  const answer = questions.length === 1 ? pairs[0][1][0] : JSON.stringify(Object.fromEntries(pairs));
  if (new TextEncoder().encode(answer).byteLength > 16384) return invalid();
  return answer;
}
export async function decisionWrite(item: AgentDecision, decision: DecisionWrite["decision"], answer?: string): Promise<DecisionWrite> {
  if (!item.actionable || item.state !== "pending" || (decision === "allow_once" && item.kind !== "permission") ||
      (decision === "answer" && (item.kind !== "question" || !answer?.trim())) || (decision !== "answer" && answer !== undefined)) return invalid();
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(item.payload));
  const encoded = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
  if (encoded !== item.payload_digest) throw new WorkbenchError("protocol_error", "The stored request does not match its payload digest. No decision was saved.");
  return { id: item.id, request_id: newID(), expected_revision: item.revision, payload_digest: encoded, decision, ...(answer !== undefined ? { answer } : {}) };
}
export async function saveAgentDecision(source: AgentDecision, input: DecisionWrite): Promise<AgentDecision> {
  if (input.id !== source.id || input.expected_revision !== source.revision || input.payload_digest !== source.payload_digest) return invalid();
  const result = record(await request("decisions.decide", { ...input }), agentDecision);
  if (result.id !== source.id || result.revision !== source.revision + 1 || result.state !== "decided" || result.decision !== input.decision || result.answer !== (input.answer ?? null)) return invalid();
  for (const key of ["run_id", "task_id", "source_revision", "repository_id", "repository_revision", "owner_id", "session_id", "provider_thread_id", "provider_turn_id", "protocol_request_id", "payload", "payload_digest", "policy_revision", "permission_mode", "cwd", "expires_at"] as const) {
    if (result[key] !== source[key]) return invalid();
  }
  return result;
}
export interface NotificationSettings extends RecordVersion {
  profile_id: string; enabled: boolean; sound: boolean; background: boolean; after_sequence: number;
  quiet_start: number | null; quiet_end: number | null;
  muted_workspace_ids: string[]; muted_repository_ids: string[]; muted_task_ids: string[];
}
export type NotificationDraft = Omit<NotificationSettings, "id" | "revision" | "updated_at" | "profile_id" | "after_sequence">;
export type NotificationWrite = NotificationDraft & { id: "profile"; expected_revision: number; request_id: string };
export interface NativeNotificationStatus { available: boolean; authorization: "unavailable" | "unknown" | "not_determined" | "denied" | "authorized" | "provisional"; error: string | null }
export interface NotificationDelivery extends RecordVersion { task_id: string; native_id: string; state: "uncertain" | "submitted" | "failed"; created_at: number; updated_at: number; activated_at: number | null; acknowledged_at: number | null }
export function notificationSettings(value: unknown): NotificationSettings {
  const raw = object(value), base = version(raw), profile = text(raw.profile_id);
  const minute = (v: unknown) => { if (v === null) return null; const n = integer(v); return n < 1440 ? n : invalid(); };
  const scopes = (v: unknown) => { const ids = strings(v); return ids.length <= 64 && new Set(ids).size === ids.length && ids.every((id) => /^[A-Za-z0-9_-]{1,128}$/.test(id)) ? ids : invalid(); };
  const start = minute(raw.quiet_start), end = minute(raw.quiet_end);
  if (base.id !== "profile" || !/^[a-f0-9]{32}$/.test(profile) || (start === null) !== (end === null) || start !== null && start === end) return invalid();
  return { ...base, profile_id: profile, enabled: boolean(raw.enabled), sound: boolean(raw.sound), background: boolean(raw.background), after_sequence: integer(raw.after_sequence), quiet_start: start, quiet_end: end, muted_workspace_ids: scopes(raw.muted_workspace_ids), muted_repository_ids: scopes(raw.muted_repository_ids), muted_task_ids: scopes(raw.muted_task_ids) };
}
export function notificationDraft(settings: NotificationSettings): NotificationDraft {
  return { enabled: settings.enabled, sound: settings.sound, background: settings.background, quiet_start: settings.quiet_start, quiet_end: settings.quiet_end, muted_workspace_ids: [...settings.muted_workspace_ids], muted_repository_ids: [...settings.muted_repository_ids], muted_task_ids: [...settings.muted_task_ids] };
}
export async function getNotificationSettings(): Promise<NotificationSettings> { return record(await request("notifications.settings.get", { id: "profile" }), notificationSettings); }
export async function putNotificationSettings(write: NotificationWrite): Promise<NotificationSettings> {
  const result = record(await request("notifications.settings.put", { ...write }), notificationSettings);
  return result.revision === write.expected_revision + 1 ? result : invalid();
}
export async function nativeNotificationStatus(authorize = false): Promise<NativeNotificationStatus> {
  const raw = object(await request(authorize ? "notifications.native.authorize" : "notifications.native.status", {}));
  const authorization = raw.authorization;
  if (authorization !== "unavailable" && authorization !== "unknown" && authorization !== "not_determined" && authorization !== "denied" && authorization !== "authorized" && authorization !== "provisional") return invalid();
  const available = boolean(raw.available);
  if (!available && authorization !== "unavailable") return invalid();
  return { available, authorization, error: nullableText(raw.error) };
}
export function notificationDelivery(value: unknown): NotificationDelivery {
  const raw = object(value), base = version(raw), state = raw.state, native = text(raw.native_id), task = text(raw.task_id);
  if ((state !== "uncertain" && state !== "submitted" && state !== "failed") || !/^event-[0-9]{1,16}$/.test(base.id) || !new RegExp(`^gitpulse\\.[a-f0-9]{32}\\.${base.id}$`).test(native) || !/^[A-Za-z0-9_-]{1,128}$/.test(task)) return invalid();
  const activated = raw.activated_at === null ? null : integer(raw.activated_at), acknowledged = raw.acknowledged_at === null ? null : integer(raw.acknowledged_at);
  if (acknowledged !== null && activated === null) return invalid();
  return { ...base, state, native_id: native, task_id: task, created_at: integer(raw.created_at), updated_at: integer(raw.updated_at), activated_at: activated, acknowledged_at: acknowledged };
}
export async function getNotificationDelivery(id: string): Promise<NotificationDelivery> {
  const result = record(await request("notifications.delivery.get", { id }), notificationDelivery);
  return result.id === id ? result : invalid();
}
export async function pendingNotificationActivation(): Promise<NotificationDelivery | null> {
  const result = page(await request("notifications.native.pending", {}), notificationDelivery);
  if (result.items.length > 1 || result.items.some((d) => d.activated_at === null || d.acknowledged_at !== null)) return invalid();
  return result.items[0] ?? null;
}
export async function acknowledgeNotification(id: string): Promise<void> {
  const current = await getNotificationDelivery(id);
  if (current.acknowledged_at !== null) return;
  const write = { id, expected_revision: current.revision, request_id: newID() };
  const result = record(await request("notifications.ack", write), notificationDelivery);
  if (result.id !== id || result.revision !== current.revision + 1 || result.acknowledged_at === null) return invalid();
}

export class WorkbenchError extends Error {
  constructor(readonly code: string, message: string) { super(message); }
}
function invalid(): never { throw new WorkbenchError("protocol_error", "Task storage returned an invalid response."); }
function object(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return invalid();
  return value as Record<string, unknown>;
}
function text(value: unknown): string { return typeof value === "string" ? value : invalid(); }
function integer(value: unknown): number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : invalid(); }
function nullableText(value: unknown): string | null { return value === null ? null : text(value); }
function boolean(value: unknown): boolean { return typeof value === "boolean" ? value : invalid(); }
function strings(value: unknown): string[] { if (!Array.isArray(value) || value.length > 10_000) return invalid(); return value.map(text); }
function version(raw: Record<string, unknown>): RecordVersion {
  const revision = integer(raw.revision);
  const id = text(raw.id);
  if (!revision || !/^[A-Za-z0-9_-]{1,128}$/.test(id)) return invalid();
  return { id, revision, updated_at: integer(raw.updated_at) };
}
export function repository(value: unknown): Repository {
  const raw = object(value);
  return { ...version(raw), name: text(raw.name), identity_key: text(raw.identity_key), remote_url: nullableText(raw.remote_url) };
}
function workspaceBase(raw: Record<string, unknown>) {
  return { ...version(raw), name: text(raw.name), icon: text(raw.icon), color: text(raw.color), position: integer(raw.position), archived: boolean(raw.archived), pinned: boolean(raw.pinned) };
}
export function workspaceCard(value: unknown): WorkspaceCard { const raw = object(value); return { ...workspaceBase(raw), repository_count: integer(raw.repository_count) }; }
export function workspace(value: unknown): Workspace { const raw = object(value); return { ...workspaceBase(raw), description: text(raw.description), repository_ids: strings(raw.repository_ids) }; }
export function taskCard(value: unknown): TaskCard {
  const raw = object(value);
  const status = STATUSES.find((s) => s === raw.status);
  const priority = integer(raw.priority);
  const repositories = strings(raw.repository_ids);
  const primary = text(raw.primary_repository_id);
  if (!status || priority > 3 || repositories.length > 64 || new Set(repositories).size !== repositories.length || !repositories.includes(primary)) return invalid();
  return { ...version(raw), title: text(raw.title), kind: text(raw.kind), status, priority,
    severity: nullableText(raw.severity), owner: nullableText(raw.owner), due_at: raw.due_at === null ? null : integer(raw.due_at),
    labels: strings(raw.labels), repository_ids: repositories, primary_repository_id: primary,
    home_workspace_id: nullableText(raw.home_workspace_id), position: integer(raw.position) };
}
function lockFields(value: unknown): EnhancementField[] {
  const fields = strings(value);
  if (fields.length > 2 || new Set(fields).size !== fields.length) return invalid();
  return fields.map((field) => field === "title" || field === "description" ? field : invalid());
}
export function task(value: unknown): Task { const raw = object(value); return { ...taskCard(raw), description: text(raw.description), acceptance_criteria: strings(raw.acceptance_criteria), ...(raw.locked_fields === undefined ? {} : { locked_fields: lockFields(raw.locked_fields) }) }; }
export function enhancementSummary(value: unknown): EnhancementSummary {
  const raw = object(value);
  const state = ENHANCEMENT_STATES.find((state) => state === raw.state);
  const fields = lockFields(raw.fields);
  const taskID = text(raw.task_id), sourceRevision = integer(raw.source_revision);
  const provider = text(raw.provider), model = text(raw.model);
  const worker = raw.worker_id === undefined ? null : text(raw.worker_id);
  const failure = raw.failure === undefined ? "" : text(raw.failure);
  const accepted = raw.accepted_fields === undefined ? [] : lockFields(raw.accepted_fields);
  const edited = raw.edited_fields === undefined ? [] : lockFields(raw.edited_fields);
  const uncertain = raw.outcome_uncertain === undefined ? false : boolean(raw.outcome_uncertain);
  if (!state || !fields.length || !sourceRevision || !/^[A-Za-z0-9_-]{1,128}$/.test(taskID) ||
      !provider.trim() || provider.length > 128 || !model.trim() || model.length > 512 ||
      (worker !== null && !/^[A-Za-z0-9_-]{1,128}$/.test(worker)) ||
      (["running", "cancel_requested"].includes(state) && worker === null) ||
      (["failed", "cancelled", "interrupted"].includes(state) && !failure.trim()) ||
      (state === "interrupted" && !uncertain) ||
      (["accepted", "undone"].includes(state) && !accepted.length) ||
      [...accepted, ...edited].some((field) => !fields.includes(field))) return invalid();
  return { ...version(raw), task_id: taskID, source_revision: sourceRevision, fields, state, provider, model,
    automatic: boolean(raw.automatic), created_at: integer(raw.created_at), expires_at: integer(raw.expires_at),
    worker_id: worker, failure, accepted_fields: accepted, edited_fields: edited, outcome_uncertain: uncertain };
}
function proposalFields(value: unknown, requested: EnhancementField[], required: boolean): Partial<Record<EnhancementField, string>> {
  const proposed: Partial<Record<EnhancementField, string>> = {};
  if (value !== undefined) {
    const fields = object(value);
    for (const [field, value] of Object.entries(fields)) {
      if ((field !== "title" && field !== "description") || !requested.includes(field)) return invalid();
      const content = text(value);
      if (content.includes("\0") || (field === "title" && (!content.trim() || [...content].length > 300)) ||
          new TextEncoder().encode(content).length > (field === "title" ? 1200 : 65_536)) return invalid();
      proposed[field] = content;
    }
    if (requested.some((field) => proposed[field] === undefined)) return invalid();
  } else if (required) return invalid();
  return proposed;
}
export function enhancement(value: unknown): Enhancement {
  const raw = object(value), summary = enhancementSummary(raw), source = task(raw.source);
  if (source.id !== summary.task_id || source.revision !== summary.source_revision) return invalid();
  const proposed = proposalFields(raw.proposed, summary.fields, ["ready", "accepted", "undone"].includes(summary.state));
  const original = raw.original_proposed === undefined ? null : proposalFields(raw.original_proposed, summary.fields, true);
  if ((summary.edited_fields.length && original === null) || (original && summary.fields.some((field) => (original[field] !== proposed[field]) !== summary.edited_fields.includes(field)))) return invalid();
  return { ...summary, source, proposed, original_proposed: original, rationale: raw.rationale === undefined ? "" : text(raw.rationale) };
}
export function page<T>(value: unknown, decode: (raw: unknown) => T): Page<T> {
  const raw = object(value);
  if (raw.ok !== true || !Array.isArray(raw.items) || raw.items.length > 200) return invalid();
  const total = integer(raw.total), shown = integer(raw.shown), more = boolean(raw.has_more), cursor = nullableText(raw.next_cursor);
  if (shown !== raw.items.length || total < shown || (more && (!cursor || shown === 0)) || (!more && cursor !== null)) return invalid();
  return { items: raw.items.map(decode), total, shown, has_more: more, next_cursor: cursor };
}
export function explainError(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null && "message" in error && typeof error.message === "string") return error.message;
  return typeof error === "string" ? error : "Task storage is unavailable. Try again.";
}
export function newID(): string { return crypto.randomUUID(); }
export async function request(method: WorkbenchMethod, input: Record<string, unknown>): Promise<unknown> {
  try { return decodeJSON(await invoke<unknown>("cmd_workbench_request", { method, input: JSON.stringify(input) })); }
  catch (error) {
    const code = typeof error === "object" && error !== null && "code" in error && typeof error.code === "string" ? error.code : "transport_error";
    throw new WorkbenchError(code, explainError(error));
  }
}
function decodeJSON(value: unknown): unknown {
  if (typeof value !== "string" || value.length > 2 * 1024 * 1024) return invalid();
  try { return JSON.parse(value); } catch { return invalid(); }
}
export function taskRun(value: unknown): TaskRun {
  const raw = object(value);
  const state = RUN_STATES.find((state) => state === raw.state);
  const permission = PERMISSION_MODES.find((mode) => mode === raw.permission_mode);
  const provider = raw.provider;
  const kind = raw.kind;
  if (kind !== "external_terminal" && kind !== "managed") return invalid();
  const providerState = PROVIDER_STATES.find((value) => value === raw.provider_state);
  if (raw.provider_state !== undefined && (!providerState || kind !== "managed")) return invalid();
  const thread = raw.provider_thread_id === undefined ? undefined : text(raw.provider_thread_id);
  const turn = raw.provider_turn_id === undefined ? undefined : nullableText(raw.provider_turn_id);
  if (providerState && ["ready", "running", "completed"].includes(providerState) && !thread?.trim()) return invalid();
  if (providerState && ["running", "completed"].includes(providerState) && !turn?.trim()) return invalid();
  if ((thread !== undefined || turn !== undefined) && (!providerState || !thread?.trim())) return invalid();
  if (!state || !permission || (provider !== "claude" && provider !== "codex")) return invalid();
  const source = integer(raw.source_revision);
  if (!source) return invalid();
  return { ...version(raw), kind,
    ...(providerState ? {provider_state: providerState} : {}),
    ...(thread === undefined ? {} : {provider_thread_id: thread}),
    ...(turn === undefined ? {} : {provider_turn_id: turn}),
    ...(raw.effective_configuration === undefined ? {} : {effective_configuration: text(raw.effective_configuration)}),
    ...(raw.output === undefined ? {} : {output: text(raw.output)}),
    ...(raw.output_truncated === undefined ? {} : {output_truncated: boolean(raw.output_truncated)}),
    task_id: text(raw.task_id), source_revision: source, task_title: text(raw.task_title), repository_id: text(raw.repository_id), provider, permission_mode: permission, state,
    cwd: text(raw.cwd), created_at: integer(raw.created_at), expires_at: integer(raw.expires_at), session_id: nullableText(raw.session_id),
    exit_code: raw.exit_code === null ? null : integer(raw.exit_code), reason: text(raw.reason), outcome_uncertain: boolean(raw.outcome_uncertain) };
}
export async function getRepository(id: string): Promise<Repository> {
  const repo = record(await request("repositories.get", { id }), repository);
  return repo.id === id ? repo : invalid();
}
export async function prepareTaskRun(input: RunPreparation): Promise<TaskRun> {
  const { kind = "external_terminal", ...body } = input;
  const run = record(await request(kind === "managed" ? "runs.prepare_managed" : "runs.prepare_terminal", body), taskRun);
  if (run.kind !== kind || run.id !== input.id || run.task_id !== input.task_id || run.source_revision !== input.source_revision || run.repository_id !== input.repository_id || run.provider !== input.provider || run.permission_mode !== input.permission_mode || run.state !== "prepared") return invalid();
  return run;
}
export async function launchManagedRun(id: string): Promise<TaskRun> {
  const run = record(await request("runs.launch_managed", {id}), taskRun);
  return run.id === id && run.kind === "managed" && run.provider === "codex" ? run : invalid();
}
export async function stopManagedRun(id: string): Promise<void> {
  const result = object(await request("runs.stop_managed", {id}));
  if (result.ok !== true) invalid();
}
export async function getTaskRun(id: string): Promise<TaskRun> {
  const run = record(await request("runs.get", { id }), taskRun);
  return run.id === id ? run : invalid();
}
export async function listTaskRuns(taskID: string, cursor?: string): Promise<Page<TaskRun>> {
  const result = page(await request("runs.list", { task_id: taskID, limit: 30, newest: true, ...(cursor ? { cursor } : {}) }), taskRun);
  return result.items.every((run) => run.task_id === taskID) ? result : invalid();
}
export async function cancelTaskRun(run: TaskRun): Promise<TaskRun> {
  const saved = record(await request("runs.cancel", { id: run.id, expected_revision: run.revision, request_id: newID() }), taskRun);
  return saved.id === run.id && saved.task_id === run.task_id && saved.state === "cancelled" ? saved : invalid();
}
function record<T>(response: unknown, decode: (value: unknown) => T): T { const raw = object(response); if (raw.ok !== true) return invalid(); return decode(raw.item); }
export async function registerRepository(path: string): Promise<Repository> {
  const raw = object(decodeJSON(await invoke<unknown>("cmd_workbench_register_repository", { repoPath: path, id: newID(), requestId: newID() })));
  return repository(raw.repository);
}
export async function listRepositories(cursor?: string): Promise<Page<Repository>> { return page(await request("repositories.list", { limit: 200, ...(cursor ? { cursor } : {}) }), repository); }
export async function listWorkspaces(cursor?: string): Promise<Page<WorkspaceCard>> { return page(await request("workspaces.list", { limit: 200, include_archived: true, ...(cursor ? { cursor } : {}) }), workspaceCard); }
export async function getWorkspace(id: string): Promise<Workspace> { return record(await request("workspaces.get", { id }), workspace); }
export async function getTask(id: string): Promise<Task> { return record(await request("items.get", { id }), task); }
export async function getTaskBrief(id: string, expectedRevision: number): Promise<TaskBrief> {
  const brief = record(await request("items.brief.get", { id, expected_revision: expectedRevision }), taskBrief);
  if (brief.id !== id || brief.revision !== expectedRevision) return invalid();
  return brief;
}
export async function getEnhancement(id: string): Promise<Enhancement> { return record(await request("enhancements.get", { id }), enhancement); }
export async function listEnhancements(taskID: string, cursor?: string): Promise<Page<EnhancementSummary>> {
  return page(await request("enhancements.list", { task_id: taskID, newest: true, limit: 30, ...(cursor ? { cursor } : {}) }), enhancementSummary);
}
export async function changeEnhancement(method: EnhancementMutation, input: Record<string, unknown>): Promise<Enhancement> {
  return record(await request(method, input), enhancement);
}
export async function enhancementConfiguration(): Promise<EnhancementConfiguration> {
  const raw = object(await request("enhancements.configuration", {}));
  if (raw.ok !== true) return invalid();
  const providers = strings(raw.providers), provider = text(raw.provider), model = text(raw.model);
  if (!providers.length || providers.length > 64 || new Set(providers).size !== providers.length || !providers.includes(provider) || model.length > 512) return invalid();
  return { provider, model, model_source: text(raw.model_source), providers };
}
export function scopeParams(scope: Scope): Record<string, string> { return scope.kind === "global" ? {} : scope.kind === "workspace" ? { workspace_id: scope.id } : { repository_id: scope.id }; }
export async function listTasks(scope: Scope, status: TaskStatus, query: string, cursor?: string): Promise<Page<TaskCard>> {
  return page(await request("items.list", { ...scopeParams(scope), status, query, limit: 30, ...(cursor ? { cursor } : {}) }), taskCard);
}
export function taskDraft(full: Task): TaskDraft {
  const { id: _id, revision: _revision, updated_at: _updated, ...draft } = full;
  return draft;
}
export function workspaceDraft(full: Workspace): WorkspaceDraft { const { id: _id, revision: _revision, updated_at: _updated, ...draft } = full; return draft; }
// Keep the same mutation object for an uncertain retry; never regenerate its ID.
export function taskWrite(id: string, revision: number, draft: TaskDraft): Record<string, unknown> {
  const body: Record<string, unknown> = { ...draft, id, expected_revision: revision, request_id: newID() };
  // Null date is a read representation; omission clears it on a replacement.
  if (draft.due_at === null) delete body.due_at;
  return body;
}
export async function putTask(input: Record<string, unknown>): Promise<Task> {
  const receipt = object(await request("items.put", input));
  const saved = record(receipt, task);
  if (receipt.automatic_enhancement_queued === true) void wakeAutomatic();
  return saved;
}
export async function putWorkspace(input: Record<string, unknown>): Promise<Workspace> { return record(await request("workspaces.put", input), workspace); }

export async function deleteTask(input: Record<string, unknown>): Promise<void> {
  const receipt = object(await request("items.delete", input));
  const saved = object(receipt.item);
  if (receipt.ok !== true || saved.deleted !== true || saved.id !== input.id ||
      typeof input.expected_revision !== "number" || saved.revision !== input.expected_revision + 1) return invalid();
}

export function automationSettings(value: unknown): AutomationSettings {
  const raw = object(value), base = version(raw), provider = nullableText(raw.provider), model = nullableText(raw.model);
  if (base.id !== "profile" || (provider === null) !== (model === null) ||
      (provider !== null && (!provider.trim() || provider.length > 128)) ||
      (model !== null && (!model.trim() || model.length > 512))) return invalid();
  return { ...base, enabled: boolean(raw.enabled), provider, model };
}
export function automaticStatus(value: unknown): AutomaticStatus {
  const raw = object(value), state = AUTOMATIC_STATES.find((state) => state === raw.state);
  const taskID = text(raw.task_id), proposalID = text(raw.proposal_id), reason = text(raw.reason);
  if (raw.ok !== true || !state || reason.length > 4096 || [taskID, proposalID].some((id) => id !== "" && !/^[A-Za-z0-9_-]{1,128}$/.test(id))) return invalid();
  return { state, reason, task_id: taskID, proposal_id: proposalID, next_check_at: integer(raw.next_check_at) };
}
export async function getAutomation(): Promise<AutomationSettings> { return record(await request("automation.get", { id: "profile" }), automationSettings); }
export async function putAutomation(input: Record<string, unknown>): Promise<AutomationSettings> {
  const saved = record(await request("automation.put", input), automationSettings);
  void wakeAutomatic();
  return saved;
}
export async function automaticQueueCount(): Promise<number> {
  return page(await request("automation.list", { limit: 1 }), (value) => {
    const raw = object(value);
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(text(raw.id)) || !integer(raw.revision)) return invalid();
    return { id: text(raw.id), not_before_ms: integer(raw.not_before_ms) };
  }).total;
}
type AutomaticUpdate = { status: AutomaticStatus | null; error: string };
let automaticUpdate: AutomaticUpdate = { status: null, error: "" };
const automaticListeners = new Set<(value: AutomaticUpdate) => void>();
let automaticWatchers = 0, automaticTimer: ReturnType<typeof setInterval> | null = null;
function scheduleAutomaticRefresh() {
  const busy = automaticUpdate.status && ["checking", "waiting", "generating", "stopping"].includes(automaticUpdate.status.state);
  if (automaticWatchers > 0 && busy && automaticTimer === null) automaticTimer = setInterval(() => { void refreshAutomatic(); }, 2000);
  else if ((!automaticWatchers || !busy) && automaticTimer !== null) { clearInterval(automaticTimer); automaticTimer = null; }
}
// Visible boards share one status timer. Idle, paused and hidden boards use none.
export function watchAutomatic(): () => void {
  automaticWatchers++; scheduleAutomaticRefresh();
  let stopped = false;
  return () => { if (!stopped) { stopped = true; automaticWatchers--; scheduleAutomaticRefresh(); } };
}
export const automaticUpdates = { subscribe(listener: (value: AutomaticUpdate) => void) {
  automaticListeners.add(listener); listener(automaticUpdate);
  return () => { automaticListeners.delete(listener); };
} };
function publishAutomatic(value: AutomaticUpdate) {
  automaticUpdate = value;
  scheduleAutomaticRefresh();
  for (const listener of automaticListeners) listener(value);
}
let wakePending: Promise<void> | null = null, wakeAgain = false;
let automaticEpoch = 0;
// This is a delivery hint, not a second scheduler. Manvi owns debounce, claims,
// quotas and recovery. A failed wake never rejects an already committed save.
export function wakeAutomatic(): Promise<void> {
  automaticEpoch++;
  wakeAgain = true;
  if (wakePending) return wakePending;
  wakePending = (async () => {
    do {
      wakeAgain = false;
      try { publishAutomatic({ status: automaticStatus(await request("enhancements.wake", {})), error: "" }); }
      catch (error) { publishAutomatic({ status: null, error: explainError(error) }); }
    } while (wakeAgain);
  })().finally(() => { wakePending = null; if (wakeAgain) void wakeAutomatic(); });
  return wakePending;
}
let statusPending: Promise<void> | null = null;
export function refreshAutomatic(): Promise<void> {
  if (statusPending) return statusPending;
  const epoch = automaticEpoch;
  statusPending = (async () => {
    try { const status = automaticStatus(await request("enhancements.worker", {})); if (epoch === automaticEpoch) publishAutomatic({ status, error: "" }); }
    catch (error) { if (epoch === automaticEpoch) publishAutomatic({ status: null, error: explainError(error) }); }
  })().finally(() => { statusPending = null; });
  return statusPending;
}
function briefReference(value: unknown): BriefReference {
  const raw = object(value), name = text(raw.name);
  if (!name.trim() || name.includes("\0") || new TextEncoder().encode(name).length > 300) return invalid();
  return { ...version(raw), name };
}
export function taskBrief(value: unknown): TaskBrief {
  const raw = object(value), saved = version(raw), full = task(raw.task), markdown = text(raw.markdown);
  if (raw.format_version !== 1 || saved.id !== full.id || saved.revision !== full.revision || saved.updated_at !== full.updated_at ||
      !Array.isArray(raw.repositories) || raw.repositories.length !== full.repository_ids.length ||
      !markdown.trim() || markdown.includes("\0") || new TextEncoder().encode(markdown).length > 2 * 1024 * 1024) return invalid();
  const repositories = raw.repositories.map(briefReference);
  if (repositories.some((repo, index) => repo.id !== full.repository_ids[index])) return invalid();
  const workspace = raw.workspace === null ? null : briefReference(raw.workspace);
  if ((workspace?.id ?? null) !== full.home_workspace_id) return invalid();
  return { ...saved, format_version: 1, task: full, repositories, workspace, markdown };
}
