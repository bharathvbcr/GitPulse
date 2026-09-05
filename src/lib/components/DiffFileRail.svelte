<script lang="ts">
  /**
   * The file list that stays beside the diff.
   *
   * Presentational on purpose: it takes a built rail and reports which entry
   * was clicked. The Diff view owns which store call that turns into, because
   * a commit file and a working-tree file open through different commands and
   * only the view knows which selection is live.
   *
   * What changed, and why: the old rail printed the basename of every entry
   * into a fixed 224px column with no filter and no grouping. On this
   * repository's own head commit that is `marketplace.json` twice,
   * `plugin.json` three times and `mod.rs` eight times — two hundred rows
   * that cannot be told apart, every one of them a tab stop in the DOM. Rows
   * are labelled by `railRows` now (shortest unique path suffix, or the
   * shared explorer's tree), filtered, and windowed.
   */
  import {
    ChevronDown,
    ChevronRight,
    FileCode,
    Filter,
    GitCommitHorizontal,
    ListTree,
    PanelLeftClose,
    Pencil,
    Rows3,
    X,
  } from "lucide-svelte";
  import { churnLabel, truncationNote, type FileRail, type RailEntry } from "../diff/fileRail";
  import {
    activeRowIndex,
    buildRailRows,
    filterNote,
    type RailMode,
    type RailRow,
  } from "../diff/railRows";
  import {
    commitLabel,
    isCurrentCommit,
    pickerNote,
    type CommitEntry,
    type CommitRail,
  } from "../diff/commitRail";
  import { formatRelativeTime, shortHash } from "../format";
  import VirtualList from "./VirtualList.svelte";

  let {
    rail,
    commits,
    currentPath,
    currentIsStaged,
    selectedCommitId,
    workingTreeCount,
    onOpen,
    onPickCommit,
    onPickWorkingTree,
    onCollapse,
    width = 248,
    onResize,
    commitsOpen = $bindable(false),
  }: {
    rail: FileRail;
    /** Recent commits, so moving BETWEEN changes needs no trip to Graph. */
    commits: CommitRail;
    currentPath: string | null;
    currentIsStaged: boolean;
    selectedCommitId: string | null;
    /** Uncommitted files; -1 when the count is not known. */
    workingTreeCount: number;
    onOpen: (entry: RailEntry) => void;
    onPickCommit: (entry: CommitEntry) => void;
    onPickWorkingTree: () => void;
    onCollapse: () => void;
    width?: number;
    onResize?: (next: number) => void;
    /**
     * Whether the change picker is unfolded.
     *
     * Owned by the caller so the choice survives a file switch, and so the
     * open state is reachable without a click — a folded section is invisible
     * to anything that renders the component once.
     */
    commitsOpen?: boolean;
  } = $props();

  /** Row height for the windowed file list; must match the row's own height. */
  const ROW_HEIGHT = 22;
  /**
   * Below this the list fits on screen and windowing only costs a wrapper.
   * Above it, every row is a DOM node and a tab stop.
   */
  const VIRTUALIZE_ABOVE = 60;

  let query = $state("");
  let mode = $state<RailMode>("list");
  let collapsed = $state<Set<string>>(new Set());
  // The picker starts folded (see `commitsOpen` above). It used to be a
  // permanently open 208px scroll box stacked above a second scroll box,
  // inside a 224px column — two nested scrollers competing for one wheel
  // event. It answers "which change", asked once per visit; the file list
  // answers "which file", asked constantly.
  let listScroll = $state(0);

  const note = $derived(truncationNote(rail));
  const commitNote = $derived(pickerNote(commits));
  /** True while the diff on screen is uncommitted work rather than a commit. */
  const onWorkingTree = $derived(selectedCommitId === null);

  const result = $derived(
    buildRailRows({
      entries: rail.entries,
      mode,
      query,
      isCollapsed: (dir) => collapsed.has(dir),
    }),
  );
  const rows = $derived(result.rows);
  const activeIndex = $derived(
    activeRowIndex(rows, currentPath, currentIsStaged, rail.source === "worktree"),
  );
  const searchNote = $derived(filterNote(result, query));
  const virtualize = $derived(rows.length > VIRTUALIZE_ABOVE);

  /**
   * Keeps the open file in view when the selection moves without a click —
   * the Alt+Arrow stepper, or a jump from another pane. Only follows the
   * selection, never the reader's own scrolling.
   */
  let lastFollowed = $state(-1);
  $effect(() => {
    const index = activeIndex;
    if (index < 0 || index === lastFollowed) return;
    lastFollowed = index;
    if (!virtualize) return;
    listScroll = Math.max(0, index * ROW_HEIGHT - ROW_HEIGHT * 4);
  });

  function toggleDir(path: string): void {
    const next = new Set(collapsed);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    collapsed = next;
  }

  /**
   * Status letters get a colour so the list scans without reading every row.
   * Unknown letters fall through to muted rather than to a colour that would
   * assert a meaning this build does not know.
   */
  function statusTone(code: string): string {
    switch (code.charAt(0)) {
      case "A":
        return "text-emerald-600 dark:text-emerald-400";
      case "D":
        return "text-rose-600 dark:text-rose-400";
      case "R":
      case "C":
        return "text-sky-600 dark:text-sky-400";
      case "M":
        return "text-amber-600 dark:text-amber-400";
      case "?":
        return "text-textMuted";
      default:
        return "text-textMuted";
    }
  }

  function isActiveRow(row: RailRow, index: number): boolean {
    return row.kind === "file" && index === activeIndex;
  }

  // --- resize handle -------------------------------------------------------
  const MIN_WIDTH = 180;
  const MAX_WIDTH = 520;
  let dragging = $state(false);

  function startResize(event: PointerEvent): void {
    if (!onResize) return;
    dragging = true;
    const startX = event.clientX;
    const startWidth = width;
    const target = event.currentTarget as HTMLElement;
    target.setPointerCapture?.(event.pointerId);
    const move = (e: PointerEvent) => {
      const next = Math.round(startWidth + (e.clientX - startX));
      onResize?.(Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, next)));
    };
    const stop = () => {
      dragging = false;
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
  }

  /** Keyboard resizing, because a drag handle alone is a pointer-only control. */
  function resizeKey(event: KeyboardEvent): void {
    if (!onResize) return;
    const step = event.shiftKey ? 40 : 12;
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      onResize(Math.max(MIN_WIDTH, width - step));
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      onResize(Math.min(MAX_WIDTH, width + step));
    } else if (event.key === "Home") {
      // Home/End jump to the extremes: the window-splitter pattern's answer to
      // "narrowest" and "widest", which arrow-stepping alone makes a chore.
      event.preventDefault();
      onResize(MIN_WIDTH);
    } else if (event.key === "End") {
      event.preventDefault();
      onResize(MAX_WIDTH);
    }
  }
