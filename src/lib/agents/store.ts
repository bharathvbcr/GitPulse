/**
 * The sweep behind the Agents plane.
 *
 * One insights snapshot per open repository, four at a time, and a deadline
 * that stops the sweep from starting another one. A snapshot already in
 * flight is allowed to finish: cancelling a git process from here is not
 * something the command offers, and reporting a read that completed as a
 * skip would be the other lie. Repositories past the cap are skipped before
 * any of that, so the cap is a decision rather than a timeout.
 *
 * A newer sweep, or a cancel, drops whatever the older one still has in
 * flight. The probes on screen stay until a sweep finishes, so a refresh
 * does not blank the plane into "no sessions" while it is still reading.
 */

import { writable, type Readable } from "svelte/store";
import { mapWithConcurrency } from "../async/pool";
import { getInsightsSnapshot } from "../insights/client";
import type { InsightsSnapshot } from "../insights/types";
import { identityKey, isCaseInsensitiveFs, type PathIdentityOptions } from "../repos/paths";
import { formatError } from "../ui/formatError";
import { MAX_AGENT_REPOS, type PlaneProbe } from "./plane";

/** How long a sweep may keep starting snapshots. */
export const AGENT_SWEEP_DEADLINE_MS = 10_000;
/** Snapshots in flight at once. Matches the rest of the workspace fan-out. */
export const AGENT_SWEEP_CONCURRENCY = 4;

export interface AgentRepoTarget {
  path: string;
  label: string;
}

/**
 * One open checkout of a repository.
 *
 * `family` is the common Git directory. Every worktree of one repository
 * shares it, and a snapshot from any of them lists the others, so sweeping
 * each open tab reads the same sessions twice.
 */
export interface SweepTab {
  path: string;
  label: string;
  family?: string | null;
  familyRoot?: string | null;
}

/**
 * The repositories a sweep should read.
 *
 * Tabs with the same family are one repository. The target is the family's
 * own checkout when one of them is that checkout, so the row is named for the
 * repository. A tab whose family could not be read stays on its own: folding
 * it into a neighbour would mix two repositories.
 */
export function sweepTargets(tabs: readonly SweepTab[]): AgentRepoTarget[] {
  const groups = new Map<string, SweepTab[]>();
  const order: string[] = [];
  for (const tab of tabs) {
    if (!tab.path) continue;
    const key = tab.family || tab.path;
    const list = groups.get(key);
    if (list) list.push(tab);
    else {
      groups.set(key, [tab]);
      order.push(key);
    }
  }
  return order.map((key) => {
    const group = groups.get(key) ?? [];
    const root = group.find((tab) => tab.familyRoot && tab.path === tab.familyRoot) ?? group[0];
    return { path: root.path, label: root.label || root.path };
  });
}

export interface AgentPlaneState {
  readonly probes: readonly PlaneProbe[];
  readonly scanning: boolean;
}

export interface AgentPlaneStore extends Readable<AgentPlaneState> {
  /** Reads these repositories. A call already running is superseded. */
  refresh(targets: readonly AgentRepoTarget[]): Promise<void>;
  /** Drops the sweep in flight and leaves the last finished probes. */
  cancel(): void;
}

export interface AgentPlaneStoreDeps {
  readonly snapshot?: (path: string) => Promise<InsightsSnapshot>;
  readonly now?: () => number;
  readonly paths?: PathIdentityOptions;
}

const INITIAL: AgentPlaneState = { probes: [], scanning: false };

function skip(target: AgentRepoTarget, reason: "deadline" | "cap"): PlaneProbe {
  return {
    path: target.path,
    label: target.label,
    snapshot: null,
    error: "",
    skipped: true,
    skipReason: reason,
  };
}

export function createAgentPlaneStore(deps: AgentPlaneStoreDeps = {}): AgentPlaneStore {
  const snapshot = deps.snapshot ?? getInsightsSnapshot;
  const now = deps.now ?? (() => Date.now());
  const paths = deps.paths ?? { caseInsensitive: isCaseInsensitiveFs() };
  const store = writable<AgentPlaneState>(INITIAL);
  let epoch = 0;

  function dedupe(targets: readonly AgentRepoTarget[]): AgentRepoTarget[] {
    const seen = new Set<string>();
    const unique: AgentRepoTarget[] = [];
    for (const target of targets) {
      if (!target.path) continue;
      const key = identityKey(target.path, paths) || target.path;
      if (seen.has(key)) continue;
      seen.add(key);
      unique.push(target);
    }
    return unique;
  }

  async function refresh(targets: readonly AgentRepoTarget[]): Promise<void> {
    const mine = ++epoch;
    const unique = dedupe(targets);
    const reading = unique.slice(0, MAX_AGENT_REPOS);
    const overflow = unique.slice(MAX_AGENT_REPOS);
    store.update((state) => ({ ...state, scanning: true }));
    const started = now();
    const probes: Array<PlaneProbe | undefined> = new Array(reading.length);
    await mapWithConcurrency(reading.length, AGENT_SWEEP_CONCURRENCY, async (index) => {
      if (mine !== epoch) return;
      const target = reading[index];
      if (now() - started >= AGENT_SWEEP_DEADLINE_MS) {
        probes[index] = skip(target, "deadline");
        return;
      }
      try {
        const read = await snapshot(target.path);
        if (mine !== epoch) return;
        probes[index] = {
          path: target.path,
          label: target.label,
          snapshot: read,
          error: "",
          skipped: false,
          skipReason: "",
        };
      } catch (err: unknown) {
        if (mine !== epoch) return;
        probes[index] = {
          path: target.path,
          label: target.label,
          snapshot: null,
          error: formatError(err),
          skipped: false,
          skipReason: "",
        };
      }
    });
    if (mine !== epoch) return;
    const finished = probes.map((probe, index) => probe ?? {
      path: reading[index].path,
      label: reading[index].label,
      snapshot: null,
      error: "The read did not finish.",
      skipped: false,
      skipReason: "" as const,
    });
    store.set({
      probes: [...finished, ...overflow.map((target) => skip(target, "cap"))],
      scanning: false,
    });
  }

  function cancel(): void {
    epoch += 1;
    store.update((state) => ({ ...state, scanning: false }));
  }

  return { subscribe: store.subscribe, refresh, cancel };
}

export const agentPlaneStore = createAgentPlaneStore();
