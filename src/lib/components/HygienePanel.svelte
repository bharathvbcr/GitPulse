<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { RefreshCw, ShieldCheck, FolderTree, Clock, X } from "@lucide/svelte";
  import type { StorageReport } from "../storage/types";
  import type { CacheInventory, HygienePlan, HygieneOutcome } from "../storage/hygiene/types";
  import { DEFAULT_PREFERENCES, ignoreRule, readPreferences, reviewDue, savePreferences, type HygienePreferences } from "../storage/hygiene/preferences";
  import { humanBytes } from "../storage/format";
  import { copyText } from "../desktop/clipboard";
  import { harnessStore, type Guarded } from "../stores/harnessStore";
  import { identityKey, isCaseInsensitiveFs } from "../repos/paths";

  let { report, onchanged }: { report: StorageReport; onchanged: () => Promise<void> } = $props();
  let preferences = $state<HygienePreferences>({ ...DEFAULT_PREFERENCES });
  let inventory = $state<CacheInventory | null>(null);
  let plan = $state<HygienePlan | null>(null);
  let result = $state<HygieneOutcome | null>(null);
  let busy = $state(false);
  let scanning = $state(false);
  let executing = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let clock = $state(Date.now());
  let alive = true;
  let epoch = 0;
  let retryAfter = 0;
  let pending: HygienePlan | null = null;
  let repoPath = $derived(report.repo_path);
  let key = $derived(identityKey(repoPath, { caseInsensitive: isCaseInsensitiveFs() }));
  let expired = $derived(plan !== null && clock >= plan.expires_at * 1000);

  function storage() { try { return localStorage; } catch { return null; } }
  function persist() {
    if (!savePreferences(storage(), key, preferences)) notice = "Preferences could not be saved; these settings apply only to this visit.";
  }

  async function scanCaches() {
    if (scanning || executing) return;
    const mine = epoch;
    scanning = true;
    error = null;
    try {
      const next = await invoke<CacheInventory>("cmd_cache_inventory");
      if (!alive || mine !== epoch) return;
      inventory = next;
      preferences = { ...preferences, lastReview: Date.now() };
      persist();
    } catch (e) { if (alive && mine === epoch) { error = String(e); retryAfter = Date.now() + 60 * 60 * 1000; } }
    finally { if (alive && mine === epoch) scanning = false; }
  }

  async function cancelPreview() {
    const current = pending;
    if (!current) return;
    try {
      await invoke<void>("cmd_hygiene_cancel", { repoPath: current.repo_path, planId: current.id });
      if (alive) { if (!executing) { plan = null; pending = null; } else notice = "Cancellation requested. Entries already removed cannot be restored."; }
    } catch (e) { if (alive) error = String(e); }
  }

  async function preview(target: string) {
    if (busy || executing) return;
    const mine = ++epoch;
    busy = true;
    error = null;
    result = null;
    try {
      await cancelPreview();
      const next = await invoke<HygienePlan>("cmd_hygiene_prepare", { repoPath, target, minAgeDays: preferences.retentionDays });
      if (!alive || mine !== epoch) {
        await invoke<void>("cmd_hygiene_cancel", { repoPath: next.repo_path, planId: next.id });
        return;
      }
      plan = next;
      pending = next;
      clock = Date.now();
    } catch (e) { if (alive && mine === epoch) error = String(e); }
    finally { if (alive && mine === epoch) busy = false; }
  }

  async function execute() {
    const current = plan;
    if (!current || expired || executing) return;
    executing = true;
    error = null;
    try {
      const next = await invoke<Guarded<HygieneOutcome>>("cmd_hygiene_execute", { repoPath: current.repo_path, planId: current.id });
      harnessStore.recordVerdict(next.policy, current.repo_path);
      if (!alive) return;
      result = next.output;
      plan = null;
      pending = null;
      await onchanged();
      if (current.scope === "shared") inventory = null;
    } catch (e) { if (alive) { error = String(e); plan = null; pending = null; } }
    finally { if (alive) executing = false; }
  }

  async function copyIgnore(path: string) {
    const rule = ignoreRule(path);
    if (!rule) { error = "This path requires manual ignore-rule review."; return; }
    try {
      if (!await copyText(rule + "\n")) { error = "Clipboard is unavailable. Add this reviewed rule manually: " + rule; return; }
      notice = `Copied ${rule} — review it in the repository's .gitignore. Tracked files remain tracked.`;
    }
    catch (e) { error = String(e); }
  }

  onMount(() => {
    preferences = readPreferences(storage(), key);
    // Only inventory runs on a timer. Every deletion still needs a fresh
    // preview and the user's explicit click, with backend revalidation.
    const tick = () => {
      clock = Date.now();
      if (document.visibilityState === "visible" && clock >= retryAfter && reviewDue(preferences, clock) && !busy && !executing && !scanning) void scanCaches();
    };
    tick();
    const timer = window.setInterval(tick, 30_000);
    return () => {
      alive = false;
      epoch++;
      window.clearInterval(timer);
      if (pending) void invoke<void>("cmd_hygiene_cancel", { repoPath: pending.repo_path, planId: pending.id }).catch(e => console.warn("Could not cancel hygiene preview", String(e)));
    };
  });
