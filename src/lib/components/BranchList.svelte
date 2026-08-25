<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { repoStore, type BranchInfo, type TagInfo } from "../stores/repoStore";
  import { askConfirm, askText } from "../stores/modalStore";
  import { filterStore } from "../stores/filterStore";
  import {
    branchLeafName,
    countFolder,
    filterBranchSections,
    groupBranches,
    isStaleBranch,
    localNameFor,
  } from "../branches/groupBranches";
  import type { BranchFolder, BranchSection } from "../branches/types";
  import { portal } from "../dom/portal";
  import ChurnBar from "./ChurnBar.svelte";
  import {
    ChevronDown,
    ChevronRight,
    Cloud,
    Copy,
    GitBranch,
    GitCompare,
    GitMerge,
    MoreHorizontal,
    Pencil,
    Plus,
    Search,
    Sparkles,
    Tag,
    Trash2,
    Upload,
    Download,
  } from "lucide-svelte";

  let query = $state("");
  let creating = $state(false);
  let createName = $state("");
  let suggesting = $state(false);
  let collapsed = $state<Record<string, boolean>>({});
  let menu = $state<{ x: number; y: number; branch?: BranchInfo; tag?: TagInfo } | null>(null);

  let workAdd = $derived($repoStore.statuses.reduce((n, s) => n + (s.additions || 0), 0));
  let workDel = $derived($repoStore.statuses.reduce((n, s) => n + (s.deletions || 0), 0));

  let sections = $derived(
    filterBranchSections(groupBranches($repoStore.branches, $repoStore.tags), query)
  );

  function isCollapsed(id: string, kind: BranchSection["kind"]): boolean {
    if (id in collapsed) return collapsed[id];
    return kind === "remote" || kind === "tags";
  }

  function toggle(id: string, kind: BranchSection["kind"]) {
    collapsed = { ...collapsed, [id]: !isCollapsed(id, kind) };
  }

  function selectRef(name: string) {
    const next = $filterStore.selectedBranch === name ? null : name;
    filterStore.selectBranch(next);
  }

  function checkoutName(name: string) {
    void repoStore.checkoutBranch(name);
  }

  async function submitCreate() {
    const name = createName.trim();
    if (!name) return;
    await repoStore.createBranch(name);
    createName = "";
    creating = false;
  }

  async function suggestName() {
    if (!$repoStore.currentPath) return;
    suggesting = true;
    try {
      const gen = await invoke<{ text: string }>("cmd_ai_suggest_branch_name", {
        repoPath: $repoStore.currentPath,
      });
      const raw = (gen?.text || "").trim().split(/\s+/)[0] || "";
      if (raw) createName = raw.replace(/^[#`]+/, "").replace(/[`]+$/, "");
    } catch (err) {
      repoStore.setError(String(err));
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
    const ok = await askConfirm({
      title: "Delete branch",
      message: `Delete branch ${branch.name}?`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
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

  onMount(() => {
    window.addEventListener("click", onWindowClick);
    return () => window.removeEventListener("click", onWindowClick);
  });
</script>

{#snippet branchRow(branch: BranchInfo, depth: number)}
  {@const selected = $filterStore.selectedBranch === branch.name}
  {@const leaf = branchLeafName(branch)}
  <div
    class="w-full rounded-full flex items-center gap-1.5 pr-1 group transition-[color,background-color,border-color,box-shadow] duration-150 {branch.is_current
      ? 'bg-accent/15 text-accent font-semibold ring-1 ring-accent/40'
      : selected
        ? 'bg-accent/10 text-textPrimary'
        : 'text-textPrimary hover:bg-surfaceHover'}"
    style="padding-left: {10 + depth * 12}px; padding-top: 4px; padding-bottom: 4px;"
  >
    <button
      type="button"
      onclick={() => selectRef(branch.name)}
      ondblclick={() => checkoutName(localNameFor(branch))}
      oncontextmenu={(e) => openBranchMenu(e, branch)}
      title="{branch.name}{branch.last_summary ? `\n${branch.last_summary}` : ''}"
      class="flex-1 min-w-0 flex items-center gap-2 text-left"
    >
      <GitBranch size={13} class={branch.is_current ? "text-accent shrink-0" : "text-textMuted shrink-0"} />
      <span class="truncate text-[12px]">{leaf}</span>
      {#if branch.is_default}
        <span class="text-[9px] px-1.5 py-0.2 rounded-full bg-surface border border-border/80 text-textMuted font-mono">default</span>
      {/if}
      {#if branch.is_gone}
        <span class="text-[9px] px-1.5 py-0.2 rounded-full bg-rose-500/15 text-rose-400 font-mono">gone</span>
      {/if}
      {#if isStaleBranch(branch.last_commit_timestamp)}
        <span class="text-[9px] px-1.5 py-0.2 rounded-full bg-surface text-textMuted font-mono">stale</span>
      {/if}
    </button>
    <div class="flex items-center gap-1.5 shrink-0 opacity-90">
      {#if branch.additions > 0 || branch.deletions > 0}
        <ChurnBar additions={branch.additions} deletions={branch.deletions} />
      {/if}
      {#if branch.is_current && (workAdd > 0 || workDel > 0)}
        <span class="text-[9px] text-textMuted font-mono" title="Uncommitted working tree">wt</span>
        <ChurnBar additions={workAdd} deletions={workDel} />
      {/if}
      {#if branch.ahead_count > 0}
        <span class="text-[10px] font-mono font-bold px-1 py-0.2 rounded-full bg-emerald-500/15 text-emerald-400 border border-emerald-500/25">↑{branch.ahead_count}</span>
      {/if}
      {#if branch.behind_count > 0}
        <span class="text-[10px] font-mono font-bold px-1 py-0.2 rounded-full bg-amber-500/15 text-amber-400 border border-amber-500/25">↓{branch.behind_count}</span>
      {/if}
      {#if branch.commits_ahead_of_base > 0 && !branch.is_current}
        <span class="text-[10px] font-mono text-textMuted">+{branch.commits_ahead_of_base}</span>
      {/if}
      {#if branch.is_current}
        <span class="w-2 h-2 rounded-full bg-accent animate-pulse shadow-sm"></span>
      {/if}
      <button
        type="button"
        class="p-0.5 rounded-full opacity-0 group-hover:opacity-100 hover:bg-background text-textMuted transition-opacity"
        onclick={(e) => {
          e.stopPropagation();
          openBranchMenu(e, branch);
        }}
        title="Branch actions"
      >
        <MoreHorizontal size={12} />
      </button>
    </div>
  </div>
{/snippet}

{#snippet folderTree(folders: BranchFolder[], depth: number)}
  {#each folders as folder (folder.id)}
    {@const closed = isCollapsed(folder.id, "local")}
    <button
      type="button"
      onclick={() => toggle(folder.id, "local")}
      class="w-full flex items-center gap-1.5 py-1 text-[11px] font-semibold text-textMuted uppercase tracking-wider hover:text-textPrimary transition-colors"
      style="padding-left: {10 + depth * 12}px"
    >
      {#if closed}
        <ChevronRight size={12} />
      {:else}
        <ChevronDown size={12} />
      {/if}
      <span class="truncate">{folder.label}</span>
      <span class="text-textMuted/70 font-normal text-[10px]">({countFolder(folder)})</span>
    </button>
    {#if !closed}
      {@render folderTree(folder.folders, depth + 1)}
      {#each folder.branches as branch (branch.name)}
        {@render branchRow(branch, depth + 1)}
      {/each}
    {/if}
  {/each}
{/snippet}


<div>
  <div class="flex items-center justify-between text-[10px] font-bold text-textMuted uppercase tracking-wider px-2 mb-1">
    <span>Branches ({$repoStore.branches.length})</span>
    <button
      type="button"
      onclick={() => (creating = !creating)}
      title="Create branch"
      class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent transition-colors"
    >
      <Plus size={12} />
    </button>
  </div>

  <div class="px-1 mb-1.5">
    <div class="flex items-center gap-1 bg-background border border-border/80 rounded-full px-2 py-1 focus-within:border-accent/60 focus-within:shadow-[var(--ring-focus)] transition-[border-color,box-shadow] duration-150">
      <Search size={11} class="text-textMuted shrink-0" />
      <input
        type="text"
        bind:value={query}
        placeholder="Filter branches…"
        class="w-full bg-transparent text-[11px] text-textPrimary placeholder:text-textMuted/60 focus:outline-none"
      />
    </div>
  </div>

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

  <div class="space-y-2">
    {#each sections as section (section.id)}
      {@const closed = isCollapsed(section.id, section.kind)}
      <div>
        <button
          type="button"
          onclick={() => toggle(section.id, section.kind)}
          class="w-full flex items-center gap-1 px-2 py-1 text-[10px] font-bold text-textMuted uppercase tracking-wider hover:text-textPrimary"
        >
          {#if closed}
            <ChevronRight size={11} />
          {:else}
            <ChevronDown size={11} />
          {/if}
          {#if section.kind === "remote"}
            <Cloud size={11} />
          {:else if section.kind === "tags"}
            <Tag size={11} />
          {:else}
            <GitBranch size={11} />
          {/if}
          <span>{section.label}</span>
          <span class="font-normal text-textMuted/70">({section.branchCount})</span>
        </button>
        {#if !closed}
          {#if section.kind === "tags"}
            {#each section.tags as tag (tag.name)}
              <button
                type="button"
                onclick={() => selectRef(tag.name)}
                oncontextmenu={(e) => openTagMenu(e, tag)}
                class="w-full px-2 py-1 rounded-full flex items-center gap-1.5 text-left transition-colors {$filterStore.selectedBranch === tag.name
                  ? 'bg-accent/10 text-accent'
                  : 'text-textPrimary hover:bg-surfaceHover'}"
              >
                <Tag size={12} class="text-textMuted shrink-0" />
                <span class="truncate text-[11px] font-mono">{tag.name}</span>
              </button>
            {/each}
          {:else}
            {@render folderTree(section.folders, 0)}
            {#each section.branches as branch (branch.name)}
              {@render branchRow(branch, 0)}
            {/each}
          {/if}
        {/if}
      </div>
    {/each}
  </div>
</div>

{#if menu}
  <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
  <!-- Portaled to body: the sidebar's gp-pane paint containment clips and
       stacks fixed popovers inside the pane, hiding this menu under main. -->
  <div
    use:portal
    class="fixed z-50 min-w-44 gp-menu gp-pop text-xs text-textPrimary"
    style="left: {Math.min(menu.x, window.innerWidth - 200)}px; top: {Math.min(menu.y, window.innerHeight - 280)}px"
    role="menu"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.key === "Escape" && (menu = null)}
  >
    {#if menu.branch}
      {@const b = menu.branch}
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
