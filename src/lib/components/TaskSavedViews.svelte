<script lang="ts">
  /**
   * Named views for one Tasks board: save what is on screen — layout, lanes,
   * columns, card chips, filters and search — and put it back with a click.
   *
   * Views belong to the board they were saved on (`boardKey`): a workspace's
   * views are offered on that workspace's board and nowhere else. They live in
   * the reader's preferences (`interfaceStore.taskBoards`), not in the task
   * store, so saving one never writes a task or a workspace.
   *
   * Placed and dismissed the way the View menu beside it is: `absolute` under
   * its own trigger, with dismissal from the shared popover owner.
   */
  import { Bookmark, X } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { popover } from "../ui/popover";
  import { LAYERS } from "../ui/layers";
  import { MAX_VIEW_NAME, boardPrefs, sameSnapshot, type SavedTaskView, type TaskViewSnapshot } from "../ui/taskBoardViews";
  import { newID } from "../workbench/client";

  let { disabled = false, board, boardName, current, onApply }: {
    disabled?: boolean;
    board: string;
    boardName: string;
    /** What the board is showing now, which Save stores. */
    current: TaskViewSnapshot;
    onApply: (view: SavedTaskView) => void;
  } = $props();

  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();
  let name = $state("");
  let message = $state("");
  let failed = $state(false);

  const views = $derived(boardPrefs($interfaceStore.taskBoards, board).views);
  const showing = $derived(views.find((view) => sameSnapshot(view, current)) ?? null);
  // A different board's draft name and message must not carry over.
  $effect(() => { board; name = ""; message = ""; failed = false; });

  const dismissal = {
    dismiss: { inside: "[data-task-saved-views]", escape: "bubble" as const },
    onDismiss: (reason: string) => {
      open = false;
      if (reason === "escape") root?.querySelector<HTMLButtonElement>("[data-task-views-toggle]")?.focus();
    },
  };

  function save(event: SubmitEvent) {
    event.preventDefault();
    const result = interfaceStore.saveTaskView(board, name, current, newID);
    failed = !result.ok;
    if (!result.ok) { message = result.reason; return; }
    message = result.replaced ? `Updated “${result.view.name}”.` : `Saved “${result.view.name}”.`;
    name = "";
  }
  function apply(view: SavedTaskView) {
    onApply(view);
    failed = false;
    message = `Showing “${view.name}”.`;
  }
  function remove(view: SavedTaskView) {
    interfaceStore.deleteTaskView(board, view.id);
    failed = false;
    message = `Deleted “${view.name}”.`;
  }
</script>

<div class="relative" bind:this={root} data-task-saved-views>
  <button
    type="button"
    class="gp-btn"
    data-task-views-toggle
    aria-expanded={open}
    aria-haspopup="true"
    {disabled}
    onclick={() => { open = !open; }}
    title={`Saved views for ${boardName}`}
  >
    <Bookmark size={12} />
    <span>{showing ? showing.name : "Views"}</span>
  </button>
  {#if open}
    <div
      use:popover={dismissal}
      class="panel absolute right-0 top-full mt-1 gp-menu gp-pop p-2 w-64 flex flex-col gap-1"
      style="z-index: {LAYERS.MENU}"
      role="group"
      aria-label={`Saved views for ${boardName}`}
      data-testid="task-saved-views"
    >
      <p class="px-1.5 pb-1 text-[10px] uppercase tracking-wide text-textMuted">Views on {boardName}</p>
      {#if views.length === 0}<p class="note">None yet. Arrange the board, then save it here by name.</p>{/if}
      {#each views as view (view.id)}
        <div class="view-row" data-task-view={view.id}>
          <button type="button" class="apply" aria-pressed={showing?.id === view.id} onclick={() => apply(view)}>{view.name}</button>
          <button type="button" class="gp-icon-btn" aria-label={`Delete view ${view.name}`} onclick={() => remove(view)}><X size={11} /></button>
        </div>
      {/each}
      <form class="save" onsubmit={save}>
        <input class="gp-field" aria-label="View name" placeholder="Name this view" maxlength={MAX_VIEW_NAME} bind:value={name} />
        <button type="submit" class="gp-btn">Save</button>
      </form>
      <p class="note" role="status" class:failed>{message}</p>
    </div>
  {/if}
</div>

<style>
  .panel{max-height:min(70vh,32rem);overflow:hidden auto}
  .view-row{display:flex;align-items:center;gap:4px}
  .apply{flex:1;min-width:0;text-align:left;padding:4px 6px;border-radius:5px;font-size:11px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .apply:hover{background:rgb(var(--c-surface-hover) / 0.7)}
  .apply[aria-pressed="true"]{color:rgb(var(--c-accent));font-weight:600}
  .apply:focus-visible{outline:2px solid rgb(var(--c-accent) / 0.6);outline-offset:-1px}
  .save{display:flex;gap:4px;padding-top:4px;border-top:1px solid rgb(var(--c-border) / 0.6)}
  .save input{flex:1;min-width:0;font-size:11px;padding:3px 6px}
  .note{margin:0;padding:0 6px;font-size:10px;line-height:1.35;color:rgb(var(--c-text-muted));min-height:0}
  .note.failed{color:#d15a64}
</style>
