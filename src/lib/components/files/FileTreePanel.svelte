<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { densityStore } from "../../stores/densityStore";
  import { rowHeight } from "../../ui/density";
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
    FoldVertical,
    SlidersHorizontal,
    X,
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
    parentDirectoryRowIndex,
    type FileRow,
  } from "../../files/fileTree";
  import { filterPathsByFileQuery, parseFileQuery } from "../../files/fileQuery";
  import {
    classifyFileChange,
    dirtyAncestorCounts,
    mergeListedAndStatusPaths,
    statusBadgeClass,
    statusBadgeLabel,
    statusMatchesScope,
    statusPathKey,
    summarizeStatuses,
  } from "../../files/fileStatus";
  import { copyText } from "../../desktop/clipboard";
  import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
  import { askConfirm, askText } from "../../stores/modalStore";
  import Skeleton from "../Skeleton.svelte";
  import { portal } from "../../dom/portal";
  import { LAYERS } from "../../ui/layers";
  import { shouldDismissOverlay } from "../../ui/dismiss";
  import { clampMenuPosition } from "../../branches/menuPosition";
  import { enumerateFocusables } from "../../ui/focusTrap";
  import VirtualList from "../VirtualList.svelte";
  import EmptyState from "../EmptyState.svelte";
  import LanguageLogo from "../LanguageLogo.svelte";

  let {
    onSelectFile,
    onPinFile,
    selectedFile = null,
    revealRequest = null,
  }: {
    onSelectFile?: (path: string) => void;
    onPinFile?: (path: string) => void;
    selectedFile?: string | null;
    /**
     * A directory the surrounding view wants brought into view — the file
     * breadcrumb's folder crumbs raise these. Carries a nonce because asking
     * twice for the same folder is a real second request, and comparing paths
     * alone would swallow it.
     */
    revealRequest?: { path: string; nonce: number } | null;
  } = $props();

  let ROW_HEIGHT = $derived(rowHeight("fileTree", $densityStore));
  const OVERSCAN = 12;
  /**
   * Row height tracks the app-wide density switch instead of a private
   * constant. The setting already governed the graph; the explorer was simply
   * not listening, so "compact" left the densest list in the window alone.
   */
  /** Left gutter before depth 0, and the width of one nesting level. */
  const INDENT_BASE = 6;
  const INDENT_STEP = 14;

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
  let filtersPinned = $state(false);
  let queryFocused = $state(false);

  let collapsed = $state<Record<string, boolean>>({});
  let selectedIndex = $state<number>(-1);
  let locatePath = $state<string | null>(null);

  let contextMenuRow = $state<FileRow | null>(null);
  let menuPos = $state<{ x: number; y: number } | null>(null);
  let menuEl: HTMLDivElement | undefined = $state();
  let contextMenuOpener: HTMLElement | null = null;

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

  let dirtyDirs = $derived.by(() => dirtyAncestorCounts($repoStore.statuses.map((s) => s.path)));

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

  /**
   * The query grammar, stated once. It used to live in the input's
   * placeholder, where a 288px rail truncated it to "Filter: name, *.ts, /re…"
   * — a syntax reference nobody could finish reading. As a caption under the
   * field it wraps into the space it has, and it steps aside as soon as there
   * is a query to look at.
   */
  const QUERY_SYNTAX = "name · *.ts glob · /regex/ · ~fuzzy · is:staged";

  /**
   * `mark` is the same letter the row badges use, so the chip that filters to
   * modified files and the badge on a modified row say the same thing. The
   * full word lives in the accessible name and the tooltip — spelling it out
   * in the rail is what overflowed the old pills off the right edge.
   */
  const STATUS_SCOPES = [
    { id: "modified", label: "Modified", mark: statusBadgeLabel("unstaged"), tint: "text-amber-300", on: "bg-amber-500/25 text-amber-300 border-amber-500/50" },
    { id: "staged", label: "Staged", mark: statusBadgeLabel("staged"), tint: "text-emerald-300", on: "bg-emerald-500/25 text-emerald-300 border-emerald-500/50" },
    { id: "untracked", label: "Untracked", mark: statusBadgeLabel("untracked"), tint: "text-cyan-300", on: "bg-cyan-500/25 text-cyan-300 border-cyan-500/50" },
    { id: "conflicted", label: "Conflicted", mark: statusBadgeLabel("conflict"), tint: "text-rose-300", on: "bg-rose-500/25 text-rose-300 border-rose-500/50" },
  ] as const satisfies readonly { id: Exclude<StatusFilter, "all">; label: string; mark: string; tint: string; on: string }[];

  let anyChanges = $derived(
    statusCounts.modified + statusCounts.staged + statusCounts.untracked + statusCounts.conflicted >
      0,
  );

  // A filter the shelf is hiding would narrow the listing with nothing on
  // screen to explain it, so an applied filter forces the shelf open.
  let filtersOpen = $derived(filtersPinned || selectedExt !== null || sortOrder !== "name-asc");


  // Flattened rows
  let rows = $derived.by<FileRow[]>(() => {
    const flattened = flattenFileTree(buildFileTree(filteredPaths), (dirPath) =>
      isFiltering ? false : collapsed[dirPath] === true,
    );
    if (sortOrder === "name-asc") return flattened;
    // Non-name sorts are flat file lists. Reset hierarchy metadata along with
    // removing directory rows so ARIA level and ArrowLeft remain truthful.
    const fileRows = flattened
      .filter((row) => row.kind === "file")
      .map((row) => ({ ...row, depth: 0 }));
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

  /** Drives the header's one fold control, which flips to Expand when true. */
  let allCollapsed = $derived.by(() => {
    const dirs = rows.filter((row) => row.kind === "dir");
    return dirs.length > 0 && dirs.every((row) => collapsed[row.path] === true);
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
    if (typeof onSelectFile === "function") {
      onSelectFile(path);
    } else {
      repoStore.selectFilePath(path);
    }
  }

  function rowAction(row: FileRow) {
    selectedIndex = rows.findIndex((candidate) => candidate.key === row.key);
    if (row.kind === "dir") {
      toggle(row.path);
    } else {
      chooseFile(row.path);
    }
  }

  function pinFile(path: string) {
    if (typeof onPinFile === "function") {
      onPinFile(path);
    } else {
      chooseFile(path);
    }
  }



  function treeItemId(row: FileRow): string {
    return `file-tree-item-${encodeURIComponent(row.key).replaceAll("%", "_")}`;
  }

  function openContextMenu(
    row: FileRow,
    event: MouseEvent | KeyboardEvent,
    keyboardAnchor?: HTMLElement | null,
  ) {
    event.preventDefault();
    event.stopPropagation();
    selectedIndex = rows.findIndex((candidate) => candidate.key === row.key);
    const eventAnchor = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
    const anchor = keyboardAnchor ?? eventAnchor;
    const rect = anchor?.getBoundingClientRect();
    const x = event instanceof MouseEvent && event.clientX > 0
      ? event.clientX
      : (rect?.left ?? 8) + Math.min(rect?.width ?? 0, 32);
    const y = event instanceof MouseEvent && event.clientY > 0
      ? event.clientY
      : (rect?.bottom ?? 8);
    const clamped = clampMenuPosition(x, y, 208, 360, window.innerWidth, window.innerHeight);
    const active = document.activeElement;
    contextMenuOpener = active instanceof HTMLElement && active !== document.body
      ? active
      : anchor;
    contextMenuRow = row;
    menuPos = { x: clamped.left, y: clamped.top };
  }

  function closeContextMenu(options?: { restoreFocus?: boolean }) {
    const opener = contextMenuOpener;
    contextMenuRow = null;
    menuPos = null;
    contextMenuOpener = null;
    if (options?.restoreFocus && opener?.isConnected) {
      window.setTimeout(() => opener.focus(), 0);
    }
  }

  $effect(() => {
    if (!contextMenuRow || !menuEl) return;
    const timer = window.setTimeout(() => {
      menuEl?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    }, 0);
    return () => window.clearTimeout(timer);
  });

  function focusAdjacentToMenuOpener(
    popup: HTMLElement,
    opener: HTMLElement | null,
    backwards: boolean,
  ) {
    const candidates = enumerateFocusables<HTMLElement>(document).filter(
      (candidate) =>
        candidate.tabIndex >= 0 &&
        !popup.contains(candidate) &&
        candidate.getClientRects().length > 0,
    );
    if (candidates.length === 0) return;

    const openerIndex = opener ? candidates.indexOf(opener) : -1;
    let target = openerIndex >= 0
      ? candidates[openerIndex + (backwards ? -1 : 1)]
      : undefined;
    if (!target && opener?.isConnected) {
      const direction = backwards
        ? Node.DOCUMENT_POSITION_PRECEDING
        : Node.DOCUMENT_POSITION_FOLLOWING;
      const ordered = backwards ? [...candidates].reverse() : candidates;
      target = ordered.find((candidate) => opener.compareDocumentPosition(candidate) & direction);
    }
    target ??= candidates[backwards ? candidates.length - 1 : 0];
    window.setTimeout(() => target?.focus(), 0);
  }

  function handleMenuKeydown(e: KeyboardEvent) {
    if (e.key === "Tab") {
      e.preventDefault();
      if (menuEl) focusAdjacentToMenuOpener(menuEl, contextMenuOpener, e.shiftKey);
      closeContextMenu();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      closeContextMenu({ restoreFocus: true });
      return;
    }
    const items = menuEl
      ? [...menuEl.querySelectorAll<HTMLElement>('[role="menuitem"]')]
      : [];
    if (items.length === 0) return;
    const current = items.findIndex((item) => item === document.activeElement);
    let next: number | null = null;
    if (e.key === "ArrowDown") next = (current + 1 + items.length) % items.length;
    else if (e.key === "ArrowUp") next = (current - 1 + items.length) % items.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = items.length - 1;
    if (next === null) return;
    e.preventDefault();
    items[next]?.focus();
  }

  function moveTreeSelection(index: number) {
    if (rows.length === 0) {
      selectedIndex = -1;
      return;
    }
    selectedIndex = Math.max(0, Math.min(index, rows.length - 1));
    ensureVisible(selectedIndex);
  }

  function parentRowIndex(index: number): number {
    return parentDirectoryRowIndex(rows, index);
  }

  async function copyPath(path: string) {
    closeContextMenu();
    if (!(await copyText(path))) {
      repoStore.setError("Could not copy path to clipboard");
    }
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
    if (
      !(e.target instanceof HTMLInputElement) &&
      (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) &&
      selectedIndex >= 0 &&
      selectedIndex < rows.length
    ) {
      const row = rows[selectedIndex];
      openContextMenu(row, e, document.getElementById(treeItemId(row)));
      return;
    }
    if (e.key === "Escape") {
      if (contextMenuRow) {
        e.preventDefault();
        closeContextMenu({ restoreFocus: true });
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
      moveTreeSelection(selectedIndex + 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveTreeSelection(selectedIndex - 1);
    } else if (e.key === "Home") {
      e.preventDefault();
      moveTreeSelection(0);
    } else if (e.key === "End") {
      e.preventDefault();
      moveTreeSelection(rows.length - 1);
    } else if (e.key === "Enter") {
      if (selectedIndex >= 0 && selectedIndex < rows.length) {
        e.preventDefault();
        rowAction(rows[selectedIndex]);
      }
    } else if (e.key === "ArrowRight") {
      if (selectedIndex >= 0 && selectedIndex < rows.length) {
        const row = rows[selectedIndex];
        e.preventDefault();
        if (row.kind !== "dir") return;
        if (isCollapsed(row.path)) {
          toggle(row.path);
          return;
        }
        const childIndex = selectedIndex + 1;
        if (rows[childIndex]?.depth === row.depth + 1) moveTreeSelection(childIndex);
      }
    } else if (e.key === "ArrowLeft") {
      if (selectedIndex >= 0 && selectedIndex < rows.length) {
        const row = rows[selectedIndex];
        e.preventDefault();
        if (row.kind === "dir" && !isCollapsed(row.path)) {
          toggle(row.path);
          return;
        }
        const parentIndex = parentRowIndex(selectedIndex);
        if (parentIndex >= 0) moveTreeSelection(parentIndex);
      }
    }
  }

  function ensureVisible(index: number) {
    if (index < 0) return;
    // VirtualList owns the actual scroll container, while this same-height
    // wrapper gives us its viewport. Update the binding minimally so routine
    // arrow navigation does not pin every selected row to the top.
    const itemTop = index * ROW_HEIGHT;
    const itemBottom = itemTop + ROW_HEIGHT;
    const viewportHeight = containerEl?.clientHeight ?? 0;
    if (viewportHeight <= 0 || itemTop < scrollTop) {
      scrollTop = itemTop;
    } else if (itemBottom > scrollTop + viewportHeight) {
      scrollTop = itemBottom - viewportHeight;
    }
  }

  let effectiveSelected = $derived(selectedFile ?? $repoStore.selectedFilePath);

  /**
   * Reveal the active file in the tree.
   *
   * The body reads and writes `collapsed`, so tracking it made this effect
   * re-run on every expand and collapse — and each re-run re-armed
   * `locatePath`, which scrolled the tree back to the open file. Opening a
   * folder anywhere in a large repository therefore threw away the position
   * the user had just navigated to. A new selection is the only thing that
   * should reveal anything, so the collapse bookkeeping is untracked.
   */
  $effect(() => {
    const selected = effectiveSelected;
    if (!selected) return;
    untrack(() => {
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
  });

  $effect(() => {
    const target = locatePath;
    if (!target) return;
    const idx = rows.findIndex((row) => row.path === target);
    if (idx < 0) {
      locatePath = null;
      return;
    }
    locatePath = null;
    selectedIndex = idx;
    ensureVisible(idx);
  });

  /**
   * Reveal a directory: open the path down to it, open the folder itself so
   * its contents are what the user lands on, then scroll it into view through
   * the same `locatePath` handshake a file selection uses.
   */
  let lastRevealNonce = 0;
  $effect(() => {
    const request = revealRequest;
    if (!request?.path || request.nonce === lastRevealNonce) return;
    lastRevealNonce = request.nonce;
    untrack(() => {
      const next = { ...collapsed };
      for (const dir of [...ancestorsOf(request.path), request.path]) next[dir] = false;
      collapsed = next;
      locatePath = request.path;
    });
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
    const handleResize = () => closeContextMenu();
    window.addEventListener("resize", handleResize);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("resize", handleResize);
    };
  });
</script>

<div class="flex flex-col h-full bg-surface/50 font-sans text-xs min-h-0 border-r border-border/70 select-none">
  <!--
    Explorer chrome.

    This used to be four stacked rows — title, search, status pills, then
    extension chips beside a native `<select>` — roughly 118px of header above
    the first file in a 288px rail, with the last pill and the last chip both
    clipped off the right edge behind a scrollbar-less overflow. It is now two
    rows plus a shelf that opens on request. The shelf cannot hide an active
    filter: `filtersOpen` is the toggle OR'd with "a filter is actually
    applied", so a narrowed listing always shows what narrowed it.
  -->
  <div class="flex items-center justify-between gap-1 px-2.5 h-9 shrink-0 border-b border-border/60 bg-surface/80">
    <div class="flex items-baseline gap-1.5 min-w-0">
      <span class="text-[11px] font-bold uppercase tracking-wider text-textMuted">Explorer</span>
      <span class="text-[10px] text-textMuted/80 tabular-nums font-mono" title="{filteredPaths.length} of {listedPaths.length} files shown">
        {filteredPaths.length}{#if isFiltering}<span class="text-textMuted/60">/{listedPaths.length}</span>{/if}
      </span>
    </div>
    <div class="flex items-center gap-0.5 shrink-0">
      <button
        type="button"
        onclick={() => createNewFile("")}
        title="New file in repository root"
        aria-label="New file in repository root"
        class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
      >
        <FilePlus size={13} />
      </button>
      <button
        type="button"
        onclick={() => createNewFolder("")}
        title="New folder in repository root"
        aria-label="New folder in repository root"
        class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
      >
        <FolderPlus size={13} />
      </button>
      <button
        type="button"
        onclick={allCollapsed ? expandAll : collapseAll}
        title={allCollapsed ? "Expand all folders" : "Collapse all folders"}
        aria-label={allCollapsed ? "Expand all folders" : "Collapse all folders"}
        class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
      >
        <FoldVertical size={13} />
      </button>
      <button
        type="button"
        onclick={() => (filtersPinned = !filtersOpen)}
        aria-expanded={filtersOpen}
        aria-controls="file-tree-filters"
        title="Sort and file-type filters"
        aria-label="Sort and file-type filters"
        class="gp-icon-btn !p-1 {filtersOpen ? 'text-accent bg-accent/15' : 'text-textMuted hover:text-textPrimary'}"
      >
        <SlidersHorizontal size={13} />
      </button>
    </div>
  </div>

  <div class="px-2 pt-2 pb-1.5 shrink-0 border-b border-border/60 bg-surface/30 space-y-1.5">
    <div class="flex items-center gap-1.5 bg-background/90 border border-border/80 rounded-full pl-2.5 pr-1.5 py-1 focus-within:border-accent/70 transition-colors">
      <Search size={12} class="text-textMuted shrink-0" />
      <input
        type="text"
        bind:value={query}
        oninput={(e) => applyQuery((e.target as HTMLInputElement).value)}
        onkeydown={handleKeydown}
        onfocus={() => (queryFocused = true)}
        onblur={() => (queryFocused = false)}
        placeholder="Filter files"
        title={QUERY_SYNTAX}
        spellcheck="false"
        class="w-full bg-transparent text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-none"
      />
      {#if query}
        <button
          type="button"
          onclick={() => { query = ""; applyQuery.cancel(); debouncedQuery = ""; }}
          aria-label="Clear filter"
          title="Clear filter"
          class="gp-icon-btn !p-0.5 text-textMuted hover:text-textPrimary"
        >
          <X size={11} />
        </button>
      {/if}
    </div>

    <!--
      The grammar appears when someone is about to use it and costs nothing at
      rest. As a permanent caption it spent ~21px of a 288px rail on a line
      most users read once; as a placeholder before that, it was truncated
      mid-token and could not be read even once.
    -->
    {#if parsedQuery.error}
      <p class="text-[10px] text-rose-400 px-1">{parsedQuery.error}</p>
    {:else if queryFocused}
      <p class="text-[10px] text-textMuted/70 px-1.5 leading-snug">{QUERY_SYNTAX}</p>
    {/if}

    <!--
      Status scopes. Wordless in the rail: a coloured dot plus its count fits
      four scopes inside 288px, which the old "Untracked (2)" labels did not —
      they overflowed, and the overflow had no scrollbar to advertise itself.
      The name still reaches keyboard and screen-reader users through the
      accessible label, and the mouse through the tooltip.
    -->
    {#if anyChanges}
      <div class="flex items-center gap-1" role="group" aria-label="Filter by working-tree status">
        <button
          type="button"
          onclick={() => (statusFilter = "all")}
          aria-pressed={statusFilter === "all"}
          title="Show all files"
          class="px-2 py-0.5 text-[10px] rounded-full border transition-colors shrink-0 font-medium {statusFilter === 'all'
            ? 'bg-accent/20 text-accent font-semibold border-accent/40'
            : 'text-textMuted hover:bg-surfaceHover border-transparent'}"
        >All</button>
        {#each STATUS_SCOPES as scope (scope.id)}
          {@const count = statusCounts[scope.id]}
          {#if count > 0}
            <button
              type="button"
              onclick={() => (statusFilter = statusFilter === scope.id ? "all" : scope.id)}
              aria-pressed={statusFilter === scope.id}
              aria-label="{scope.label} ({count})"
              title="{scope.label} ({count})"
              class="flex-1 min-w-0 px-1.5 py-0.5 rounded-full border transition-colors flex items-center justify-center gap-1 font-mono text-[10px] {statusFilter === scope.id
                ? `${scope.on} font-bold`
                : `border-transparent ${scope.tint} hover:bg-surfaceHover`}"
            >
              <span class="font-bold" aria-hidden="true">{scope.mark}</span>
              <span class="tabular-nums">{count}</span>
            </button>
          {/if}
        {/each}
      </div>
    {/if}

    {#if filtersOpen}
      <div id="file-tree-filters" class="space-y-1.5 pt-0.5">
        <label class="flex items-center gap-1.5">
          <span class="text-[10px] text-textMuted shrink-0">Sort</span>
          <select bind:value={sortOrder} class="gp-select flex-1 text-[10px]">
            <option value="name-asc">Name, A to Z</option>
            <option value="name-desc">Name, Z to A</option>
            <option value="status">Git status</option>
            <option value="churn">Lines changed</option>
          </select>
        </label>

        {#if availableExts.length > 0}
          <div class="flex flex-wrap items-center gap-1" role="group" aria-label="Filter by file type">
            {#each availableExts as ext (ext)}
              <button
                type="button"
                onclick={() => (selectedExt = selectedExt === ext ? null : ext)}
                aria-pressed={selectedExt === ext}
                class="px-1.5 py-0.5 text-[9px] rounded font-mono uppercase transition-colors {selectedExt === ext
                  ? 'bg-accent/20 text-accent font-bold border border-accent/40'
                  : 'text-textMuted/80 hover:bg-surfaceHover border border-transparent'}"
              >{ext.replace('.', '')}</button>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  </div>

  <!-- Main Tree Viewport -->
  <div
    class="flex-1 min-h-0 relative"
    role="tree"
    aria-label="Workspace Files"
    aria-activedescendant={selectedIndex >= 0 && selectedIndex < rows.length
      ? treeItemId(rows[selectedIndex])
      : undefined}
    tabindex="0"
    onfocus={() => {
      if (selectedIndex < 0 && rows.length > 0) selectedIndex = 0;
    }}
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
          {#snippet row(r, index)}
            {#if r}
              {@const isOpen = effectiveSelected === r.path}
              {@const isCursor = rows[selectedIndex]?.key === r.key}
              {@const status = r.kind === "file" ? statusMap.get(r.path) : null}
              {@const changeKind = classifyFileChange(status)}
              {@const dirtyCount = r.kind === "dir" ? (dirtyDirs.get(r.path) ?? 0) : 0}
              <div
                id={treeItemId(r)}
                role="treeitem"
                tabindex="-1"
                aria-selected={isOpen}
                aria-level={r.depth + 1}
                aria-setsize={rows.length}
                aria-posinset={index + 1}
                aria-expanded={r.kind === "dir" ? !isCollapsed(r.path) : undefined}
                onclick={() => rowAction(r)}
                ondblclick={() => { if (r.kind === "file") pinFile(r.path); }}
                onkeydown={(e) => { if (e.key === "Enter") rowAction(r); }}
                oncontextmenu={(e) => openContextMenu(r, e)}
                style="height: {ROW_HEIGHT}px;"
                class="relative flex items-center gap-1 w-full pr-1.5 text-left cursor-pointer group transition-colors {isOpen
                  ? 'bg-accent/15 text-textPrimary'
                  : isCursor
                    ? 'bg-surfaceHover text-textPrimary'
                    : 'hover:bg-surfaceHover/70 text-textPrimary/90'} {isCursor && !isOpen
                  ? 'ring-1 ring-inset ring-accent/40'
                  : ''}"
                title={r.path}
              >
                <!--
                  The height is stated, not inherited. VirtualList places row
                  n at n * rowHeight and renders the snippet with no sizing
                  wrapper, so a row whose content happens to measure something
                  else drifts out of its slot — which is exactly what the
                  previous `h-full` did the moment the density switch moved
                  ROW_HEIGHT off the 24px the old padding happened to produce.

                  Depth is drawn, not padded. Indent guides make a fifth-level
                  file traceable to its folder; the old rail only shifted the
                  row right and left the reader counting pixels. The bar and
                  the guides are both `inset` so neither adds to the box —
                  selection used to come with `border-l-2`, which widened the
                  row and nudged every glyph in it 2px sideways.
                -->
                {#if isOpen}
                  <span class="absolute inset-y-0 left-0 w-[2px] bg-accent" aria-hidden="true"></span>
                {/if}
                {#each { length: r.depth } as _, level (level)}
                  <span
                    class="absolute inset-y-0 w-px bg-border/45 pointer-events-none"
                    style="left: {INDENT_BASE + level * INDENT_STEP}px;"
                    aria-hidden="true"
                  ></span>
                {/each}

                <div
                  class="flex items-center gap-1.5 min-w-0 flex-1"
                  style="padding-left: {INDENT_BASE + r.depth * INDENT_STEP}px;"
                >
                  {#if r.kind === "dir"}
                    {#if isCollapsed(r.path)}
                      <ChevronRight size={12} class="shrink-0 text-textMuted" />
                      <Folder size={13} class="shrink-0 text-amber-400/80" />
                    {:else}
                      <ChevronDown size={12} class="shrink-0 text-textMuted" />
                      <FolderOpen size={13} class="shrink-0 text-amber-400" />
                    {/if}
                    <span class="truncate text-textPrimary font-medium">{r.name}</span>
                    {#if dirtyCount > 0}
                      <!-- A collapsed folder says how much it is hiding, not
                           merely that it hides something. -->
                      <span
                        class="shrink-0 px-1 rounded-full text-[9px] font-mono tabular-nums bg-amber-500/15 text-amber-300/90"
                        title="{dirtyCount} changed {dirtyCount === 1 ? 'file' : 'files'} inside"
                      >{dirtyCount}</span>
                    {/if}
                  {:else}
                    <span class="w-3 shrink-0" aria-hidden="true"></span>
                    <LanguageLogo filePath={r.path} size={14} class="shrink-0" />
                    <span class="truncate {isOpen ? 'text-accent font-semibold' : status ? 'text-textPrimary' : 'text-textPrimary/80'}">
                      {#each highlightMatches(r.name, debouncedQuery) as chunk, i (`${i}:${chunk.matched}:${chunk.text}`)}{#if chunk.matched}<mark class="bg-accent/30 text-textPrimary rounded-sm font-semibold">{chunk.text}</mark>{:else}{chunk.text}{/if}{/each}
                    </span>
                  {/if}
                </div>

                <!-- Right: Git Status Badge & Churn -->
                <div class="flex items-center gap-1 shrink-0">
                  {#if status && changeKind !== "clean"}
                    <span class="px-1 py-0.2 text-[9px] font-bold rounded {statusBadgeClass(changeKind)}">{statusBadgeLabel(changeKind)}</span>
                    {#if status.additions > 0 || status.deletions > 0}
                      {@const churnWarnings = status.warnings ?? []}
                      <!-- A count the backend flagged as possibly understated
                           must not render identically to one it verified: the
                           tilde and amber say "at least this much". -->
                      <span
                        class="text-[9px] font-mono {churnWarnings.length > 0
                          ? 'text-amber-400'
                          : 'text-emerald-400'}"
                        title={churnWarnings.length > 0
                          ? `Line counts may understate this file — ${churnWarnings.join("; ")}`
                          : undefined}>{churnWarnings.length > 0 ? "~" : ""}+{status.additions}</span>
                      <span
                        class="text-[9px] font-mono {churnWarnings.length > 0
                          ? 'text-amber-400'
                          : 'text-rose-400'}">-{status.deletions}</span>
                    {/if}
                  {/if}

                  <button
                    type="button"
                    tabindex="-1"
                    onclick={(e) => { e.stopPropagation(); openContextMenu(r, e); }}
                    aria-label={`Actions for ${r.path}`}
                    class="opacity-0 group-hover:opacity-100 focus-visible:opacity-100 p-0.5 rounded hover:bg-surface text-textMuted hover:text-textPrimary transition-opacity"
                    title="Actions"
                  >
                    <MoreVertical size={11} />
                  </button>
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
    bind:this={menuEl}
    use:portal={"body"}
    data-file-tree-menu
    role="menu"
    aria-label={`File actions for ${row.path}`}
    tabindex="-1"
    onkeydown={handleMenuKeydown}
    class="fixed min-w-52 max-h-[calc(100vh-1rem)] overflow-y-auto gp-menu gp-pop text-xs text-textPrimary py-1"
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
      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { chooseFile(row.path); repoStore.setActiveTab('code', 'explorer'); closeContextMenu(); }}>
        <FileCode size={13} class="text-accent" />
        <span>Open in Editor</span>
      </button>

      {#if status}
        {#if status.is_staged}
          <button type="button" role="menuitem" class="gp-menu-item text-amber-300" onclick={() => unstageFile(row.path)}>
            <Undo2 size={13} />
            <span>Unstage Changes</span>
          </button>
        {:else}
          <button type="button" role="menuitem" class="gp-menu-item text-emerald-300" onclick={() => stageFile(row.path)}>
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

      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { repoStore.selectFilePath(row.path); repoStore.setActiveTab('history', 'diff'); closeContextMenu(); }}>
        <Layers size={13} class="text-cyan-400" />
        <span>View in Diff Tab</span>
      </button>
      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { repoStore.selectFilePath(row.path); repoStore.setActiveTab('code', 'blame'); closeContextMenu(); }}>
        <GitCommit size={13} class="text-purple-400" />
        <span>View Git Blame</span>
      </button>
    {/if}

    <div class="my-1 border-t border-border/60"></div>

    <button type="button" role="menuitem" class="gp-menu-item" onclick={() => void copyPath(row.path)}>
      <Copy size={13} class="text-textMuted" />
      <span>Copy Relative Path</span>
    </button>

    {#if $repoStore.currentPath}
      <button type="button" role="menuitem" class="gp-menu-item" onclick={() => void copyPath(`${$repoStore.currentPath}/${row.path}`)}>
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
