/**
 * Which live attempt is working in a checkout.
 *
 * Its own module rather than a helper in `attemptWorktree.ts` because the
 * Worktrees panel, which the sidebar mounts at startup, needs only this
 * lookup. Importing it from `attemptWorktree.ts` put that module — and, through
 * it, the whole of `taskSessions.ts`, which the task board shares — in the
 * entry chunk, and took the entry past the build's size budget
 * (`vite.config.ts`, `MAX_PRODUCTION_CHUNK_BYTES`).
 */
import { identityKey, isCaseInsensitiveFs } from "../repos/paths";
import type { TaskRun } from "./client";

/**
 * The live attempt working in a worktree, by checkout identity, from a read of
 * live runs. A GitPulse lane with no live attempt answers undefined: its
 * attempt ended (its worktree stays on disk), which a live read cannot name.
 */
export function liveRunIn<T extends Pick<TaskRun, "cwd">>(path: string, runs: readonly T[]): T | undefined {
  const opts = { caseInsensitive: isCaseInsensitiveFs() };
  const key = identityKey(path, opts);
  return key ? runs.find((run) => identityKey(run.cwd, opts) === key) : undefined;
}
