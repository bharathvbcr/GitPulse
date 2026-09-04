<script lang="ts">
  import type { BlameLine } from "../files/types";
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { FileCode, PanelLeftClose, PanelLeftOpen, Search } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { coverageHitClass } from "../coverage/format";
  import { buildHitMap, fetchFileCoverage, hitBadgeClass } from "../coverage/fileCoverage";
  import { shortHash } from "../format";
  import { reportPanelError } from "../diagnostics/report";
  import VirtualList from "./VirtualList.svelte";
  import EmptyState from "./EmptyState.svelte";
  import FileTreePanel from "./files/FileTreePanel.svelte";


  // Worktree-only blame lines carry an all-zero OID from --line-porcelain;
  // they have no commit to inspect and must not render as a dead link.
  const ZERO_OID_RE = /^0+$/;

  let filePath = $state("");
  let blameLines: BlameLine[] = $state([]);
  let coverageHits = $state<Map<number, number>>(new Map());
  // Distinguishes "file has no coverage data" (dim dots) from "the coverage
  // lookup itself failed" — a silent catch would conflate the two.
  let coverageFailed = $state(false);
  let isLoading = $state(false);
  let errorMsg = $state<string | null>(null);
  let explorerOpen = $state(true);
  let inflight: AsyncGuard | null = null;

  async function loadBlameFor(repo: string, path: string) {
    if (!repo || !path) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    errorMsg = null;
    // The previous file's verdict must not bleed into the loading frame:
    // old lines linger until the guard applies, so clear eagerly.
    coverageFailed = false;
    try {
      const [next, coverage] = await Promise.all([
        invoke<BlameLine[]>("cmd_get_file_blame", {
          repoPath: repo,
          filePath: path,
        }),
        fetchFileCoverage(repo, path)
          .then((res) => ({ ok: true as const, hits: buildHitMap(res.lines) }))
          .catch(() => ({ ok: false as const, hits: new Map<number, number>() })),
      ]);
      if (!guard.isLive()) return;
      blameLines = next;
      coverageHits = coverage.hits;
      coverageFailed = !coverage.ok;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      errorMsg = reportPanelError("blame", err);
      blameLines = [];
      coverageHits = new Map();
      coverageFailed = false;
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  /**
   * Reload the selected file after a failure.
   *
   * Retry used to mean re-typing the path into Blame's own box and pressing
   * Enter — the box was the only way back, and it existed only because Blame
   * was a destination you could arrive at with nothing selected. Code's
   * Explorer section owns picking a file now, so what is left here is the one
   * thing a selection cannot express: asking for the same file again. The
   * store does not notify for an unchanged value, so this calls the loader
   * directly.
   */
  function retryBlame() {
    const repo = $repoStore.currentPath;
    const path = $repoStore.selectedFilePath;
    if (!repo || !path) return;
    void loadBlameFor(repo, path);
  }

  $effect(() => {
    return () => inflight?.cancel();
  });

  // Selection- and freshness-driven blame load, memoized on a fingerprint of
  // its real dependencies. Status-poll emissions re-run this effect body but
  // skip the IPC unless something that can change blame output moved: the
  // selection, the file's worktree/index status, or the checked-out branch's
  // tip (external commits land through watcher refreshes).
  let prevKey: string | null = null;
  $effect(() => {
    const selected = $repoStore.selectedFilePath;
    const repo = $repoStore.currentPath;
    const statusCode = selected
      ? ($repoStore.statuses.find((s) => s.path === selected)?.status_code ?? "")
      : "";
    const tip =
      $repoStore.branches.find((b) => b.is_current)?.tip_commit_id ?? "";
    const key = `${repo ?? ""}\u0000${selected ?? ""}\u0000${statusCode}\u0000${tip}`;
    if (key === prevKey) return;
    prevKey = key;
    if (selected) {
      filePath = selected;
    }
    if (!repo || !selected) {
      if (!repo) {
        inflight?.cancel();
        blameLines = [];
        coverageHits = new Map();
        coverageFailed = false;
        errorMsg = null;
        isLoading = false;
      }
      return;
    }
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

  function handleWindowKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "b" && !e.shiftKey) {
      e.preventDefault();
      explorerOpen = !explorerOpen;
    }
  }
</script>

<svelte:window onkeydown={handleWindowKeydown} />

<div class="flex-1 flex flex-col bg-background h-full text-xs font-mono select-none overflow-hidden">
  <!-- Toolbar -->
  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between font-sans shrink-0">
    <div class="flex items-center gap-3 min-w-0">
      <button
        type="button"
        onclick={() => (explorerOpen = !explorerOpen)}
        title="{explorerOpen ? 'Hide' : 'Show'} Explorer (⌘B)"
        class="p-1 rounded-full text-textMuted hover:text-accent hover:bg-surfaceHover transition-colors"
      >
        {#if explorerOpen}
          <PanelLeftClose size={15} />
        {:else}
          <PanelLeftOpen size={15} />
        {/if}
      </button>
      <FileCode size={16} class="text-accent" />
      <!-- The file, named rather than typed. Blame reads the selection Code's
           Explorer section sets, so a second path box here would be a second
           way to say the same thing — and the one that could disagree. -->
      <span class="truncate font-mono text-xs text-textPrimary" title={filePath || undefined}>
        {filePath || "No file selected"}
      </span>
    </div>

    <!-- Legend -->
    <div class="flex items-center gap-3 text-[11px] text-textMuted shrink-0">
      {#if coverageFailed && blameLines.length > 0}
        <span
          class="flex items-center gap-1 text-amber-400/80"
          title="Coverage data could not be loaded; hit badges are unavailable."
        >
          <span class="w-2 h-2 rounded-full bg-amber-400/70"></span> Coverage unavailable
        </span>
      {/if}
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-red-500/40"></span> &lt; 7d</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-amber-500/40"></span> &lt; 30d</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-blue-500/40"></span> &lt; 90d</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full bg-gray-500/30"></span> Older</span>
    </div>
  </div>

  <!-- Body -->
  <div class="flex-1 min-h-0 flex">
    {#if explorerOpen}
      <div class="w-72 shrink-0 h-full overflow-hidden">
        <FileTreePanel />
      </div>
    {/if}

    <!-- Blame Lines -->
    <div class="flex-1 min-w-0 flex flex-col">
      {#if isLoading}
        <div class="h-full flex items-center justify-center text-textMuted font-sans text-xs">
          Loading blame for {filePath}...
        </div>
      {:else if errorMsg}
        <div class="h-full flex flex-col items-center justify-center gap-3 text-rose-400 font-sans text-xs p-4 text-center">
          <span class="max-w-md">{errorMsg}</span>
          <button type="button" onclick={retryBlame} class="gp-btn !py-1 !px-3 text-[11px]">
            Retry
          </button>
        </div>
      {:else if blameLines.length > 0}
        <VirtualList items={blameLines} rowHeight={24} overscan={15} class="h-full px-1.5 py-1">
          {#snippet row(line)}
            {#if line}
              <div
                class="flex items-center h-6 rounded-md px-1 hover:bg-surfaceHover/80 transition-colors"
                style="background-color: {getHeatmapColor(line.timestamp)}"
              >
                {#if ZERO_OID_RE.test(line.commit_id)}
                  <span
                    class="w-16 px-2 text-[10px] font-mono text-textMuted/50 italic text-left shrink-0"
                    title="Not committed yet"
                  >uncommitted</span>
                {:else}
                  <button
                    type="button"
                    class="w-16 px-2 text-[10px] text-accent/80 font-mono select-none cursor-pointer hover:underline text-left shrink-0"
                    onclick={() => {
                      repoStore.inspectCommitInHistory(line.commit_id);
                    }}
                  >{shortHash(line.commit_id)}</button>
                {/if}
                <span class="w-24 px-2 text-[10px] text-textMuted truncate font-sans shrink-0">{line.author_name}</span>
                <span class="w-8 px-2 text-right text-textMuted/40 text-[10px] select-none shrink-0">{line.line_no}</span>
                <span class={hitBadgeClass(coverageHits.get(line.line_no))}>{coverageHits.get(line.line_no) ?? "·"}</span>
                <span class="px-3 whitespace-pre overflow-hidden text-textPrimary {coverageHitClass(coverageHits.get(line.line_no))}">{line.content}</span>
              </div>
            {/if}
          {/snippet}
        </VirtualList>
      {:else}
        <EmptyState
          icon={Search}
          title="No blame loaded"
          hint={explorerOpen
            ? "Pick a file in the explorer to see line authorship and code age."
            : "Open the explorer (⌘B), or pick a file in Code → Explorer."}
        />
      {/if}
    </div>
  </div>
</div>