</script>

<aside
  class="relative flex shrink-0 flex-col border-r border-border/60 bg-surface/40 font-sans"
  style="width: {width}px"
  aria-label="Files in this diff"
>
  <!-- Which change: uncommitted work, or one of the recent commits. Folded by
       default; the file list below is what gets used every few seconds. -->
  <div class="flex items-center gap-1.5 border-b border-border/60 px-2 py-1.5">
    <button
      type="button"
      class="flex min-w-0 flex-1 items-center gap-1.5 rounded px-1 py-0.5 text-left hover:bg-surfaceHover"
      aria-expanded={commitsOpen}
      onclick={() => (commitsOpen = !commitsOpen)}
      title="Show recent commits"
    >
      {#if commitsOpen}
        <ChevronDown size={12} class="shrink-0 text-textMuted" />
      {:else}
        <ChevronRight size={12} class="shrink-0 text-textMuted" />
      {/if}
      <GitCommitHorizontal size={12} class="shrink-0 text-accent" />
      <span class="text-[11px] font-semibold text-textPrimary">Change</span>
      <span class="min-w-0 flex-1 truncate text-[10px] text-textMuted">
        {onWorkingTree
          ? "Uncommitted changes"
          : commits.entries.find((c) => isCurrentCommit(c, selectedCommitId))
            ? commitLabel(commits.entries.find((c) => isCurrentCommit(c, selectedCommitId))!)
            : selectedCommitId
              ? shortHash(selectedCommitId)
              : ""}
      </span>
    </button>
    <button
      type="button"
      class="gp-icon-btn !p-1"
      onclick={onCollapse}
      title="Hide the file list"
      aria-label="Hide the file list"
    >
      <PanelLeftClose size={12} />
    </button>
  </div>

  {#if commitsOpen}
    <div class="max-h-56 shrink-0 overflow-y-auto gp-scroll border-b border-border/60 py-1">
      <!-- Uncommitted work is first because it is what a reader is most often
           coming back to, and it is the one entry that is not in the graph. -->
      <button
        type="button"
        class="flex w-full items-center gap-1.5 px-2.5 py-1 text-left text-[11px] hover:bg-surfaceHover {onWorkingTree
          ? 'bg-accent/10 text-accent'
          : 'text-textMuted'}"
        aria-current={onWorkingTree ? "true" : undefined}
        onclick={onPickWorkingTree}
      >
        <Pencil size={11} class="shrink-0" />
        <span class="min-w-0 flex-1 truncate">Uncommitted changes</span>
        <!-- -1 is "not counted"; rendering it as 0 would report an unscanned
             working tree as clean. -->
        {#if workingTreeCount > 0}
          <span class="shrink-0 font-mono text-[9px]">{workingTreeCount}</span>
        {:else if workingTreeCount === 0}
          <span class="shrink-0 text-[9px]">clean</span>
        {/if}
      </button>

      {#each commits.entries as commit (commit.id)}
        {@const active = isCurrentCommit(commit, selectedCommitId)}
        <button
          type="button"
          class="flex w-full items-start gap-1.5 px-2.5 py-1 text-left text-[11px] hover:bg-surfaceHover {active
            ? 'bg-accent/10 text-accent'
            : 'text-textMuted'}"
          aria-current={active ? "true" : undefined}
          title={commitLabel(commit)}
          onclick={() => onPickCommit(commit)}
        >
          <span class="mt-px shrink-0 font-mono text-[9px] opacity-70">{shortHash(commit.id)}</span>
          <span class="min-w-0 flex-1">
            <span class="block truncate">{commitLabel(commit)}</span>
            <span class="block truncate text-[9px] opacity-70">
              {commit.authorName} · {formatRelativeTime(commit.timestamp)}{commit.isMerge
                ? " · merge"
                : ""}
            </span>
          </span>
        </button>
      {/each}

      {#if commitNote}
        <p class="px-2.5 py-1 text-[9px] text-textMuted">{commitNote}</p>
      {/if}
    </div>
  {/if}

  <div class="flex items-center gap-1.5 border-b border-border/60 px-2.5 py-1.5">
    <FileCode size={12} class="shrink-0 text-accent" />
    <span class="text-[11px] font-semibold text-textPrimary">
      {rail.source === "commit" ? "Commit files" : "Changed files"}
    </span>
    <span class="ml-auto text-[10px] tabular-nums text-textMuted">{rail.entries.length}</span>
    <div class="gp-segmented !p-0.5" role="group" aria-label="File list layout">
      <button
        type="button"
        class="gp-seg-btn !px-1.5 !py-0.5"
        aria-pressed={mode === "list"}
        data-active={mode === "list" ? "true" : "false"}
        title="Flat list, in the order git reports"
        onclick={() => (mode = "list")}
      >
        <Rows3 size={11} />
      </button>
      <button
        type="button"
        class="gp-seg-btn !px-1.5 !py-0.5"
        aria-pressed={mode === "tree"}
        data-active={mode === "tree" ? "true" : "false"}
        title="Group by directory"
        onclick={() => (mode = "tree")}
      >
        <ListTree size={11} />
      </button>
    </div>
  </div>

  <div class="flex items-center gap-1 border-b border-border/60 px-2 py-1">
    <Filter size={11} class="shrink-0 text-textMuted" />
    <input
      bind:value={query}
      type="text"
      class="min-w-0 flex-1 bg-transparent py-0.5 text-[11px] text-textPrimary outline-none placeholder:text-textMuted/60"
      placeholder="Filter files…"
      aria-label="Filter files in this diff"
      onkeydown={(e) => {
        if (e.key === "Escape" && query) {
          e.stopPropagation();
          query = "";
        }
      }}
    />
    {#if query}
      <button
        type="button"
        class="gp-icon-btn !p-0.5"
        onclick={() => (query = "")}
        title="Clear the filter"
        aria-label="Clear the filter"
      >
        <X size={11} />
      </button>
    {/if}
  </div>

  <!-- A cut-short list must say so. Rendering the first fifty of three hundred
       as though they were all of them tells the reader they have seen the
       whole commit. -->
  {#if note}
    <p class="border-b border-border/60 px-2.5 py-1 text-[10px] text-amber-600 dark:text-amber-400">
      {note}
    </p>
  {/if}
  {#if searchNote}
    <p class="border-b border-border/60 px-2.5 py-1 text-[10px] text-textMuted">{searchNote}</p>
  {/if}

  {#snippet railRow(row: RailRow | undefined, index: number)}
    {#if row?.kind === "dir"}
      <button
        type="button"
        class="flex w-full items-center gap-1 py-0.5 pr-2 text-left text-[11px] text-textMuted hover:bg-surfaceHover"
        style="height: {ROW_HEIGHT}px; padding-left: {8 + row.depth * 10}px"
        aria-expanded={!collapsed.has(row.path)}
        title={row.path}
        onclick={() => toggleDir(row.path)}
      >
        {#if collapsed.has(row.path)}
          <ChevronRight size={11} class="shrink-0" />
        {:else}
          <ChevronDown size={11} class="shrink-0" />
        {/if}
        <span class="min-w-0 flex-1 truncate font-medium">{row.name}</span>
        <span class="shrink-0 font-mono text-[9px] opacity-70">{row.fileCount}</span>
      </button>
    {:else if row?.kind === "file"}
      {@const active = isActiveRow(row, index)}
      {@const churn = churnLabel(row.entry)}
      <button
        type="button"
        class="flex w-full items-center gap-1.5 py-0.5 pr-2 text-left text-[11px] hover:bg-surfaceHover {active
          ? 'bg-accent/10 text-accent'
          : 'text-textMuted'}"
        style="height: {ROW_HEIGHT}px; padding-left: {8 + row.depth * 10}px"
        aria-current={active ? "true" : undefined}
        title={row.title}
        onclick={() => onOpen(row.entry)}
      >
        <span class="w-3 shrink-0 font-mono text-[10px] {statusTone(row.entry.statusCode)}">
          {row.entry.statusCode.charAt(0) || "?"}
        </span>
        <span class="flex min-w-0 flex-1 items-baseline gap-0.5 truncate">
          {#if row.dir}
            <span class="min-w-0 shrink truncate text-[10px] opacity-55">{row.dir}/</span>
          {/if}
          <span class="shrink-0 truncate">{row.name}</span>
        </span>
        {#if rail.source === "worktree" && row.entry.isStaged}
          <span class="shrink-0 text-[9px] text-emerald-600 dark:text-emerald-400">staged</span>
        {/if}
        {#if churn}
          <span class="shrink-0 font-mono text-[9px] tabular-nums text-textMuted">{churn}</span>
        {/if}
      </button>
    {/if}
  {/snippet}

  {#if rows.length === 0}
    <p class="px-2.5 py-3 text-[11px] text-textMuted">
      {rail.entries.length === 0 ? "No files in this change." : "No file matches the filter."}
    </p>
  {:else if virtualize}
    <VirtualList
      items={rows}
      rowHeight={ROW_HEIGHT}
      overscan={12}
      bind:scrollTop={listScroll}
      class="min-h-0 flex-1 py-1"
    >
      {#snippet row(item, index)}
        {@render railRow(item, index)}
      {/snippet}
    </VirtualList>
  {:else}
    <ul class="min-h-0 flex-1 overflow-y-auto gp-scroll py-1">
      {#each rows as row, index (row.key)}
        <li>{@render railRow(row, index)}</li>
      {/each}
    </ul>
  {/if}

  {#if onResize}
    <!-- Sits on the border, so a 224px column stops being a fixed cost for
         every repository whose paths are longer than 224px. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <!-- Justified: a focusable `separator` IS the ARIA window-splitter
         pattern — it takes focus, reports aria-valuenow/min/max, moves on the
         arrow keys and jumps to its extremes on Home/End. svelte-check
         classifies every separator as non-interactive, which is true only of
         the decorative kind. -->
    <div
      class="absolute inset-y-0 -right-1 z-10 w-2 cursor-col-resize {dragging
        ? 'bg-accent/40'
        : 'hover:bg-accent/25'}"
      role="separator"
      aria-label="Resize the file list"
      aria-orientation="vertical"
      aria-valuenow={width}
      aria-valuemin={MIN_WIDTH}
      aria-valuemax={MAX_WIDTH}
      tabindex="0"
      onpointerdown={startResize}
      onkeydown={resizeKey}
    ></div>
  {/if}
</aside>
