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
  import { ShieldCheck, ShieldAlert, ShieldQuestion, Sparkles, ChevronDown, RefreshCw, CircleAlert } from "@lucide/svelte";

  const instanceId = $props.id();

  onMount(() => {
    // One probe at startup: the sweep is a handful of loopback connections and
    // a handshake, and the answer decides what the commit box can offer.
    void harnessStore.refresh();
  });

  /**
   * The harness and verdict chips open the section that owns their subject.
   * Model selection is global and stays directly in the header.
   */
  function openManvi(target: ManviFocusId) {
    requestManviFocus(target);
    repoStore.setActiveTab("work", "policy");
  }

  // MANVI is a repository view: with no repository open there is no session to
  // switch tabs on, so navigation chips remain disabled until a repo is open.
  let reachable = $derived(Boolean($repoStore.currentPath));
  let unreachableHint = "Open a repository to reach the MANVI view.";

  let harness = $derived($harnessStore.harness);
  let ai = $derived($harnessStore.ai);
  let preferred = $derived($harnessStore.preferred);
  let endpoints = $derived(ai?.endpoints.filter((endpoint) => endpoint.reachable) ?? []);
  let preferredKey = $derived(preferred ? JSON.stringify([preferred.base_url, preferred.model]) : "");
  let preferredListed = $derived(!preferred || endpoints.some((endpoint) =>
    endpoint.base_url === preferred.base_url && endpoint.models.includes(preferred.model),
  ));
  let modelReady = $derived(Boolean(
    !$harnessStore.isProbing && !$harnessStore.error && ai?.ready && ai.selected &&
    (!preferred || (preferred.base_url === ai.selected.base_url && preferred.model === ai.selected.model)),
  ));
  let modelAttention = $derived(!$harnessStore.isProbing && Boolean($harnessStore.error || (ai && !modelReady)));
  let modelStatus = $derived(
    $harnessStore.isProbing
      ? preferred ? `Checking ${preferred.model}…` : "Refreshing local models…"
      : $harnessStore.error
        ? `Model check failed. ${$harnessStore.error} Use Retry to check again.`
        : modelReady && ai?.selected
          ? `${preferred ? "Using" : "Automatically using"} ${ai.selected.model}.`
          : preferred
            ? `${preferred.model} is unavailable. Choose another model or refresh.`
            : ai
              ? "No local models available. Start a local model server, then refresh."
              : "Local models have not been checked. Refresh to discover models.",
  );
  let refreshLabel = $derived($harnessStore.isProbing
    ? "Refreshing local models"
    : $harnessStore.error ? "Retry model discovery" : "Refresh local models");
  let automaticDetail = $derived(preferred ? "" : $harnessStore.isProbing
    ? " · checking…"
    : $harnessStore.error ? " · retry needed"
    : modelReady && ai?.selected ? ` · ${ai.selected.model}`
    : ai ? " · no models" : " · not checked");
  let verdict = $derived($harnessStore.lastVerdict);
  let permissionMode = $derived(harnessPermissionMode(harness));

  let harnessTitle = $derived(
    `${harnessPermissionSummary(harness)}${harness?.binary ? `\n${harness.binary}` : ""}\n${reachable ? manviFocusHint("harness") : unreachableHint}`,
  );

  let modelTitle = $derived(
    `${modelStatus}${preferred ? `\n${preferred.model} at ${preferred.base_url}`
      : modelReady && ai?.selected ? `\n${ai.selected.base_url}` : ""}${
      modelReady ? `\n${ai?.model_info?.describe ?? "context window not probed"}` : ""
    }\nChoose a local model.`,
  );

  let verdictTitle = $derived(
    verdict
      ? `${verdictDetail(verdict)}\n${reachable ? manviFocusHint("activity") : unreachableHint}`
      : unreachableHint,
  );

  function pickModel(key: string) {
    if (key === "") {
      void harnessStore.selectModel(null);
      return;
    }
    for (const endpoint of endpoints) {
      const model = endpoint.models.find((model) => JSON.stringify([endpoint.base_url, model]) === key);
      if (model !== undefined) {
        void harnessStore.selectModel({ base_url: endpoint.base_url, model });
        return;
      }
    }
  }
