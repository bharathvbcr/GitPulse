/**
 * Pure TypeScript Agents plane projection extracted from `src/lib/agents/plane.ts`,
 * extended with `active_subagents` rollups, per-session `ApprovalChannel`,
 * `ControlCapability` matrix, and honest `"ungated"` / `"Queued"` badges.
 *
 * Zero Svelte, DOM, or Tauri imports.
 */

import type {
  ApprovalChannel,
  CapabilitySet,
  SubagentSummary,
} from "../protocol/types";
import {
  defaultCapabilitiesForHarness,
  describeControlCapabilities,
  type CapabilityBadge,
} from "./capabilities";
import {
  identityKey,
  nearestContaining,
  plural,
  type PathIdentityOptions,
} from "./paths";
import {
  agentKind,
  asksForReader,
  type SessionAttentionUrgency,
} from "./vocabulary";

export const MAX_AGENT_REPOS = 64;
export const MAX_AGENT_ROWS = 400;

export const AGENT_COLUMNS = [
  { key: "checkout", label: "Checkout" },
  { key: "presence", label: "Presence" },
  { key: "attention", label: "Attention" },
  { key: "parallel", label: "Parallel" },
  { key: "changes", label: "Changes" },
] as const;

export type AgentColumnKey = (typeof AGENT_COLUMNS)[number]["key"];
export type AgentFilter =
  | "all"
  | "attention"
  | "notifications"
  | "gaps"
  | "live"
  | "parallel";

export function isAgentFilter(value: unknown): value is AgentFilter {
  return (
    value === "all" ||
    value === "attention" ||
    value === "notifications" ||
    value === "gaps" ||
    value === "live" ||
    value === "parallel"
  );
}

export const ATTENTION_REASONS = [
  "needs-you",
  "error",
  "pending",
  "blocked",
  "collision",
  "disconnected",
  "unstarted",
  "signalled",
  "idle-waiting",
  "finished",
  "notice",
  "unscanned",
  "unprobed",
  "unmeasured",
  "dirty",
] as const;

export type AttentionReason = (typeof ATTENTION_REASONS)[number];

const ATTENTION_RANK = new Map(
  ATTENTION_REASONS.map((reason, index) => [reason, index]),
);

const FILTER_ATTENTION = new Set<AttentionReason>([
  "needs-you",
  "error",
  "pending",
  "blocked",
  "collision",
  "disconnected",
  "unstarted",
  "signalled",
  "unscanned",
  "unprobed",
  "unmeasured",
]);

export const NOTIFICATION_REASONS = new Set<AttentionReason>([
  "idle-waiting",
  "finished",
  "notice",
]);

export const UNREAD_REASONS = new Set<AttentionReason>([
  "unscanned",
  "unprobed",
  "unmeasured",
]);

const REASON_LABEL: Record<AttentionReason, string> = {
  "needs-you": "Needs you",
  error: "Stopped on an error",
  pending: "Requests waiting",
  blocked: "Git operation parked",
  collision: "Overlaps another worktree",
  disconnected: "Running, not shown in this window",
  unstarted: "Prepared, not started",
  signalled: "Asked for attention",
  "idle-waiting": "Idle, waiting for input",
  finished: "Turn finished",
  notice: "Notification",
  unscanned: "Collisions not fully read",
  unprobed: "Parked operation not read",
  unmeasured: "Changes not read",
  dirty: "Uncommitted changes",
};

export type AgentPresence = "live" | "exited" | "on-disk" | "missing";
export type AgentOrigin = "checkout" | "terminal" | "task" | "session" | "subagent";
export type GapKind = "failed" | "skipped" | "unread" | "partial";

export interface ProbeGap {
  repoPath: string;
  label: string;
  kind: GapKind;
  reason: string;
}

/** Minimal worktree summary shape needed by `projectAgentPlane`. */
export interface PlaneWorktreeSummary {
  path: string;
  branch: string | null;
  is_detached: boolean;
  is_main: boolean;
  dirty_files: number | null;
  agent_kind: string;
  session_slug: string;
  operation_kind: string;
  operation_ok: boolean;
}

export interface PlaneLiveSession {
  kind: string;
  pid: number;
  status: string;
  cwd: string;
  tree?: string;
  parent_pid?: number;
}

export interface PlaneInsightsSnapshot {
  worktrees: {
    ok: boolean;
    error: string;
    truncated: boolean;
    items: readonly PlaneWorktreeSummary[];
  };
  agents: {
    ok: boolean;
    sessions: number;
    truncated: boolean;
    live: {
      worktrees: readonly {
        path: string;
        sessions: readonly PlaneLiveSession[];
      }[];
    };
  };
  collisions: {
    ok: boolean;
    error: string;
    unscanned_worktrees: number;
    failed_worktrees: number;
    truncated: boolean;
    items: readonly {
      worktrees: readonly { path: string }[];
    }[];
  };
}

export interface PlaneProbe {
  path: string;
  label: string;
  snapshot: PlaneInsightsSnapshot | null;
  error: string;
  skipped: boolean;
  skipReason: "" | "deadline" | "cap";
}

export interface PlaneTerminal {
  key: string;
  repoPath: string;
  label: string;
  launcher?: string | null;
  title: string;
  status: string;
  sessionId: string;
  taskRunId: string;
  continuesRunId: string;
  cwd: string | null;
  attention: "needs-you" | "error" | "signalled" | "finished" | null;
  attentionUrgency?: SessionAttentionUrgency | null;
  attentionEvent?: string | null;
  attentionChannel?: string | null;
  attentionLabel?: string | null;
  attentionDetail?: string | null;
  attentionAt?: number | null;
  fullyIdle?: boolean;
  providerSessionId?: string | null;
  /** Active sub-agents / swarm workers promoted from `HubSessionState`. */
  activeSubagents?: readonly SubagentSummary[];
  /** Runtime approval channel for this session. */
  approvalChannel?: ApprovalChannel;
  /** Explicit host-reported runtime capability matrix. */
  capabilities?: CapabilitySet;
  /** Number of steering messages currently queued for the next turn. */
  queuedSteeringCount?: number;
}

export interface PlaneTask {
  runId: string;
  title: string;
  repoPath: string;
  cwd: string;
  provider: string;
  tone:
    | "needs-you"
    | "error"
    | "signalled"
    | "finished"
    | "problem"
    | "starting"
    | "active"
    | "quiet"
    | "adopted"
    | null;
  disconnected: boolean;
  unstarted: boolean;
  pendingCount: number;
  pendingMore: boolean;
  approvalChannel?: ApprovalChannel;
  capabilities?: CapabilitySet;
}

export interface TaskProbe {
  ok: boolean;
  unread: boolean;
  error: string;
  complete: boolean;
  tasks: readonly PlaneTask[];
}

export interface PlaneInput {
  probes: readonly PlaneProbe[];
  terminals: readonly PlaneTerminal[];
  tasks: TaskProbe | null;
  paths: PathIdentityOptions;
  /** Whether terminal bell counts as an ask. Passed explicitly for purity. */
  bellAsks?: boolean;
}

export interface AgentRollup {
  /** Rows nested anywhere under this one (including promoted sub-agent rows). */
  descendants: number;
  /** Of those, how many are live. */
  live: number;
  /** Attention reasons rolled up from descendants, most urgent first. */
  attention: AttentionReason[];
  /** Descendants whose attention nothing observes. */
  unobserved: number;
  /** Count of active sub-agents rolled up from this row and descendants. */
  subagents: number;
  /** Count of live/running sub-agents rolled up from this row and descendants. */
  liveSubagents: number;
}

