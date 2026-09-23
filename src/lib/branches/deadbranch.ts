import { invoke } from "@tauri-apps/api/core";
import type {
  DeadbranchBackupInfo,
  DeadbranchCleanResult,
  DeadbranchConfig,
  DeadbranchRestoreResult,
  DeadbranchScanResult,
  DeadbranchSeverity,
  StaleBranchInfo,
} from "./types";

interface Guarded<T> {
  policy: unknown;
  output: T;
}

/**
 * Scans a repository for stale branches, ancestry merges, and squash/rebase merges via `git merge-tree`.
 */
export async function deadbranchScan(
  repoPath: string,
  config?: DeadbranchConfig,
): Promise<DeadbranchScanResult> {
  return invoke<DeadbranchScanResult>("cmd_deadbranch_scan", {
    repoPath,
    config: config ?? null,
  });
}

/**
 * Safely cleans selected branches with pre-deletion backups and guard verification.
 */
export async function deadbranchClean(
  repoPath: string,
  branches: string[],
  force = false,
  createBackup = true,
): Promise<DeadbranchCleanResult> {
  const guarded = await invoke<Guarded<DeadbranchCleanResult>>("cmd_deadbranch_clean", {
    repoPath,
    branches,
    force,
    createBackup,
  });
  return guarded.output;
}

/**
 * Lists available branch backup files for the repository.
 */
export async function deadbranchListBackups(
  repoPath: string,
): Promise<DeadbranchBackupInfo[]> {
  return invoke<DeadbranchBackupInfo[]>("cmd_deadbranch_list_backups", {
    repoPath,
  });
}

/**
 * Restores branches from a saved backup file.
 */
export async function deadbranchRestore(
  repoPath: string,
  backupPath: string,
  branches?: string[],
): Promise<DeadbranchRestoreResult> {
  const guarded = await invoke<Guarded<DeadbranchRestoreResult>>("cmd_deadbranch_restore", {
    repoPath,
    backupPath,
    branches: branches ?? null,
  });
  return guarded.output;
}

/**
 * Formats a human-readable age string from days.
 */
export function formatBranchAge(days: number): string {
  if (days <= 0) return "today";
  if (days === 1) return "1 day ago";
  if (days < 30) return `${days} days ago`;
  const months = Math.floor(days / 30);
  if (months === 1) return "1 month ago";
  if (months < 12) return `${months} months ago`;
  const years = Math.floor(days / 365);
  return years === 1 ? "1 year ago" : `${years} years ago`;
}

/**
 * Returns color classes and labels for severity badges.
 */
export function severityBadge(severity: DeadbranchSeverity): {
  label: string;
  bgClass: string;
  textClass: string;
  borderClass: string;
} {
  switch (severity) {
    case "fresh":
      return {
        label: "Fresh",
        bgClass: "bg-emerald-500/10",
        textClass: "text-emerald-400",
        borderClass: "border-emerald-500/20",
      };
    case "moderate":
      return {
        label: "Moderate",
        bgClass: "bg-amber-500/10",
        textClass: "text-amber-400",
        borderClass: "border-amber-500/20",
      };
    case "stale":
      return {
        label: "Stale",
        bgClass: "bg-rose-500/10",
        textClass: "text-rose-400",
        borderClass: "border-rose-500/20",
      };
  }
}

/**
 * Formats an epoch timestamp into a relative or locale time.
 */
export function formatBackupTimestamp(timestampSec: number): string {
  if (!timestampSec) return "Unknown";
  const date = new Date(timestampSec * 1000);
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * Filters branches based on user search term, age, and merge mode.
 */
export function filterStaleBranches(
  branches: StaleBranchInfo[],
  query: string,
  minDays: number,
  mergedOnly: boolean,
  localOnly: boolean,
): StaleBranchInfo[] {
  const q = query.trim().toLowerCase();
  return branches.filter((b) => {
    if (q && !b.name.toLowerCase().includes(q) && !b.last_author.toLowerCase().includes(q)) {
      return false;
    }
    if (b.age_days < minDays && !b.is_merged) {
      return false;
    }
    if (mergedOnly && !b.is_merged) {
      return false;
    }
    if (localOnly && b.is_remote) {
      return false;
    }
    return true;
  });
}
