/**
 * What one repository's agents ask of the reader, for the header.
 *
 * The header says it in two places: the per-repository Agents pill in the view
 * bar and the attention mark on a background repository tab. Both read the one
 * Agents plane (`projectAgentPlane`) through `scopeToRepository`, so the
 * answer cannot drift from the Agents surface's own. This module adds no
 * attention model of its own: the reasons and their order are `ATTENTION_REASONS`,
 * and the terminal side comes from `sessionActivity`'s `ATTENTION_EVENTS`
 * through the `attention` field of each terminal.
 *
 * Unknown stays unknown. A plane that was never swept, or whose read of this
 * repository failed or was skipped, is not "no agents".
 */

import { identityKey, type PathIdentityOptions } from "../repos/paths";
import type { AttentionKind } from "../terminal/sessionActivity";
import type { AgentRepoTarget } from "./store";
import {
  ATTENTION_REASONS,
  scopeToRepository,
  type AgentPlane,
  type AgentRow,
  type AttentionReason,
  type PlaneProbe,
  type PlaneTerminal,
} from "./plane";

/** Reasons that mean "this repository's agents are waiting on the reader". */
const ASKS: ReadonlySet<AttentionReason> = new Set([
  "needs-you",
  "error",
  "pending",
  "blocked",
  "signalled",
  "finished",
]);

/** Reasons that mean the plane could not say, rather than that nothing is wrong. */
const UNREAD: ReadonlySet<AttentionReason> = new Set(["unscanned", "unprobed", "unmeasured"]);

export type RepoAgentTone = "danger" | "warn" | "info";

export interface RepoAgentPill {
  /**
   * `attention`: a row in this repository asks for the reader.
   * `active`: agents are live and none asks.
   * `unknown`: the plane has not read this repository, or could not.
   * `none`: read, and no agents.
   */
  state: "attention" | "active" | "unknown" | "none";
  /** Live agent terminals in this repository. */
  live: number;
  /** The most urgent reason, in `ATTENTION_REASONS` order. Null unless `state` is `attention`. */
  reason: AttentionReason | null;
  tone: RepoAgentTone | null;
  /** The word that goes with the reason, short enough for a pill. */
  phrase: string;
  /**
   * Why the pill says "unknown", for its tooltip. Empty for every other state.
   * "Not read yet" is a state with a cause, never a shrug.
   */
  why: string;
}

const PHRASE: Partial<Record<AttentionReason, string>> = {
  "needs-you": "Needs you",
  error: "Error",
  pending: "Waiting",
  blocked: "Blocked",
  signalled: "Asked",
  finished: "Finished",
};

function toneOf(reason: AttentionReason): RepoAgentTone {
  if (reason === "error") return "danger";
  if (reason === "finished") return "info";
  return "warn";
}

/** Terminals as the plane reads them: the one mapping the header and the Agents view share. */
export function planeTerminals(
  records: readonly {
    key: string;
    repoPath: string;
    label: string;
    title?: string;
    status: string;
    sessionId?: string | null;
    taskRunId?: string | null;
    continuesRunId?: string | null;
  }[],
  activity: ReadonlyMap<string, { title: string | null; attention: { kind: AttentionKind } | null }>,
  directories: ReadonlyMap<string, string>,
): PlaneTerminal[] {
  return records.map((record) => {
    const seen = record.sessionId ? activity.get(record.sessionId) : undefined;
    return {
      key: record.key,
      repoPath: record.repoPath,
      label: record.label,
      title: seen?.title || record.title || record.label,
      status: record.status,
      sessionId: record.sessionId ?? "",
      taskRunId: record.taskRunId ?? "",
      continuesRunId: record.continuesRunId ?? "",
      // Unknown stays unknown: no directory is not the repository root.
      cwd: record.sessionId ? directories.get(record.sessionId) ?? null : null,
      attention: seen?.attention?.kind ?? null,
    };
  });
}

function mostUrgent(rows: readonly AgentRow[], pick: ReadonlySet<AttentionReason>): AttentionReason | null {
  const found = new Set<AttentionReason>();
  const walk = (row: AgentRow) => {
    for (const reason of row.attention) if (pick.has(reason)) found.add(reason);
    for (const child of row.children) walk(child);
  };
  for (const row of rows) walk(row);
  return ATTENTION_REASONS.find((reason) => found.has(reason)) ?? null;
}