export interface AgentRow {
  id: string;
  parentId: string | null;
  children: AgentRow[];
  rollup: AgentRollup;
  attentionObserved: boolean;
  repoPath: string;
  repoLabel: string;
  kind: string;
  session: string;
  checkout: string;
  worktreePath: string;
  checkoutPath: string | null;
  origin: AgentOrigin;
  presence: AgentPresence;
  presenceDetail: string;
  attention: AttentionReason[];
  attentionLabel: string;
  attentionUrgency: SessionAttentionUrgency | null;
  attentionEvent: string | null;
  attentionChannel: string | null;
  attentionDetail: string | null;
  attentionAt: number | null;
  launcher: string | null;
  sessionId: string | null;
  fullyIdle: boolean;
  providerSessionId: string | null;
  parallelCount: number;
  parallelFloor: boolean;
  dirtyFiles: number | null;
  dirtyKnown: boolean;
  liveKey: string | null;
  taskRunId: string | null;
  outsideSessions: number;
  /** Promoted active sub-agents belonging directly to this session. */
  activeSubagents: SubagentSummary[];
  /** Per-session runtime approval channel. */
  approvalChannel: ApprovalChannel;
  /** Per-session runtime capability matrix. */
  capabilities: CapabilitySet;
  /** UI-ready capability badges (includes `"ungated"` and `"Queued"` when applicable). */
  capabilityBadges: CapabilityBadge[];
  /** Convenience flag: true when tool calls are not remotely gated (e.g. Antigravity). */
  ungated: boolean;
  /** Queued steering messages waiting for the next turn. */
  queuedSteeringCount: number;
}

export interface AgentPlane {
  rows: AgentRow[];
  roots: AgentRow[];
  childSessions: number;
  unplacedSessions: number;
  /** Total active sub-agents projected across all rows. */
  activeSubagents: number;
  shown: number;
  total: number;
  truncated: boolean;
  gaps: ProbeGap[];
  sessionsAreFloor: boolean;
  checkouts: number;
  checkoutsFloor: boolean;
  live: number;
  taskAttempts: number | null;
  tasksWatched: boolean;
  tasksFloor: boolean;
  attention: { needing: number; unread: number };
  notifications: number;
  requested: number;
  read: number;
  failed: number;
  skipped: number;
}

interface Draft {
  repoPath: string;
  repoLabel: string;
  kind: string;
  launcher: string | null;
  session: string;
  checkout: string;
  worktreePath: string;
  checkoutPath: string | null;
  origin: AgentOrigin;
  presence: AgentPresence;
  presenceDetail: string;
  attention: Set<AttentionReason>;
  terminalPrimaryReason: AttentionReason | null;
  attentionUrgency: SessionAttentionUrgency | null;
  attentionEvent: string | null;
  attentionChannel: string | null;
  attentionLabel: string | null;
  attentionDetail: string | null;
  attentionAt: number | null;
  fullyIdle: boolean;
  providerSessionId: string | null;
  liveKey: string | null;
  taskRunId: string | null;
  dirtyFiles: number | null;
  dirtyKnown: boolean;
  parallelFloor: boolean;
  diskSessions: number | null;
  sessionId: string | null;
  runRole: "attempt" | "continuation" | null;
  parent: Draft | null;
  activeSubagents: SubagentSummary[];
  approvalChannel: ApprovalChannel | null;
  capabilities: CapabilitySet | null;
  queuedSteeringCount: number;
}

interface Family {
  repoPath: string;
  repoLabel: string;
  diskSessions: number | null;
  floor: boolean;
  listed: number;
}

interface Place {
  family: Family;
  path: string;
  checkout: string;
}

const LIVE_STATUS = new Set(["starting", "running"]);
const LIVE_SUBAGENT_STATUS = new Set(["starting", "running", "working", "needs_you", "waiting"]);
const MAX_TEXT = 160;
const LIVE_DETAIL = "A process this window started.";
const EXITED_DETAIL = "A process this window started. It has exited.";

function clean(value: string, max = MAX_TEXT): string {
  const text = value.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  if (text.length <= max) return text;
  return `${text.slice(0, max - 1)}…`;
}

function keyOf(path: string, paths: PathIdentityOptions): string {
  return identityKey(path, paths) || path;
}

function knownCwd(
  terminal: { cwd: string | null },
  paths?: PathIdentityOptions,
): string | null {
  const cwd = terminal.cwd?.trim() ?? "";
  if (!cwd) return null;
  if (paths && !identityKey(cwd, paths)) return null;
  return cwd;
}

export function terminalInScope(terminal: {
  label: string;
  repoPath: string;
  cwd: string | null;
}): boolean {
  if (agentKind(knownCwd(terminal) ?? terminal.repoPath)) return true;
  const label = terminal.label.trim().toLowerCase();
  return label !== "" && label !== "shell";
}

export function liveAgentCount(
  records: readonly {
    key?: string;
    label: string;
    status: string;
    repoPath: string;
    sessionId?: string | null;
  }[],
  directories: ReadonlyMap<string, string> = new Map(),
): number {
  const seen = new Set<string>();
  let count = 0;
  for (const record of records) {
    if (!LIVE_STATUS.has(record.status)) continue;
    const cwd = record.sessionId ? (directories.get(record.sessionId) ?? null) : null;
    if (!terminalInScope({ label: record.label, repoPath: record.repoPath, cwd })) continue;
    if (record.key) {
      if (seen.has(record.key)) continue;
      seen.add(record.key);
    }
    count += 1;
  }
  return count;
}

export function uniqueRowId(base: string, used: ReadonlySet<string>): string {
  if (!used.has(base)) return base;
  let suffix = 1;
  while (used.has(`${base}|${suffix}`)) suffix += 1;
  return `${base}|${suffix}`;
}

function checkoutLabel(item: PlaneWorktreeSummary): string {
  if (item.branch) return clean(item.branch, 80);
  if (item.is_detached) return "Detached HEAD";
  return "Branch unread";
}

function orderReasons(reasons: Set<AttentionReason>): AttentionReason[] {
  return [...reasons].sort(
    (a, b) => (ATTENTION_RANK.get(a) ?? 99) - (ATTENTION_RANK.get(b) ?? 99),
  );
}

const TERMINAL_EVENT_REASONS = new Set<AttentionReason>([
  "needs-you",
  "error",
  "finished",
]);

function attentionText(reasons: readonly AttentionReason[]): string {
  if (reasons.length === 0) return "";
  const [first, ...rest] = reasons;
  const label = REASON_LABEL[first];
  return rest.length === 0 ? label : `${label} · +${rest.length}`;
}

function rowAttentionLabel(reasons: readonly AttentionReason[], draft: Draft): string {
  if (reasons.length === 0) return "";
  const [first, ...rest] = reasons;
  const label =
    TERMINAL_EVENT_REASONS.has(first) &&
    first === draft.terminalPrimaryReason &&
    draft.attentionLabel
      ? draft.attentionLabel
      : REASON_LABEL[first];
  return rest.length === 0 ? label : `${label} · +${rest.length}`;
}

