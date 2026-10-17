/**
 * A stand-in for the repoStore members `attemptWorktree` acts through, for its
 * tests. Each wrapper does what `runMutating` does at its edges and nothing
 * more: it refuses a missing or no-longer-named target without sending, and it
 * turns a throw into `{ ok: false, cause }`. What it sends goes to `send`, so a
 * test reads the host command and its arguments exactly as before the store
 * owned them.
 */
export interface FakeTarget { tabId: string; path: string }

export const REPOSITORY_CHANGED = "The repository changed while this was waiting; nothing was run.";

/** The host command each wrapper sends, named by the test: this file is scanned for direct sends. */
export interface FakeCommands { merge: string; remove: string; deleteBranch: string }

export function fakeAttemptStore(send: (command: string, args: Record<string, unknown>) => Promise<unknown>, commands: FakeCommands) {
  /** The targets each wrapper was called with, in order. */
  const seen: { command: string; target: FakeTarget | null | undefined }[] = [];
  const state = { stillNamed: (_target: FakeTarget) => true, openFails: false, mainOpen: true };
  /** The paths `targetForPath` opened a tab for, in order: a lookup that only looks opens nothing. */
  const opened: string[] = [];
  async function run(command: string, args: Record<string, unknown>, target: FakeTarget | null | undefined) {
    seen.push({ command, target });
    if (!target || !state.stillNamed(target)) return { ok: false, error: REPOSITORY_CHANGED, refused: true, surfaced: false };
    try {
      const result = (await send(command, { repoPath: target.path, ...args })) as { output?: unknown } | null | undefined;
      return { ok: true, output: result && typeof result === "object" && "output" in result ? result.output : result ?? undefined };
    } catch (cause) {
      return { ok: false, cause, error: cause instanceof Error ? cause.message : String(cause), surfaced: false };
    }
  }
  const members = {
    targetForPath: async (path: string, lookup: { open?: boolean } = {}): Promise<FakeTarget | null> => {
      const target = { tabId: `tab:${path}`, path };
      if (state.mainOpen) return target;
      if (lookup.open === false) return null;
      if (state.openFails) return null;
      opened.push(path);
      state.mainOpen = true;
      return target;
    },
    worktreeMergeTeardown: (request: Record<string, unknown>, target?: FakeTarget | null) => run(commands.merge, request, target),
    removeWorktree: (targetPath: string, force: boolean, target?: FakeTarget | null) => run(commands.remove, { targetPath, force }, target),
    deleteBranch: (branchName: string, force: boolean, target?: FakeTarget | null) => run(commands.deleteBranch, { branchName, force }, target),
  };
  return { members, seen, state, opened };
}
