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
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { LAYERS } from "../ui/layers";
  import { STATUSES, STATUS_LABELS } from "../workbench/vocabulary";
  import { TASK_CARD_FIELDS, TASK_CARD_FIELD_LABELS, canHideStatus } from "../ui/taskView";
  import { onMount } from "svelte";

  let { disabled = false }: { disabled?: boolean } = $props();

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
      $interfaceStore.taskLayout !== "board",
  );

  onMount(() => {
    const onPointer = (event: PointerEvent) => {
      if (open && shouldDismissOverlay(event.target, "[data-task-view-menu]")) open = false;
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && open) {
        open = false;
        root?.querySelector<HTMLButtonElement>("[data-task-view-toggle]")?.focus();
      }
    };
    window.addEventListener("pointerdown", onPointer, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("keydown", onKey);
    };
  });
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
      class="absolute right-0 top-full mt-1 gp-menu gp-pop p-2 w-56 flex flex-col gap-0.5"
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
  .row:disabled:hover{background:transparent}
</style>