function wantsAttention(reasons: readonly AttentionReason[]): boolean {
  return reasons.some((reason) => FILTER_ATTENTION.has(reason));
}

function asksForAttention(reasons: readonly AttentionReason[]): boolean {
  return reasons.some(
    (reason) => FILTER_ATTENTION.has(reason) && !UNREAD_REASONS.has(reason),
  );
}

function urgentRank(reasons: readonly AttentionReason[]): number {
  if (reasons.length === 0) return 4;
  if (asksForAttention(reasons)) return 0;
  if (reasons.some((reason) => NOTIFICATION_REASONS.has(reason))) return 1;
  if (reasons.some((reason) => UNREAD_REASONS.has(reason))) return 2;
  return 3;
}

function probeRank(item: PlaneProbe): number {
  if (!item.skipped && item.error === "" && item.snapshot?.worktrees.ok === true) return 2;
  if (!item.skipped) return 1;
  return 0;
}

function collapseProbes(
  probes: readonly PlaneProbe[],
  paths: PathIdentityOptions,
): PlaneProbe[] {
  const by = new Map<string, PlaneProbe>();
  const order: string[] = [];
  for (const probe of probes) {
    const key = keyOf(probe.path, paths);
    const previous = by.get(key);
    if (!previous) {
      by.set(key, probe);
      order.push(key);
      continue;
    }
    if (probeRank(probe) > probeRank(previous)) by.set(key, probe);
  }
  return order.map((key) => by.get(key)!);
}

function collisionPaths(
  snapshot: PlaneInsightsSnapshot,
  paths: PathIdentityOptions,
): Set<string> | null {
  const facet = snapshot.collisions;
  if (
    !facet.ok ||
    facet.unscanned_worktrees > 0 ||
    facet.failed_worktrees > 0 ||
    facet.truncated
  ) {
    return null;
  }
  const found = new Set<string>();
  for (const item of facet.items) {
    for (const party of item.worktrees) {
      const key = keyOf(party.path, paths);
      if (key) found.add(key);
    }
  }
  return found;
}

function checkoutReasons(
  item: PlaneWorktreeSummary,
  collisions: Set<string> | null,
  paths: PathIdentityOptions,
): Set<AttentionReason> {
  const reasons = new Set<AttentionReason>();
  if (collisions === null) reasons.add("unscanned");
  else if (collisions.has(keyOf(item.path, paths))) reasons.add("collision");
  if (!item.operation_ok) reasons.add("unprobed");
  else if (item.operation_kind) reasons.add("blocked");
  if (item.dirty_files === null) reasons.add("unmeasured");
  else if (item.dirty_files > 0) reasons.add("dirty");
  return reasons;
}

function terminalReasons(
  terminal: PlaneTerminal,
  bellAsks: boolean,
): AttentionReason[] {
  if (terminal.attention === "needs-you") {
    if (
      terminal.attentionUrgency === "informational" ||
      terminal.attentionEvent === "idle_prompt"
    ) {
      return ["idle-waiting"];
    }
    return ["needs-you"];
  }
  if (terminal.attention === "error") return ["error"];
  if (terminal.attention === "signalled") {
    if (asksForReader("signalled", bellAsks)) return ["signalled"];
    const channel = terminal.attentionChannel?.toLowerCase();
    if ((channel && channel !== "bell") || Boolean(terminal.attentionDetail)) {
      return ["notice"];
    }
    return [];
  }
  if (terminal.attention === "finished") {
    return terminal.attentionEvent === "idle_prompt"
      ? ["idle-waiting"]
      : ["finished"];
  }
  return [];
}

function taskReasons(task: PlaneTask, bellAsks: boolean): AttentionReason[] {
  const reasons: AttentionReason[] = [];
  if (task.tone === "needs-you") reasons.push("needs-you");
  if (task.tone === "error" || (task.tone === "problem" && !task.disconnected)) {
    reasons.push("error");
  }
  if (task.tone === "signalled" && asksForReader("signalled", bellAsks)) {
    reasons.push("signalled");
  }
  if (task.tone === "finished") reasons.push("finished");
  if (task.pendingCount > 0 || task.pendingMore) reasons.push("pending");
  if (task.disconnected) reasons.push("disconnected");
  if (task.unstarted) reasons.push("unstarted");
  return reasons;
}

function keepTerminal(terminal: PlaneTerminal, bellAsks: boolean): boolean {
  if (!terminal.key) return false;
  if (LIVE_STATUS.has(terminal.status)) return true;
  return terminalReasons(terminal, bellAsks).some(
    (reason) => !NOTIFICATION_REASONS.has(reason),
  );
}

function deriveTerminalUrgency(
  terminal: PlaneTerminal,
  bellAsks: boolean,
): SessionAttentionUrgency | null {
  if (terminal.attention === "signalled" && asksForReader("signalled", bellAsks)) {
    return "blocking";
  }
  if (terminal.attentionUrgency) return terminal.attentionUrgency;
  if (terminal.attentionEvent === "idle_prompt") return "informational";
  if (terminal.attention === "needs-you") return "blocking";
  if (terminal.attention === "error") return "error";
  if (terminal.attention === "finished") return "informational";
  if (terminal.attention === "signalled") {
    const channel = terminal.attentionChannel?.toLowerCase();
    if ((channel && channel !== "bell") || Boolean(terminal.attentionDetail)) {
      return "informational";
    }
  }
  return null;
}

function attach(draft: Draft, terminal: PlaneTerminal, bellAsks: boolean): void {
  const live = LIVE_STATUS.has(terminal.status);
  draft.presence = live ? "live" : "exited";
  draft.presenceDetail = live ? LIVE_DETAIL : EXITED_DETAIL;
  draft.liveKey = terminal.key;
  draft.sessionId = terminal.sessionId || null;
  if (terminal.launcher) {
    draft.launcher = clean(terminal.launcher, 40) || draft.launcher;
  } else if (!draft.launcher && terminal.label) {
    draft.launcher = clean(terminal.label, 40) || null;
  }
  if (terminal.taskRunId) {
    draft.taskRunId = terminal.taskRunId;
    draft.runRole = "attempt";
  } else if (terminal.continuesRunId) {
    if (!draft.taskRunId) draft.taskRunId = terminal.continuesRunId;
    draft.runRole = "continuation";
  }
  const reasons = terminalReasons(terminal, bellAsks);
  for (const reason of reasons) draft.attention.add(reason);
  draft.terminalPrimaryReason = reasons[0] ?? null;
  if (reasons.length > 0) {
    draft.attentionUrgency = deriveTerminalUrgency(terminal, bellAsks);
    draft.attentionEvent = terminal.attentionEvent ?? null;
    draft.attentionChannel = terminal.attentionChannel ?? null;
    draft.attentionLabel = terminal.attentionLabel
      ? clean(terminal.attentionLabel, 80)
      : null;
    draft.attentionDetail = terminal.attentionDetail
      ? clean(terminal.attentionDetail, 240)
      : null;
    draft.attentionAt =
      typeof terminal.attentionAt === "number" &&
      Number.isFinite(terminal.attentionAt) &&
      terminal.attentionAt > 0 &&
      terminal.attentionAt <= 8.64e15
        ? terminal.attentionAt
        : null;
  }
  draft.fullyIdle = Boolean(terminal.fullyIdle);
  draft.providerSessionId = terminal.providerSessionId ?? null;
  if (terminal.activeSubagents && terminal.activeSubagents.length > 0) {
    draft.activeSubagents = [...terminal.activeSubagents];
  }
  if (terminal.capabilities) {
    draft.capabilities = { ...terminal.capabilities };
    draft.approvalChannel = terminal.capabilities.approval_channel;
  } else if (terminal.approvalChannel) {
    draft.approvalChannel = terminal.approvalChannel;
  }
  if (typeof terminal.queuedSteeringCount === "number" && terminal.queuedSteeringCount > 0) {
    draft.queuedSteeringCount = terminal.queuedSteeringCount;
  }
  const title = clean(terminal.title || terminal.label, 80);
  if (title && draft.session === draft.kind) draft.session = title;
  else if (title && !draft.session) draft.session = title;
}

