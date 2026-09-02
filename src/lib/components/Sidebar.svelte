<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { displayName } from "../repos/paths";
  import BranchList from "./BranchList.svelte";
  import CommitComposer from "./CommitComposer.svelte";
  import WorktreesPanel from "./WorktreesPanel.svelte";
  import LanguageLogo from "./LanguageLogo.svelte";
  import { formatPathParts } from "../files/formatPath";
  import {
    layoutStore,
    loadSections,
    readSectionsRaw,
    saveSections,
    type SidebarSections,
  } from "../sidebar/layoutStore";
  import {
    SIDEBAR_COLLAPSED_WIDTH,
    SIDEBAR_DEFAULT_WIDTH,
    SIDEBAR_MAX_WIDTH,
    SIDEBAR_MIN_WIDTH,
    SIDEBAR_RESIZE_STEP,
  } from "../sidebar/metrics";
  import {
    AlertTriangle,
    Archive,
    ArrowDownToLine,
    ArrowUpFromLine,
    CheckCircle2,
    ChevronDown,
    ChevronRight,
    DownloadCloud,
    FolderGit2,
    FolderOpen,
    GitBranch,
    Minus,
    PanelLeftClose,
    PanelLeftOpen,
    Plus,
    Search,
  } from "lucide-svelte";

  // An agent can touch thousands of files in one pass; lists mount a window
  // and grow on demand so the sidebar never becomes the bottleneck.
  const FILE_LIST_STEP = 300;

  /* --- Persisted shell + section state ------------------------------------ */

  let sections = $state<SidebarSections>(loadSections(readSectionsRaw()));

  function toggleSection(key: keyof SidebarSections) {
    sections = { ...sections, [key]: !sections[key] };
    saveSections(sections);
  }

  /* --- File filter + windowed lists ---------------------------------------- */

  let fileFilter = $state("");
  let stagedLimit = $state(FILE_LIST_STEP);
  let unstagedLimit = $state(FILE_LIST_STEP);

  /* --- Resize drag ---------------------------------------------------------- */

  let dragging = $state(false);
  let dragStartX = 0;
  let dragStartWidth = 0;
  /** Set on pointerup after a real drag; dblclick inside the window is ignored. */
  let lastDragEndAt = 0;

  /* --- Derived repo pulse --------------------------------------------------- */

  let statuses = $derived($repoStore.statuses);
  let dirtyCount = $derived(statuses.length);
  let totalAdditions = $derived(statuses.reduce((n, s) => n + (s.additions || 0), 0));
  let totalDeletions = $derived(statuses.reduce((n, s) => n + (s.deletions || 0), 0));
  let conflictedCount = $derived(statuses.filter((s) => s.is_conflicted).length);

  // The pulse strip speaks only for the checked-out branch; a remote ref or a
  // missing match must never surface someone else's counts.
  let currentBranchInfo = $derived(
    $repoStore.branches.find((b) => b.is_current && !b.is_remote) ?? null,
  );
  let aheadCount = $derived(currentBranchInfo?.ahead_count ?? 0);
  let behindCount = $derived(currentBranchInfo?.behind_count ?? 0);

  let query = $derived(fileFilter.trim().toLowerCase());
  let isFiltering = $derived(query.length > 0);
  let stagedFiles = $derived(statuses.filter((s) => s.is_staged));
  let unstagedFiles = $derived(statuses.filter((s) => !s.is_staged));
  let filteredStaged = $derived(
    isFiltering ? stagedFiles.filter((f) => f.path.toLowerCase().includes(query)) : stagedFiles,
  );
  let filteredUnstaged = $derived(
    isFiltering
      ? unstagedFiles.filter((f) => f.path.toLowerCase().includes(query))
      : unstagedFiles,
  );
  let visibleStaged = $derived(filteredStaged.slice(0, stagedLimit));
  let visibleUnstaged = $derived(filteredUnstaged.slice(0, unstagedLimit));

  function stageAll() {
    void repoStore.stageAll();
  }
  function unstageAll() {
    void repoStore.unstageAll();
  }

  /**
   * Stepwise growth instead of the old jump-to-full-length: each click reveals
   * one more window; once the remainder is small enough that two more clicks
   * would finish anyway, offer the whole tail at once.
   */
  function showMoreLabel(total: number, shown: number): string {
    const remaining = total - shown;
    return remaining <= FILE_LIST_STEP * 2
      ? `Show all ${total.toLocaleString()}`
      : `Show ${remaining.toLocaleString()} more`;
  }

  function growLimit(limit: number, total: number): number {
    return total - limit <= FILE_LIST_STEP * 2 ? total : limit + FILE_LIST_STEP;
  }

  function pathClass(conflicted: boolean): string {
    return conflicted ? "text-amber-400 font-bold" : "text-textPrimary";
  }

  /* --- Drag-to-resize (Pointer Events + capture) ---------------------------- */

  function startResize(event: PointerEvent) {
    if ($layoutStore.collapsed) return;
    event.preventDefault();
    const target = event.currentTarget as HTMLElement;
    target.setPointerCapture(event.pointerId);
    dragging = true;
    dragStartX = event.clientX;
    dragStartWidth = $layoutStore.width;
  }

  function moveResize(event: PointerEvent) {
    if (!dragging) return;
    // Main pane keeps min-w-0 flex-1, so even narrow windows tolerate the
    // full MIN..MAX range; clampSidebarWidth remains the single authority.
    // Live updates skip per-tick localStorage writes (~60Hz of wasted I/O);
    // endResize persists once.
    const dx = event.clientX - dragStartX;
    layoutStore.setWidthLive(dragStartWidth + dx);
  }

  function endResize(event: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    lastDragEndAt = Date.now();
    // Persist the settled width exactly once per gesture. A dblclick lands
    // after this inside the suppression window (see ondblclick), so a
    // two-click fine-tune never snaps back to the default mid-gesture.
    layoutStore.setWidth($layoutStore.width);
    const target = event.currentTarget as HTMLElement;
    if (target.hasPointerCapture?.(event.pointerId)) {
      target.releasePointerCapture(event.pointerId);
    }
  }

  function resizeKeydown(event: KeyboardEvent) {
    switch (event.key) {
      case "ArrowLeft":
        event.preventDefault();
        layoutStore.setWidth($layoutStore.width - SIDEBAR_RESIZE_STEP);
        break;
      case "ArrowRight":
        event.preventDefault();
        layoutStore.setWidth($layoutStore.width + SIDEBAR_RESIZE_STEP);
        break;
      case "Home":
        event.preventDefault();
        layoutStore.setWidth(SIDEBAR_MIN_WIDTH);
        break;
      case "End":
        event.preventDefault();
        layoutStore.setWidth(SIDEBAR_MAX_WIDTH);
        break;
    }
  }
