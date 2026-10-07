/**
 * The Agents plane: one reading of every session a workspace is holding.
 *
 * Three sources, and they do not say the same thing:
 *
 * - A git worktree under `.<agent>/worktrees/<slug>` is a checkout. It is not
 *   a running process. Insights can see the checkout and cannot see whether
 *   anyone is still in it.
 * - A terminal session is a process this window started. A Claude the person
 *   launched outside GitPulse is absent here, not idle.
 * - A task attempt is a run the workbench recorded. A run whose list could
 *   not be read is not a run that does not exist.
 *
 * The projection refuses to collapse those into one "status". A probe that
 * did not run, a list that was capped, and a checkout whose changes were not
 * measured each stay visible as that fact. Callers render the gaps; they do
 * not treat an empty row list as a quiet workspace.
 */

import type { InsightsSnapshot, WorktreeSummary } from "../insights/types";
import { identityKey, type PathIdentityOptions } from "../repos/paths";
import { agentKind } from "../work/agentWorktree";

/** Repositories one sweep will read. The rest are reported as skipped. */
export const MAX_AGENT_REPOS = 64;
/** Rows one projection will return. `total` still counts the rest. */
export const MAX_AGENT_ROWS = 400;

export const AGENT_COLUMNS = [
  { key: "checkout", label: "Checkout" },
  { key: "presence", label: "Presence" },
  { key: "attention", label: "Attention" },
  { key: "parallel", label: "Parallel" },
  { key: "changes", label: "Changes" },
] as const;

export type AgentColumnKey = (typeof AGENT_COLUMNS)[number]["key"];

export type AgentFilter = "all" | "attention" | "parallel" | "live";

export function isAgentFilter(value: unknown): value is AgentFilter {
  return value === "all" || value === "attention" || value === "parallel" || value === "live";
}

/**
 * Why a row is on the reader's plate, most urgent first when stored on a row.
 *
 * `dirty` is a change, not a request. The attention filter leaves it out so
 * a checkout with uncommitted work does not shout over one that asked a
 * question. Unknown measurements stay in the filter: an unread count is not
 * a zero.
 */
export const ATTENTION_REASONS = [
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
  "dirty",
] as const;

export type AttentionReason = (typeof ATTENTION_REASONS)[number];

const ATTENTION_RANK = new Map(ATTENTION_REASONS.map((reason, index) => [reason, index]));

/** Reasons that put a row in the attention filter. `dirty` is not one. */
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

const REASON_LABEL: Record<AttentionReason, string> = {
  "needs-you": "Needs you",
  error: "Stopped on an error",
  pending: "Requests waiting",
  blocked: "Git operation parked",
  collision: "Overlaps another checkout",
  disconnected: "Running, not shown in this window",
  unstarted: "Prepared, not started",
  signalled: "Asked for attention",
  unscanned: "Collisions not fully read",
  unprobed: "Parked operation not read",
  unmeasured: "Changes not read",
  dirty: "Uncommitted changes",
};

export type AgentPresence = "live" | "on-disk" | "missing";

export type GapKind = "failed" | "skipped" | "unread" | "partial";

export interface ProbeGap {
  repoPath: string;
  label: string;
  kind: GapKind;
  reason: string;
}

export interface PlaneProbe {
  path: string;
  label: string;
  snapshot: InsightsSnapshot | null;
  /** Set when the probe threw or returned nothing. Empty when it ran. */
  error: string;
  skipped: boolean;
  /** Why a skip happened. Empty when the probe was not skipped. */
  skipReason: "" | "deadline" | "cap";
}

export interface PlaneTerminal {
  key: string;
  repoPath: string;
  label: string;
  title: string;
  status: string;
  sessionId: string;
  taskRunId: string;
  continuesRunId: string;
  /** Unknown is null. It is not the repository root. */
  cwd: string | null;
  attention: "needs-you" | "error" | "signalled" | "finished" | null;
}

export interface PlaneTask {
  runId: string;
  title: string;
  repoPath: string;
  cwd: string;
  provider: string;
  tone: "needs-you" | "error" | "signalled" | "finished" | "problem" | "starting" | "active" | "quiet" | "adopted" | null;
  disconnected: boolean;
  unstarted: boolean;
  pendingCount: number;
  pendingMore: boolean;
}

