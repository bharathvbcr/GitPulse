<script lang="ts">
  import { onMount } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { harnessStore, verdictDetail, verdictLabel } from "../stores/harnessStore";
  import {
    harnessPermissionMode,
    harnessPermissionSummary,
  } from "../harness/availability";
  import { MANVI_FOCUS_TARGETS, manviFocusHint, type ManviFocusId } from "../ui/manviFocus";
  import { openManviFocus } from "../ui/openManviFocus";
  import { ShieldCheck, ShieldAlert, ShieldQuestion, Sparkles, RefreshCw, CircleAlert } from "@lucide/svelte";

  const instanceId = $props.id();

  onMount(() => {
    // One probe at startup: the sweep is a handful of loopback connections and
    // a handshake, and the answer decides what the commit box can offer.
    void harnessStore.refresh();
  });

  /**
   * The harness and verdict chips open the card that owns their subject: the
   * connection is configuration (Settings → Agents), a verdict is history of
   * this repository (Work → Overview → Policy activity). Model selection is
   * global and stays directly in the header.
   */
  function openManvi(target: ManviFocusId) {
    openManviFocus(target);
  }

  // Settings opens with or without a repository; Overview is a repository
  // view, so a chip that lands there stays disabled until a repo is open.
  let hasRepo = $derived(Boolean($repoStore.currentPath));
  function reachableFor(target: ManviFocusId): boolean {
    return MANVI_FOCUS_TARGETS[target].destination.kind === "settings" || hasRepo;
  }
  let unreachableHint = "Open a repository to see its policy activity.";

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
        ? `Couldn't check local models. ${$harnessStore.error} Refresh to try again.`
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
    : $harnessStore.error ? "Try local model discovery again" : "Refresh local models");
  let automaticDetail = $derived(preferred ? "" : $harnessStore.isProbing
    ? " · checking…"
    : $harnessStore.error ? " · retry needed"
    : modelReady && ai?.selected ? ` · ${ai.selected.model}`
    : ai ? " · no models" : " · not checked");
  let verdict = $derived($harnessStore.lastVerdict);
  let permissionMode = $derived(harnessPermissionMode(harness));
  /** Not probed yet is unknown, never a neutral that reads as "fine". */
  let harnessTone = $derived(
    permissionMode === "connected" ? "ok"
      : permissionMode === "blocked" ? "danger"
        : permissionMode === "unguarded" ? "warn"
          : "unknown",
  );
  let modelTone = $derived(
    $harnessStore.error && !$harnessStore.isProbing ? "text-danger" : modelReady ? "text-accent" : "text-textMuted",
  );

  let harnessTitle = $derived(
    `${harnessPermissionSummary(harness)}${harness?.binary ? `\n${harness.binary}` : ""}\n${manviFocusHint("harness")}`,
  );

  let modelTitle = $derived(
    `${modelStatus}${preferred ? `\n${preferred.model} at ${preferred.base_url}`
      : modelReady && ai?.selected ? `\n${ai.selected.base_url}` : ""}${
      modelReady ? `\n${ai?.model_info?.describe ?? "context window not probed"}` : ""
    }\nChoose a local model.`,
  );

  let verdictTitle = $derived(
    verdict
      ? `${verdictDetail(verdict)}\n${reachableFor("activity") ? manviFocusHint("activity") : unreachableHint}`
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
  <!-- The harness and the local model are separate facts with separate states,
       so each keeps its own tone. The harness chip's states stay distinct on
       purpose: connected, unavailable, "a rule fired on the last action" and
       "not probed yet" never collapse into one another. -->
  <div role="group" aria-label="Manvi and local model" class="flex min-w-0 items-center gap-1.5">
    <button
      onclick={() => openManvi("harness")}
      disabled={!reachableFor("harness")}
      title={harnessTitle}
      class="gp-chip shrink-0 gap-1.5 enabled:cursor-pointer disabled:cursor-default"
      data-tone={harnessTone}
    >
      {#if permissionMode === "connected"}
        <ShieldCheck size={12} />
      {:else if permissionMode === "not-probed"}
        <ShieldQuestion size={12} />
      {:else}
        <ShieldAlert size={12} />
      {/if}
      <span class="font-medium">Manvi</span>
    </button>

    <!-- Native selection supplies keyboard navigation, typeahead and dismissal. -->
    <div class="relative min-w-0 max-w-[180px] flex-1">
      <span class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 {modelTone}" aria-hidden="true">
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
        class="gp-select w-full pl-7"
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
    </div>
    <button
      type="button"
      class="gp-icon-btn shrink-0"
      data-size="sm"
      aria-label={refreshLabel}
      title={refreshLabel}
      disabled={$harnessStore.isProbing}
      onclick={() => { if (!$harnessStore.isProbing) void harnessStore.refreshAi(); }}
    >
      <RefreshCw size={11} class={$harnessStore.isProbing ? "animate-spin motion-reduce:animate-none" : ""} aria-hidden="true" />
    </button>
  </div>
  <span id="{instanceId}-model-status" class="sr-only" role="status" aria-live="polite" aria-atomic="true">{modelStatus}</span>

  {#if verdict}
    <button
      onclick={() => openManvi("activity")}
      disabled={!reachableFor("activity")}
      title={verdictTitle}
      class="gp-chip shrink-0 gap-1.5 enabled:cursor-pointer disabled:cursor-default"
      data-tone={verdict.status === "blocked" ? "danger" : verdict.status === "unchecked" ? "unknown" : "neutral"}
    >
      <ShieldQuestion size={12} />
      <span class="truncate max-w-[150px]">{verdictLabel(verdict)}</span>
    </button>
  {/if}
</div>
