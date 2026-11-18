<script lang="ts">
  import { Check, X } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { themeStore } from "../stores/themeStore";
  import { BUILTIN_BACKDROPS, builtinBackdropUrl, isBuiltinBackdrop } from "../ui/appearance/backdrops";
  import { THEME_PRESETS, sameTheme, type ThemePreset } from "../ui/appearance/presets";
  import { customWallpaper } from "../ui/appearance/wallpaperStore";
  import ThemeTile from "./ThemeTile.svelte";

  /**
   * What is in force now, and the built-in presets, with Default first.
   *
   * A preset applies its theme, wallpaper and material in one step and switches
   * the window to the preset's appearance. A tile reads as picked when its theme
   * is in force, whatever wallpaper was chosen after it: the wallpaper row sits
   * right below and marks its own choice. Default and "Remove theme" are one
   * action, `interfaceStore.clearTheme`, which also takes back whatever the
   * theme stamped that is still as it left it.
   */
  const prefs = $derived($interfaceStore);
  const theme = $derived($themeStore);
  const active = $derived(prefs.customTheme);
  const custom = $derived($customWallpaper);

  let status = $state("");

  const wallpaperName = $derived.by(() => {
    if (prefs.backdrop === "none") return null;
    if (prefs.backdrop === "custom") return custom.status === "ready" ? custom.name : "your image";
    return BUILTIN_BACKDROPS.find((entry) => entry.id === prefs.backdrop)?.label ?? null;
  });

  function apply(preset: ThemePreset) {
    interfaceStore.applyPreset(preset);
    status = `Applied “${preset.label}”.`;
  }

  function clear() {
    if (active === null) return;
    const name = active.name;
    interfaceStore.clearTheme();
    status = `Removed “${name}”. Back to the default palette.`;
  }
</script>

<div class="flex flex-col gap-3" data-theme-gallery>
  <!-- What draws now, and the one way back from it. -->
  <div
    class="flex items-center justify-between gap-2 rounded-lg border border-edge px-2.5 py-2 text-meta"
    data-current-look
    data-theme-active={active ? active.name : undefined}
  >
    <span class="min-w-0 text-textPrimary">
      {#if active}
        Using “{active.name}”{#if wallpaperName}<span class="text-textMuted"> with the {wallpaperName} wallpaper</span>{/if}
        {#if active.appearance !== theme}
          <span class="block text-micro text-textMuted">
            It draws in {active.appearance} mode, so this {theme} window uses the default palette.
          </span>
        {/if}
      {:else}
        Default palette{#if wallpaperName}<span class="text-textMuted"> with the {wallpaperName} wallpaper</span>{/if}
      {/if}
    </span>
    {#if active}
      <button
        type="button"
        class="gp-btn shrink-0"
        data-variant="secondary"
        data-size="sm"
        data-clear-theme
        onclick={clear}
      >
        <X size={12} />
        Remove theme
      </button>
    {/if}
  </div>

  <div class="flex flex-col gap-2">
    <div class="text-meta font-medium text-textPrimary">Presets</div>
    <div class="grid grid-cols-2 gap-2 sm:grid-cols-3" role="group" aria-label="Theme presets">
      <button
        type="button"
        class="flex flex-col gap-1.5 rounded-lg border p-1.5 text-left transition-colors {active === null
          ? 'border-accent bg-accent/10'
          : 'border-edge hover:bg-surfaceHover'}"
        aria-pressed={active === null}
        title="The built-in palette, without a theme"
        data-preset="default"
        onclick={clear}
      >
        <span
          class="flex aspect-[16/10] w-full items-center justify-center rounded-md border border-dashed border-edge text-micro text-textMuted"
          aria-hidden="true"
        >
          No theme
        </span>
        <span class="flex items-center justify-between gap-1 px-0.5">
          <span class="truncate text-meta font-medium text-textPrimary">Default</span>
          {#if active === null}<Check size={12} class="shrink-0 text-accent" />{/if}
        </span>
      </button>
      {#each THEME_PRESETS as preset (preset.id)}
        {@const picked = sameTheme(active, preset.theme)}
        <button
          type="button"
          class="flex flex-col gap-1.5 rounded-lg border p-1.5 text-left transition-colors {picked
            ? 'border-accent bg-accent/10'
            : 'border-edge hover:bg-surfaceHover'}"
          aria-pressed={picked}
          title={preset.description}
          data-preset={preset.id}
          onclick={() => apply(preset)}
        >
          <ThemeTile theme={preset.theme} backdrop={isBuiltinBackdrop(preset.backdrop) ? builtinBackdropUrl(preset.backdrop) : null} />
          <span class="flex items-center justify-between gap-1 px-0.5">
            <span class="truncate text-meta font-medium text-textPrimary">{preset.label}</span>
            {#if picked}<Check size={12} class="shrink-0 text-accent" />{/if}
          </span>
        </button>
      {/each}
    </div>
  </div>

  {#if status}
    <p class="text-meta text-textMuted" role="status" data-theme-gallery-status>{status}</p>
  {/if}
  <p class="text-micro text-textMuted">
    A preset sets the colors, the wallpaper and the material together and switches to its own light or dark mode.
    Every preset passes the same contrast check as a custom accent.
  </p>
</div>
