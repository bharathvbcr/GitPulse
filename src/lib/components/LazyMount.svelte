<script module lang="ts">
  import type { Component } from "svelte";
  import { SvelteMap } from "svelte/reactivity";

  /**
   * A deferred component's dynamic import. Must be a STABLE reference —
   * declared once at module scope, never inline as `load={() => import(...)}`,
   * for the same reason LazyView says so: an inline arrow is a new function
   * every render, misses the cache and remounts the component.
   */
  /*
   * Props are erased at this seam and checked at each call site instead. A
   * concrete prop type here would have to be the UNION of every deferred
   * component's props, which no single loader satisfies — the strict version
   * of this type rejected the first caller that had required props.
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  export type MountLoader = () => Promise<{ default: Component<any> }>;

  /** Components already fetched, keyed by loader. Shared across instances. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const resolved = new SvelteMap<MountLoader, Component<any>>();

  /** Test seam: how many distinct deferred components have been fetched. */
  export function mountedComponentCount(): number {
    return resolved.size;
  }
</script>

<script lang="ts">
  import { diagnostics } from "../diagnostics/diagnostics";
  import { toastStore } from "../stores/toastStore";

  /**
   * Mounts a deferred component with props, rendering NOTHING until it lands.
   *
   * The difference from `LazyView` is what the pending and failed states owe
   * the user. A view fills the pane, so it says "Loading Coverage…" and shows
   * its failure in place. An overlay has no pane: a modal that has not arrived
   * yet must draw nothing rather than paint a placeholder over the app, and a
   * modal that fails to arrive has to say so somewhere the user is looking —
   * a toast — because the surface it would have owned does not exist.
   *
   * Callers keep the component mounted once opened (rather than unmounting on
   * close) so exit transitions still play and the second open is instant; this
   * component only decides WHEN the code arrives, not how long it stays.
   */
  interface Props {
    load: MountLoader;
    /** Human name, used only when the load fails. */
    name: string;
    /** Spread onto the component once it resolves. */
    props?: Record<string, unknown>;
  }

  let { load, name, props = {} }: Props = $props();

  let view = $derived(resolved.get(load));

  $effect(() => {
    const loader = load;
    if (resolved.has(loader)) return;
    // Guarded like LazyView's: the caller can close and unmount before the
    // chunk lands, and a late failure from an abandoned load must not raise a
    // toast about something the user is no longer waiting for.
    let cancelled = false;
    loader().then(
      (module) => {
        resolved.set(loader, module.default);
      },
      (error: unknown) => {
        if (cancelled) return;
        diagnostics.error(`lazy-mount:${name}`, error);
        toastStore.error(
          `${name} could not be loaded. It ships as its own chunk, so this means the install is incomplete — reopening the window retries it.`,
        );
      },
    );
    return () => {
      cancelled = true;
    };
  });
</script>

{#if view}
  {@const Mounted = view}
  <Mounted {...props} />
{/if}
