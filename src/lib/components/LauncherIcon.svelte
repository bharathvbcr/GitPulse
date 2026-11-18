<script lang="ts" module>
  import type BrandMarkComponent from "./BrandMark.svelte";

  let cachedBrandMark: typeof BrandMarkComponent | null = null;
  let brandMarkPromise: Promise<typeof BrandMarkComponent | null> | null = null;

  function loadBrandMark(): Promise<typeof BrandMarkComponent | null> {
    if (!brandMarkPromise) {
      brandMarkPromise = import("./BrandMark.svelte")
        .then((mod) => {
          cachedBrandMark = mod.default;
          return mod.default;
        })
        .catch(() => {
          brandMarkPromise = null;
          return null;
        });
    }
    return brandMarkPromise;
  }
</script>

<script lang="ts">
  import { launcherIcon } from "../terminal/launcherIcons";
  import type { TabKind } from "../terminal/tabs";

  /**
   * What a terminal-dock tab runs, as a glyph. Decorative: every call site
   * says the kind in text too (a label, a title or screen-reader text).
   *
   * BrandMark's SVG path table is fetched once on first mark render and cached
   * at module scope so the startup entry chunk stays lean.
   */
  interface Props {
    kind: TabKind | null | undefined;
    size?: number;
    class?: string;
  }
  let { kind, size = 12, class: className = "" }: Props = $props();

  const spec = $derived(launcherIcon(kind));
  let BrandMark = $state<typeof BrandMarkComponent | null>(cachedBrandMark);

  $effect(() => {
    if (spec.mark && !BrandMark) {
      void loadBrandMark().then((component) => {
        if (component) BrandMark = component;
      });
    }
  });
</script>

<span
  class="launcher-icon inline-flex shrink-0 items-center justify-center {className}"
  style:color={spec.tint}
  style:width="{size}px"
  style:height="{size}px"
  aria-hidden="true"
  data-launcher-icon={kind ?? "shell"}
>
  {#if spec.mark}
    {#if BrandMark}
      <BrandMark mark={spec.mark} {size} />
    {/if}
  {:else if spec.icon}
    <spec.icon {size} />
  {/if}
</span>
