<script lang="ts">
  import { onMount, untrack } from "svelte";
  import NativeNotificationSettings from "./NativeNotificationSettings.svelte";
  import { listen } from "@tauri-apps/api/event";
  import { isTauri } from "../platform";
  import { automaticUpdates, attentionWrite, explainError, getAttention, listAttention, updateAttention, type Attention, type AttentionAction, type AttentionFilter, type AttentionWrite, type Page, type Scope } from "../workbench/client";

  let { scope, active = true, onopen }: { scope: Scope; active?: boolean; onopen: (taskID: string) => Promise<void> } = $props();
  let filter = $state<AttentionFilter>("active"), result = $state<Page<Attention> | null>(null);
  let busy = $state(false), error = $state(""), notice = $state("");
  let pending = $state<AttentionWrite | null>(null), visible = $state(true);
  let generation = 0, disposed = false, loading = false, again = false;
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  async function load(cursor?: string) {
    if (!active || !visible || disposed) return;
    if (loading) { again = true; return; }
    const epoch = generation; loading = true;
    try {
      const next = await listAttention(scope, filter, cursor);
      if (epoch === generation && !disposed) { result = next; if (!pending) error = ""; }
    } catch (cause) { if (epoch === generation && !disposed) error = explainError(cause); }
    finally { loading = false; if (again) { again = false; void load(); } }
  }
  function schedule() {
    if (!active || !visible || disposed) return;
    clearTimeout(refreshTimer); refreshTimer = setTimeout(() => { void load(); }, 200);
  }
  onMount(() => {
    let unlisten: (() => void) | undefined;
    if (isTauri()) void listen("workbench-changed", schedule).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch((cause) => { if (!disposed) notice = `Live updates unavailable: ${explainError(cause)}. Use Refresh inbox.`; });
    const automatic = automaticUpdates.subscribe(schedule);
    const visibility = () => { visible = !document.hidden; };
    visibility(); document.addEventListener("visibilitychange", visibility); window.addEventListener("focus", schedule);
    return () => { disposed = true; generation++; clearTimeout(refreshTimer); unlisten?.(); automatic(); document.removeEventListener("visibilitychange", visibility); window.removeEventListener("focus", schedule); };
  });
  $effect(() => {
    scope; filter; active; visible;
    generation++; result = null; clearTimeout(refreshTimer);
    untrack(() => { void load(); });
  });
  async function apply(item: Attention, action: AttentionAction) {
    if (busy || pending) return;
    pending = attentionWrite(item, action); await submit();
  }
  async function submit() {
    if (!pending || busy) return;
    busy = true; error = "";
    try { await updateAttention(pending); pending = null; await load(); }
    catch (cause) {
      error = explainError(cause);
      if (typeof cause === "object" && cause !== null && "code" in cause && cause.code !== "transport_error" && cause.code !== "worker_error" && cause.code !== "protocol_error") pending = null;
    } finally { busy = false; }
  }
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

<section class="inbox" aria-label="Activity inbox">
  <header><div><strong>Activity inbox</strong><p>Agent requests, run outcomes and task enhancements. Reading or dismissing a notice leaves the work unchanged.</p></div><div class="controls"><select aria-label="Inbox filter" bind:value={filter}><option value="active">Active</option><option value="unread">Unread</option><option value="all">All history</option></select><button onclick={() => load()} disabled={busy}>Refresh inbox</button></div></header>
  <p class="scope">{scope.kind === "global" ? "All repositories" : scope.kind === "workspace" ? "This workspace" : "This repository"} · {result ? `${result.shown} shown / ${result.total} matching` : "Loading…"}</p>
  <NativeNotificationSettings {scope} />
  {#if error}<div role="alert">{error}{#if pending}<button onclick={submit} disabled={busy}>Retry saved action</button>{/if}</div>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}
  {#if result?.items.length === 0}<p class="empty">No matching notifications. Saved results will appear here.</p>{/if}
  <div class="entries">
    {#each result?.items ?? [] as item (item.id)}
      <article class:unread={item.read_at === null}>
        <div class="summary"><strong>{item.title}</strong><span>{item.read_at === null ? "Unread · " : ""}{new Date(item.created_at * 1000).toLocaleString()}</span><small>Task {item.task_id} · {item.target_type} {item.target_id}{item.target_status !== "current" ? ` · ${item.target_status.replaceAll("_", " ")}` : ""}{item.dismissed_at !== null ? " · Dismissed" : ""}{item.snoozed_until !== null ? ` · Snoozed until ${new Date(item.snoozed_until * 1000).toLocaleString()}` : ""}</small></div>
        <div class="entry-actions"><button onclick={() => open(item)} disabled={busy || !!pending || item.target_status === "task_deleted" || item.target_status === "unavailable"}>Open task</button><button onclick={() => apply(item, item.read_at === null ? "read" : "unread")} disabled={busy || !!pending}>{item.read_at === null ? "Mark read" : "Mark unread"}</button><button onclick={() => apply(item, item.snoozed_until === null ? "snooze" : "unsnooze")} disabled={busy || !!pending}>{item.snoozed_until === null ? "Snooze 1h" : "Clear snooze"}</button><button onclick={() => apply(item, item.dismissed_at === null ? "dismiss" : "restore")} disabled={busy || !!pending}>{item.dismissed_at === null ? "Dismiss" : "Restore"}</button></div>
      </article>
    {/each}
  </div>
  {#if result?.has_more}<button onclick={() => load(result?.next_cursor ?? undefined)} disabled={busy}>Older notifications</button>{/if}
</section>

<style>
  .inbox{max-height:45vh;overflow:auto;flex-shrink:0;margin:8px 18px 14px;padding:16px;border:1px solid rgb(var(--c-border));border-radius:12px;background:rgb(var(--c-surface));font-size:12px;color:rgb(var(--c-text))}
  header,.controls,.entry-actions{display:flex;align-items:center;gap:8px;flex-wrap:wrap}header{justify-content:space-between;align-items:flex-start}p{margin:6px 0;color:rgb(var(--c-text-muted))}.scope{font-size:11px}.entries{max-height:340px;overflow:auto;margin-top:12px}article{display:flex;justify-content:space-between;gap:12px;padding:12px 8px;border-top:1px solid rgb(var(--c-border))}article.unread{border-left:2px solid rgb(var(--c-accent))}.summary{display:flex;flex-direction:column;gap:5px;min-width:0}.summary strong{font-weight:550}.summary span,.summary small{color:rgb(var(--c-text-muted));overflow-wrap:anywhere}.entry-actions{justify-content:flex-end}button,select{font:inherit;color:inherit;background:transparent;border:1px solid rgb(var(--c-border));border-radius:6px;padding:6px 9px}button{cursor:pointer}button:disabled{opacity:.45;cursor:default}button:hover:not(:disabled){background:rgb(var(--c-surface-hover))}button:focus-visible,select:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:2px}[role=alert]{color:#ef9a9a}.empty{padding:12px 0}@media(max-width:900px){article{flex-direction:column}.entry-actions{justify-content:flex-start}}
</style>
