/**
 * Which repository a checkout belongs to.
 *
 * Every worktree of a repository shares one common Git directory, so that
 * directory — resolved by the backend from filesystem metadata, never guessed
 * from a path layout — is the family's identity. Two tabs with the same key are
 * the same repository checked out twice; that is the fact the tab strip stacks
 * on. A tab whose common directory could not be read has no family and stands
 * alone: joining it to a guess would put one repository's work under another's
 * name.
 *
 * The root is what a person calls the repository: the directory holding
 * `.git` for an ordinary repository (the primary checkout), or the bare
 * directory itself.
 */
import { agentLayout } from "../work/agentWorktree";
import {
  disambiguateLabels,
  displayName,
  identityKey,
  normalizeRepoPath,
  pathSegments,
  type PathIdentityOptions,
} from "./paths";

export interface RepoFamily {
  /** Identity of the common Git directory; equal for every checkout. */
  key: string;
  /** The repository's own directory, for naming it. */
  root: string;
}

/** Family of a checkout from the backend's `common_dir`; null when unknown. */
export function familyFromCommonDir(
  commonDir: unknown,
  options: PathIdentityOptions,
): RepoFamily | null {
  if (typeof commonDir !== "string") return null;
  const normalized = normalizeRepoPath(commonDir);
  if (!normalized) return null;
  const key = identityKey(normalized, options);
  if (!key) return null;
  const segments = pathSegments(normalized);
  const last = segments[segments.length - 1] ?? "";
  if (last.toLowerCase() !== ".git" || segments.length < 2) {
    return { key, root: normalized };
  }
  // Only the final `/.git` is cut, so a UNC or drive prefix survives intact.
  const root = normalizeRepoPath(normalized.slice(0, normalized.length - last.length));
  return { key, root: root ?? normalized };
}

/** What a repository is called on the strip: its directory, minus a bare `.git`. */
export function familyName(root: string): string {
  const name = displayName(root);
  const stripped = name.replace(/\.git$/i, "");
  return stripped || name;
}

/**
 * One distinct label per family. Two repositories that are both called `api`
 * must not stack under headers that read the same, so the root paths are
 * widened exactly as tab labels are.
 */
export function familyLabels(roots: Iterable<string>): Map<string, string> {
  const unique = Array.from(new Set(roots));
  const widened = disambiguateLabels(unique);
  const labels = new Map<string, string>();
  for (const root of unique) {
    const label = widened.get(root) ?? displayName(root);
    const name = familyName(root);
    // Widening only happens on a collision; keep the bare-repo trim when the
    // label is still the leaf alone.
    labels.set(root, label === displayName(root) ? name : label);
  }
  return labels;
}

export interface CheckoutName {
  /** What to call this checkout among its siblings. */
  name: string;
  /** The checkout the repository lives in (the one holding `.git`). */
  primary: boolean;
  /** Agent that created the worktree (`claude`, `codex`, …), or empty. */
  agent: string;
}

/**
 * How one checkout is named inside its repository's stack. The primary
 * checkout keeps its tab label; an agent worktree is called by its session
 * slug, with the agent reported separately rather than folded into the name;
 * anything else keeps its tab label.
 */
export function checkoutName(
  path: string,
  label: string,
  root: string,
  identity: (path: string) => string,
): CheckoutName {
  const primary = identity(path) === identity(root);
  const layout = primary ? null : agentLayout(path);
  return {
    name: layout?.slug || label,
    primary,
    agent: layout?.kind ?? "",
  };
}
