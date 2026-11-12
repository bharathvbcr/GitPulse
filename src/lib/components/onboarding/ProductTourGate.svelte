<script lang="ts">
  /**
   * The product tour draws nothing until it is open, so its component loads
   * then. Imported eagerly it put ~19 KB in the entry chunk of every launch,
   * one of the modules that took `main` over the 640 KB production limit
   * (vite.config.ts). The loader is the shared one (lazyLoaders.ts), so the
   * after-first-paint warm-up and this mount fetch the same chunk.
   */
  import { onMount, type ComponentProps } from "svelte";
  import type ProductTour from "./ProductTour.svelte";
  import { productTour } from "../../tools/productTour";
  import LazyView from "../LazyView.svelte";
  import { loadProductTour } from "../lazyLoaders";

  /** Everything ProductTour takes, passed through as it is. */
  let props: ComponentProps<typeof ProductTour> = $props();
  // The store the tour reads: the app's, unless a host (the harness) hands one.
  // Justified: read once on purpose; a tour does not change stores while it is
  // mounted, and `$tour` needs a plain store binding, not a reactive one.
  // svelte-ignore state_referenced_locally
  const tour = props.tour ?? productTour;
  // The saved progress decides whether the tour opens at launch (first run, or
  // left mid-way). ProductTour used to read it on its own mount, but it now
  // mounts only once the tour is open, so the gate reads it, at launch. It is
  // idempotent, so ProductTour's own call stays harmless. Reading it here also
  // means Settings' replay resumes from the saved step rather than from step 0.
  onMount(() => tour.initialize());
</script>

{#if $tour.open}
  <LazyView load={loadProductTour} name="the product tour" {props} />
{/if}
