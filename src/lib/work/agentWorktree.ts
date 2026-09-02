/**
 * Recognising a worktree an agent created for itself.
 *
 * Claude Code isolates each task in its own git worktree under
 * `<repo>/.claude/worktrees/<slug>`, on a branch named `claude/<slug>`. Those
 * directories look like any other worktree to git, but they mean something
 * different to a person reading the Work view: this is not a checkout you made
 * and forgot, it is a session that is either running right now or was
 * abandoned mid-change.
 *
 * The distinction earns its place because the two have opposite remedies. A
 * stale hand-made worktree wants pruning; a stale agent worktree wants its
 * branch merged or its session resumed. Labelling them identically is what
 * makes the list unreadable once more than two exist.
 *
 * Detection is deliberately conservative: it matches the directory layout the
 * tool actually creates, and does NOT guess from the branch name alone. A
 * human can name a branch `claude/anything`, and calling their worktree an
 * agent session because of it would put a wrong label on real work.
 */

/** The path segment Claude Code nests its worktrees under. */
export const AGENT_WORKTREE_SEGMENT = ".claude/worktrees/";

/**
 * True when this path is a worktree Claude Code created.
 *
 * Windows separators are normalised first: the same repository opened on
 * Windows reports `\\.claude\\worktrees\\`, and matching only the POSIX form
 * would silently label every agent worktree there as hand-made.
 */
export function isAgentWorktree(path: string): boolean {
  if (!path) return false;
  return path.replace(/\\/g, "/").includes(AGENT_WORKTREE_SEGMENT);
}

/**
 * The session slug, when the path is an agent worktree.
 *
 * Claude Code appends a short hash to keep concurrent sessions on the same
 * task distinct (`agentic-git-repo-system-8540d4`). The whole segment is
 * returned rather than a prettified prefix — it is the only thing that
 * distinguishes two sessions working the same feature, so trimming it would
 * merge them in the reader's eye.
 */
export function agentSessionSlug(path: string): string {
  if (!isAgentWorktree(path)) return "";
  const normalised = path.replace(/\\/g, "/");
  const after = normalised.slice(
    normalised.indexOf(AGENT_WORKTREE_SEGMENT) + AGENT_WORKTREE_SEGMENT.length,
  );
  return after.split("/").filter(Boolean)[0] ?? "";
}
