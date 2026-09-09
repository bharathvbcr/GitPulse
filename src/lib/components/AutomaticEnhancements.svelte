<script lang="ts">
  import { onMount } from "svelte";
  import { automaticQueueCount, automaticUpdates, enhancementConfiguration, explainError, getAutomation, newID, putAutomation, wakeAutomatic, watchAutomatic, WorkbenchError, type AutomationSettings } from "../workbench/client";

  let { active = true }: { active?: boolean } = $props();
  let opened = $state(false), loading = $state(false), saving = $state(false), visible = $state(true);
  let settings = $state<AutomationSettings | null>(null), error = $state(""), note = $state("");
  let enabled = $state(true), override = $state(false), provider = $state(""), model = $state("");
  let configured = $state(""), queued = $state<number | null>(null);
  let pending = $state<Record<string, unknown> | null>(null);
  let disposed = false;
  const labels = { not_started: "Not started", checking: "Checking saved work", idle: "Idle", disabled: "Off", waiting: "Waiting", generating: "Generating a suggestion", stopping: "Stopping", paused: "Needs attention", stopped: "Stopped" };
  const status = $derived($automaticUpdates.status);

  onMount(() => {
    const update = () => { visible = document.visibilityState === "visible"; };
    update(); document.addEventListener("visibilitychange", update);
    return () => { disposed = true; document.removeEventListener("visibilitychange", update); };
  });
  $effect(() => { if (active) void wakeAutomatic(); });
  $effect(() => { if (active && visible) return watchAutomatic(); });
  async function queueCount() {
    try { const count = await automaticQueueCount(); if (!disposed) queued = count; }
    catch (cause) { if (!disposed) { queued = null; error = explainError(cause); } }
  }
  async function load() {
    if (loading || saving || pending) return;
    loading = true; error = "";
    try {
      const saved = await getAutomation();
      if (disposed) return;
      settings = saved; enabled = saved.enabled; override = saved.provider !== null;
      provider = saved.provider ?? ""; model = saved.model ?? "";
      await queueCount();
      try {
        const selection = await enhancementConfiguration();
        if (!disposed) configured = selection.model ? `${selection.provider} / ${selection.model}` : "No model selected in Manvi";
      } catch (cause) { if (!disposed) configured = `Manvi selection unavailable: ${explainError(cause)}`; }
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) loading = false; }
  }
  async function save(stop = false) {
    if (!settings || saving) return;
    if (!pending) pending = { id: "profile", request_id: newID(), expected_revision: settings.revision,
      enabled: stop ? false : enabled, provider: stop ? settings.provider : override ? provider.trim() : null, model: stop ? settings.model : override ? model.trim() : null };
    saving = true; error = ""; note = "";
    try {
      const saved = await putAutomation(pending);
      if (disposed) return;
      pending = null; settings = saved; enabled = saved.enabled;
      note = saved.enabled ? "Automatic suggestions are enabled for future text saves. Suggestions require your acceptance." : "Automatic suggestions are off. Queued work was cleared; running work must acknowledge cancellation.";
      await queueCount();
    } catch (cause) {
      if (disposed) return;
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) pending = null;
    } finally { if (!disposed) saving = false; }
  }
  function toggle() { if (pending || saving) return; opened = !opened; if (opened) void load(); }
</script>

<section class="automation" aria-label="Automatic Manvi suggestions">
  <div class="summary">
    <button type="button" aria-expanded={opened} disabled={saving || pending !== null} onclick={toggle}>Automatic suggestions <span>{$automaticUpdates.error ? "Unavailable" : status ? labels[status.state] : "Checking…"}</span></button>
    {#if $automaticUpdates.error || status?.state === "paused" || status?.state === "stopped"}<button type="button" onclick={() => wakeAutomatic()}>Resume saved work</button>{/if}
  </div>
  {#if $automaticUpdates.error}<p role="alert">Tasks remain saved. Automatic suggestions could not start: {$automaticUpdates.error}</p>{/if}
  {#if status?.reason}<p role="status">{status.reason}</p>{/if}
  {#if opened}
    <div class="settings">
      <p>Applies to this GitPulse profile across all workspaces and repositories. Saved title and description changes are queued after one second; moving cards does not generate suggestions.</p>
      {#if loading}<p role="status">Loading settings…</p>{/if}
      {#if settings}
        <fieldset disabled={loading || saving || pending !== null}>
          <label class="check"><input type="checkbox" bind:checked={enabled} />Suggest clearer task titles and descriptions automatically</label>
          <label class="check"><input type="checkbox" bind:checked={override} />Use a provider and model for this profile</label>
          {#if override}<div class="pair"><label>Automatic provider<input bind:value={provider} maxlength="128" placeholder="Provider name" /></label><label>Automatic model<input bind:value={model} maxlength="512" placeholder="Model name" /></label></div>
          {:else}<p>Manvi selection: {configured || "Loading…"}</p>{/if}
          <div class="actions"><button type="button" onclick={() => save()} disabled={override && (!provider.trim() || !model.trim())}>Save automatic settings</button><button type="button" onclick={() => save(true)} disabled={!settings.enabled}>Stop automatic suggestions</button></div>
        </fieldset>
        <p>{queued === null ? "Queue count unavailable" : `${queued} saved tasks waiting`} · One generation at a time · Up to 20 automatic suggestions per hour</p>
        <p>Only unlocked fields are proposed. Review suggestions in each task’s Manvi enhancements panel. No automatic acceptance or provider fallback.</p>
      {/if}
      {#if error}<p role="alert">{error}</p>{/if}
      {#if note}<p role="status">{note}</p>{/if}
      {#if pending}<p>The settings result is uncertain. Retry the same request to reconcile it.</p><button type="button" disabled={saving} onclick={() => save()}>Retry settings request</button>{:else}<button type="button" disabled={loading || saving} onclick={() => load()}>Reload automatic settings</button>{/if}
    </div>
  {/if}
</section>

<style>
  .automation{border-bottom:1px solid rgb(var(--c-border));padding:8px 24px;font-size:12px}.summary{display:flex;gap:10px;align-items:center;flex-wrap:wrap}.summary>button:first-child{display:flex;gap:12px;align-items:center}.summary span{color:rgb(var(--c-text-muted));font-size:11px}button{border:1px solid rgb(var(--c-border));border-radius:6px;padding:6px 9px;font-size:11px}button:disabled,fieldset:disabled{opacity:.55}button:hover{background:rgb(var(--c-surface-hover))}p{margin:8px 0;line-height:1.5;color:rgb(var(--c-text-muted));max-width:850px}p[role="alert"]{color:#d15a64}.settings{padding:5px 0 10px;max-width:850px}fieldset{border:0;padding:0;margin:10px 0}.check{display:flex;align-items:center;gap:8px;margin:9px 0}.pair{display:flex;gap:12px;flex-wrap:wrap;margin:12px 0}.pair label{display:grid;gap:5px;flex:1;min-width:160px}.pair input{padding:7px;border:1px solid rgb(var(--c-border));background:rgb(var(--c-surface));border-radius:6px;color:inherit}.actions{display:flex;gap:8px;margin-top:12px}
</style>
