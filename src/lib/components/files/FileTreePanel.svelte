<script lang="ts">
  import { onMount } from "svelte";
  import { repoStore, type FileStatus } from "../../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    ChevronDown,
    ChevronRight,
    Folder,
    FolderOpen,
    Search,
    FilePlus,
    FolderPlus,
    Copy,
    ExternalLink,
    GitCommit,
    Layers,
    FileCode,
    MoreVertical,
    Check,
    Undo2,
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import { debounce } from "../../async/debounce";
  import { formatError } from "../../ui/formatError";
  import { highlightMatches } from "../../branches/groupBranches";
  import {
    ancestorsOf,
    buildFileTree,
    flattenFileTree,
    joinWorktreePath,
    type FileRow,
  } from "../../files/fileTree";
  import { filterPathsByFileQuery, parseFileQuery } from "../../files/fileQuery";
  import {
    classifyFileChange,
    dirtyAncestorSet,
    mergeListedAndStatusPaths,
    statusMatchesScope,
    statusPathKey,
    summarizeStatuses,
  } from "../../files/fileStatus";
  import { getFileIconMeta } from "../../files/fileIcons";
  import { copyText } from "../../desktop/clipboard";
  import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
  import { askConfirm, askText } from "../../stores/modalStore";
  import Skeleton from "../Skeleton.svelte";
  import { portal } from "../../dom/portal";
  import { LAYERS } from "../../ui/layers";
  import { shouldDismissOverlay } from "../../ui/dismiss";
  import VirtualList from "../VirtualList.svelte";
  import EmptyState from "../EmptyState.svelte";

  let {
    onSelectFile,
    onPinFile,
    selectedFile = null,
  }: {
    onSelectFile: (path: string) => void;
    onPinFile?: (path: string) => void;
    selectedFile?: string | null;
  } = $props();

  const ROW_HEIGHT = 24;
  const OVERSCAN = 12;

  type StatusFilter = "all" | "modified" | "staged" | "untracked" | "conflicted";
  type SortOrder = "name-asc" | "name-desc" | "status" | "churn";

  let files = $state<string[]>([]);
  let isLoading = $state(false);
  let errorMsg = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;

  let query = $state("");
  let debouncedQuery = $state("");
  let statusFilter = $state<StatusFilter>("all");
  let sortOrder = $state<SortOrder>("name-asc");
  let selectedExt = $state<string | null>(null);

  let collapsed = $state<Record<string, boolean>>({});
  let selectedIndex = $state<number>(-1);
  let locatePath = $state<string | null>(null);

  let contextMenuRow = $state<FileRow | null>(null);
  let menuPos = $state<{ x: number; y: number } | null>(null);

  let containerEl: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);

  const applyQuery = debounce((value: string) => {
    debouncedQuery = value;
    selectedIndex = -1;
  }, 120);

  let statusMap = $derived.by(() => {
    const map = new Map<string, FileStatus>();
    for (const status of $repoStore.statuses) {
      map.set(status.path, status);
    }
    return map;
  });

  let statusCounts = $derived.by(() => {
    const summary = summarizeStatuses($repoStore.statuses);
    return {
      modified: summary.unstaged,
      staged: summary.staged,
      untracked: summary.untracked,
      conflicted: summary.conflicted,
    };
  });

  let dirtyDirs = $derived.by(() => dirtyAncestorSet($repoStore.statuses.map((s) => s.path)));

  let parsedQuery = $derived.by(() => parseFileQuery(debouncedQuery));

  // Available extensions in current repo for quick filter chips
  let availableExts = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const file of files) {
      const dot = file.lastIndexOf(".");
      if (dot > 0 && dot < file.length - 1) {
        const ext = file.slice(dot).toLowerCase();
        counts.set(ext, (counts.get(ext) || 0) + 1);
      }
    }
    return Array.from(counts.entries())
      .filter(([_, count]) => count >= 2)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 8)
      .map(([ext]) => ext);
  });

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

  let lastRepo = "";
  $effect(() => {
    const repo = $repoStore.currentPath;
    const membership = statusPathKey($repoStore.statuses);
    void membership;
    if (!repo) {
      inflight?.cancel();
      files = [];
      errorMsg = null;
      isLoading = false;
      selectedIndex = -1;
      lastRepo = "";
      return;
    }
    if (repo !== lastRepo) {
      collapsed = {};
      lastRepo = repo;
    }
    void loadFiles(repo);
    const started = inflight;
    return () => {
      if (inflight === started) started?.cancel();
    };
  });

  let listedPaths = $derived.by(() =>
    mergeListedAndStatusPaths(
      files,
      $repoStore.statuses.map((s) => s.path),
    ),
  );

  let filteredPaths = $derived.by(() => {
    let result = filterPathsByFileQuery(listedPaths, parsedQuery);

    const pillScope =
      statusFilter === "conflicted"
        ? "conflict"
        : statusFilter === "modified"
          ? "modified"
          : statusFilter;
    const scope = parsedQuery.status !== "all" ? parsedQuery.status : pillScope;
    if (scope !== "all") {
      result = result.filter((p) => statusMatchesScope(statusMap.get(p), scope));
    }

    const ext = selectedExt ?? parsedQuery.ext;
    if (ext) {
      result = result.filter((p) => p.toLowerCase().endsWith(ext));
    }

    return result;
  });

  let isFiltering = $derived(
    debouncedQuery.trim().length > 0 || statusFilter !== "all" || selectedExt !== null
  );

  // Flattened rows
  let rows = $derived.by<FileRow[]>(() => {
    const flattened = flattenFileTree(buildFileTree(filteredPaths), (dirPath) =>
      isFiltering ? false : collapsed[dirPath] === true,
    );
    if (sortOrder === "name-asc") return flattened;
    const fileRows = flattened.filter((row) => row.kind === "file");
    if (sortOrder === "name-desc") {
      return [...fileRows].reverse();
    }
    if (sortOrder === "status") {
      return [...fileRows].sort((a, b) => {
        const sA = statusMap.get(a.path)?.status_code || "zz";
        const sB = statusMap.get(b.path)?.status_code || "zz";
        return sA.localeCompare(sB);
      });
    }
    if (sortOrder === "churn") {
      return [...fileRows].sort((a, b) => {
        const chA = (statusMap.get(a.path)?.additions || 0) + (statusMap.get(a.path)?.deletions || 0);
        const chB = (statusMap.get(b.path)?.additions || 0) + (statusMap.get(b.path)?.deletions || 0);
        return chB - chA;
      });
    }
    return flattened;
  });

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
    const next: Record<string, boolean> = {};
    for (const row of rows) {
      if (row.kind === "dir") next[row.path] = true;
    }
    collapsed = next;
  }

  function chooseFile(path: string) {
    onSelectFile(path);
  }

  function rowAction(row: FileRow) {
    if (row.kind === "dir") {
      toggle(row.path);
    } else {
      chooseFile(row.path);
    }
  }

  function pinFile(path: string) {
    (onPinFile ?? onSelectFile)(path);
  }

  function rowKindClass(kind: ReturnType<typeof classifyFileChange>): string {
    switch (kind) {
      case "conflict":
        return "bg-rose-500/20 text-rose-300 border border-rose-500/40";
      case "staged":
        return "bg-emerald-500/20 text-emerald-300 border border-emerald-500/40";
      case "untracked":
        return "bg-cyan-500/20 text-cyan-300 border border-cyan-500/40";
      case "unstaged":
        return "bg-amber-500/20 text-amber-300 border border-amber-500/40";
      default:
        return "";
    }
  }

  function rowKindLabel(kind: ReturnType<typeof classifyFileChange>): string {
    switch (kind) {
      case "conflict":
        return "!C";
      case "staged":
        return "S";
      case "untracked":
        return "U";
      case "unstaged":
        return "M";
      default:
        return "";
    }
  }

  function openContextMenu(row: FileRow, event: MouseEvent) {
    event.preventDefault();
    event.stopPropagation();
    contextMenuRow = row;
    menuPos = { x: event.clientX, y: event.clientY };
  }

  function closeContextMenu() {
    contextMenuRow = null;
    menuPos = null;
  }

  // File operations
  async function createNewFile(parentDir: string = "") {
    closeContextMenu();
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const name = await askText({
      title: "New File",
      message: parentDir ? `Create file in ${parentDir}/` : "Create file in repository root",
      placeholder: "example.ts",
      confirmLabel: "Create",
    });
    if (!name?.trim()) return;
    const fullRelative = parentDir ? `${parentDir}/${name.trim()}` : name.trim();
    try {
      await invoke("cmd_write_file_content", {
        repoPath: repo,
        filePath: fullRelative,
        content: "",
      });
      await loadFiles(repo);
      await repoStore.refresh();
      chooseFile(fullRelative);
    } catch (err: unknown) {
      repoStore.setError(formatError(err));
    }
  }

  async function createNewFolder(parentDir: string = "") {
    closeContextMenu();
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const folderName = await askText({
      title: "New Folder",
      message: parentDir ? `Create folder in ${parentDir}/` : "Create folder in repository root",
      placeholder: "components",
      confirmLabel: "Create Folder",
    });
    if (!folderName?.trim()) return;
    const dummyPath = parentDir
      ? `${parentDir}/${folderName.trim()}/.gitkeep`
      : `${folderName.trim()}/.gitkeep`;
    try {
      await invoke("cmd_write_file_content", {
        repoPath: repo,
        filePath: dummyPath,
        content: "",
      });
      await loadFiles(repo);
      await repoStore.refresh();
    } catch (err: unknown) {
      repoStore.setError(formatError(err));
    }
  }

  async function stageFile(path: string) {
    closeContextMenu();
    await repoStore.stageFile(path);
  }

  async function unstageFile(path: string) {
    closeContextMenu();
    await repoStore.unstageFile(path);
  }

  async function discardFile(path: string) {
    closeContextMenu();
    const confirmed = await askConfirm({
      title: "Discard Changes",
      message: `Are you sure you want to discard all uncommitted changes to ${path}? This cannot be undone.`,
      confirmLabel: "Discard Changes",
    });
    if (!confirmed) return;
    await repoStore.discardChanges(path);
  }

  async function openInSystem(filePath: string) {
    closeContextMenu();
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const fullPath = joinWorktreePath(repo, filePath);
    if (!fullPath) {
      repoStore.setError("Cannot open a path outside the repository");
      return;
    }
    try {
      await openPath(fullPath);
    } catch {
      try {
        await revealItemInDir(fullPath);
      } catch (err) {
        repoStore.setError(formatError(err));
      }
    }
  }

  async function revealInFinder(filePath: string) {
    closeContextMenu();
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const fullPath = joinWorktreePath(repo, filePath);
    if (!fullPath) {
      repoStore.setError("Cannot reveal a path outside the repository");
      return;
    }
    try {
      await revealItemInDir(fullPath);
    } catch (err) {
      repoStore.setError(formatError(err));
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (contextMenuRow) {
        closeContextMenu();
        return;
      }
      if (query) {
        e.preventDefault();
        query = "";
        applyQuery.cancel();
        debouncedQuery = "";
        return;
      }
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
    containerEl.scrollTo({ top: itemTop });
  }

  // Reveal active file in tree
  $effect(() => {
    const selected = selectedFile;
    if (!selected) return;
    locatePath = selected;
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

  $effect(() => {
    const target = locatePath;
    if (!target) return;
    const idx = rows.findIndex((row) => row.kind === "file" && row.path === target);
    if (idx < 0) {
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


  onMount(() => {
    const handlePointerDown = (event: PointerEvent) => {
      if (!contextMenuRow) return;
      if (!shouldDismissOverlay(event.target, "[data-file-tree-menu]")) return;
      closeContextMenu();
    };
    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("resize", closeContextMenu);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("resize", closeContextMenu);
    };
  });
</script>

<div class="flex flex-col h-full bg-surface/50 font-sans text-xs min-h-0 border-r border-border/70 select-none">
  <!-- Top Explorer Header -->
  <div class="flex items-center justify-between px-3 py-2 border-b border-border/60 shrink-0 bg-surface/80">
    <div class="flex items-center gap-1.5 min-w-0">
      <span class="text-[11px] font-bold uppercase tracking-wider text-textMuted">Explorer</span>
      <span class="text-[10px] text-textMuted/80 tabular-nums font-mono">({filteredPaths.length})</span>
    </div>
    <div class="flex items-center gap-1">
      <button
        type="button"
        onclick={() => createNewFile("")}
        title="New File"
        class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
      >
        <FilePlus size={13} />
      </button>
      <button
        type="button"
        onclick={() => createNewFolder("")}
        title="New Folder"
        class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
      >
        <FolderPlus size={13} />
      </button>
      <button
        type="button"
        onclick={expandAll}
        title="Expand all folders"
        class="px-1.5 py-0.5 text-[9px] font-mono rounded hover:bg-surfaceHover text-textMuted hover:text-textPrimary transition-colors"
      >+All</button>
      <button
        type="button"
        onclick={collapseAll}
        title="Collapse all folders"
        class="px-1.5 py-0.5 text-[9px] font-mono rounded hover:bg-surfaceHover text-textMuted hover:text-textPrimary transition-colors"
      >-All</button>
    </div>
  </div>

  <!-- Filter & Search Bar -->
  <div class="p-2 space-y-2 border-b border-border/60 shrink-0 bg-surface/30">
    <div class="flex items-center gap-1.5 bg-background/90 border border-border/80 rounded-full px-2.5 py-1 focus-within:border-accent/70 transition-colors">
      <Search size={12} class="text-textMuted shrink-0" />
      <input
        type="text"
        bind:value={query}
        oninput={(e) => applyQuery((e.target as HTMLInputElement).value)}
        onkeydown={handleKeydown}
        placeholder="Filter: name, *.ts, /regex/, ~fuzzy, is:staged"
        spellcheck="false"
        class="w-full bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-none"
      />
      {#if query}
        <button
          type="button"
          onclick={() => { query = ""; debouncedQuery = ""; }}
          class="text-textMuted hover:text-textPrimary text-[10px] px-1"
        >✕</button>
      {/if}
    </div>

      {#if parsedQuery.error}
        <p class="text-[10px] text-rose-400 px-1">{parsedQuery.error}</p>
      {/if}
    <div class="flex items-center gap-1 overflow-x-auto gp-header-scroll py-0.5">
      <button
        type="button"
        onclick={() => (statusFilter = "all")}
        class="px-2 py-0.5 text-[10px] rounded-full transition-all shrink-0 font-medium {statusFilter === 'all'
          ? 'bg-accent/20 text-accent font-semibold border border-accent/40'
          : 'text-textMuted hover:bg-surfaceHover border border-transparent'}"
      >All</button>

      {#if statusCounts.modified > 0}
        <button
          type="button"
          onclick={() => (statusFilter = statusFilter === 'modified' ? 'all' : 'modified')}
          class="px-2 py-0.5 text-[10px] rounded-full transition-all shrink-0 flex items-center gap-1 font-medium {statusFilter === 'modified'
            ? 'bg-amber-500/25 text-amber-300 font-semibold border border-amber-500/50'
            : 'text-amber-400/80 hover:bg-surfaceHover border border-transparent'}"
        >
          <span class="w-1.5 h-1.5 rounded-full bg-amber-400"></span>
          <span>Mod ({statusCounts.modified})</span>
        </button>
      {/if}

      {#if statusCounts.staged > 0}
        <button
          type="button"
          onclick={() => (statusFilter = statusFilter === 'staged' ? 'all' : 'staged')}
          class="px-2 py-0.5 text-[10px] rounded-full transition-all shrink-0 flex items-center gap-1 font-medium {statusFilter === 'staged'
            ? 'bg-emerald-500/25 text-emerald-300 font-semibold border border-emerald-500/50'
            : 'text-emerald-400/80 hover:bg-surfaceHover border border-transparent'}"
        >
          <span class="w-1.5 h-1.5 rounded-full bg-emerald-400"></span>
          <span>Staged ({statusCounts.staged})</span>
        </button>
      {/if}

      {#if statusCounts.untracked > 0}
        <button
          type="button"
          onclick={() => (statusFilter = statusFilter === 'untracked' ? 'all' : 'untracked')}
          class="px-2 py-0.5 text-[10px] rounded-full transition-all shrink-0 flex items-center gap-1 font-medium {statusFilter === 'untracked'
            ? 'bg-cyan-500/25 text-cyan-300 font-semibold border border-cyan-500/50'
            : 'text-cyan-400/80 hover:bg-surfaceHover border border-transparent'}"
        >
          <span class="w-1.5 h-1.5 rounded-full bg-cyan-400"></span>
          <span>Untracked ({statusCounts.untracked})</span>
        </button>
      {/if}

      {#if statusCounts.conflicted > 0}
        <button
          type="button"
          onclick={() => (statusFilter = statusFilter === 'conflicted' ? 'all' : 'conflicted')}
          class="px-2 py-0.5 text-[10px] rounded-full transition-all shrink-0 flex items-center gap-1 font-medium {statusFilter === 'conflicted'
            ? 'bg-rose-500/25 text-rose-300 font-semibold border border-rose-500/50'
            : 'text-rose-400 hover:bg-surfaceHover border border-transparent'}"
        >
          <span class="w-1.5 h-1.5 rounded-full bg-rose-500"></span>
          <span>Conflict ({statusCounts.conflicted})</span>
        </button>
      {/if}
    </div>

    <!-- Quick Extension Filters & Sort dropdown -->
    <div class="flex items-center justify-between gap-1 pt-0.5">
      {#if availableExts.length > 0}
        <div class="flex items-center gap-1 overflow-x-auto gp-header-scroll min-w-0">
          {#each availableExts as ext}
            <button
              type="button"
              onclick={() => (selectedExt = selectedExt === ext ? null : ext)}
              class="px-1.5 py-0.5 text-[9px] rounded font-mono uppercase transition-colors shrink-0 {selectedExt === ext
                ? 'bg-accent/20 text-accent font-bold border border-accent/40'
                : 'text-textMuted/80 hover:bg-surfaceHover'}"
            >{ext.replace('.', '')}</button>
          {/each}
        </div>
      {/if}
      <div class="shrink-0 flex items-center">
        <select
          bind:value={sortOrder}
          class="bg-background text-[10px] text-textMuted rounded border border-border/70 px-1 py-0.5 focus:outline-none cursor-pointer"
        >
          <option value="name-asc">Sort: A-Z</option>
          <option value="name-desc">Sort: Z-A</option>
          <option value="status">Sort: Git Status</option>
          <option value="churn">Sort: Churn</option>
        </select>
      </div>
    </div>
  </div>

  <!-- Main Tree Viewport -->
  <div
    class="flex-1 min-h-0 relative"
    role="tree"
    aria-label="Workspace Files"
    tabindex="0"
    onkeydown={handleKeydown}
  >
    {#if isLoading}
      <div class="p-3 space-y-1 overflow-hidden h-full">
        <Skeleton variant="tree-row" count={14} />
      </div>
    {:else if errorMsg}
      <div class="h-full flex items-center justify-center text-rose-400 text-xs p-4 text-center">
        {errorMsg}
      </div>
    {:else if rows.length === 0}
      <EmptyState
        icon={Search}
        title={isFiltering ? "No matching files" : "Workspace is empty"}
        hint={isFiltering ? "Try clearing or relaxing your search/status filter." : "No files detected in this repository."}
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
          {#snippet row(r)}
            {#if r}
              {@const isSelected = selectedFile === r.path}
              {@const status = r.kind === "file" ? statusMap.get(r.path) : null}
              {@const changeKind = classifyFileChange(status)}
              {@const iconMeta = r.kind === "file" ? getFileIconMeta(r.path) : null}
              {@const dirDirty = r.kind === "dir" && dirtyDirs.has(r.path)}
              <div
                role="treeitem"
                tabindex="-1"
                aria-selected={isSelected}
                onclick={() => rowAction(r)}
                ondblclick={() => { if (r.kind === "file") pinFile(r.path); }}
                onkeydown={(e) => { if (e.key === "Enter") rowAction(r); }}
                oncontextmenu={(e) => openContextMenu(r, e)}
                class="flex items-center justify-between w-full h-full px-2 text-left rounded-md transition-colors group cursor-pointer {isSelected
                  ? 'bg-accent/15 text-textPrimary font-medium border-l-2 border-accent'
                  : selectedIndex >= 0 && rows[selectedIndex]?.key === r.key
                  ? 'bg-surfaceHover text-textPrimary'
                  : 'hover:bg-surfaceHover/80 text-textPrimary/90'}"
                style="padding-left: {6 + r.depth * 14}px;"
                title="{r.path}{status ? ` (${status.status_code})` : ''}"
              >
                <!-- Left: Icon + Label -->
                <div class="flex items-center gap-1.5 min-w-0 flex-1 truncate pr-1">
                  {#if r.kind === "dir"}
                    {#if isCollapsed(r.path)}
                      <ChevronRight size={12} class="shrink-0 text-textMuted" />
                      <Folder size={13} class="shrink-0 text-amber-400/80" />
                    {:else}
                      <ChevronDown size={12} class="shrink-0 text-textMuted" />
                      <FolderOpen size={13} class="shrink-0 text-amber-400" />
                    {/if}
                    <span class="truncate text-textPrimary font-medium">{r.name}</span>
                    {#if dirDirty}
                      <span class="w-1.5 h-1.5 rounded-full bg-amber-400 shrink-0" title="Contains uncommitted changes"></span>
                    {/if}
                  {:else}
                    <span class="w-3 shrink-0"></span>
                    <span class="text-[9px] font-mono font-bold px-1 rounded bg-surface/90 border border-border/60 shrink-0 {iconMeta?.colorClass}">
                      {iconMeta?.badgeLabel}
                    </span>
                    <span class="truncate {isSelected ? 'text-accent font-semibold' : status ? 'text-textPrimary' : 'text-textPrimary/80'}">
                      {#each highlightMatches(r.name, debouncedQuery) as chunk, i (`${i}:${chunk.matched}:${chunk.text}`)}{#if chunk.matched}<mark class="bg-accent/30 text-textPrimary rounded-sm font-semibold">{chunk.text}</mark>{:else}{chunk.text}{/if}{/each}
                    </span>
                  {/if}
                </div>

                <!-- Right: Git Status Badge & Churn -->
                <div class="flex items-center gap-1 shrink-0">
                  {#if status && changeKind !== "clean"}
                    <span class="px-1 py-0.2 text-[9px] font-bold rounded {rowKindClass(changeKind)}">{rowKindLabel(changeKind)}</span>
                    {#if status.additions > 0 || status.deletions > 0}
                      <span class="text-[9px] font-mono text-emerald-400">+{status.additions}</span>
                      <span class="text-[9px] font-mono text-rose-400">-{status.deletions}</span>
                    {/if}
                  {/if}

                  <span
                    role="button"
                    tabindex="-1"
                    onclick={(e) => { e.stopPropagation(); openContextMenu(r, e); }}
                    onkeydown={(e) => { if (e.key === "Enter") { e.stopPropagation(); openContextMenu(r, e as any); } }}
                    class="opacity-0 group-hover:opacity-100 p-0.5 rounded hover:bg-surface text-textMuted hover:text-textPrimary transition-opacity"
                    title="Actions"
                  >
                    <MoreVertical size={11} />
                  </span>
                </div>
              </div>
            {/if}
          {/snippet}
        </VirtualList>
      </div>
    {/if}
  </div>
</div>

<!-- Context Menu Popover -->
{#if contextMenuRow && menuPos}
  {@const row = contextMenuRow}
  {@const status = row.kind === "file" ? statusMap.get(row.path) : null}
  <div
    use:portal={"body"}
    data-file-tree-menu
    role="menu"
    class="fixed min-w-52 gp-menu gp-pop text-xs text-textPrimary py-1"
    style="left: {menuPos.x}px; top: {menuPos.y}px; z-index: {LAYERS.MENU}"
  >
    <div class="px-2.5 py-1 text-[10px] text-textMuted font-mono border-b border-border/60 truncate max-w-xs font-semibold">
      {row.path}
    </div>

    {#if row.kind === "dir"}
      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => createNewFile(row.path)}>
        <FilePlus size={13} class="text-accent" />
        <span>New File in folder…</span>
      </button>
      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => createNewFolder(row.path)}>
        <FolderPlus size={13} class="text-accent" />
        <span>New Folder in folder…</span>
      </button>
    {:else}
      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { chooseFile(row.path); closeContextMenu(); }}>
        <FileCode size={13} class="text-accent" />
        <span>Open in Editor</span>
      </button>

      {#if status}
        {#if status.is_staged}
          <button type="button" role="menuitem" class="gp-menu-item text-amber-300" onclick={() => stageFile(row.path)}>
            <Undo2 size={13} />
            <span>Unstage Changes</span>
          </button>
        {:else}
          <button type="button" role="menuitem" class="gp-menu-item text-emerald-300" onclick={() => unstageFile(row.path)}>
            <Check size={13} />
            <span>Stage File</span>
          </button>
          <button type="button" role="menuitem" class="gp-menu-item text-rose-300" onclick={() => discardFile(row.path)}>
            <Undo2 size={13} />
            <span>Discard Changes…</span>
          </button>
        {/if}
      {/if}

      <div class="my-1 border-t border-border/60"></div>

      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { repoStore.selectFilePath(row.path); repoStore.setActiveTab('diff'); closeContextMenu(); }}>
        <Layers size={13} class="text-cyan-400" />
        <span>View in Diff Tab</span>
      </button>
      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { repoStore.selectFilePath(row.path); repoStore.setActiveTab('blame'); closeContextMenu(); }}>
        <GitCommit size={13} class="text-purple-400" />
        <span>View Git Blame</span>
      </button>
    {/if}

    <div class="my-1 border-t border-border/60"></div>

    <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { copyText(row.path); closeContextMenu(); }}>
      <Copy size={13} class="text-textMuted" />
      <span>Copy Relative Path</span>
    </button>

    {#if $repoStore.currentPath}
      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { copyText(`${$repoStore.currentPath}/${row.path}`); closeContextMenu(); }}>
        <Copy size={13} class="text-textMuted" />
        <span>Copy Full Path</span>
      </button>
    {/if}

    <button type="button" role="menuitem" class="gp-menu-item" onclick={() => openInSystem(row.path)}>
      <ExternalLink size={13} class="text-textMuted" />
      <span>Open with Default App</span>
    </button>
    <button type="button" role="menuitem" class="gp-menu-item" onclick={() => revealInFinder(row.path)}>
      <FolderOpen size={13} class="text-textMuted" />
      <span>Reveal in File Manager</span>
    </button>
  </div>
{/if}
