<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { FolderPlus, RefreshCw, Clock, ShieldCheck, X } from "@lucide/svelte";
  import type { CleanerConfig, CleanerInventory, CleanerState } from "../storage/hygiene/globalTypes";
  import { humanBytes } from "../storage/format";
  let { active = true }: { active?: boolean } = $props();
  let cleaner = $state<CleanerState | null>(null);
  let draft = $state<CleanerConfig | null>(null);
  let inventory = $state<CleanerInventory | null>(null);
  let rootsText = $state("");
  let exclusionsText = $state("");
  let dirty = $state(false);
  let busy = $state(false);
  let inspecting = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let epoch = 0;
  let live = false;
  let loading = false;
  const paths = (text: string) => text.split("\n").map(p => p.trim()).filter(Boolean);

  function accept(next: CleanerState, reset = false) {
    cleaner = next;
    if (!dirty || reset) {
      draft = { ...next.config, roots: [...next.config.roots], exclusions: [...next.config.exclusions] };
      rootsText = draft.roots.join("\n"); exclusionsText = draft.exclusions.join("\n"); dirty = false;
    }
  }
  async function refresh() {
    if (loading) return;
    const mine = epoch; loading = true;
    try {
      const next = await invoke<CleanerState>("cmd_cleaner_state");
      if (live && mine === epoch) accept(next);
    } catch (e) { if (live && mine === epoch) error = String(e); }
    finally { if (mine === epoch) loading = false; }
  }
  function edited() { dirty = true; inventory = null; }
  async function reloadSaved() {
    if (busy) return;
    const mine = epoch; busy = true; error = null;
    try { const next = await invoke<CleanerState>("cmd_cleaner_state"); if (live && mine === epoch) { accept(next, true); notice = "Reloaded the saved policy. Unsaved edits were discarded."; } }
    catch (e) { if (live && mine === epoch) error = String(e); }
    finally { if (live && mine === epoch) busy = false; }
  }
  async function addRoot() {
    try { const path = await invoke<string | null>("cmd_pick_folder"); if (path && live) { rootsText = [...new Set([...paths(rootsText), path])].join("\n"); edited(); } }
    catch (e) { if (live) error = String(e); }
  }
  async function save() {
    if (!draft || busy) return;
    const mine = epoch; busy = true; error = null; notice = null;
    const config = { ...draft, roots: paths(rootsText), exclusions: paths(exclusionsText) };
    if (config.enabled && config.next_run_at === 0) config.next_run_at = Math.floor(Date.now()/1000) + config.interval_hours * 3600;
    try {
      const next = await invoke<CleanerState>("cmd_cleaner_save", { config });
      if (live && mine === epoch) { accept(next, true); notice = "Policy saved. Changes stop any cleanup already in progress."; }
    } catch (e) { if (live && mine === epoch) error = String(e); }
    finally { if (live && mine === epoch) busy = false; }
  }
  async function inspect() {
    if (busy || inspecting || dirty) return;
    const mine = epoch; inspecting = true; error = null;
    try { const next = await invoke<CleanerInventory>("cmd_cleaner_scan"); if (live && mine === epoch) inventory = next; }
    catch (e) { if (live && mine === epoch) error = String(e); }
    finally { if (live && mine === epoch) inspecting = false; }
  }
  async function run() {
    if (!cleaner || dirty || busy || cleaner.running || !inventory || inventory.partial) return;
    const mine = epoch; busy = true; error = null;
    try { const next = await invoke<CleanerState>("cmd_cleaner_run", { revision: cleaner.config.revision }); if (live && mine === epoch) { accept(next); inventory = null; notice = "Cleanup started with the saved policy. Every target is inspected again before removal."; } }
    catch (e) { if (live && mine === epoch) error = String(e); }
    finally { if (live && mine === epoch) busy = false; }
  }
  async function stop() {
    const mine = epoch; error = null;
    try { const next = await invoke<CleanerState>("cmd_cleaner_cancel"); if (live && mine === epoch) { accept(next); notice = "Cancellation requested. Removed entries cannot be restored."; } }
    catch (e) { if (live && mine === epoch) error = String(e); }
  }
  $effect(() => {
    if (!active) return;
    live = true; epoch++; busy = false; inspecting = false; loading = false; void refresh();
    const timer = window.setInterval(() => { if (document.visibilityState === "visible") void refresh(); }, 3000);
    return () => { live = false; epoch++; window.clearInterval(timer); };
  });
