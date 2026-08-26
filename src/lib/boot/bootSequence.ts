import type { NativeMenuHandlers } from "../desktop/nativeActions";

/**
 * nativeShell.ts consumes NativeMenuHandlers without re-exporting it;
 * aliasing here keeps App.svelte's boot wiring single-sourced while the
 * shapes stay structurally identical by construction.
 */
export type NativeShellHandlers = NativeMenuHandlers;

export type BootStepName =
  | "native-shell"
  | "pending-open"
  | "workspace-restore"
  | "open-pending"
  | "recent-menu"
  | "repo-changed-listen";

export interface BootStepDeps {
  subscribeNativeShell(handlers: NativeShellHandlers): Promise<() => void>;
  takePendingOpen(): Promise<string | null>;
  restoreWorkspace(): Promise<void>;
  /** openFromExternal — applies a pending external open intent. */
  openRepo(path: string): Promise<void>;
  syncRecentMenu(paths: string[]): Promise<unknown>;
  /** The effect of a repo-changed event (repoStore.handleRepoChanged). */
  handleRepoChanged(path?: string): void;
  listenRepoChanged(handler: (path?: string) => void): Promise<() => void>;
  track(unlisten: () => void): void;
  onError(step: BootStepName, err: unknown): void;
}

/**
 * The app's startup sequence, dependency-injected so every step can be
 * exercised against fakes.
 *
 * Every step is individually guarded: a failure is reported through
 * onError(step, err) and boot CONTINUES with the remaining steps in order.
 * This is the whole point — pre-extraction, a throw in restoreWorkspace
 * aborted everything after it including registration of the repo-changed
 * listener, silently killing watcher-driven refresh for the whole session.
 */
export async function runBootSequence(
  deps: BootStepDeps,
  recentPaths: string[],
  handlers: NativeShellHandlers,
): Promise<void> {
  const guarded = async (step: BootStepName, body: () => Promise<void>): Promise<void> => {
    try {
      await body();
    } catch (err) {
      deps.onError(step, err);
    }
  };

  // A failed native-shell subscription unwinds its own listeners before
  // rethrowing; skipping the rest of boot would lose the persisted session.
  await guarded("native-shell", async () => {
    deps.track(await deps.subscribeNativeShell(handlers));
  });

  // Pending open is captured BEFORE restore so an external open intent is
  // not clobbered by hydration, and applied AFTER restore so hydration wins
  // nothing over it.
  let pending: string | null = null;
  await guarded("pending-open", async () => {
    pending = await deps.takePendingOpen();
  });
  await guarded("workspace-restore", () => deps.restoreWorkspace());
  await guarded("open-pending", async () => {
    if (pending) await deps.openRepo(pending);
  });

  await guarded("recent-menu", async () => {
    await deps.syncRecentMenu(recentPaths);
  });

  // Registered last but reached even when every earlier step threw.
  await guarded("repo-changed-listen", async () => {
    deps.track(
      await deps.listenRepoChanged((path) => deps.handleRepoChanged(path)),
    );
  });
}
