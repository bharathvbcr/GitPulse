<script lang="ts">
  import { onMount } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { harnessStore, verdictDetail, verdictLabel } from "../stores/harnessStore";
  import {
    harnessPermissionMode,
    harnessPermissionSummary,
  } from "../harness/availability";
  import { ShieldCheck, ShieldAlert, ShieldQuestion, Sparkles } from "lucide-svelte";

  onMount(() => {
    // One probe at startup: the sweep is a handful of loopback connections and
    // a handshake, and the answer decides what the commit box can offer.
    void harnessStore.refresh();
  });

  let harness = $derived($harnessStore.harness);
  let ai = $derived($harnessStore.ai);
  let verdict = $derived($harnessStore.lastVerdict);
  let permissionMode = $derived(harnessPermissionMode(harness));

  let harnessTitle = $derived(
    `${harnessPermissionSummary(harness)}${harness?.binary ? `\n${harness.binary}` : ""}\nClick to open the MANVI view.`,
  );

  let modelTitle = $derived(
    ai?.ready && ai.selected
      ? `${ai.selected.model} at ${ai.selected.base_url}\n${ai.model_info?.describe ?? "context window not probed"}\nClick to open the MANVI view.`
      : (ai?.detail ?? "Looking for a local model server…") + "\nClick to open the MANVI view.",
  );

  let modelLabel = $derived(
    ai?.ready && ai.selected
      ? ai.selected.model
      : ai
        ? "no local model"
        : "…"
  );
</script>

<div class="flex items-center gap-1.5">
  <!-- Harness state. The three states are distinct on purpose: connected,
       unavailable, and "a rule fired on the last action" never collapse into
       one another. Both chips lead to the single MANVI view, which owns all
       harness and local-AI controls. -->
  <button
    onclick={() => repoStore.setActiveTab("manvi")}
    title={harnessTitle}
    class="px-2.5 py-1 rounded-full border text-[11px] flex items-center gap-1.5 transition-colors shadow-sm
      {permissionMode === 'connected'
        ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/20'
        : permissionMode === 'blocked'
          ? 'border-rose-500/30 bg-rose-500/10 text-rose-400 hover:bg-rose-500/20'
          : permissionMode === 'unguarded'
            ? 'border-amber-500/30 bg-amber-500/10 text-amber-400 hover:bg-amber-500/20'
            : 'border-border/80 bg-surfaceHover text-textMuted hover:text-textPrimary'}"
  >
    {#if permissionMode === "connected"}
      <ShieldCheck size={12} />
    {:else if permissionMode === "not-probed"}
      <ShieldQuestion size={12} />
    {:else}
      <ShieldAlert size={12} />
    {/if}
    <span class="font-medium">MANVI</span>
  </button>

  <!-- Local model. -->
  <button
    onclick={() => repoStore.setActiveTab("manvi")}
    title={modelTitle}
    class="px-2.5 py-1 rounded-full border text-[11px] flex items-center gap-1.5 transition-colors max-w-[180px] shadow-sm
      {ai?.ready
        ? 'border-accent/30 bg-accent/10 text-accent hover:bg-accent/20'
        : 'border-border/80 bg-surfaceHover text-textMuted hover:text-textPrimary'}"
  >
    <Sparkles size={12} />
    <span class="truncate">{modelLabel}</span>
  </button>

  {#if verdict}
    <button
      onclick={() => repoStore.setActiveTab("manvi")}
      title={verdictDetail(verdict)}
      class="px-2.5 py-1 rounded-full border text-[11px] flex items-center gap-1.5 shadow-sm
        {verdict.status === 'blocked'
          ? 'border-rose-500/30 bg-rose-500/10 text-rose-400'
          : verdict.status === 'unchecked'
            ? 'border-amber-500/30 bg-amber-500/10 text-amber-400'
            : 'border-border/80 bg-surfaceHover text-textMuted'}"
    >
      <ShieldQuestion size={12} />
      <span class="truncate max-w-[150px]">{verdictLabel(verdict)}</span>
    </button>
  {/if}
</div>
