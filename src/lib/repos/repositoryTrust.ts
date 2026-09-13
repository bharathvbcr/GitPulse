import { invoke } from "@tauri-apps/api/core";
import { askConfirm } from "../stores/modalStore";
import type { InvokeFn } from "../stores/graphStore";

export interface TrustPreview {
  path: string;
  git_dir: string;
  common_dir: string;
  identity: string;
  trusted: boolean;
}

// A linked worktree keeps its own private Git directory under the shared one;
// the checkout the repository lives in has them the same.
export function isLinkedWorktree(preview: TrustPreview): boolean {
  return preview.git_dir !== preview.common_dir;
}

// What the approval reaches, said before it is given rather than discovered
// afterwards: the decision is about a repository, so it has to name the scope
// it actually carries.
function scopeOf(preview: TrustPreview): string {
  const shared = `They share one configuration, one set of hooks, and one object database, which is what this decision is about.`;
  return isLinkedWorktree(preview)
    ? `This is a linked worktree of the repository at ${preview.common_dir}. Trusting it covers that whole repository — its main checkout and every worktree of it, including ones added later. ${shared}`
    : `This covers the whole repository — this checkout and every linked worktree of it, including ones added later. ${shared}`;
}

export async function requestRepositoryTrust(path: string, confirmLabel: string, invokeFn: InvokeFn = invoke): Promise<string | null> {
  const preview = await invokeFn<TrustPreview>("cmd_repository_trust", { repoPath: path });
  if (preview.trusted) return preview.path;
  const approved = await askConfirm({
    title: "Trust this repository?",
    message: `${preview.path}\n\nOpening or operating on this checkout can run Git hooks, helpers, and project tools with your account's permissions, including code in its submodules. Trust it only if you trust its source and contents. This decision also allows GitPulse's MCP tools to read this checkout.\n\n${scopeOf(preview)}`,
    confirmLabel,
    cancelLabel: "Cancel",
  });
  if (!approved) return null;
  await invokeFn("cmd_grant_repository_trust", { repoPath: preview.path, expectedIdentity: preview.identity });
  return preview.path;
}
