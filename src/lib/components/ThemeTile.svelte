<script lang="ts">
  import { themePreview } from "../ui/appearance/presets";
  import type { GitPulseTheme } from "../ui/appearance/themeSchema";

  /**
   * A miniature window in a theme's own derived palette (after the contrast
   * guard): a title bar, a sidebar and three rows over the wallpaper, if any.
   * Drawn by the preset gallery and the My themes shelf.
   */
  let { theme, backdrop = null }: { theme: GitPulseTheme; backdrop?: string | null } = $props();
  const c = $derived(themePreview(theme));
</script>

<span
  class="relative block aspect-[16/10] w-full overflow-hidden rounded-md border border-edge"
  style:background-color={c.bg}
  aria-hidden="true"
>
  {#if backdrop}
    <img src={backdrop} alt="" class="absolute inset-0 size-full object-cover opacity-90" loading="lazy" decoding="async" />
  {/if}
  <span
    class="absolute inset-x-[12%] top-[16%] bottom-[14%] flex flex-col overflow-hidden rounded-sm border"
    style:background-color={c.bg}
    style:border-color={c.border}
    style:opacity={backdrop ? 0.8 : 1}
  >
    <span class="flex h-[18%] items-center gap-[3%] px-[5%]">
      <span class="size-1 rounded-full" style:background-color={c.accent}></span>
      <span class="h-[30%] w-1/3 rounded-full" style:background-color={c.textMuted}></span>
    </span>
    <span class="flex flex-1 gap-[4%] px-[5%] pb-[6%]">
      <span class="w-1/4 rounded-sm" style:background-color={c.surface}></span>
      <span class="flex flex-1 flex-col gap-[10%] pt-[4%]">
        <span class="h-[14%] w-4/5 rounded-full" style:background-color={c.accent}></span>
        <span class="h-[12%] w-3/5 rounded-full" style:background-color={c.text}></span>
        <span class="h-[12%] w-2/3 rounded-full" style:background-color={c.textMuted}></span>
      </span>
    </span>
  </span>
</span>
