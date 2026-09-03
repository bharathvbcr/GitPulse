<script lang="ts" module>
  import type { Component } from "svelte";

  export type ViewLoader = () => Promise<{ default: Component<any> }>;

  /**
   * Resolved view components, keyed by loader identity.
   *
   * The {#if} chain in App.svelte destroys the outgoing view on every tab
   * switch — state lives in stores, so a remount is cheap and expected. But an
   * `await` of an already-settled module still costs a microtask, so without
   * this cache re-entering a tab would flash the pending branch on *every*
   * switch, reintroducing the full-pane flicker that the chain (rather than
   * `{#key activeTab}`) was written to avoid. Loaders are module-level
   * constants in App.svelte, so their identity is stable across renders.
   */
  const resolved = new Map<ViewLoader, Component<any>>();

  /** Test seam: forget cached chunks so each case starts from a cold load. */
  export function __resetLazyViewCache(): void {
    resolved.clear();
  }
</script>

<script lang="ts">
  import { diagnostics } from "../diagnostics/diagnostics";
  import { formatError } from "../ui/formatError";
  import Skeleton from "./Skeleton.svelte";

  let { load, label }: { load: ViewLoader; label: string } = $props();

  let loaded = $state<Component<any> | undefined>(undefined);
  let failure = $state<unknown>(undefined);
  let attempt = $state(0);

  // Cache hit resolves synchronously at init, so a revisited tab paints the
  // view in its first frame instead of a skeleton.
  const View = $derived(resolved.get(load) ?? loaded);

  $effect(() => {
    // Re-read on retry as well as on tab change.
    attempt;
    const loader = load;
    if (resolved.has(loader)) return;

    // A fast tab switch can leave an earlier chunk in flight; without this
    // guard its late resolution would paint the wrong view over the new one.
    let cancelled = false;
    loaded = undefined;
    failure = undefined;

    loader().then(
      (module) => {
        resolved.set(loader, module.default);
        if (!cancelled) loaded = module.default;
      },
      (error) => {
        // A chunk that failed to load must not read as an empty view.
        diagnostics.error("view-chunk-load", error);
        if (!cancelled) failure = error;
      },
    );

    return () => {
      cancelled = true;
    };
  });
</script>

{#if failure !== undefined}
  <div
    class="flex-1 flex flex-col items-center justify-center gap-2 p-4"
    title={formatError(failure)}
  >
    <span class="text-xs text-textMuted font-sans">{label} failed to load</span>
    <button type="button" class="gp-btn" onclick={() => (attempt += 1)}>Retry</button>
  </div>
{:else if View}
  <View />
{:else}
  <div class="flex-1 flex flex-col gap-3 p-4" aria-label="Loading {label}" aria-busy="true">
    <Skeleton variant="card" count={3} />
  </div>
{/if}
