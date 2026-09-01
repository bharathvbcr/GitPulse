/**
 * Work-in-progress across the whole workspace.
 *
 * Every risk this summarizes is the same risk in a different costume: work
 * that exists only on this machine, in this checkout, right now. Uncommitted
 * edits, a half-finished merge, commits that were never pushed, a stash nobody
 * remembers — each is invisible from any other tab, and all of them are lost
 * or stranded by the ordinary actions of closing a window, switching branches,
 * or letting an agent run unattended.
 *
 * The point is a single answer to "is it safe to walk away". So the model is
 * deliberately conservative in one direction: **anything unknown counts as at
 * risk.** A repository whose status could not be read is reported as unknown,
 * never as clean, because "we could not check" and "we checked and it is
 * clean" must not render the same.
 */

import type { OperationState } from "./operation";

/** The per-repository facts this summary is computed from. */
export interface RepoWipInput {
  path: string;
  label: string;
  /** Files with working-tree or index changes. */
  changedFiles: number;
  /** Of those, how many carry conflict markers. */
  conflictedFiles: number;
  /** Commits on the current branch not present upstream. */
  unpushedCommits: number;
  /** Entries on the stash stack. */
  stashEntries: number;
  /** The parked operation facet, if this repository's probe ran. */
  operation: OperationState;
  /** True when this repository's snapshot failed to load at all. */
  loadFailed: boolean;
  /** True before the first snapshot has landed; not yet knowable. */
  hydrated: boolean;
}

/** Why one repository is considered to hold work at risk. */
export type WipReasonKind =
  | "conflicts"
  | "operation"
  | "uncommitted"
  | "unpushed"
  | "stash"
  | "unknown";

export interface WipReason {
  kind: WipReasonKind;
  /** One short clause, e.g. "3 files with conflicts". */
  detail: string;
}

export interface RepoWip {
  path: string;
  label: string;
  reasons: WipReason[];
  /** Highest-severity reason present, for sorting and for the one-line answer. */
  severity: WipReasonKind | null;
}

export interface WorkspaceWip {
  /** Repositories holding work at risk, most severe first. */
  repos: RepoWip[];
  /** Repositories examined, including clean ones. */
  examined: number;
  /** Repositories whose state could not be determined. */
  unknown: number;
  /** True when every examined repository is known-clean. */
  allClear: boolean;
}

/**
 * Severity order, worst first.
 *
 * `unknown` ranks above merely-uncommitted deliberately: not knowing whether a
 * repository holds work is a worse position to act from than knowing it does.
 * Conflicts and a parked operation outrank both — they block every other
 * action until resolved.
 */
const SEVERITY_ORDER: readonly WipReasonKind[] = [
  "conflicts",
  "operation",
  "unknown",
  "uncommitted",
  "unpushed",
  "stash",
];

function rank(kind: WipReasonKind): number {
  const index = SEVERITY_ORDER.indexOf(kind);
  // An unrecognized kind from a newer caller sorts last rather than first, so
  // it can never displace a real conflict at the top of the list.
  return index < 0 ? SEVERITY_ORDER.length : index;
}

