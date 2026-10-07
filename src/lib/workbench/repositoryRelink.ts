/**
 * Relinking a registered repository to the checkout it moved to.
 *
 * A repository's identity is its git common directory (`local:<dir>`), so a
 * checkout that was moved or re-cloned resolves to a different identity and,
 * once opened, registers as a second record — with none of the first one's
 * tasks. The store's `repositories.relink` keeps the original record's id and
 * swaps only the identity, so tasks, workspace memberships, runs and their
 * history stay attached. The host resolves the folder; this module only owns
 * the conversation around it: pick, confirm, write, and say what happened.
 *
 * It never decides that two records are one. The store refuses to fold in a
 * record that holds anything, and that refusal reaches the reader verbatim.
 */
import { explainError, newID, relinkRepository, WorkbenchError, type Repository } from "./client";
import { identityCommonDir } from "./openMembership";

export interface RelinkIO {
  /** A folder the reader chose, or null when they cancelled the picker. */
  pick: () => Promise<string | null>;
  confirm: (options: { title: string; message: string; confirmLabel: string }) => Promise<boolean>;
  relink: (repo: Repository, path: string, requestId: string) => Promise<Repository>;
}

export type RelinkOutcome =
  | { kind: "cancelled" }
  | { kind: "relinked"; repository: Repository; path: string; message: string }
  /** The write may have committed. `retry` resends the same request id. */
  | { kind: "uncertain"; message: string; retry: () => Promise<RelinkOutcome> }
  | { kind: "failed"; message: string };

/** Where the store believes the repository lives, for the confirmation. */
export function currentLocation(repo: Pick<Repository, "identity_key">): string {
  const common = identityCommonDir(repo.identity_key);
  if (!common) return repo.identity_key;
  return common.replace(/[\\/]+$/, "").replace(/[\\/]\.git$/, "");
}

export function relinkConfirmation(repo: Repository, path: string) {
  return {
    title: `Relink ${repo.name}?`,
    message: [
      `Tasks linked to ${repo.name} will follow it to:`,
      path,
      "",
      `It was registered at ${currentLocation(repo)}. Its tasks, workspaces and run history keep their place.`,
      "If that folder was opened here already and holds nothing, its empty entry is folded in. A copy with tasks of its own is never merged.",
    ].join("\n"),
    confirmLabel: "Relink",
  };
}

/** A transport loss or worker failure may have committed; a refusal did not. */
function uncertain(cause: unknown): boolean {
  return !(cause instanceof WorkbenchError) || ["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code);
}

async function write(io: RelinkIO, repo: Repository, path: string, requestId: string): Promise<RelinkOutcome> {
  try {
    const repository = await io.relink(repo, path, requestId);
    return { kind: "relinked", repository, path, message: `Relinked ${repository.name} to ${path}` };
  } catch (cause) {
    const message = explainError(cause);
    if (uncertain(cause)) {
      return {
        kind: "uncertain",
        message: `The relink of ${repo.name} may not have finished: ${message}`,
        retry: () => write(io, repo, path, requestId),
      };
    }
    return { kind: "failed", message };
  }
}

export async function relinkCheckout(repo: Repository, io: RelinkIO): Promise<RelinkOutcome> {
  let path: string | null;
  try { path = await io.pick(); }
  catch (cause) { return { kind: "failed", message: explainError(cause) }; }
  if (!path) return { kind: "cancelled" };
  if (!(await io.confirm(relinkConfirmation(repo, path)))) return { kind: "cancelled" };
  return write(io, repo, path, newID());
}

export const defaultRelinkIO = (pick: () => Promise<string | null>, confirm: RelinkIO["confirm"]): RelinkIO => ({
  pick,
  confirm,
  relink: relinkRepository,
});
