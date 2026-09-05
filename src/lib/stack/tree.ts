/**
 * Shaping the stacked-branch payload into what the Stack page renders and acts on.
 *
 * The backend answers with a flat node list carrying parent/child names. Two
 * things are missing before a reader can use it, and both are pure functions
 * over that list, so they live here rather than inside the component:
 *
 * 1. **The tree.** A stack IS its shape. Rendering "based on X" on every row
 *    of a flat list makes the reader rebuild the chain in their head, which is
 *    the one thing the page exists to do for them.
 * 2. **The cascade.** Rebasing the root of a stack moves every branch above it
 *    off the tip it was cut from, so a single restack silently strands the
 *    rest of the stack — and, because the hierarchy is tip-anchored, strands
 *    them *invisibly*: they stop being children and reappear as roots. The
 *    plan is computed from the snapshot the reader is looking at, before
 *    anything moves, which is the only moment those fork points still exist.
 *
 * # What the hierarchy can and cannot see
 *
 * `parent_branch_name` is set only while a branch sits exactly on another
 * branch's current tip. That is a real property of the repository and not a
 * gap in the backend: git stores no "this was cut from that" edge, and once
 * the parent moves, nothing on disk distinguishes a drifted child from an
 * unrelated branch that happens to share history. So a drifted stack does not
 * render as stale — it renders as separate roots, and [`rootlessBranches`]
 * exists so the page can say that out loud instead of quietly showing fewer
 * branches than the repository has.
 */

import type { BranchInfo } from "../branches/types";
import type { StackedBranchNode } from "./types";

/** One rendered line of the stack tree. */
export interface StackTreeRow {
  node: StackedBranchNode;
  /** 0 for a root; each step down the chain adds one. */
  depth: number;
  /** True for the last child of its parent, which draws the elbow connector. */
  isLast: boolean;
  /** True when anything is stacked on this branch. */
  hasChildren: boolean;
}

/** One step of a cascading restack, in the order it must be executed. */
export interface RestackStep {
  branch: string;
  /** The ref the branch is replayed onto; resolved at execution time. */
  onto: string;
  /**
   * The parent tip recorded when the stack was read, or null for the branch
   * the cascade starts at.
   *
   * Null means "compute it": the first branch has not had its parent rewritten
   * by this cascade, so `merge-base` still describes it. Every branch above it
   * has, and for those the recorded tip is the only surviving record of where
   * they were cut — the backend refuses it if the stack has moved since.
   */
  forkPoint: string | null;
}

/** Index of nodes by branch name, for the walks below. */
function indexNodes(
  nodes: readonly StackedBranchNode[],
): Map<string, StackedBranchNode> {
  return new Map(nodes.map((node) => [node.branch_name, node]));
}

/**
 * Children derived from the parent pointers, sorted by name.
 *
 * The payload also carries `child_branch_names`, and the backend builds it
 * from these same pointers — but two encodings of one relationship can
 * disagree, and a branch is either put into a history rewrite or left out of
 * one on the strength of this answer. Deriving from the pointer alone means
 * there is nothing to disagree with.
 */
function childrenByParent(
  nodes: readonly StackedBranchNode[],
): Map<string, StackedBranchNode[]> {
  const byName = indexNodes(nodes);
  const children = new Map<string, StackedBranchNode[]>();
  for (const node of nodes) {
    const parent = node.parent_branch_name;
    if (!parent || !byName.has(parent) || parent === node.branch_name) continue;
    const bucket = children.get(parent);
    if (bucket) bucket.push(node);
    else children.set(parent, [node]);
  }
  for (const bucket of children.values()) {
    bucket.sort((a, b) => a.branch_name.localeCompare(b.branch_name));
  }
  return children;
}

/**
 * Depth-first rows, roots first, children under their parent.
 *
 * Sorted by name within each level so the same repository always draws the
 * same tree — the payload's own order is the backend's sort, and a stack that
 * reshuffles between refreshes is unreadable.
 *
 * Cycle-safe by construction: a branch already emitted is never expanded a
 * second time, so a parent/child pair pointing at each other (which the
 * backend's own breadcrumb walk guards against separately) terminates here
 * too, and every node is emitted exactly once whatever the edges say.
 */
export function stackTreeRows(
  nodes: readonly StackedBranchNode[],
): StackTreeRow[] {
  const byName = indexNodes(nodes);
  const children = childrenByParent(nodes);
  const emitted = new Set<string>();
  const rows: StackTreeRow[] = [];

  // A parent named by a child but absent from the payload cannot anchor a
  // subtree, so its children are roots here rather than being dropped.
  const isRoot = (node: StackedBranchNode) =>
    !node.parent_branch_name ||
    !byName.has(node.parent_branch_name) ||
    node.parent_branch_name === node.branch_name;

  const walk = (node: StackedBranchNode, depth: number, isLast: boolean) => {
    if (emitted.has(node.branch_name)) return;
    emitted.add(node.branch_name);
    const kids = (children.get(node.branch_name) ?? []).filter(
      (child) => !emitted.has(child.branch_name),
    );
    rows.push({ node, depth, isLast, hasChildren: kids.length > 0 });
    kids.forEach((child, i) => walk(child, depth + 1, i === kids.length - 1));
  };

  const roots = nodes
    .filter(isRoot)
    .slice()
    .sort((a, b) => a.branch_name.localeCompare(b.branch_name));
  roots.forEach((root, i) => walk(root, 0, i === roots.length - 1));

  // Anything left is inside a cycle: emit it as its own root rather than
  // losing it, so the tree still accounts for every branch in the payload.
  for (const node of nodes) {
    if (!emitted.has(node.branch_name)) walk(node, 0, true);
  }
  return rows;
}

