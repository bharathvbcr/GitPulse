<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { repoStore, type BranchInfo, type TagInfo } from "../stores/repoStore";
  import { askConfirm, askText } from "../stores/modalStore";
  import { filterStore } from "../stores/filterStore";
  import { debounce } from "../async/debounce";
  import { formatError } from "../ui/formatError";
  import {
    branchLeafName,
    countFolder,
    filterBranchSections,
    groupBranches,
    highlightMatches,
    isStaleBranch,
    localNameFor,
  } from "../branches/groupBranches";
  import {
    flattenRows,
    type FolderHeaderRow,
    type SectionHeaderRow,
    type TagRow,
  } from "../branches/flattenRows";
  import type { BranchFilterTab, BranchSection } from "../branches/types";
  import { escalateDeleteDecision } from "../branches/deleteEscalation";
  import { computeWindow } from "../dom/virtualWindow";
  import { portal } from "../dom/portal";
  import ChurnBar from "./ChurnBar.svelte";
  import {
    ChevronDown,
    ChevronRight,
    Cloud,
    Copy,
    Crosshair,
    Download,
    GitBranch,
    GitCompare,
    GitMerge,
    MoreHorizontal,
    Pencil,
    Plus,
    Search,
    Sparkles,
    Star,
    Tag,
    Trash2,
    Upload,
    X,
  } from "lucide-svelte";

  const ROW_HEIGHT = 26;
  const OVERSCAN = 12;
  const FILTER_DEBOUNCE_MS = 80;

  let query = $state("");
  let debouncedQuery = $state("");
  const applyFilter = debounce((q: string) => (debouncedQuery = q), FILTER_DEBOUNCE_MS);

  let activeTab = $state<BranchFilterTab>("all");
  let pinnedNames = $state<Set<string>>(new Set());
  let creating = $state(false);
  let createName = $state("");
  let suggesting = $state(false);
  let collapsed = $state<Record<string, boolean>>({});
  let selectedIndex = $state<number>(-1);
  let locateName = $state<string | null>(null);
  let menu = $state<{ x: number; y: number; branch?: BranchInfo; tag?: TagInfo } | null>(null);

  let containerEl: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewportHeight = $state(450);

  let workAdd = $derived($repoStore.statuses.reduce((n, s) => n + (s.additions || 0), 0));
  let workDel = $derived($repoStore.statuses.reduce((n, s) => n + (s.deletions || 0), 0));
  let statsPending = $derived($repoStore.statsPending ?? false);
  let statsFailed = $derived($repoStore.statsFailed ?? false);

  // Grouping, filtering, and flattening pipeline
  let groupedSections = $derived(groupBranches($repoStore.branches, $repoStore.tags, pinnedNames));
  let filteredSections = $derived(filterBranchSections(groupedSections, debouncedQuery, activeTab));
  let allRows = $derived(flattenRows(filteredSections, isCollapsed));

  // O(1) Virtual Windowing math
  let win = $derived(computeWindow(scrollTop, viewportHeight, allRows.length, ROW_HEIGHT, OVERSCAN));
  let visibleRows = $derived(allRows.slice(win.start, win.end));
  let totalHeight = $derived(allRows.length * ROW_HEIGHT);
  let offsetY = $derived(win.start * ROW_HEIGHT);

  // Counts for quick filter tabs
  let localCount = $derived($repoStore.branches.filter((b) => !b.is_remote).length);
  let remoteCount = $derived($repoStore.branches.filter((b) => b.is_remote).length);
  let pinnedCount = $derived(pinnedNames.size);
  let staleCount = $derived(
    $repoStore.branches.filter((b) => isStaleBranch(b.last_commit_timestamp)).length
  );
  let activeCount = $derived($repoStore.branches.length - staleCount);

  function isCollapsed(id: string, kind: BranchSection["kind"]): boolean {
    if (id in collapsed) return collapsed[id];
    return kind === "remote" || kind === "tags";
  }

  function toggle(id: string, kind: BranchSection["kind"]) {
    collapsed = { ...collapsed, [id]: !isCollapsed(id, kind) };
  }

  function collapseAll() {
    const next: Record<string, boolean> = {};
    for (const section of groupedSections) {
      next[section.id] = true;
      const walk = (folders: typeof section.folders) => {
        for (const f of folders) {
          next[f.id] = true;
          walk(f.folders);
        }
      };
      walk(section.folders);
    }
    collapsed = next;
  }

  function expandAll() {
    const next: Record<string, boolean> = {};
    for (const section of groupedSections) {
      next[section.id] = false;
      const walk = (folders: typeof section.folders) => {
        for (const f of folders) {
          next[f.id] = false;
          walk(f.folders);
        }
      };
      walk(section.folders);
    }
    collapsed = next;
  }

  function togglePin(branchName: string, e?: MouseEvent) {
    e?.stopPropagation();
    const next = new Set(pinnedNames);
    if (next.has(branchName)) {
      next.delete(branchName);
    } else {
      next.add(branchName);
    }
    pinnedNames = next;
    savePinned();
  }

  function loadPinned() {
    const path = $repoStore.currentPath;
    if (!path) return;
    try {
      const raw = localStorage.getItem(`gitpulse:pinned:${path}`);
      if (raw) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed)) pinnedNames = new Set(parsed);
      }
    } catch {}
  }

  function savePinned() {
    const path = $repoStore.currentPath;
    if (!path) return;
    try {
      localStorage.setItem(`gitpulse:pinned:${path}`, JSON.stringify([...pinnedNames]));
    } catch {}
  }

  function locateCurrentBranch() {
    const curr = $repoStore.currentBranch;
    if (!curr) return;

    const nextCollapsed: Record<string, boolean> = { ...collapsed, local: false };
    const parts = curr.split("/").filter(Boolean);
    let path = "local";
    for (let i = 0; i < parts.length - 1; i++) {
      path += `/${parts[i]}`;
      nextCollapsed[path] = false;
    }
    collapsed = nextCollapsed;
    locateName = curr;
  }

  $effect(() => {
    const name = locateName;
    if (!name) return;
    const idx = allRows.findIndex((r) => r.kind === "branch" && r.branch.name === name);
    if (idx < 0) {
      if (allRows.length > 0) locateName = null;
      return;
    }
    locateName = null;
    if (!containerEl) return;
    selectedIndex = idx;
    const targetY = Math.max(0, idx * ROW_HEIGHT - viewportHeight / 3);
    containerEl.scrollTo({ top: targetY, behavior: "smooth" });
  });

  $effect(() => {
    if (selectedIndex >= allRows.length) {
      selectedIndex = allRows.length > 0 ? allRows.length - 1 : -1;
    }
  });

  function selectRef(name: string) {
    const next = $filterStore.selectedBranch === name ? null : name;
    filterStore.selectBranch(next);
  }

  function checkoutName(name: string) {
    void repoStore.checkoutBranch(name);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (query || menu) {
        e.preventDefault();
        query = "";
        debouncedQuery = "";
        menu = null;
      }
      return;
    }
    if (e.target instanceof HTMLInputElement) return;

    if (e.key === "ArrowDown") {
      e.preventDefault();
      selectedIndex = Math.min(allRows.length - 1, selectedIndex + 1);
      ensureVisible(selectedIndex);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selectedIndex = Math.max(0, selectedIndex - 1);
      ensureVisible(selectedIndex);
    } else if (e.key === "Enter" || e.key === " ") {
      if (selectedIndex >= 0 && selectedIndex < allRows.length) {
        e.preventDefault();
        const row = allRows[selectedIndex];
        if (row.kind === "section-header") {
          toggle(row.sectionId, row.section.kind);
        } else if (row.kind === "folder-header") {
          toggle(row.folderId, "local");
        } else if (row.kind === "branch") {
          if (e.metaKey || e.ctrlKey) {
            checkoutName(localNameFor(row.branch));
          } else {
            selectRef(row.branch.name);
          }
        } else if (row.kind === "tag") {
          selectRef(row.tag.name);
        }
      }
    } else if (e.key === "ArrowRight") {
      if (selectedIndex >= 0 && selectedIndex < allRows.length) {
        const row = allRows[selectedIndex];
        if (row.kind === "section-header" && isCollapsed(row.sectionId, row.section.kind)) {
          toggle(row.sectionId, row.section.kind);
        } else if (row.kind === "folder-header" && isCollapsed(row.folderId, "local")) {
          toggle(row.folderId, "local");
        }
      }
    } else if (e.key === "ArrowLeft") {
      if (selectedIndex >= 0 && selectedIndex < allRows.length) {
        const row = allRows[selectedIndex];
        if (row.kind === "section-header" && !isCollapsed(row.sectionId, row.section.kind)) {
          toggle(row.sectionId, row.section.kind);
        } else if (row.kind === "folder-header" && !isCollapsed(row.folderId, "local")) {
          toggle(row.folderId, "local");
        }
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

  async function submitCreate() {
    const name = createName.trim();
    if (!name) return;
    const outcome = await repoStore.createBranch(name);
    // F14: a failed create keeps the form open and the typed name intact.
    if (!outcome.ok) return;
    createName = "";
    creating = false;
  }

  async function suggestName() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    suggesting = true;
    try {
      const gen = await invoke<{ text: string }>("cmd_ai_suggest_branch_name", {
        repoPath: repo,
      });
      // The suggestion raced a tab switch: it belongs to another repo now.
      if ($repoStore.currentPath !== repo) return;
      const raw = (gen?.text || "").trim().split(/\s+/)[0] || "";
      if (raw) createName = raw.replace(/^[#`]+/, "").replace(/[`]+$/, "");
    } catch (err) {
      if ($repoStore.currentPath !== repo) return;
      repoStore.setError(formatError(err));
    } finally {
      suggesting = false;
    }
  }

  function openBranchMenu(e: MouseEvent, branch: BranchInfo) {
    e.preventDefault();
    e.stopPropagation();
    menu = { x: e.clientX, y: e.clientY, branch };
  }

  function openTagMenu(e: MouseEvent, tag: TagInfo) {
    e.preventDefault();
    e.stopPropagation();
    menu = { x: e.clientX, y: e.clientY, tag };
  }

  // A background refresh can land while the menu is open; actions must run
  // against the branch as it exists NOW (is_current/is_remote gating), not
  // the snapshot captured at right-click time.
  function liveBranch(stale: BranchInfo): BranchInfo {
    return $repoStore.branches.find((b) => b.name === stale.name && b.is_remote === stale.is_remote) ?? stale;
  }

  async function copyText(value: string) {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      repoStore.setError("Could not copy to clipboard");
    }
    menu = null;
  }

  async function runMerge(branch: BranchInfo, ffOnly: boolean) {
    menu = null;
    await repoStore.mergeBranch(localNameFor(branch), ffOnly);
  }

  async function runRename(branch: BranchInfo) {
    menu = null;
    const next = await askText({
      title: "Rename branch",
      message: branch.name,
      initialValue: branch.name,
      confirmLabel: "Rename",
    });
    const trimmed = next?.trim();
    if (!trimmed || trimmed === branch.name) return;
    await repoStore.renameBranch(branch.name, trimmed);
  }

  async function runDelete(branch: BranchInfo) {
    menu = null;
    // Two-step escalation (WorktreesPanel's armed remove is the house style):
    // the first confirm runs the SAFE delete. Only an explicit second confirm
    // — shown when git refused because the branch is unmerged — retries with
    // force. The backend still refuses force-deletes of the default branch or
    // any worktree-checked-out branch regardless of what this dialog offers.
    const ok = await askConfirm({
      title: "Delete branch",
      message: `Delete branch ${branch.name}?`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    const outcome = await repoStore.deleteBranch(branch.name, false);
    if (outcome.ok) return;
    const decision = escalateDeleteDecision(outcome.error ?? "", branch);
    if (!decision.canRetryForce || !decision.message) return;
    const forceOk = await askConfirm({
      title: "Force-delete branch",
      message: decision.message,
      confirmLabel: "Force delete",
    });
    if (!forceOk) return;
    await repoStore.deleteBranch(branch.name, true);
  }

  async function runCompare(branch: BranchInfo) {
    menu = null;
    const from = $repoStore.currentBranch || $repoStore.defaultBranch || "main";
    await repoStore.selectRangeDiff(from, localNameFor(branch));
  }

  function onWindowClick() {
    menu = null;
  }

  $effect(() => {
    if ($repoStore.currentPath) {
      loadPinned();
    }
  });

  onMount(() => {
    window.addEventListener("click", onWindowClick);
    return () => {
      window.removeEventListener("click", onWindowClick);
      applyFilter.cancel();
    };
  });
</script>

{#snippet highlightedLabel(text: string, q: string)}
  {#if !q}
    <span class="truncate">{text}</span>
  {:else}
    <span class="truncate inline-flex items-center gap-0">
      {#each highlightMatches(text, q) as chunk, i (`${i}:${chunk.matched}:${chunk.text}`)}
        {#if chunk.matched}
          <mark class="bg-accent/30 text-accent font-semibold rounded-[2px] px-0.5 leading-none">{chunk.text}</mark>
        {:else}
          <span>{chunk.text}</span>
        {/if}
      {/each}
    </span>
  {/if}
{/snippet}

{#snippet branchRow(branch: BranchInfo, depth: number, isRowSelected: boolean)}
  {@const selected = $filterStore.selectedBranch === branch.name}
  {@const isPinned = pinnedNames.has(branch.name)}
  {@const leaf = branchLeafName(branch)}
  {@const statsMissing =
    branch.additions === 0 &&
    branch.deletions === 0 &&
    branch.commits_ahead_of_base === 0 &&
    !branch.is_current &&
    !branch.is_default}
  <div
    class="gp-cv-row w-full h-[26px] rounded-full flex items-center gap-1.5 pr-1 group transition-colors select-none {branch.is_current
      ? 'bg-accent/15 text-accent font-semibold ring-1 ring-accent/40'
      : selected
        ? 'bg-accent/10 text-textPrimary ring-1 ring-accent/20'
        : isRowSelected
          ? 'bg-surfaceHover text-textPrimary ring-1 ring-border'
          : 'text-textPrimary hover:bg-surfaceHover'}"
    style="padding-left: {8 + depth * 12}px;"
  >
    <button
      type="button"
      onclick={(e) => togglePin(branch.name, e)}
      title={isPinned ? "Unpin branch" : "Pin branch to top"}
      aria-label={isPinned ? "Unpin branch" : "Pin branch to top"}
      class="p-0.5 rounded-full text-textMuted hover:text-amber-400 transition-opacity shrink-0 {isPinned ? 'text-amber-400 opacity-100' : 'opacity-0 group-hover:opacity-60 group-focus-within:opacity-60'}"
    >
      <Star size={11} class={isPinned ? "fill-amber-400" : ""} />
    </button>
    <button
      type="button"
      onclick={() => selectRef(branch.name)}
      ondblclick={() => checkoutName(localNameFor(branch))}
      oncontextmenu={(e) => openBranchMenu(e, branch)}
      title="{branch.name}{branch.last_summary ? `\n${branch.last_summary}` : ''}"
      class="flex-1 min-w-0 flex items-center gap-1.5 text-left truncate"
    >
      <GitBranch size={13} class={branch.is_current ? "text-accent shrink-0" : "text-textMuted shrink-0"} />
      {@render highlightedLabel(leaf, debouncedQuery)}
      {#if branch.is_default}
        <span class="text-[9px] px-1 py-0 rounded-full bg-surface border border-border/80 text-textMuted font-mono shrink-0">default</span>
      {/if}
      {#if branch.is_gone}
        <span class="text-[9px] px-1 py-0 rounded-full bg-rose-500/15 text-rose-400 font-mono shrink-0">gone</span>
      {/if}
      {#if isStaleBranch(branch.last_commit_timestamp)}
        <span class="text-[9px] px-1 py-0 rounded-full bg-surface text-textMuted font-mono shrink-0">stale</span>
      {/if}
    </button>
    <div class="flex items-center gap-1 shrink-0 opacity-90">
      {#if statsPending && statsMissing}
        <span class="inline-block w-6 h-1 rounded-full bg-border/70 animate-pulse" aria-hidden="true"></span>
      {:else if statsFailed && statsMissing}
        <!-- Stats fetch failed outright: a dimmed static marker instead of zeros pretending "no churn". -->
        <span class="inline-block w-6 h-1 rounded-full bg-rose-500/20 opacity-60" title="Churn unavailable (stats failed)" aria-hidden="true"></span>
      {/if}
      {#if branch.additions > 0 || branch.deletions > 0}
        <ChurnBar additions={branch.additions} deletions={branch.deletions} />
      {/if}
      {#if branch.is_current && (workAdd > 0 || workDel > 0)}
        <span class="text-[9px] text-textMuted font-mono" title="Uncommitted working tree">wt</span>
        <ChurnBar additions={workAdd} deletions={workDel} />
      {/if}
      {#if branch.ahead_count > 0}
        <span class="text-[10px] font-mono font-bold px-1 py-0 rounded-full bg-emerald-500/15 text-emerald-400 border border-emerald-500/25">↑{branch.ahead_count}</span>
      {/if}
      {#if branch.behind_count > 0}
        <span class="text-[10px] font-mono font-bold px-1 py-0 rounded-full bg-amber-500/15 text-amber-400 border border-amber-500/25">↓{branch.behind_count}</span>
      {/if}
      {#if branch.commits_ahead_of_base > 0 && !branch.is_current}
        <span class="text-[10px] font-mono text-textMuted">+{branch.commits_ahead_of_base}</span>
      {/if}
      {#if branch.is_current}
        <span class="w-1.5 h-1.5 rounded-full bg-accent animate-pulse shadow-sm shrink-0"></span>
      {/if}
      <button
        type="button"
        class="p-0.5 rounded-full opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 hover:bg-background text-textMuted transition-opacity shrink-0"
        onclick={(e) => {
          e.stopPropagation();
          openBranchMenu(e, branch);
        }}
        title="Branch actions"
        aria-label="Branch actions"
      >
        <MoreHorizontal size={12} />
      </button>
    </div>
  </div>
{/snippet}

{#snippet tagRow(row: TagRow, isRowSelected: boolean)}
  <button
    type="button"
    onclick={() => selectRef(row.tag.name)}
    oncontextmenu={(e) => openTagMenu(e, row.tag)}
    class="gp-cv-row w-full h-[26px] px-2 rounded-full flex items-center gap-1.5 text-left transition-colors select-none {$filterStore.selectedBranch === row.tag.name
      ? 'bg-accent/10 text-accent ring-1 ring-accent/20'
      : isRowSelected
        ? 'bg-surfaceHover text-textPrimary ring-1 ring-border'
        : 'text-textPrimary hover:bg-surfaceHover'}"
  >
    <Tag size={12} class="text-textMuted shrink-0" />
    {@render highlightedLabel(row.tag.name, debouncedQuery)}
  </button>
{/snippet}

{#snippet folderHeader(row: FolderHeaderRow, isRowSelected: boolean)}
  {@const closed = isCollapsed(row.folderId, "local")}
  <button
    type="button"
    onclick={() => toggle(row.folderId, "local")}
    aria-expanded={!closed}
    class="gp-cv-row w-full h-[26px] flex items-center gap-1.5 text-[11px] font-semibold text-textMuted uppercase tracking-wider hover:text-textPrimary transition-colors select-none {isRowSelected ? 'bg-surfaceHover text-textPrimary' : ''}"
    style="padding-left: {8 + row.depth * 12}px"
  >
    {#if closed}
      <ChevronRight size={12} class="shrink-0" />
    {:else}
      <ChevronDown size={12} class="shrink-0" />
    {/if}
    {@render highlightedLabel(row.folder.label, debouncedQuery)}
    <span class="text-textMuted/70 font-normal text-[10px]">({countFolder(row.folder)})</span>
  </button>
{/snippet}

{#snippet sectionHeader(row: SectionHeaderRow, isRowSelected: boolean)}
  {@const closed = isCollapsed(row.sectionId, row.section.kind)}
  <button
    type="button"
    onclick={() => toggle(row.sectionId, row.section.kind)}
    aria-expanded={!closed}
    class="sticky top-0 z-10 h-[26px] bg-surface w-full flex items-center gap-1 px-2 text-[10px] font-bold text-textMuted uppercase tracking-wider hover:text-textPrimary select-none {isRowSelected ? 'bg-surfaceHover text-textPrimary' : ''}"
  >
    {#if closed}
      <ChevronRight size={11} class="shrink-0" />
    {:else}
      <ChevronDown size={11} class="shrink-0" />
    {/if}
    {#if row.section.kind === "pinned"}
      <Star size={11} class="text-amber-400 fill-amber-400 shrink-0" />
    {:else if row.section.kind === "remote"}
      <Cloud size={11} class="shrink-0" />
    {:else if row.section.kind === "tags"}
      <Tag size={11} class="shrink-0" />
    {:else}
      <GitBranch size={11} class="shrink-0" />
    {/if}
    <span class="truncate">{row.section.label}</span>
    <span class="font-normal text-textMuted/70">({row.section.branchCount})</span>
  </button>
{/snippet}

<!-- Container with full keyboard navigation support -->
<!--
  Deliberately NOT a strict ARIA tree: correct semantics need treeitem/group
  children with roving tabindex; faking it announces broken structure to
  screen readers. Rows stay real buttons so native semantics carry instead.
-->
<!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_static_element_interactions -->
<div
  class="flex flex-col h-full focus:outline-none"
  role="tree"
  tabindex="0"
  onkeydown={handleKeydown}
>
  <!-- Header Bar -->
  <div class="flex items-center justify-between text-[10px] font-bold text-textMuted uppercase tracking-wider px-2 mb-1">
    <span>Branches ({$repoStore.branches.length})</span>
    <div class="flex items-center gap-0.5">
      <button
        type="button"
        onclick={locateCurrentBranch}
        title="Locate checked-out branch"
        class="p-1 rounded-full hover:bg-surfaceHover hover:text-accent text-textMuted transition-colors"
      >
        <Crosshair size={12} />
      </button>
      <button
        type="button"
        onclick={expandAll}
        title="Expand all folders"
        class="px-1 py-0.5 text-[9px] rounded hover:bg-surfaceHover hover:text-textPrimary text-textMuted transition-colors"
      >
        +All
      </button>
      <button
        type="button"
        onclick={collapseAll}
        title="Collapse all folders"
        class="px-1 py-0.5 text-[9px] rounded hover:bg-surfaceHover hover:text-textPrimary text-textMuted transition-colors"
      >
        -All
      </button>
      <button
        type="button"
        onclick={() => (creating = !creating)}
        title="Create branch"
        class="p-1 rounded-full hover:bg-surfaceHover hover:text-accent text-textMuted transition-colors ml-0.5"
      >
        <Plus size={12} />
      </button>
    </div>
  </div>

  <!-- Search Box -->
  <div class="px-1 mb-1">
    <div class="flex items-center gap-1 bg-background border border-border/80 rounded-full px-2 py-1 focus-within:border-accent/60 focus-within:shadow-[var(--ring-focus)] transition-all duration-150">
      <Search size={11} class="text-textMuted shrink-0" />
      <input
        type="text"
        bind:value={query}
        oninput={(e) => applyFilter(e.currentTarget.value)}
        placeholder="Filter branches…"
        class="w-full bg-transparent text-[11px] text-textPrimary placeholder:text-textMuted/60 focus:outline-none"
      />
      {#if query}
        <button
          type="button"
          onclick={() => { query = ""; debouncedQuery = ""; }}
          class="text-textMuted hover:text-textPrimary p-0.5"
        >
          <X size={10} />
        </button>
      {/if}
    </div>
  </div>

  <!-- Quick Filter Chips -->
  <div class="flex items-center gap-1 px-1 mb-1.5 overflow-x-auto no-scrollbar text-[10px]">
    <button
      type="button"
      onclick={() => (activeTab = "all")}
      class="px-2 py-0.5 rounded-full border transition-all shrink-0 {activeTab === 'all' ? 'bg-accent/15 border-accent/40 text-accent font-semibold' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
    >
      All
    </button>
    <button
      type="button"
      onclick={() => (activeTab = "local")}
      class="px-2 py-0.5 rounded-full border transition-all shrink-0 {activeTab === 'local' ? 'bg-accent/15 border-accent/40 text-accent font-semibold' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
    >
      Local ({localCount})
    </button>
    <button
      type="button"
      onclick={() => (activeTab = "remote")}
      class="px-2 py-0.5 rounded-full border transition-all shrink-0 {activeTab === 'remote' ? 'bg-accent/15 border-accent/40 text-accent font-semibold' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
    >
      Remote ({remoteCount})
    </button>
    {#if pinnedCount > 0}
      <button
        type="button"
        onclick={() => (activeTab = "pinned")}
        class="px-2 py-0.5 rounded-full border transition-all shrink-0 flex items-center gap-0.5 {activeTab === 'pinned' ? 'bg-amber-500/15 border-amber-500/40 text-amber-400 font-semibold' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
      >
        <Star size={9} class="fill-amber-400 text-amber-400" />
        {pinnedCount}
      </button>
    {/if}
    <button
      type="button"
      onclick={() => (activeTab = "active")}
      class="px-2 py-0.5 rounded-full border transition-all shrink-0 {activeTab === 'active' ? 'bg-accent/15 border-accent/40 text-accent font-semibold' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
    >
      Active ({activeCount})
    </button>
    <button
      type="button"
      onclick={() => (activeTab = "stale")}
      class="px-2 py-0.5 rounded-full border transition-all shrink-0 {activeTab === 'stale' ? 'bg-accent/15 border-accent/40 text-accent font-semibold' : 'bg-surface border-border/60 text-textMuted hover:text-textPrimary'}"
    >
      Stale ({staleCount})
    </button>
  </div>

  <!-- Create branch form -->
  {#if creating}
    <form
      class="px-1 mb-2 flex items-center gap-1"
      onsubmit={(e) => {
        e.preventDefault();
        void submitCreate();
      }}
    >
      <input
        bind:value={createName}
        placeholder="feat/name"
        class="flex-1 min-w-0 bg-background border border-border/80 rounded-full px-2.5 py-1 text-[11px] text-textPrimary focus:outline-none focus:border-accent/60 font-mono transition-colors"
      />
      <button
        type="button"
        onclick={() => void suggestName()}
        title="Suggest name"
        class="p-1 rounded-full hover:bg-surfaceHover text-textMuted transition-colors"
        disabled={suggesting}
      >
        <Sparkles size={12} class={suggesting ? "animate-pulse text-accent" : ""} />
      </button>
      <button type="submit" class="gp-btn-primary !px-2 !py-0.5 !text-[10px]">Create</button>
    </form>
  {/if}

  <!-- Virtual Scroller Container -->
  <div
    bind:this={containerEl}
    bind:clientHeight={viewportHeight}
    onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}
    class="flex-1 overflow-y-auto min-h-0 relative select-none will-change-scroll"
  >
    {#if allRows.length === 0}
      <div class="px-3 py-6 text-center text-[11px] text-textMuted/70">
        {query ? `No branches match "${query}"` : "No branches found"}
      </div>
    {:else}
      <!-- Total Virtual Spacer -->
      <div style="height: {totalHeight}px; position: relative; width: 100%;">
        <!-- Rendered Slice Container -->
        <div
          style="transform: translate3d(0, {offsetY}px, 0); position: absolute; top: 0; left: 0; right: 0; will-change: transform;"
        >
          {#each visibleRows as row, i (row.key)}
            {@const globalIdx = win.start + i}
            {@const isRowSelected = selectedIndex === globalIdx}
            {#if row.kind === "section-header"}
              {@render sectionHeader(row, isRowSelected)}
            {:else if row.kind === "folder-header"}
              {@render folderHeader(row, isRowSelected)}
            {:else if row.kind === "branch"}
              {@render branchRow(row.branch, row.depth, isRowSelected)}
            {:else}
              {@render tagRow(row, isRowSelected)}
            {/if}
          {/each}
        </div>
      </div>
    {/if}
  </div>
</div>

{#if menu}
  <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
  <div
    use:portal={"body"}
    class="fixed z-50 min-w-44 gp-menu gp-pop text-xs text-textPrimary"
    style="left: {Math.min(menu.x, window.innerWidth - 200)}px; top: {Math.min(menu.y, window.innerHeight - 280)}px"
    role="menu"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.key === "Escape" && (menu = null)}
  >
    {#if menu.branch}
      {@const b = liveBranch(menu.branch)}
      <button class="gp-menu-item" onclick={() => { const name = b.name; menu = null; togglePin(name); }}>
        <Star size={12} class={pinnedNames.has(b.name) ? "fill-amber-400 text-amber-400" : ""} />
        {pinnedNames.has(b.name) ? "Unpin branch" : "Pin branch"}
      </button>
      {#if !b.is_current}
        <button class="gp-menu-item" onclick={() => { menu = null; checkoutName(localNameFor(b)); }}>
          <GitBranch size={12} /> Checkout
        </button>
        <button class="gp-menu-item" onclick={() => void runMerge(b, false)}>
          <GitMerge size={12} /> Merge into current
        </button>
        <button class="gp-menu-item" onclick={() => void runMerge(b, true)}>
          <GitMerge size={12} /> Fast-forward merge
        </button>
      {/if}
      <button class="gp-menu-item" onclick={() => void runCompare(b)}>
        <GitCompare size={12} /> Compare with current
      </button>
      {#if !b.is_remote}
        <button class="gp-menu-item" onclick={() => void runRename(b)}>
          <Pencil size={12} /> Rename…
        </button>
        <button class="gp-menu-item" onclick={() => { menu = null; void repoStore.push(undefined, b.name); }}>
          <Upload size={12} /> Push
        </button>
        <button class="gp-menu-item" onclick={() => { menu = null; void repoStore.pull(undefined, b.name); }}>
          <Download size={12} /> Pull
        </button>
        {#if !b.is_current}
          <button class="w-full px-3 py-1.5 text-left hover:bg-surfaceHover flex items-center gap-2 text-rose-400" onclick={() => void runDelete(b)}>
            <Trash2 size={12} /> Delete…
          </button>
        {/if}
      {/if}
      <button class="gp-menu-item" onclick={() => void copyText(b.name)}>
        <Copy size={12} /> Copy name
      </button>
    {:else if menu.tag}
      {@const t = menu.tag}
      <button class="gp-menu-item" onclick={() => { const name = t.name; menu = null; selectRef(name); }}>
        <Tag size={12} /> Filter history
      </button>
      <button class="gp-menu-item" onclick={() => void copyText(t.name)}>
        <Copy size={12} /> Copy name
      </button>
    {/if}
  </div>
{/if}
