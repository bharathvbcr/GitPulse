<script lang="ts">
  import type { VisualCommitRow } from "../canvas/GraphRenderer";
  import { getBranchColor } from "../canvas/Palette";
  import { formatRelativeTime } from "../format";
  import { authorColor, authorIdentity } from "../authors/authorIdentity";
  import { isKeyboardFocus } from "../dom/focusVisibility";
  import {
    GitMerge,
    GitBranch,
    Cloud,
    Tag,
    Compass,
    Copy,
    Check,
    Plus,
    Filter,
    GitCommit,
    Undo2,
  } from "lucide-svelte";
  import { copyText } from "../desktop/clipboard";
  import { toastStore } from "../stores/toastStore";
  import { repoStore } from "../stores/repoStore";
  import { filterStore } from "../stores/filterStore";
  import { askText } from "../stores/modalStore";
  import { clampMenuPosition } from "../branches/menuPosition";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";

  export interface RefItem {
    name: string;
    kind: "head" | "current-branch" | "local-branch" | "remote-branch" | "tag";
  }

  let {
    row,
    isSelected = false,
    density = "spacious",
    refs = [],
    onSelect,
    mergeTarget = null,
    onFocusRow,
    onBlurRow,
  }: {
    row: VisualCommitRow;
    isSelected?: boolean;
    density?: "spacious" | "compact";
    refs?: RefItem[];
    onSelect?: () => void;
    /**
     * The merge point this commit's closing line lands on, when it closes
     * into another branch. Rendered as screen-reader-only text so the
     * relationship the graph draws is part of the row's accessible content
     * — the accessible path must carry what the pointer tooltip shows.
     */
    mergeTarget?: Pick<VisualCommitRow, "id" | "summary"> | null;
    /**
     * Row focus, with whether it is keyboard-driven (`:focus-visible`).
     * The table shows the graph tooltip card for keyboard focus so the
     * hover affordance is not pointer-only.
     */
    onFocusRow?: (element: HTMLElement, keyboardVisible: boolean) => void;
    onBlurRow?: () => void;
  } = $props();

  let isCopied = $state(false);
  let isMenuOpen = $state(false);
  let menuPos = $state<{ left: number; top: number }>({ left: 0, top: 0 });
  let menuEl = $state<HTMLDivElement | undefined>();

  const isCompact = $derived(density === "compact");
  const avatar = $derived(authorIdentity(row.author_name, row.author_email));

  function getConventionalType(msg: string): { type: string; color: string } | null {
    const match = msg.match(/^([a-zA-Z]+)(\([^\)]+\))?(!)?:\s/);
    if (!match) return null;
    const type = match[1].toLowerCase();
    switch (type) {
      case "feat": return { type: "feat", color: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 border-emerald-500/30" };
      case "fix": return { type: "fix", color: "bg-rose-500/15 text-rose-600 dark:text-rose-400 border-rose-500/30" };
      case "refactor": return { type: "refactor", color: "bg-purple-500/15 text-purple-600 dark:text-purple-400 border-purple-500/30" };
      case "docs": return { type: "docs", color: "bg-sky-500/15 text-sky-600 dark:text-sky-400 border-sky-500/30" };
      case "chore": return { type: "chore", color: "bg-slate-500/15 text-slate-600 dark:text-slate-400 border-slate-500/30" };
      case "perf": return { type: "perf", color: "bg-amber-500/15 text-amber-600 dark:text-amber-400 border-amber-500/30" };
      case "test": return { type: "test", color: "bg-orange-500/15 text-orange-600 dark:text-orange-400 border-orange-500/30" };
      case "build": return { type: "build", color: "bg-teal-500/15 text-teal-600 dark:text-teal-400 border-teal-500/30" };
      case "ci": return { type: "ci", color: "bg-indigo-500/15 text-indigo-600 dark:text-indigo-400 border-indigo-500/30" };
      default: return { type, color: "bg-zinc-500/15 text-zinc-600 dark:text-zinc-400 border-zinc-500/30" };
    }
  }

  async function handleCopySha(e: MouseEvent) {
    e.stopPropagation();
    await copyText(row.id);
    isCopied = true;
    toastStore.info(`Copied SHA ${row.id.slice(0, 7)}`, undefined, 2000);
    setTimeout(() => {
      isCopied = false;
    }, 1500);
  }

  function openContextMenu(clientX: number, clientY: number) {
    const clamped = clampMenuPosition(
      clientX,
      clientY,
      200,
      280,
      window.innerWidth,
      window.innerHeight
    );
    menuPos = clamped;
    isMenuOpen = true;
  }

  function handleContextMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    openContextMenu(e.clientX, e.clientY);
  }

  function closeMenu() {
    isMenuOpen = false;
  }

  async function createBranchHere() {
    closeMenu();
    const name = await askText({
      title: "Create Branch at Commit",
      message: `Branch name from ${row.id.slice(0, 7)}:`,
      placeholder: "feat/new-branch",
      confirmLabel: "Create Branch",
    });
    if (name?.trim()) {
      await repoStore.createBranch(name.trim(), row.id);
      toastStore.success(`Created branch "${name.trim()}" at ${row.id.slice(0, 7)}`);
    }
  }

  function filterByAuthor() {
    closeMenu();
    if (row.author_name) {
      filterStore.setSearch(`author:${row.author_name}`);
    }
  }

  function handleMenuKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      closeMenu();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
      e.preventDefault();
      const target = e.currentTarget;
      if (target instanceof HTMLElement) {
        const bounds = target.getBoundingClientRect();
        openContextMenu(bounds.left + Math.min(bounds.width, 32), bounds.bottom);
      }
      return;
    }
    if (e.key === "Escape") {
      closeMenu();
      onBlurRow?.();
      return;
    }
    if (e.key !== "Enter" && e.key !== " ") return;
    e.preventDefault();
    onSelect?.();
  }

  function handleFocus(e: FocusEvent) {
    const el = e.currentTarget;
    if (!(el instanceof HTMLElement)) return;
    onFocusRow?.(el, isKeyboardFocus(el));
  }

  $effect(() => {
    if (!isMenuOpen) return;
    const focusTimer = setTimeout(() => {
      menuEl?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    }, 0);
    return () => clearTimeout(focusTimer);
  });

  let conventional = $derived(getConventionalType(row.summary || ""));
