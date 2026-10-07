/**
 * The Agents plane: one row per agent checkout, per terminal this window
 * started, and per task attempt that is in neither, across the open
 * repositories.
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
 *
 * Where a process is decides which checkout it is in. A directory the OS
 * reported binds to the worktree that contains it, subdirectories included,
 * and to nothing when no listed worktree does. Only an unknown directory
 * falls back to the tab the terminal was opened from.
 *
 * The headline counts two quantities other surfaces show, with one
 * definition each: agent checkouts on disk (Fleet's agent count and the Work
 * view's Agent worktrees tile, `agent_summary` in insights/mod.rs) and live
 * agent terminals (the tab bar's chip, `liveAgentCount`).
 */

import type { InsightsSnapshot, WorktreeSummary } from "../insights/types";
import { identityKey, type PathIdentityOptions } from "../repos/paths";
import { agentKind } from "../work/agentWorktree";
import { nearestContaining } from "./cwd";

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
 * a zero. The headline counts them apart (`UNREAD_REASONS`), so a partial
 * collision scan is "not fully read", not a request from every checkout.
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

/** Filter reasons that say a read did not happen, not that the reader is wanted. */
const UNREAD_REASONS = new Set<AttentionReason>(["unscanned", "unprobed", "unmeasured"]);

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

/**
 * `exited` is a process this window started that has stopped and still
 * wants the reader. It is not live, and the live filter and count leave it out.
 */
export type AgentPresence = "live" | "exited" | "on-disk" | "missing";

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
  /** The registered repository's checkout. Empty when it could not be resolved. */
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
  /** True when `rows` is shorter than the rows that were projected. */
  truncated: boolean;
  gaps: ProbeGap[];
  /** Distinct agent checkouts the read repositories listed. */
  checkouts: number;
  /** True when a listing or agent summary was capped or unread. */
  checkoutsAreFloor: boolean;
  /** Live agent terminals, by the definition `liveAgentCount` uses. */
  live: number;
  /** Distinct task attempts on the plane; null when attempts were not read. */
  tasks: number | null;
  tasksAreFloor: boolean;
  /**
   * Rows in the attention filter, over every projected row: `needing` has a
   * reason that asks for the reader, `unread` only reasons that a read did
   * not happen. The filter holds both.
   */
  attention: { needing: number; unread: number };
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

interface LiveCandidate {
  key: string;
  label: string;
  status: string;
  repoPath: string;
  cwd: string | null;
}

/** Keys of the live agent terminals: running, in scope, each key once. */
function liveKeys(terminals: Iterable<LiveCandidate>): Set<string> {
  const keys = new Set<string>();
  for (const terminal of terminals) {
    if (!terminal.key || !LIVE_STATUS.has(terminal.status)) continue;
    if (terminalInScope(terminal)) keys.add(terminal.key);
  }
  return keys;
}

/**
 * Live agent terminals this window started: the tab bar's chip and the
 * plane's headline. `directories` is what the last cwd sweep read, by
 * session id; a session it did not read stays unknown rather than placed.
 */
