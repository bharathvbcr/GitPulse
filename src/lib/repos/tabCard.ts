/**
 * What a repository tab's hover card may say (HEADER_SPEC E2).
 *
 * A tab only carries what every open repository carries: path, branch, the
 * uncommitted count and its terminals. Sync with the upstream and the last
 * fetch are read for the repository on screen only; for any other tab the card
 * says "Not read yet" rather than leaving the row out or guessing, so a missing
 * answer never reads as a quiet one.
 */

import { formatRelativeTime, plural } from "../format";
import type { RepoAgentPill } from "../agents/repoAttention";
import type { OpenRepoTab } from "./storeTypes";

export interface TabCardRow {
  label: string;
  value: string;
  /** True when the value is "Not read yet": the card does not know. */
  unknown?: boolean;
}

export interface TabCardInput {
  tab: Pick<OpenRepoTab, "path" | "currentBranch" | "changedCount" | "conflictedCount" | "countsKnown" | "isDirty">;
  /** Ahead and behind for the current branch, only when this repository's branches are loaded. */
  sync: { ahead: number; behind: number } | null;
  /**
   * Last fetch time in unix seconds, or null when none is recorded. `undefined`
   * means this repository's metadata is not loaded (not the active tab).
   */
  fetchedAt: number | null | undefined;
  terminals: number;
  agents: RepoAgentPill;
  nowSec?: number;
}

const NOT_READ = "Not read yet";

function syncText(sync: TabCardInput["sync"]): string {
  if (!sync) return NOT_READ;
  if (sync.ahead === 0 && sync.behind === 0) return "In sync";
  const parts: string[] = [];
  if (sync.ahead > 0) parts.push(`${sync.ahead} ahead`);
  if (sync.behind > 0) parts.push(`${sync.behind} behind`);
  return parts.join(", ");
}

export function tabCardRows(input: TabCardInput): TabCardRow[] {
  const { tab, sync, fetchedAt, terminals, agents } = input;
  const rows: TabCardRow[] = [{ label: "Path", value: tab.path }];

  rows.push({
    label: "Branch",
    value: tab.currentBranch ? `${tab.currentBranch} · ${syncText(sync)}` : "Detached HEAD or not read",
    unknown: !tab.currentBranch || sync === null,
  });

  if (tab.countsKnown === false) {
    rows.push({ label: "Uncommitted", value: NOT_READ, unknown: true });
  } else {
    const conflicts = tab.conflictedCount > 0 ? `, ${plural(tab.conflictedCount, "conflict")}` : "";
    rows.push({
      label: "Uncommitted",
      value: tab.changedCount > 0 ? `${plural(tab.changedCount, "file")}${conflicts}` : `None${conflicts}`,
    });
  }

  let agentsValue: string;
  switch (agents.state) {
    case "attention":
      agentsValue = agents.live > 0 ? `${agents.phrase} · ${agents.live} running` : agents.phrase;
      break;
    case "active":
      agentsValue = `${agents.live} running`;
      break;
    case "unknown":
      agentsValue = NOT_READ;
      break;
    default:
      agentsValue = "None";
  }
  rows.push({ label: "Agents", value: agentsValue, unknown: agents.state === "unknown" });
  rows.push({ label: "Terminals", value: terminals > 0 ? plural(terminals, "session") : "None" });

  if (fetchedAt === undefined) {
    rows.push({ label: "Last fetch", value: NOT_READ, unknown: true });
  } else {
    rows.push({
      label: "Last fetch",
      value: fetchedAt ? formatRelativeTime(fetchedAt, input.nowSec) : "No fetch recorded",
    });
  }
  return rows;
}
