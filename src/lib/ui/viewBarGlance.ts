/**
 * Row 2's glance chips (HEADER_SPEC §3 row 2b, decision D8): four facts about the
 * repository on screen, each drawn from a read something else already made.
 * Nothing here asks the host for anything.
 *
 * - Last fetch: the metadata facet's `cmd_last_fetch_at` (repoStore).
 * - Stash: the metadata facet's stash list (repoStore).
 * - The branch's pull request and its CI: the last `cmd_github_context` a
 *   panel read for this repository (github/pullRequestReads.ts).
 * - Files changed in another worktree too: the collision facet of the
 *   insights snapshot the Agents pill's sweep already took. That sweep is not
 *   polled, so the chip names the age of the read, as the pull request does.
 *
 * Each returns null when there is nothing to say, and never a zero it did not
 * measure: a stash list or collision scan that did not finish shows its count
 * as a floor ("3+"), and one that failed shows nothing here, because the
 * place that owns it says so (the sidebar's stash section; the Agents pill,
 * which turns "unknown" when the collision scan was partial).
 */
import type { CollisionRisk } from "../insights/types";
import type { PullRequestInfo } from "../github/types";
import { formatAge } from "../storage/format";

/** A fetch older than this tints the chip: the remote view is a day behind. */
export const STALE_FETCH_MS = 24 * 60 * 60 * 1000;

export type GlanceTone = "neutral" | "ok" | "warn" | "danger" | "info";

export interface Glance {
  /** What the chip shows beside its icon: short, tabular. */
  text: string;
  /** The chip's accessible name and tooltip: the whole sentence. */
  label: string;
  tone: GlanceTone;
}

function count(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/**
 * "now", "12m", "5h", "3d": the age in the fewest characters that stay true.
 * Not `formatAge` in metrics/freshness.ts: that one counts seconds, which the
 * glance's once-a-minute clock cannot keep true, and it rounds up, so a fetch
 * 59 minutes old would read "1h" a minute early. This one floors, and keeps
 * hours to two days, where "26h" says more than "1d".
 */
export function compactAge(ms: number): string {
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

/**
 * The last fetch's age. `undefined` is "not read yet" and `null` "no fetch
 * recorded" (a repository with no remote reads the same): neither is shown.
 */
export function fetchGlance(fetchedAt: number | null | undefined, now: number): Glance | null {
  if (typeof fetchedAt !== "number" || !Number.isFinite(fetchedAt) || fetchedAt <= 0) return null;
  const age = Math.max(0, now - fetchedAt);
  // The tone says "over a day"; the words say how far over.
  return {
    text: compactAge(age),
    label: `Last fetched ${formatAge(fetchedAt, now)}. Fetch now`,
    tone: age >= STALE_FETCH_MS ? "warn" : "neutral",
  };
}

export function stashGlance(stash: { entries: number; truncated: boolean; failed: boolean }): Glance | null {
  if (stash.failed || stash.entries <= 0) return null;
  const shown = stash.truncated ? `${stash.entries}+` : String(stash.entries);
  return {
    text: shown,
    label: stash.truncated
      ? `At least ${count(stash.entries, "stash entry", "stash entries")}; the list was capped`
      : count(stash.entries, "stash entry", "stash entries"),
    tone: "neutral",
  };
}

/** The open pull request whose head is this branch of this repository. */
export function branchPullRequest(
  prs: readonly PullRequestInfo[],
  branch: string | null,
): PullRequestInfo | null {
  if (!branch) return null;
  return prs.find((pr) => pr.head_ref === branch && !pr.is_cross_repository && pr.state.toUpperCase() !== "CLOSED" && pr.state.toUpperCase() !== "MERGED") ?? null;
}

const CI: Record<string, { tone: GlanceTone; words: string }> = {
  success: { tone: "ok", words: "checks passed" },
  failure: { tone: "danger", words: "checks failing" },
  pending: { tone: "warn", words: "checks running" },
  none: { tone: "neutral", words: "no checks" },
};

export function pullRequestGlance(
  pr: PullRequestInfo | null,
  readAt: number,
  now: number,
): Glance | null {
  if (!pr) return null;
  const ci = CI[pr.ci_status.toLowerCase()] ?? { tone: "neutral" as const, words: "checks unknown" };
  const draft = pr.is_draft ? "Draft pull request" : "Pull request";
  return {
    text: `#${pr.number}`,
    label: `${draft} #${pr.number}, ${ci.words}, as of ${formatAge(readAt, now)}: ${pr.title}`,
    tone: ci.tone,
  };
}

/** Files with uncommitted changes in two or more of this repository's worktrees. */
export function overlapGlance(
  facet: CollisionRisk | null | undefined,
  readAt: number | null,
  now: number,
): Glance | null {
  if (!facet || !facet.ok || facet.overlapping_files <= 0) return null;
  const floor = facet.truncated || facet.unscanned_worktrees > 0 || facet.failed_worktrees > 0;
  const n = facet.overlapping_files;
  const files = floor ? `At least ${count(n, "file", "files")}` : count(n, "file", "files");
  const others = facet.worktrees_involved > 1 ? ` across ${facet.worktrees_involved} worktrees` : "";
  return {
    text: floor ? `${n}+` : String(n),
    label: `${files} changed in more than one worktree${others}${readAt === null ? "" : `, as of ${formatAge(readAt, now)}`}. Open Agents`,
    tone: "warn",
  };
}
