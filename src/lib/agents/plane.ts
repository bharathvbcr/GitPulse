/**
 * The Agents plane: the agent checkouts, terminals and task attempts the
 * open repositories are holding, one row each.
 *
 * Three sources, and they do not say the same thing:
 *
 * - A git worktree under `.<agent>/worktrees/<slug>` is a checkout. It is not
 *   a running process. Insights can see the checkout and cannot see whether
 *   anyone is still in it.
 * - A terminal is a process this window started. A Claude the person
 *   launched outside GitPulse is absent here, not idle.
 * - A task attempt is a run the workbench recorded. A run whose list could
 *   not be read is not a run that does not exist.
 *
 * The projection refuses to collapse those into one "status". A probe that
 * did not run, a list that was capped, and a checkout whose changes were not
 * measured each stay visible as that fact. Callers render the gaps; they do
 * not treat an empty row list as a quiet workspace.
 *
 * Attribution is by containment. A terminal or an attempt belongs to the
 * checkout whose directory contains its own (a subdirectory included), and a
 * directory that is known and outside every checkout belongs to none of them,
 * whatever tab the process was opened from. One repository read through two
 * of its own checkouts is one repository: each checkout appears once.
 */

import type { InsightsSnapshot, WorktreeSummary } from "../insights/types";
import { identityKey, type PathIdentityOptions } from "../repos/paths";
import { agentKind } from "../work/agentWorktree";
import { nearestContaining } from "./cwd";
import { plural } from "../format";

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

/**
 * `exited` is a process this window started that has ended and still has
 * something to say (it needs the reader, or stopped on an error). It is not
 * live, and the live filter and the live count leave it out.
 */
export type AgentPresence = "live" | "exited" | "on-disk" | "missing";

/** Which source a row was first drawn from. A checkout row stays one when a terminal joins it. */
export type AgentOrigin = "checkout" | "terminal" | "task";

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
  /** The repository the run belongs to, resolved from its id. Empty when that failed. */
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
  /** The repository's path. Empty for an attempt whose repository could not be resolved. */
  repoPath: string;
  repoLabel: string;
  kind: string;
  /** Checkout slug, terminal title, or task title. Never invented. */
  session: string;
  checkout: string;
  /** Where the row is: the checkout, the terminal's directory, or the attempt's. */
  worktreePath: string;
  /** The checkout "Open checkout" opens. Null when the row is outside every checkout read. */
  checkoutPath: string | null;
  origin: AgentOrigin;
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
  /** True when `rows` is shorter than the rows that were found. */
  truncated: boolean;
  gaps: ProbeGap[];
  /** True when any included count is a floor rather than a total. */
  sessionsAreFloor: boolean;
  /**
   * Agent checkouts on disk, by the rule `agent_summary` (insights/mod.rs)
   * uses for Fleet: every listed worktree with an agent kind, each once.
   */
  checkouts: number;
  checkoutsFloor: boolean;
  /** Live agent terminals this window started: `liveAgentCount` over the same terminals. */
  live: number;
  /** Task attempts the workbench returned. Null when they were not read or not watched. */
  taskAttempts: number | null;
  tasksWatched: boolean;
  tasksFloor: boolean;
  /**
   * Rows in the attention filter, split by why. `needing` asked for the
   * reader; `unread` are there only because a measurement was not read.
   * Their sum is what `applyAgentFilter(rows, "attention")` keeps.
   */
  attention: { needing: number; unread: number };
  requested: number;
  read: number;
  failed: number;
  skipped: number;
}

interface Draft {
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
  attention: Set<AttentionReason>;
  liveKey: string | null;
  taskRunId: string | null;
  dirtyFiles: number | null;
  dirtyKnown: boolean;
  parallelFloor: boolean;
  /** Agent-summary session count for this repository, when the summary ran. */
  diskSessions: number | null;
}