export interface TaskProbe {
  ok: boolean;
  /** True before the first read. Not the same as a read that failed. */
  unread: boolean;
  error: string;
  /** False when the run list was capped, so every task count is a floor. */
  complete: boolean;
  tasks: readonly PlaneTask[];
}

export interface PlaneInput {
  probes: readonly PlaneProbe[];
  terminals: readonly PlaneTerminal[];
  /** Null when the caller is not watching task attempts at all. */
  tasks: TaskProbe | null;
  paths: PathIdentityOptions;
}

export interface AgentRow {
  id: string;
  repoPath: string;
  repoLabel: string;
  kind: string;
  /** Session slug, terminal title, or task title. Never invented. */
  session: string;
  checkout: string;
  worktreePath: string;
  presence: AgentPresence;
  /** What the presence word is claiming, in one sentence. */
  presenceDetail: string;
  attention: AttentionReason[];
  /** Primary sentence for the attention cell. */
  attentionLabel: string;
  parallelCount: number;
  /** True when the count is a floor, from a cap or an unread agent summary. */
  parallelFloor: boolean;
  dirtyFiles: number | null;
  dirtyKnown: boolean;
  liveKey: string | null;
  taskRunId: string | null;
}

export interface AgentPlane {
  rows: AgentRow[];
  shown: number;
  total: number;
  /** True when `rows` is shorter than the sessions that were found. */
  truncated: boolean;
  gaps: ProbeGap[];
  /** True when any included count is a floor rather than a total. */
  sessionsAreFloor: boolean;
  requested: number;
  read: number;
  failed: number;
  skipped: number;
}

interface Draft {
  id: string;
  repoPath: string;
  repoLabel: string;
  kind: string;
  session: string;
  checkout: string;
  worktreePath: string;
  presence: AgentPresence;
  presenceDetail: string;
  attention: Set<AttentionReason>;
  liveKey: string | null;
  taskRunId: string | null;
  dirtyFiles: number | null;
  dirtyKnown: boolean;
  parallelFloor: boolean;
  /** Agent-summary session count for this repository, when the summary ran. */
  diskSessions: number | null;
}

const LIVE_STATUS = new Set(["starting", "running"]);
const MAX_TEXT = 160;

function clean(value: string, max = MAX_TEXT): string {
  const text = value.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  if (text.length <= max) return text;
  return `${text.slice(0, max - 1)}…`;
}

function same(a: string, b: string, paths: PathIdentityOptions): boolean {
  const left = identityKey(a, paths);
  const right = identityKey(b, paths);
  return left !== "" && left === right;
}

function keyOf(path: string, paths: PathIdentityOptions): string {
  return identityKey(path, paths) || path;
}

/**
 * A terminal this plane should see.
 *
 * A shell the person opened is not an agent. A shell whose directory is an
 * agent checkout is still an occupant of that checkout, which is the fact
 * parallel sessions exist to show. Anything else with a launcher name other
 * than Shell is an agent this window started, including one sitting in the
 * main tree.
 */
export function terminalInScope(terminal: { label: string; repoPath: string; cwd: string | null }): boolean {
  if (agentKind(terminal.cwd ?? "") || agentKind(terminal.repoPath)) return true;
  const label = terminal.label.trim().toLowerCase();
  return label !== "" && label !== "shell";
}

export function liveAgentCount(
  records: readonly { label: string; status: string; repoPath: string }[],
): number {
  let count = 0;
  for (const record of records) {
    if (!LIVE_STATUS.has(record.status)) continue;
    if (!terminalInScope({ label: record.label, repoPath: record.repoPath, cwd: null })) continue;
    count += 1;
  }
  return count;
}

function checkoutLabel(item: WorktreeSummary): string {
  if (item.branch) return clean(item.branch, 80);
  if (item.is_detached) return "Detached HEAD";
  return "Branch unread";
}

