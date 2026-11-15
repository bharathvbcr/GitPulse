<script lang="ts">
  import { Check, Save, X } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { themeStore } from "../stores/themeStore";
  import { builtinBackdropUrl, isBuiltinBackdrop } from "../ui/appearance/backdrops";
  import { THEME_PRESETS, sameTheme, themePreview, type ThemePreset } from "../ui/appearance/presets";
  import { MAX_SAVED_THEMES } from "../ui/appearance/savedThemes";
  import { exportCurrentTheme } from "../ui/appearance/themeFile";
  import type { GitPulseTheme } from "../ui/appearance/themeSchema";

  /**
   * The theme gallery: built-in presets, then the reader's own saved themes.
   *
   * A preset applies its theme, wallpaper and material in one step and switches
   * the window to the preset's appearance. A saved theme applies its palette and
   * leaves the wallpaper alone. Both go through the same validator and contrast
   * guard as an imported file; the gallery adds no second palette path.
   */
  const prefs = $derived($interfaceStore);
  const theme = $derived($themeStore);
  const active = $derived(prefs.customTheme);

  let saveName = $state("My theme");
  let status = $state("");
  let failed = $state(false);

  function presetActive(preset: ThemePreset): boolean {
    return sameTheme(active, preset.theme) && prefs.backdrop === preset.backdrop;
  }

  function apply(preset: ThemePreset) {
    interfaceStore.applyPreset(preset);
    failed = false;
    status = `Applied “${preset.label}”.`;
  }

  function applySaved(saved: GitPulseTheme) {
    interfaceStore.applyTheme(saved);
    failed = false;
    status = `Applied “${saved.name}”.`;
  }

  function remove(saved: GitPulseTheme) {
    interfaceStore.removeSavedTheme(saved);
    failed = false;
    status = `Removed “${saved.name}” from your themes.`;
  }

  function saveCurrent() {
    const name = saveName.trim() === "" ? "My theme" : saveName.trim();
    const result = exportCurrentTheme(prefs, theme, name);
    if (!result.ok) {
      failed = true;
      status = `Couldn't save the theme: ${result.reason}`;
      return;
    }
    const outcome = interfaceStore.saveTheme(result.theme);
    failed = outcome === "full";
    status =
      outcome === "full"
        ? `You already have ${MAX_SAVED_THEMES} saved themes. Remove one to save another.`
        : outcome === "replaced"
          ? `Updated “${result.theme.name}”.`
          : `Saved “${result.theme.name}” to your themes.`;
  }
</script>

<!-- A miniature window in the theme's own derived palette (after the contrast
     guard): a title bar, a sidebar and three rows over the wallpaper, if any. -->
{#snippet tile(theme: GitPulseTheme, backdrop: string | null)}
  {@const c = themePreview(theme)}
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
{/snippet}

<div class="flex flex-col gap-3" data-theme-gallery>
  <div class="grid grid-cols-2 gap-2 sm:grid-cols-3" role="group" aria-label="Theme presets">
    {#each THEME_PRESETS as preset (preset.id)}
      {@const picked = presetActive(preset)}
      <button
        type="button"
        class="group flex flex-col gap-1.5 rounded-lg border p-1.5 text-left transition-colors {picked
          ? 'border-accent bg-accent/10'
          : 'border-edge hover:bg-surfaceHover'}"
        aria-pressed={picked}
        title={preset.description}
        data-preset={preset.id}
        onclick={() => apply(preset)}
      >
        {@render tile(preset.theme, isBuiltinBackdrop(preset.backdrop) ? builtinBackdropUrl(preset.backdrop) : null)}
        <span class="flex items-center justify-between gap-1 px-0.5">
          <span class="truncate text-meta font-medium text-textPrimary">{preset.label}</span>
          {#if picked}<Check size={12} class="shrink-0 text-accent" />{/if}
        </span>
      </button>
    {/each}
  </div>

  <div class="flex flex-col gap-2" data-saved-themes>
    <div class="flex items-center justify-between gap-2">
      <span class="text-meta font-medium text-textPrimary">My themes</span>
      <span class="text-micro text-textMuted">{prefs.savedThemes.length} of {MAX_SAVED_THEMES}</span>
    </div>
    {#if prefs.savedThemes.length === 0}
      <p class="text-micro text-textMuted" data-saved-themes-empty>
        Save the look you have now, or import a theme file below. Imported themes are kept here too.
      </p>
    {:else}
      <div class="grid grid-cols-2 gap-2 sm:grid-cols-3" role="group" aria-label="My themes">
        {#each prefs.savedThemes as saved (`${saved.appearance}:${saved.name}`)}
          {@const picked = sameTheme(active, saved)}
          <div
            class="relative flex flex-col gap-1.5 rounded-lg border p-1.5 {picked
              ? 'border-accent bg-accent/10'
              : 'border-edge'}"
            data-saved-theme={saved.name}
          >
            <button
              type="button"
              class="flex flex-col gap-1.5 text-left"
              aria-pressed={picked}
              title="Apply “{saved.name}” ({saved.appearance})"
              onclick={() => applySaved(saved)}
            >
              {@render tile(saved, null)}
              <span class="flex items-center gap-1 px-0.5 pr-5">
                <span class="truncate text-meta font-medium text-textPrimary">{saved.name}</span>
                {#if picked}<Check size={12} class="shrink-0 text-accent" />{/if}
              </span>
            </button>
            <button
              type="button"
              class="gp-icon-btn absolute right-1 bottom-1"
              aria-label="Remove “{saved.name}” from my themes"
              title="Remove"
              onclick={() => remove(saved)}
            >
              <X size={12} />
            </button>
          </div>
        {/each}
      </div>
    {/if}

    <div class="flex flex-wrap items-center gap-2">
      <input
        type="text"
        class="gp-field min-w-0 flex-1"
        bind:value={saveName}
        maxlength="64"
        aria-label="Name for the saved theme"
        data-save-theme-name
      />
      <button type="button" class="gp-btn" data-variant="secondary" data-size="sm" onclick={saveCurrent}>
        <Save size={12} />
        Save current look
      </button>
    </div>
  </div>

  {#if status}
    <p class="text-meta {failed ? 'text-warn' : 'text-textMuted'}" role="status" data-theme-gallery-status>{status}</p>
  {/if}
  <p class="text-micro text-textMuted">
    A preset sets the colors, the wallpaper and the material together, and switches to its own light or dark mode.
    Every preset passes the same contrast check as a custom accent.
  </p>
</div>
