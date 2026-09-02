<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";

  // Survives remounts ({#key} on currentPath re-creates this bar on every
  // repo switch) so switching back to a repo renders its bar instantly; the
  // fetch then refreshes it in place.
  const statsCache = createRepoPanelCache<LanguageStat[]>();
</script>

<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import LanguageLogo from "./LanguageLogo.svelte";
  import {
    pickLanguageBarStats,
    type LanguageStat,
    type LanguageStatsReport,
  } from "../language/barStats";

  /** Wire shape of `crate::engine::git_reader::LanguageStatsReport`. */

  let stats: LanguageStat[] = $state([]);
  /** Non-null when the shown percentages cover only part of the worktree. */
  let partialNotice: string | null = $state(null);
  let isLoading = $state(false);

  // Memoized on currentPath: store emissions from the ~6s status poll and
  // branch-stats drains re-trigger $effect. We track activeRequestId monotonically
  // so that unrelated store emissions for the SAME path do NOT invalidate an in-flight
  // request. When the path genuinely switches, a new request starts and prior responses
  // are discarded.
  let currentActivePath: string | null = null;
  let activeRequestId = 0;

  $effect(() => {
    const path = $repoStore.currentPath;
    if (path === currentActivePath) return;
    currentActivePath = path;
    const reqId = ++activeRequestId;

    if (!path) {
      stats = [];
      partialNotice = null;
      isLoading = false;
      return;
    }

    const cached = statsCache.get(path);
    if (cached) {
      stats = cached;
      isLoading = false;
    } else {
      isLoading = true;
    }

    invoke<LanguageStatsReport>("cmd_get_language_stats", {
      repoPath: path,
    })
      .then((report) => {
        if (reqId === activeRequestId) {
          stats = pickLanguageBarStats(report.stats);
          partialNotice = report.truncated
            ? `Partial scan: ${report.scanned_files} of ${report.candidate_files} files counted`
            : null;
          statsCache.set(path, stats);
          isLoading = false;
        }
      })
      .catch(() => {
        if (reqId === activeRequestId) {
          if (!cached) {
            stats = [];
            partialNotice = null;
          }
          isLoading = false;
        }
      });
  });

  function tipFor(lang: LanguageStat): string {
    const base = `${lang.language} ${lang.percentage}%`;
    if (!lang.other_languages?.length) return base;
    return `${base}\n${lang.other_languages.join(", ")}`;
  }

  function handleLanguageClick(lang: LanguageStat) {
    if (lang.language === "Other") return;
    repoStore.setActiveTab("files");
    if (typeof window !== "undefined") {
      window.dispatchEvent(
        new CustomEvent("gitpulse:filter-lang", { detail: { language: lang.language } }),
      );
    }
  }
</script>

{#if stats.length > 0}
  <div class="h-8 bg-surface/60 border-b border-border/60 px-4 flex items-center justify-between text-[11px] select-none">
    <div class="flex items-center gap-3 flex-1 max-w-md">
      <div class="h-1.5 flex-1 flex rounded-full overflow-hidden bg-background ring-1 ring-border/50">
        {#each stats as lang}
          <div
            style="width: {Number.isFinite(lang.percentage) ? lang.percentage : 0}%; background-color: {lang.color_hex};"
            title={tipFor(lang)}
          ></div>
        {/each}
      </div>
    </div>
    <div class="flex items-center gap-2 sm:gap-3 text-textMuted overflow-x-auto">
      {#if partialNotice}
        <span
          class="text-[10px] text-amber-400/90 shrink-0"
          title="The scan stopped early (time budget); percentages cover only the files counted"
        >
          ⚠ {partialNotice}
        </span>
      {/if}
      {#each stats as lang}
        <button
          type="button"
          onclick={() => handleLanguageClick(lang)}
          class="flex items-center gap-1.5 px-1.5 py-0.5 rounded-md hover:bg-surfaceHover transition-colors shrink-0 {lang.language !== 'Other' ? 'cursor-pointer' : 'cursor-default'}"
          title={tipFor(lang) + (lang.language !== "Other" ? " — Click to view files" : "")}
        >
          <LanguageLogo language={lang.language} size={13} class="shrink-0" />
          <span class="text-textPrimary/90 font-medium">{lang.language}</span>
          <span class="text-textMuted/70 tabular-nums text-[10px]">{lang.percentage}%</span>
        </button>
      {/each}
    </div>
  </div>
{:else if isLoading}
  <div class="h-8 bg-surface/60 border-b border-border/60 px-4 flex items-center justify-between text-[11px] select-none text-textMuted">
    <div class="flex items-center gap-3 flex-1 max-w-xs">
      <div class="h-1.5 flex-1 rounded-full overflow-hidden bg-background ring-1 ring-border/50">
        <div class="h-full w-1/3 bg-border rounded-full animate-pulse"></div>
      </div>
    </div>
    <div class="flex items-center gap-2 text-[10px] text-textMuted/70">
      <span>Analyzing repository languages...</span>
    </div>
  </div>
{/if}
