import { invoke } from "./invoke";

/**
 * One owner for `cmd_github_context`: four `gh` round trips, up to 45 s.
 *
 * The Work view, the GitHub panel and the MANVI panel each asked for it, each
 * with a cache of its own, and Work asked again on every return. This module
 * answers all of them from one read: a caller that arrives while the read is
 * in flight joins it, and one that arrives within `GITHUB_CONTEXT_FRESH_MS` of
 * a clean answer gets that answer.
 *
 * What is shared is the payload exactly as the backend sent it, so every
 * consumer sees the same `error`, `*_error` and `warnings` fields and renders
 * the same degradation. Sharing never turns a failure into an empty result:
 *
 *  - a rejected read is handed to every caller that joined it and kept for none;
 *  - a payload that reports a failed or partial section is handed to the
 *    callers that joined it and is not kept, so the next caller reads again.
 *
 * Each caller applies its own deadline and cancellation to the wait; abandoning
 * a wait does not abandon the read the others are sharing.
 */
export const GITHUB_CONTEXT_FRESH_MS = 15_000;

interface Entry {
  promise: Promise<unknown>;
  /** When the read finished cleanly; null while it is in flight. */
  keptAt: number | null;
}

const entries = new Map<string, Entry>();

/** True when a section of the payload could not be read, or the whole of it. */
export function githubContextDegraded(payload: unknown): boolean {
  if (payload === null || typeof payload !== "object") return true;
  for (const [key, value] of Object.entries(payload)) {
    if ((key === "error" || key.endsWith("_error")) && typeof value === "string" && value.length > 0) return true;
    if (key === "warnings" && Array.isArray(value) && value.length > 0) return true;
  }
  return false;
}

export function sharedGitHubContext<T>(
  repoPath: string,
  read: () => Promise<T>,
  options: { force?: boolean; now?: () => number } = {},
): Promise<T> {
  const now = options.now ?? Date.now;
  const held = entries.get(repoPath);
  if (held && !options.force && (held.keptAt === null || now() - held.keptAt < GITHUB_CONTEXT_FRESH_MS)) {
    return held.promise as Promise<T>;
  }
  const entry: Entry = { keptAt: null, promise: Promise.resolve() };
  entry.promise = read().then(
    (payload) => {
      if (entries.get(repoPath) === entry) {
        if (githubContextDegraded(payload)) entries.delete(repoPath);
        else entry.keptAt = now();
      }
      return payload;
    },
    (error: unknown) => {
      if (entries.get(repoPath) === entry) entries.delete(repoPath);
      throw error;
    },
  );
  entries.set(repoPath, entry);
  return entry.promise as Promise<T>;
}

/** Forget one repository's answer, or all of them (a refresh the person asked for). */
export function invalidateGitHubContext(repoPath?: string): void {
  if (repoPath === undefined) entries.clear();
  else entries.delete(repoPath);
}

/**
 * `invoke`, except `cmd_github_context` goes through the shared read. Every
 * other command is passed through untouched.
 */
export const invokeSharingGitHubContext: typeof invoke = <T>(
  command: string,
  args?: Parameters<typeof invoke>[1],
  options?: Parameters<typeof invoke>[2],
): Promise<T> => {
  const repoPath = typeof args === "object" && args !== null && "repoPath" in args ? args.repoPath : undefined;
  if (command === "cmd_github_context" && typeof repoPath === "string") {
    return sharedGitHubContext(repoPath, () => invoke<T>(command, args, options));
  }
  return invoke<T>(command, args, options);
};
