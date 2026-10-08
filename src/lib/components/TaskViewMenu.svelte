<script lang="ts">
  /**
   * The Tasks board's "View" popover: layout, density, which columns are on
   * screen and which chips a card carries.
   *
   * Follows the Fleet grid's Columns menu deliberately — same `gp-menu` shell,
   * same `role="switch"` rows, same "Show all" escape hatch — because these
   * are the same kind of choice and a reader should not have to learn two.
   */
  import { Settings2, Square, SquareCheck } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { popover } from "../ui/popover";
  import { LAYERS } from "../ui/layers";
  import { STATUSES, STATUS_LABELS } from "../workbench/vocabulary";
  import { TASK_CARD_FIELDS, TASK_CARD_FIELD_LABELS, canHideStatus } from "../ui/taskView";
  import { MAX_WIP_LIMIT, SWIMLANES, SWIMLANE_LABELS, boardPrefs, effectiveSwimlane, sanitizeWipLimit } from "../ui/taskBoardViews";

  let { disabled = false, board, boardName }: {
    disabled?: boolean;
    /** `boardKey` of the board on screen: limits belong to one board, not to every board. */
    board: string;
    boardName: string;
  } = $props();
  const limits = $derived(boardPrefs($interfaceStore.taskBoards, board).wip);
  const lane = $derived($interfaceStore.taskSwimlane);
  const laneIgnored = $derived(lane !== "none" && effectiveSwimlane($interfaceStore.taskLayout, lane) === "none");

  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();

  const hidden = $derived(new Set($interfaceStore.taskHiddenColumns));
  const fields = $derived(new Set($interfaceStore.taskCardFields));
  const compact = $derived($interfaceStore.taskDensity === "compact");
  // Layout is not listed here: the header segmented control owns it. It still
  // counts as customization, so "Reset board view" can put it back.
  const customized = $derived(
    hidden.size > 0 ||
      compact ||
      fields.size !== TASK_CARD_FIELDS.length ||
      $interfaceStore.taskLayout !== "board" ||
      lane !== "none",
  );

  /**
   * Dismissal only, from the shared popover owner: this panel is placed by
   * CSS, `absolute` under its own trigger inside a `relative` wrapper, so it
   * has no anchor to clamp and the owner leaves its position alone.
   *
   * Escape bubbles rather than capturing — nothing behind the board menu also
   * closes on Escape, so there is nothing to stop propagation from.
   */
  const dismissal = {
    dismiss: { inside: "[data-task-view-menu]", escape: "bubble" as const },
    onDismiss: (reason: string) => {
      open = false;
      // Only Escape hands focus back. A pointer dismissal means the reader is
      // already somewhere else, and pulling focus to the trigger would take
      // them off whatever they just clicked.
      if (reason === "escape") root?.querySelector<HTMLButtonElement>("[data-task-view-toggle]")?.focus();
    },
  };
</script>

