/**
 * The last pull-request list each repository's `cmd_github_context` returned,
 * whoever asked for it (Work's load, the Remote panel, the Policy panel).
 *
 * Row 2's glance shows the current branch's pull request from here, so it
 * costs no `gh` call of its own: a repository no panel has read shows no
 * chip. A read that failed, or whose pull-request listing failed, does not
 * replace the last good one; the chip names the time of the read it shows.
 */
import { writable, type Readable } from "svelte/store";
import type { GitHubContext, PullRequestInfo } from "./types";

export interface PullRequestRead {
  readonly prs: readonly PullRequestInfo[];
  /** When the read returned (epoch ms). */
  readonly at: number;
}

/** Repositories remembered at once; the oldest read is dropped past this. */
export const MAX_REMEMBERED_REPOSITORIES = 64;

const store = writable<ReadonlyMap<string, PullRequestRead>>(new Map());

export const pullRequestReads: Readable<ReadonlyMap<string, PullRequestRead>> = { subscribe: store.subscribe };

export function notePullRequests(repoPath: string, context: GitHubContext, at: number = Date.now()): void {
  if (!repoPath || !context.available || context.error || context.prs_error) return;
  store.update((current) => {
    const next = new Map(current);
    // Re-inserted so the map's order is oldest read first.
    next.delete(repoPath);
    next.set(repoPath, { prs: context.pull_requests, at });
    while (next.size > MAX_REMEMBERED_REPOSITORIES) {
      const oldest = next.keys().next().value;
      if (oldest === undefined) break;
      next.delete(oldest);
    }
    return next;
  });
}