function orderReasons(reasons: Set<AttentionReason>): AttentionReason[] {
  return [...reasons].sort((a, b) => (ATTENTION_RANK.get(a) ?? 99) - (ATTENTION_RANK.get(b) ?? 99));
}

function attentionText(reasons: readonly AttentionReason[]): string {
  if (reasons.length === 0) return "";
  const [first, ...rest] = reasons;
  const label = REASON_LABEL[first];
  return rest.length === 0 ? label : `${label} · +${rest.length}`;
}

function wantsAttention(reasons: readonly AttentionReason[]): boolean {
  return reasons.some((reason) => FILTER_ATTENTION.has(reason));
}

function urgentRank(reasons: readonly AttentionReason[]): number {
  if (reasons.length === 0) return 3;
  if (wantsAttention(reasons)) {
    const top = ATTENTION_RANK.get(reasons[0]) ?? 99;
    return top <= ATTENTION_RANK.get("signalled")! ? 0 : 1;
  }
  return 2;
}

/** 2 = a worktree list was read, 1 = a read was attempted, 0 = never started. */
function probeRank(item: PlaneProbe): number {
  if (!item.skipped && item.error === "" && item.snapshot?.worktrees.ok === true) return 2;
  if (!item.skipped) return 1;
  return 0;
}

function collapseProbes(probes: readonly PlaneProbe[], paths: PathIdentityOptions): PlaneProbe[] {
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
    // A later read replaces an earlier one only when it knows more. A success
    // replaces a failure, and a failure replaces a skip: the skip never
    // looked, so it must not hide the attempt that did.
    if (probeRank(probe) > probeRank(previous)) by.set(key, probe);
  }
  return order.map((key) => by.get(key)!);
}

