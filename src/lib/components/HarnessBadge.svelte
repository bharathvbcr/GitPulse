<script lang="ts">
  import { onMount } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { harnessStore, verdictDetail, verdictLabel } from "../stores/harnessStore";
  import {
    harnessPermissionMode,
    harnessPermissionSummary,
  } from "../harness/availability";
  import {
    manviFocusHint,
    requestManviFocus,
    type ManviFocusId,
  } from "../ui/manviFocus";
  import { ShieldCheck, ShieldAlert, ShieldQuestion, Sparkles } from "lucide-svelte";

  onMount(() => {
    // One probe at startup: the sweep is a handful of loopback connections and
    // a handshake, and the answer decides what the commit box can offer.
    void harnessStore.refresh();
  });

  /**
   * Each chip opens the section that owns its subject, not just the view: the
   * shield, the model and the last verdict live on different panes and rows of
   * one long page, so "open MANVI" answered none of the three questions.
   */
  function openManvi(target: ManviFocusId) {
    requestManviFocus(target);
    repoStore.setActiveTab("work", "policy");
  }

  // MANVI is a repository view: with no repository open there is no session to
  // switch tabs on, so the chips report status without pretending to be links.
  let reachable = $derived(Boolean($repoStore.currentPath));
  let unreachableHint = "Open a repository to reach the MANVI view.";

  let harness = $derived($harnessStore.harness);
  let ai = $derived($harnessStore.ai);
  let verdict = $derived($harnessStore.lastVerdict);
  let permissionMode = $derived(harnessPermissionMode(harness));

  let harnessTitle = $derived(
    `${harnessPermissionSummary(harness)}${harness?.binary ? `\n${harness.binary}` : ""}\n${reachable ? manviFocusHint("harness") : unreachableHint}`,
  );

  let modelTitle = $derived(
    `${
      ai?.ready && ai.selected
        ? `${ai.selected.model} at ${ai.selected.base_url}\n${ai.model_info?.describe ?? "context window not probed"}`
        : (ai?.detail ?? "Looking for a local model server…")
    }\n${reachable ? manviFocusHint("model") : unreachableHint}`,
  );

  let verdictTitle = $derived(
    verdict
      ? `${verdictDetail(verdict)}\n${reachable ? manviFocusHint("activity") : unreachableHint}`
      : unreachableHint,
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
       one another. Each chip opens the MANVI section that owns its subject,
       so the three chips are three destinations rather than one. -->
  <button
    onclick={() => openManvi("harness")}
    disabled={!reachable}
    title={harnessTitle}
    class="px-2.5 py-1 rounded-full border text-[11px] flex items-center gap-1.5 transition-colors shadow-sm disabled:cursor-default
      {permissionMode === 'connected'
        ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-400 enabled:hover:bg-emerald-500/20'
        : permissionMode === 'blocked'
          ? 'border-rose-500/30 bg-rose-500/10 text-rose-400 enabled:hover:bg-rose-500/20'
          : permissionMode === 'unguarded'
            ? 'border-amber-500/30 bg-amber-500/10 text-amber-400 enabled:hover:bg-amber-500/20'
            : 'border-border/80 bg-surfaceHover text-textMuted enabled:hover:text-textPrimary'}"
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
    onclick={() => openManvi("model")}
    disabled={!reachable}
    title={modelTitle}
    class="px-2.5 py-1 rounded-full border text-[11px] flex items-center gap-1.5 transition-colors max-w-[180px] shadow-sm disabled:cursor-default
      {ai?.ready
        ? 'border-accent/30 bg-accent/10 text-accent enabled:hover:bg-accent/20'
        : 'border-border/80 bg-surfaceHover text-textMuted enabled:hover:text-textPrimary'}"
  >
    <Sparkles size={12} />
    <span class="truncate">{modelLabel}</span>
  </button>

  {#if verdict}
    <button
      onclick={() => openManvi("activity")}
      disabled={!reachable}
      title={verdictTitle}
      class="px-2.5 py-1 rounded-full border text-[11px] flex items-center gap-1.5 shadow-sm disabled:cursor-default
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
