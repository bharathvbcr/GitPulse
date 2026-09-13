/**
 * How an agent-host integration plan is worded before it is applied.
 *
 * Separate from the panel because the wording is the consent. `devmap
 * integrate` has no flag to write a repository's assets without also writing a
 * machine-wide MCP registration in the user's home directory, so applying is
 * all-or-nothing — and a summary that merged the two counts would ask for
 * agreement to edit `~/.claude.json` while appearing to ask about a project.
 */

import type { IntegrationHost, IntegrationKind, IntegrationPlan } from "../codeintel/types";

export const HOST_LABEL: Record<string, string> = {
  claude: "Claude Code",
  cursor: "Cursor",
  codex: "Codex",
};

export const KIND_LABEL: Record<IntegrationKind, string> = {
  guide: "Agent guide",
  project_mcp: "Project MCP",
  global_mcp: "Global MCP",
  hook: "Hook config",
  skill: "Skill",
};

/** Every host this panel offers, in the order the backend surveys them. */
export const INTEGRATION_HOSTS: readonly IntegrationHost[] = ["claude", "cursor", "codex"];

/**
 * Index a survey by the host that was *asked about*, never by the host the
 * payload names.
 *
 * A row's identity has to come from the request, because it is also the `each`
 * key: storing a response under the host it claims meant one mismatched
 * payload put two rows under the same key, and a keyed block with duplicates
 * throws and takes the whole panel down. Measured, not hypothesized — a
 * host-agnostic stub reproduced it immediately.
 *
 * A requested host with no matching plan comes back unavailable and says so,
 * which is the honest rendering of "the backend did not answer about this
 * one" and is not the same as "already current".
 */
export function indexPlansByHost(
  requested: readonly IntegrationHost[],
  returned: readonly IntegrationPlan[],
): Map<IntegrationHost, IntegrationPlan> {
  const byHost = new Map<IntegrationHost, IntegrationPlan>();
  for (const host of requested) {
    const match = returned.find((plan) => plan.host === host);
    byHost.set(
      host,
      match ?? {
        available: false,
        host,
        repo: returned[0]?.repo ?? "",
        applied: false,
        reason: `no integration plan came back for ${hostLabel(host)}`,
        entries: [],
        notes: [],
        repo_changes: 0,
        outside_changes: 0,
        protected: [],
      },
    );
  }
  return byHost;
}

/**
 * Accept a re-read only when it is about the host that was asked for.
 *
 * Same reason as above, one host at a time: a plan filed under the wrong key
 * is both a wrong render and a duplicate key waiting to happen.
 */
export function acceptPreview(
  host: IntegrationHost,
  fresh: IntegrationPlan,
): IntegrationPlan | { mismatch: string } {
  if (fresh.host !== host) {
    return {
      mismatch: `asked about ${hostLabel(host)} but the answer was about ${hostLabel(fresh.host)}`,
    };
  }
  return fresh;
}

export function hostLabel(host: string): string {
  return HOST_LABEL[host] ?? host;
}

export function kindLabel(kind: IntegrationKind): string {
  return KIND_LABEL[kind] ?? kind;
}

/** Everything applying would write, across both locations. */
export function totalChanges(plan: IntegrationPlan): number {
  return plan.repo_changes + plan.outside_changes;
}

function files(count: number): string {
  return `${count} file${count === 1 ? "" : "s"}`;
}

/** The one-line state of a host row. */
export function planSummary(plan: IntegrationPlan): string {
  if (!plan.available) return plan.reason ?? "could not be checked";
  if (totalChanges(plan) === 0) return "Already registered and current.";
  const head = `${files(plan.repo_changes)} in this repository`;
  return plan.outside_changes > 0
    ? `${head}, ${plan.outside_changes} in your home directory`
    : head;
}

/**
 * The confirmation text shown before writing.
 *
 * Names the home-directory writes explicitly whenever there are any, and says
 * that the repository files become visible in `git status` — the consequence a
 * user of a Git client cares about and the one thing the CLI's own output does
 * not mention.
 */
export function applyConfirmText(plan: IntegrationPlan): string {
  const scope =
    plan.outside_changes > 0
      ? `${files(plan.repo_changes)} in this repository and ${plan.outside_changes} outside it, in your home directory`
      : `${files(plan.repo_changes)} in this repository`;
  const lines = [
    `Write DevMap's ${hostLabel(plan.host)} integration?`,
    "",
    `This changes ${scope}.`,
  ];
  if (plan.repo_changes > 0) {
    lines.push("Repository files will appear in git status.");
  }
  if (plan.protected.length > 0) {
    lines.push(
      `Left alone because you wrote ${plan.protected.length === 1 ? "it" : "them"}: ${plan.protected.join(", ")}.`,
    );
  }
  return lines.join("\n");
}

/** Applying is only offered when there is something to write. */
export function canApply(plan: IntegrationPlan): boolean {
  return plan.available && totalChanges(plan) > 0;
}