</script>

<aside
  class="relative bg-surface border-r border-border flex flex-col font-sans select-none text-xs shrink-0 h-full gp-pane {!dragging
    ? 'transition-[width] duration-150'
    : ''}"
  style="width:{$layoutStore.collapsed ? SIDEBAR_COLLAPSED_WIDTH : $layoutStore.width}px"
>
  {#if !$layoutStore.collapsed}
    <!-- Repo Header & Open Button -->
    <div class="p-3 flex items-center justify-between bg-surfaceHover/30 gap-1">
      <div class="flex items-center gap-2 truncate">
        <FolderGit2 size={15} class="text-accent shrink-0" />
        <span class="font-semibold text-textPrimary truncate" title={$repoStore.currentPath || "No Repo"}>
          {$repoStore.currentPath ? displayName($repoStore.currentPath) : "No Repository"}
        </span>
      </div>
      <div class="flex items-center shrink-0">
        <button
          type="button"
          onclick={() => layoutStore.toggleCollapsed()}
          title="Collapse sidebar"
          aria-label="Collapse sidebar"
          class="gp-icon-btn !p-1 hover:text-accent"
        >
          <PanelLeftClose size={14} />
        </button>
        <button
          type="button"
          onclick={() => repoStore.pickAndOpenRepo()}
          title="Open Repository"
          aria-label="Open Repository"
          class="gp-icon-btn !p-1 hover:text-accent"
        >
          <FolderOpen size={14} />
        </button>
      </div>
    </div>

    {#if $repoStore.currentPath}
      <!-- At-a-glance repo pulse strip -->
      <div class="mx-3 mt-2 rounded-xl border border-border/70 bg-surface p-2.5 space-y-1.5">
        <div class="flex items-center gap-1.5 min-w-0">
          <GitBranch size={13} class="text-accent shrink-0" />
          <span
            class="font-semibold truncate"
            title={$repoStore.currentBranch
              ? `Current branch: ${$repoStore.currentBranch}`
              : "No branch is checked out (detached HEAD)"}
          >
            {$repoStore.currentBranch ?? "detached"}
          </span>
          <div class="ml-auto flex items-center gap-1 shrink-0">
            {#if aheadCount > 0}
              <span
                class="text-[10px] font-mono font-bold px-1 py-0 rounded-full bg-emerald-500/15 text-emerald-400 border border-emerald-500/25"
                title={`${aheadCount} commit${aheadCount === 1 ? "" : "s"} ahead of upstream`}
              >
                ↑{aheadCount}
              </span>
            {/if}
            {#if behindCount > 0}
              <span
                class="text-[10px] font-mono font-bold px-1 py-0 rounded-full bg-amber-500/15 text-amber-400 border border-amber-500/25"
                title={`${behindCount} commit${behindCount === 1 ? "" : "s"} behind upstream`}
              >
                ↓{behindCount}
              </span>
            {/if}
            {#if currentBranchInfo?.is_default}
              <span
                class="text-[9px] font-semibold uppercase tracking-wide px-1.5 py-0 rounded-full border border-border/70 bg-surface text-textMuted"
                title="The current branch is this repository's default branch"
              >
                default
              </span>
            {/if}
          </div>
        </div>

        {#if dirtyCount === 0}
          <div class="flex items-center gap-1.5 text-textMuted/70" title="No uncommitted changes">
            <CheckCircle2 size={12} class="text-green-400/80 shrink-0" />
            <span>Working tree clean</span>
          </div>
        {:else}
          <div class="flex items-center gap-1.5 min-w-0">
            <span
              class="font-mono text-[11px] shrink-0"
              title="Total added / deleted lines across all changed files"
            >
              <span class="text-green-400">+{totalAdditions}</span>
              <span class="text-red-400">−{totalDeletions}</span>
            </span>
            <span class="truncate" title={`${dirtyCount} file${dirtyCount === 1 ? "" : "s"} with uncommitted changes`}>
              {dirtyCount} changed file{dirtyCount === 1 ? "" : "s"}
            </span>
            {#if conflictedCount > 0}
              <span
                class="ml-auto flex items-center gap-1 shrink-0 text-[10px] font-mono font-bold px-1 py-0 rounded-full bg-amber-500/15 text-amber-400 border border-amber-500/25"
                title={`${conflictedCount} file${conflictedCount === 1 ? "" : "s"} with merge conflicts`}
              >
                <AlertTriangle size={10} class="shrink-0" />
                {conflictedCount} conflict{conflictedCount === 1 ? "" : "s"}
              </span>
            {/if}
          </div>
        {/if}

        <!-- Quick actions -->
        <div class="flex gap-1 pt-1.5 border-t border-border/60">
          <button
            type="button"
            class="gp-icon-btn !p-1 hover:text-accent disabled:cursor-not-allowed"
            onclick={() => void repoStore.fetch()}
            disabled={$repoStore.isLoading}
            title="Fetch from remote"
            aria-label="Fetch from remote"
          >
            <DownloadCloud size={13} />
          </button>
          <button
            type="button"
            class="gp-icon-btn !p-1 hover:text-accent disabled:cursor-not-allowed"
            onclick={() => void repoStore.pull()}
            disabled={$repoStore.isLoading}
            title="Pull from upstream"
            aria-label="Pull from upstream"
          >
            <ArrowDownToLine size={13} />
          </button>
          <button
            type="button"
            class="gp-icon-btn !p-1 hover:text-accent disabled:cursor-not-allowed"
            onclick={() => void repoStore.push()}
            disabled={$repoStore.isLoading}
            title="Push to upstream"
            aria-label="Push to upstream"
          >
            <ArrowUpFromLine size={13} />
          </button>
          <button
            type="button"
            class="gp-icon-btn !p-1 hover:text-accent disabled:cursor-not-allowed"
            onclick={() => void repoStore.stashSave()}
            disabled={$repoStore.isLoading}
            title="Stash changes"
            aria-label="Stash changes"
          >
            <Archive size={13} />
          </button>
        </div>
      </div>
    {/if}

    <!-- Main Scrollable Section -->
    <div class="flex-1 overflow-y-auto p-3 space-y-5 mt-2">
      <BranchList />

      <WorktreesPanel />

      <!-- Shared file filter -->
      <div class="relative">
        <Search
          size={13}
          class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none"
        />
        <input
          bind:value={fileFilter}
          type="text"
          placeholder="Filter files…"
          aria-label="Filter files"
          title="Filter both change lists by path"
          class="gp-field w-full pl-8 pr-2 py-1.5"
        />
      </div>

      <!-- Staged Changes -->
      <div>
        <div class="flex items-center justify-between px-2 mb-1">
          <button
            type="button"
            onclick={() => toggleSection("staged")}
            aria-expanded={!sections.staged}
            title={sections.staged ? "Expand staged changes" : "Collapse staged changes"}
            class="flex items-center gap-1 text-[10px] font-bold text-textMuted uppercase tracking-wider hover:text-textPrimary transition-colors"
          >
            {#if sections.staged}
              <ChevronRight size={12} />
            {:else}
              <ChevronDown size={12} />
            {/if}
            <span>
              Staged Changes ({isFiltering
                ? `${filteredStaged.length} of ${stagedFiles.length}`
                : stagedFiles.length})
            </span>
          </button>
          {#if !sections.staged && stagedFiles.length > 0}
            <button
              type="button"
              onclick={unstageAll}
              title="Unstage all files"
              class="text-[9px] lowercase font-normal text-textMuted hover:text-red-400 transition-colors"
            >
              unstage all
            </button>
          {/if}
        </div>
        {#if !sections.staged}
          {#if filteredStaged.length === 0}
            <div class="text-[11px] text-textMuted/60 px-2 py-1.5 italic">
              {isFiltering ? `No matches for '${fileFilter}'` : "No staged changes"}
            </div>
          {:else}
            <div class="space-y-0.5">
              {#each visibleStaged as f (f.path)}
                {@const parts = formatPathParts(f.path)}
                <div class="px-2 py-1.5 rounded-full flex items-center gap-1.5 hover:bg-surfaceHover group transition-colors">
                  <LanguageLogo filePath={f.path} size={13} class="shrink-0" />
                  <button
                    type="button"
                    class="flex-1 min-w-0 truncate text-left font-mono text-[11px] {pathClass(f.is_conflicted)}"
                    onclick={() => repoStore.selectFileDiff(f.path, true)}
                    title={f.path}
                  >
                    {#if parts.dir}
                      <span class="text-textMuted/60 text-[10px]">{parts.dir}</span>
                    {/if}
                    <span class="text-textPrimary font-medium">{parts.name}</span>
                  </button>
                  <button
                    type="button"
                    onclick={(e) => {
                      e.stopPropagation();
                      void repoStore.unstageFile(f.path);
                    }}
                    title="Unstage file"
                    aria-label="Unstage file"
                    class="p-0.5 rounded-full opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:bg-background hover:text-red-400 shrink-0"
                  >
                    <Minus size={12} />
                  </button>
                </div>
              {/each}
              {#if filteredStaged.length > visibleStaged.length}
                <button
                  type="button"
                  class="w-full px-2 py-1 rounded-xl border border-dashed border-border/80 text-[10px] text-textMuted hover:text-textPrimary"
                  onclick={() => (stagedLimit = growLimit(stagedLimit, filteredStaged.length))}
                >
                  {showMoreLabel(filteredStaged.length, visibleStaged.length)}
                </button>
              {/if}
            </div>
          {/if}
        {/if}
      </div>

      <!-- Unstaged / Working Tree Changes -->
      <div>
        <div class="flex items-center justify-between px-2 mb-1">
          <button
            type="button"
            onclick={() => toggleSection("unstaged")}
            aria-expanded={!sections.unstaged}
            title={sections.unstaged ? "Expand working tree changes" : "Collapse working tree changes"}
            class="flex items-center gap-1 text-[10px] font-bold text-textMuted uppercase tracking-wider hover:text-textPrimary transition-colors"
          >
            {#if sections.unstaged}
              <ChevronRight size={12} />
            {:else}
              <ChevronDown size={12} />
            {/if}
            <span>
              Changes ({isFiltering
                ? `${filteredUnstaged.length} of ${unstagedFiles.length}`
                : unstagedFiles.length})
            </span>
          </button>
          {#if !sections.unstaged && unstagedFiles.length > 0}
            <button
              type="button"
              onclick={stageAll}
              title="Stage all files"
              class="text-[9px] lowercase font-normal text-textMuted hover:text-green-400 transition-colors"
            >
              stage all
            </button>
          {/if}
        </div>
        {#if !sections.unstaged}
          {#if filteredUnstaged.length === 0}
            <div class="text-[11px] text-textMuted/60 px-2 py-1.5 italic">
              {isFiltering ? `No matches for '${fileFilter}'` : "Working tree clean"}
            </div>
          {:else}
            <div class="space-y-0.5">
              {#each visibleUnstaged as f (f.path + "-" + f.status_code)}
                {@const parts = formatPathParts(f.path)}
                <div class="px-2 py-1.5 rounded-full flex items-center gap-1.5 hover:bg-surfaceHover group transition-colors">
                  <LanguageLogo filePath={f.path} size={13} class="shrink-0" />
                  <button
                    type="button"
                    class="flex-1 min-w-0 truncate text-left font-mono text-[11px] {pathClass(f.is_conflicted)}"
                    onclick={() => repoStore.selectFileDiff(f.path, false)}
                    title={f.path}
                  >
                    {#if parts.dir}
                      <span class="text-textMuted/60 text-[10px]">{parts.dir}</span>
                    {/if}
                    <span class="text-textPrimary font-medium">{parts.name}</span>
                  </button>
                  <button
                    type="button"
                    onclick={(e) => {
                      e.stopPropagation();
                      void repoStore.stageFile(f.path);
                    }}
                    title="Stage file"
                    aria-label="Stage file"
                    class="p-0.5 rounded-full opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:bg-background hover:text-green-400 shrink-0"
                  >
                    <Plus size={12} />
                  </button>
                </div>
              {/each}
              {#if filteredUnstaged.length > visibleUnstaged.length}
                <button
                  type="button"
                  class="w-full px-2 py-1 rounded-xl border border-dashed border-border/80 text-[10px] text-textMuted hover:text-textPrimary"
                  onclick={() => (unstagedLimit = growLimit(unstagedLimit, filteredUnstaged.length))}
                >
                  {showMoreLabel(filteredUnstaged.length, visibleUnstaged.length)}
                </button>
              {/if}
            </div>
          {/if}
        {/if}
      </div>
    </div>

    <CommitComposer />
  {:else}
    <!-- Collapsed rail -->
    <div class="flex flex-col items-center gap-3 pt-3 pb-2 flex-1 overflow-hidden">
      <button
        type="button"
        onclick={() => layoutStore.toggleCollapsed()}
        title="Expand sidebar"
        aria-label="Expand sidebar"
        class="gp-icon-btn hover:text-accent"
      >
        <PanelLeftOpen size={15} />
      </button>
      <FolderGit2 size={15} class="text-accent shrink-0" />
      {#if $repoStore.currentPath}
        <span
          class="text-[9px] font-mono font-bold px-1 py-0 rounded-full min-w-[18px] text-center {dirtyCount > 0
            ? 'bg-amber-500/15 text-amber-400 border border-amber-500/25'
            : 'border border-border/70 bg-surface text-textMuted'}"
          title={`${dirtyCount} changed file${dirtyCount === 1 ? "" : "s"}`}
        >
          {dirtyCount}
        </span>
      {/if}
    </div>
  {/if}

  {#if !$layoutStore.collapsed}
    <!-- Resize handle: drag, double-click resets, arrow keys step.
         WAI-ARIA adjustable separator: focusable + keyboard-operable by design,
         so the div-with-role warnings are intentional. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- Justified: this is the WAI-ARIA window-splitter pattern — a focusable
         `separator` with aria-valuenow/min/max, which the spec defines as
         interactive precisely when it is focusable. The rule treats every
         `separator` as non-interactive. -->
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize sidebar"
      tabindex="0"
      aria-valuemin={SIDEBAR_MIN_WIDTH}
      aria-valuemax={SIDEBAR_MAX_WIDTH}
      aria-valuenow={$layoutStore.width}
      class="absolute top-0 -right-[3px] z-10 h-full w-[6px] cursor-col-resize hover:bg-accent/40 transition-colors"
      title="Drag to resize · double-click to reset · ←/→ to nudge"
      onpointerdown={startResize}
      onpointermove={moveResize}
      onpointerup={endResize}
      onpointercancel={endResize}
      onkeydown={resizeKeydown}
      ondblclick={() => {
        if (Date.now() - lastDragEndAt > 250) layoutStore.setWidth(SIDEBAR_DEFAULT_WIDTH);
      }}
    >
    </div>
  {/if}
</aside>
