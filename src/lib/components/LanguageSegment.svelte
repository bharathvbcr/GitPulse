<script lang="ts">
  import { onMount } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import LanguageLogo from "./LanguageLogo.svelte";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { locMetric } from "../metrics/repoMetrics";
  import type { MetricSnapshot } from "../metrics/freshness";
  import {
    describeLanguageMix,
    type LanguageMix,
    type LanguageStat,
    type LanguageStatsReport,
  } from "../language/barStats";

  /**
   * The language breakdown, as a status-bar segment rather than a full-width
   * strip.
   *
   * It was a permanent 32px row that accepted no input beyond a click-to-
   * filter it shares with the file tree — reference material charged rent as
   * chrome. Compact here, expandable to the same breakdown it always showed.
   *
   * The scan itself is NOT fetched here. `locMetric` already owns
   * `cmd_get_language_stats` ("the headline LOC number and the language bar
   * are two readings of one scan"), and the old bar ran its own invoke and
   * its own cache beside it — two fetches, two caches, and one of them
   * swallowed every failure. Subscribing to the metric means the scan runs
   * once, revalidates as the repository changes, and reports its failures to
   * diagnostics instead of blanking silently.
   */

  const EMPTY: LanguageMix = {
    stats: [],
    dominant: null,
    partial: false,
    partialNotice: null,
    failed: false,
  };

  /** Everything drawn below, derived by one tested function. */
  let mix = $state<LanguageMix>(EMPTY);

  let open = $state(false);
  let anchor = $state<{ x: number; bottom: number } | null>(null);

  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path) {
      mix = EMPTY;
      open = false;
      return;
    }
    return locMetric.subscribe(path, (snap: MetricSnapshot<LanguageStatsReport>) => {
      mix = describeLanguageMix(snap);
    });
  });

  function tipFor(lang: LanguageStat): string {
    const base = `${lang.language} ${lang.percentage}%`;
    if (!lang.other_languages?.length) return base;
    return `${base}\n${lang.other_languages.join(", ")}`;
  }

  function handleLanguageClick(lang: LanguageStat) {
    if (lang.language === "Other") return;
    repoStore.setActiveTab("code", "explorer");
    if (typeof window !== "undefined") {
      window.dispatchEvent(
        new CustomEvent("gitpulse:filter-lang", { detail: { language: lang.language } }),
      );
    }
    close();
  }

  function close() {
    open = false;
    anchor = null;
  }

  function toggle(event: MouseEvent) {
    if (open) {
      close();
      return;
    }
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    // Anchored from the bottom: the status bar is the last row on screen, so
    // the panel has to grow upward or it opens off the window.
    anchor = { x: rect.left, bottom: window.innerHeight - rect.top + 6 };
    open = true;
  }

  function handlePointerDown(event: PointerEvent) {
    if (!open) return;
    if (!shouldDismissOverlay(event.target, "[data-language-panel], [data-language-trigger]")) {
      return;
    }
    close();
  }

  function handleKey(event: KeyboardEvent) {
    if (event.key === "Escape" && open) {
      event.preventDefault();
      close();
    }
  }

  onMount(() => {
    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKey);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("resize", close);
    };
  });
</script>

{#if mix.stats.length > 0 && mix.dominant}
  <button
    type="button"
    data-language-trigger
    aria-haspopup="dialog"
    aria-expanded={open}
    onclick={toggle}
    class="inline-flex items-center gap-1.5 text-textMuted hover:text-textPrimary transition-colors shrink-0"
    title="{mix.dominant.language} {mix.dominant.percentage}% — click for the full language breakdown"
  >
    <span class="h-1.5 w-9 flex rounded-full overflow-hidden bg-background ring-1 ring-border/50 shrink-0">
      {#each mix.stats as lang (lang.language)}
        <span
          style="width: {Number.isFinite(lang.percentage) ? lang.percentage : 0}%; background-color: {lang.color_hex};"
        ></span>
      {/each}
    </span>
    <span class="font-medium">{mix.dominant.language}</span>
    <span class="tabular-nums text-[10px] text-textMuted/70">{mix.dominant.percentage}%</span>
    {#if mix.partial}
      <!-- The capped-scan marker rides the compact form too. A floor that
           reads as a total is the one thing this segment must never do. -->
      <span class="text-amber-600 dark:text-amber-400" title={mix.partialNotice ?? "Partial scan"}>⚠</span>
    {/if}
  </button>
{:else if mix.failed}
  <span class="inline-flex items-center gap-1 text-amber-600 dark:text-amber-400 shrink-0" title="The language scan failed; see Diagnostics">
    <span>⚠</span><span>Languages</span>
  </span>
{/if}

{#if open && anchor}
  <div
    use:portal={"body"}
    data-language-panel
    role="dialog"
    aria-label="Language breakdown"
    class="fixed w-72 gp-menu gp-pop p-3 text-xs text-textPrimary"
    style="left: {anchor.x}px; bottom: {anchor.bottom}px; z-index: {LAYERS.MENU}"
  >
    <div class="h-1.5 flex rounded-full overflow-hidden bg-background ring-1 ring-border/50 mb-2.5">
      {#each mix.stats as lang (lang.language)}
        <div
          style="width: {Number.isFinite(lang.percentage) ? lang.percentage : 0}%; background-color: {lang.color_hex};"
          title={tipFor(lang)}
        ></div>
      {/each}
    </div>

    {#if mix.partialNotice}
      <p class="mb-2 text-[10px] text-amber-600 dark:text-amber-400 leading-snug">
        ⚠ {mix.partialNotice}
      </p>
    {/if}

    <div class="flex flex-col gap-0.5 max-h-64 overflow-y-auto">
      {#each mix.stats as lang (lang.language)}
        <button
          type="button"
          onclick={() => handleLanguageClick(lang)}
          disabled={lang.language === "Other"}
          class="flex items-center gap-2 px-1.5 py-1 rounded-md hover:bg-surfaceHover transition-colors text-left disabled:hover:bg-transparent disabled:cursor-default"
          title={tipFor(lang) + (lang.language !== "Other" ? " — Click to view files" : "")}
        >
          <LanguageLogo language={lang.language} size={13} class="shrink-0" />
          <span class="text-textPrimary/90 font-medium truncate flex-1">{lang.language}</span>
          <span class="text-textMuted tabular-nums text-[10px] shrink-0">{lang.percentage}%</span>
        </button>
      {/each}
    </div>
  </div>
{/if}