function collisionPaths(snapshot: InsightsSnapshot, paths: PathIdentityOptions): Set<string> | null {
  const facet = snapshot.collisions;
  if (!facet.ok || facet.unscanned_worktrees > 0 || facet.failed_worktrees > 0 || facet.truncated) {
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

function checkoutReasons(item: WorktreeSummary, collisions: Set<string> | null, paths: PathIdentityOptions): Set<AttentionReason> {
  const reasons = new Set<AttentionReason>();
  if (collisions === null) reasons.add("unscanned");
  else if (collisions.has(keyOf(item.path, paths))) reasons.add("collision");
  if (!item.operation_ok) reasons.add("unprobed");
  else if (item.operation_kind) reasons.add("blocked");
  if (item.dirty_files === null) reasons.add("unmeasured");
  else if (item.dirty_files > 0) reasons.add("dirty");
  return reasons;
}

function terminalReasons(terminal: PlaneTerminal): AttentionReason[] {
  if (terminal.attention === "needs-you") return ["needs-you"];
  if (terminal.attention === "error") return ["error"];
  if (terminal.attention === "signalled") return ["signalled"];
  return [];
}

function taskReasons(task: PlaneTask): AttentionReason[] {
  const reasons: AttentionReason[] = [];
  if (task.tone === "needs-you") reasons.push("needs-you");
  // `problem` is also how a running attempt with no session here is ranked.
  // That fact is `disconnected`. Calling it an error says the attempt stopped.
  if (task.tone === "error" || (task.tone === "problem" && !task.disconnected)) reasons.push("error");
  if (task.tone === "signalled") reasons.push("signalled");
  // A capped page that reported no rows still has requests behind it.
  // `pendingCount === 0` is then a floor, not an empty queue.
  if (task.pendingCount > 0 || task.pendingMore) reasons.push("pending");
  if (task.disconnected) reasons.push("disconnected");
  if (task.unstarted) reasons.push("unstarted");
  return reasons;
}

function keepTerminal(terminal: PlaneTerminal): boolean {
  if (!terminal.key) return false;
  if (LIVE_STATUS.has(terminal.status)) return true;
  return terminalReasons(terminal).length > 0;
}

function attach(draft: Draft, terminal: PlaneTerminal): void {
  draft.presence = "live";
  draft.presenceDetail = "A process this window started.";
  draft.liveKey = terminal.key;
  if (terminal.taskRunId) draft.taskRunId = terminal.taskRunId;
  else if (terminal.continuesRunId && !draft.taskRunId) draft.taskRunId = terminal.continuesRunId;
  for (const reason of terminalReasons(terminal)) draft.attention.add(reason);
  const title = clean(terminal.title || terminal.label, 80);
  if (title && draft.session === draft.kind) draft.session = title;
  else if (title && !draft.session) draft.session = title;
}

function fromCheckout(probe: PlaneProbe, item: WorktreeSummary, collisions: Set<string> | null, paths: PathIdentityOptions, diskSessions: number | null, parallelFloor: boolean): Draft {
  const slug = clean(item.session_slug, 80);
  return {
    id: "",
    repoPath: probe.path,
    repoLabel: clean(probe.label || probe.path, 80),
    kind: clean(item.agent_kind, 40),
    session: slug,
    checkout: checkoutLabel(item),
    worktreePath: item.path,
    presence: "on-disk",
    presenceDetail: "A checkout on disk. This does not say a process is running.",
    attention: checkoutReasons(item, collisions, paths),
    liveKey: null,
    taskRunId: null,
    dirtyFiles: item.dirty_files,
    dirtyKnown: item.dirty_files !== null,
    parallelFloor,
    diskSessions,
  };
}

function fromTerminal(probe: PlaneProbe | undefined, terminal: PlaneTerminal, paths: PathIdentityOptions): Draft {
  const repoPath = probe?.path || terminal.repoPath;
  return {
    id: "",
    repoPath,
    repoLabel: clean(probe?.label || terminal.repoPath, 80),
    kind: clean(agentKind(terminal.repoPath) || terminal.label, 40),
    session: clean(terminal.title || terminal.label, 80),
    checkout: agentKind(terminal.repoPath) ? "Agent checkout" : "Main checkout",
    worktreePath: terminal.cwd && identityKey(terminal.cwd, paths) ? terminal.cwd : terminal.repoPath,
    presence: "live",
    presenceDetail: "A process this window started.",
    attention: new Set(terminalReasons(terminal)),
    liveKey: terminal.key,
    taskRunId: terminal.taskRunId || terminal.continuesRunId || null,
    dirtyFiles: null,
    dirtyKnown: false,
    parallelFloor: false,
    diskSessions: null,
  };
}

function foldTask(draft: Draft, task: PlaneTask): void {
  draft.taskRunId = task.runId;
  if (!draft.kind) draft.kind = clean(task.provider, 40);
  if (!draft.session) draft.session = clean(task.title, 80);
  for (const reason of taskReasons(task)) draft.attention.add(reason);
}

function fromTask(task: PlaneTask): Draft {
  return {
    id: "",
    repoPath: task.repoPath || task.cwd,
    repoLabel: clean(task.repoPath || task.cwd, 80),
    kind: clean(task.provider, 40),
    session: clean(task.title, 80),
    checkout: clean(task.cwd, 80),
    worktreePath: task.cwd,
    presence: "missing",
    presenceDetail: "The workbench has this attempt, and no terminal in this window is attached to it.",
    attention: new Set(taskReasons(task)),
    liveKey: null,
    taskRunId: task.runId,
    dirtyFiles: null,
    dirtyKnown: false,
    parallelFloor: false,
    diskSessions: null,
  };
}

function findHost(drafts: readonly Draft[], path: string, paths: PathIdentityOptions): Draft | undefined {
  return drafts.find((draft) => draft.worktreePath !== "" && same(draft.worktreePath, path, paths));
}

/**
 * Projects probes, live terminals and task attempts into one plane.
 *
 * A failed or skipped probe contributes a gap and no rows. Rows from a
 * repository whose agent list was capped carry `parallelFloor`, and the
 * plane's `sessionsAreFloor` is set. `rows` is capped; `total` is not.
 */
export function projectAgentPlane(input: PlaneInput): AgentPlane {
  const paths = input.paths;
  const probes = collapseProbes(input.probes, paths);
  const gaps: ProbeGap[] = [];
  const drafts: Draft[] = [];
  let sessionsAreFloor = false;
  let read = 0;
  let failed = 0;
  let skipped = 0;

  for (const probe of probes) {
    const label = clean(probe.label || probe.path, 80);
    if (probe.skipped) {
      skipped += 1;
      gaps.push({
        repoPath: probe.path,
        label,
        kind: "skipped",
        reason: probe.skipReason === "cap"
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
        reason: clean(probe.error || "The repository could not be read.", 240),
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
        reason: clean(snapshot.worktrees.error || "Worktrees could not be listed.", 240),
      });
      continue;
    }
    read += 1;
    const diskSessions = snapshot.agents.ok ? snapshot.agents.sessions : null;
    const parallelFloor = !snapshot.agents.ok || snapshot.agents.truncated || snapshot.worktrees.truncated;
    if (parallelFloor) sessionsAreFloor = true;
    if (!snapshot.agents.ok) {
      gaps.push({
        repoPath: probe.path,
        label,
        kind: "partial",
        reason: "Agent counts could not be read. The checkouts below are only the ones whose paths were listed.",
      });
    } else if (snapshot.agents.truncated || snapshot.worktrees.truncated) {
      gaps.push({
        repoPath: probe.path,
        label,
        kind: "partial",
        reason: "The worktree list was capped. Session counts for this repository are a floor.",
      });
    }
    const collisions = collisionPaths(snapshot, paths);
    if (collisions === null) {
      gaps.push({
        repoPath: probe.path,
        label,
        kind: "partial",
        reason: snapshot.collisions.ok
          ? "Collision scan did not cover every worktree. An empty overlap list is not a clear one."
          : clean(snapshot.collisions.error || "Collisions could not be read.", 240),
      });
    }
    for (const item of snapshot.worktrees.items) {
      if (!item.agent_kind || !item.session_slug) continue;
      drafts.push(fromCheckout(probe, item, collisions, paths, diskSessions, parallelFloor));
    }
  }

  const seenTerminal = new Set<string>();
  for (const terminal of input.terminals) {
    if (!keepTerminal(terminal) || seenTerminal.has(terminal.key)) continue;
    if (!terminalInScope(terminal)) continue;
    seenTerminal.add(terminal.key);
    const hostPath = terminal.cwd || terminal.repoPath;
    const host = findHost(drafts, hostPath, paths) ?? findHost(drafts, terminal.repoPath, paths);
    if (host && host.liveKey === null) {
      attach(host, terminal);
      continue;
    }
    const probe = probes.find((item) => same(item.path, terminal.repoPath, paths) || (host ? same(item.path, host.repoPath, paths) : false));
    const draft = host
      ? {
          ...host,
          attention: new Set(host.attention),
          liveKey: null,
          taskRunId: null,
          session: clean(host.session, 80),
        }
      : fromTerminal(probe, terminal, paths);
    attach(draft, terminal);
    // A second process in a checkout is its own session, not a rewrite of the first.
    if (host) draft.session = clean(terminal.title || terminal.label || host.session, 80);
    drafts.push(draft);
  }

  if (input.tasks === null) {
    // The caller is not watching attempts. Say nothing about them.
  } else if (input.tasks.unread) {
    gaps.push({
      repoPath: "",
      label: "Tasks",
      kind: "unread",
      reason: "Task attempts have not been read yet.",
    });
  } else if (!input.tasks.ok) {
    gaps.push({
      repoPath: "",
      label: "Tasks",
      kind: "failed",
      reason: clean(input.tasks.error || "Task attempts could not be read.", 240),
    });
  } else {
    if (!input.tasks.complete) {
      sessionsAreFloor = true;
      gaps.push({
        repoPath: "",
        label: "Tasks",
        kind: "partial",
        reason: "The task-attempt list was capped. Counts of attempts are a floor.",
      });
    }
    for (const task of input.tasks.tasks) {
      if (!task.runId) continue;
      const existing = drafts.find((draft) => draft.taskRunId === task.runId);
      if (existing) {
        foldTask(existing, task);
        continue;
      }
      const host = findHost(drafts, task.cwd, paths);
      if (host && host.taskRunId === null) {
        foldTask(host, task);
        continue;
      }
      const reasons = taskReasons(task);
      if (reasons.length === 0) continue;
      drafts.push(fromTask(task));
    }
  }

  const perRepo = new Map<string, Draft[]>();
  for (const draft of drafts) {
    const key = keyOf(draft.repoPath, paths);
    const list = perRepo.get(key);
    if (list) list.push(draft);
    else perRepo.set(key, [draft]);
  }

  const finished: AgentRow[] = [];
  const used = new Set<string>();
  for (const draft of drafts) {
    const siblings = perRepo.get(keyOf(draft.repoPath, paths)) ?? [draft];
    // One repository has one session count. A row that did not come from the
    // agent summary (a terminal sitting in the main tree) must not report a
    // smaller number than the checkout beside it.
    const disks = siblings.flatMap((sibling) => sibling.diskSessions === null ? [] : [sibling.diskSessions]);
    const disk = disks.length === 0 ? null : Math.max(...disks);
    const parallelFloor = siblings.some((sibling) => sibling.parallelFloor) || (disk !== null && disk > siblings.length);
    const parallelCount = disk === null ? siblings.length : Math.max(siblings.length, disk);
    if (parallelFloor) sessionsAreFloor = true;
    const attention = orderReasons(draft.attention);
    let id = [keyOf(draft.repoPath, paths), keyOf(draft.worktreePath, paths), draft.liveKey ?? "", draft.taskRunId ?? ""].join("|");
    if (used.has(id)) id = `${id}|${used.size}`;
    used.add(id);
    finished.push({
      id,
      repoPath: draft.repoPath,
      repoLabel: draft.repoLabel || clean(draft.repoPath, 80),
      kind: draft.kind || "agent",
      session: draft.session || draft.kind || "session",
      checkout: draft.checkout,
      worktreePath: draft.worktreePath,
      presence: draft.presence,
      presenceDetail: draft.presenceDetail,
      attention,
      attentionLabel: attentionText(attention),
      parallelCount,
      parallelFloor,
      dirtyFiles: draft.dirtyKnown ? draft.dirtyFiles : null,
      dirtyKnown: draft.dirtyKnown,
      liveKey: draft.liveKey,
      taskRunId: draft.taskRunId,
    });
  }

  finished.sort((a, b) =>
    urgentRank(a.attention) - urgentRank(b.attention)
    || a.repoLabel.localeCompare(b.repoLabel)
    || a.kind.localeCompare(b.kind)
    || a.session.localeCompare(b.session)
    || a.id.localeCompare(b.id));

  const total = finished.length;
  const truncated = total > MAX_AGENT_ROWS;
  const rows = truncated ? finished.slice(0, MAX_AGENT_ROWS) : finished;
  return {
    rows,
    shown: rows.length,
    total,
    truncated,
    gaps,
    sessionsAreFloor: sessionsAreFloor || truncated,
    requested: probes.length,
    read,
    failed,
    skipped,
  };
}

export function applyAgentFilter(rows: readonly AgentRow[], filter: AgentFilter): AgentRow[] {
  if (filter === "all") return [...rows];
  if (filter === "live") return rows.filter((row) => row.presence === "live");
  if (filter === "parallel") return rows.filter((row) => row.parallelCount > 1 || row.parallelFloor);
  return rows.filter((row) => wantsAttention(row.attention));
}

export function planeHeadline(plane: AgentPlane, visible: number): string {
  const noun = plane.sessionsAreFloor ? "at least " : "";
  const sessions = `${noun}${plane.total} ${plane.total === 1 ? "session" : "sessions"}`;
  const needing = plane.rows.filter((row) => wantsAttention(row.attention)).length;
  // `rows` may be capped; the attention count is then also a floor.
  const need = plane.truncated ? `${needing}+ need attention` : `${needing} need attention`;
  // Rows past the cap are not hidden by the filter. `visible` counts how many
  // of the returned rows the caller kept.
  const hidden = plane.rows.length - visible;
  const filter = hidden > 0 ? ` · ${hidden} hidden by the filter` : "";
  const unread = plane.failed + plane.skipped;
  const gap = unread > 0 ? ` · ${unread} ${unread === 1 ? "repository" : "repositories"} not read` : "";
  return `${sessions} · ${need}${filter}${gap}`;
}
