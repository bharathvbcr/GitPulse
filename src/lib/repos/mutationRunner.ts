import { writable } from "svelte/store";
import { harnessStore, type PolicyVerdict } from "../stores/harnessStore";
import { describeError } from "../ui/copy/describeFailure";
import { isSectionOnScreen } from "../views/viewRegistry";
import type { HydrationFacet } from "./hydration";
import type { MutationOutcome, MutationTarget, RepoSession } from "./storeTypes";

/** The refusal a mutation returns when its captured target no longer names the repository. */
export const REPOSITORY_CHANGED =
  "The repository changed while this was waiting; nothing was run.";

/**
 * `runMutating` options for a mutation that rewrites the index or worktree
 * bytes and cannot move a ref: stage, unstage, patch staging, discard.
 */
export const WORKTREE_ONLY = {
  facet: "status",
} as const satisfies { facet: HydrationFacet };

/**
 * Mutation kinds that rewrite what the open worktree diff pane shows.
 * After these, the open selection is refetched so the pane stops displaying
 * pre-mutation content.
 */
export const REFETCH_SELECTION_KINDS = new Set([
  "stage",
  "unstage",
  "stage-all",
  "unstage-all",
  "stage-patch",
  "unstage-patch",
  "discard",
  "commit",
]);

/**
 * Files a `Guarded<T>` result's verdict with the harness store, and clears the
 * last verdict for an action that carried none.
 */
export function recordPolicyVerdict(
  result: unknown,
  repoPath: string,
): PolicyVerdict | undefined {
  if (result && typeof result === "object" && "policy" in result) {
    const policy = (result as { policy: PolicyVerdict }).policy;
    if (policy && typeof policy.status === "string") {
      harnessStore.recordVerdict(policy, repoPath);
      return policy;
    }
  }
  harnessStore.recordVerdict(null, repoPath);
  return undefined;
}

export function mutationOutput<T>(result: unknown): T | undefined {
  if (result && typeof result === "object" && "output" in result) {
    return (result as { output: T }).output;
  }
  return result === undefined ? undefined : (result as T);
}

export interface MutationRunnerDeps {
  activeSession(): RepoSession | undefined;
  sessionById(id: string): RepoSession | undefined;
  activeId(): string | null;
  resolveTarget(target: MutationTarget): RepoSession | null;
  applyToSession(
    id: string,
    generation: number,
    patch: Partial<RepoSession>,
  ): boolean;
  refresh(repoPath?: string, facet?: HydrationFacet): Promise<void>;
  selectFileDiff(filePath: string, isStaged: boolean): Promise<void>;
  markEchoSuccess(path: string): void;
  replayDroppedEchoes(path: string, refreshed: boolean): void;
}

/**
 * Owns per-repository mutation queuing, active mutation tracking for the quit
 * guard and watch echo rule, policy verdict recording, and post-mutation
 * snapshot/diff refresh.
 */
export function createMutationRunner(deps: MutationRunnerDeps) {
  const mutationActivity = writable<Record<string, string[]>>({});
  const mutations = new Map<number, { path: string; kind: string }>();
  let mutationSequence = 0;

  function beginMutation(path: string, kind: string): () => void {
    const token = ++mutationSequence;
    const publishActivity = () => {
      const activity: Record<string, string[]> = {};
      for (const entry of mutations.values()) {
        (activity[entry.path] ??= []).push(entry.kind);
      }
      mutationActivity.set(activity);
    };
    mutations.set(token, { path, kind });
    publishActivity();
    return () => {
      mutations.delete(token);
      publishActivity();
    };
  }

  function holdsPath(path: string): boolean {
    for (const entry of mutations.values()) {
      if (entry.path === path) return true;
    }
    return false;
  }

  const pathQueues = new Map<string, Promise<void>>();
  function enterPathQueue(path: string): Promise<() => void> {
    const previous = pathQueues.get(path) ?? Promise.resolve();
    let release!: () => void;
    const mine = new Promise<void>((resolve) => {
      release = resolve;
    });
    const tail = previous.then(() => mine);
    pathQueues.set(path, tail);
    void tail.then(() => {
      if (pathQueues.get(path) === tail) pathQueues.delete(path);
    });
    return previous.then(() => release);
  }

  async function runMutating<T = unknown>(
    kind: string,
    label: string,
    action: (path: string) => Promise<unknown>,
    opts: {
      skipRefresh?: boolean;
      facet?: HydrationFacet;
      session?: RepoSession;
      target?: MutationTarget | null;
      trackActivity?: boolean;
      expected?: (err: unknown) => boolean;
    } = {},
  ): Promise<MutationOutcome<T>> {
    let session: RepoSession | undefined;
    if (opts.target !== undefined) {
      session = (opts.target && deps.resolveTarget(opts.target)) || undefined;
      if (!session) {
        return {
          ok: false,
          error: REPOSITORY_CHANGED,
          refused: true,
          surfaced: false,
        };
      }
    } else {
      session = opts.session ?? deps.activeSession();
      if (!session) return { ok: false, error: "No repository is open." };
    }
    const path = session.path;
    const generation = session.generation;
    const finishActivity =
      opts.trackActivity === false ? () => {} : beginMutation(path, kind);
    const release = await enterPathQueue(path);
    let succeeded = false;
    let refreshed = false;
    try {
      const stillNamed =
        opts.target !== undefined
          ? opts.target !== null && deps.resolveTarget(opts.target) !== null
          : deps.sessionById(session.id) !== undefined;
      if (!stillNamed) {
        return {
          ok: false,
          error: REPOSITORY_CHANGED,
          refused: true,
          surfaced: false,
        };
      }
      const result = await action(path);
      succeeded = true;
      const policy = recordPolicyVerdict(result, path);
      harnessStore.recordAction({
        repoPath: path,
        kind,
        label,
        ok: true,
        verdict: policy ?? null,
      });
      const still = deps.sessionById(session.id);
      if (still && still.generation === generation && !opts.skipRefresh) {
        const operationLive =
          still.operation.operation !== null || still.operation.probeFailed;
        const refreshFacet: HydrationFacet =
          opts.facet && still.hasHydrated && !operationLive ? opts.facet : "all";
        await deps.refresh(path, refreshFacet);
        refreshed = true;
        if (
          deps.activeId() === session.id &&
          REFETCH_SELECTION_KINDS.has(kind) &&
          still.selectionKind === "file" &&
          still.selectedFilePath &&
          !still.selectedCommitId &&
          isSectionOnScreen(still.activeTab, still.viewSections, "history", "diff")
        ) {
          void deps.selectFileDiff(still.selectedFilePath, still.selectedIsStaged);
        }
      }
      const output = mutationOutput<T>(result);
      return output === undefined
        ? { ok: true, policy }
        : { ok: true, policy, output };
    } catch (err: unknown) {
      const error = describeError(err, "repo");
      harnessStore.recordAction({ repoPath: path, kind, label, ok: false });
      if (opts.expected?.(err)) {
        return { ok: false, error, cause: err, surfaced: false };
      }
      deps.applyToSession(session.id, generation, { error });
      return {
        ok: false,
        error,
        cause: err,
        surfaced:
          deps.activeId() === session.id &&
          deps.sessionById(session.id)?.error === error,
      };
    } finally {
      release();
      try {
        if (succeeded) deps.markEchoSuccess(path);
        finishActivity();
      } finally {
        deps.replayDroppedEchoes(path, refreshed);
      }
    }
  }

  return {
    mutationActivity: { subscribe: mutationActivity.subscribe },
    beginMutation,
    holdsPath,
    enterPathQueue,
    runMutating,
  };
}