export function liveAgentCount(
  records: readonly { key: string; label: string; status: string; repoPath: string; sessionId?: string | null }[],
  directories: ReadonlyMap<string, string> = new Map(),
): number {
  return liveKeys(records.map((record) => ({
    key: record.key,
    label: record.label,
    status: record.status,
    repoPath: record.repoPath,
    cwd: record.sessionId ? directories.get(record.sessionId) ?? null : null,
  }))).size;
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

function needsReader(reasons: readonly AttentionReason[]): boolean {
  return reasons.some((reason) => FILTER_ATTENTION.has(reason) && !UNREAD_REASONS.has(reason));
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

/**
 * How much of a repository one read listing saw: a whole agent summary
 * outranks a capped one, which outranks an unread one, and a full collision
 * scan breaks the tie.
 */
function listingRank(probe: PlaneProbe, paths: PathIdentityOptions): number {
  const snapshot = probe.snapshot!;
  const agents = !snapshot.agents.ok ? 0 : snapshot.agents.truncated || snapshot.worktrees.truncated ? 1 : 2;
  return agents * 2 + (collisionPaths(snapshot, paths) === null ? 0 : 1);
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

function presenceOf(terminal: PlaneTerminal): Pick<Draft, "presence" | "presenceDetail"> {
  return LIVE_STATUS.has(terminal.status)
    ? { presence: "live", presenceDetail: "A process this window started." }
    : { presence: "exited", presenceDetail: "A process this window started. It has exited and still wants the reader." };
}

function attach(draft: Draft, terminal: PlaneTerminal): void {
  Object.assign(draft, presenceOf(terminal));
  draft.liveKey = terminal.key;
  if (terminal.taskRunId) draft.taskRunId = terminal.taskRunId;
  else if (terminal.continuesRunId && !draft.taskRunId) draft.taskRunId = terminal.continuesRunId;
  for (const reason of terminalReasons(terminal)) draft.attention.add(reason);
  const title = clean(terminal.title || terminal.label, 80);
  if (title && draft.session === draft.kind) draft.session = title;
  else if (title && !draft.session) draft.session = title;
}

/** A worktree some read repository listed, and its row when it is an agent checkout. */
interface Place {
  probe: PlaneProbe;
  item: WorktreeSummary;
  draft: Draft | null;
}

function placeLabel(item: WorktreeSummary): string {
  if (item.agent_kind) return "Agent checkout";
  return item.is_main ? "Main checkout" : "Linked checkout";
}

function fromCheckout(probe: PlaneProbe, item: WorktreeSummary, collisions: Set<string> | null, paths: PathIdentityOptions, diskSessions: number | null, parallelFloor: boolean): Draft {
  // A checkout at the layout's container has no slug. It is still a worktree
  // an agent layout holds, and `agent_summary` counts it, so it gets a row.
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

/**
 * A terminal no agent checkout holds.
 *
 * `where` is the directory the OS reported, or the tab's path when that is
 * unknown. Kind and checkout are read from `where`, so a process that left
 * its tab's checkout is not named after it.
 */
function fromTerminal(terminal: PlaneTerminal, where: string, known: boolean, place: Place | undefined, probe: PlaneProbe | undefined): Draft {
  const kind = agentKind(where);
  const checkout = place
    ? placeLabel(place.item)
    : kind
      ? "Agent checkout"
      : known ? "Outside the open repositories" : "Directory not read";
  return {
    id: "",
    repoPath: probe?.path || terminal.repoPath,
    repoLabel: clean(probe?.label || terminal.repoPath, 80),
    kind: clean(kind || terminal.label, 40),
    session: clean(terminal.title || terminal.label, 80),
    checkout,
    worktreePath: place?.item.path ?? where,
    ...presenceOf(terminal),
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
  for (const reason of taskReasons(task)) {
    // The attempt's own terminal is in this window. "Not shown in this
    // window" would contradict the row it is on.
    if (reason === "disconnected" && draft.liveKey !== null) continue;
    draft.attention.add(reason);
  }
}

function fromTask(task: PlaneTask, place: Place | undefined, repo: PlaneProbe | undefined): Draft {
  const repoPath = repo?.path || task.repoPath;
  return {
    id: "",
    repoPath,
    repoLabel: clean(repo?.label || repoPath, 80) || "Repository not resolved",
    kind: clean(task.provider, 40),
    session: clean(task.title, 80),
    checkout: place ? placeLabel(place.item) : clean(task.cwd, 80),
    worktreePath: place?.item.path ?? task.cwd,
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

/** One repository's session count, computed once for all of its rows. */
interface RepoCount {
  rows: number;
  disk: number | null;
  floor: boolean;
}

function repoGroup(draft: Draft, paths: PathIdentityOptions): string {
  // A task whose repository could not be resolved belongs to no repository,
  // so it does not lend its count to, or borrow one from, any other row.
  return draft.repoPath ? keyOf(draft.repoPath, paths) : `\u0000run:${draft.taskRunId ?? ""}`;
}

/**
 * Projects probes, live terminals and task attempts into one plane.
 *
 * A failed or skipped probe contributes a gap and no rows. Rows from a
 * repository whose agent list was capped carry `parallelFloor`, and the
 * plane's `checkoutsAreFloor` is set. `rows` is capped; `total` is not.
 *
 * Linear in its inputs: every lookup goes through a map keyed by
 * `identityKey` (worktrees, probes) or run id, and a containment lookup
 * costs one probe per directory level.
 */
export function projectAgentPlane(input: PlaneInput): AgentPlane {
  const paths = input.paths;
  const probes = collapseProbes(input.probes, paths);
  const gaps: ProbeGap[] = [];
  const drafts: Draft[] = [];
  /** Every worktree a read listed. The first listing of a path wins. */
  const worktrees = new Map<string, Place>();
  const probeByPath = new Map<string, PlaneProbe>();
  const byRun = new Map<string, Draft>();
  let checkoutsAreFloor = false;
  let checkouts = 0;
  let read = 0;
  let failed = 0;
  let skipped = 0;

  // Probes that listed worktrees, grouped by repository. Two tabs of one
  // repository whose common directory was not read are two probes listing
  // the same worktrees, and a shared worktree is what makes them one group.
  const groups: PlaneProbe[][] = [];
  const groupOf = new Map<string, PlaneProbe[]>();
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
    const probeKey = identityKey(probe.path, paths);
    if (probeKey && !probeByPath.has(probeKey)) probeByPath.set(probeKey, probe);
    let group: PlaneProbe[] | undefined;
    for (const item of snapshot.worktrees.items) {
      group = groupOf.get(identityKey(item.path, paths));
      if (group) break;
    }
    if (!group) {
      group = [];
      groups.push(group);
    }
    group.push(probe);
    for (const item of snapshot.worktrees.items) {
      const key = identityKey(item.path, paths);
      if (key && !groupOf.has(key)) groupOf.set(key, group);
    }
  }

  for (const group of groups) {
    // The most complete listing speaks for the repository: its gaps, its
    // counts, and the path its rows are filed under. Another tab of it adds
    // only worktrees the first did not list, and no second set of notes.
    const primary = group.reduce((best, probe) => (listingRank(probe, paths) > listingRank(best, paths) ? probe : best));
    const snapshot = primary.snapshot!;
    const label = clean(primary.label || primary.path, 80);
    const diskSessions = snapshot.agents.ok ? snapshot.agents.sessions : null;
    const parallelFloor = !snapshot.agents.ok || snapshot.agents.truncated || snapshot.worktrees.truncated;
    if (parallelFloor) checkoutsAreFloor = true;
    if (!snapshot.agents.ok) {
      gaps.push({
        repoPath: primary.path,
        label,
        kind: "partial",
        reason: "Agent counts could not be read. The checkouts below are only the ones whose paths were listed.",
      });
    } else if (snapshot.agents.truncated || snapshot.worktrees.truncated) {
      gaps.push({
        repoPath: primary.path,
        label,
        kind: "partial",
        reason: "The worktree list was capped. Session counts for this repository are a floor.",
      });
    }
    if (collisionPaths(snapshot, paths) === null) {
      gaps.push({
        repoPath: primary.path,
        label,
        kind: "partial",
        reason: snapshot.collisions.ok
          ? "Collision scan did not cover every worktree. An empty overlap list is not a clear one."
          : clean(snapshot.collisions.error || "Collisions could not be read.", 240),
      });
    }
    for (const probe of [primary, ...group.filter((other) => other !== primary)]) {
      const collisions = collisionPaths(probe.snapshot!, paths);
      for (const item of probe.snapshot!.worktrees.items) {
        // A checkout is one row, however many listings name it.
        const key = identityKey(item.path, paths);
        if (key && worktrees.has(key)) continue;
        const draft = item.agent_kind ? fromCheckout(primary, item, collisions, paths, diskSessions, parallelFloor) : null;
        if (key) worktrees.set(key, { probe: primary, item, draft });
        if (!draft) continue;
        drafts.push(draft);
        checkouts += 1;
      }
    }
  }

  const seenTerminal = new Set<string>();
  for (const terminal of input.terminals) {
    if (!keepTerminal(terminal) || seenTerminal.has(terminal.key)) continue;
    if (!terminalInScope(terminal)) continue;
    seenTerminal.add(terminal.key);
    // A directory the OS reported decides the checkout on its own. Falling
    // back to the tab would bind a process that left to the place it left.
    const known = terminal.cwd !== null && identityKey(terminal.cwd, paths) !== "";
    const where = known ? terminal.cwd! : terminal.repoPath;
    const place = nearestContaining(worktrees, where, paths);
    const host = place?.draft ?? null;
    if (host && host.liveKey === null) {
      attach(host, terminal);
      if (host.taskRunId) byRun.set(host.taskRunId, host);
      continue;
    }
    let draft: Draft;
    if (host) {
      // A second process in a checkout is its own session, not a rewrite of the first.
      draft = { ...host, attention: new Set(host.attention), liveKey: null, taskRunId: null };
      attach(draft, terminal);
      draft.session = clean(terminal.title || terminal.label || host.session, 80);
    } else {
      const repo = place?.probe
        ?? nearestContaining(worktrees, terminal.repoPath, paths)?.probe
        ?? probeByPath.get(identityKey(terminal.repoPath, paths));
      draft = fromTerminal(terminal, where, known, place, repo);
    }
    drafts.push(draft);
    if (draft.taskRunId && !byRun.has(draft.taskRunId)) byRun.set(draft.taskRunId, draft);
  }

  let tasks: number | null = null;
  let tasksAreFloor = false;
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
      tasksAreFloor = true;
      gaps.push({
        repoPath: "",
        label: "Tasks",
        kind: "partial",
        reason: "The task-attempt list was capped. Counts of attempts are a floor.",
      });
    }
    const attempts = new Set<string>();
    for (const task of input.tasks.tasks) {
      if (!task.runId) continue;
      const own = byRun.get(task.runId);
      if (own) {
        foldTask(own, task);
        attempts.add(task.runId);
        continue;
      }
      // An attempt joins a checkout it is running in only while no terminal
      // holds that checkout. A terminal there is some other process, and
      // folding into it would put this attempt's facts on that row.
      const place = task.cwd ? nearestContaining(worktrees, task.cwd, paths) : undefined;
      const host = place?.draft;
      if (host && host.liveKey === null && host.taskRunId === null) {
        foldTask(host, task);
        byRun.set(task.runId, host);
        attempts.add(task.runId);
        continue;
      }
      // P2, kept on purpose: an attempt with nothing to report and no
      // checkout or terminal here has no fact this plane can show. Its
      // presence is the workbench's to say, and the task board lists it.
      // Anything that wants the reader, `disconnected` included, still
      // gets its own row.
      if (taskReasons(task).length === 0) continue;
      const repo = task.repoPath
        ? nearestContaining(worktrees, task.repoPath, paths)?.probe ?? probeByPath.get(identityKey(task.repoPath, paths))
        : place?.probe;
      const draft = fromTask(task, place, repo);
      drafts.push(draft);
      byRun.set(task.runId, draft);
      attempts.add(task.runId);
    }
    tasks = attempts.size;
  }

  const perRepo = new Map<string, RepoCount>();
  for (const draft of drafts) {
    const key = repoGroup(draft, paths);
    const count = perRepo.get(key) ?? { rows: 0, disk: null, floor: false };
    count.rows += 1;
    if (draft.diskSessions !== null) count.disk = Math.max(count.disk ?? 0, draft.diskSessions);
    count.floor ||= draft.parallelFloor;
    perRepo.set(key, count);
  }
  for (const count of perRepo.values()) {
    // One repository has one session count. A row that did not come from the
    // agent summary (a terminal sitting in the main tree) must not report a
    // smaller number than the checkout beside it.
    if (count.disk !== null && count.disk > count.rows) count.floor = true;
  }

  const finished: AgentRow[] = [];
  const used = new Set<string>();
  let needing = 0;
  let unread = 0;
  for (const draft of drafts) {
    const count = perRepo.get(repoGroup(draft, paths))!;
    const attention = orderReasons(draft.attention);
    if (needsReader(attention)) needing += 1;
    else if (wantsAttention(attention)) unread += 1;
    const base = [keyOf(draft.repoPath, paths), keyOf(draft.worktreePath, paths), draft.liveKey ?? "", draft.taskRunId ?? ""].join("|");
    let id = base;
    for (let suffix = 1; used.has(id); suffix += 1) id = `${base}|${suffix}`;
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
      parallelCount: count.disk === null ? count.rows : Math.max(count.rows, count.disk),
      parallelFloor: count.floor,
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
    checkouts,
    checkoutsAreFloor,
    live: liveKeys(input.terminals).size,
    tasks,
    tasksAreFloor,
    attention: { needing, unread },
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

function counted(count: number, floor: boolean, one: string, many: string): string {
  return `${floor ? "at least " : ""}${count} ${count === 1 ? one : many}`;
}

/**
 * What the plane holds, by the quantities its rows are made of.
 *
 * Checkouts, live terminals and task attempts overlap: a terminal in a
 * checkout is one row and counts in both. "Need attention" and "not fully
 * read" count rows, and together they are what the attention filter keeps.
 */
export function planeHeadline(plane: AgentPlane, visible: number): string {
  const parts = [
    counted(plane.checkouts, plane.checkoutsAreFloor, "agent checkout", "agent checkouts"),
    counted(plane.live, false, "live terminal", "live terminals"),
  ];
  if (plane.tasks !== null) parts.push(counted(plane.tasks, plane.tasksAreFloor, "task attempt", "task attempts"));
  parts.push(`${plane.attention.needing} need attention`);
  if (plane.attention.unread > 0) parts.push(`${plane.attention.unread} not fully read`);
  // Rows past the cap are not hidden by the filter. `visible` counts how many
  // of the returned rows the caller kept.
  const hidden = plane.rows.length - visible;
  if (hidden > 0) parts.push(`${hidden} hidden by the filter`);
  if (plane.truncated) parts.push(`${plane.total - plane.shown} past the row cap`);
  const unreadRepos = plane.failed + plane.skipped;
  if (unreadRepos > 0) parts.push(`${unreadRepos} ${unreadRepos === 1 ? "repository" : "repositories"} not read`);
  return parts.join(" · ");
}
