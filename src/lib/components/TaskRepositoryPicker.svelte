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
   * question the list answered: how many are linked, and which is primary. It
   * answers with a chip per link, the primary one starred and drawn first —
   * naming them rather than counting them, because "2 repositories linked"
   * still needs opening to find out *which*. `triggerChips` guarantees the
   * primary is never the chip that gets collapsed into the overflow count, and
   * `summaryLine` stays on screen beneath for what chips cannot say: a
   * remainder, or a link with no primary chosen yet.
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
  import { ChevronDown, FolderGit2, Star } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { popover, restoreFocusTo } from "../ui/popover";
  import { groupRows, summaryLine, outsiderLine, type LinkSummary, type PickerRow, type TriggerChip } from "../workbench/taskRepositories";
  import type { OpenTabRef } from "../workbench/openMembership";

  let {
    /** Catalog rows, already filtered and ordered by `repositoryRows`. */
    rows,
    summary,
    /** What the closed trigger draws, from `triggerChips`. */
    chips = [],
    overflow = 0,
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
    chips?: TriggerChip[];
    overflow?: number;
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

  const label = $derived(summaryLine(summary));
  const groups = $derived(groupRows(rows));

  function close(options?: { restoreFocus?: boolean }) {
    const opener = triggerEl;
    open = false;
    if (options?.restoreFocus) restoreFocusTo(opener);
  }

  function toggleOpen() {
    open = !open;
  }

  $effect(() => {
    // A disabled trigger with an open popup is a control that cannot be
    // dismissed by the thing that opened it.
    if (disabled && open) close();
  });

  /**
   * Placement and dismissal, from the shared popover owner.
   *
   * `scroll` is why this popup is portaled and why the listener has to be on
   * the capture phase: `.sheet-body` is the scroller that moves the trigger
   * out from under the popup, and scroll does not bubble. A scroll *inside*
   * the popup is the reader reading the list, so the owner judges that one by
   * containment rather than by the selector below.
   *
   * Escape is `capture` because the task sheet closes on Escape too; stopping
   * propagation there is what keeps one Escape from dismissing the popover
   * *and* the task behind it. `revision` re-measures when the filter changes
   * how tall the list is.
   */
  const dismissal = $derived({
    anchor: { kind: "element" as const, element: triggerEl, gap: 6 },
    estimate: { width: 300, height: 320 },
    revision: rows.length,
    dismiss: {
      inside: "[data-task-repo-picker], [data-task-repo-popup]",
      scroll: true,
      resize: true,
      escape: "capture" as const,
    },
    onDismiss: (reason: string) => close({ restoreFocus: reason === "escape" }),
  });

  onMount(() => {
    /**
     * Tab off either end of the popup closes it rather than leaving a
     * floating panel behind the reader's focus. This stays here rather than
     * moving to the popover owner: it is the only surface with an *edge*
     * check — the other two menus that close on Tab close on any Tab — and
     * the owner deliberately does not reach into focus. Capture, and on
     * window rather than the popup, because a `role="group"` div is not an
     * interactive element.
     */
    const onKey = (event: KeyboardEvent) => {
      if (!open || event.key !== "Tab") return;
      if (!popupEl || !(event.target instanceof Node) || !popupEl.contains(event.target)) return;
      const focusables = popupEl.querySelectorAll<HTMLElement>("input:not(:disabled), button:not(:disabled)");
      if (!focusables.length) return;
      const edge = event.shiftKey ? focusables[0] : focusables[focusables.length - 1];
      if (document.activeElement === edge) close();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
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
    <span class="repo-chips">
      {#each chips as chip (chip.id)}
        <span class="gp-chip repo-chip" class:is-primary={chip.primary}>
          {#if chip.primary}<Star size={9} fill="currentColor" class="shrink-0" aria-hidden="true" />{/if}
          <span class="repo-chip-name">{chip.name}</span>
        </span>
      {/each}
      {#if overflow > 0}<span class="repo-more">+{overflow}</span>{/if}
      {#if summary.linked === 0}<span class="repo-trigger-label">{label}</span>{/if}
    </span>
    <ChevronDown size={11} class="shrink-0 text-textMuted" aria-hidden="true" />
  </button>
  <!-- `summaryLine` is still the single owner of "how many are linked, and
       which is primary". The chips above answer it at a glance; this keeps the
       exact sentence on screen for the cases chips cannot carry — a collapsed
       remainder, or a link with no primary chosen. -->
  {#if summary.linked > 0}
    <p class="repo-summary-line" data-testid="task-repo-summary-line">{label}</p>
  {/if}
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
    use:popover={dismissal}
    data-task-repo-popup
    data-testid="task-repo-popup"
    class="fixed gp-menu gp-pop repo-popup"
    style="z-index: {LAYERS.MENU}"
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
      <!-- Grouped rather than flat: a reader looking for the two they linked
           should not have to read every name in the catalog. `groupRows` keeps
           catalog order inside each group, so a row still never moves out from
           under the pointer that just checked it. -->
      {#each groups as group (group.id)}
        <p class="repo-group">{group.label}</p>
        {#each group.rows as row (row.id)}
          <div class="repo-row" class:linked={row.linked}>
            <label class="check">
              <input type="checkbox" checked={row.linked} onchange={(e) => onToggle(row.id, e.currentTarget.checked)} />
              <span class="repo-name">{row.name}</span>
              <!-- Only under "Linked", where the group heading does not
                   already say it. A linked repository that is *not* a member
                   is what the outsider notice under the trigger is about, so
                   the reader needs to be able to tell the two apart here. -->
              {#if row.linked && row.member}<span class="repo-mark">In workspace</span>{/if}
              {#if row.keptByLink}<span class="repo-mark">Filtered out</span>{/if}
            </label>
            {#if row.linked}
              <label class="primary-pick" class:is-primary={row.primary}>
                <input
                  type="radio"
                  name="task-primary-{name}"
                  checked={row.primary}
                  onchange={() => onPrimary(row.id)}
                  aria-label="Make {row.name} the primary repository"
                />
                <Star size={9} fill={row.primary ? "currentColor" : "none"} class="shrink-0" aria-hidden="true" />
                Primary
              </label>
            {/if}
          </div>
        {/each}
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
  /* A field, not a pill: it holds chips and sits in a column of inputs, so it
     takes the same geometry as the fields around it. */
  .repo-trigger{width:100%;justify-content:flex-start;gap:7px;margin-top:4px;text-align:left;border-radius:12px;padding:6px 9px;min-height:38px;background:rgb(var(--c-bg) / 0.6);border-color:rgb(var(--c-border))}
  .repo-chips{display:flex;flex-wrap:wrap;gap:5px;flex:1;min-width:0;align-items:center}
  .repo-chip{background:rgb(var(--c-surface-hover) / 0.8);max-width:100%}
  .repo-chip-name{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  /* The primary is the one that decides where an agent runs, so it is the one
     chip that is coloured rather than merely present. */
  .repo-chip.is-primary{border-color:rgb(var(--c-accent) / 0.55);background:rgb(var(--c-accent) / 0.13);color:rgb(var(--c-text))}
  .repo-more{flex-shrink:0;color:rgb(var(--c-text-muted));font-size:11px}
  .repo-trigger-label{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  /* No repository linked is not a neutral state — it is the one thing that
     stops the task saving, so the trigger says so at full contrast. */
  .repo-trigger.needs{border-color:rgb(var(--c-accent) / 0.5)}
  .repo-trigger.needs .repo-trigger-label{color:rgb(var(--c-text))}
  .repo-summary-line{margin:5px 0 0;font-size:11px;color:rgb(var(--c-text-muted))}
  .repo-group{margin:7px 0 2px;padding:0 6px;font-size:10px;letter-spacing:.05em;text-transform:uppercase;color:rgb(var(--c-text-muted))}
  .repo-group:first-child{margin-top:2px}
  .repo-unknown{display:block;color:rgb(var(--c-text-muted));font-size:11px;margin-top:4px}
  .repo-note{display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin:8px 0 0;font-size:12px;color:rgb(var(--c-text-muted))}
  .repo-popup{width:min(360px,92vw);padding:10px;font-size:12px;color:rgb(var(--c-text))}
  .repo-summary{margin:0 0 6px;font-size:11px;color:rgb(var(--c-text-muted))}
  .repo-summary.needs{color:rgb(var(--c-text))}
  .repo-filter{margin-bottom:6px;width:100%}
  /* Only the rows scroll. The summary and the filter above them are how the
     picker stays readable at any catalog size, so they must not scroll away. */
  .repo-list{max-height:220px;overflow:auto;margin:2px 0 4px}
  .repo-row{display:flex;align-items:center;gap:8px;justify-content:space-between;border-radius:8px;padding:3px 6px}
  .repo-row:hover{background:rgb(var(--c-surface-hover) / 0.6)}
  .repo-row.linked{background:rgb(var(--c-accent) / 0.08)}
  .repo-row .check{flex:1;min-width:0;gap:7px}
  .repo-name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .repo-mark{flex-shrink:0;color:rgb(var(--c-text-muted));font-size:10px}
  .primary-pick{display:flex;flex-direction:row;align-items:center;gap:4px;flex:0 0 auto;margin:0;font-size:10px;color:rgb(var(--c-text-muted));border:1px solid transparent;border-radius:999px;padding:1px 7px;cursor:pointer}
  .primary-pick:hover{border-color:rgb(var(--c-border))}
  .primary-pick input{width:auto}
  .repo-row.linked .primary-pick{color:rgb(var(--c-text))}
  /* The radio stays the control — it is what makes this one-of-many for the
     keyboard and for a screen reader. The star is the painting beside it, the
     same trade `gp-select` makes with a native <select>. */
  .primary-pick.is-primary{color:rgb(var(--c-accent));border-color:rgb(var(--c-accent) / 0.45);background:rgb(var(--c-accent) / 0.12)}
  .repo-empty{margin:6px 0;color:rgb(var(--c-text-muted))}
  .check{display:flex;flex-direction:row;align-items:center;margin:5px 0;gap:7px}
  .check input{width:auto}
  .open-mark{color:rgb(var(--c-text-muted));font-size:10px;margin-left:6px}
  small{color:rgb(var(--c-text-muted));font-size:11px}
  input[type="search"]{padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}
</style>
