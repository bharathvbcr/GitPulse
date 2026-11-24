/**
 * Starting a Mavi thread in a worktree of its own.
 *
 * One live local thread per worktree is the host's rule (two agents in one
 * checkout make "what this turn changed" a mix of both). Rather than only
 * refusing, the new-thread form offers a fresh worktree, named the way the
 * dock's isolated-agent preset names one: a sibling `<repo>.wt-<slug>` on a
 * new branch, here `mavi/<slug>`.
 */
import { sanitizeWorktreeSlug } from "../terminal/tabs";

export interface WorktreePlan {
  readonly path: string;
  readonly branch: string;
  readonly slug: string;
}

/** A slug from the first words of the message: lowercase, hyphenated, short. */
export function slugFromMessage(opening: string, now: number = Date.now()): string {
  const words = opening
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, " ")
    .split(/\s+/)
    .filter((w) => w.length > 1)
    .slice(0, 4)
    .join("-");
  const stamp = now.toString(36).slice(-4);
  return sanitizeWorktreeSlug(words ? `${words}-${stamp}` : `thread-${stamp}`) ?? `thread-${stamp}`;
}

/** The sibling worktree path and branch for `repo`, or null when it has no parent directory. */
export function planWorktree(repo: string, slug: string): WorktreePlan | null {
  const clean = sanitizeWorktreeSlug(slug);
  if (!clean) return null;
  const trimmed = repo.replace(/[\\/]+$/, "");
  const sep = trimmed.includes("\\") ? "\\" : "/";
  const at = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  if (at <= 0) return null;
  return {
    path: `${trimmed.slice(0, at)}${sep}${trimmed.slice(at + 1)}.wt-${clean}`,
    branch: `mavi/${clean}`,
    slug: clean,
  };
}