function fromCheckout(
  family: Family,
  item: PlaneWorktreeSummary,
  collisions: Set<string> | null,
  paths: PathIdentityOptions,
): Draft {
  const kind = clean(item.agent_kind, 40);
  return {
    repoPath: family.repoPath,
    repoLabel: family.repoLabel,
    kind,
    launcher: kind || null,
    session: clean(item.session_slug, 80),
    checkout: checkoutLabel(item),
    worktreePath: item.path,
    checkoutPath: item.path,
    origin: "checkout",
    presence: "on-disk",
    presenceDetail: "A checkout on disk. This does not say a process is running.",
    attention: checkoutReasons(item, collisions, paths),
    terminalPrimaryReason: null,
    attentionUrgency: null,
    attentionEvent: null,
    attentionChannel: null,
    attentionLabel: null,
    attentionDetail: null,
    attentionAt: null,
    fullyIdle: false,
    providerSessionId: null,
    liveKey: null,
    taskRunId: null,
    dirtyFiles: item.dirty_files,
    dirtyKnown: item.dirty_files !== null,
    parallelFloor: family.floor,
    diskSessions: family.diskSessions,
    sessionId: null,
    runRole: null,
    parent: null,
    activeSubagents: [],
    approvalChannel: null,
    capabilities: null,
    queuedSteeringCount: 0,
  };
}

function fromTerminal(
  terminal: PlaneTerminal,
  cwd: string | null,
  places: ReadonlyMap<string, Place>,
  paths: PathIdentityOptions,
): Draft {
  const where = cwd ?? terminal.repoPath;
  const place = nearestContaining(places, where, paths);
  const family =
    place?.family ?? nearestContaining(places, terminal.repoPath, paths)?.family;
  const kind = agentKind(where);
  const checkout = kind
    ? "Agent checkout"
    : place
      ? place.checkout
      : cwd
        ? "Outside the repositories read"
        : "Main worktree";
  const resolvedKind = clean(kind || terminal.label, 40);
  return {
    repoPath: family?.repoPath || terminal.repoPath,
    repoLabel: family?.repoLabel || clean(terminal.repoPath, 80),
    kind: resolvedKind,
    launcher: clean(terminal.launcher ?? resolvedKind, 40) || null,
    session: clean(terminal.title || terminal.label, 80),
    checkout,
    worktreePath: where,
    checkoutPath: place?.path ?? (cwd ? null : terminal.repoPath),
    origin: "terminal",
    presence: "live",
    presenceDetail: LIVE_DETAIL,
    attention: new Set(),
    terminalPrimaryReason: null,
    attentionUrgency: null,
    attentionEvent: null,
    attentionChannel: null,
    attentionLabel: null,
    attentionDetail: null,
    attentionAt: null,
    fullyIdle: false,
    providerSessionId: null,
    liveKey: null,
    taskRunId: null,
    dirtyFiles: null,
    dirtyKnown: false,
    parallelFloor: false,
    diskSessions: null,
    sessionId: null,
    runRole: null,
    parent: null,
    activeSubagents: [],
    approvalChannel: null,
    capabilities: null,
    queuedSteeringCount: 0,
  };
}

function foldTask(draft: Draft, task: PlaneTask, bellAsks: boolean): void {
  draft.taskRunId = task.runId;
  const provider = clean(task.provider, 40);
  if (provider) {
    draft.kind = provider;
    if (!draft.launcher) draft.launcher = provider;
  }
  const title = clean(task.title, 80);
  if (title) draft.session = title;
  if (task.capabilities && !draft.capabilities) {
    draft.capabilities = { ...task.capabilities };
    draft.approvalChannel = task.capabilities.approval_channel;
  } else if (task.approvalChannel && !draft.approvalChannel) {
    draft.approvalChannel = task.approvalChannel;
  }
  for (const reason of taskReasons(task, bellAsks)) {
    if (reason === "disconnected" && draft.liveKey !== null) continue;
    if (
      reason === "finished" &&
      draft.liveKey !== null &&
      draft.attention.has("idle-waiting")
    ) {
      continue;
    }
    draft.attention.add(reason);
  }
}

function fromTask(
  task: PlaneTask,
  places: ReadonlyMap<string, Place>,
  paths: PathIdentityOptions,
  bellAsks: boolean,
): Draft {
  const family =
    nearestContaining(places, task.repoPath, paths)?.family ??
    nearestContaining(places, task.cwd, paths)?.family;
  const repoPath = family?.repoPath || task.repoPath;
  const kind = clean(task.provider, 40);
  return {
    repoPath,
    repoLabel:
      family?.repoLabel || (repoPath ? clean(repoPath, 80) : "Repository not resolved"),
    kind,
    launcher: kind || null,
    session: clean(task.title, 80),
    checkout: clean(task.cwd, 80),
    worktreePath: task.cwd,
    checkoutPath: task.cwd || null,
    origin: "task",
    presence: "missing",
    presenceDetail:
      "The workbench has this run, and no terminal in this window is attached to it.",
    attention: new Set(taskReasons(task, bellAsks)),
    terminalPrimaryReason: null,
    attentionUrgency: null,
    attentionEvent: null,
    attentionChannel: null,
    attentionLabel: null,
    attentionDetail: null,
    attentionAt: null,
    fullyIdle: false,
    providerSessionId: null,
    liveKey: null,
    taskRunId: task.runId,
    dirtyFiles: null,
    dirtyKnown: false,
    parallelFloor: false,
    diskSessions: null,
    sessionId: null,
    runRole: null,
    parent: null,
    activeSubagents: [],
    approvalChannel: task.capabilities?.approval_channel ?? task.approvalChannel ?? null,
    capabilities: task.capabilities ? { ...task.capabilities } : null,
    queuedSteeringCount: 0,
  };
}

function completeRead(snapshot: PlaneInsightsSnapshot): boolean {
  return (
    snapshot.agents.ok && !snapshot.agents.truncated && !snapshot.worktrees.truncated
  );
}

const EMPTY_ROLLUP: AgentRollup = Object.freeze({
  descendants: 0,
  live: 0,
  attention: [] as AttentionReason[],
  unobserved: 0,
  subagents: 0,
  liveSubagents: 0,
});

function compareRows(a: AgentRow, b: AgentRow): number {
  return (
    urgentRank(a.attention) - urgentRank(b.attention) ||
    a.repoLabel.localeCompare(b.repoLabel) ||
    a.kind.localeCompare(b.kind) ||
    a.session.localeCompare(b.session) ||
    a.id.localeCompare(b.id)
  );
}

interface LiveProcess {
  pid: number;
  kind: string;
  tree: string | null;
  parentPid: number | null;
  cwd: string;
  status: string;
}

