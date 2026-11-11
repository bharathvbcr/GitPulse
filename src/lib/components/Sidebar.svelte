<script lang="ts" module>
  import type { StatusDashboard } from "../files/fileStatus";

  export type WorkingTreeSegmentKind = "staged" | "unstaged" | "untracked" | "conflict";

  export interface WorkingTreeSegment {
    kind: WorkingTreeSegmentKind;
    count: number;
    /** Share of the filled track. Zero-length when the tree is clean. */
    percent: number;
  }

  /** Staged, unstaged, untracked, then conflicts — the counts summarizeStatuses already publishes. */
  export function workingTreeSegments(
    summary: Pick<StatusDashboard, "staged" | "unstaged" | "untracked" | "conflicted">,
  ): WorkingTreeSegment[] {
    const parts: { kind: WorkingTreeSegmentKind; count: number }[] = [
      { kind: "staged", count: summary.staged },
      { kind: "unstaged", count: summary.unstaged },
      { kind: "untracked", count: summary.untracked },
      { kind: "conflict", count: summary.conflicted },
    ];
    const total = parts.reduce((sum, part) => sum + part.count, 0);
    if (total <= 0) return [];
    return parts
      .filter((part) => part.count > 0)
      .map((part) => ({ ...part, percent: (part.count / total) * 100 }));
  }

  /** Same hues as statusBadgeClass, solid so a 6px track stays readable. */
  export function workingTreeSegmentClass(kind: WorkingTreeSegmentKind): string {
    switch (kind) {
      case "staged":
        return "bg-ok";
      case "unstaged":
        return "bg-warn";
      case "untracked":
        return "bg-info";
      case "conflict":
        return "bg-danger";
    }
  }

  export function workingTreeSegmentLabel(kind: WorkingTreeSegmentKind, count: number): string {
    const name = kind === "conflict" ? (count === 1 ? "conflict" : "conflicts") : kind;
    return `${count} ${name}`;
  }
</script>

