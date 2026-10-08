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
import { sameRepo, type PathIdentityOptions } from "../repos/paths";

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

/**
 * Whether a registered repository's checkout is where the store says it is.
 *
 * Four answers, and `unknown` is never folded into the other three: a check
 * that could not run must not read as a checkout that is fine, nor as one
 * that is gone. `remote` is a repository registered without any local path —
 * its identity is not `local:` — so there is nothing on disk to look for.
 */
export type CheckoutState = "available" | "missing" | "remote" | "unknown";
export interface CheckoutHealth {
  state: CheckoutState;
  /** Where the checkout was expected, or null for a remote-only repository. */
  path: string | null;
  /** One sentence for a tooltip or a banner. */
  detail: string;
}

/** How a repository record names its checkout; the host's `find_git_root` answers it. */
export type ResolveRoot = (path: string) => Promise<string>;

/** The host's refusal for a folder that holds no repository (`cmd_resolve_git_root`). */
const NOT_A_REPOSITORY = /^Not a Git repository\b/;

export async function readCheckout(
  repo: Pick<Repository, "name" | "identity_key" | "remote_url">,
  resolve: ResolveRoot,
  options: PathIdentityOptions,
): Promise<CheckoutHealth> {
  if (!identityCommonDir(repo.identity_key)) {
    const remote = repo.remote_url?.trim();
    return { state: "remote", path: null, detail: remote ? `${repo.name} has no local checkout; it is known by its remote, ${remote}.` : `${repo.name} has no local checkout registered.` };
  }
  const path = currentLocation(repo);
  try {
    const root = await resolve(path);
    // `find_git_root` walks up: a deleted checkout inside another repository
    // answers with the parent, which is not this checkout either.
    if (sameRepo(root, path, options)) return { state: "available", path, detail: `${repo.name} is checked out at ${path}.` };
    return { state: "missing", path, detail: `${repo.name} is no longer a Git checkout at ${path}. It was moved or deleted; relink it to where it is now.` };
  } catch (cause) {
    const message = explainError(cause);
    if (NOT_A_REPOSITORY.test(message)) return { state: "missing", path, detail: `${repo.name} was not found at ${path}. It was moved or deleted; relink it to where it is now.` };
    return { state: "unknown", path, detail: `Could not check ${repo.name}'s checkout at ${path}: ${message}` };
  }
}

/** At most this many checks in flight, so a large catalog does not flood the host. */
export const CHECKOUT_READ_CONCURRENCY = 6;

/** Every repository's checkout state, keyed by repository id. Never rejects. */
export async function readCheckouts(
  repos: readonly Pick<Repository, "id" | "name" | "identity_key" | "remote_url">[],
  resolve: ResolveRoot,
  options: PathIdentityOptions,
): Promise<Map<string, CheckoutHealth>> {
  const out = new Map<string, CheckoutHealth>();
  let next = 0;
  const worker = async () => {
    while (next < repos.length) {
      const repo = repos[next++];
      out.set(repo.id, await readCheckout(repo, resolve, options));
    }
  };
  await Promise.all(Array.from({ length: Math.min(CHECKOUT_READ_CONCURRENCY, repos.length) }, worker));
  return out;
}

/** The card chip for a task whose primary repository cannot be launched in here, or null. */
export function checkoutFlag(health: CheckoutHealth | undefined): { label: string; detail: string } | null {
  if (!health) return null;
  if (health.state === "missing") return { label: "Checkout missing", detail: health.detail };
  if (health.state === "remote") return { label: "Remote only", detail: health.detail };
  return null;
}

export function relinkConfirmation(repo: Repository, path: string) {
  return {
    title: `Relink ${repo.name}?`,
    message: [
      `Tasks linked to ${repo.name} will follow it to:`,
      path,
      "",
      `${identityCommonDir(repo.identity_key) ? `It was registered at ${currentLocation(repo)}.` : `It had no local checkout${repo.remote_url ? ` (known by its remote, ${repo.remote_url})` : ""}.`} Its tasks, workspaces and run history keep their place.`,
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
