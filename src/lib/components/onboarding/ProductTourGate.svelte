<script module lang="ts">
  /**
   * The product tour draws nothing until it is open, so its component loads
   * then. Imported eagerly it put ~19 KB in the entry chunk of every launch,
   * one of the modules that took `main` over the 640 KB production limit
   * (vite.config.ts). Module scope, because LazyView keys its cache on the
   * loader's identity.
   */
  const loadProductTour = () => import("./ProductTour.svelte");
</script>

<script lang="ts">
  import type { ComponentProps } from "svelte";
  import type ProductTour from "./ProductTour.svelte";
  import { productTour } from "../../tools/productTour";
  import LazyView from "../LazyView.svelte";

  /** Everything ProductTour takes, passed through as it is. */
  let props: ComponentProps<typeof ProductTour> = $props();
</script>

{#if $productTour.open}
  <LazyView load={loadProductTour} name="the product tour" {props} />
{/if}
