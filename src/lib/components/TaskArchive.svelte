<script lang="ts">
  /**
   * The archive dock: completed tasks for the current scope, and the way back.
   *
   * A dock rather than a destination, for the same reason the Inbox is one.
   * The board already owns the scope navigator, the search, the selection and
   * the confirm-and-retry dialog; a separate page would have to grow its own
   * copy of each, and a reader restoring a task would land somewhere other
   * than the board it belongs on. This panel opens over the board, reads the
   * board's scope, and hands every write back to the board's existing
   * `TaskActionDialog`.
   *
   * It owns no write path of its own. Restore is `restoreAction`, which is
   * the same `TaskBatch` status update the board's Move menu builds, and
   * delete is the board's delete. That is what keeps revision checks,
   * interrupted-write recovery and receipt identity in one place.
   *
   * The one number this panel must never get wrong is how much it is showing.
   * `items.list` pages, so the rows on screen are a prefix of the scope's
   * completed work; `archiveSummary` prints the loaded count beside the
   * server's total for as long as they differ.
   *
   * Two layout rules the panel is built around, both of them defects it had:
   *
   * * **One scroller.** The panel scrolled *and* the row list scrolled inside
   *   it, with independent caps. On a 768px window the row list won, and the
   *   action bar rendered 107px below the panel's own fold at scroll top — so
   *   ticking a checkbox produced no visible change at all.
   * * **The actions are never off screen.** They are pinned to the bottom of
   *   the panel, because a selection whose only verb has scrolled away reads
   *   as a panel that cannot act on what it just let you select.
   *
   * Both the select-all head and the action bar sit *outside* the row list,
   * as rows of the panel's own column, rather than sticking inside it. That
   * costs the list their height and buys the thing that matters: `.entries`
   * clips its own overflow, so no row can ever be painted under either of
   * them. Sticky was written first and failed on a Mac, where `bg-surface`
   * remaps to a 50%-alpha material and a task title showed through the
   * select-all label. Every denser fill is worse — an alpha-less token is the
   * opaque slab `mac-material-contract` exists to prevent, and a second
   * `backdrop-filter` inside an already-blurred panel makes this a backdrop
   * root and costs frames for a blur whose only input is a flat wash. The
   * action bar keeps `position:sticky` anyway, as the last resort for a
   * window so short that the panel's own chrome overflows the cap.
   */
  import { onMount, untrack } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { Archive, ExternalLink } from "@lucide/svelte";
  import { isTauri } from "../platform";
  import { createListenerTracker } from "../dom/listenerTracker";
  import { formatRelativeTime } from "../format";
  import { MAX_TASK_SELECTION, type TaskAction } from "../workbench/taskActions";
  import { ARCHIVE_RULE, ARCHIVE_STATUS, RESTORE_STATUSES, archiveSummary, boardPresence, restoreAction } from "../workbench/taskArchive";
  import {
    STATUS_LABELS, explainError, listTasks,
    type Page, type Scope, type TaskCard, type TaskStatus,
  } from "../workbench/client";

  let {
    scope,
    active = true,
    busy = false,
    hiddenColumns = [],
    refreshToken = 0,
    onopen,
    onaction,
    ontogglecolumn,
  }: {
    scope: Scope;
    active?: boolean;
    /** The board is mid-write; the dock must not queue a second one. */
    busy?: boolean;
    hiddenColumns?: readonly TaskStatus[];
    /** Bumped by the board after a write, so a lost live event still reloads. */
    refreshToken?: number;
    onopen: (taskID: string) => Promise<void>;
    onaction: (cards: TaskCard[], action: TaskAction) => void;
    ontogglecolumn: () => void;
  } = $props();

  let result = $state<Page<TaskCard> | null>(null);
  let query = $state("");
  let restoreTo = $state<TaskStatus>(RESTORE_STATUSES[RESTORE_STATUSES.length - 1]);
  let selected = $state<Set<string>>(new Set());
  let error = $state(""), notice = $state(""), visible = $state(true), working = $state(false);
  let now = $state(Math.floor(Date.now() / 1000));
  let generation = 0, disposed = false, loading = false, again = false;
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;

  const rows = $derived(result?.items ?? []);
  // `null` until a read succeeds, so an unread dock never renders as an empty
  // archive. The dock defers while the window is in the background, which is
  // exactly when that distinction stops being theoretical.
  const summary = $derived(archiveSummary(result ? rows.length : null, result?.total ?? 0));
  const presence = $derived(boardPresence(hiddenColumns));
  const chosen = $derived(rows.filter((card) => selected.has(card.id)));
  // The cap is the board's, not a second policy: a batch larger than this is
  // refused by `TaskBatch` itself, so the dock stops offering it first.
  const overCap = $derived(chosen.length > MAX_TASK_SELECTION);
  const canAct = $derived(chosen.length > 0 && !overCap && !busy && !working);

  async function load(cursor?: string) {
    if (!active || !visible || disposed) return;
    if (loading) { again = true; return; }
    const epoch = generation;
    loading = true;
    try {
      const next = await listTasks(scope, ARCHIVE_STATUS, query, cursor);
      if (epoch !== generation || disposed) return;
      // "Load more" grows the page the way the board's columns do, so a
      // reader who paged deep does not lose those rows on the next refresh.
      const items = cursor
        ? [...new Map([...(result?.items ?? []), ...next.items].map((item) => [item.id, item])).values()]
        : next.items;
      result = { ...next, items, shown: items.length };
      selected = new Set([...selected].filter((id) => items.some((item) => item.id === id)));
      error = "";
    } catch (cause) {
      if (epoch === generation && !disposed) error = explainError(cause);
    } finally {
      loading = false;
      if (again) { again = false; void load(); }
    }
  }

  function schedule() {
    if (!active || !visible || disposed) return;
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => { void load(); }, 200);
  }

  onMount(() => {
    const listeners = createListenerTracker();
    if (isTauri()) {
      void listen("workbench-changed", schedule)
        .then((stop) => listeners.track(stop))
        .catch((cause) => { if (!disposed) notice = `Live updates unavailable: ${explainError(cause)}. Use Refresh.`; });
    }
    const visibility = () => { visible = !document.hidden; };
    const clock = window.setInterval(() => { now = Math.floor(Date.now() / 1000); }, 30_000);
    visibility();
    document.addEventListener("visibilitychange", visibility);
    window.addEventListener("focus", schedule);
    listeners.track(() => window.clearInterval(clock));
    listeners.track(() => document.removeEventListener("visibilitychange", visibility));
    listeners.track(() => window.removeEventListener("focus", schedule));
    return () => { disposed = true; generation++; clearTimeout(refreshTimer); listeners.dispose(); };
  });

  // Scope, search and a completed write each invalidate the whole page, so
  // the accumulated rows are dropped rather than merged into a stale list.
  $effect(() => {
    scope; query; active; visible; refreshToken;
    generation++; result = null; selected = new Set(); clearTimeout(refreshTimer);
    untrack(() => { void load(); });
  });

  function toggle(id: string, on: boolean) {
    const next = new Set(selected);
    if (on) next.add(id); else next.delete(id);
    selected = next;
  }

  function selectLoaded(on: boolean) {
    selected = on ? new Set(rows.map((card) => card.id)) : new Set();
  }

  async function open(card: TaskCard) {
    if (working) return;
    working = true;
    try { await onopen(card.id); }
    catch (cause) { error = explainError(cause); }
    finally { if (!disposed) working = false; }
  }

  function restore() {
    if (!canAct) return;
    try { onaction([...chosen], restoreAction(restoreTo)); error = ""; }
    catch (cause) { error = explainError(cause); }
  }

  function remove() {
    if (!canAct) return;
    onaction([...chosen], { kind: "delete" });
  }
