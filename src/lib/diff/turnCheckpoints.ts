/**
 * IPC helpers and pure formatters for `refs/gitpulse/turns/` checkpoint browsing.
 */
import { invoke } from "../ipc/invoke";

export interface TurnCheckpointOmissions {
  untracked_found: number;
  untracked_admitted: number;
  env_files: number;
  secret_flagged: number;
  unreadable_or_special: number;
  too_large: number;
  nested_repos: number;
  tool_state: number;
  wholesale: string | null;
  wholesale_detail: string | null;
  cap_files: number;
  cap_bytes: number;
  found_bytes: number;
}

export interface TurnCheckpointSummary {
  id: string;
  ref_name: string;
  commit: string;
  tree: string;
  parent: string | null;
  created_at: number;
  label: string;
  event: string | null;
  session_id: string | null;
  branch: string | null;
  files: number;
  additions: number;
  deletions: number;
  stats_complete: boolean;
  omissions: TurnCheckpointOmissions;
}

export interface TurnCheckpointList {
  key: string;
  entries: TurnCheckpointSummary[];
  total: number;
  truncated: boolean;
}

export interface TurnCheckpointDiff {
  id: string;
  against: string | null;
  base_commit: string;
  commit: string;
  patch: string;
  truncated: boolean;
  redacted: boolean;
}

export async function listTurnCheckpoints(
  repoPath: string,
  limit = 50,
): Promise<TurnCheckpointList> {
  return invoke<TurnCheckpointList>("cmd_list_turn_checkpoints", { repoPath, limit });
}

export async function diffTurnCheckpoint(
  repoPath: string,
  id: string,
  against: string | null = null,
): Promise<TurnCheckpointDiff> {
  return invoke<TurnCheckpointDiff>("cmd_diff_turn_checkpoint", { repoPath, id, against });
}

export function totalOmittedFiles(omissions: TurnCheckpointOmissions): number {
  const skippedUntracked = Math.max(0, omissions.untracked_found - omissions.untracked_admitted);
  return (
    skippedUntracked +
    omissions.env_files +
    omissions.secret_flagged +
    omissions.unreadable_or_special +
    omissions.too_large +
    omissions.nested_repos +
    omissions.tool_state
  );
}

export function formatOmissionSummary(omissions: TurnCheckpointOmissions): string[] {
  const notes: string[] = [];
  if (omissions.wholesale) {
    const detail = omissions.wholesale_detail ? `: ${omissions.wholesale_detail}` : "";
    notes.push(`Skipped untracked snapshot (${omissions.wholesale}${detail})`);
  } else {
    const skippedUntracked = Math.max(0, omissions.untracked_found - omissions.untracked_admitted);
    if (skippedUntracked > 0) {
      notes.push(
        `${skippedUntracked} untracked file${skippedUntracked === 1 ? "" : "s"} skipped by cap`,
      );
    }
  }
  if (omissions.env_files > 0) {
    notes.push(`${omissions.env_files} .env file${omissions.env_files === 1 ? "" : "s"} excluded`);
  }
  if (omissions.secret_flagged > 0) {
    notes.push(
      `${omissions.secret_flagged} secret-flagged file${omissions.secret_flagged === 1 ? "" : "s"} excluded`,
    );
  }
  if (omissions.unreadable_or_special > 0) {
    notes.push(
      `${omissions.unreadable_or_special} unreadable or special file${omissions.unreadable_or_special === 1 ? "" : "s"} skipped`,
    );
  }
  if (omissions.too_large > 0) {
    notes.push(`${omissions.too_large} oversized file${omissions.too_large === 1 ? "" : "s"} skipped`);
  }
  if (omissions.nested_repos > 0) {
    notes.push(
      `${omissions.nested_repos} nested ${omissions.nested_repos === 1 ? "repository" : "repositories"} skipped`,
    );
  }
  if (omissions.tool_state > 0) {
    notes.push(`${omissions.tool_state} tool-state file${omissions.tool_state === 1 ? "" : "s"} skipped`);
  }
  return notes;
}

export function filterTurnCheckpoints(
  entries: readonly TurnCheckpointSummary[],
  query: string,
): TurnCheckpointSummary[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [...entries];
  return entries.filter((entry) =>
    [entry.label, entry.branch ?? "", entry.session_id ?? "", entry.id, entry.event ?? "", entry.commit]
      .some((field) => field.toLowerCase().includes(needle)),
  );
}

export function formatRelativeTurnTime(
  epochSeconds: number,
  nowSeconds = Math.floor(Date.now() / 1000),
): string {
  if (!Number.isFinite(epochSeconds) || epochSeconds <= 0) return "";
  const delta = Math.max(0, Math.floor(nowSeconds - epochSeconds));
  if (delta < 60) return "just now";
  const mins = Math.floor(delta / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}
