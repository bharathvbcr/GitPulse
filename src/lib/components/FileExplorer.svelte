<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    ChevronDown,
    ChevronRight,
    File as FileIcon,
    Folder,
    FolderOpen,
    Search,
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { debounce } from "../async/debounce";
  import { formatError } from "../ui/formatError";
  import { highlightMatches } from "../branches/groupBranches";
  import {
    ancestorsOf,
    buildFileTree,
    filterPathsByQuery,
    flattenFileTree,
    type FileRow,
  } from "../files/fileTree";
  import VirtualList from "./VirtualList.svelte";
  import EmptyState from "./EmptyState.svelte";

  const ROW_HEIGHT = 22;
  const OVERSCAN = 10;

  let files = $state<string[]>([]);
  let isLoading = $state(false);
  let errorMsg = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;

  let query = $state("");
  let debouncedQuery = $state("");
  let collapsed = $state<Record<string, boolean>>({});
  let selectedIndex = $state<number>(-1);
  // Set when the store's selection changes and rows must scroll to it; the
  // post-flush effect below resolves it once `rows` actually contains it.
  let locatePath = $state<string | null>(null);

  let containerEl: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewportHeight = $state(300);

  const applyQuery = debounce((value: string) => {
    debouncedQuery = value;
    selectedIndex = -1;
  }, 150);

  async function loadFiles(repo: string) {
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    errorMsg = null;
    try {
      const list = await invoke<string[]>("cmd_list_repo_files", { repoPath: repo });
      if (!guard.isLive()) return;
      files = list;
      collapsed = {};
      selectedIndex = -1;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      errorMsg = formatError(err);
      files = [];
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  $effect(() => {
    return () => inflight?.cancel();
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    if (!repo) {
      inflight?.cancel();
      files = [];
      errorMsg = null;
      isLoading = false;
      selectedIndex = -1;
      return;
    }
    void loadFiles(repo);
    const started = inflight;
    return () => {
      if (inflight === started) {
        started?.cancel();
      }
    };
  });

  let isFiltering = $derived(debouncedQuery.trim().length > 0);

  // While filtering, every match must be visible: collapse state would hide
  // exactly the rows the user asked for, so filters always render expanded.
  let rows = $derived.by<FileRow[]>(() =>
    flattenFileTree(buildFileTree(filterPathsByQuery(files, debouncedQuery)), (dirPath) =>
      isFiltering ? false : collapsed[dirPath] === true
    )
  );

  let fileCount = $derived(files.length);

  function isCollapsed(dirPath: string): boolean {
    return collapsed[dirPath] === true;
  }

  function toggle(dirPath: string) {
    collapsed = { ...collapsed, [dirPath]: !isCollapsed(dirPath) };
  }

  function expandAll() {
    collapsed = {};
  }

  function collapseAll() {
    // Only directories that actually exist get a key; stale entries from
    // previous listings cannot survive an explicit rebuild.
    const next: Record<string, boolean> = {};
    for (const row of rows) {
      if (row.kind === "dir") next[row.path] = true;
    }
    collapsed = next;
  }

  function chooseFile(path: string) {
    // Mirrored outward through the session like CoverageViewer does, so
    // Diff/Coverage/Blame all converge on one shared selection.
    repoStore.selectFilePath(path);
  }

  function rowAction(row: FileRow) {
    if (row.kind === "dir") {
      toggle(row.path);
    } else {
      chooseFile(row.path);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && query) {
      e.preventDefault();
      query = "";
      applyQuery.cancel();
      debouncedQuery = "";
      return;
    }
    if (e.target instanceof HTMLInputElement && e.key !== "ArrowDown" && e.key !== "ArrowUp") {
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selectedIndex = Math.min(rows.length - 1, selectedIndex + 1);
      ensureVisible(selectedIndex);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selectedIndex = Math.max(0, selectedIndex - 1);
      ensureVisible(selectedIndex);
    } else if (e.key === "Enter") {
      if (selectedIndex >= 0 && selectedIndex < rows.length) {
        e.preventDefault();
        rowAction(rows[selectedIndex]);
      }
    } else if (e.key === "ArrowRight") {
      if (selectedIndex >= 0 && selectedIndex < rows.length) {
        const row = rows[selectedIndex];
        if (row.kind === "dir" && !isCollapsed(row.path)) return;
        e.preventDefault();
        if (row.kind === "dir") toggle(row.path);
      }
    } else if (e.key === "ArrowLeft") {
      if (selectedIndex >= 0 && selectedIndex < rows.length) {
        const row = rows[selectedIndex];
        if (row.kind !== "dir" || isCollapsed(row.path)) return;
        e.preventDefault();
        toggle(row.path);
      }
    }
  }

  function ensureVisible(index: number) {
    if (!containerEl || index < 0) return;
    const itemTop = index * ROW_HEIGHT;
    const itemBottom = itemTop + ROW_HEIGHT;
    if (itemTop < scrollTop) {
      containerEl.scrollTo({ top: itemTop });
    } else if (itemBottom > scrollTop + viewportHeight) {
      containerEl.scrollTo({ top: itemBottom - viewportHeight });
    }
  }

  // Reveal-and-select the store's current file whenever it changes (including
  // on mount after a tab switch): expand its ancestor chain, then scroll to
  // its row once the derived pipeline has caught up.
  $effect(() => {
    const selected = $repoStore.selectedFilePath;
    if (!selected) return;
    locatePath = selected;
    // Conditional write: spreading-and-always-assigning would re-trigger
    // this effect forever (collapsed is read here as a dependency).
    let changed = false;
    const next = { ...collapsed };
    for (const dir of ancestorsOf(selected)) {
      if (next[dir] === true) {
        next[dir] = false;
        changed = true;
      }
    }
    if (changed) collapsed = next;
  });

  // Mirrors VirtualList's own viewport tracking so ensureVisible's math uses
  // the real box, not a stale guess; degenerate measurements collapse to 0.
  $effect(() => {
    const el = containerEl;
    if (!el) return;
    const observer = new ResizeObserver((entries) => {
      const measured = entries[0]?.contentRect.height;
      viewportHeight = Number.isFinite(measured) && measured > 0 ? measured : 0;
    });
    observer.observe(el);
    return () => observer.disconnect();
  });

  $effect(() => {
    const target = locatePath;
    if (!target) return;
    const idx = rows.findIndex((row) => row.kind === "file" && row.path === target);
    if (idx < 0) {
      // Filtered out or still loading: drop the request rather than pinning
      // selection to a stale index.
      locatePath = null;
      return;
    }
    locatePath = null;
    selectedIndex = idx;
    ensureVisible(idx);
  });

  $effect(() => {
    if (selectedIndex >= rows.length) {
      selectedIndex = rows.length > 0 ? rows.length - 1 : -1;
    }
  });
</script>

<div class="flex flex-col h-full bg-surface/40 font-sans text-xs min-h-0">
  <!-- Header -->
  <div class="flex items-center justify-between px-3 py-2 border-b border-border/60 shrink-0">
    <span class="text-[11px] uppercase tracking-wide text-textMuted font-medium">Files</span>
    <div class="flex items-center gap-1 text-textMuted">
      <span class="text-[10px] tabular-nums">{fileCount}</span>
      <button
        type="button"
        onclick={expandAll}
        title="Expand all folders"
        class="px-1 py-0.5 text-[9px] rounded hover:bg-surfaceHover hover:text-textPrimary transition-colors"
      >+All</button>
      <button
        type="button"
        onclick={collapseAll}
        title="Collapse all folders"
        class="px-1 py-0.5 text-[9px] rounded hover:bg-surfaceHover hover:text-textPrimary transition-colors"
      >-All</button>
    </div>
  </div>

  <!-- Filter box -->
  <div class="px-3 py-2 border-b border-border/60 shrink-0">
    <div class="flex items-center gap-1.5 bg-background border border-border/80 rounded-full px-2.5 py-1 focus-within:border-accent/60 transition-colors">
      <Search size={12} class="text-textMuted shrink-0" />
      <input
        type="text"
        bind:value={query}
        oninput={(e) => applyQuery((e.target as HTMLInputElement).value)}
        onkeydown={handleKeydown}
        placeholder="Filter files..."
        spellcheck="false"
        class="w-full bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-none"
      />
    </div>
  </div>

  <!-- Tree -->
  <!-- role="tree": the rows form a collapsible hierarchy navigated with
       arrow keys (expand/collapse on Left/Right); without a role the
       keydown handling is invisible to assistive tech. tabindex="0" makes
       that keyboard interface reachable on its own, not only via the filter
       input. -->
  <div class="flex-1 min-h-0" role="tree" aria-label="Repository files" tabindex="0" onkeydown={handleKeydown}>
    {#if isLoading}
      <div class="h-full flex items-center justify-center text-textMuted text-xs">Loading files...</div>
    {:else if errorMsg}
      <div class="h-full flex items-center justify-center text-rose-400 text-xs p-3">{errorMsg}</div>
    {:else if rows.length === 0}
      <EmptyState
        icon={Search}
        title={isFiltering ? "No matches" : "No files"}
        hint={isFiltering ? "Adjust or clear the filter." : "This repository has no tracked or untracked files."}
      />
    {:else}
      <div bind:this={containerEl} class="h-full">
        <VirtualList
          items={rows}
          rowHeight={ROW_HEIGHT}
          overscan={OVERSCAN}
          bind:scrollTop
          class="h-full"
        >
          {#snippet row(row)}
            {#if row}
              <button
                type="button"
                onclick={() => rowAction(row)}
                class="flex items-center w-full h-full px-2 text-left rounded-md transition-colors {selectedIndex >= 0 && rows[selectedIndex]?.key === row.key
                  ? 'bg-accent/15 text-textPrimary'
                  : 'hover:bg-surfaceHover'}"
                style="padding-left: {8 + row.depth * 12}px;"
                title={row.path}
              >
                {#if row.kind === "dir"}
                  {#if isCollapsed(row.path)}
                    <ChevronRight size={12} class="shrink-0 text-textMuted" />
                    <Folder size={13} class="ml-0.5 shrink-0 text-textMuted" />
                  {:else}
                    <ChevronDown size={12} class="shrink-0 text-textMuted" />
                    <FolderOpen size={13} class="ml-0.5 shrink-0 text-textMuted" />
                  {/if}
                  <span class="ml-1 truncate text-textPrimary">{row.name}</span>
                {:else}
                  <FileIcon size={13} class="shrink-0 ml-[13px] text-textMuted/80" />
                  <span class="ml-1 truncate {$repoStore.selectedFilePath === row.path ? 'text-accent' : 'text-textPrimary/90'}">
                    {#each highlightMatches(row.name, debouncedQuery) as chunk, i (`${i}:${chunk.matched}:${chunk.text}`)}{#if chunk.matched}<mark class="bg-accent/30 text-textPrimary rounded-sm">{chunk.text}</mark>{:else}{chunk.text}{/if}{/each}
                  </span>
                {/if}
              </button>
            {/if}
          {/snippet}
        </VirtualList>
      </div>
    {/if}
  </div>
</div>
