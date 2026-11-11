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
   * Placed and dismissed by the shared popover owner: portaled to the body,
   * anchored under its own trigger, and clamped to the window.
   */
  import { Bookmark, X } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { portal } from "../dom/portal";
  import { popover, restoreFocusTo, popoverExit } from "../ui/popover";
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
  let name = $state("");
  let message = $state("");
  let failed = $state(false);

  const views = $derived(boardPrefs($interfaceStore.taskBoards, board).views);
  const showing = $derived(views.find((view) => sameSnapshot(view, current)) ?? null);
  // A different board's draft name and message must not carry over.
  $effect(() => { board; name = ""; message = ""; failed = false; });

  let toggleEl: HTMLButtonElement | undefined = $state();

  /**
   * Anchored under the trigger and right-aligned to it, then clamped to the
   * window: the Tasks pane is paint-contained and sits beside the sidebar, so
   * a CSS `absolute` panel that opens leftwards was cut off at the pane edge.
   * `revision` re-places it when a saved or deleted view changes its height.
   */
  const dismissal = $derived({
    anchor: { kind: "element" as const, element: toggleEl, gap: 4, align: "end" as const },
    inset: 8,
    revision: `${views.length}|${message}`,
    dismiss: { inside: "[data-task-saved-views]", escape: "bubble" as const, resize: true },
    onDismiss: (reason: string) => {
      open = false;
      if (reason === "escape") restoreFocusTo(toggleEl);
    },
  });

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

<div class="relative" data-task-saved-views>
  <button
    type="button"
    class="gp-btn"
    data-task-views-toggle
    bind:this={toggleEl}
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
      use:portal={"body"}
      use:popover={dismissal} out:popoverExit
      data-task-saved-views
      class="panel fixed gp-menu gp-pop p-2 w-64 flex flex-col gap-1"
      style="z-index: {LAYERS.MENU}"
      role="group"
      aria-label={`Saved views for ${boardName}`}
      data-testid="task-saved-views"
    >
      <p class="gp-label px-1.5 pb-1">Views on {boardName}</p>
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
  .apply{flex:1;min-width:0;text-align:left;padding:4px 6px;border-radius:5px;font-size:var(--text-meta);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .apply:hover{background:rgb(var(--c-surface-hover) / 0.7)}
  .apply[aria-pressed="true"]{color:rgb(var(--c-accent));font-weight:600}
  .apply:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:-1px}
  .save{display:flex;gap:4px;padding-top:6px}
  .save input{flex:1;min-width:0;font-size:var(--text-meta);padding:3px 6px}
  .note{margin:0;padding:0 6px;font-size:var(--text-micro);line-height:1.35;color:rgb(var(--c-text-muted));min-height:0}
  .note.failed{color:rgb(var(--c-danger))}
</style>
