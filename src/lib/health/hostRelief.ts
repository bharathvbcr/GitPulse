/**
 * The pressure-relief supervisor's status as Health and the status bar read it
 * (`cmd_host_relief_status`, from `host-relief.v1.json`).
 *
 * The rule they share: a status that is stale or could not be read is never
 * shown as "nothing is paused". It says it could not be checked.
 */
import { readable, type Readable } from "svelte/store";
import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import { humanBytes } from "../storage/format";

export type ReliefMode = "off" | "advise" | "act";
export type ReliefRung = "none" | "slow" | "freeze";
export type ReliefBinding = "swap" | "compressor_segments" | "compressor_thrash" | "memory_pressure";

export interface FrozenTree {
  pid: number;
  start_us: number;
  agent_root_pid: number;
  terminal_session_id: string | null;
  repo_path: string;
  command: string;
  footprint_bytes: number;
  since_ms: number;
  /** Advise mode: what Act would have paused. Nothing is stopped. */
  advised_only: boolean;
}

export type ReliefActionKind = "slow" | "freeze" | "resume" | "cap_resume" | "refused";

export interface ReliefAction {
  at_ms: number;
  kind: ReliefActionKind;
  detail: string;
}

export interface ReliefFailure {
  at_ms: number;
  pid: number;
  what: string;
}

export interface ReliefStatus {
  version: number;
  written_at_ms: number;
  mode: ReliefMode;
  rung: ReliefRung;
  binding: ReliefBinding | null;
  slowed: number;
  frozen: FrozenTree[];
  last_action: ReliefAction | null;
  failures: ReliefFailure[];
  cap_hits: number;
}

/** `ReadOutcome` in `sysload/relief_status.rs`, as serialised. */
export type ReliefOutcome =
  | { state: "absent" }
  | { state: "fresh"; status: ReliefStatus }
  | { state: "stale"; status: ReliefStatus; age_ms: number }
  | { state: "unreadable"; reason: string };

/** How often a watching view rereads the file; it is small and local. */
export const HOST_RELIEF_POLL_MS = 5000;

/** The trees that are actually stopped; an advised-only entry is not one. */
export function pausedTrees(status: ReliefStatus): FrozenTree[] {
  return status.frozen.filter((tree) => !tree.advised_only);
}

export interface ReliefChipInput {
  paused: number;
  /** The count is the last thing known, or the status could not be read. */
  unverified: boolean;
}

export const NO_RELIEF_CHIP: ReliefChipInput = { paused: 0, unverified: false };

/** What the status bar needs from an outcome. */
export function reliefChipInput(outcome: ReliefOutcome | null): ReliefChipInput {
  if (!outcome || outcome.state === "absent") return NO_RELIEF_CHIP;
  if (outcome.state === "unreadable") return { paused: 0, unverified: true };
  return {
    paused: pausedTrees(outcome.status).length,
    unverified: outcome.state === "stale",
  };
}

/** What each binding means, in words. */
export function reliefBindingLabel(binding: ReliefBinding | null): string {
  switch (binding) {
    case "swap":
      return "swap is nearly full";
    case "compressor_segments":
      return "the memory compressor is nearly out of segments";
    case "compressor_thrash":
      return "the memory compressor is thrashing";
    case "memory_pressure":
      return "memory pressure is critical";
    default:
      return "memory is nearly exhausted";
  }
}

const MODE_LABEL: Record<ReliefMode, string> = {
  off: "Off",
  advise: "Advise only",
  act: "Act",
};

export function reliefModeLabel(mode: ReliefMode): string {
  return MODE_LABEL[mode];
}

export function reliefRungLabel(rung: ReliefRung): string {
  switch (rung) {
    case "none":
      return "Nothing slowed or paused";
    case "slow":
      return "Agent processes slowed";
    case "freeze":
      return "A build or test is paused";
  }
}

/** "the last 3 min", or the stated age. */
export function reliefAge(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  return minutes < 60 ? `${minutes} min` : `${Math.round(minutes / 60)} h`;
}

/** The repository's last path component: the whole path is in the title. */
export function repoName(path: string): string {
  const parts = path.split(/[\\/]/).filter((part) => part.length > 0);
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

export interface PausedRow {
  key: string;
  command: string;
  repo: string;
  repoPath: string;
  agent: string;
  footprint: string;
  since: string;
}

/** One row per paused tree, `now` supplied so the age is testable. */
export function pausedRows(status: ReliefStatus, now: number): PausedRow[] {
  return pausedTrees(status).map((tree) => ({
    key: `${tree.pid}:${tree.start_us}`,
    command: tree.command,
    repo: repoName(tree.repo_path),
    repoPath: tree.repo_path,
    agent: tree.terminal_session_id
      ? `session ${tree.terminal_session_id}`
      : `agent pid ${tree.agent_root_pid}`,
    footprint: humanBytes(tree.footprint_bytes),
    since: `${reliefAge(now - tree.since_ms)} ago`,
  }));
}

/**
 * The one sentence Health leads with, which never says "nothing is paused" for
 * a status that was not read.
 */
export function reliefHeadline(outcome: ReliefOutcome | null): string {
  if (!outcome) return "Reading the memory protection status.";
  switch (outcome.state) {
    case "absent":
      return "No memory protection status has been published. Nothing is known to be paused.";
    case "unreadable":
      return `The memory protection status couldn't be read, so whether anything is paused is unknown: ${outcome.reason}`;
    case "stale":
      return `The memory protection status was last written ${reliefAge(outcome.age_ms)} ago, so the supervisor may no longer be running. What follows may be out of date, and a paused process may still be paused.`;
    case "fresh": {
      const paused = pausedTrees(outcome.status).length;
      if (paused > 0) return `${paused} build or test paused by memory protection. It resumes automatically.`;
      if (outcome.status.rung === "slow") {
        return `${outcome.status.slowed} agent process(es) slowed; nothing is paused.`;
      }
      return "Memory protection is idle: nothing is slowed or paused.";
    }
  }
}

/** Asks the supervisor to resume everything it paused. */
export async function requestResumeAll(): Promise<void> {
  await invoke<void>("cmd_host_relief_request_resume");
}

/** One read of the status. Outside the app there is nothing to read. */
export async function loadHostRelief(): Promise<ReliefOutcome | null> {
  if (!isTauri()) return null;
  return invoke<ReliefOutcome>("cmd_host_relief_status");
}

/**
 * The status as last read, kept fresh while anything subscribes. A read that
 * failed is an `unreadable` outcome, never the last good one: a status that
 * could not be read must not keep showing what it said before.
 */
export const hostRelief: Readable<ReliefOutcome | null> = readable<ReliefOutcome | null>(
  null,
  (set) => {
    if (!isTauri()) return () => {};
    let live = true;
    const read = () => {
      void loadHostRelief()
        .then((outcome) => {
          if (live) set(outcome);
        })
        .catch((cause: unknown) => {
          if (live) {
            set({
              state: "unreadable",
              reason: cause instanceof Error ? cause.message : String(cause),
            });
          }
        });
    };
    read();
    const stop = createAdaptiveTimer(read, HOST_RELIEF_POLL_MS);
    return () => {
      live = false;
      stop();
    };
  },
);