</script>

<svelte:window onclick={() => isMenuOpen && closeMenu()} />

<div
  role="button"
  tabindex="0"
  onclick={onSelect}
  onkeydown={handleKeydown}
  onfocus={handleFocus}
  onblur={() => onBlurRow?.()}
  oncontextmenu={handleContextMenu}
  aria-pressed={isSelected}
  aria-haspopup="menu"
  aria-expanded={isMenuOpen}
  class="{isCompact ? 'h-[26px] px-2.5 gap-2 text-[11px]' : 'h-9 px-3 gap-3 text-xs'} flex items-center cursor-pointer select-none transition-[color,background-color,border-color,box-shadow] duration-150 rounded-lg group {isSelected ? 'bg-accent/15 text-textPrimary font-medium ring-1 ring-inset ring-accent/35 shadow-sm' : 'hover:bg-surfaceHover/70 text-textPrimary/90'}"
>
  <!-- Short SHA with interactive Copy Button -->
  <button
    type="button"
    onclick={handleCopySha}
    class="font-mono text-accent/80 hover:text-accent shrink-0 tracking-tight flex items-center gap-1 group/sha {isCompact ? 'text-[10px] w-16' : 'text-[11px] w-20'}"
    title="Click to copy full SHA ({row.id})"
  >
    <span>{row.id.substring(0, 7)}</span>
    {#if isCopied}
      <Check size={11} class="text-emerald-500 shrink-0" />
    {:else}
      <Copy size={10} class="opacity-0 group-hover/sha:opacity-100 text-textMuted transition-opacity shrink-0" />
    {/if}
  </button>

  <!-- Commit Summary, Badges & Ref Pills -->
  <div class="flex-1 flex items-center gap-2 truncate">
    {#if conventional}
      <span class="inline-flex items-center text-[9px] font-semibold px-1.5 py-0.5 rounded-full border {conventional.color}">
        {conventional.type}
      </span>
    {/if}

    {#if row.is_merge}
      <span class="inline-flex items-center gap-1 text-[9px] px-1.5 py-0.5 bg-purple-500/20 text-purple-400 dark:text-purple-300 rounded-full border border-purple-500/30 font-medium">
        <GitMerge size={10} />
        merge
      </span>
    {/if}

    {#each refs as r}
      {#if r.kind === "head"}
        <span class="inline-flex items-center gap-1 text-[9px] font-mono font-bold px-1.5 py-0.5 rounded-full border border-accent bg-accent/25 text-accent shadow-sm">
          <Compass size={10} />
          HEAD
        </span>
      {:else if r.kind === "current-branch"}
        <span
          class="inline-flex items-center gap-1 text-[9px] font-mono font-semibold px-2 py-0.5 rounded-full border shadow-sm"
          style="border-color: {getBranchColor(row.color_index)}; color: {getBranchColor(row.color_index)};"
        >
          <GitBranch size={10} />
          {r.name}
        </span>
      {:else if r.kind === "remote-branch"}
        <span class="inline-flex items-center gap-1 text-[9px] font-mono px-1.5 py-0.5 rounded-full border border-sky-500/30 bg-sky-500/10 text-sky-600 dark:text-sky-400">
          <Cloud size={10} />
          {r.name}
        </span>
      {:else if r.kind === "tag"}
        <span class="inline-flex items-center gap-1 text-[9px] font-mono px-1.5 py-0.5 rounded-full border border-amber-500/30 bg-amber-500/10 text-amber-600 dark:text-amber-300">
          <Tag size={10} />
          {r.name}
        </span>
      {:else}
        <span
          class="inline-flex items-center gap-1 text-[9px] font-mono px-1.5 py-0.5 rounded-full border border-border/80 bg-surfaceHover text-textPrimary"
        >
          <GitBranch size={10} class="text-textMuted" />
          {r.name}
        </span>
      {/if}
    {/each}

    <span class="truncate select-text" title={row.summary || undefined}>{row.summary || "No commit message"}</span>

    {#if mergeTarget}
      <span class="sr-only">
        Merges into {mergeTarget.id.slice(0, 7)} — {mergeTarget.summary || "no commit message"}
      </span>
    {/if}
  </div>

  <!-- Author Name & Relative Date -->
  <span class="{isCompact ? 'text-[10px] w-24' : 'text-[11px] w-28'} text-textMuted shrink-0 truncate text-right font-medium select-text">
    {row.author_name}
  </span>
  <span class="{isCompact ? 'text-[10px] w-14' : 'text-[11px] w-16'} text-textMuted/70 shrink-0 text-right">
    {formatRelativeTime(row.timestamp)}
  </span>

  <!-- Author Initials Avatar -->
  <div
    class="{isCompact ? 'w-3.5 h-3.5 text-[8px]' : 'w-4.5 h-4.5 text-[10px]'} rounded-full flex items-center justify-center text-white font-bold shrink-0 shadow-sm ring-1 ring-background"
    style="background-color: {authorColor(avatar.hue)}"
    title="{row.author_name || 'Unknown'}{row.author_email ? ` <${row.author_email}>` : ''}"
  >
    {avatar.initials}
  </div>
</div>

<!-- Context Menu -->
{#if isMenuOpen}
  <div
    bind:this={menuEl}
    use:portal={"body"}
    class="fixed z-50 min-w-48 gp-menu gp-pop text-xs text-textPrimary focus:outline-none shadow-float"
    style="left: {menuPos.left}px; top: {menuPos.top}px; z-index: {LAYERS.MENU};"
    role="menu"
    aria-orientation="vertical"
    tabindex="-1"
    onkeydown={handleMenuKeydown}
  >
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={(e) => { closeMenu(); handleCopySha(e); }}
    >
      <Copy size={12} class="text-textMuted" />
      <span>Copy Full SHA</span>
    </button>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={async () => {
        closeMenu();
        if (row.summary) {
          await copyText(row.summary);
          toastStore.info("Copied commit message", undefined, 2000);
        }
      }}
    >
      <Copy size={12} class="text-textMuted" />
      <span>Copy Message</span>
    </button>
    <div class="h-px bg-border/60 my-1"></div>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={createBranchHere}
    >
      <Plus size={12} class="text-textMuted" />
      <span>Create Branch Here…</span>
    </button>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={() => { closeMenu(); repoStore.checkoutBranch(row.id); }}
    >
      <GitBranch size={12} class="text-textMuted" />
      <span>Checkout Commit</span>
    </button>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={() => { closeMenu(); void repoStore.cherryPick([row.id]); }}
    >
      <GitCommit size={12} class="text-textMuted" />
      <span>Cherry-pick onto current branch</span>
    </button>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={() => { closeMenu(); void repoStore.revertCommits([row.id]); }}
    >
      <Undo2 size={12} class="text-textMuted" />
      <span>Revert this commit</span>
    </button>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={filterByAuthor}
    >
      <Filter size={12} class="text-textMuted" />
      <span>Filter by this Author</span>
    </button>
  </div>
{/if}