<script lang="ts">
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { LAYERS } from "../ui/layers";
  import { displayName } from "../repos/paths";
  import { repoScope } from "../repos/repoScope.svelte";
  import BranchList from "./BranchList.svelte";
  import CommitComposer from "./CommitComposer.svelte";
  import AgentCommitMenu from "./AgentCommitMenu.svelte";
  import WorktreesPanel from "./WorktreesPanel.svelte";
  import TrustExtensionBanner from "./TrustExtensionBanner.svelte";
  import LanguageLogo from "./LanguageLogo.svelte";
  import { formatPathParts } from "../files/formatPath";
  import { hasUnstagedChanges, statusForSide, summarizeStatuses } from "../files/fileStatus";
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
    GitBranch,
    Minus,
    PanelLeftClose,
    PanelLeftOpen,
    Pin,
    PinOff,
    Plus,
    Search,
  } from "@lucide/svelte";
  import ScrollCue from "./ScrollCue.svelte";
  import { onDestroy } from "svelte";
  import { DURATION_MS, motionExitMs, motionMs } from "../motion/tokens";
  import { panelModes } from "../ui/panelMode";
  import { floatingPanelDismiss } from "../ui/panelDismiss";
  import { popover, type PopoverOptions } from "../ui/popover";
  import { springEasing } from "../ui/transitions";

  /* --- Mode, collapse and the slide ------------------------------------------ */

  let overlay = $derived($panelModes.sidebar === "overlay");
  let collapsed = $derived($layoutStore.collapsed);
  /** True for one `fast` window after a docked toggle: the one-shot width slide. */
  let settling = $state(false);
  let settleTimer: ReturnType<typeof setTimeout> | null = null;

  function toggleCollapsed() {
    if (!overlay) {
      // The class and the new width land in the same flush, so the width
      // transition runs once; it is dropped again before the next gesture.
      settling = true;
      if (settleTimer) clearTimeout(settleTimer);
      settleTimer = setTimeout(() => {
        settling = false;
        settleTimer = null;
      }, DURATION_MS.fast + 40);
    }
    layoutStore.toggleCollapsed();
  }

  function toggleMode() {
    panelModes.toggle("sidebar");
  }

  /** Escape closes a floating panel, unless something inside it (a menu) took the key. */
  function escapeCloses(event: KeyboardEvent) {
    if (event.key !== "Escape" || event.defaultPrevented || !overlay || collapsed) return;
    const target = event.target;
    if (!(target instanceof Element) || !target.closest("[data-sidebar-panel]")) return;
    layoutStore.toggleCollapsed();
  }

  /*
   * A pointerdown outside the floating panel closes it. Menus the panel opened
   * are portaled to <body>, so "inside" is a selector over the panel and every
   * floating layer (ui/panelDismiss.ts), judged by the popover owner.
   */
  const panelDismissOptions: PopoverOptions = {
    dismiss: floatingPanelDismiss("[data-sidebar-panel]"),
    onDismiss: () => {
      if (overlay && !collapsed) layoutStore.toggleCollapsed();
    },
  };

  const slideEase = springEasing("spatial");

  /** The floating panel rises out of the rail's edge: transform and opacity only. */
  function panelIn(_node: Element) {
    return {
      duration: motionMs("base"),
      easing: slideEase,
      css: (t: number, u: number) => `opacity: ${t}; transform: translate3d(${-u * 100}%, 0, 0)`,
    };
  }

  /** Leaves in an exit: opacity only, shorter than the entrance. */
  function panelOut(_node: Element) {
    return { duration: motionExitMs("base"), css: (t: number) => `opacity: ${t}` };
  }

  onDestroy(() => {
    if (settleTimer) clearTimeout(settleTimer);
  });

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
  let bodyScroller: HTMLDivElement | undefined = $state();

  /* --- Repository switch -----------------------------------------------------
   * The sidebar is mounted once per window, so a tab switch re-points the store
   * it reads and nothing else. The filter text, how far each list was grown and
   * how far the body was scrolled belong to the repository being left; the
   * persisted section flags and the width are the person's, and stay.
   */
  repoScope(
    () => $currentRepoPath,
    () => {
      fileFilter = "";
      stagedLimit = FILE_LIST_STEP;
      unstagedLimit = FILE_LIST_STEP;
      if (bodyScroller) bodyScroller.scrollTop = 0;
    },
  );

  /* --- Derived repo pulse --------------------------------------------------- */

  let statuses = $derived($repoStore.statuses);
  let statusSummary = $derived(summarizeStatuses(statuses));
  let dirtyCount = $derived(statusSummary.dirty);
  let totalAdditions = $derived(statusSummary.additions);
  let totalDeletions = $derived(statusSummary.deletions);
  let conflictedCount = $derived(statusSummary.conflicted);
  let changeSegments = $derived(workingTreeSegments(statusSummary));
  let changeBarLabel = $derived(
    changeSegments.length === 0
      ? "No uncommitted changes"
      : changeSegments.map((segment) => workingTreeSegmentLabel(segment.kind, segment.count)).join(", "),
  );

  // The pulse strip speaks only for the checked-out branch; a remote ref or a
  // missing match must never surface someone else's counts.
  let currentBranchInfo = $derived(
    $repoStore.branches.find((b) => b.is_current && !b.is_remote) ?? null,
  );
  let aheadCount = $derived(currentBranchInfo?.ahead_count ?? 0);
  let behindCount = $derived(currentBranchInfo?.behind_count ?? 0);

  let query = $derived(fileFilter.trim().toLowerCase());
  let isFiltering = $derived(query.length > 0);
  let stagedFiles = $derived(statuses.filter((s) => s.is_staged).map((s) => statusForSide(s, true)));
  let unstagedFiles = $derived(statuses.filter(hasUnstagedChanges).map((s) => statusForSide(s, false)));
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
    return conflicted ? "text-warn font-semibold" : "text-textPrimary";
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

