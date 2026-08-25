<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { FileCode, Search } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { coverageHitClass } from "../coverage/format";
  import type { FileCoverage } from "../coverage/types";
  import { shortHash } from "../format";
  import { formatError } from "../ui/formatError";
  import VirtualList from "./VirtualList.svelte";
  import EmptyState from "./EmptyState.svelte";

  interface BlameLine {
    line_no: number;
    commit_id: string;
    author_name: string;
    author_email: string;
    timestamp: number;
    content: string;
  }

  let filePath = $state("");
  let blameLines: BlameLine[] = $state([]);
  let coverageHits = $state<Map<number, number>>(new Map());
  let isLoading = $state(false);
  let errorMsg = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;

  async function loadBlameFor(repo: string, path: string) {
    if (!repo || !path) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    errorMsg = null;
    try {
      const next = await invoke<BlameLine[]>("cmd_get_file_blame", {
        repoPath: repo,
        filePath: path,
      });
      if (!guard.isLive()) return;
      blameLines = next;
      try {
        const coverage = await invoke<FileCoverage>("cmd_get_file_coverage", {
          repoPath: repo,
          filePath: path,
        });
        if (!guard.isLive()) return;
        const hits = new Map<number, number>();
        for (const line of coverage.lines) {
          hits.set(line.line_no, line.hits);
        }
        coverageHits = hits;
      } catch {
        if (!guard.isLive()) return;
        coverageHits = new Map();
      }
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      errorMsg = formatError(err);
      blameLines = [];
      coverageHits = new Map();
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  function loadBlame() {
    const repo = $repoStore.currentPath;
    const path = filePath.trim();
    if (!repo || !path) return;
    void loadBlameFor(repo, path);
  }

  $effect(() => {
    return () => inflight?.cancel();
  });

  $effect(() => {
    const selected = $repoStore.selectedFilePath;
    const repo = $repoStore.currentPath;
    if (selected) {
      filePath = selected;
    }
    if (!repo) {
      inflight?.cancel();
      blameLines = [];
      coverageHits = new Map();
      errorMsg = null;
      isLoading = false;
      return;
    }
    if (!selected) return;
    void loadBlameFor(repo, selected);
    const started = inflight;
    return () => {
      if (inflight === started) {
        started?.cancel();
      }
    };
  });

  function getHeatmapColor(ts: number): string {
    if (!ts) return "transparent";
    const nowSec = Math.floor(Date.now() / 1000);
    const daysAgo = Math.max(0, (nowSec - ts) / 86400);

    if (daysAgo <= 7) return "rgba(239, 68, 68, 0.20)"; // Red (<7d)
    if (daysAgo <= 30) return "rgba(245, 158, 11, 0.15)"; // Amber (<30d)
    if (daysAgo <= 90) return "rgba(59, 130, 246, 0.12)"; // Blue (<90d)
    return "rgba(107, 114, 128, 0.08)"; // Gray (>90d)
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-mono select-none overflow-hidden">
  <!-- Toolbar -->
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between font-sans">
    <div class="flex items-center gap-3">
      <FileCode size={16} class="text-accent" />
      <div class="flex items-center gap-1 bg-background border border-border/80 rounded-full px-3 py-1 focus-within:border-accent/60 transition-colors">
        <input
          type="text"
          bind:value={filePath}
          onkeydown={(e) => e.key === "Enter" && loadBlame()}
          placeholder="Path to file in repo..."
          class="w-64 bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-none font-mono"
        />
        <button onclick={loadBlame} class="p-1 rounded-full hover:text-accent hover:bg-surfaceHover">
          <Search size={13} />
        </button>
      </div>
    </div>

    <!-- Legend -->
    <div class="flex items-center gap-3 text-[11px] text-textMuted">
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-red-500/40"></span> &lt; 7d</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-amber-500/40"></span> &lt; 30d</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-blue-500/40"></span> &lt; 90d</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-gray-500/30"></span> Older</span>
    </div>
  </div>

  <!-- Blame Lines -->
  <div class="flex-1 min-h-0 flex flex-col">
    {#if isLoading}
      <div class="h-full flex items-center justify-center text-textMuted font-sans text-xs">
        Loading blame for {filePath}...
      </div>
    {:else if errorMsg}
      <div class="h-full flex items-center justify-center text-rose-400 font-sans text-xs p-4">
        {errorMsg}
      </div>
    {:else if blameLines.length > 0}
      <VirtualList items={blameLines} rowHeight={24} overscan={15} class="h-full px-1.5 py-1">
        {#snippet row(line)}
          {#if line}
            <div
              class="flex items-center h-6 rounded-md px-1 hover:bg-surfaceHover/80 transition-colors"
              style="background-color: {getHeatmapColor(line.timestamp)}"
            >
              <button
                type="button"
                class="w-16 px-2 text-[10px] text-accent/80 font-mono select-none cursor-pointer hover:underline text-left shrink-0"
                onclick={() => {
                  repoStore.inspectCommitInHistory(line.commit_id);
                }}
              >{shortHash(line.commit_id)}</button>
              <span class="w-24 px-2 text-[10px] text-textMuted truncate font-sans shrink-0">{line.author_name}</span>
              <span class="w-8 px-2 text-right text-textMuted/40 text-[10px] select-none shrink-0">{line.line_no}</span>
              <span class="w-8 px-1 text-right text-[10px] tabular-nums shrink-0 {coverageHits.get(line.line_no) === undefined ? 'text-transparent' : (coverageHits.get(line.line_no) ?? 0) > 0 ? 'text-emerald-400/80' : 'text-red-400/80'}">{coverageHits.get(line.line_no) ?? "·"}</span>
              <span class="px-3 whitespace-pre overflow-hidden text-textPrimary {coverageHitClass(coverageHits.get(line.line_no))}">{line.content}</span>
            </div>
          {/if}
        {/snippet}
      </VirtualList>
    {:else}
      <EmptyState
        icon={Search}
        title="No blame loaded"
        hint="Enter a file path above to view git blame heatmaps."
      />
    {/if}
  </div>
</div>