</script>

<div class="flex items-center gap-1.5">
  <!-- Harness state. The three states are distinct on purpose: connected,
       unavailable, and "a rule fired on the last action" never collapse into
       one another. -->
  <button
    onclick={() => openManvi("harness")}
    disabled={!reachable}
    title={harnessTitle}
    class="px-2.5 py-1 rounded-full border text-[11px] flex items-center gap-1.5 transition-colors shadow-xs disabled:cursor-default
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

  <!-- Native selection supplies keyboard navigation, typeahead and dismissal. -->
  <div
    class="flex max-w-[180px] items-center rounded-full border text-[11px] shadow-xs transition-colors
      {$harnessStore.error && !$harnessStore.isProbing
        ? 'border-rose-500/30 bg-rose-500/10 text-rose-400'
        : modelReady
          ? 'border-accent/30 bg-accent/10 text-accent'
          : 'border-border/80 bg-surfaceHover text-textMuted'}"
  >
    <div class="relative min-w-0 flex-1">
      <span class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2" aria-hidden="true">
        {#if modelAttention}
          <CircleAlert size={12} />
        {:else}
          <Sparkles size={12} />
        {/if}
      </span>
      <select
        aria-label="Local model"
        aria-describedby="{instanceId}-model-status"
        aria-busy={$harnessStore.isProbing}
        title={modelTitle}
        value={preferredKey}
        onchange={(event) => pickModel(event.currentTarget.value)}
        class="w-full cursor-pointer appearance-none truncate rounded-l-full bg-transparent py-1 pl-7 pr-5 text-[11px] transition-colors enabled:hover:bg-accent/10 focus-visible:outline-2 focus-visible:outline-accent"
      >
        <option value="">Automatic{automaticDetail}</option>
        {#if preferred && !preferredListed}
          <option value={preferredKey} disabled>{preferred.model} ({$harnessStore.isProbing ? "checking…" : "unavailable"})</option>
        {/if}
        {#each endpoints as endpoint}
          <optgroup label={endpoint.base_url}>
            {#each endpoint.models as model}
              <option value={JSON.stringify([endpoint.base_url, model])}>{model}</option>
            {/each}
          </optgroup>
        {/each}
        {#if endpoints.every((endpoint) => endpoint.models.length === 0)}
          <option disabled>{$harnessStore.isProbing ? "Looking for local models…" : "No local models available"}</option>
        {/if}
      </select>
      <ChevronDown size={11} class="pointer-events-none absolute right-1.5 top-1/2 -translate-y-1/2" aria-hidden="true" />
    </div>
    <button
      type="button"
      aria-label={refreshLabel}
      title={refreshLabel}
      disabled={$harnessStore.isProbing}
      onclick={() => { if (!$harnessStore.isProbing) void harnessStore.refreshAi(); }}
      class="flex w-7 shrink-0 self-stretch items-center justify-center rounded-r-full border-l border-current/15 transition-colors enabled:hover:bg-accent/10 focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-wait"
    >
      <RefreshCw size={11} class={$harnessStore.isProbing ? "animate-spin motion-reduce:animate-none" : ""} aria-hidden="true" />
    </button>
  </div>
  <span id="{instanceId}-model-status" class="sr-only" role="status" aria-live="polite" aria-atomic="true">{modelStatus}</span>

  {#if verdict}
    <button
      onclick={() => openManvi("activity")}
      disabled={!reachable}
      title={verdictTitle}
      class="px-2.5 py-1 rounded-full border text-[11px] flex items-center gap-1.5 shadow-xs disabled:cursor-default
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