const MAX_LINEAGE = 32;

function collectProcesses(
  snapshots: readonly PlaneInsightsSnapshot[],
): Map<number, LiveProcess> {
  const found = new Map<number, LiveProcess>();
  for (const snapshot of snapshots) {
    for (const worktree of snapshot.agents.live.worktrees) {
      for (const session of worktree.sessions) {
        if (found.has(session.pid)) continue;
        found.set(session.pid, {
          pid: session.pid,
          kind: session.kind,
          tree: session.tree ?? null,
          parentPid: session.parent_pid ?? null,
          cwd: session.cwd,
          status: session.status,
        });
      }
    }
  }
  return found;
}

function subagentAttentionReasons(subagent: SubagentSummary): AttentionReason[] {
  const status = subagent.status.trim().toLowerCase();
  if (status === "needs_you" || status === "needs-you" || status === "approval") {
    return ["needs-you"];
  }
  if (status === "error" || status === "failed") {
    return ["error"];
  }
  return [];
}

function subagentRow(id: string, sub: SubagentSummary, above: AgentRow): AgentRow {
  const kind = clean(sub.kind, 40) || above.kind || "agent";
  const status = clean(sub.status, 40);
  const live = LIVE_SUBAGENT_STATUS.has(status.toLowerCase());
  const attention = subagentAttentionReasons(sub);
  const caps = defaultCapabilitiesForHarness(kind, {
    approvalChannel: above.approvalChannel,
  });
  const desc = describeControlCapabilities(caps);
  return {
    id,
    parentId: above.id,
    children: [],
    rollup: EMPTY_ROLLUP,
    attentionObserved: true,
    repoPath: above.repoPath,
    repoLabel: above.repoLabel,
    kind,
    launcher: kind,
    session: clean(sub.label || `Sub-agent · ${sub.agent_id}`, 80),
    checkout: above.checkout,
    worktreePath: above.worktreePath,
    checkoutPath: above.checkoutPath,
    origin: "subagent",
    presence: live ? "live" : "exited",
    presenceDetail: `Active sub-agent (${sub.agent_id}) reported under this session.${status ? ` Status: ${status}.` : ""}`,
    attention,
    attentionLabel: attentionText(attention),
    attentionUrgency:
      attention[0] === "needs-you"
        ? "blocking"
        : attention[0] === "error"
          ? "error"
          : null,
    attentionEvent: null,
    attentionChannel: null,
    attentionDetail: sub.label ?? null,
    attentionAt: sub.updated_at_ms || null,
    sessionId: sub.agent_id,
    fullyIdle: status.toLowerCase() === "idle",
    providerSessionId: null,
    parallelCount: above.parallelCount,
    parallelFloor: above.parallelFloor,
    dirtyFiles: null,
    dirtyKnown: false,
    liveKey: null,
    taskRunId: null,
    outsideSessions: 0,
    activeSubagents: [],
    approvalChannel: caps.approval_channel,
    capabilities: caps,
    capabilityBadges: desc.badges,
    ungated: desc.isUngated,
    queuedSteeringCount: 0,
  };
}

function childSessionRow(id: string, process: LiveProcess, above: AgentRow): AgentRow {
  const status = clean(process.status, 40);
  const kind = clean(process.kind, 40) || "agent";
  const caps = defaultCapabilitiesForHarness(kind);
  const desc = describeControlCapabilities(caps);
  return {
    id,
    parentId: above.id,
    children: [],
    rollup: EMPTY_ROLLUP,
    attentionObserved: false,
    repoPath: above.repoPath,
    repoLabel: above.repoLabel,
    kind,
    launcher: kind,
    session: `Child session · pid ${process.pid}`,
    checkout: above.checkout,
    worktreePath: process.cwd,
    checkoutPath: above.checkoutPath,
    origin: "session",
    presence: "live",
    presenceDetail: `A child agent process the process table shows under this session. This window did not start it.${status ? ` The agent reports it as ${status}.` : ""}`,
    attention: [],
    attentionLabel: "",
    attentionUrgency: null,
    attentionEvent: null,
    attentionChannel: null,
    attentionDetail: null,
    attentionAt: null,
    sessionId: null,
    fullyIdle: false,
    providerSessionId: null,
    parallelCount: above.parallelCount,
    parallelFloor: above.parallelFloor,
    dirtyFiles: null,
    dirtyKnown: false,
    liveKey: null,
    taskRunId: null,
    outsideSessions: 0,
    activeSubagents: [],
    approvalChannel: caps.approval_channel,
    capabilities: caps,
    capabilityBadges: desc.badges,
    ungated: desc.isUngated,
    queuedSteeringCount: 0,
  };
}

function placeSessions(
  processes: ReadonlyMap<number, LiveProcess>,
  rows: readonly AgentRow[],
  draftOf: ReadonlyMap<AgentRow, Draft>,
  checkouts: ReadonlyMap<string, Draft>,
  rowOf: ReadonlyMap<Draft, AgentRow>,
  paths: PathIdentityOptions,
  used: Set<string>,
): { placed: number; unplaced: number } {
  const kept = new Set(rows);
  const byTree = new Map<string, AgentRow>();
  for (const row of rows) {
    const draft = draftOf.get(row);
    if (!draft) continue;
    if (draft.sessionId && !byTree.has(`session:${draft.sessionId}`)) {
      byTree.set(`session:${draft.sessionId}`, row);
    }
    if (
      draft.taskRunId &&
      draft.runRole !== "continuation" &&
      !byTree.has(`run:${draft.taskRunId}`)
    ) {
      byTree.set(`run:${draft.taskRunId}`, row);
    }
  }
  const ownerOf = (process: LiveProcess): AgentRow | null => {
    const launched = process.tree ? byTree.get(process.tree) : undefined;
    if (launched) return launched;
    const host = nearestContaining(checkouts, process.cwd, paths);
    const row = host ? rowOf.get(host) : undefined;
    return row && kept.has(row) ? row : null;
  };
  const made = new Map<number, AgentRow | null>();
  const containerOf = (
    process: LiveProcess,
    trail: Set<number>,
  ): AgentRow | null => {
    const memo = made.get(process.pid);
    if (memo !== undefined) return memo;
    if (process.parentPid === null) return ownerOf(process);
    const parent = processes.get(process.parentPid);
    if (!parent) {
      made.set(process.pid, null);
      return null;
    }
    if (trail.has(process.pid) || trail.size >= MAX_LINEAGE) {
      made.set(process.pid, null);
      return null;
    }
    trail.add(process.pid);
    const above = containerOf(parent, trail);
    trail.delete(process.pid);
    if (!above) {
      made.set(process.pid, null);
      return null;
    }
    const id = uniqueRowId(`${above.id}|session|${process.pid}`, used);
    used.add(id);
    const row = childSessionRow(id, process, above);
    above.children.push(row);
    made.set(process.pid, row);
    return row;
  };
  let placed = 0;
  let unplaced = 0;
  for (const process of [...processes.values()].sort((a, b) => a.pid - b.pid)) {
    if (process.parentPid === null) continue;
    if (containerOf(process, new Set()) === null) unplaced += 1;
    else placed += 1;
  }
  return { placed, unplaced };
}

