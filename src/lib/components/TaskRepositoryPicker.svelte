<script lang="ts">
  /**
   * The task sheet's repository control: one trigger that links repositories
   * and picks the primary among them.
   *
   * This used to be an inline 168px scroller sitting above the title, because
   * a task cannot be saved without a repository and the picker had to be the
   * first thing on the pane. That made the sheet open with a list box where
   * the title belongs, and pushed every other field down by a control most
   * edits never touch.
   *
   * Collapsing it costs nothing only if the closed trigger still answers the
   * question the list answered: how many are linked, and which is primary.
   * So the trigger's label *is* `summaryLine` — the same sentence the inline
   * summary showed. Linking and the primary choice stay one control that
   * cannot disagree with itself.
   *
   * Two deliberate choices about the widgets inside:
   *
   * * The **primary** control is a native radio group, for the same reason
   *   `gp-select` keeps a native `<select>` (`app.css`): one-of-many is what
   *   radios are, and they bring arrow-key navigation and the right screen
   *   reader role for free. Only the painting is ours.
   * * The popover is **not** a `listbox`. A listbox option carries one piece
   *   of state; these rows carry two independent ones — linked, and primary —
   *   so it is a group holding a checkbox list and a radio group.
   *
   * It is portaled. `.sheet-body` is `overflow:auto`, so a popover positioned
   * inside it would be clipped by the scroller it is trying to escape.
   */
  import { onMount } from "svelte";
  import { ChevronDown, FolderGit2 } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { clampMenuPosition } from "../branches/menuPosition";
  import { summaryLine, outsiderLine, type LinkSummary, type PickerRow } from "../workbench/taskRepositories";
  import type { OpenTabRef } from "../workbench/openMembership";

  let {
    /** Catalog rows, already filtered and ordered by `repositoryRows`. */
    rows,
    summary,
    /** Whether the catalog is large enough to be worth filtering. */
    offerFilter = false,
    filter = $bindable(""),
    /** How many repositories the profile knows about at all. */
    knownCount = 0,
    /** Radio group name; unique per mounted sheet. */
    name,
    addable = [],
    adding = false,
    attaching = false,
    /** The task's home workspace, or null when it has none. */
    workspace = null,
    disabled = false,
    onToggle,
    onPrimary,
    onAddPaths,
    onAttachOutsiders,
  }: {
    rows: PickerRow[];
    summary: LinkSummary;
    offerFilter?: boolean;
    filter?: string;
    knownCount?: number;
    name: string;
    addable?: OpenTabRef[];
    adding?: boolean;
    attaching?: boolean;
    workspace?: { name: string; error: string } | null;
    disabled?: boolean;
    onToggle: (id: string, checked: boolean) => void;
    onPrimary: (id: string) => void;
    onAddPaths: (paths: string[]) => void;
    onAttachOutsiders: () => void;
  } = $props();

  let open = $state(false);
  let triggerEl: HTMLButtonElement | undefined = $state();
  let popupEl: HTMLDivElement | undefined = $state();
  let pos = $state({ left: 0, top: 0 });

  const label = $derived(summaryLine(summary));

  function close(options?: { restoreFocus?: boolean }) {
    const opener = triggerEl;
    open = false;
    if (options?.restoreFocus && opener?.isConnected) window.setTimeout(() => opener.focus(), 0);
  }

  /**
   * Place the popup under the trigger, clamped into the viewport.
   *
   * Called once on open with an estimated size and again once the popup has
   * measured itself, because a first-frame guess that is too short opens a
   * tall list off the bottom edge.
   */
  function fit(estimate?: { width: number; height: number }) {
    if (!triggerEl) return;
    const rect = triggerEl.getBoundingClientRect();
    const width = popupEl?.offsetWidth || estimate?.width || Math.max(rect.width, 300);
    const height = popupEl?.offsetHeight || estimate?.height || 320;
    pos = clampMenuPosition(rect.left, rect.bottom + 6, width, height, window.innerWidth, window.innerHeight);
  }

  function toggleOpen() {
    if (open) { close(); return; }
    fit({ width: Math.max(triggerEl?.getBoundingClientRect().width ?? 0, 300), height: 320 });
    open = true;
  }

  $effect(() => { if (open && popupEl) fit(); });
  $effect(() => {
    // A disabled trigger with an open popup is a control that cannot be
    // dismissed by the thing that opened it.
    if (disabled && open) close();
  });

  onMount(() => {
    const onPointer = (event: PointerEvent) => {
      if (!open) return;
      if (shouldDismissOverlay(event.target, "[data-task-repo-picker], [data-task-repo-popup]")) close();
    };
    const onKey = (event: KeyboardEvent) => {
      if (!open) return;
      if (event.key === "Escape") {
        // The sheet closes on Escape too, and this listener is on the capture
        // phase, so stopping here is what keeps one Escape from dismissing the
        // popover *and* the task behind it.
        event.preventDefault();
        event.stopPropagation();
        close({ restoreFocus: true });
        return;
      }
      // Tab off either end of the popup closes it rather than leaving a
      // floating panel behind the reader's focus. Handled here rather than
      // with a listener on the popup itself: a `role="group"` div is not an
      // interactive element, and one keyboard owner beats two.
      if (event.key === "Tab" && popupEl && event.target instanceof Node && popupEl.contains(event.target)) {
        const focusables = popupEl.querySelectorAll<HTMLElement>("input:not(:disabled), button:not(:disabled)");
        if (!focusables.length) return;
        const edge = event.shiftKey ? focusables[0] : focusables[focusables.length - 1];
        if (document.activeElement === edge) close();
      }
    };
    // Capture, because scroll does not bubble: this is how the sheet's own
    // scroller reaches us. A scroll *inside* the popup is the reader reading
    // the list, not leaving it, so only outside scrolls dismiss.
    const onScroll = (event: Event) => {
      if (!open) return;
      if (event.target instanceof Node && popupEl?.contains(event.target)) return;
      close();
    };
    const onResize = () => close();
    window.addEventListener("pointerdown", onPointer, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
    };
  });