function plural(count: number, singular: string, pluralForm = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : pluralForm}`;
}

/** Computes one repository's at-risk reasons, most severe first. */
export function repoWip(input: RepoWipInput): RepoWip {
  const reasons: WipReason[] = [];

  if (!input.hydrated || input.loadFailed || input.operation.probeFailed) {
    reasons.push({
      kind: "unknown",
      detail: input.loadFailed
        ? "state could not be read"
        : !input.hydrated
          ? "not loaded yet"
          : "operation state unknown",
    });
  }

  if (input.conflictedFiles > 0) {
    reasons.push({
      kind: "conflicts",
      detail: `${plural(input.conflictedFiles, "file")} with conflicts`,
    });
  }

  if (input.operation.operation) {
    reasons.push({
      kind: "operation",
      detail: `${input.operation.operation.kind === "Merge" ? "merge" : "operation"} in progress`,
    });
  }

  // Conflicted files are already counted above; counting them again here would
  // double-report the same file as two separate risks.
  const uncommitted = Math.max(0, input.changedFiles - input.conflictedFiles);
  if (uncommitted > 0) {
    reasons.push({
      kind: "uncommitted",
      detail: `${plural(uncommitted, "uncommitted change")}`,
    });
  }

  if (input.unpushedCommits > 0) {
    reasons.push({
      kind: "unpushed",
      detail: `${plural(input.unpushedCommits, "unpushed commit")}`,
    });
  }

  if (input.stashEntries > 0) {
    reasons.push({
      kind: "stash",
      detail: `${plural(input.stashEntries, "stash entry", "stash entries")}`,
    });
  }

  reasons.sort((a, b) => rank(a.kind) - rank(b.kind));
  return {
    path: input.path,
    label: input.label,
    reasons,
    severity: reasons[0]?.kind ?? null,
  };
}

/** Rolls per-repository facts into the workspace answer. */
export function summarizeWorkspace(inputs: readonly RepoWipInput[]): WorkspaceWip {
  const all = inputs.map(repoWip);
  const repos = all
    .filter((repo) => repo.reasons.length > 0)
    .sort((a, b) => {
      const bySeverity = rank(a.severity ?? "stash") - rank(b.severity ?? "stash");
      // Stable, human-predictable ordering within a severity band.
      return bySeverity !== 0 ? bySeverity : a.label.localeCompare(b.label);
    });
  const unknown = all.filter((repo) =>
    repo.reasons.some((reason) => reason.kind === "unknown"),
  ).length;
  return {
    repos,
    examined: all.length,
    unknown,
    allClear: repos.length === 0 && all.length > 0,
  };
}

/**
 * The workspace answer in one sentence.
 *
 * Says "nothing" only when every repository was examined AND every one came
 * back clean. With nothing open at all it says so instead, because "no
 * uncommitted work" would be a claim about repositories that were never
 * looked at.
 */
export function describeWorkspace(summary: WorkspaceWip): string {
  if (summary.examined === 0) return "No repositories are open.";
  if (summary.allClear) {
    return summary.examined === 1
      ? "No uncommitted work — the repository is clean."
      : `No uncommitted work across ${summary.examined} repositories.`;
  }
  const count = summary.repos.length;
  const head = `${plural(count, "repository", "repositories")} with work in progress`;
  const top = summary.repos[0];
  return top ? `${head} — ${top.label}: ${top.reasons[0].detail}` : head;
}

/**
 * Whether closing the workspace right now would strand work.
 *
 * Unknown state counts as blocking: a confirmation the user can dismiss is
 * cheap, and silently closing over an unread repository is not.
 */
export function shouldWarnBeforeClosing(summary: WorkspaceWip): boolean {
  return summary.repos.some((repo) =>
    repo.reasons.some((reason) =>
      reason.kind === "conflicts" ||
      reason.kind === "operation" ||
      reason.kind === "uncommitted" ||
      reason.kind === "unknown",
    ),
  );
}

/**
 * Reasons that make a repository a bad target for an unattended bulk action.
 *
 * Used by workspace-wide pull/fetch runs to skip — and *report* skipping —
 * repositories where the operation would fail or make things worse. Returns
 * null when the repository is a fine target.
 */
export function bulkSkipReason(input: RepoWipInput): string | null {
  if (input.loadFailed) return "Repository state could not be read.";
  if (!input.hydrated) return "Repository has not finished loading.";
  if (input.operation.operation) {
    return `A ${input.operation.operation.kind === "Merge" ? "merge" : "git operation"} is in progress here.`;
  }
  if (input.conflictedFiles > 0) {
    return `${plural(input.conflictedFiles, "file")} still have conflicts.`;
  }
  return null;
}
