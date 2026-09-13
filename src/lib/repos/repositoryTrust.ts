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

export async function requestRepositoryTrust(path: string, confirmLabel: string, invokeFn: InvokeFn = invoke): Promise<string | null> {
  const preview = await invokeFn<TrustPreview>("cmd_repository_trust", { repoPath: path });
  if (preview.trusted) return preview.path;
  const approved = await askConfirm({
    title: "Trust this repository?",
    message: `${preview.path}\n\nOpening or operating on this checkout can run Git hooks, helpers, and project tools with your account's permissions, including code in its submodules. Trust it only if you trust its source and contents. This decision also allows GitPulse's MCP tools to read this checkout.`,
    confirmLabel,
    cancelLabel: "Cancel",
  });
  if (!approved) return null;
  await invokeFn("cmd_grant_repository_trust", { repoPath: preview.path, expectedIdentity: preview.identity });
  return preview.path;
}
