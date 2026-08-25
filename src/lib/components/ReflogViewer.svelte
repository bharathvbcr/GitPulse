<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { History } from "lucide-svelte";
  import EmptyState from "./EmptyState.svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";

  interface ReflogEntry {
    index: number;
    commit_id: string;
    selector: string;
    action: string;
    message: string;
    timestamp: number;
  }

  let entries: ReflogEntry[] = $state([]);
  let loading = $state(false);
  let errorMsg = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;

  async function load() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    errorMsg = null;
    try {
      const next = await invoke<ReflogEntry[]>("cmd_get_reflog", {
        repoPath: repo,
        maxEntries: 200,
      });
      if (!guard.isLive()) return;
      entries = next;
    } catch (err) {
      if (!guard.isLive()) return;
      errorMsg = String(err);
      entries = [];
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  $effect(() => {
    return () => inflight?.cancel();
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    if (!repo) {
      inflight?.cancel();
      entries = [];
      errorMsg = null;
      loading = false;
      return;
    }
    void load();
    const started = inflight;
    return () => {
      if (inflight === started) {
        started?.cancel();
      }
    };
  });

  function formatTime(ts: number): string {
    if (!ts) return "";
    return new Date(ts * 1000).toLocaleString();
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans overflow-hidden">
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between">
    <div class="flex items-center gap-2">
      <History size={16} class="text-accent" />
      <span class="font-semibold text-textPrimary">Reflog</span>
    </div>
    <button onclick={load} class="gp-btn">Refresh</button>
  </div>

  <div class="flex-1 min-h-0 flex flex-col">
    {#if loading}
      <div class="p-4 text-textMuted">Loading reflog…</div>
    {:else if errorMsg}
      <div class="p-4 text-rose-400">{errorMsg}</div>
    {:else if entries.length === 0}
      <EmptyState
        icon={History}
        title="No reflog entries"
        hint="Reference updates (commits, checkouts, resets, merges) will appear here."
      />
    {:else}
      <table class="w-full text-left">
        <thead class="sticky top-0 bg-surface text-[10px] uppercase text-textMuted">
          <tr>
            <th class="px-3 py-2 font-medium">Selector</th>
            <th class="px-3 py-2 font-medium">SHA</th>
            <th class="px-3 py-2 font-medium">Action</th>
            <th class="px-3 py-2 font-medium">Message</th>
            <th class="px-3 py-2 font-medium">When</th>
          </tr>
        </thead>
        <tbody>
          {#each entries as entry}
            <tr
              class="border-t border-border/30 hover:bg-surfaceHover/60 cursor-pointer transition-colors"
              onclick={() => repoStore.selectCommitDiff(entry.commit_id)}
            >
              <td class="px-3 py-1.5 font-mono text-accent rounded-l-lg">{entry.selector}</td>
              <td class="px-3 py-1.5 font-mono">{entry.commit_id.substring(0, 8)}</td>
              <td class="px-3 py-1.5 text-textPrimary">{entry.action}</td>
              <td class="px-3 py-1.5 text-textMuted truncate max-w-md">{entry.message}</td>
              <td class="px-3 py-1.5 text-textMuted whitespace-nowrap rounded-r-lg">{formatTime(entry.timestamp)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>