</script>

<section aria-label="Global build cleaner" class="space-y-4 text-xs max-w-5xl">
  <div class="flex flex-wrap justify-between gap-3">
    <div><h3 class="flex items-center gap-2 font-semibold text-sm text-textPrimary"><ShieldCheck size={16} /> Global build cleaner</h3><p class="mt-1 text-textMuted">Maintain projects beneath your chosen roots, including repositories that are closed in GitPulse.</p></div>
    <button class="gp-btn" onclick={refresh}><RefreshCw size={12} />Refresh status</button>
  </div>
  {#if error}<p role="alert" class="rounded border border-rose-400/30 p-3 text-rose-300">{error}</p>{/if}
  {#if notice}<p role="status" class="text-textSecondary">{notice}</p>{/if}
  {#if draft && cleaner}
    {#if cleaner.background_error}<p role="alert" class="text-amber-300">Background setup failed; schedule disabled. {cleaner.background_error}</p>{/if}
    <fieldset disabled={busy} class="space-y-4 border-0 p-0 m-0 min-w-0">
    {#if !cleaner.supported}<p class="text-amber-300">Cleanup is unavailable on this platform. Inventory and policy guidance remain available.</p>{/if}
    <div class="grid gap-3 md:grid-cols-2">
      <label class="space-y-1">Project roots · one absolute directory per line<textarea aria-label="Project roots" class="block w-full rounded border border-border bg-surface p-2 font-mono" rows="3" bind:value={rootsText} oninput={edited} disabled={busy}></textarea></label>
      <label class="space-y-1">Excluded directories · one absolute directory per line<textarea aria-label="Excluded directories" class="block w-full rounded border border-border bg-surface p-2 font-mono" rows="3" bind:value={exclusionsText} oninput={edited} disabled={busy}></textarea></label>
    </div>
    <button class="gp-btn" onclick={addRoot} disabled={busy}><FolderPlus size={12} />Add project root</button>
    <div class="flex flex-wrap gap-4">
      <label>Keep output modified within <select aria-label="Global retention" class="ml-1 rounded border border-border bg-surface p-1" bind:value={draft.retention_days} onchange={edited}><option value={7}>7 days</option><option value={14}>14 days</option><option value={30}>30 days</option><option value={90}>90 days</option></select></label>
      <label>Per-run byte budget <select aria-label="Cleanup byte budget" class="ml-1 rounded border border-border bg-surface p-1" bind:value={draft.max_run_bytes} onchange={edited}><option value={1073741824}>1 GiB</option><option value={10737418240}>10 GiB</option><option value={53687091200}>50 GiB</option><option value={107374182400}>100 GiB</option></select></label>
      <label>Maximum directories <input aria-label="Maximum cleanup directories" class="ml-1 w-16 rounded border border-border bg-surface p-1" type="number" min="1" max="100" bind:value={draft.max_targets} oninput={edited} /></label>
    </div>
    <div class="rounded-xl border border-border p-3 space-y-2">
      <label class="flex items-center gap-2"><input type="checkbox" aria-label="Enable scheduled cleanup" bind:checked={draft.enabled} onchange={edited} disabled={!cleaner.supported} /><Clock size={13} />Automatically remove eligible output on schedule</label>
      <label>Repeat every <select aria-label="Cleanup interval" class="mx-1 rounded border border-border bg-surface p-1" bind:value={draft.interval_hours} onchange={() => { if (draft) draft.next_run_at = 0; edited(); }}><option value={24}>day</option><option value={168}>week</option><option value={720}>30 days</option></select></label>
      <label class="flex items-center gap-2"><input type="checkbox" aria-label="Run cleanup when GitPulse is closed" bind:checked={draft.run_when_closed} onchange={edited} disabled={!cleaner.background_supported} />Also run when GitPulse is closed</label>
      <p class="text-textMuted">{cleaner.background_supported ? "Closed-app mode uses a per-user macOS background job and the same saved limits." : "Closed-app mode is available from an installed macOS application bundle."} Otherwise, schedules run while GitPulse is open, including in the background. A missed run is attempted once after restart or wake. Scheduling is off until you save it here.</p>
      <p class="text-textMuted">Next scheduled run: {cleaner.config.enabled ? new Date(cleaner.config.next_run_at * 1000).toLocaleString() : "Off"}</p>
    </div>
    <p class="text-textMuted">Only ignored output with recognized producer evidence is eligible. Dirty repositories, active tasks, recent files, symlinks, environments, dependencies and persistent state are preserved. Shared package caches and non-Git projects are excluded from scheduled sweeps.</p>
    </fieldset>
    <div class="flex flex-wrap gap-2">
      <button class="gp-btn-primary" onclick={save} disabled={!dirty || busy}>{busy ? "Working…" : "Save cleanup policy"}</button>
      <button class="gp-btn" onclick={inspect} disabled={dirty || busy || inspecting || cleaner.running || !cleaner.config.roots.length}><RefreshCw size={12} />{inspecting ? "Inspecting roots…" : "Inspect saved roots"}</button>
      {#if cleaner.running || inspecting}<button class="gp-btn" onclick={stop}><X size={12} />Stop {inspecting ? "inspection" : "cleanup"}</button>{/if}
      {#if dirty}<button class="gp-btn" onclick={reloadSaved} disabled={busy}>Discard changes and reload</button><span class="self-center text-amber-300">Unsaved policy · inspect and run after saving</span>{/if}
    </div>
    {#if inventory}
      <div class="space-y-2 rounded-xl border border-border p-3">
        <strong>{inventory.repositories} repositories · {inventory.candidates.length} output candidates · {inventory.visited_entries.toLocaleString()} entries inspected</strong>
        {#if inventory.partial}<p class="text-amber-300">Partial inventory. Cleanup is blocked until a complete scan succeeds; narrow the selected roots or add exclusions.</p>{/if}
        {#each inventory.issues as issue}<p class="text-amber-300 break-all">{issue}</p>{/each}
        <div class="max-h-52 overflow-y-auto">{#each inventory.candidates as row (`${row.repo_path}/${row.path}`)}<div class="flex justify-between gap-3 border-b border-border/40 py-2"><div class="min-w-0"><p class="font-mono break-all">{row.repo_path}/{row.path}</p><p class="text-textMuted">{row.provider}</p></div><span class="shrink-0">{humanBytes(row.bytes)}</span></div>{/each}</div>
        <p class="text-textMuted">These are current logical sizes. Fresh retention, index, activity and policy checks decide which candidates can be removed, up to the saved limits. Removal is permanent.</p>
        <button class="gp-btn-primary" onclick={run} disabled={dirty || busy || cleaner.running || !cleaner.supported || inventory.partial || !inventory.candidates.length}>Clean eligible output now</button>
      </div>
    {/if}
    {#if cleaner.history.length}
      <details open><summary class="font-semibold cursor-pointer">Recent cleanup runs ({cleaner.history.length})</summary>
        <div class="max-h-72 overflow-y-auto space-y-3 mt-2">{#each cleaner.history as run (run.id)}<div class="rounded border border-border p-3 space-y-1"><strong>{run.status.replaceAll("_", " ")} · {run.trigger}</strong><p class="text-textMuted">{new Date(run.started_at*1000).toLocaleString()} · {run.repositories} repositories · {run.items.length} targets reported</p>{#each run.issues as issue}<p class="text-amber-300 break-all">{issue}</p>{/each}{#each run.items as item}<div class="border-t border-border/40 pt-1"><p class="font-mono break-all">{item.repo_path}/{item.path} · {item.status}</p><p>{item.message}</p><p class="text-textMuted">Before {humanBytes(item.bytes_before)} · After {item.bytes_after === null ? "unavailable" : humanBytes(item.bytes_after)}</p></div>{/each}</div>{/each}</div>
      </details>
    {/if}
    <details><summary class="cursor-pointer">DevCouncil agent hygiene rules</summary><p class="mt-2 whitespace-pre-wrap text-textMuted">{cleaner.agent_rules}</p></details>
  {:else}<p class="text-textMuted">Loading saved cleanup policy…</p>{/if}
</section>
