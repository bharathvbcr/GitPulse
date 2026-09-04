<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";


  // Survives the per-tab remount so revisiting the reflog view renders the
  // last-known table instantly; the fetch then refreshes it in place.
  const reflogCache = createRepoPanelCache<ReflogEntry[]>();
</script>

<script lang="ts">
  import type { ReflogEntry } from "../branches/types";
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { History } from "lucide-svelte";
  import EmptyState from "./EmptyState.svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatDate, shortHash } from "../format";
  import { reportPanelError } from "../diagnostics/report";

  const REFLOG_ENTRY_LIMIT = 200;

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
        maxEntries: REFLOG_ENTRY_LIMIT,
      });
      if (!guard.isLive()) return;
      entries = next;
      reflogCache.set(repo, next);
    } catch (err) {
      if (!guard.isLive()) return;
      errorMsg = reportPanelError("reflog", err);
      entries = [];
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  function inspectEntry(entry: ReflogEntry) {
    repoStore.inspectCommitInHistory(entry.commit_id);
  }

  $effect(() => {
    return () => inflight?.cancel();
  });

  // Memoized on currentPath: the ~6s status poll and stats drains re-emit the
  // store object, so an unguarded rerun would cancel and restart the reflog
  // IPC on every emission.
  let prevRepo: string | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    if (repo === prevRepo) return;
    prevRepo = repo;
    if (!repo) {
      inflight?.cancel();
      entries = [];
      errorMsg = null;
      loading = false;
      return;
    }
    // Hydrate last-known entries synchronously so a revisit renders instantly
    // and the refresh below updates the table in place.
    const cached = reflogCache.get(repo);
    if (cached) entries = cached;
    void load();
    const started = inflight;
    return () => {
      if (inflight === started) {
        started?.cancel();
      }
    };
  });
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans overflow-hidden">
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between shrink-0">
    <div class="flex items-center gap-2">
      <History size={16} class="text-accent" />
      <span class="font-semibold text-textPrimary">Reflog</span>
    </div>
    <button onclick={load} class="gp-btn">Refresh</button>
  </div>

  <div class="flex-1 min-h-0 flex flex-col">
    {#if loading && entries.length === 0}
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
      {#if entries.length >= REFLOG_ENTRY_LIMIT}
        <div class="shrink-0 border-b border-amber-500/30 bg-amber-500/10 px-4 py-1.5 text-[11px] text-amber-600 dark:text-amber-300" role="status">
          Showing the 200 most recent reflog entries. Older reflog history may exist.
        </div>
      {/if}
      <div class="flex-1 min-h-0 overflow-auto">
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
                class="group border-t border-border/30 hover:bg-surfaceHover/60 transition-colors focus-within:bg-surfaceHover"
              >
                <td class="px-3 py-1.5 font-mono rounded-l-lg">
                  <button
                    type="button"
                    aria-label={`Inspect ${entry.selector}, commit ${shortHash(entry.commit_id, 8)}, ${entry.action}: ${entry.message}, ${formatDate(entry.timestamp)}`}
                    title="Inspect this commit in History"
                    onclick={() => inspectEntry(entry)}
                    class="rounded px-1 -mx-1 text-accent hover:underline focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent"
                  >
                    {entry.selector}
                  </button>
                </td>
                <td class="px-3 py-1.5 font-mono">{shortHash(entry.commit_id, 8)}</td>
                <td class="px-3 py-1.5 text-textPrimary">{entry.action}</td>
                <td class="px-3 py-1.5 text-textMuted truncate max-w-md">{entry.message}</td>
                <td class="px-3 py-1.5 text-textMuted whitespace-nowrap rounded-r-lg">{formatDate(entry.timestamp)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</div>
