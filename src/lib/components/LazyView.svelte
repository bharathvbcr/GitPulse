<script module lang="ts">
  import type { Component } from "svelte";
  import { SvelteMap } from "svelte/reactivity";

  /**
   * A view's dynamic import. Must be a STABLE reference — declare it once at
   * module scope in the parent, never inline as `load={() => import(...)}`.
   * An inline arrow is a new function on every render, which misses the cache
   * below and remounts the view on every parent update.
   */
  export type ViewLoader = () => Promise<{ default: Component }>;

  /**
   * Views already fetched, keyed by loader.
   *
   * The tab chain in App.svelte destroys and recreates the pane on every
   * switch. `import()` has its own module cache, but it still resolves on a
   * later microtask — long enough for a spinner to paint — so without this a
   * deferred view would flash on *every* visit rather than only the first.
   *
   * A `SvelteMap` rather than a plain one: the entry is written from the load
   * callback, and a plain map's `get` is not something `$derived` can track,
   * so the first view would resolve and never render.
   */
  const resolved = new SvelteMap<ViewLoader, Component>();

  /** Test seam: how many distinct views have been fetched so far. */
  export function loadedViewCount(): number {
    return resolved.size;
  }
</script>

<script lang="ts">
  interface Props {
    load: ViewLoader;
    /**
     * Human name for the pending and failure states. A chunk that fails to
     * load must say which view is missing rather than leave an empty pane —
     * an empty pane is indistinguishable from a view with nothing to show.
     */
    name: string;
  }

  let { load, name }: Props = $props();

  let view = $derived(resolved.get(load));
  let failure = $state<string | null>(null);

  $effect(() => {
    const loader = load;
    if (resolved.has(loader)) return;
    // Guarded rather than fire-and-forget: the pane can be switched away
    // before the chunk lands, and a late failure from an abandoned load must
    // not paint an error over whatever the user is looking at now.
    let cancelled = false;
    failure = null;
    loader().then(
      (module) => {
        resolved.set(loader, module.default);
      },
      (error: unknown) => {
        if (cancelled) return;
        failure = error instanceof Error ? error.message : String(error);
      },
    );
    return () => {
      cancelled = true;
    };
  });
</script>

{#if view}
  {@const View = view}
  <View />
{:else if failure}
  <div
    class="flex-1 flex flex-col items-center justify-center gap-2 p-6 text-center font-sans"
    role="alert"
  >
    <span class="text-sm text-textPrimary">{name} could not be loaded.</span>
    <span class="text-[11px] text-textMuted max-w-md">{failure}</span>
    <span class="text-[11px] text-textMuted">
      This view ships as its own chunk, so a failed load means the install is
      incomplete — not that the view has nothing to show. Reopening the window
      retries it.
    </span>
  </div>
{:else}
  <div class="flex-1 flex items-center justify-center font-sans text-[11px] text-textMuted">
    Loading {name}…
  </div>
{/if}