/** One repository, however many of its checkouts were probed. */
interface Family {
  repoPath: string;
  repoLabel: string;
  diskSessions: number | null;
  floor: boolean;
  listed: number;
}

/** A listed worktree (or a probed path), and the repository it belongs to. */
interface Place {
  family: Family;
  path: string;
  checkout: string;
}

const LIVE_STATUS = new Set(["starting", "running"]);
const MAX_TEXT = 160;
const LIVE_DETAIL = "A process this window started.";
const EXITED_DETAIL = "A process this window started. It has exited.";

/** Reasons that say a reading is missing, not that anyone asked for anything. */
const UNREAD_REASONS = new Set<AttentionReason>(["unscanned", "unprobed", "unmeasured"]);

function clean(value: string, max = MAX_TEXT): string {
  const text = value.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  if (text.length <= max) return text;
  return `${text.slice(0, max - 1)}…`;
}

function keyOf(path: string, paths: PathIdentityOptions): string {
  return identityKey(path, paths) || path;
}

/** Where a terminal is: its directory when the OS said, else the tab it was opened from. */
function knownCwd(terminal: { cwd: string | null }, paths?: PathIdentityOptions): string | null {
  const cwd = terminal.cwd?.trim() ?? "";
  if (!cwd) return null;
  if (paths && !identityKey(cwd, paths)) return null;
  return cwd;
}

/**
 * A terminal this plane should see.
 *
 * A shell the person opened is not an agent. A shell whose directory is an
 * agent checkout is still an occupant of that checkout, which is the fact
 * parallel sessions exist to show. The directory decides when it is known;
 * the tab only when it is not, so a shell that left the checkout is not
 * counted as still in it. Anything else with a launcher name other than
 * Shell is an agent this window started, including one in the main tree.
 */
export function terminalInScope(terminal: { label: string; repoPath: string; cwd: string | null }): boolean {
  if (agentKind(knownCwd(terminal) ?? terminal.repoPath)) return true;
  const label = terminal.label.trim().toLowerCase();
  return label !== "" && label !== "shell";
}

/**
 * Live agent terminals this window started: the one definition the tab chip
 * and the plane's headline both use.
 *
 * A record counts when it is starting or running and in scope, with its
 * directory taken from `directories` (by backend session id) when that is
 * known. A key counts once however many records repeat it.
 */
export function liveAgentCount(
  records: readonly { key?: string; label: string; status: string; repoPath: string; sessionId?: string | null }[],
  directories: ReadonlyMap<string, string> = new Map(),
): number {
  const seen = new Set<string>();
  let count = 0;
  for (const record of records) {
    if (!LIVE_STATUS.has(record.status)) continue;
    const cwd = record.sessionId ? directories.get(record.sessionId) ?? null : null;
    if (!terminalInScope({ label: record.label, repoPath: record.repoPath, cwd })) continue;
    if (record.key) {
      if (seen.has(record.key)) continue;
      seen.add(record.key);
    }
    count += 1;
  }
  return count;
}

