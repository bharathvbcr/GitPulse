/**
 * Which repository the Agents view is narrowed to, when a surface opened it
 * for one (Fleet's agent count). Session-only: a scope a person did not ask
 * for again on the next launch would hide rows without saying why.
 */
import { writable, type Readable } from "svelte/store";

const scope = writable<string | null>(null);

export const agentsRepositoryScope: Readable<string | null> = { subscribe: scope.subscribe };

/** Narrows the Agents view to one repository's rows; null shows every repository. */
export function setAgentsRepositoryScope(repoPath: string | null): void {
  const trimmed = typeof repoPath === "string" ? repoPath.trim() : "";
  scope.set(trimmed || null);
}