</script>

<section class="space-y-4 max-w-4xl rounded-2xl border border-border/70 bg-surface/40 p-4" aria-label="Repository hygiene">
  <div class="flex items-start justify-between gap-3">
    <div>
      <h3 class="flex items-center gap-2 text-sm font-semibold text-textPrimary"><ShieldCheck size={16} /> Repository hygiene</h3>
      <p class="mt-1 text-xs text-textMuted">Review stale build output, protect local work, and maintain shared caches with their owning tools.</p>
    </div>
    <button class="gp-btn shrink-0" onclick={scanCaches} disabled={scanning || executing || busy}><RefreshCw size={12} class={scanning ? "animate-spin" : ""} />{scanning ? "Inspecting…" : "Inspect shared caches"}</button>
  </div>

  <div class="flex flex-wrap items-center gap-4 text-xs text-textSecondary">
    <label class="flex items-center gap-2"><Clock size={12} /> Keep output modified within
      <select class="rounded border border-border bg-surface px-2 py-1" bind:value={preferences.retentionDays} onchange={persist} disabled={busy || executing}>
        <option value={7}>7 days</option><option value={14}>14 days</option><option value={30}>30 days</option><option value={90}>90 days</option>
      </select>
    </label>
    <label class="flex items-center gap-2"><input type="checkbox" bind:checked={preferences.weeklyReview} onchange={persist} />Review shared caches weekly while this page is open</label>
  </div>
  <p class="text-xs text-textMuted">Cleanup checks ignore rules, tracked files, producer markers, modification times and open files. Stop builds first. Recent output, environments and persistent state are preserved.</p>

  {#if error}<p role="alert" class="rounded-lg border border-rose-400/30 bg-rose-400/5 p-3 text-xs text-rose-300">{error}</p>{/if}
  {#if notice}<p role="status" class="text-xs text-textSecondary">{notice}</p>{/if}
  {#if result}
    <div role="status" class="rounded-xl border border-border p-3 text-xs space-y-1">
      <strong class={result.success ? "text-emerald-400" : "text-amber-300"}>{result.success ? "Maintenance completed" : "Maintenance incomplete"}</strong>
      <p>{result.message}</p>
      <p class="font-mono text-textMuted">Before: {humanBytes(result.bytes_before)} · After: {result.bytes_after === null ? "measurement unavailable" : humanBytes(result.bytes_after)}</p>
    </div>
  {/if}
  {#if plan}
    <div role="region" aria-label="Cleanup preview" class="rounded-xl border border-amber-400/40 bg-amber-400/5 p-4 space-y-2 text-xs">
      <div class="flex justify-between"><strong class="text-textPrimary">{plan.scope === "shared" ? "Shared cache maintenance" : "Cleanup preview"} · {plan.label}</strong><span>{expired ? "Expired — preview again" : `Expires in ${Math.ceil((plan.expires_at * 1000 - clock) / 60_000)} min`}</span></div>
      <p class="break-all font-mono">{plan.path}</p>
      <p>{plan.files.toLocaleString()} files · {humanBytes(plan.bytes)} current logical size</p>
      <p>{plan.warning}</p>
      <p class="break-all font-mono text-textMuted">{plan.command}</p>
      <div class="flex gap-2 pt-2">
        <button class="gp-btn-primary" onclick={execute} disabled={expired || executing}>{executing ? "Maintaining…" : plan.scope === "shared" ? "Run shared cache maintenance" : "Remove reviewed output"}</button>
        <button class="gp-btn" onclick={cancelPreview}><X size={12} />{executing ? "Stop" : "Cancel"}</button>
      </div>
    </div>
  {/if}

  <details open>
    <summary class="cursor-pointer text-xs font-semibold text-textSecondary">Repository output ({report.artifacts.length} shown)</summary>
    {#if report.scan.truncated}<p class="mt-2 text-xs text-amber-300">Inventory is partial. Every selected directory must pass a separate complete inspection.</p>{/if}
    <div class="mt-2 space-y-2">
      {#each report.artifacts as artifact (artifact.path)}
        {@const blocked = report.reclaim.find(item => item.label === artifact.path)?.blocked_reason}
        <div class="flex flex-wrap items-center gap-2 border-b border-border/40 py-2 text-xs">
          <FolderTree size={12} class="text-textMuted" /><span class="flex-1 min-w-32 font-mono break-all">{artifact.path}</span>
          <span class="text-textMuted">{humanBytes(artifact.bytes)}</span>
          {#if artifact.tracked_files > 0}<span class="text-amber-300">{artifact.tracked_files} tracked · preserved</span>{/if}
          {#if artifact.unignored && artifact.tracked_files === 0}<button class="gp-btn" onclick={() => copyIgnore(artifact.path)}>Copy ignore rule</button>{/if}
          {#if blocked}<span class="text-amber-300" title={blocked}>Preserved · review required</span>{/if}
          <button class="gp-btn" disabled={busy || executing || scanning || artifact.tracked_files > 0 || artifact.unignored || !!blocked} onclick={() => preview(`local:${artifact.path}`)}>Preview cleanup</button>
        </div>
      {:else}<p class="py-2 text-xs text-textMuted">No build or cache directories were found in this scan.</p>{/each}
    </div>
  </details>
  {#if inventory}
    <div class="space-y-3">
      <h4 class="text-xs font-semibold text-textSecondary">Shared caches · measured {new Date(inventory.measured_at * 1000).toLocaleString()}</h4>
      {#each inventory.entries as entry (entry.id)}
        <div class="rounded-xl border border-border/50 p-3 text-xs space-y-1">
          <div class="flex items-center justify-between gap-2"><strong>{entry.label}</strong><span class="font-mono">{entry.bytes === null ? "Not measured" : humanBytes(entry.bytes)}</span></div>
          {#if entry.path}<p class="break-all font-mono text-textMuted">{entry.path}</p>{/if}
          <p class="text-textSecondary">{entry.note}</p>
          {#if entry.error}<p class="text-amber-300">Unavailable: {entry.error}</p>{/if}
          {#if entry.action}<button class="gp-btn mt-2" disabled={busy || executing || scanning} onclick={() => preview(`cache:${entry.id}`)}>{entry.action}…</button>{/if}
        </div>
      {/each}
    </div>
  {/if}
  <details class="text-xs text-textMuted">
    <summary class="cursor-pointer">Efficient maintenance strategies</summary>
    <ul class="list-disc pl-4 mt-2 space-y-1">
      <li>Cargo: keep automatic global GC enabled; clean stale project output separately. Prefer per-project targets or a compiler cache over one shared target folder.</li>
      <li>Go: let automatic build-cache expiry work. Module downloads and fuzzing inputs are preserved.</li>
      <li>Gradle: use native cache retention; preserve wrappers, module caches and daemon state.</li>
      <li>Swift / Xcode: use native build cleanup after stopping builds; .build can contain dependency checkouts.</li>
      <li>Python / Node: prune unused shared entries while retaining environments, node_modules, models and datasets.</li>
    </ul>
  </details>
</section>
