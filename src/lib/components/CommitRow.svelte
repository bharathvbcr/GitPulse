<script lang="ts">
  import type { VisualCommitRow } from "../canvas/GraphRenderer";
  import { getBranchColor } from "../canvas/Palette";
  import {
    formatTimestamp,
    timestampTitle,
    type TimestampStyle,
  } from "../ui/timestampStyle";
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
    Hash,
  } from "@lucide/svelte";
  import { copyText } from "../desktop/clipboard";
  import { toastStore } from "../stores/toastStore";
  import { reportFailure } from "../stores/mutationOutcome";
  import { repoStore } from "../stores/repoStore";
  import { filterStore } from "../stores/filterStore";
  import { askText } from "../stores/modalStore";
  import { shortRefLabel } from "../graph/refScope";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { popover, popoverExit } from "../ui/popover";

  import type { RefItem } from "../graph/graphDecorations";

  let {
    row,
    isSelected = false,
    density = "spacious",
    timestampStyle = "relative",
    refs = [],
    onSelect,
    mergeTarget = null,
    onFocusRow,
    onBlurRow,
  }: {
    row: VisualCommitRow;
    isSelected?: boolean;
    density?: "spacious" | "compact";
    /**
     * Relative ("3d ago") or absolute ("2026-09-06"). A prop rather than a
     * store read: this component renders once per visible row, and it already
     * takes `density` — the other preference that changes its geometry — the
     * same way.
     */
    timestampStyle?: TimestampStyle;
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
  let menuAnchor = $state<{ x: number; y: number }>({ x: 0, y: 0 });
  let menuEl = $state<HTMLDivElement | undefined>();

  /**
   * Placement and dismissal, from the shared popover owner.
   *
   * `click` rather than `pointerdown`, on the bubble phase, because that is
   * what this menu has always used — and every item closes the menu itself
   * before acting, so an outside-only click is the same dismissal the old
   * `<svelte:window onclick>` provided without also firing on the way through
   * the menu. The estimate is the size the menu used to be *assumed* to have;
   * the owner now measures the real one and re-clamps, so a menu whose items
   * make it taller than 280px can no longer paint off the bottom edge.
   *
   * Escape stays with `handleMenuKeydown`, which the menu element owns.
   */
  const dismissal = $derived({
    anchor: { kind: "point" as const, x: menuAnchor.x, y: menuAnchor.y },
    estimate: { width: 200, height: 280 },
    dismiss: { pointer: "click" as const, escape: "none" as const },
    onDismiss: closeMenu,
  });

  const isCompact = $derived(density === "compact");
  const isAbsoluteTime = $derived(timestampStyle === "absolute");
  const avatar = $derived(authorIdentity(row.author_name, row.author_email));

  type ChipTone = "neutral" | "accent" | "ok" | "warn" | "danger" | "info" | "unknown";

  /** The role tone a conventional-commit type wears. The word says what it is; the tone only groups it. */
  const CONVENTIONAL_TONES: ReadonlyMap<string, ChipTone> = new Map<string, ChipTone>([
    ["feat", "ok"],
    ["fix", "danger"],
    ["refactor", "accent"],
    ["docs", "info"],
    ["chore", "neutral"],
    ["perf", "warn"],
    ["test", "warn"],
    ["build", "info"],
    ["ci", "info"],
  ]);

  function getConventionalType(msg: string): { type: string; tone: ChipTone } | null {
    const match = msg.match(/^([a-zA-Z]+)(\([^\)]+\))?(!)?:\s/);
    if (!match) return null;
    const type = match[1].toLowerCase();
    return { type, tone: CONVENTIONAL_TONES.get(type) ?? "neutral" };
  }

  async function handleCopySha(e: MouseEvent) {
    e.stopPropagation();
    if (!(await copyText(row.id))) {
      toastStore.error("Couldn't copy the commit SHA.");
      return;
    }
    isCopied = true;
    toastStore.info(`Copied SHA ${row.id.slice(0, 7)}`, undefined, 2000);
    setTimeout(() => {
      isCopied = false;
    }, 1500);
  }

  function openContextMenu(clientX: number, clientY: number) {
    menuAnchor = { x: clientX, y: clientY };
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
    // The repository this commit belongs to, held across the prompt.
    const target = repoStore.captureTarget();
    const name = await askText({
      title: "Create branch at commit",
      message: `Branch name from ${row.id.slice(0, 7)}:`,
      placeholder: "feat/new-branch",
      confirmLabel: "Create branch",
    });
    if (name?.trim()) {
      const outcome = await repoStore.createBranch(name.trim(), row.id, target);
      if (!reportFailure(outcome, "Couldn't create the branch")) return;
      toastStore.success(`Created branch "${name.trim()}" at ${row.id.slice(0, 7)}`);
    }
  }

  async function checkoutHere() {
    closeMenu();
    const outcome = await repoStore.checkoutDetached(row.id);
    reportFailure(outcome, "Couldn't check out the commit");
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
  class="{isCompact ? 'h-[26px] px-2.5 gap-2 text-meta' : 'h-9 px-3 gap-3 text-xs'} flex items-center cursor-pointer select-none transition-colors rounded-lg group {isSelected ? 'bg-accent/15 text-textPrimary font-medium ring-1 ring-inset ring-accent/35' : 'hover:bg-surfaceHover text-textPrimary'}"
>
  <!-- Short SHA with interactive Copy Button -->
  <button
    type="button"
    onclick={handleCopySha}
    class="font-mono text-accent hover:underline focus-visible:underline shrink-0 tracking-tight flex items-center gap-1 group/sha {isCompact ? 'text-micro w-16' : 'text-meta w-20'}"
    title="Click to copy full SHA ({row.id})"
    aria-label="Copy full SHA {row.id}"
  >
    <span>{row.id.substring(0, 7)}</span>
    {#if isCopied}
      <Check size={11} class="text-ok shrink-0" />
    {:else}
      <Copy size={10} class="opacity-0 group-hover/sha:opacity-100 group-focus-within/sha:opacity-100 text-textMuted transition-opacity shrink-0" />
    {/if}
  </button>

  <!-- Commit Summary, Badges & Ref Pills -->
  <div class="flex-1 flex items-center gap-2 truncate">
    {#if conventional}
      <span class="gp-chip font-semibold" data-tone={conventional.tone}>
        {conventional.type}
      </span>
    {/if}

    {#if row.is_merge}
      <span class="gp-chip font-medium" data-tone="accent">
        <GitMerge size={10} />
        merge
      </span>
    {/if}

    {#each refs as r}
      {#if r.kind === "head"}
        <span class="gp-chip font-mono font-semibold" data-tone="accent">
          <Compass size={10} />
          HEAD
        </span>
      {:else if r.kind === "current-branch"}
        <!-- The branch colour is the graph's own lane colour (canvas palette),
             so it is handed through as a style, not mapped to a role tone. -->
        <span
          class="gp-chip font-mono font-semibold"
          style="border-color: {getBranchColor(row.color_index)}; color: {getBranchColor(row.color_index)};"
        >
          <GitBranch size={10} />
          {r.name}
        </span>
      {:else if r.kind === "remote-branch"}
        <span class="gp-chip font-mono" data-tone="info">
          <Cloud size={10} />
          {r.name}
        </span>
      {:else if r.kind === "tag"}
        <span
          class="gp-chip font-mono"
          data-tone="warn"
          title={r.comparedTo
            ? r.aheadOfBase && r.aheadOfBase > 0
              ? `Tag ${r.name} — ${r.aheadOfBase} commits not in ${r.comparedTo}`
              : `Tag ${r.name} — every commit is already in ${r.comparedTo}`
            : `Tag ${r.name}`}
        >
          <Tag size={10} />
          {r.name}
          {#if r.comparedTo && r.aheadOfBase && r.aheadOfBase > 0}
            <span class="opacity-80">+{r.aheadOfBase}</span>
          {/if}
        </span>
      {:else if r.kind === "other"}
        <!--
          A ref outside branches, remotes and tags, drawn only under the "all
          refs" scope. Muted and marked with its full path so it reads as
          "some other ref" rather than as a branch: an agent-harness
          checkpoint wearing a branch chip is exactly the mislabelling that
          made these lanes unreadable.
        -->
        <span
          class="gp-chip font-mono"
          data-tone="neutral"
          title="refs/{r.name} — outside branches, remotes and tags"
        >
          <Hash size={10} />
          {shortRefLabel(r.name)}
        </span>
      {:else}
        <span class="gp-chip font-mono text-textPrimary" data-tone="neutral">
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
  <span class="{isCompact ? 'text-micro w-24' : 'text-meta w-28'} text-textMuted shrink-0 truncate text-right font-medium select-text">
    {row.author_name}
  </span>
  <!-- Widened for the absolute style: "2026-09-06" does not fit the column a
       relative "3d ago" needs, and a truncated date is a wrong date. The
       title carries whichever form is not on screen. -->
  <span
    class="{isCompact
      ? isAbsoluteTime
        ? 'text-micro w-[4.6rem]'
        : 'text-micro w-14'
      : isAbsoluteTime
        ? 'text-meta w-[5.2rem]'
        : 'text-meta w-16'} text-textMuted shrink-0 text-right"
    title={timestampTitle(row.timestamp, timestampStyle)}
  >
    {formatTimestamp(row.timestamp, timestampStyle)}
  </span>

  <!-- Author Initials Avatar -->
  <div
    class="{isCompact ? 'w-4 h-4' : 'w-5 h-5'} text-micro rounded-full flex items-center justify-center text-white font-semibold shrink-0 ring-1 ring-background"
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
    use:popover={dismissal} out:popoverExit
    class="fixed min-w-48 gp-menu gp-pop text-xs text-textPrimary focus:outline-hidden shadow-float"
    style="z-index: {LAYERS.MENU};"
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
      <span>Copy full SHA</span>
    </button>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={async () => {
        closeMenu();
        if (row.summary) {
          if (await copyText(row.summary)) {
            toastStore.info("Copied commit message", undefined, 2000);
          } else {
            toastStore.error("Couldn't copy the commit message.");
          }
        }
      }}
    >
      <Copy size={12} class="text-textMuted" />
      <span>Copy message</span>
    </button>
    <div class="gp-menu-sep" role="separator"></div>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={createBranchHere}
    >
      <Plus size={12} class="text-textMuted" />
      <span>Create branch here…</span>
    </button>
    <button
      role="menuitem"
      class="gp-menu-item"
      onclick={() => void checkoutHere()}
    >
      <GitBranch size={12} class="text-textMuted" />
      <span>Check out commit</span>
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
      <span>Filter by this author</span>
    </button>
  </div>
{/if}
