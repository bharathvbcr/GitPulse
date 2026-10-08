<script lang="ts">
  /**
   * The archive dock: archived tasks for the current scope, deleted tasks a
   * restore can bring back, and the way back for both.
   *
   * A dock rather than a destination, for the same reason the Inbox is one.
   * The board already owns the scope navigator, the search, the selection and
   * the confirm-and-retry dialog; a separate page would have to grow its own
   * copy of each, and a reader restoring a task would land somewhere other
   * than the board it belongs on. This panel opens over the board, reads the
   * board's scope, and hands every write back to the board.
   *
   * It owns no write path of its own. Restoring from the archive is
   * `restoreAction`, the same `TaskBatch` update every board change uses;
   * delete is the board's delete; and bringing a deleted task back is the
   * board's `onrestoredeleted`, which runs the bounded pass in `taskDelete.ts`.
   *
   * The one number this panel must never get wrong is how much it is showing.
   * `items.list` pages, so the rows on screen are a prefix of the scope's
   * archive; `archiveSummary` prints the loaded count beside the server's
   * total for as long as they differ. After a write the pages already loaded
   * are re-read in place (`refreshPages`), so a reader who paged deep keeps
   * their place instead of being thrown back to the first page.
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
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
  import { formatRelativeTime } from "../format";
  import { MAX_TASK_SELECTION, type TaskAction } from "../workbench/taskActions";
  import { MAX_DELETE_BATCH } from "../workbench/taskDelete";
  import { ARCHIVE_RULE, archiveStamp, archiveSummary, refreshPages, restoreAction, type ArchiveView } from "../workbench/taskArchive";
  import {
    STATUS_LABELS, explainError, listTasks,
    type Page, type Scope, type TaskCard,
  } from "../workbench/client";

  let {
    scope,
    active = true,
    busy = false,
    refreshToken = 0,
    hiddenIds = new Set<string>(),
    onopen,
    onaction,
    onrestoredeleted,
  }: {
    scope: Scope;
    active?: boolean;
    /** The board is mid-write; the dock must not queue a second one. */
    busy?: boolean;
    /** Bumped by the board after a write, so a lost live event still reloads. */
    refreshToken?: number;
    /** Tasks the board is about to delete (its undo window); already gone here too. */
    hiddenIds?: ReadonlySet<string>;
    onopen: (taskID: string) => Promise<void>;
    onaction: (cards: TaskCard[], action: TaskAction) => void;
    /** Bring deleted tasks back; resolves with the line to show. */
    onrestoredeleted: (cards: TaskCard[]) => Promise<string>;
  } = $props();

  let view = $state<ArchiveView>("archived");
  let result = $state<Page<TaskCard> | null>(null);
  /** How many pages are on screen, so a refresh re-reads exactly those. */
  let pages = 0;
  let query = $state("");
  let selected = $state<Set<string>>(new Set());
  let error = $state(""), notice = $state(""), visible = $state(!readBackgroundDocument()), working = $state(false);
  let now = $state(Math.floor(Date.now() / 1000));
  let generation = 0, disposed = false, loading = false, again = false;
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;

  const rows = $derived((result?.items ?? []).filter((card) => !hiddenIds.has(card.id)));
  const hiddenHere = $derived((result?.items.length ?? 0) - rows.length);
  // `null` until a read succeeds, so an unread dock never renders as an empty
  // archive. The dock defers while the window is in the background, which is
  // exactly when that distinction stops being theoretical.
  const summary = $derived(archiveSummary(result ? rows.length : null, Math.max(0, (result?.total ?? 0) - hiddenHere), view));
  const chosen = $derived(rows.filter((card) => selected.has(card.id)));
  // The caps are the board's, not a second policy: a batch larger than this is
  // refused by `TaskBatch` (or the delete/restore pass) itself.
  const cap = $derived(view === "deleted" ? MAX_DELETE_BATCH : MAX_TASK_SELECTION);
  const overCap = $derived(chosen.length > cap);
  const canAct = $derived(chosen.length > 0 && !overCap && !busy && !working);

  function read(cursor: string | undefined) {
    return listTasks(scope, null, query, cursor, 30, view === "archived"
      ? { archived: true, order: "completed" }
      : { deleted: true, order: "updated" });
  }

  /**
   * `"first"` starts over (scope, search or view changed); `"more"` appends
   * the next page; `"refresh"` re-reads the pages already on screen.
   */
  async function load(mode: "first" | "more" | "refresh" = "refresh") {
    if (!active || !visible || disposed) return;
    if (loading) { again = true; return; }
    const epoch = generation;
    loading = true;
    try {
      if (mode === "more" && result?.next_cursor) {
        const next = await read(result.next_cursor);
        if (epoch !== generation || disposed) return;
        const items = [...new Map([...(result?.items ?? []), ...next.items].map((item) => [item.id, item])).values()];
        result = { ...next, items, shown: items.length };
        pages++;
      } else {
        const fresh = await refreshPages(mode === "first" || !result ? 1 : pages, read);
        if (epoch !== generation || disposed) return;
        result = { items: fresh.items, total: fresh.total, shown: fresh.items.length, has_more: fresh.next_cursor !== null, next_cursor: fresh.next_cursor };
        pages = fresh.pages;
      }
      const items = result.items;
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
    const onForeground = () => {
      visible = !readBackgroundDocument();
      if (visible) schedule();
    };
    const stopClock = createAdaptiveTimer(() => { now = Math.floor(Date.now() / 1000); }, 30_000);
    listeners.track(stopClock);
    listeners.track(bindForegroundChanges(document, typeof window === "undefined" ? null : window, onForeground));
    return () => { disposed = true; generation++; clearTimeout(refreshTimer); listeners.dispose(); };
  });

  // Scope, search and view each change what the list is, so the page starts
  // over. A completed write only changes rows within it: refreshed in place.
  $effect(() => {
    scope; query; view; active; visible;
    generation++; result = null; pages = 0; selected = new Set(); clearTimeout(refreshTimer);
    untrack(() => { void load("first"); });
  });
  let seenToken = untrack(() => refreshToken);
  $effect(() => {
    if (refreshToken === seenToken) return;
    seenToken = refreshToken;
    untrack(() => { void load("refresh"); });
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

  async function restore() {
    if (!canAct) return;
    if (view === "archived") { onaction([...chosen], restoreAction()); error = ""; return; }
    working = true;
    try { notice = await onrestoredeleted([...chosen]); error = ""; }
    catch (cause) { error = explainError(cause); }
    finally { if (!disposed) { working = false; void load("refresh"); } }
  }

  function remove() {
    if (!canAct || view !== "archived") return;
    onaction([...chosen], { kind: "delete" });
  }

  function stamp(card: TaskCard): string {
    const relative = (at: number) => formatRelativeTime(at, now);
    return view === "deleted" ? `Deleted ${relative(card.updated_at)} · was ${STATUS_LABELS[card.status]}` : archiveStamp(card, relative);
  }
</script>

<section id="task-archive-dock" class="archive gp-glass bg-surface" aria-label="Archive" data-testid="task-archive">
  <header>
    <strong><Archive size={13} aria-hidden="true" /> Archive</strong>
    <div class="gp-seg" role="group" aria-label="Show">
      <button type="button" class="gp-seg-btn" data-active={view === "archived"} aria-pressed={view === "archived"} data-testid="task-archive-view-archived" onclick={() => { view = "archived"; }}>Archived</button>
      <button type="button" class="gp-seg-btn" data-active={view === "deleted"} aria-pressed={view === "deleted"} data-testid="task-archive-view-deleted" onclick={() => { view = "deleted"; }}>Deleted</button>
    </div>
    <div class="controls">
      <input
        class="gp-field"
        type="search"
        aria-label={view === "archived" ? "Search archived tasks" : "Search deleted tasks"}
        placeholder={view === "archived" ? "Search archived tasks" : "Search deleted tasks"}
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

  <!-- How a task gets here, said whether or not the panel is empty. -->
  <p class="rule" data-testid="task-archive-rule">
    {view === "archived" ? ARCHIVE_RULE : "Deleted tasks keep their history; Restore brings one back with its id, fields and archive state."}
  </p>

  {#if error}<div role="alert">{error}</div>{/if}
  {#if notice}<p class="notice" role="status">{notice}</p>{/if}

  <!-- Gated on a successful read, not on an empty list: "nothing here" is a
       finding, and a dock that has not run its query has not found it. -->
  {#if summary.pending}
    <p class="empty">{error ? "The archive could not be read. Retry with Refresh." : "Reading tasks…"}</p>
  {:else if rows.length === 0}
    <p class="empty">{query.trim() ? `No ${view} tasks match this search.` : view === "archived" ? `Nothing here yet. ${ARCHIVE_RULE}` : "No deleted tasks in this scope."}</p>
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
            <span data-testid="task-archive-stamp">{stamp(card)}{card.owner ? ` · ${card.owner}` : ""}</span>
          </div>
          {#if view === "archived"}
            <button type="button" class="gp-btn" onclick={() => void open(card)} disabled={busy || working}>
              <ExternalLink size={12} aria-hidden="true" /> Open
            </button>
          {/if}
        </article>
      {/each}
      <!-- Inside the scroller, at the end of the rows it extends. -->
      {#if result?.next_cursor}
        <button type="button" class="gp-btn more" onclick={() => load("more")} disabled={busy || working}>Load more</button>
      {/if}
    </div>
  {/if}

  {#if chosen.length > 0}
    <div class="actions bg-surface" role="group" aria-label={view === "archived" ? "Archived task actions" : "Deleted task actions"}>
      <span>{chosen.length} selected</span>
      <button type="button" class="gp-btn" data-testid="task-archive-restore" onclick={() => void restore()} disabled={!canAct}>Restore</button>
      {#if view === "archived"}<button type="button" class="gp-btn-danger" onclick={remove} disabled={!canAct}>Delete</button>{/if}
      <button type="button" class="gp-btn" onclick={() => selectLoaded(false)}>Clear</button>
      {#if overCap}<p class="over" role="alert">Select at most {cap} tasks per action.</p>{/if}
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
  .rule{margin:4px 0;color:rgb(var(--c-text-muted))}
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
  .over{flex-basis:100%;margin:0;color:#ef9a9a}
  button,input[type=search]{font:inherit;color:inherit;background:transparent;border:1px solid rgb(var(--c-border));border-radius:6px;padding:6px 9px}
  button{cursor:pointer}button:disabled{opacity:.45;cursor:default}
  [role=alert]{color:#ef9a9a}
  .empty{padding:8px 0}
</style>
