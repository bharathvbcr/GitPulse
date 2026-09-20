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
 *
 * # The kind and the slug are one decision, not two
 *
 * Everything below derives from a single left-to-right scan that returns the
 * position it matched at. An earlier version found the kind by locating
 * `/.<name>/worktrees`, then found the slug by independently searching for
 * the first `/worktrees/` in the whole path — two searches for one fact, and
 * they disagreed whenever any ancestor directory happened to be named
 * `worktrees`. `/Users/me/worktrees/app/.claude/worktrees/session-abc` read
 * its slug as `app`, so every session under such a parent collapsed to one
 * label: the exact merging-in-the-reader's-eye the slug exists to prevent.
 * One scan cannot disagree with itself.
 *
 * Both implementations of this rule — this one and `agent_layout` in
 * `src-tauri/src/engine/worktree.rs` — are held to the shared corpus in
 * `agentWorktree.cases.json`, so they cannot drift apart silently.
 */

/** The path segment Claude Code nests its worktrees under. */
export const AGENT_WORKTREE_SEGMENT = ".claude/worktrees/";

/** The directory name an agent nests its sessions under. */
const WORKTREES_SEGMENT = "worktrees";

/** What a matched agent layout yields: the tool, and the session under it. */
export interface AgentLayout {
  /** The hidden directory's name, without the leading dot. Never empty. */
  kind: string;
  /**
   * The session directory under `worktrees`, or empty when the path stops at
   * the container itself. Empty means "this path names no session" — never a
   * session whose name could not be read.
   */
  slug: string;
}

/**
 * Path segments, separator-normalised.
 *
 * Windows separators go first: the same repository opened on Windows reports
 * `\.claude\worktrees\`, and matching only the POSIX form would silently
 * label every agent worktree there as hand-made. Splitting on runs of
 * separators and dropping empties collapses `//` and any trailing slash in
 * the same pass, so no quadratic de-duplication loop is needed.
 */
function normalisedSegments(path: string): string[] {
  return path.split(/[\\/]+/).filter(Boolean);
}

/**
 * The agent's name when `segment` is the hidden directory one nests worktrees
 * under, else empty.
 *
 * Rejected, in order:
 *
 * * anything not starting with a dot — an ordinary directory;
 * * a bare `.`, and anything starting with `..` — those are traversal, not
 *   directory names, and no agent is called `.foo`. Without this,
 *   `/repo/../worktrees/x` reported an agent of kind `.`;
 * * `git` in any case. On the case-insensitive volumes macOS and Windows ship
 *   by default, `.GIT/worktrees` IS git's own metadata store; an earlier
 *   case-sensitive comparison let `/repo/.GIT/worktrees` through as an agent
 *   of kind `GIT`.
 */
function agentDirectoryName(segment: string): string {
  if (!segment.startsWith(".")) return "";
  const name = segment.slice(1);
  if (!name || name.startsWith(".")) return "";
  if (name.toLowerCase() === "git") return "";
  return name;
}

/**
 * The agent layout this path sits in, or null when it is not an agent
 * worktree.
 *
 * The single scan every other function here is built on. Left to right, so
 * the outermost layout wins and the slug is always read from the match that
 * named the kind.
 */
export function agentLayout(path: string): AgentLayout | null {
  if (!path) return null;
  const segments = normalisedSegments(path);
  for (let i = 0; i + 1 < segments.length; i += 1) {
    const kind = agentDirectoryName(segments[i]);
    if (!kind || segments[i + 1] !== WORKTREES_SEGMENT) continue;
    return { kind, slug: segments[i + 2] ?? "" };
  }
  return null;
}

/**
 * True when this path is a worktree an agent created.
 *
 * A path that stops at the container (`.claude/worktrees`) still counts: the
 * layout is what is recognised here, and a caller that needs a session name
 * asks for the slug and gets an honest empty string.
 */
export function isAgentWorktree(path: string): boolean {
  return agentLayout(path) !== null;
}

/**
 * The agent that created this worktree (`claude`, `cursor`, `codex`, `grok`, `agy`, …).
 *
 * Empty when the path is not an agent worktree. The hidden-directory name is
 * returned as-is: inventing a prettier label would claim knowledge we do not
 * have about a tool we have only seen a folder of.
 */
export function agentKind(path: string): string {
  return agentLayout(path)?.kind ?? "";
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
 *
 * Read from the same match that named the kind, so an ancestor directory
 * called `worktrees` cannot redirect it to an unrelated segment.
 */
export function agentSessionSlug(path: string): string {
  return agentLayout(path)?.slug ?? "";
}