/** `base`, or the first `base|n` the set does not already hold. */
export function uniqueRowId(base: string, used: ReadonlySet<string>): string {
  if (!used.has(base)) return base;
  let suffix = 1;
  while (used.has(`${base}|${suffix}`)) suffix += 1;
  return `${base}|${suffix}`;
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

/** In the attention filter because someone or something asked, not only because a reading is missing. */
function asksForAttention(reasons: readonly AttentionReason[]): boolean {
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
  const live = LIVE_STATUS.has(terminal.status);
  draft.presence = live ? "live" : "exited";
  draft.presenceDetail = live ? LIVE_DETAIL : EXITED_DETAIL;
  draft.liveKey = terminal.key;
  if (terminal.taskRunId) draft.taskRunId = terminal.taskRunId;
  else if (terminal.continuesRunId && !draft.taskRunId) draft.taskRunId = terminal.continuesRunId;
  for (const reason of terminalReasons(terminal)) draft.attention.add(reason);
  const title = clean(terminal.title || terminal.label, 80);
  if (title && draft.session === draft.kind) draft.session = title;
  else if (title && !draft.session) draft.session = title;
}

function fromCheckout(family: Family, item: WorktreeSummary, collisions: Set<string> | null, paths: PathIdentityOptions): Draft {
  return {
    repoPath: family.repoPath,
    repoLabel: family.repoLabel,
    kind: clean(item.agent_kind, 40),
    // A worktree at the layout's container has no slug. It is still a
    // checkout an agent layout holds; the row falls back to its kind.
    session: clean(item.session_slug, 80),
    checkout: checkoutLabel(item),
    worktreePath: item.path,
    checkoutPath: item.path,
    origin: "checkout",
    presence: "on-disk",
    presenceDetail: "A checkout on disk. This does not say a process is running.",
    attention: checkoutReasons(item, collisions, paths),
    liveKey: null,
    taskRunId: null,
    dirtyFiles: item.dirty_files,
    dirtyKnown: item.dirty_files !== null,
    parallelFloor: family.floor,
    diskSessions: family.diskSessions,
  };
}

function fromTerminal(terminal: PlaneTerminal, cwd: string | null, places: ReadonlyMap<string, Place>, paths: PathIdentityOptions): Draft {
  // Kind, checkout and repository come from where the process is. The tab
  // it was opened from speaks only when that directory is unknown.
  const where = cwd ?? terminal.repoPath;
  const place = nearestContaining(places, where, paths);
  const family = place?.family ?? nearestContaining(places, terminal.repoPath, paths)?.family;
  const kind = agentKind(where);
  const checkout = kind
    ? "Agent checkout"
    : place
      ? place.checkout
      : cwd
        ? "Outside the repositories read"
        : "Main checkout";
  return {
    repoPath: family?.repoPath || terminal.repoPath,
    repoLabel: family?.repoLabel || clean(terminal.repoPath, 80),
    kind: clean(kind || terminal.label, 40),
    session: clean(terminal.title || terminal.label, 80),
    checkout,
    worktreePath: where,
    checkoutPath: place?.path ?? (cwd ? null : terminal.repoPath),
    origin: "terminal",
    presence: "live",
    presenceDetail: LIVE_DETAIL,
    attention: new Set(),
    liveKey: null,
    taskRunId: null,
    dirtyFiles: null,
    dirtyKnown: false,
    parallelFloor: false,
    diskSessions: null,
  };
}

/**
 * Folds an attempt into the row of its own run.
 *
 * A run-backed row is named for the run: its provider is the kind, and its
 * task is the name. A GitPulse lane's own kind (`gitpulse`) says who made
 * the checkout, not which agent works in it. A row with this window's
 * terminal attached is the run being shown here, so `disconnected` is not
 * carried onto it.
 */
function foldTask(draft: Draft, task: PlaneTask): void {
  draft.taskRunId = task.runId;
  const provider = clean(task.provider, 40);
  if (provider) draft.kind = provider;
  const title = clean(task.title, 80);
  if (title) draft.session = title;
  for (const reason of taskReasons(task)) {
    if (reason === "disconnected" && draft.liveKey !== null) continue;
    draft.attention.add(reason);
  }
}

function fromTask(task: PlaneTask, places: ReadonlyMap<string, Place>, paths: PathIdentityOptions): Draft {
  const family = nearestContaining(places, task.repoPath, paths)?.family
    ?? nearestContaining(places, task.cwd, paths)?.family;
  const repoPath = family?.repoPath || task.repoPath;
  return {
    repoPath,
    repoLabel: family?.repoLabel || (repoPath ? clean(repoPath, 80) : "Repository not resolved"),
    kind: clean(task.provider, 40),
    session: clean(task.title, 80),
    checkout: clean(task.cwd, 80),
    worktreePath: task.cwd,
    checkoutPath: task.cwd || null,
    origin: "task",
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

/** True when this read listed every worktree and counted every agent. */
function completeRead(snapshot: InsightsSnapshot): boolean {
  return snapshot.agents.ok && !snapshot.agents.truncated && !snapshot.worktrees.truncated;
}

/**
 * Projects probes, live terminals and task attempts into one plane.
 *
 * A failed or skipped probe contributes a gap and no rows. Rows from a
 * repository whose agent list was capped carry `parallelFloor`, and the
 * plane's `sessionsAreFloor` is set. `rows` is capped; `total` and every
 * count are not.
 *
 * Linear in the input: checkouts, places and runs are looked up in maps
 * keyed by `identityKey` and run id, and a directory's containing checkout
 * is found by walking its own ancestors, not by scanning every checkout.
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

  // Which repository each read probe belongs to. Every checkout of one
  // repository lists the same main worktree, so that path names the family;
  // a listing without one keeps the probe on its own.
  const readProbes: { probe: PlaneProbe; snapshot: InsightsSnapshot; family: string }[] = [];
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
    const main = snapshot.worktrees.items.find((item) => item.is_main);
    readProbes.push({ probe, snapshot, family: keyOf(main?.path || probe.path, paths) });
  }

  const members = new Map<string, typeof readProbes>();
  for (const entry of readProbes) {
    const list = members.get(entry.family);
    if (list) list.push(entry);
    else members.set(entry.family, [entry]);
  }

  /** Every listed worktree and probed path, to the repository it belongs to. */
  const places = new Map<string, Place>();
  /** Agent checkouts by path. A checkout is a row once, whichever tab listed it. */
  const checkouts = new Map<string, Draft>();
  const families: Family[] = [];

  for (const [familyKey, entries] of members) {
    // The repository is named for its main checkout when that was a tab;
    // otherwise for the first tab that read it.
    const named = entries.find((entry) => keyOf(entry.probe.path, paths) === familyKey) ?? entries[0];
    const complete = entries.filter((entry) => completeRead(entry.snapshot));
    const counted = entries.filter((entry) => entry.snapshot.agents.ok);
    const family: Family = {
      repoPath: named.probe.path,
      repoLabel: clean(named.probe.label || named.probe.path, 80),
      diskSessions: counted.length === 0 ? null : Math.max(...counted.map((entry) => entry.snapshot.agents.sessions)),
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
        reason: "Agent counts could not be read. The checkouts below are only the ones whose paths were listed.",
      });
    } else if (family.floor) {
      gaps.push({
        repoPath: family.repoPath,
        label: family.repoLabel,
        kind: "partial",
        reason: "The worktree list was capped. Session counts for this repository are a floor.",
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
          : clean(named.snapshot.collisions.error || "Collisions could not be read.", 240),
      });
    }
    for (const entry of entries) {
      const probeKey = keyOf(entry.probe.path, paths);
      if (!places.has(probeKey)) places.set(probeKey, { family, path: entry.probe.path, checkout: "Checkout" });
      for (const item of entry.snapshot.worktrees.items) {
        const key = keyOf(item.path, paths);
        if (!key) continue;
        const known = places.get(key);
        if (!known || known.checkout === "Checkout") {
          places.set(key, { family, path: item.path, checkout: item.is_main ? "Main checkout" : checkoutLabel(item) });
        }
        // The rule `agent_summary` counts by, so a row here is a session there.
        if (!item.agent_kind || checkouts.has(key)) continue;
        const draft = fromCheckout(family, item, collisions, paths);
        checkouts.set(key, draft);
        drafts.push(draft);
        family.listed += 1;
      }
    }
  }

  // One record per key. When a key repeats, the live record is the one shown,
  // so the row and the live count agree.
  const chosen = new Map<string, PlaneTerminal>();
  for (const terminal of input.terminals) {
    if (!keepTerminal(terminal) || !terminalInScope(terminal)) continue;
    const previous = chosen.get(terminal.key);
    if (!previous || (!LIVE_STATUS.has(previous.status) && LIVE_STATUS.has(terminal.status))) {
      chosen.set(terminal.key, terminal);
    }
  }

  let live = 0;
  for (const terminal of chosen.values()) {
    if (LIVE_STATUS.has(terminal.status)) live += 1;
    const cwd = knownCwd(terminal, paths);
    // A known directory decides alone. Falling back to the tab here is how a
    // process in `/tmp` used to land in the agent checkout it was opened from.
    const host = nearestContaining(checkouts, cwd ?? terminal.repoPath, paths);
    if (host && host.liveKey === null) {
      attach(host, terminal);
      continue;
    }
    const draft: Draft = host
      ? { ...host, origin: "terminal", attention: new Set(host.attention), liveKey: null, taskRunId: null }
      : fromTerminal(terminal, cwd, places, paths);
    attach(draft, terminal);
    // A second process in a checkout is its own row, not a rewrite of the first.
    if (host) draft.session = clean(terminal.title || terminal.label || host.session, 80);
    drafts.push(draft);
  }

  let taskAttempts: number | null = null;
  let tasksFloor = false;
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
      tasksFloor = true;
      gaps.push({
        repoPath: "",
        label: "Tasks",
        kind: "partial",
        reason: "The task-attempt list was capped. Counts of attempts are a floor.",
      });
    }
    const byRun = new Map<string, Draft>();
    for (const draft of drafts) {
      if (draft.taskRunId && !byRun.has(draft.taskRunId)) byRun.set(draft.taskRunId, draft);
    }
    const attempts = new Set<string>();
    for (const task of input.tasks.tasks) {
      if (!task.runId) continue;
      attempts.add(task.runId);
      const existing = byRun.get(task.runId);
      if (existing) {
        foldTask(existing, task);
        continue;
      }
      // The checkout the attempt runs in hosts it only while nothing else
      // does: a terminal of another run (or of no run) in that checkout is
      // not this attempt, and folding it there would show one row as both
      // live and "not shown in this window".
      const host = nearestContaining(checkouts, task.cwd, paths);
      if (host && host.liveKey === null && host.taskRunId === null) {
        foldTask(host, task);
        byRun.set(task.runId, host);
        continue;
      }
      // P2, kept: an attempt with nothing to report and no checkout row to
      // fold into adds no row. `monitorAttempt` gives a running attempt with
      // no terminal here `disconnected`, and one not started `unstarted`, so
      // an attempt reaching this line has a terminal in this window already
      // (whose row carries it when that terminal is in scope) and asks for
      // nothing. A row for it would count one process twice.
      if (taskReasons(task).length === 0) continue;
      const draft = fromTask(task, places, paths);
      drafts.push(draft);
      byRun.set(task.runId, draft);
    }
    taskAttempts = attempts.size;
  }

  // One repository has one parallel count, computed once. A row that did not
  // come from the agent summary (a terminal in the main tree) must not report
  // a smaller number than the checkout beside it. An attempt whose repository
  // was not resolved is a repository of its own.
  const groups = new Map<string, { size: number; disk: number | null; floor: boolean }>();
  const groupKeys = drafts.map((draft, index) => draft.repoPath ? keyOf(draft.repoPath, paths) : `\u0000${index}`);
  drafts.forEach((draft, index) => {
    const group = groups.get(groupKeys[index]) ?? { size: 0, disk: null, floor: false };
    group.size += 1;
    if (draft.diskSessions !== null) group.disk = Math.max(group.disk ?? 0, draft.diskSessions);
    group.floor = group.floor || draft.parallelFloor;
    groups.set(groupKeys[index], group);
  });

  const finished: AgentRow[] = [];
  const used = new Set<string>();
  let needing = 0;
  let unread = 0;
  drafts.forEach((draft, index) => {
    const group = groups.get(groupKeys[index])!;
    const parallelFloor = group.floor || (group.disk !== null && group.disk > group.size);
    const parallelCount = group.disk === null ? group.size : Math.max(group.size, group.disk);
    if (parallelFloor) sessionsAreFloor = true;
    const attention = orderReasons(draft.attention);
    if (asksForAttention(attention)) needing += 1;
    else if (wantsAttention(attention)) unread += 1;
    const id = uniqueRowId(
      [keyOf(draft.repoPath, paths), keyOf(draft.worktreePath, paths), draft.liveKey ?? "", draft.taskRunId ?? ""].join("|"),
      used,
    );
    used.add(id);
    finished.push({
      id,
      repoPath: draft.repoPath,
      repoLabel: draft.repoLabel || clean(draft.repoPath, 80),
      kind: draft.kind || "agent",
      session: draft.session || draft.kind || "agent",
      checkout: draft.checkout,
      worktreePath: draft.worktreePath,
      checkoutPath: draft.checkoutPath,
      origin: draft.origin,
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
  });

  finished.sort((a, b) =>
    urgentRank(a.attention) - urgentRank(b.attention)
    || a.repoLabel.localeCompare(b.repoLabel)
    || a.kind.localeCompare(b.kind)
    || a.session.localeCompare(b.session)
    || a.id.localeCompare(b.id));

  let checkoutCount = 0;
  let checkoutsFloor = false;
  for (const family of families) {
    checkoutCount += Math.max(family.listed, family.diskSessions ?? 0);
    if (family.floor) checkoutsFloor = true;
  }

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
    checkouts: checkoutCount,
    checkoutsFloor,
    live,
    taskAttempts,
    tasksWatched: input.tasks !== null,
    tasksFloor,
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

/**
 * The rows of one repository, for a view opened from that repository (Fleet's
 * agent count). A row belongs when its repository — or the checkout it sits
 * in — is the scope path under the same identity rule the tab strip uses, so
 * a scope named by any checkout of the repository finds the same rows. A row
 * whose repository was not resolved belongs to no scope. A null scope keeps
 * every row.
 */
export function scopeToRepository(
  rows: readonly AgentRow[],
  scope: string | null,
  paths: PathIdentityOptions,
): AgentRow[] {
  const key = scope ? identityKey(scope, paths) : null;
  if (!key) return [...rows];
  return rows.filter((row) =>
    (row.repoPath !== "" && identityKey(row.repoPath, paths) === key) ||
    (row.checkoutPath !== null && identityKey(row.checkoutPath, paths) === key));
}

/** `1 agent checkout`, `2 agent checkouts` — the app's one pluralizer, re-exported for the view. */
export { plural };

/**
 * One line naming what the rows are: agent checkouts, live terminals and task
 * attempts, then how many rows are in the attention filter and why. Each
 * count is the plane's own, computed before the row cap.
 */
export function planeHeadline(plane: AgentPlane, visible: number): string {
  const parts = [
    `${plane.checkoutsFloor ? "at least " : ""}${plural(plane.checkouts, "agent checkout", "agent checkouts")}`,
    plural(plane.live, "live terminal", "live terminals"),
  ];
  if (plane.tasksWatched) {
    parts.push(plane.taskAttempts === null
      ? "task attempts not read"
      : `${plane.tasksFloor ? "at least " : ""}${plural(plane.taskAttempts, "task attempt", "task attempts")}`);
  }
  parts.push(`${plane.attention.needing} need attention`);
  if (plane.attention.unread > 0) parts.push(`${plane.attention.unread} not fully read`);
  // Rows past the cap are not hidden by the filter. `visible` counts how many
  // of the returned rows the caller kept.
  const hidden = plane.rows.length - visible;
  if (hidden > 0) parts.push(`${hidden} hidden by the filter`);
  const notRead = plane.failed + plane.skipped;
  if (notRead > 0) parts.push(`${plural(notRead, "repository", "repositories")} not read`);
  return parts.join(" · ");
}
