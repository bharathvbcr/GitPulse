import { untrack } from "svelte";

/**
 * What a component that outlives a repository tab switch needs to stay honest.
 *
 * Sidebar and the status bar are mounted once per window, not once per
 * repository: switching tabs re-points the store they read, it does not
 * rebuild them. A remount used to be the thing that threw away every draft,
 * filter, open menu, scroll offset and in-flight answer that belonged to the
 * repository being left. This is the explicit version of that: one hook per
 * stateful component, declared next to the state it clears.
 *
 * The rule for a `reset` is the old lifecycle, spelled out. Do what the
 * component's teardown did (cancel timers and in-flight work, close what is
 * open) and put every per-repository `$state` back to its initial value.
 * Preferences that belong to the person rather than the repository (a
 * collapsed section, a density) stay.
 */
export interface RepoScope {
  /**
   * Bumped once per repository change. An async continuation captures it
   * before it awaits and drops its result when `isCurrent` says the
   * repository it was started for is no longer the one on screen.
   */
  readonly epoch: number;
  isCurrent(epoch: number): boolean;
}

/**
 * Runs `reset` when the active repository changes, before the DOM updates for
 * it, and never on mount: the initial state is the mounted state.
 *
 * Call from component initialisation. `activePath` is read inside the effect,
 * so passing `() => $repoStore.currentPath` makes the change tracked; `reset`
 * runs untracked, so writing `$state` in it cannot loop.
 */
export function repoScope(
  activePath: () => string | null,
  reset: (path: string | null, previous: string | null) => void,
): RepoScope {
  let epoch = 0;
  let previous = untrack(activePath);
  $effect.pre(() => {
    const path = activePath();
    untrack(() => {
      if (path === previous) return;
      const was = previous;
      previous = path;
      epoch += 1;
      reset(path, was);
    });
  });
  return {
    get epoch() {
      return epoch;
    },
    isCurrent: (captured) => captured === epoch,
  };
}