<div class="relative" bind:this={root} data-task-view-menu>
  <button
    type="button"
    class="gp-btn"
    data-task-view-toggle
    aria-expanded={open}
    aria-haspopup="true"
    {disabled}
    onclick={() => { open = !open; }}
    title="Choose the board layout, how tightly it packs, which columns it shows and what each card carries."
  >
    <Settings2 size={12} />
    <span>View</span>
    {#if customized}<span class="tabular-nums text-textMuted">{STATUSES.length - hidden.size}/{STATUSES.length}</span>{/if}
  </button>
  {#if open}
    <div
      use:popover={dismissal}
      class="view-panel absolute right-0 top-full mt-1 gp-menu gp-pop p-2 w-56 flex flex-col gap-0.5"
      style="z-index: {LAYERS.MENU}"
      role="group"
      aria-label="Board view"
      data-testid="task-view-menu"
    >
      <button
        type="button"
        class="row"
        role="switch"
        aria-checked={compact}
        onclick={() => interfaceStore.setTaskDensity(compact ? "comfortable" : "compact")}
      >
        {@render mark(compact)}<span class="flex-1 text-textPrimary">Compact cards</span>
      </button>

      <p class="px-1.5 pt-2 pb-1 text-[10px] uppercase tracking-wide text-textMuted" id="task-lanes-label">Lanes</p>
      <div role="radiogroup" aria-labelledby="task-lanes-label" class="flex flex-col gap-0.5">
        {#each SWIMLANES as option (option)}
          <button
            type="button"
            class="row"
            role="radio"
            aria-checked={lane === option}
            data-task-lane-option={option}
            onclick={() => interfaceStore.setTaskSwimlane(option)}
          >
            {@render mark(lane === option)}<span class="flex-1 text-textPrimary">{SWIMLANE_LABELS[option]}{option === "label" ? " (first label)" : ""}</span>
          </button>
        {/each}
      </div>
      {#if laneIgnored}<p class="note" data-testid="task-lanes-note">The board's columns are already statuses, so status lanes group the List layout only.</p>{/if}
      {#if lane !== "none" && !laneIgnored}<p class="note">Dragging a card into another lane changes its status only.</p>{/if}

      <p class="px-1.5 pt-2 pb-1 text-[10px] uppercase tracking-wide text-textMuted">Columns</p>
      {#each STATUSES as status (status)}
        {@const hideable = canHideStatus($interfaceStore.taskHiddenColumns, status)}
        <button
          type="button"
          class="row"
          role="switch"
          aria-checked={!hidden.has(status)}
          data-task-column-toggle={status}
          disabled={!hideable}
          title={hideable ? "" : "The board always keeps one column — it is where a new task goes."}
          onclick={() => interfaceStore.toggleTaskColumn(status)}
        >
          {@render mark(!hidden.has(status))}<span class="flex-1 text-textPrimary">{STATUS_LABELS[status]}</span>
        </button>
      {/each}

      <p class="px-1.5 pt-2 pb-1 text-[10px] uppercase tracking-wide text-textMuted">Work-in-progress limits</p>
      <p class="note">For {boardName} only. A column over its limit is marked; nothing is refused.</p>
      {#each STATUSES as status (status)}
        <label class="limit">
          <span class="flex-1 text-textPrimary">{STATUS_LABELS[status]}</span>
          <input
            class="gp-field"
            type="number"
            inputmode="numeric"
            min="1"
            max={MAX_WIP_LIMIT}
            placeholder="None"
            data-task-wip-input={status}
            aria-label={`Work-in-progress limit for ${STATUS_LABELS[status]}`}
            value={limits[status] ?? ""}
            onchange={(e) => {
              const raw = e.currentTarget.value;
              const limit = sanitizeWipLimit(raw);
              interfaceStore.setTaskWipLimit(board, status, limit);
              // A value the limit cannot hold is put back to what is stored,
              // so the box never shows a limit the board is not applying.
              e.currentTarget.value = limit === null ? "" : String(limit);
            }}
          />
        </label>
      {/each}

      <p class="px-1.5 pt-2 pb-1 text-[10px] uppercase tracking-wide text-textMuted">Card shows</p>
      {#each TASK_CARD_FIELDS as field (field)}
        <button
          type="button"
          class="row"
          role="switch"
          aria-checked={fields.has(field)}
          data-task-field-toggle={field}
          onclick={() => interfaceStore.toggleTaskCardField(field)}
        >
          {@render mark(fields.has(field))}<span class="flex-1 text-textPrimary">{TASK_CARD_FIELD_LABELS[field]}</span>
        </button>
      {/each}

      <div class="border-t border-border/60 mt-1 pt-1">
        <button
          type="button"
          class="reset"
          disabled={!customized}
          onclick={() => interfaceStore.resetTaskView()}
        >Reset board view</button>
      </div>
    </div>
  {/if}
</div>

{#snippet mark(on: boolean)}
  {#if on}<SquareCheck size={11} class="text-accent shrink-0" />{:else}<Square size={11} class="text-textMuted shrink-0" />{/if}
{/snippet}

<style>
  .row{display:flex;align-items:center;gap:8px;padding:4px 6px;border-radius:5px;font-size:11px;text-align:left;width:100%}
  .row:hover{background:rgb(var(--c-surface-hover) / 0.7)}
  .row:focus-visible,.reset:focus-visible{outline:2px solid rgb(var(--c-accent) / 0.6);outline-offset:-1px}
  .reset{width:100%;padding:4px 6px;border-radius:5px;font-size:11px;text-align:left;color:rgb(var(--c-text-muted))}
  .reset:hover:not(:disabled){color:rgb(var(--c-text));background:rgb(var(--c-surface-hover) / 0.7)}
  .reset:disabled{opacity:.5}
  .row:disabled{opacity:.55;cursor:default}
  /* Lanes and limits made the panel taller than a short window; it scrolls rather than run off it. */
  .view-panel{max-height:min(70vh,36rem);overflow:hidden auto}
  .note{margin:0;padding:0 6px 4px;font-size:10px;line-height:1.35;color:rgb(var(--c-text-muted))}
  .limit{display:flex;align-items:center;gap:8px;padding:2px 6px;font-size:11px}
  .limit input{width:4.5rem;padding:2px 6px;font-size:11px}
  .row:disabled:hover{background:transparent}
</style>