</script>

<section id="task-archive-dock" class="archive gp-glass bg-surface" aria-label="Archive" data-testid="task-archive">
  <header>
    <strong><Archive size={13} aria-hidden="true" /> Archive</strong>
    <div class="controls">
      <input
        class="gp-field"
        type="search"
        aria-label="Search completed tasks"
        placeholder="Search completed tasks"
        maxlength="512"
        bind:value={query}
      />
      <button type="button" class="gp-btn" onclick={() => load()} disabled={busy || working}>Refresh</button>
    </div>
  </header>

  <p class="summary" role="status" data-testid="task-archive-summary">
    {summary.text}
    {#if summary.partial}<span class="more-note">Load more to see the rest.</span>{/if}
    <!-- Deferring the query while the window is in the background is the
         Inbox's behaviour too, and worth the saved round trips. Leaving the
         reader to guess why the panel is blank is not. -->
    {#if summary.pending && !visible && !error}<span class="more-note">Paused while this window is in the background.</span>{/if}
  </p>

  <!-- One row on a normal window, two when the panel is narrow. Both
       sentences are fixed chrome above the scrolling list, so every line
       they do not spend is a line the list keeps. -->
  <div class="about">
    <!-- How a task gets here, said whether or not the panel is empty. A dock
         named Archive that offers Restore and never names the one thing that
         archives a task is the reason this line is not in the empty state. -->
    <p class="rule" data-testid="task-archive-rule">{ARCHIVE_RULE}</p>

    <!-- The dock is a second way to read completed work, not a move. Whether
         the Done column is still on the board is said plainly, with the
         toggle that changes it; the board's `taskHiddenColumns` stays the
         only owner of that choice. -->
    <p class="presence">
      {presence.sentence}
      <button type="button" class="link" onclick={ontogglecolumn}>{presence.actionLabel}</button>
    </p>
  </div>

  {#if error}<div role="alert">{error}</div>{/if}
  {#if notice}<p class="notice" role="status">{notice}</p>{/if}

  <!-- Gated on a successful read, not on an empty list: "nothing here" is a
       finding, and a dock that has not run its query has not found it. -->
  {#if summary.pending}
    <p class="empty">{error ? "The archive could not be read. Retry with Refresh." : "Reading completed tasks…"}</p>
  {:else if rows.length === 0}
    <!-- The same sentence the header carries, from the same constant: an
         empty archive and a full one must not teach two different rules. -->
    <p class="empty">{query.trim() ? "No completed tasks match this search." : `Nothing here yet. ${ARCHIVE_RULE}`}</p>
  {:else}
    <!-- A row above the scroller, never a bar floating inside it. Why, and
         what was tried first, is in the component note at the top. -->
    <div class="list-head">
      <label class="pick">
        <input
          type="checkbox"
          checked={chosen.length === rows.length && rows.length > 0}
          indeterminate={chosen.length > 0 && chosen.length < rows.length}
          onchange={(event) => selectLoaded(event.currentTarget.checked)}
        />
        Select the {rows.length} loaded
      </label>
    </div>
    <div class="entries">
      {#each rows as card (card.id)}
        <article class:picked={selected.has(card.id)} data-testid="task-archive-row" data-card-id={card.id}>
          <label class="pick">
            <input type="checkbox" checked={selected.has(card.id)} onchange={(event) => toggle(card.id, event.currentTarget.checked)} />
            <span class="sr-only">Select {card.title}</span>
          </label>
          <div class="summary-cell">
            <strong>{card.title}</strong>
            <!-- `updated_at` is the last write, not a completion stamp; the
                 store keeps no completion time, so the label says which it
                 is rather than implying the archive is ordered by recency. -->
            <span>Updated {formatRelativeTime(card.updated_at, now)}{card.owner ? ` · ${card.owner}` : ""}</span>
          </div>
          <button type="button" class="gp-btn" onclick={() => void open(card)} disabled={busy || working}>
            <ExternalLink size={12} aria-hidden="true" /> Open
          </button>
        </article>
      {/each}
      <!-- Inside the scroller, at the end of the rows it extends. Outside it
           this was a fixed row competing with the list for the panel's
           height, and it belongs after the last row anyway. -->
      {#if result?.next_cursor}
        <button type="button" class="gp-btn more" onclick={() => load(result?.next_cursor ?? undefined)} disabled={busy || working}>Load more</button>
      {/if}
    </div>
  {/if}

  {#if chosen.length > 0}
    <div class="actions bg-surface" role="group" aria-label="Archived task actions">
      <span>{chosen.length} selected</span>
      <label>
        Restore to
        <select class="gp-select" aria-label="Restore to" bind:value={restoreTo}>
          {#each RESTORE_STATUSES as status (status)}<option value={status}>{STATUS_LABELS[status]}</option>{/each}
        </select>
      </label>
      <button type="button" class="gp-btn" onclick={restore} disabled={!canAct}>Restore</button>
      <button type="button" class="gp-btn-danger" onclick={remove} disabled={!canAct}>Delete</button>
      <button type="button" class="gp-btn" onclick={() => selectLoaded(false)}>Clear</button>
      {#if overCap}<p class="over" role="alert">Select at most {MAX_TASK_SELECTION} tasks per action.</p>{/if}
    </div>
  {/if}
</section>

<style>
  /* A column, so the rows are the only part that scrolls and the action bar
     is a fixed row after them. `overflow:auto` here is the fallback for a
     window so short that the panel's own chrome exceeds the cap: it degrades
     to a scrolling panel rather than clipping the actions off. In normal use
     `.entries` absorbs the shrink and this never engages. */
  .archive{display:flex;flex-direction:column;max-height:46vh;overflow:auto;flex-shrink:0;margin:8px 14px;padding:12px;border:1px solid rgb(var(--c-border));border-radius:12px;font-size:12px;color:rgb(var(--c-text))}
  /* Every row but the list keeps its natural height and never shrinks, so
     the whole of an over-tall panel is taken out of the row list and the
     action bar cannot be the part that goes off screen. */
  .archive > *{flex:0 0 auto}
  header,.controls{display:flex;align-items:center;gap:8px;flex-wrap:wrap}header{justify-content:space-between}
  header strong{display:flex;align-items:center;gap:6px;font-weight:600}
  p{margin:6px 0;color:rgb(var(--c-text-muted))}
  .summary{font-weight:550;color:rgb(var(--c-text))}
  .more-note{font-weight:400;color:rgb(var(--c-text-muted))}
  .about{display:flex;align-items:baseline;flex-wrap:wrap;column-gap:12px}
  .about p{margin:4px 0}
  .presence{display:flex;align-items:center;gap:8px;flex-wrap:wrap}
  .link{border:0;padding:0;background:transparent;color:rgb(var(--c-accent));cursor:pointer;text-decoration:underline;font:inherit}
  .rule{color:rgb(var(--c-text-muted))}
  /* No ground and no pin: it is a row of the panel's column, above the
     scroller rather than floating over it. See the note at the markup. */
  .list-head{padding:6px 4px;border-top:1px solid rgb(var(--c-border))}
  /* The one scroller, and the only row allowed to shrink. It does not grow:
     three archived tasks draw a short panel, not a 46vh one with a gap. */
  .entries{flex:0 1 auto;min-height:0;overflow:auto}
  article{display:flex;align-items:center;gap:10px;padding:8px 4px;border-top:1px solid rgb(var(--c-border))}
  article.picked{border-left:2px solid rgb(var(--c-accent))}
  .pick{display:flex;align-items:center;gap:6px;color:rgb(var(--c-text-muted));cursor:pointer}
  .summary-cell{display:flex;flex-direction:column;gap:2px;min-width:0;flex:1}
  .summary-cell strong{font-weight:550;overflow-wrap:anywhere}
  .summary-cell span{color:rgb(var(--c-text-muted))}
  .more{margin-top:8px}
  /* Sticky as well as last. Being the final flex row already keeps it on
     screen whenever the list is the part that shrank; sticky is what keeps
     the promise in the case the list cannot shrink any further and the panel
     itself starts to scroll. A selection whose only verbs have scrolled out
     of sight is the defect this panel was reported for. */
  .actions{position:sticky;bottom:0;display:flex;align-items:center;gap:10px;flex-wrap:wrap;margin-top:10px;padding:10px 0 0;border-top:1px solid rgb(var(--c-border))}
  .actions label{display:flex;align-items:center;gap:6px;color:rgb(var(--c-text-muted))}
  .over{flex-basis:100%;margin:0;color:#ef9a9a}
  button,select,input[type=search]{font:inherit;color:inherit;background:transparent;border:1px solid rgb(var(--c-border));border-radius:6px;padding:6px 9px}
  button{cursor:pointer}button:disabled{opacity:.45;cursor:default}
  [role=alert]{color:#ef9a9a}
  .empty{padding:8px 0}
</style>