function countOutsideSessions(
  processes: ReadonlyMap<number, LiveProcess>,
  checkouts: ReadonlyMap<string, Draft>,
  rowOf: ReadonlyMap<Draft, AgentRow>,
  kept: ReadonlySet<AgentRow>,
  paths: PathIdentityOptions,
): void {
  for (const process of processes.values()) {
    if (process.parentPid !== null || process.tree !== null) continue;
    const host = nearestContaining(checkouts, process.cwd, paths);
    const row = host ? rowOf.get(host) : undefined;
    if (!row || !kept.has(row)) continue;
    row.outsideSessions += 1;
  }
  for (const row of kept) {
    if (row.outsideSessions === 0 || row.presence !== "on-disk") continue;
    row.presenceDetail = `A checkout on disk. The process table shows ${plural(row.outsideSessions, "agent process", "agent processes")} running in it that no terminal or task run here started.`;
  }
}

function asks(reason: AttentionReason): boolean {
  return FILTER_ATTENTION.has(reason) && !UNREAD_REASONS.has(reason);
}

function settleRollups(row: AgentRow): AgentRollup {
  let descendants = 0;
  let live = 0;
  let unobserved = 0;
  let subagents = 0;
  let liveSubagents = 0;
  const reasons = new Set<AttentionReason>();
  for (const child of row.children) {
    const below = settleRollups(child);
    descendants += 1 + below.descendants;
    live += (child.presence === "live" ? 1 : 0) + below.live;
    unobserved += (child.attentionObserved ? 0 : 1) + below.unobserved;
    subagents += (child.origin === "subagent" ? 1 : 0) + below.subagents;
    liveSubagents +=
      (child.origin === "subagent" && child.presence === "live" ? 1 : 0) +
      below.liveSubagents;
    for (const reason of child.attention) if (asks(reason)) reasons.add(reason);
    for (const reason of below.attention) reasons.add(reason);
  }
  row.rollup =
    descendants === 0
      ? EMPTY_ROLLUP
      : {
          descendants,
          live,
          attention: orderReasons(reasons),
          unobserved,
          subagents,
          liveSubagents,
        };
  return row.rollup;
}