</script>

<fieldset class="repositories" data-task-repo-picker>
  <legend>Repositories</legend>
  <button
    bind:this={triggerEl}
    type="button"
    class="gp-btn repo-trigger"
    class:needs={summary.linked === 0}
    aria-haspopup="dialog"
    aria-expanded={open}
    {disabled}
    data-testid="task-repo-summary"
    title="Link the repositories this task touches, and choose the primary one"
    onclick={toggleOpen}
  >
    <FolderGit2 size={12} class="shrink-0 text-accent" aria-hidden="true" />
    <span class="repo-trigger-label">{label}</span>
    <ChevronDown size={11} class="shrink-0 text-textMuted" aria-hidden="true" />
  </button>
  {#each summary.unknown as missing (missing)}
    <small class="repo-unknown">Linked repository {missing} (load more repositories to edit)</small>
  {/each}
  {#if workspace}
    {#if workspace.error}
      <p class="repo-note" role="status">Could not read {workspace.name}'s repositories: {workspace.error}</p>
    {:else if summary.outsiders.length}
      <p class="repo-note">
        {outsiderLine(summary.outsiders, workspace.name)}
        <button type="button" class="gp-btn" disabled={attaching || adding || disabled} onclick={onAttachOutsiders}>{attaching ? "Adding…" : "Add to workspace"}</button>
      </p>
    {/if}
  {/if}
</fieldset>

{#if open}
  <div
    bind:this={popupEl}
    use:portal={"body"}
    data-task-repo-popup
    data-testid="task-repo-popup"
    class="fixed gp-menu gp-pop repo-popup"
    style="left: {pos.left}px; top: {pos.top}px; z-index: {LAYERS.MENU}"
    role="group"
    aria-label="Repositories for this task"
  >
    <p class="repo-summary" class:needs={summary.linked === 0}>{label}</p>
    {#if offerFilter}
      <input
        class="gp-field repo-filter"
        type="search"
        bind:value={filter}
        aria-label="Filter repositories"
        placeholder="Filter repositories"
        maxlength="200"
        oninput={(e) => e.stopPropagation()}
      />
    {/if}
    <div class="repo-list" role="radiogroup" aria-label="Primary repository">
      {#each rows as row (row.id)}
        <div class="repo-row" class:linked={row.linked}>
          <label class="check">
            <input type="checkbox" checked={row.linked} onchange={(e) => onToggle(row.id, e.currentTarget.checked)} />
            <span class="repo-name">{row.name}</span>
            {#if row.member}<span class="repo-mark">In workspace</span>{/if}
            {#if row.keptByLink}<span class="repo-mark">Linked</span>{/if}
          </label>
          {#if row.linked}
            <label class="primary-pick">
              <input
                type="radio"
                name="task-primary-{name}"
                checked={row.primary}
                onchange={() => onPrimary(row.id)}
                aria-label="Make {row.name} the primary repository"
              />
              Primary
            </label>
          {/if}
        </div>
      {/each}
      {#if knownCount === 0}
        <p class="repo-empty">No repositories are registered yet. Open one in GitPulse, then link it here.</p>
      {:else if rows.length === 0}
        <p class="repo-empty">No repository matches this filter.</p>
      {/if}
    </div>
    {#if addable.length}
      <small>Open in GitPulse</small>
      {#each addable as openTab (openTab.path)}
        <label class="check" title={openTab.path}>
          <input type="checkbox" checked={false} disabled={adding} onchange={(e) => { e.currentTarget.checked = false; onAddPaths([openTab.path]); }} />
          {openTab.label}<span class="open-mark">Open</span>
        </label>
      {/each}
      {#if addable.length > 1}<button type="button" class="gp-btn" disabled={adding} onclick={() => onAddPaths(addable.map((item) => item.path))}>Add all open</button>{/if}
    {/if}
  </div>
{/if}

<style>
  .repositories{border:0;padding:0;min-width:0;margin:0 0 13px}
  legend{color:rgb(var(--c-text-muted));font-size:11px}
  .repo-trigger{width:100%;justify-content:flex-start;gap:7px;margin-top:4px;text-align:left}
  .repo-trigger-label{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  /* No repository linked is not a neutral state — it is the one thing that
     stops the task saving, so the trigger says so at full contrast. */
  .repo-trigger.needs .repo-trigger-label{color:rgb(var(--c-text))}
  .repo-unknown{display:block;color:rgb(var(--c-text-muted));font-size:11px;margin-top:4px}
  .repo-note{display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin:8px 0 0;font-size:12px;color:rgb(var(--c-text-muted))}
  .repo-popup{width:min(360px,92vw);padding:10px;font-size:12px;color:rgb(var(--c-text))}
  .repo-summary{margin:0 0 6px;font-size:11px;color:rgb(var(--c-text-muted))}
  .repo-summary.needs{color:rgb(var(--c-text))}
  .repo-filter{margin-bottom:6px;width:100%}
  /* Only the rows scroll. The summary and the filter above them are how the
     picker stays readable at any catalog size, so they must not scroll away. */
  .repo-list{max-height:220px;overflow:auto;margin:2px 0 4px}
  .repo-row{display:flex;align-items:center;gap:8px;justify-content:space-between}
  .repo-row .check{flex:1;min-width:0;gap:7px}
  .repo-name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .repo-mark{flex-shrink:0;color:rgb(var(--c-text-muted));font-size:10px}
  .primary-pick{display:flex;flex-direction:row;align-items:center;gap:4px;flex:0 0 auto;margin:0;font-size:10px;color:rgb(var(--c-text-muted))}
  .primary-pick input{width:auto}
  .repo-row.linked .primary-pick{color:rgb(var(--c-text))}
  .repo-empty{margin:6px 0;color:rgb(var(--c-text-muted))}
  .check{display:flex;flex-direction:row;align-items:center;margin:5px 0;gap:7px}
  .check input{width:auto}
  .open-mark{color:rgb(var(--c-text-muted));font-size:10px;margin-left:6px}
  small{color:rgb(var(--c-text-muted));font-size:11px}
  input[type="search"]{padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}
</style>