{#snippet expanded()}
    <!-- Repository header. Opening another repository is the title bar's
         "Open" (decision D3, HEADER_SPEC §9): the sidebar no longer repeats it. -->
    <div class="p-3 flex items-center justify-between bg-surfaceHover/30 gap-1">
      <div class="flex items-center gap-2 truncate">
        <FolderGit2 size={15} class="text-accent shrink-0" />
        <span class="font-semibold text-textPrimary truncate" title={$repoStore.currentPath || "No repository"}>
          {$repoStore.currentPath ? displayName($repoStore.currentPath) : "No repository"}
        </span>
      </div>
      <div class="flex items-center shrink-0">
        <button
          type="button"
          onclick={toggleMode}
          title={overlay ? "Dock the sidebar beside the workspace" : "Float the sidebar over the workspace"}
          aria-label="Float sidebar over the workspace"
          aria-pressed={overlay}
          data-sidebar-mode-toggle
          class="gp-icon-btn p-1! hover:text-accent"
        >
          {#if overlay}<PinOff size={14} />{:else}<Pin size={14} />{/if}
        </button>
        <button
          type="button"
          onclick={toggleCollapsed}
          title="Collapse sidebar"
          aria-label="Collapse sidebar"
          class="gp-icon-btn p-1! hover:text-accent"
        >
          <PanelLeftClose size={14} />
        </button>
      </div>
    </div>

    {#if $repoStore.currentPath}
      <!-- At-a-glance repo pulse strip -->
      <div class="mx-3 mt-2 rounded-xl border border-edge bg-surface p-2.5 space-y-1.5">
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
                class="text-micro font-mono font-semibold px-1 py-0 rounded-full bg-ok/15 text-ok border border-ok/35"
                title={`${aheadCount} commit${aheadCount === 1 ? "" : "s"} ahead of upstream`}
              >
                ↑{aheadCount}
              </span>
            {/if}
            {#if behindCount > 0}
              <span
                class="text-micro font-mono font-semibold px-1 py-0 rounded-full bg-warn/15 text-warn border border-warn/35"
                title={`${behindCount} commit${behindCount === 1 ? "" : "s"} behind upstream`}
              >
                ↓{behindCount}
              </span>
            {/if}
            {#if currentBranchInfo?.is_default}
              <span
                class="text-micro font-semibold px-1.5 py-0 rounded-full border border-edge bg-surface text-textMuted"
                title="The current branch is this repository's default branch"
              >
                default
              </span>
            {/if}
          </div>
        </div>

        <div class="space-y-1">
          {#if dirtyCount === 0}
            <div class="flex items-center gap-1.5 text-faint" title="No uncommitted changes">
              <CheckCircle2 size={12} class="text-ok/80 shrink-0" />
              <span>Working tree clean</span>
            </div>
          {:else}
            <div class="flex items-center justify-between gap-1.5 min-w-0 w-full">
              <button type="button" onclick={() => void repoStore.previewUncommitted()} class="flex items-center gap-1.5 min-w-0 flex-1 text-left hover:text-accent" title="Preview uncommitted files and modifications">
                <span
                  class="font-mono text-meta shrink-0"
                  title="Total added / deleted lines across all changed files"
                >
                  <span class="text-ok">+{totalAdditions}</span>
                  <span class="text-danger">−{totalDeletions}</span>
                </span>
                <span class="truncate" title={`${dirtyCount} file${dirtyCount === 1 ? "" : "s"} with uncommitted changes`}>
                  {dirtyCount} changed file{dirtyCount === 1 ? "" : "s"}
                </span>
                {#if conflictedCount > 0}
                  <span
                    class="ml-auto flex items-center gap-1 shrink-0 text-micro font-mono font-semibold px-1 py-0 rounded-full bg-warn/15 text-warn border border-warn/35"
                    title={`${conflictedCount} file${conflictedCount === 1 ? "" : "s"} with merge conflicts`}
                  >
                    <AlertTriangle size={10} class="shrink-0" />
                    {conflictedCount} conflict{conflictedCount === 1 ? "" : "s"}
                  </span>
                {/if}
              </button>
              <AgentCommitMenu compact={true} align="right" />
            </div>
          {/if}
          <div
            class="flex h-1.5 w-full overflow-hidden rounded-full bg-textMuted/20"
            data-working-tree-bar
            role="img"
            aria-label={changeBarLabel}
            title={changeBarLabel}
          >
            {#each changeSegments as segment (segment.kind)}
              <span
                class="h-full {workingTreeSegmentClass(segment.kind)}"
                style:flex="{segment.count} 0 0px"
                data-segment={segment.kind}
                data-count={segment.count}
                title={workingTreeSegmentLabel(segment.kind, segment.count)}
              ></span>
            {/each}
          </div>
        </div>

        <!-- Quick actions -->
        <div class="@container/actions flex w-full min-w-0 gap-1 overflow-x-hidden pt-1.5 mt-2">
          <button
            type="button"
            class="gp-btn min-w-0 flex-1 justify-center gap-1 px-1.5! py-0.5! hover:text-accent disabled:cursor-not-allowed"
            onclick={() => void repoStore.fetch()}
            disabled={$repoStore.isLoading}
            title="Fetch from remote"
            aria-label="Fetch from remote"
          >
            <DownloadCloud size={13} class="shrink-0" />
            <span class="@max-[279px]/actions:hidden">Fetch</span>
          </button>
          <button
            type="button"
            class="gp-btn min-w-0 flex-1 justify-center gap-1 px-1.5! py-0.5! hover:text-accent disabled:cursor-not-allowed"
            onclick={() => void repoStore.pull()}
            disabled={$repoStore.isLoading}
            title="Pull from upstream"
            aria-label="Pull from upstream"
          >
            <ArrowDownToLine size={13} class="shrink-0" />
            <span class="@max-[279px]/actions:hidden">Pull</span>
          </button>
          <button
            type="button"
            class="gp-btn min-w-0 flex-1 justify-center gap-1 px-1.5! py-0.5! hover:text-accent disabled:cursor-not-allowed"
            onclick={() => void repoStore.push()}
            disabled={$repoStore.isLoading}
            title="Push to upstream"
            aria-label="Push to upstream"
          >
            <ArrowUpFromLine size={13} class="shrink-0" />
            <span class="@max-[279px]/actions:hidden">Push</span>
          </button>
          <button
            type="button"
            class="gp-btn min-w-0 flex-1 justify-center gap-1 px-1.5! py-0.5! hover:text-accent disabled:cursor-not-allowed"
            onclick={() => void repoStore.stashSave()}
            disabled={$repoStore.isLoading}
            title="Stash changes"
            aria-label="Stash changes"
          >
            <Archive size={13} class="shrink-0" />
            <span class="@max-[279px]/actions:hidden">Stash</span>
          </button>
        </div>
      </div>
    {/if}

    <!-- Main Scrollable Section -->
    <div class="relative flex-1 min-h-0 mt-2">
    <div bind:this={bodyScroller} class="h-full overflow-y-auto p-3 space-y-5">
      <!-- Above the branch list on purpose. This says why everything below it
           may be incomplete, and the branch list has no height cap in this
           scroller — under it, on a repository with many branches, the notice
           sat below the fold. It renders nothing when there is nothing to
           extend, so it costs no space on a repository that is already fine. -->
      <TrustExtensionBanner />

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
            class="flex items-center gap-1 gp-label hover:text-textPrimary transition-colors"
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
              class="text-micro lowercase font-normal text-textMuted hover:text-danger transition-colors"
            >
              unstage all
            </button>
          {/if}
        </div>
        {#if !sections.staged}
          {#if filteredStaged.length === 0}
            <div class="text-meta text-faint px-2 py-1.5 italic">
              {isFiltering ? `No matches for '${fileFilter}'` : "No staged changes"}
            </div>
          {:else}
            <div class="space-y-0.5">
              {#each visibleStaged as f, i (`${f.path}#${i}`)}
                {@const parts = formatPathParts(f.path)}
                <div class="px-2 py-1.5 rounded-full flex items-center gap-1.5 hover:bg-surfaceHover group transition-colors">
                  <LanguageLogo filePath={f.path} size={13} class="shrink-0" />
                  <button
                    type="button"
                    class="flex-1 min-w-0 truncate text-left font-mono text-meta {pathClass(f.is_conflicted)}"
                    onclick={() => repoStore.selectFileDiff(f.path, true)}
                    title={f.path}
                  >
                    {#if parts.dir}
                      <span class="text-faint text-micro">{parts.dir}</span>
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
                    class="p-0.5 rounded-full opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:bg-background hover:text-danger shrink-0"
                  >
                    <Minus size={12} />
                  </button>
                </div>
              {/each}
              {#if filteredStaged.length > visibleStaged.length}
                <button
                  type="button"
                  class="w-full px-2 py-1 rounded-xl border border-dashed border-edgeStrong text-micro text-textMuted hover:text-textPrimary"
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
            class="flex items-center gap-1 gp-label hover:text-textPrimary transition-colors"
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
              class="text-micro lowercase font-normal text-textMuted hover:text-ok transition-colors"
            >
              stage all
            </button>
          {/if}
        </div>
        {#if !sections.unstaged}
          {#if filteredUnstaged.length === 0}
            <div class="text-meta text-faint px-2 py-1.5 italic">
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
                    class="flex-1 min-w-0 truncate text-left font-mono text-meta {pathClass(f.is_conflicted)}"
                    onclick={() => repoStore.selectFileDiff(f.path, false)}
                    title={f.path}
                  >
                    {#if parts.dir}
                      <span class="text-faint text-micro">{parts.dir}</span>
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
                    class="p-0.5 rounded-full opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:bg-background hover:text-ok shrink-0"
                  >
                    <Plus size={12} />
                  </button>
                </div>
              {/each}
              {#if filteredUnstaged.length > visibleUnstaged.length}
                <button
                  type="button"
                  class="w-full px-2 py-1 rounded-xl border border-dashed border-edgeStrong text-micro text-textMuted hover:text-textPrimary"
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
    <ScrollCue target={bodyScroller} axis="y" />
    </div>

    <CommitComposer />
{/snippet}

{#snippet rail()}
    <!-- Collapsed rail: a 44 px column of icons. Everything the open sidebar
         does at a glance stays one click away. -->
    <div class="flex flex-col items-center gap-3 pt-3 pb-2 flex-1 overflow-hidden" data-sidebar-rail>
      <button
        type="button"
        onclick={toggleCollapsed}
        title="Expand sidebar"
        aria-label="Expand sidebar"
        class="gp-icon-btn hover:text-accent"
      >
        <PanelLeftOpen size={15} />
      </button>
      <FolderGit2 size={15} class="text-accent shrink-0" />
      {#if $repoStore.currentPath}
        <button type="button" onclick={() => void repoStore.previewUncommitted()}
          class="text-micro font-mono font-semibold px-1 py-0 rounded-full min-w-[18px] text-center {dirtyCount > 0
            ? 'bg-warn/15 text-warn border border-warn/35'
            : 'border border-edge bg-surface text-textMuted'}"
          title={`${dirtyCount} changed file${dirtyCount === 1 ? "" : "s"}`}
        >
          {dirtyCount}
        </button>
        {#if dirtyCount > 0}
          <AgentCommitMenu compact={true} align="left" />
        {/if}
        <button
          type="button"
          class="gp-icon-btn hover:text-accent"
          onclick={() => void repoStore.fetch()}
          disabled={$repoStore.isLoading}
          title="Fetch from remote"
          aria-label="Fetch from remote"
        >
          <DownloadCloud size={14} />
        </button>
        <button
          type="button"
          class="gp-icon-btn hover:text-accent"
          onclick={() => void repoStore.pull()}
          disabled={$repoStore.isLoading}
          title="Pull from upstream"
          aria-label="Pull from upstream"
        >
          <ArrowDownToLine size={14} />
        </button>
        <button
          type="button"
          class="gp-icon-btn hover:text-accent"
          onclick={() => void repoStore.push()}
          disabled={$repoStore.isLoading}
          title="Push to upstream"
          aria-label="Push to upstream"
        >
          <ArrowUpFromLine size={14} />
        </button>
      {/if}
    </div>
{/snippet}

{#snippet resizer()}
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
      style:z-index={LAYERS.STICKY} class="absolute top-0 right-[-3px] h-full w-[6px] cursor-col-resize hover:bg-accent/40 transition-colors"
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
{/snippet}

<!-- The window's one sidebar, in one of two modes (panelMode.ts).
     Docked: the aside owns a column, the workspace reflows, and the open or
     close animates once (`gp-slide-docked`, never while a drag writes the
     width). Overlay: a 44 px rail keeps the column and the open panel floats
     over the workspace, sliding on transform and opacity. -->
<svelte:window onkeydown={escapeCloses} />

{#if overlay}
  <aside
    class="gp-glass gp-sidebar relative bg-surface flex flex-col font-sans select-none text-xs shrink-0 h-full gp-pane"
    style="width:{SIDEBAR_COLLAPSED_WIDTH}px"
    data-sidebar-mode="overlay"
    data-sidebar-rail-slot
    inert={!collapsed}
  >
    {@render rail()}
  </aside>
  {#if !collapsed}
    <aside
      class="gp-glass gp-sidebar bg-surface flex flex-col font-sans select-none text-xs h-full gp-pane shadow-float"
      style="position:absolute; top:0; bottom:0; left:0; width:{$layoutStore.width}px; z-index:{LAYERS.STICKY}"
      data-sidebar-mode="overlay"
      data-sidebar-panel
      use:popover={panelDismissOptions}
      in:panelIn
      out:panelOut
    >
      {@render expanded()}
      {@render resizer()}
    </aside>
  {/if}
{:else}
  <aside
    class="gp-glass gp-sidebar relative bg-surface flex flex-col font-sans select-none text-xs shrink-0 h-full gp-pane {settling && !dragging
      ? 'gp-slide-docked'
      : ''}"
    style="width:{collapsed ? SIDEBAR_COLLAPSED_WIDTH : $layoutStore.width}px"
    data-sidebar-mode="docked"
  >
    {#if !collapsed}
      {@render expanded()}
      {@render resizer()}
    {:else}
      {@render rail()}
    {/if}
  </aside>
{/if}
