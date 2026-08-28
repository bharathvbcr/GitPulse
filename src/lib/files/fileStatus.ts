/**
 * Working-tree status decorations and dashboard aggregates. Porcelain v1
 * codes are two characters (`XY`); untracked is `??`. Classification is
 * centralized so the explorer pills and the live dashboard cannot drift.
 */

import { ancestorsOf } from "./fileTree";
import type { FileStatusScope } from "./fileQuery";

export interface StatusLike {
  path: string;
  status_code: string;
  is_staged: boolean;
  is_conflicted: boolean;
  additions: number;
  deletions: number;
}

export type FileChangeKind = "clean" | "staged" | "unstaged" | "untracked" | "conflict";

export interface StatusDashboard {
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  additions: number;
  deletions: number;
  dirty: number;
}

export function isUntrackedStatusCode(code: string): boolean {
  const compact = code.replaceAll(" ", "");
  return compact === "?" || compact === "??";
}

export function classifyFileChange(status: StatusLike | null | undefined): FileChangeKind {
  if (!status) return "clean";
  if (status.is_conflicted) return "conflict";
  if (isUntrackedStatusCode(status.status_code)) return "untracked";
  if (status.is_staged) return "staged";
  return "unstaged";
}

export function statusMatchesScope(status: StatusLike | undefined, scope: FileStatusScope): boolean {
  if (scope === "all") return true;
  if (!status) return false;
  const kind = classifyFileChange(status);
  switch (scope) {
    case "staged":
      return kind === "staged";
    case "unstaged":
      return kind === "unstaged";
    case "untracked":
      return kind === "untracked";
    case "conflict":
      return kind === "conflict";
    case "modified":
      return kind === "unstaged";
    default:
      return true;
  }
}

export function summarizeStatuses(statuses: readonly StatusLike[]): StatusDashboard {
  const next: StatusDashboard = {
    staged: 0,
    unstaged: 0,
    untracked: 0,
    conflicted: 0,
    additions: 0,
    deletions: 0,
    dirty: statuses.length,
  };
  for (const status of statuses) {
    const kind = classifyFileChange(status);
    if (kind === "conflict") next.conflicted += 1;
    else if (kind === "staged") next.staged += 1;
    else if (kind === "untracked") next.untracked += 1;
    else next.unstaged += 1;
    next.additions += status.additions || 0;
    next.deletions += status.deletions || 0;
  }
  return next;
}

/** Fingerprint including codes and churn — dashboard "last synced" key. */
export function statusLiveKey(statuses: readonly StatusLike[]): string {
  if (statuses.length === 0) return "";
  return statuses
    .map(
      (status) =>
        `${status.path}\t${status.status_code}\t${Number(status.is_staged)}\t${Number(status.is_conflicted)}\t${status.additions}\t${status.deletions}`,
    )
    .sort()
    .join("\n");
}

/** Sorted unique status paths — listing reload key, not churn numbers. */
export function statusPathKey(statuses: readonly Pick<StatusLike, "path">[]): string {
  if (statuses.length === 0) return "";
  const paths = statuses.map((status) => status.path);
  paths.sort();
  return paths.join("\n");
}

/** Directories that contain at least one dirty descendant. */
export function dirtyAncestorSet(statusPaths: readonly string[]): Set<string> {
  const dirs = new Set<string>();
  for (const path of statusPaths) {
    for (const ancestor of ancestorsOf(path)) dirs.add(ancestor);
  }
  return dirs;
}

export function mergeListedAndStatusPaths(
  listed: readonly string[],
  statusPaths: readonly string[],
): string[] {
  if (statusPaths.length === 0) return [...listed];
  const seen = new Set(listed);
  const extra: string[] = [];
  for (const path of statusPaths) {
    if (seen.has(path)) continue;
    seen.add(path);
    extra.push(path);
  }
  if (extra.length === 0) return [...listed];
  return listed.concat(extra);
}
