<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { pickLanguageBarStats, type LanguageStat } from "../language/barStats";

  /** Wire shape of `crate::engine::git_reader::LanguageStatsReport`. */
  interface LanguageStatsReport {
    stats: LanguageStat[];
    /** True when the backend scan stopped early (deadline or cap). */
    truncated: boolean;
    scanned_files: number;
    candidate_files: number;
  }

  let stats: LanguageStat[] = $state([]);
  /** Non-null when the shown percentages cover only part of the worktree. */
  let partialNotice: string | null = $state(null);

  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path) {
      stats = [];
      partialNotice = null;
      return;
    }
    let cancelled = false;
    invoke<LanguageStatsReport>("cmd_get_language_stats", {
      repoPath: path,
    })
      .then((report) => {
        if (cancelled) return;
        stats = pickLanguageBarStats(report.stats);
        partialNotice = report.truncated
          ? `Partial scan: ${report.scanned_files} of ${report.candidate_files} files counted`
          : null;
      })
      .catch(() => {
        if (!cancelled) {
          stats = [];
          partialNotice = null;
        }
      });
    return () => {
      cancelled = true;
    };
  });

  function tipFor(lang: LanguageStat): string {
    const base = `${lang.language} ${lang.percentage}%`;
    if (!lang.other_languages?.length) return base;
    return `${base}\n${lang.other_languages.join(", ")}`;
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
    <div class="flex items-center gap-4 text-textMuted">
      {#if partialNotice}
        <span
          class="text-[10px] text-amber-400/90"
          title="The scan stopped early (time budget); percentages cover only the files counted"
        >
          ⚠ {partialNotice}
        </span>
      {/if}
      {#each stats as lang}
        <div class="flex items-center gap-1.5" title={tipFor(lang)}>
          <span class="w-2 h-2 rounded-full shadow-sm" style="background-color: {lang.color_hex};"></span>
          <span class="text-textPrimary/80">{lang.language}</span>
          <span class="text-textMuted/70 tabular-nums">{lang.percentage}%</span>
        </div>
      {/each}
    </div>
  </div>
{/if}