export function projectAgentPlane(input: PlaneInput): AgentPlane {
  const paths = input.paths;
  const bellAsks = Boolean(input.bellAsks);
  const probes = collapseProbes(input.probes, paths);
  const gaps: ProbeGap[] = [];
  const drafts: Draft[] = [];
  let sessionsAreFloor = false;
  let read = 0;
  let failed = 0;
  let skipped = 0;

  const readProbes: {
    probe: PlaneProbe;
    snapshot: PlaneInsightsSnapshot;
    family: string;
  }[] = [];

  for (const probe of probes) {
    const label = clean(probe.label || probe.path, 80);
    if (probe.skipped) {
      skipped += 1;
      gaps.push({
        repoPath: probe.path,
        label,
        kind: "skipped",
        reason:
          probe.skipReason === "cap"
            ? `Not read: the sweep stops at ${MAX_AGENT_REPOS} repositories.`
            : "Not read: the sweep ran out of time.",
      });
      continue;
    }
    if (probe.error || !probe.snapshot) {
      failed += 1;
      gaps.push({
        repoPath: probe.path,
        label,
        kind: "failed",
        reason: clean(probe.error || "The repository couldn't be read.", 240),
      });
      continue;
    }
    const snapshot = probe.snapshot;
    if (!snapshot.worktrees.ok) {
      failed += 1;
      gaps.push({
        repoPath: probe.path,
        label,
        kind: "failed",
        reason: clean(
          snapshot.worktrees.error || "Worktrees couldn't be listed.",
          240,
        ),
      });
      continue;
    }
    read += 1;
    const main = snapshot.worktrees.items.find((item) => item.is_main);
    readProbes.push({
      probe,
      snapshot,
      family: keyOf(main?.path || probe.path, paths),
    });
  }

  const members = new Map<string, typeof readProbes>();
  for (const entry of readProbes) {
    const list = members.get(entry.family);
    if (list) list.push(entry);
    else members.set(entry.family, [entry]);
  }

  const places = new Map<string, Place>();
  const checkouts = new Map<string, Draft>();
  const families: Family[] = [];

  for (const [familyKey, entries] of members) {
    const named =
      entries.find((entry) => keyOf(entry.probe.path, paths) === familyKey) ??
      entries[0];
    const complete = entries.filter((entry) => completeRead(entry.snapshot));
    const counted = entries.filter((entry) => entry.snapshot.agents.ok);
    const family: Family = {
      repoPath: named.probe.path,
      repoLabel: clean(named.probe.label || named.probe.path, 80),
      diskSessions:
        counted.length === 0
          ? null
          : Math.max(...counted.map((entry) => entry.snapshot.agents.sessions)),
      floor: complete.length === 0,
      listed: 0,
    };
    families.push(family);
    if (family.floor) sessionsAreFloor = true;
    if (counted.length === 0) {
      gaps.push({
        repoPath: family.repoPath,
        label: family.repoLabel,
        kind: "partial",
        reason:
          "Agent counts couldn't be read. The checkouts below are only the ones whose paths were listed.",
      });
    } else if (family.floor) {
      gaps.push({
        repoPath: family.repoPath,
        label: family.repoLabel,
        kind: "partial",
        reason:
          "The worktree list was capped. Session counts for this repository are a floor.",
      });
    }
    let collisions: Set<string> | null = null;
    for (const entry of entries) {
      collisions = collisionPaths(entry.snapshot, paths);
      if (collisions !== null) break;
    }
    if (collisions === null) {
      gaps.push({
        repoPath: family.repoPath,
        label: family.repoLabel,
        kind: "partial",
        reason: named.snapshot.collisions.ok
          ? "Collision scan did not cover every worktree. An empty overlap list is not a clear one."
          : clean(
              named.snapshot.collisions.error || "Collisions couldn't be read.",
              240,
            ),
      });
    }
    for (const entry of entries) {
      const probeKey = keyOf(entry.probe.path, paths);
      if (!places.has(probeKey)) {
        places.set(probeKey, {
          family,
          path: entry.probe.path,
          checkout: "Checkout",
        });
      }
      for (const item of entry.snapshot.worktrees.items) {
        const key = keyOf(item.path, paths);
        if (!key) continue;
        const known = places.get(key);
        if (!known || known.checkout === "Checkout") {
          places.set(key, {
            family,
            path: item.path,
            checkout: item.is_main ? "Main worktree" : checkoutLabel(item),
          });
        }
        if (!item.agent_kind || checkouts.has(key)) continue;
        const draft = fromCheckout(family, item, collisions, paths);
        checkouts.set(key, draft);
        drafts.push(draft);
        family.listed += 1;
      }
    }
  }

  const chosen = new Map<string, PlaneTerminal>();
  for (const terminal of input.terminals) {
    if (!keepTerminal(terminal, bellAsks) || !terminalInScope(terminal)) continue;
    const previous = chosen.get(terminal.key);
    if (
      !previous ||
      (!LIVE_STATUS.has(previous.status) && LIVE_STATUS.has(terminal.status))
    ) {
      chosen.set(terminal.key, terminal);
    }
  }

  const checkoutAttention = new Map<Draft, ReadonlySet<AttentionReason>>();
  let live = 0;
  for (const terminal of chosen.values()) {
    if (LIVE_STATUS.has(terminal.status)) live += 1;
    const cwd = knownCwd(terminal, paths);
    const host = nearestContaining(checkouts, cwd ?? terminal.repoPath, paths);
    if (host && host.liveKey === null) {
      checkoutAttention.set(host, new Set(host.attention));
      attach(host, terminal, bellAsks);
      continue;
    }
    const draft: Draft = host
      ? {
          ...host,
          origin: "terminal",
          attention: new Set(checkoutAttention.get(host) ?? host.attention),
          terminalPrimaryReason: null,
          attentionUrgency: null,
          attentionEvent: null,
          attentionChannel: null,
          attentionLabel: null,
          attentionDetail: null,
          attentionAt: null,
          fullyIdle: false,
          providerSessionId: null,
          liveKey: null,
          taskRunId: null,
          sessionId: null,
          runRole: null,
          parent: null,
          activeSubagents: [],
          approvalChannel: null,
          capabilities: null,
          queuedSteeringCount: 0,
        }
      : fromTerminal(terminal, cwd, places, paths);
    attach(draft, terminal, bellAsks);
    if (host) draft.session = clean(terminal.title || terminal.label || host.session, 80);
    drafts.push(draft);
  }

  let taskAttempts: number | null = null;
  let tasksFloor = false;
  if (input.tasks === null) {
    // Caller not watching attempts.
  } else if (input.tasks.unread) {
    gaps.push({
      repoPath: "",
      label: "Tasks",
      kind: "unread",
      reason: "Task runs have not been read yet.",
    });
  } else if (!input.tasks.ok) {
    gaps.push({
      repoPath: "",
      label: "Tasks",
      kind: "failed",
      reason: clean(input.tasks.error || "Task runs couldn't be read.", 240),
    });
  } else {
    if (!input.tasks.complete) {
      sessionsAreFloor = true;
      tasksFloor = true;
      gaps.push({
        repoPath: "",
        label: "Tasks",
        kind: "partial",
        reason: "The task run list was capped. Counts of runs are a floor.",
      });
    }
    const byRun = new Map<string, Draft>();
    for (const draft of drafts) {
      if (draft.taskRunId && !byRun.has(draft.taskRunId)) {
        byRun.set(draft.taskRunId, draft);
      }
    }
    const attempts = new Set<string>();
    for (const task of input.tasks.tasks) {
      if (!task.runId) continue;
      attempts.add(task.runId);
      const existing = byRun.get(task.runId);
      if (existing) {
        foldTask(existing, task, bellAsks);
        continue;
      }
      const host = nearestContaining(checkouts, task.cwd, paths);
      if (host && host.liveKey === null && host.taskRunId === null) {
        foldTask(host, task, bellAsks);
        byRun.set(task.runId, host);
        continue;
      }
      if (taskReasons(task, bellAsks).length === 0) continue;
      const draft = fromTask(task, places, paths, bellAsks);
      drafts.push(draft);
      byRun.set(task.runId, draft);
    }
    taskAttempts = attempts.size;
  }

  const attemptRows = new Map<string, Draft>();
  for (const draft of drafts) {
    if (
      draft.taskRunId &&
      draft.runRole !== "continuation" &&
      !attemptRows.has(draft.taskRunId)
    ) {
      attemptRows.set(draft.taskRunId, draft);
    }
  }
  for (const draft of drafts) {
    if (
      draft.runRole !== "continuation" ||
      draft.origin !== "terminal" ||
      !draft.taskRunId
    ) {
      continue;
    }
    const host = attemptRows.get(draft.taskRunId);
    if (host && host !== draft) draft.parent = host;
  }

  const groups = new Map<
    string,
    { size: number; disk: number | null; floor: boolean }
  >();
  const groupKeys = drafts.map((draft, index) =>
    draft.repoPath ? keyOf(draft.repoPath, paths) : `\u0000${index}`,
  );
  drafts.forEach((draft, index) => {
    const group = groups.get(groupKeys[index]) ?? {
      size: 0,
      disk: null,
      floor: false,
    };
    group.size += 1;
    if (draft.diskSessions !== null) {
      group.disk = Math.max(group.disk ?? 0, draft.diskSessions);
    }
    group.floor = group.floor || draft.parallelFloor;
    groups.set(groupKeys[index], group);
  });

  const finished: AgentRow[] = [];
  const used = new Set<string>();
  const draftOf = new Map<AgentRow, Draft>();
  const rowOf = new Map<Draft, AgentRow>();
  let needing = 0;
  let unread = 0;
  let notifications = 0;

  drafts.forEach((draft, index) => {
    const group = groups.get(groupKeys[index])!;
    const parallelFloor =
      group.floor || (group.disk !== null && group.disk > group.size);
    const parallelCount =
      group.disk === null ? group.size : Math.max(group.size, group.disk);
    if (parallelFloor) sessionsAreFloor = true;
    const attention = orderReasons(draft.attention);
    if (asksForAttention(attention)) needing += 1;
    else if (wantsAttention(attention)) unread += 1;
    if (attention.some((reason) => NOTIFICATION_REASONS.has(reason))) {
      notifications += 1;
    }
    const id = uniqueRowId(
      [
        keyOf(draft.repoPath, paths),
        keyOf(draft.worktreePath, paths),
        draft.liveKey ?? "",
        draft.taskRunId ?? "",
      ].join("|"),
      used,
    );
    used.add(id);

    const resolvedKind = draft.kind || "agent";
    const caps =
      draft.capabilities ??
      defaultCapabilitiesForHarness(draft.launcher ?? resolvedKind, {
        approvalChannel: draft.approvalChannel ?? undefined,
      });
    const desc = describeControlCapabilities(caps, {
      fullyIdle: draft.fullyIdle,
      queuedSteeringCount: draft.queuedSteeringCount,
    });

    const row: AgentRow = {
      id,
      parentId: null,
      children: [],
      rollup: EMPTY_ROLLUP,
      attentionObserved: true,
      repoPath: draft.repoPath,
      repoLabel: draft.repoLabel || clean(draft.repoPath, 80),
      kind: resolvedKind,
      launcher: draft.launcher,
      session: draft.session || resolvedKind,
      checkout: draft.checkout,
      worktreePath: draft.worktreePath,
      checkoutPath: draft.checkoutPath,
      origin: draft.origin,
      presence: draft.presence,
      presenceDetail: draft.presenceDetail,
      attention,
      attentionLabel: rowAttentionLabel(attention, draft),
      attentionUrgency: draft.attentionUrgency,
      attentionEvent: draft.attentionEvent,
      attentionChannel: draft.attentionChannel,
      attentionDetail: draft.attentionDetail,
      attentionAt: draft.attentionAt,
      sessionId: draft.sessionId,
      fullyIdle: draft.fullyIdle,
      providerSessionId: draft.providerSessionId,
      parallelCount,
      parallelFloor,
      dirtyFiles: draft.dirtyKnown ? draft.dirtyFiles : null,
      dirtyKnown: draft.dirtyKnown,
      liveKey: draft.liveKey,
      taskRunId: draft.taskRunId,
      outsideSessions: 0,
      activeSubagents: [...draft.activeSubagents],
      approvalChannel: caps.approval_channel,
      capabilities: caps,
      capabilityBadges: desc.badges,
      ungated: desc.isUngated,
      queuedSteeringCount: draft.queuedSteeringCount,
    };
    finished.push(row);
    draftOf.set(row, draft);
    rowOf.set(draft, row);
  });

  finished.sort(compareRows);

  let checkoutCount = 0;
  let checkoutsFloor = false;
  for (const family of families) {
    checkoutCount += Math.max(family.listed, family.diskSessions ?? 0);
    if (family.floor) checkoutsFloor = true;
  }

  const total = finished.length;
  const truncated = total > MAX_AGENT_ROWS;
  const rows = truncated ? finished.slice(0, MAX_AGENT_ROWS) : finished;

  const kept = new Set(rows);
  for (const row of rows) {
    const parent = draftOf.get(row)?.parent;
    const parentRow = parent ? rowOf.get(parent) : undefined;
    if (!parentRow || !kept.has(parentRow)) continue;
    row.parentId = parentRow.id;
    parentRow.children.push(row);
  }

  // Attach promoted active sub-agents directly beneath their parent session row.
  let activeSubagentsCount = 0;
  for (const row of rows) {
    for (const sub of row.activeSubagents) {
      const subId = uniqueRowId(`${row.id}|subagent|${sub.agent_id}`, used);
      used.add(subId);
      row.children.push(subagentRow(subId, sub, row));
      activeSubagentsCount += 1;
    }
  }

  const lineage = collectProcesses(readProbes.map((entry) => entry.snapshot));
  countOutsideSessions(lineage, checkouts, rowOf, kept, paths);
  const placed = placeSessions(
    lineage,
    rows,
    draftOf,
    checkouts,
    rowOf,
    paths,
    used,
  );
  if (placed.unplaced > 0) {
    gaps.push({
      repoPath: "",
      label: "Child sessions",
      kind: "partial",
      reason: `${plural(placed.unplaced, "child agent session", "child agent sessions")} couldn't be placed under a row: the session above ${placed.unplaced === 1 ? "it is" : "them is"} not one of the rows shown, or was not listed by any repository read. Their count is kept; their place is unknown.`,
    });
  }
  for (const row of rows) {
    if (row.parentId === null) settleRollups(row);
  }

  return {
    rows,
    roots: rows.filter((row) => row.parentId === null),
    childSessions: placed.placed,
    unplacedSessions: placed.unplaced,
    activeSubagents: activeSubagentsCount,
    shown: rows.length,
    total,
    truncated,
    gaps,
    sessionsAreFloor: sessionsAreFloor || truncated,
    checkouts: checkoutCount,
    checkoutsFloor,
    live,
    taskAttempts,
    tasksWatched: input.tasks !== null,
    tasksFloor,
    attention: { needing, unread },
    notifications,
    requested: probes.length,
    read,
    failed,
    skipped,
  };
}

