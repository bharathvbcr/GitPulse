/**
 * Recognising a worktree an agent created for itself.
 *
 * Coding agents isolate a task in its own git worktree under
 * `<repo>/.<agent>/worktrees/<slug>` — Claude Code uses `.claude/worktrees/`,
 * and the same layout is what Cursor, Codex and others have converged on.
 * Those directories look like any other worktree to git, but they mean
 * something different to a person reading the Work view: this is not a
 * checkout you made and forgot, it is a session that is either running right
 * now or was abandoned mid-change.
 *
 * The distinction earns its place because the two have opposite remedies. A
 * stale hand-made worktree wants pruning; a stale agent worktree wants its
 * branch merged or its session resumed. Labelling them identically is what
 * makes the list unreadable once more than two exist.
 *
 * Detection is deliberately conservative: it matches the directory layout
 * agents actually create (`/.<name>/worktrees/`), and does NOT guess from the
 * branch name. A human can name a branch `claude/anything`, and calling their
 * worktree an agent session because of it would put a wrong label on real
 * work. Git's own metadata store (`.git/worktrees/`) is the same shape and
 * is excluded: that path is not a checkout.
 */

/** The path segment Claude Code nests its worktrees under. */
export const AGENT_WORKTREE_SEGMENT = ".claude/worktrees/";

/** Git's linked-worktree metadata directory — never a working tree. */
const GIT_INTERNAL_SEGMENT = "/.git/worktrees/";

/**
 * `/.<agent>/worktrees/` anywhere in a normalised path, except git's own
 * store. The agent name is the hidden directory, so a new tool that follows
 * the same layout is recognised without a code change.
 */
const AGENT_LAYOUT = /\/\.(?!git(?:\/|$))([^/]+)\/worktrees(?:\/|$)/;

function normalisedPath(path: string): string {
  // Leading slash so a bare `.claude/worktrees/…` still matches the layout,
  // and so `C:\…` Windows paths become comparable to POSIX ones.
  return `/${path.replace(/\\/g, "/")}`.replace(/\/{2,}/g, "/");
}

/**
 * True when this path is a worktree an agent created.
 *
 * Windows separators are normalised first: the same repository opened on
 * Windows reports `\\.claude\\worktrees\\`, and matching only the POSIX form
 * would silently label every agent worktree there as hand-made.
 */
export function isAgentWorktree(path: string): boolean {
  if (!path) return false;
  const normalised = normalisedPath(path);
  // Git's metadata dir is `.git` on every platform we ship; comparing
  // case-insensitively covers a checkout on a case-insensitive volume
  // that reports `.GIT`.
  if (normalised.toLowerCase().includes(GIT_INTERNAL_SEGMENT)) return false;
  return AGENT_LAYOUT.test(normalised);
}

/**
 * The agent that created this worktree (`claude`, `cursor`, `codex`, …).
 *
 * Empty when the path is not an agent worktree. The hidden-directory name is
 * returned as-is: inventing a prettier label would claim knowledge we do not
 * have about a tool we have only seen a folder of.
 */
export function agentKind(path: string): string {
  if (!isAgentWorktree(path)) return "";
  const match = normalisedPath(path).match(AGENT_LAYOUT);
  return match?.[1] ?? "";
}

/**
 * Distinct agent kinds on a set of worktree paths, in first-seen order.
 *
 * A task row can hold worktrees from more than one agent; showing the same
 * chip twice is noise, and dropping one of them hides a session.
 */
export function agentKindsOn(paths: readonly string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const path of paths) {
    const kind = agentKind(path);
    if (!kind || seen.has(kind)) continue;
    seen.add(kind);
    out.push(kind);
  }
  return out;
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
  const normalised = normalisedPath(path);
  const marker = "/worktrees/";
  const at = normalised.indexOf(marker);
  if (at < 0) return "";
  return normalised
    .slice(at + marker.length)
    .split("/")
    .filter(Boolean)[0] ?? "";
}
