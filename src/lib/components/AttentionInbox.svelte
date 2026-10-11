<script lang="ts">
  import { onMount, untrack } from "svelte";
  import NativeNotificationSettings from "./NativeNotificationSettings.svelte";
  import { createListenerTracker } from "../dom/listenerTracker";
  import { readBackgroundDocument } from "../runtime/foreground";
  import { createLiveLoader } from "../workbench/liveLoader";
  import { formatRelativeTime } from "../format";
  import { buriedUnread, notificationPile, pileToggleLabel } from "../ui/notificationPile";
  import { automaticUpdates, explainError, sameAutomaticUpdate, getAttention, listAttention, type Attention, type AttentionFilter, type AutomaticUpdate, type Page, type Scope } from "../workbench/client";

  let { scope, active = true, onopen }: { scope: Scope; active?: boolean; onopen: (taskID: string) => Promise<void> } = $props();
  let filter = $state<AttentionFilter>("active"), result = $state<Page<Attention> | null>(null);
  let busy = $state(false), error = $state(""), notice = $state("");
  let visible = $state(!readBackgroundDocument());
  let expanded = $state(false);
  let pileKey = "";
  const loader = createLiveLoader<string>({
    canRun: () => active && visible,
    read: async (cursor, isCurrent) => {
      const next = await listAttention(scope, filter, cursor);
      if (isCurrent()) { result = next; error = ""; }
    },
    onError: (cause) => { error = explainError(cause); },
  });
  const load = loader.load;
  const schedule = loader.schedule;
  onMount(() => {
    const listeners = createListenerTracker();
    listeners.track(loader.connect({
      onVisibility: (next) => { visible = next; },
      onUnavailable: (message) => { notice = `Live updates unavailable: ${message}. Use Refresh inbox.`; },
    }));
    // The load effect reads once on mount; the status poll then republishes
    // every 2 s, and only a change in what it says re-reads the inbox.
    let lastAutomatic: AutomaticUpdate | null = null;
    const automatic = automaticUpdates.subscribe((update) => {
      const changed = lastAutomatic !== null && !sameAutomaticUpdate(lastAutomatic, update);
      lastAutomatic = update;
      if (changed) schedule();
    });
    listeners.track(automatic);
    return () => { loader.dispose(); listeners.dispose(); };
  });
  $effect(() => {
    const key = scope.kind === "global" ? `global:${filter}` : `${scope.kind}:${scope.id}:${filter}`;
    if (key !== pileKey) { pileKey = key; expanded = false; }
  });
  $effect(() => {
    // Not `visible`: a blur pauses loads and a return refreshes in place
    // (`onForeground`), instead of emptying the list under the reader.
    scope; filter; active;
    loader.reset(); result = null;
    untrack(() => { void load(); });
  });
  const items = $derived(result?.items ?? []);
  const pile = $derived(notificationPile(items.length, expanded));
  const shown = $derived(items.slice(0, pile.shown));
  const label = $derived(pileToggleLabel(items.length, expanded, buriedUnread(items.map((item) => item.read_at), expanded)));
  // Deeper lips first so the nearer one paints above them without a negative
  // z-index, which would slip behind the inbox plate.
  const lips = $derived(Array.from({ length: pile.peeks }, (_, index) => pile.peeks - index));
  async function open(item: Attention) {
    if (busy) return;
    busy = true; error = "";
    try {
      const current = await getAttention(item.id);
      if (current.task_id !== item.task_id || current.target_id !== item.target_id || current.target_type !== item.target_type) throw new Error("The notification target changed. Refresh the inbox.");
      if (current.target_status === "task_deleted" || current.target_status === "unavailable") { notice = "This target is no longer available. Its notification remains in history."; return; }
      if (current.target_status !== "current") notice = "The task or result has changed. Showing the current task for review.";
      await onopen(current.task_id);
    } catch (cause) { error = explainError(cause); } finally { busy = false; }
  }
</script>

<section class="inbox gp-glass bg-surface" aria-label="Inbox">
  <header>
    <strong>Inbox</strong>
    <div class="controls">
      <select class="gp-select" aria-label="Inbox filter" bind:value={filter}><option value="active">Active</option><option value="unread">Unread</option><option value="all">All</option></select>
      <button type="button" class="gp-btn" onclick={() => load()} disabled={busy}>Refresh</button>
    </div>
  </header>
  <details>
    <summary>Notifications</summary>
    <NativeNotificationSettings {scope} />
  </details>
  {#if error}<div role="alert">{error}</div>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if result?.items.length === 0}<p class="empty">None</p>{/if}
  {#if items.length > 0}
    <div class="pile" class:open={pile.peeks === 0} data-testid="notification-pile">
      {#each shown as item (item.id)}
        <article class="gp-card" class:unread={item.read_at === null}>
          <div class="summary"><strong>{item.title}</strong><span>{formatRelativeTime(item.created_at)}</span></div>
          <button type="button" class="gp-btn" onclick={() => open(item)} disabled={busy || item.target_status === "task_deleted" || item.target_status === "unavailable"}>Open</button>
        </article>
      {/each}
      {#each lips as depth (depth)}
        <div class="peek gp-card" style:--depth={depth} aria-hidden="true"></div>
      {/each}
    </div>
  {/if}
  {#if label}<button type="button" class="gp-btn" aria-expanded={expanded} onclick={() => { expanded = !expanded; }}>{label}</button>{/if}
  {#if result?.has_more}<button type="button" class="gp-btn" onclick={() => load(result?.next_cursor ?? undefined)} disabled={busy}>Older</button>{/if}
</section>

<style>
  .inbox{max-height:40vh;overflow:auto;flex-shrink:0;margin:8px 14px;padding:12px;border:1px solid rgb(var(--c-border));border-radius:12px;font-size:12px;color:rgb(var(--c-text))}
  header,.controls{display:flex;align-items:center;gap:8px;flex-wrap:wrap}header{justify-content:space-between}
  p{margin:6px 0;color:rgb(var(--c-text-muted))}
  /* Lips tuck under the card by its padding only, so the title never sits on another card. */
  .pile{position:relative;display:flex;flex-direction:column;gap:8px;margin-top:8px}
  .pile.open{max-height:280px;overflow:auto}
  .pile:not(.open){padding-bottom:16px}
  article{position:relative;z-index:1;display:flex;justify-content:space-between;align-items:center;gap:12px;padding:10px 12px}
  article.unread{box-shadow:var(--shadow-card),inset 3px 0 0 rgb(var(--c-accent))}
  .peek{position:absolute;z-index:0;height:16px;pointer-events:none;left:calc(var(--depth) * 10px);right:calc(var(--depth) * 10px);bottom:calc((2 - var(--depth)) * 8px)}
  .pile + button{margin-top:8px}
  .summary{display:flex;flex-direction:column;gap:2px;min-width:0}
  .summary strong{font-weight:550}
  .summary span{color:rgb(var(--c-text-muted))}
  details{margin:8px 0}
  summary{cursor:pointer;color:rgb(var(--c-text-muted))}
  button,select{font:inherit;color:inherit;background:transparent;border:1px solid rgb(var(--c-border));border-radius:6px;padding:6px 9px}
  button{cursor:pointer}button:disabled{opacity:.45;cursor:default}
  [role=alert]{color:#ef9a9a}
  .empty{padding:8px 0}
</style>