function matchesAgentFilter(row: AgentRow, filter: AgentFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "live":
      return row.presence === "live";
    case "parallel":
      return row.parallelCount > 1 || row.parallelFloor;
    case "notifications":
      return row.attention.some((reason) => NOTIFICATION_REASONS.has(reason));
    case "gaps":
      return row.attention.some((reason) => UNREAD_REASONS.has(reason));
    case "attention":
      return wantsAttention(row.attention);
  }
}

export function applyAgentFilter(
  rows: readonly AgentRow[],
  filter: AgentFilter,
): AgentRow[] {
  if (filter === "all") return [...rows];
  return rows.filter((row) => matchesAgentFilter(row, filter));
}

export function applyAgentTree(
  roots: readonly AgentRow[],
  filter: AgentFilter,
): AgentRow[] {
  if (filter === "all") return [...roots];
  const keeps = (row: AgentRow): boolean =>
    matchesAgentFilter(row, filter) || row.children.some(keeps);
  return roots.filter(keeps);
}

export function countAgentRows(roots: readonly AgentRow[]): number {
  let count = 0;
  const walk = (row: AgentRow): void => {
    if (row.origin !== "session" && row.origin !== "subagent") count += 1;
    row.children.forEach(walk);
  };
  roots.forEach(walk);
  return count;
}

export interface AgentTreeEntry {
  row: AgentRow;
  level: number;
  posInSet: number;
  setSize: number;
  expandable: boolean;
  expanded: boolean;
}

export function flattenAgentTree(
  roots: readonly AgentRow[],
  collapsed: ReadonlySet<string>,
): AgentTreeEntry[] {
  const entries: AgentTreeEntry[] = [];
  const walk = (rows: readonly AgentRow[], level: number): void => {
    rows.forEach((row, index) => {
      const expandable = row.children.length > 0;
      const expanded = expandable && !collapsed.has(row.id);
      entries.push({
        row,
        level,
        posInSet: index + 1,
        setSize: rows.length,
        expandable,
        expanded,
      });
      if (expanded) walk(row.children, level + 1);
    });
  };
  walk(roots, 1);
  return entries;
}

export function rollupSummary(row: AgentRow): {
  presence: string;
  attention: string;
} {
  const { descendants, live, attention, unobserved, subagents, liveSubagents } =
    row.rollup;
  if (descendants === 0) return { presence: "", attention: "" };
  const parts: string[] = [];
  if (attention.length > 0) parts.push(`Under it: ${attentionText(attention)}`);
  if (unobserved > 0) parts.push(`${unobserved} not observed`);
  const presenceParts = [`${live} of ${plural(descendants, "nested row", "nested rows")} live`];
  if (subagents > 0) {
    presenceParts.push(
      `${liveSubagents}/${plural(subagents, "sub-agent", "sub-agents")} active`,
    );
  }
  return {
    presence: presenceParts.join(" · "),
    attention: parts.join(" · "),
  };
}

export function scopeToRepository(
  rows: readonly AgentRow[],
  scope: string | null,
  paths: PathIdentityOptions,
): AgentRow[] {
  const key = scope ? identityKey(scope, paths) : null;
  if (!key) return [...rows];
  return rows.filter(
    (row) =>
      (row.repoPath !== "" && identityKey(row.repoPath, paths) === key) ||
      (row.checkoutPath !== null &&
        identityKey(row.checkoutPath, paths) === key),
  );
}

export { plural };

export function planeHeadline(plane: AgentPlane, visible: number): string {
  const parts = [
    `${plane.checkoutsFloor ? "at least " : ""}${plural(plane.checkouts, "agent checkout", "agent checkouts")}`,
    plural(plane.live, "live terminal", "live terminals"),
  ];
  if (plane.tasksWatched) {
    parts.push(
      plane.taskAttempts === null
        ? "task runs not read"
        : `${plane.tasksFloor ? "at least " : ""}${plural(plane.taskAttempts, "task run", "task runs")}`,
    );
  }
  if (plane.activeSubagents > 0) {
    parts.push(plural(plane.activeSubagents, "sub-agent", "sub-agents"));
  }
  if (plane.childSessions > 0) {
    parts.push(plural(plane.childSessions, "child session", "child sessions"));
  }
  parts.push(`${plane.attention.needing} need attention`);
  if (plane.notifications > 0) {
    parts.push(plural(plane.notifications, "notification", "notifications"));
  }
  if (plane.attention.unread > 0) {
    parts.push(`${plane.attention.unread} not fully read`);
  }
  const hidden = plane.rows.length - visible;
  if (hidden > 0) parts.push(`${hidden} hidden by the filter`);
  const notRead = plane.failed + plane.skipped;
  if (notRead > 0) {
    parts.push(`${plural(notRead, "repository", "repositories")} not read`);
  }
  return parts.join(" · ");
}