/** Every branch stacked above `branch`, nearest first. */
export function descendantsOf(
  nodes: readonly StackedBranchNode[],
  branch: string,
): StackedBranchNode[] {
  const children = childrenByParent(nodes);
  const seen = new Set<string>([branch]);
  const out: StackedBranchNode[] = [];
  let frontier = [branch];
  while (frontier.length > 0) {
    const next: string[] = [];
    for (const name of frontier.slice().sort()) {
      for (const child of children.get(name) ?? []) {
        if (seen.has(child.branch_name)) continue;
        seen.add(child.branch_name);
        out.push(child);
        next.push(child.branch_name);
      }
    }
    frontier = next;
  }
  return out;
}

/**
 * The ordered restack plan for `branch` and everything above it.
 *
 * Empty when the branch has no parent to be replayed onto — a root of the
 * repository is not a stack, and offering to rebase it onto nothing is how a
 * page ends up with a button that cannot do anything.
 *
 * Each descendant carries the tip its parent had **in this snapshot**, which
 * is where it was cut from. After the parent is rebased that commit is no
 * longer reachable from the parent, so nothing computed afterwards can
 * recover it: the fork point has to be read before the first rewrite, which
 * is exactly what the reader was looking at when they pressed the button.
 */
export function cascadePlan(
  nodes: readonly StackedBranchNode[],
  branch: string,
): RestackStep[] {
  const byName = indexNodes(nodes);
  const start = byName.get(branch);
  if (!start?.parent_branch_name) return [];

  const steps: RestackStep[] = [
    { branch, onto: start.parent_branch_name, forkPoint: null },
  ];
  for (const node of descendantsOf(nodes, branch)) {
    const parent = node.parent_branch_name
      ? byName.get(node.parent_branch_name)
      : undefined;
    if (!parent) continue;
    steps.push({
      branch: node.branch_name,
      onto: parent.branch_name,
      forkPoint: parent.tip_commit_id,
    });
  }
  return steps;
}

/** What a branch's row can say beyond its place in the tree. */
export interface StackBranchFacts {
  /** Commits this branch is behind the branch it is compared against. */
  behindBase: number;
  /** The branch `behindBase` is measured against, or null when not measured. */
  comparedTo: string | null;
  /** Commits ahead of / behind the tracking branch; null when untracked. */
  upstream: { name: string; ahead: number; behind: number; gone: boolean } | null;
  lastCommitTimestamp: number;
  lastAuthor: string;
  lastSummary: string;
}

/**
 * Joins a stack node with what the branch list already knows about it.
 *
 * Null when the branch list has no entry — which happens while a repository is
 * still hydrating, and must render as "not known yet" rather than as a row of
 * confident zeroes. A branch reported as zero behind its base when nobody
 * measured it is the same defect as a check that could not run reading as one
 * that passed.
 */
export function stackBranchFacts(
  branchName: string,
  branches: readonly BranchInfo[],
): StackBranchFacts | null {
  const info = branches.find((b) => !b.is_remote && b.name === branchName);
  if (!info) return null;
  return {
    behindBase: info.commits_behind_base,
    comparedTo: info.compared_to ?? null,
    upstream: info.upstream
      ? {
          name: info.upstream,
          ahead: info.ahead_count,
          behind: info.behind_count,
          gone: info.is_gone,
        }
      : null,
    lastCommitTimestamp: info.last_commit_timestamp,
    lastAuthor: info.last_author,
    lastSummary: info.last_summary,
  };
}

/**
 * Local branches the payload placed on no stack at all.
 *
 * These are not an error and not noise: a branch is rootless when its
 * first-parent walk never met another branch's tip, which is what a drifted
 * stack looks like from git's side. Listing them is what keeps "no stacked
 * branches" from meaning "this repository has three branches and we are
 * showing you none of them".
 */
export function rootlessBranches(
  nodes: readonly StackedBranchNode[],
  branches: readonly BranchInfo[],
  defaultBranch: string,
): string[] {
  const children = childrenByParent(nodes);
  const placed = new Set<string>();
  for (const [parent, kids] of children) {
    placed.add(parent);
    for (const kid of kids) placed.add(kid.branch_name);
  }
  return branches
    .filter((b) => !b.is_remote)
    .map((b) => b.name)
    .filter((name) => name !== defaultBranch && !placed.has(name))
    .sort((a, b) => a.localeCompare(b));
}

/**
 * One line describing what updating this stack would do, for the confirmation.
 *
 * Spelled out branch by branch on purpose: this rewrites history on every one
 * of them, and "restack 4 branches" does not tell the reader which four.
 */
export function describeCascade(steps: readonly RestackStep[]): string {
  if (steps.length === 0) return "";
  const names = steps.map((s) => s.branch);
  const onto = steps[0].onto;
  const list = names.length > 6 ? `${names.slice(0, 6).join(", ")} …` : names.join(", ");
  if (names.length === 1) {
    return `Rebase ${names[0]} onto ${onto}. This rewrites its commits.`;
  }
  return `Rebase ${names.length} branches onto ${onto}: ${list}. This rewrites their commits.`;
}