function countLive(rows: readonly AgentRow[]): number {
  let live = 0;
  const walk = (row: AgentRow) => {
    if (row.origin === "terminal" && row.presence === "live") live += 1;
    for (const child of row.children) walk(child);
  };
  for (const row of rows) walk(row);
  return live;
}

/**
 * The probe that read `repoPath`: the one for its own path, or the one for the
 * repository it is a worktree of (a sweep reads each repository once, at its
 * family root, and lists the other checkouts in the snapshot).
 */
export function probeFor(
  probes: readonly PlaneProbe[],
  repoPath: string,
  paths: PathIdentityOptions,
): PlaneProbe | undefined {
  const key = identityKey(repoPath, paths);
  return probes.find((candidate) =>
    identityKey(candidate.path, paths) === key ||
    (candidate.snapshot?.worktrees?.items ?? []).some((item) => identityKey(item.path, paths) === key));
}

const asked = new Set<string>();

/**
 * Starts the Agents read for a repository no read has covered, once.
 *
 * The read is the shared sweep (`agentPlaneStore`), the same one the Agents
 * surface starts; this only asks for it when row 2 mounts on a repository
 * that has no probe, so the pill does not stay "not read" until someone opens
 * Agents. It asks once per repository: a read that was skipped or failed is
 * reported by the pill with its cause, not retried in a loop. Returns whether
 * it started one.
 */
export function ensurePlaneRead(input: {
  store: { refresh(targets: readonly AgentRepoTarget[]): Promise<void> };
  state: { probes: readonly PlaneProbe[]; scanning: boolean };
  repoPath: string;
  targets: readonly AgentRepoTarget[];
  paths: PathIdentityOptions;
}): boolean {
  const { store, state, repoPath, targets, paths } = input;
  if (!repoPath || state.scanning || targets.length === 0) return false;
  if (probeFor(state.probes, repoPath, paths)) return false;
  const key = identityKey(repoPath, paths);
  if (asked.has(key)) return false;
  asked.add(key);
  void store.refresh(targets);
  return true;
}

/** Forgets what `ensurePlaneRead` asked for. Tests only. */
export function resetPlaneReadRequests(): void {
  asked.clear();
}

/**
 * The header's answer for `repoPath`.
 *
 * `probes` is what the last sweep read; it decides unknown from none, because
 * the plane alone cannot tell a repository with no agents from one it never
 * read.
 */
export function repoAgentPill(input: {
  plane: AgentPlane;
  probes: readonly PlaneProbe[];
  repoPath: string;
  paths: PathIdentityOptions;
  /** True while the shared sweep is reading: an unread repository is then "reading", not "never read". */
  scanning?: boolean;
}): RepoAgentPill {
  const { plane, probes, repoPath, paths, scanning = false } = input;
  const rows = scopeToRepository(plane.roots, repoPath, paths);
  const live = countLive(rows);
  const reason = mostUrgent(rows, ASKS);
  if (reason) {
    return { state: "attention", live, reason, tone: toneOf(reason), phrase: PHRASE[reason] ?? "Needs you", why: "" };
  }
  const key = identityKey(repoPath, paths);
  const probe = probeFor(probes, repoPath, paths);
  const gap = plane.gaps.some((entry) => entry.repoPath !== "" && identityKey(entry.repoPath, paths) === key);
  const unread = mostUrgent(rows, UNREAD) !== null;
  if (!probe || probe.skipped || Boolean(probe.error) || gap || unread) {
    const why = !probe
      ? scanning
        ? "The agents read for this repository is running now."
        : "The agents read has not covered this repository yet."
      : probe.skipped
        ? probe.skipReason === "cap"
          ? "The agents read skipped this repository: more are open than one read covers."
          : "The agents read ran out of time before reaching this repository."
        : probe.error
          ? `The agents read failed: ${probe.error}`
          : gap
            ? "The agents read reported a gap for this repository."
            : "Part of the agents read for this repository could not be measured.";
    return { state: "unknown", live, reason: null, tone: null, phrase: !probe && scanning ? "Reading" : "Not read", why };
  }
  // Agent checkouts on disk are not running agents: only a live terminal makes
  // the pill appear when nothing asks for the reader.
  if (live > 0) {
    return { state: "active", live, reason: null, tone: null, phrase: "Running", why: "" };
  }
  return { state: "none", live: 0, reason: null, tone: null, phrase: "", why: "" };
}
