import { invoke } from "@tauri-apps/api/core";
import { askConfirm } from "../stores/modalStore";
import type { InvokeFn } from "../stores/graphStore";

// How far the stored approval reaches. Mirrors `repository_trust::TrustScope`.
//
// "checkout" is an approval recorded before the repository became the unit of
// trust. It still admits the one checkout it named — nothing is re-prompted
// for, nothing is widened by reading it — but it reaches none of that
// repository's worktrees, and only the human can extend it.
export type TrustScope = "none" | "checkout" | "repository";

export interface TrustPreview {
  path: string;
  git_dir: string;
  common_dir: string;
  identity: string;
  scope: TrustScope;
  /** Working trees this repository has, counted from its own registry. */
  worktrees: number;
}

// A linked worktree keeps its own private Git directory under the shared one;
// the checkout the repository lives in has them the same.
export function isLinkedWorktree(preview: TrustPreview): boolean {
  return preview.git_dir !== preview.common_dir;
}

/**
 * Whether this checkout may run Git at all — the gate's own question.
 *
 * Deliberately not the same question as "is there anything left to ask", which
 * is `needsExtension`. Collapsing the two is what shipped: one boolean meant
 * both, so a repository whose every worktree was refused reported "already
 * trusted" and the offer to fix it was never made.
 */
export function isAdmitted(preview: TrustPreview): boolean {
  return preview.scope !== "none";
}

/**
 * Whether extending this approval would change anything.
 *
 * Only for a pre-repository approval, and only where there is a sibling
 * working tree to cover: on a repository with a single checkout the older
 * record already reaches everything there is, so asking would be a prompt that
 * buys the person nothing. Adding a worktree later brings them here through
 * that worktree's own refusal, which is a moment the question makes sense.
 */
export function needsExtension(preview: TrustPreview): boolean {
  return preview.scope === "checkout" && preview.worktrees > 1;
}

function scopeOf(preview: TrustPreview): string {
  const shared = `They share one configuration, one set of hooks, and one object database, which is what this decision is about.`;
  return isLinkedWorktree(preview)
    ? `This is a linked worktree of the repository at ${preview.common_dir}. Trusting it covers that whole repository — its main checkout and every worktree of it, including ones added later. ${shared}`
    : `This covers the whole repository — this checkout and every linked worktree of it, including ones added later. ${shared}`;
}

// The extension is a narrower decision than a first approval and has to read
// like one: this checkout already runs Git, and what is being asked for is the
// rest of the family. Saying "trust this repository" to someone who did that
// months ago is how the dead end reads from the inside.
function extensionMessage(preview: TrustPreview): string {
  const others = preview.worktrees - 1;
  return (
    `${preview.path}\n\n` +
    `You approved this repository before GitPulse covered worktrees, so that approval reaches only this checkout. ` +
    `Its ${others} other working tree${others === 1 ? "" : "s"} cannot be read — worktree comparisons, collision checks, and the fleet view leave ${others === 1 ? "it" : "them"} out and say so.\n\n` +
    `Extending covers this repository and every worktree of it, including ones added later. ` +
    `They share one configuration, one set of hooks, and one object database, which is what this decision is about.`
  );
}

/**
 * Ensure the approval covering `path` reaches what the caller needs.
 *
 * Returns the canonical path when the checkout may be operated on, and null
 * only when it may not. Declining an *extension* returns the path: the
 * checkout was already admitted, and turning a declined upgrade into a failed
 * open would punish the person for being asked.
 */
export async function requestRepositoryTrust(path: string, confirmLabel: string, invokeFn: InvokeFn = invoke): Promise<string | null> {
  const preview = await invokeFn<TrustPreview>("cmd_repository_trust", { repoPath: path });
  if (isAdmitted(preview) && !needsExtension(preview)) return preview.path;

  const extending = needsExtension(preview);
  const approved = await askConfirm({
    title: extending ? "Extend trust to this repository's worktrees?" : "Trust this repository?",
    message: extending
      ? extensionMessage(preview)
      : `${preview.path}\n\nOpening or operating on this checkout can run Git hooks, helpers, and project tools with your account's permissions, including code in its submodules. Trust it only if you trust its source and contents. This decision also allows GitPulse's MCP tools to read this checkout.\n\n${scopeOf(preview)}`,
    confirmLabel: extending ? "Extend Trust" : confirmLabel,
    cancelLabel: extending ? "Not Now" : "Cancel",
  });
  if (!approved) return extending ? preview.path : null;
  await invokeFn("cmd_grant_repository_trust", { repoPath: preview.path, expectedIdentity: preview.identity });
  return preview.path;
}
