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
  import { pickLanguageBarStats, type LanguageStat } from "../language/barStats";

  let stats: LanguageStat[] = $state([]);

  // Memoized on currentPath: store emissions from the ~6s status poll and
  // branch-stats drains would otherwise cancel and refetch the language
  // stats on every tick.
  let prevPath: string | null = null;
  $effect(() => {
    const path = $repoStore.currentPath;
    if (path === prevPath) return;
    prevPath = path;
    if (!path) {
      stats = [];
      return;
    }
    const cached = statsCache.get(path);
    if (cached) stats = cached;
    let cancelled = false;
    invoke<LanguageStat[]>("cmd_get_language_stats", {
      repoPath: path,
    })
      .then((s) => {
        if (!cancelled) {
          stats = pickLanguageBarStats(s);
          statsCache.set(path, stats);
        }
      })
      .catch(() => {
        if (!cancelled && !cached) stats = [];
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
